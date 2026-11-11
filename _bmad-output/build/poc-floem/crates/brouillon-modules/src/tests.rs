use super::*;
use ed25519_dalek::{Signer, SigningKey};
use sha2::{Digest, Sha256};

fn sk() -> SigningKey {
    SigningKey::from_bytes(&[7u8; 32])
}

fn keys() -> Vec<TrustedKey> {
    let public = sk().verifying_key().to_bytes();
    vec![TrustedKey { key_id: brouillon_update::keys::key_id_of(&public), public }]
}

fn kid() -> String {
    keys()[0].key_id.clone()
}

fn hexs(b: &[u8]) -> String {
    brouillon_update::keys::hex(b)
}

/// Module signature line: domain tag + manifest bytes.
fn sign_module(manifest: &[u8]) -> String {
    let mut m = SIG_DOMAIN.to_vec();
    m.extend_from_slice(manifest);
    format!("brouillon-sig-v1 module {} {}\n", kid(), hexs(&sk().sign(&m).to_bytes()))
}

/// Signature as the UPDATE channel makes it (raw bytes, no domain, no `module`).
fn sign_update_style(manifest: &[u8]) -> String {
    format!("brouillon-sig-v1 {} {}\n", kid(), hexs(&sk().sign(manifest).to_bytes()))
}

fn fake_pe(extra: &[u8]) -> Vec<u8> {
    let mut v = vec![0u8; 128];
    v[0] = b'M';
    v[1] = b'Z';
    v[60] = 64;
    v[64..68].copy_from_slice(b"PE\0\0");
    v.extend_from_slice(extra);
    v
}

fn manifest(payload: &[u8], url: &str, build: u64, min_app: u64) -> String {
    format!(
        r#"{{"schema":1,"id":"docs","title":"Documents module","version":"0.1.0","build":{build},"published_utc":"2026-12-01T00:00:00Z","proto":1,"kinds":["pdf","docx"],"requires":{{"min_app_build":{min_app},"max_app_build":null,"min_proto":1}},"files":[{{"name":"brouillon-docs.exe","url":"{url}","size":{},"sha256":"{}"}}],"idle_ram_budget_mb":12,"min_rollback_build":{build}}}"#,
        payload.len(),
        hexs(&Sha256::digest(payload))
    )
}

fn dev_policy() -> UrlPolicy {
    UrlPolicy { dev: true, manifest_host: "127.0.0.1".into(), allowed_hosts: vec![] }
}

const URL: &str = "http://127.0.0.1:1/modules/docs/0.1.0/brouillon-docs.exe";

fn mgr(root: &Path, app_build: u64) -> ModuleManager {
    ModuleManager::new(root.join("modules"), keys(), app_build)
}

fn write_payload(dir: &Path, bytes: &[u8]) -> PathBuf {
    let p = dir.join("payload.exe");
    fs::write(&p, bytes).unwrap();
    p
}

#[test]
fn absent_offered_declined_touch_nothing_on_disk() {
    let t = tempfile::tempdir().unwrap();
    let mut m = mgr(t.path(), 100);
    assert_eq!(m.state("docs"), ModuleState::Absent);
    assert_eq!(m.offer("docs"), ModuleState::Offered);
    m.decline("docs");
    assert_eq!(m.state("docs"), ModuleState::Absent);
    assert!(!t.path().join("modules").exists(), "no folder is created before an install");
}

#[test]
fn full_path_absent_offered_installing_ready_loaded_and_shutdown() {
    let t = tempfile::tempdir().unwrap();
    let payload = fake_pe(b"module body");
    let src = write_payload(t.path(), &payload);
    let man = manifest(&payload, URL, 7, 50);
    let mut m = mgr(t.path(), 100);
    assert_eq!(m.offer("docs"), ModuleState::Offered);
    let job = m.begin_install(Consent::explicit_click(), "docs").unwrap();
    assert_eq!(m.state("docs"), ModuleState::Installing);
    // T-m12: a second click while installing does not start a second install
    assert_eq!(m.begin_install(Consent::explicit_click(), "docs").unwrap_err(), ModuleError::Busy);
    let entry = job.install_local(man.as_bytes(), &sign_module(man.as_bytes()), &src, &dev_policy()).unwrap();
    assert!(src.exists(), "the user's file is copied, never moved");
    assert_eq!(m.finish_install("docs", Ok(entry)), ModuleState::Ready);
    // not started yet: nothing is locked
    assert!(m.locks.is_empty());
    let l = m.request("docs").unwrap();
    assert_eq!(m.state("docs"), ModuleState::Loaded);
    assert!(l.exe.ends_with("brouillon-docs.exe") && l.exe.exists());
    // second request does not reopen anything
    assert_eq!(m.request("docs").unwrap(), l);
    // a fresh manager (next session) sees it installed, not loaded
    let mut m2 = mgr(t.path(), 100);
    assert_eq!(m2.state("docs"), ModuleState::Ready);
    assert_eq!(m.shutdown_all(), 1);
    assert_eq!(m.state("docs"), ModuleState::Ready);
}

