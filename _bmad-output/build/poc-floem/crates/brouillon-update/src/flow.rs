//! One update pass: fetch -> verify -> decide -> (optionally) download and verify the installer.

use crate::install::{clean_dir, verify_installer};
use crate::keys::TrustedKey;
use crate::manifest::{check_url, parse_verified, split_url, Manifest, UrlPolicy};
use crate::net::{download_to, fetch_limited, Transport};
use crate::policy::{evaluate, is_mandatory, Offer};
use crate::state::UpdateState;
use crate::{UpdateError, ALLOWED_HOSTS, DOWNLOAD_TIMEOUT_SECS, MANIFEST_TIMEOUT_SECS, MAX_INSTALLER_BYTES, MAX_MANIFEST_BYTES, MAX_SIG_BYTES};
use std::path::PathBuf;
use std::time::Duration;

pub struct Ctx {
    pub manifest_url: String,
    /// Development mode (`BROUILLON_UPDATE_DEV=1`): loopback HTTP allowed.
    pub dev: bool,
    pub keys: Vec<TrustedKey>,
    pub current_build: u64,
    pub updates_dir: PathBuf,
    /// `updates.auto_install`
    pub auto: bool,
    /// Installed Windows copy (see `install::is_installed_copy`).
    pub can_install: bool,
    /// `updates.channel`: empty = any. A manifest naming another channel is ignored.
    pub channel: String,
}

#[derive(Debug, PartialEq, Eq)]
pub enum Outcome {
    UpToDate,
    /// Manifest older than the running build was ignored (rollback protection).
    DowngradeIgnored,
    /// The user chose "Skip this version" for exactly this build (never skipped when mandatory).
    Skipped(Manifest),
    /// An automatic install of exactly this build already failed here: tell the user, do not retry.
    KnownFailure(Manifest),
    /// A newer build exists; nothing was downloaded (portable, other OS, or `auto = false`).
    Notify(Manifest),
    /// Installer downloaded and fully verified; `installer` may be launched.
    Staged { manifest: Manifest, installer: PathBuf, mandatory: bool },
}

fn policy_for(ctx: &Ctx) -> Result<UrlPolicy, UpdateError> {
    let (_, host, _) = split_url(&ctx.manifest_url).ok_or_else(|| UpdateError::InsecureUrl(ctx.manifest_url.clone()))?;
    Ok(UrlPolicy { dev: ctx.dev, manifest_host: host, allowed_hosts: ALLOWED_HOSTS.iter().map(|s| s.to_string()).collect() })
}

/// Fetch and fully verify the manifest, apply the policy, and stage the installer when allowed.
/// Updates `state.last_published_epoch` once the manifest passed every check.
/// `found` is called once when an installer is about to be downloaded; `progress(done, total)` while it downloads.
pub fn run_once(t: &dyn Transport, ctx: &Ctx, state: &mut UpdateState, now: i64, found: &mut dyn FnMut(&Manifest), progress: &mut dyn FnMut(u64, u64)) -> Result<Outcome, UpdateError> {
    let policy = policy_for(ctx)?;
    check_url(&ctx.manifest_url, &policy)?;
    let to = Duration::from_secs(MANIFEST_TIMEOUT_SECS);
    let bytes = fetch_limited(t, to, &ctx.manifest_url, MAX_MANIFEST_BYTES, &policy)?;
    let sig = fetch_limited(t, to, &format!("{}.sig", ctx.manifest_url), MAX_SIG_BYTES, &policy)?;
    let sig_text = String::from_utf8(sig).map_err(|_| UpdateError::SigMalformed)?;
    let m = parse_verified(&bytes, &sig_text, &ctx.keys, &policy)?;
    let offer = evaluate(&m, ctx.current_build, state, now)?;
    let published = crate::timeutil::parse_utc(&m.published_utc).unwrap_or(0);
    state.last_published_epoch = state.last_published_epoch.max(published);
    match offer {
        Offer::UpToDate => return Ok(Outcome::UpToDate),
        Offer::DowngradeRefused => return Ok(Outcome::DowngradeIgnored),
        Offer::Newer => {}
    }
    if !ctx.channel.is_empty() && !m.channel.is_empty() && m.channel != ctx.channel {
        return Ok(Outcome::UpToDate);
    }
    let mandatory = is_mandatory(&m, ctx.current_build);
    if !mandatory && state.skipped_build == m.build {
        return Ok(Outcome::Skipped(m));
    }
    if state.failed_build == m.build {
        return Ok(Outcome::KnownFailure(m));
    }
    if !ctx.auto || !ctx.can_install {
        return Ok(Outcome::Notify(m));
    }
    found(&m);
    let installer = stage(t, ctx, &m, &policy, progress)?;
    Ok(Outcome::Staged { manifest: m, installer, mandatory })
}