#[test]
fn tampered_payload_is_rejected_and_nothing_stays() {
    let t = tempfile::tempdir().unwrap();
    let payload = fake_pe(b"module body");
    let man = manifest(&payload, URL, 7, 0);
    let mut bad = payload.clone();
    *bad.last_mut().unwrap() ^= 1;
    let src = write_payload(t.path(), &bad);
    let mut m = mgr(t.path(), 100);
    let job = m.begin_install(Consent::explicit_click(), "docs").unwrap();
    let e = job.install_local(man.as_bytes(), &sign_module(man.as_bytes()), &src, &dev_policy()).unwrap_err();
    assert_eq!(e, ModuleError::HashMismatch);
    assert!(matches!(m.finish_install("docs", Err(e)), ModuleState::Failed(_)));
    assert!(!t.path().join("modules/docs").exists());
    assert_eq!(fs::read_dir(t.path().join("modules/.staging")).unwrap().count(), 0, "staging deleted");
    // back to an offer only on an explicit action
    assert_eq!(m.offer("docs"), ModuleState::Offered);
}

#[test]
fn update_signature_does_not_verify_as_module_and_unsigned_or_edited_fail() {
    let t = tempfile::tempdir().unwrap();
    let payload = fake_pe(b"x");
    let man = manifest(&payload, URL, 7, 0);
    let src = write_payload(t.path(), &payload);
    let mut m = mgr(t.path(), 100);
    let job = m.begin_install(Consent::explicit_click(), "docs").unwrap();
    let p = dev_policy();
    assert_eq!(job.install_local(man.as_bytes(), &sign_update_style(man.as_bytes()), &src, &p).unwrap_err(), ModuleError::BadSignature);
    // a module signature relabelled as an update-style one is also refused (domain differs)
    let relabel = sign_module(man.as_bytes()).replace(" module ", " ");
    assert_eq!(job.install_local(man.as_bytes(), &relabel, &src, &p).unwrap_err(), ModuleError::BadSignature);
    // manifest edited after signing
    let sig = sign_module(man.as_bytes());
    let edited = man.replace("\"build\":7", "\"build\":8");
    assert_eq!(job.install_local(edited.as_bytes(), &sig, &src, &p).unwrap_err(), ModuleError::BadSignature);
}

#[test]
fn rollback_floor_survives_uninstall_and_compat_is_checked() {
    let t = tempfile::tempdir().unwrap();
    let payload = fake_pe(b"x");
    let src = write_payload(t.path(), &payload);
    let mut m = mgr(t.path(), 100);
    let p = dev_policy();
    let install = |m: &mut ModuleManager, build: u64, min_app: u64| {
        let man = manifest(&payload, URL, build, min_app);
        let job = m.begin_install(Consent::explicit_click(), "docs")?;
        let r = job.install_local(man.as_bytes(), &sign_module(man.as_bytes()), &src, &p);
        let st = m.finish_install("docs", r.clone());
        let _ = st;
        r
    };
    install(&mut m, 9, 0).unwrap();
    m.uninstall("docs").unwrap();
    assert_eq!(m.state("docs"), ModuleState::Absent);
    // T-m3 + T-m11: a lower build is refused even after uninstall
    assert_eq!(install(&mut m, 8, 0).unwrap_err(), ModuleError::Rollback);
    // app too old for this module
    assert!(matches!(install(&mut m, 10, 500).unwrap_err(), ModuleError::Incompatible(_)));
    install(&mut m, 10, 0).unwrap();
    // the floor is persisted for the next session
    let mut m2 = mgr(t.path(), 100);
    assert_eq!(m2.state("docs"), ModuleState::Ready);
    assert_eq!(m2.begin_install(Consent::explicit_click(), "docs").unwrap_err(), ModuleError::BadManifest("already installed".into()));
}

#[test]
fn swapped_or_deleted_module_is_refused_without_a_loop() {
    let t = tempfile::tempdir().unwrap();
    let payload = fake_pe(b"genuine");
    let src = write_payload(t.path(), &payload);
    let man = manifest(&payload, URL, 7, 0);
    let mut m = mgr(t.path(), 100);
    let job = m.begin_install(Consent::explicit_click(), "docs").unwrap();
    let r = job.install_local(man.as_bytes(), &sign_module(man.as_bytes()), &src, &dev_policy());
    m.finish_install("docs", r);
    let exe = t.path().join("modules/docs/0.1.0/brouillon-docs.exe");
    // T-m13: another binary of the same size between runs
    fs::write(&exe, fake_pe(b"genuinX")).unwrap();
    assert_eq!(m.request("docs").unwrap_err(), ModuleError::HashMismatch);
    assert!(matches!(m.state("docs"), ModuleState::Failed(_)));
    assert_eq!(m.request("docs").unwrap_err(), ModuleError::NotInstalled, "failed is not retried by itself");
    // T-m10: deleted after install
    let mut m2 = mgr(t.path(), 100);
    fs::remove_file(&exe).unwrap();
    assert!(matches!(m2.request("docs").unwrap_err(), ModuleError::Io(_)));
}