/// Download to `<file>.part`, verify size + SHA-256 + PE header, then rename. Never leaves an
/// unverified `<file>`.
fn stage(t: &dyn Transport, ctx: &Ctx, m: &Manifest, policy: &UrlPolicy, progress: &mut dyn FnMut(u64, u64)) -> Result<PathBuf, UpdateError> {
    std::fs::create_dir_all(&ctx.updates_dir)?;
    clean_dir(&ctx.updates_dir, None);
    let part = ctx.updates_dir.join(format!("{}.part", m.installer.file));
    let dest = ctx.updates_dir.join(&m.installer.file);
    // The announced size is the cap (never above the global limit): a longer body is aborted.
    let cap = m.installer.size_bytes.min(MAX_INSTALLER_BYTES);
    download_to(t, Duration::from_secs(DOWNLOAD_TIMEOUT_SECS), &m.installer.url, &part, cap, policy, progress)?;
    if let Err(e) = verify_installer(&part, &m.installer) {
        let _ = std::fs::remove_file(&part);
        return Err(e);
    }
    std::fs::rename(&part, &dest).inspect_err(|_| {
        let _ = std::fs::remove_file(&part);
    })?;
    Ok(dest)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::net::Reply;
    use crate::install::{sha256_file, INSTALL_ARGS};
    use crate::manifest::testkit::{manifest_json, sign, signing_key, trusted};
    use std::collections::HashMap;
        use std::sync::{Arc, Mutex};

    type Routes = Arc<Mutex<HashMap<String, (u16, Vec<u8>, Option<String>)>>>;

    /// In-process fake of the update host: no socket is ever opened (loopback test servers get the
    /// freshly built test exe deleted by the security product on the dev machine).
    struct Srv {
        port: u16,
        routes: Routes,
        hits: Arc<Mutex<Vec<String>>>,
    }
    impl Transport for Srv {
        fn get(&self, url: &str, _timeout: Duration) -> Result<Reply, UpdateError> {
            let prefix = format!("http://127.0.0.1:{}", self.port);
            let Some(path) = url.strip_prefix(&prefix) else { return Err(UpdateError::Network("connection refused".into())) };
            self.hits.lock().unwrap().push(path.to_string());
            let (code, body, loc) = self.routes.lock().unwrap().get(path).cloned().unwrap_or((404, b"nope".to_vec(), None));
            Ok(Reply { status: code, location: loc, content_length: Some(body.len() as u64), body: Box::new(std::io::Cursor::new(body)) })
        }
    }
    impl Srv {
        fn start() -> Srv {
            Srv { port: 8080, routes: Arc::default(), hits: Arc::default() }
        }
        fn url(&self, p: &str) -> String {
            format!("http://127.0.0.1:{}{}", self.port, p)
        }
        fn put(&self, path: &str, body: impl Into<Vec<u8>>) {
            self.routes.lock().unwrap().insert(path.to_string(), (200, body.into(), None));
        }
        fn redirect(&self, path: &str, to: &str) {
            self.routes.lock().unwrap().insert(path.to_string(), (302, Vec::new(), Some(to.to_string())));
        }
    }

    fn ro(f: &Fixture, ctx: &Ctx, st: &mut UpdateState, now: i64) -> Result<Outcome, UpdateError> {
        run_once(&f.srv, ctx, st, now, &mut |_| {}, &mut |_, _| {})
    }

    fn pe_blob(extra: usize) -> Vec<u8> {
        let mut v = vec![0u8; 512 + extra];
        v[0] = b'M';
        v[1] = b'Z';
        v[60] = 128;
        v[128..132].copy_from_slice(b"PE\0\0");
        v
    }

    struct Fixture {
        srv: Srv,
        dir: tempfile::TempDir,
        sk: ed25519_dalek::SigningKey,
        installer: Vec<u8>,
    }
    impl Fixture {
        /// Publishes a valid signed manifest (build 90) and a PE-looking installer.
        fn new() -> Fixture {
            let f = Fixture { srv: Srv::start(), dir: tempfile::tempdir().unwrap(), sk: signing_key(5), installer: pe_blob(2000) };
            f.publish(90, "2026-11-05T14:32:10Z", 0, &f.installer.clone(), None);
            f
        }
        fn publish(&self, build: u64, published: &str, min: u64, installer: &[u8], announced: Option<(u64, String)>) {
            let tmp = self.dir.path().join("probe.bin");
            std::fs::write(&tmp, installer).unwrap();
            let (sha, size) = sha256_file(&tmp).unwrap();
            let (size, sha) = announced.unwrap_or((size, sha));
            let url = self.srv.url("/downloads/B-setup.exe");
            let j = manifest_json(build, published, &url, "B-setup.exe", size, &sha, min);
            self.srv.put("/updates/latest.json", j.clone());
            self.srv.put("/updates/latest.json.sig", sign(&self.sk, j.as_bytes()));
            self.srv.put("/downloads/B-setup.exe", installer.to_vec());
        }
        fn ctx(&self, current_build: u64) -> Ctx {
            Ctx {
                manifest_url: self.srv.url("/updates/latest.json"),
                dev: true,
                keys: vec![trusted(&self.sk)],
                current_build,
                updates_dir: self.dir.path().join("updates"),
                auto: true,
                can_install: true,
                channel: String::new(),
            }
        }
    }
    const NOW: i64 = 1_793_889_130;

    #[test]
    fn happy_path_stages_a_verified_installer() {
        let f = Fixture::new();
        let mut st = UpdateState::default();
        match ro(&f, &f.ctx(83), &mut st, NOW).unwrap() {
            Outcome::Staged { manifest, installer, mandatory } => {
                assert_eq!(manifest.build, 90);
                assert!(!mandatory);
                assert_eq!(std::fs::read(&installer).unwrap(), f.installer);
                assert!(!f.dir.path().join("updates").join("B-setup.exe.part").exists());
            }
            o => panic!("unexpected {o:?}"),
        }
        assert_eq!(st.last_published_epoch, crate::timeutil::parse_utc("2026-11-05T14:32:10Z").unwrap());
    }

    #[test]
    fn up_to_date_downloads_nothing_and_downgrade_is_ignored() {
        let f = Fixture::new();
        let mut st = UpdateState::default();
        assert_eq!(ro(&f, &f.ctx(90), &mut st, NOW).unwrap(), Outcome::UpToDate);
        assert_eq!(ro(&f, &f.ctx(95), &mut st, NOW).unwrap(), Outcome::DowngradeIgnored);
        assert!(!f.srv.hits.lock().unwrap().iter().any(|u| u.starts_with("/downloads")), "no installer download");
    }

    #[test]
    fn replay_of_an_older_signed_manifest_is_refused() {
        let f = Fixture::new();
        let mut st = UpdateState::default();
        ro(&f, &f.ctx(83), &mut st, NOW).unwrap();
        // attacker re-serves a genuinely signed but older manifest
        f.publish(89, "2026-11-04T10:00:00Z", 0, &f.installer.clone(), None);
        assert_eq!(ro(&f, &f.ctx(83), &mut st, NOW), Err(UpdateError::Replay));
    }

    #[test]
    fn wrong_signing_key_is_refused_before_any_download() {
        let f = Fixture::new();
        let evil = signing_key(9);
        let j = String::from_utf8(f.srv.routes.lock().unwrap()["/updates/latest.json"].1.clone()).unwrap();
        f.srv.put("/updates/latest.json.sig", sign(&evil, j.as_bytes()));
        let mut st = UpdateState::default();
        assert!(matches!(ro(&f, &f.ctx(83), &mut st, NOW), Err(UpdateError::UnknownKey(_))));
        assert!(!f.srv.hits.lock().unwrap().iter().any(|u| u.starts_with("/downloads")));
        assert_eq!(st.last_published_epoch, 0, "state untouched by a rejected manifest");
    }

    #[test]
    fn wrong_hash_wrong_size_and_non_pe_installers_are_deleted() {
        let f = Fixture::new();
        let ctx = f.ctx(83);
        let leftovers = || std::fs::read_dir(f.dir.path().join("updates")).map(|d| d.count()).unwrap_or(0);
        // announced hash is wrong
        f.publish(90, "2026-11-05T14:32:10Z", 0, &f.installer.clone(), Some((f.installer.len() as u64, "0".repeat(64))));
        assert_eq!(ro(&f, &ctx, &mut UpdateState::default(), NOW), Err(UpdateError::HashMismatch));
        assert_eq!(leftovers(), 0);
        // body shorter than announced
        f.publish(90, "2026-11-05T14:32:10Z", 0, &f.installer.clone(), Some((f.installer.len() as u64 + 10, "0".repeat(64))));
        assert_eq!(ro(&f, &ctx, &mut UpdateState::default(), NOW), Err(UpdateError::SizeMismatch));
        assert_eq!(leftovers(), 0);
        // right hash and size, not an executable
        let text = b"hello, I am not a Windows executable. ..........................................................".to_vec();
        f.publish(90, "2026-11-05T14:32:10Z", 0, &text, None);
        assert_eq!(ro(&f, &ctx, &mut UpdateState::default(), NOW), Err(UpdateError::NotPe));
        assert_eq!(leftovers(), 0);
    }

    #[test]
    fn body_longer_than_announced_is_aborted() {
        let f = Fixture::new();
        let big = pe_blob(5000);
        // manifest announces the small installer's numbers, server sends a bigger body
        let tmp = f.dir.path().join("p.bin");
        std::fs::write(&tmp, &f.installer).unwrap();
        let (sha, size) = sha256_file(&tmp).unwrap();
        f.publish(90, "2026-11-05T14:32:10Z", 0, &f.installer.clone(), Some((size, sha)));
        f.srv.put("/downloads/B-setup.exe", big);
        assert_eq!(ro(&f, &f.ctx(83), &mut UpdateState::default(), NOW), Err(UpdateError::TooLarge));
    }

    #[test]
    fn oversized_manifest_is_refused() {
        let f = Fixture::new();
        f.srv.put("/updates/latest.json", vec![b' '; 70 * 1024]);
        assert_eq!(ro(&f, &f.ctx(83), &mut UpdateState::default(), NOW), Err(UpdateError::TooLarge));
    }

    #[test]
    fn redirect_to_another_host_is_refused_same_host_is_followed() {
        let f = Fixture::new();
        let ctx = f.ctx(83);
        let real = f.srv.routes.lock().unwrap()["/updates/latest.json"].1.clone();
        // off-host: "localhost" is not the manifest host "127.0.0.1"
        f.srv.redirect("/updates/latest.json", &format!("http://localhost:{}/updates/latest.json", f.srv.port));
        assert!(matches!(ro(&f, &ctx, &mut UpdateState::default(), NOW), Err(UpdateError::RedirectRefused(_))));
        // same host, relative: followed
        f.srv.redirect("/updates/latest.json", "/updates/real.json");
        f.srv.put("/updates/real.json", real.clone());
        // the .sig is fetched from latest.json.sig (still valid for the same bytes)
        assert!(matches!(ro(&f, &ctx, &mut UpdateState::default(), NOW), Ok(Outcome::Staged { .. })));
        // redirect loop is bounded
        f.srv.redirect("/updates/latest.json", "/updates/latest.json");
        assert!(matches!(ro(&f, &ctx, &mut UpdateState::default(), NOW), Err(UpdateError::RedirectRefused(_))));
    }

    #[test]
    fn plain_http_is_refused_outside_dev_mode_without_any_request() {
        let f = Fixture::new();
        let mut ctx = f.ctx(83);
        ctx.dev = false;
        assert!(matches!(ro(&f, &ctx, &mut UpdateState::default(), NOW), Err(UpdateError::InsecureUrl(_))));
        assert!(f.srv.hits.lock().unwrap().is_empty(), "no connection attempted");
    }

    #[test]
    fn notify_only_for_portable_or_auto_off_downloads_nothing() {
        let f = Fixture::new();
        for (auto, can) in [(true, false), (false, true)] {
            let mut ctx = f.ctx(83);
            ctx.auto = auto;
            ctx.can_install = can;
            assert!(matches!(ro(&f, &ctx, &mut UpdateState::default(), NOW).unwrap(), Outcome::Notify(_)));
        }
        assert!(!f.srv.hits.lock().unwrap().iter().any(|u| u.starts_with("/downloads")));
    }

    #[test]
    fn min_supported_build_marks_the_update_mandatory() {
        let f = Fixture::new();
        f.publish(90, "2026-11-05T14:32:10Z", 85, &f.installer.clone(), None);
        match ro(&f, &f.ctx(83), &mut UpdateState::default(), NOW).unwrap() {
            Outcome::Staged { mandatory, .. } => assert!(mandatory),
            o => panic!("{o:?}"),
        }
    }

    /// End to end on Windows: stage, then launch a harmless test program that writes a witness file.
    #[cfg(windows)]
    #[test]
    #[ignore = "compiles and runs a fresh exe: security products delete those; run explicitly with --ignored"]
    fn e2e_download_verify_launch_fake_installer_writes_witness() {
        let work = tempfile::tempdir().unwrap();
        let witness = work.path().join("witness.txt");
        let src = work.path().join("fake.rs");
        std::fs::write(&src, format!("fn main() {{ let a: Vec<String> = std::env::args().skip(1).collect(); std::fs::write({:?}, a.join(\" \")).unwrap(); }}", witness)).unwrap();
        let exe = work.path().join("fake-installer.exe");
        let rustc = std::env::var("RUSTC").unwrap_or_else(|_| "rustc".into());
        let st = std::process::Command::new(rustc).args(["--edition", "2021", "-C", "opt-level=0", "-o"]).arg(&exe).arg(&src).status().unwrap();
        assert!(st.success(), "could not build the fake installer");
        let bytes = std::fs::read(&exe).unwrap();
        assert_eq!(&bytes[..2], b"MZ");

        let f = Fixture::new();
        f.publish(90, "2026-11-05T14:32:10Z", 0, &bytes, None);
        let mut st = UpdateState::default();
        let Outcome::Staged { installer, .. } = ro(&f, &f.ctx(83), &mut st, NOW).unwrap() else { panic!("not staged") };
        assert!(!witness.exists(), "staging must not execute anything");

        // launch only after a fresh verification, as the application does
        let m = match ro(&f, &f.ctx(83), &mut st, NOW).unwrap() {
            Outcome::Staged { manifest, .. } => manifest,
            _ => unreachable!(),
        };
        crate::install::verify_installer(&installer, &m.installer).unwrap();
        crate::install::launch_installer(&installer, &work.path().join("tmp")).unwrap();
        let t0 = std::time::Instant::now();
        while !witness.exists() && t0.elapsed() < Duration::from_secs(15) {
            std::thread::sleep(Duration::from_millis(100));
        }
        assert!(witness.exists(), "fake installer did not run");
        std::thread::sleep(Duration::from_millis(200));
        assert_eq!(std::fs::read_to_string(&witness).unwrap(), INSTALL_ARGS.join(" "));
    }

    /// Negative end to end: a tampered installer is never launched (witness stays absent).
    #[cfg(windows)]
    #[test]
    #[ignore = "compiles and runs a fresh exe: security products delete those; run explicitly with --ignored"]
    fn e2e_tampered_installer_is_never_executed() {
        let work = tempfile::tempdir().unwrap();
        let witness = work.path().join("witness.txt");
        let src = work.path().join("fake.rs");
        std::fs::write(&src, format!("fn main() {{ std::fs::write({:?}, \"ran\").unwrap(); }}", witness)).unwrap();
        let exe = work.path().join("fake-installer.exe");
        let rustc = std::env::var("RUSTC").unwrap_or_else(|_| "rustc".into());
        assert!(std::process::Command::new(rustc).args(["--edition", "2021", "-o"]).arg(&exe).arg(&src).status().unwrap().success());
        let good = std::fs::read(&exe).unwrap();
        let mut tampered = good.clone();
        let n = tampered.len();
        tampered[n - 1] ^= 0xff; // one flipped byte after the manifest was signed
        let f = Fixture::new();
        f.publish(90, "2026-11-05T14:32:10Z", 0, &good, None);
        f.srv.put("/downloads/B-setup.exe", tampered);
        let r = ro(&f, &f.ctx(83), &mut UpdateState::default(), NOW);
        assert_eq!(r, Err(UpdateError::HashMismatch));
        assert!(!f.dir.path().join("updates").join("B-setup.exe").exists());
        std::thread::sleep(Duration::from_millis(300));
        assert!(!witness.exists());
    }

    #[test]
    fn a_build_whose_install_failed_is_not_downloaded_again() {
        let f = Fixture::new();
        let mut st = UpdateState::default();
        st.failed_build = 90;
        assert!(matches!(ro(&f, &f.ctx(83), &mut st, NOW).unwrap(), Outcome::KnownFailure(_)));
        assert!(!f.srv.hits.lock().unwrap().iter().any(|u| u.starts_with("/downloads")));
        // a newer build is still offered
        f.publish(91, "2026-11-06T10:00:00Z", 0, &f.installer.clone(), None);
        assert!(matches!(ro(&f, &f.ctx(83), &mut st, NOW).unwrap(), Outcome::Staged { .. }));
    }

    #[test]
    fn skipped_build_channel_mismatch_and_progress() {
        let f = Fixture::new();
        // user skipped build 90: no download, reported as skipped
        let mut st = UpdateState::default();
        st.skipped_build = 90;
        assert!(matches!(ro(&f, &f.ctx(83), &mut st, NOW).unwrap(), Outcome::Skipped(_)));
        assert!(!f.srv.hits.lock().unwrap().iter().any(|u| u.starts_with("/downloads")));
        // ...unless the update is mandatory
        f.publish(90, "2026-11-05T14:32:10Z", 85, &f.installer.clone(), None);
        assert!(matches!(ro(&f, &f.ctx(83), &mut st, NOW).unwrap(), Outcome::Staged { mandatory: true, .. }));
        // another channel is ignored
        let mut ctx = f.ctx(83);
        ctx.channel = "stable".into();
        assert_eq!(ro(&f, &ctx, &mut UpdateState::default(), NOW).unwrap(), Outcome::UpToDate);
        ctx.channel = "alpha-test".into();
        assert!(matches!(ro(&f, &ctx, &mut UpdateState::default(), NOW).unwrap(), Outcome::Staged { .. }));
        // progress reaches the announced size
        let (mut last, mut found) = ((0, 0), 0);
        run_once(&f.srv, &f.ctx(83), &mut UpdateState::default(), NOW, &mut |_| found += 1, &mut |d, t| last = (d, t)).unwrap();
        assert_eq!(last.0, f.installer.len() as u64);
        assert_eq!(found, 1);
    }
}