#[cfg(windows)]
#[test]
fn loaded_module_file_cannot_be_swapped_while_loaded() {
    let t = tempfile::tempdir().unwrap();
    let payload = fake_pe(b"genuine");
    let src = write_payload(t.path(), &payload);
    let man = manifest(&payload, URL, 7, 0);
    let mut m = mgr(t.path(), 100);
    let job = m.begin_install(Consent::explicit_click(), "docs").unwrap();
    let r = job.install_local(man.as_bytes(), &sign_module(man.as_bytes()), &src, &dev_policy());
    m.finish_install("docs", r);
    let l = m.request("docs").unwrap();
    assert!(fs::write(&l.exe, b"evil").is_err() && fs::remove_file(&l.exe).is_err());
    m.shutdown_all();
    assert!(fs::remove_file(&l.exe).is_ok());
}

#[test]
fn always_text_is_persisted() {
    let t = tempfile::tempdir().unwrap();
    let mut m = mgr(t.path(), 1);
    m.set_always_text("pdf", true).unwrap();
    m.set_always_text("docx", true).unwrap();
    m.set_always_text("docx", false).unwrap();
    assert_eq!(mgr(t.path(), 1).always_text(), vec!["pdf".to_string()]);
}

#[test]
fn download_only_after_consent_from_a_loopback_server_and_refuses_bad_hosts() {
    let t = tempfile::tempdir().unwrap();
    let server = tiny_http::Server::http("127.0.0.1:0").unwrap();
    let port = server.server_addr().to_ip().unwrap().port();
    let payload = fake_pe(b"downloaded module");
    let url = format!("http://127.0.0.1:{port}/modules/docs/0.1.0/brouillon-docs.exe");
    let man = manifest(&payload, &url, 7, 0);
    let sig = sign_module(man.as_bytes());
    let (man2, payload2) = (man.clone(), payload.clone());
    let hits = std::sync::Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
    let hits2 = hits.clone();
    std::thread::spawn(move || {
        for req in server.incoming_requests() {
            let path = req.url().to_string();
            hits2.lock().unwrap().push(path.clone());
            let body = match path.as_str() {
                "/modules/docs/module.json" => man2.clone().into_bytes(),
                "/modules/docs/module.json.sig" => sig.clone().into_bytes(),
                "/modules/docs/0.1.0/brouillon-docs.exe" => payload2.clone(),
                _ => {
                    let _ = req.respond(tiny_http::Response::from_data(b"no".to_vec()).with_status_code(404));
                    continue;
                }
            };
            let _ = req.respond(tiny_http::Response::from_data(body));
        }
    });
    let mut m = mgr(t.path(), 100);
    // before any click: states and offers cause zero requests
    m.offer("docs");
    m.decline("docs");
    m.offer("docs");
    assert!(hits.lock().unwrap().is_empty(), "no connection without a click");
    let job = m.begin_install(Consent::explicit_click(), "docs").unwrap();
    let base = format!("http://127.0.0.1:{port}/modules");
    let entry = job.download_and_install(Consent::explicit_click(), &base, &dev_policy(), true).unwrap();
    assert_eq!(m.finish_install("docs", Ok(entry)), ModuleState::Ready);
    // plain GETs of exactly three static paths, no query string
    let h = hits.lock().unwrap().clone();
    assert_eq!(h, vec!["/modules/docs/module.json", "/modules/docs/module.json.sig", "/modules/docs/0.1.0/brouillon-docs.exe"]);
    assert_eq!(m.request("docs").unwrap().version, "0.1.0");
    // an unreachable or missing module gives a plain failure and records nothing
    let mut m2 = mgr(&t.path().join("other"), 100);
    let job2 = m2.begin_install(Consent::explicit_click(), "nope-module").unwrap();
    let e = job2.download_and_install(Consent::explicit_click(), &base, &dev_policy(), true).unwrap_err();
    assert!(matches!(e, ModuleError::Network(_)));
    assert!(matches!(m2.finish_install("nope-module", Err(e)), ModuleState::Failed(_)));
}

#[test]
fn production_policy_refuses_other_hosts_and_plain_http() {
    let t = tempfile::tempdir().unwrap();
    let payload = fake_pe(b"x");
    let src = write_payload(t.path(), &payload);
    let mut m = mgr(t.path(), 100);
    let job = m.begin_install(Consent::explicit_click(), "docs").unwrap();
    let prod = UrlPolicy::production("brouillon.rochetta.fr");
    for url in ["http://brouillon.rochetta.fr/modules/docs/0.1.0/brouillon-docs.exe", "https://evil.example/modules/docs/0.1.0/brouillon-docs.exe"] {
        let man = manifest(&payload, url, 7, 0);
        assert!(matches!(job.install_local(man.as_bytes(), &sign_module(man.as_bytes()), &src, &prod).unwrap_err(), ModuleError::BadManifest(_)));
    }
}
