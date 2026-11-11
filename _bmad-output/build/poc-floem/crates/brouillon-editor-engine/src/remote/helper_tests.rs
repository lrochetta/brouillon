//! Integration tests of the helper client, the helper-backed provider and the save service.
//!
//! The server side is the REAL helper code (`brouillon_remote::helper::Helper`) running in-process,
//! speaking the real framed protocol over `std::io::pipe`s, in front of the helper crate's fake SFTP
//! server. There is no process, no ssh and no socket anywhere. What this cannot prove (real ssh.exe,
//! real sshd, host-key and password prompts through ssh) is listed in docs/REMOTE_FS.md.

use std::io::{Read, Write};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::ThreadId;
use std::time::{Duration, Instant};

use brouillon_remote::helper::Helper;
use brouillon_remote::sftp::ClientConfig;
use brouillon_remote::testkit::{spawn_fake, FakeFs, FakeHandle, Faults, Node};
use brouillon_remote::transport::{classify_prompt, Connector, Killer, Link, Prompter};
use brouillon_remote_proto::{ConnectParams, ErrorKind, Limits, RemoteError as WireError};
use brouillon_safesave as ss;

use super::client::{CallError, HelperClient, NoPrompts, PromptHandler, Timeouts};
use super::error::{RemoteError, RemoteErrorKind};
use super::helper::{call_to_fs, connect_params, AskKind, HelperFactory, HelperProvider, PromptBroker, RealOrDemoFactory};
use super::hub::ProviderFactory;
use super::path::RemotePath;
use super::provider::{RemoteProvider, SharedProvider};
use super::saving::{save_as_suggestion, view_of, ConflictKind, SaveEvent, SaveService, Submit};
use super::sites::{AuthMethod, SiteProfile};
use crate::fs_provider::FsProvider;

// ---------------------------------------------------------------- the rig

struct NoopKill;
impl Killer for NoopKill {
    fn kill(&mut self) {}
}

struct RigConnector {
    start: Mutex<Option<(FakeFs, Faults)>>,
    handle: Arc<Mutex<Option<FakeHandle>>>,
    /// Ask this ssh-style prompt during connect, like a password-protected server would.
    ask: Option<String>,
    asked: Arc<Mutex<Option<Option<String>>>>,
}

impl Connector for RigConnector {
    fn connect(&self, _p: &ConnectParams, prompter: Arc<dyn Prompter>) -> Result<Link, WireError> {
        if let Some(text) = &self.ask {
            let a = prompter.ask(classify_prompt(text), text);
            *self.asked.lock().unwrap() = Some(a.as_ref().map(|s| s.expose().to_string()));
            if a.is_none() {
                return Err(WireError::new(ErrorKind::AuthFailed, "cancelled"));
            }
        }
        let (fs, faults) = self.start.lock().unwrap().take().unwrap_or_else(|| (FakeFs::new(), Faults::default()));
        let ((r, w), h) = spawn_fake(fs, faults);
        *self.handle.lock().unwrap() = Some(h);
        Ok(Link { reader: r, writer: w, guard: Box::new(NoopKill), diag: Arc::new(Mutex::new(vec![])) })
    }
}

/// Copies every byte the helper sends us, so a test can look at the raw wire afterwards.
struct Tap<R: Read> {
    inner: R,
    seen: Arc<Mutex<Vec<u8>>>,
}
impl<R: Read> Read for Tap<R> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let n = self.inner.read(buf)?;
        self.seen.lock().unwrap().extend_from_slice(&buf[..n]);
        Ok(n)
    }
}

struct Rig {
    client: Arc<HelperClient>,
    handle: Arc<Mutex<Option<FakeHandle>>>,
    asked: Arc<Mutex<Option<Option<String>>>>,
    wire_in: Arc<Mutex<Vec<u8>>>,
    log: Arc<Mutex<Vec<String>>>,
}

impl Rig {
    fn fake(&self) -> Arc<Mutex<FakeFs>> {
        self.handle.lock().unwrap().as_ref().expect("connected").fs.clone()
    }
    fn server_log(&self) -> Vec<String> {
        self.handle.lock().unwrap().as_ref().expect("connected").log.lock().unwrap().clone()
    }
    fn data(&self, p: &str) -> Option<Vec<u8>> {
        self.fake().lock().unwrap().data(p)
    }
}

fn small_cfg() -> ClientConfig {
    ClientConfig { idle_timeout: Duration::from_secs(5), handshake_timeout: Duration::from_secs(10), window: 8, chunk: 4096, ..Default::default() }
}

fn start_client(fs: FakeFs, faults: Faults, ask: Option<&str>, prompts: Arc<dyn PromptHandler>, timeouts: Timeouts) -> Rig {
    let handle = Arc::new(Mutex::new(None));
    let asked = Arc::new(Mutex::new(None));
    let conn = RigConnector { start: Mutex::new(Some((fs, faults))), handle: handle.clone(), ask: ask.map(str::to_string), asked: asked.clone() };
    let (in_r, in_w) = std::io::pipe().unwrap();
    let (out_r, out_w) = std::io::pipe().unwrap();
    let helper = Helper::with_client_config(Box::new(out_w), Arc::new(conn), Limits::default(), small_cfg());
    std::thread::spawn(move || helper.serve(in_r));
    let wire_in = Arc::new(Mutex::new(Vec::new()));
    let log = Arc::new(Mutex::new(Vec::new()));
    let l2 = log.clone();
    let client = HelperClient::new(
        Box::new(Tap { inner: out_r, seen: wire_in.clone() }),
        Box::new(in_w),
        Box::new(|| {}),
        prompts,
        timeouts,
        Arc::new(move |m| l2.lock().unwrap().push(m.to_string())),
    );
    Rig { client: Arc::new(client), handle, asked, wire_in, log }
}

fn profile() -> SiteProfile {
    SiteProfile::from_quick("deploy@vps.example.org", AuthMethod::Agent, "/home/u").unwrap()
}

fn connected(fs: FakeFs, faults: Faults) -> (Rig, Arc<HelperProvider>) {
    let rig = start_client(fs, faults, None, Arc::new(NoPrompts), Timeouts::default());
    let p = HelperProvider::establish(rig.client.clone(), connect_params(&profile())).expect("connect");
    (rig, Arc::new(p))
}

fn pattern(n: usize) -> Vec<u8> {
    (0..n).map(|i| (i.wrapping_mul(7) % 251) as u8).collect()
}

const SITE: &str = "deploy-vps-example-org";
const FILE: &str = "/home/u/site/index.php";

fn site_fs(content: &[u8]) -> FakeFs {
    let mut fs = FakeFs::new();
    fs.dir("/home/u/site").file(FILE, content).dir("/home/u/site/css").link("/home/u/site/cur", "/home/u/site/css");
    fs
}

struct Svc {
    svc: SaveService,
    dir: tempfile::TempDir,
}

fn service() -> Svc {
    let dir = tempfile::tempdir().unwrap();
    let svc = SaveService::new(dir.path().join("journal"), dir.path().join("backups"));
    Svc { svc, dir }
}

fn key() -> (String, String) {
    (SITE.to_string(), FILE.to_string())
}

/// Open FILE through the provider like the explorer does and register its base.
fn open_tracked(s: &Svc, p: &Arc<HelperProvider>) -> Vec<u8> {
    let bytes = p.read(Path::new(FILE)).expect("read");
    let stamp = p.take_open_stamp(&RemotePath::new(FILE)).expect("a stamp was taken at read time");
    s.svc.attach(key(), &bytes, stamp);
    bytes
}

fn wait_event(s: &Svc) -> SaveEvent {
    let t = Instant::now();
    loop {
        if let Some(e) = s.svc.drain().into_iter().next() {
            return e;
        }
        assert!(t.elapsed() < Duration::from_secs(20), "no save event");
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn shared(p: &Arc<HelperProvider>) -> SharedProvider {
    p.clone()
}

fn leftovers(rig: &Rig) -> Vec<String> {
    rig.fake().lock().unwrap().nodes.keys().filter(|k| k.contains(".brouillon-")).cloned().collect()
}

// ---------------------------------------------------------------- provider basics

#[test]
fn explorer_calls_work_through_the_real_protocol() {
    let (_rig, p) = connected(site_fs(b"<?php\n"), Faults::default());
    let names: Vec<String> = p.list_dir(Path::new("/home/u/site")).unwrap().into_iter().map(|e| e.name).collect();
    assert_eq!(names, ["css", "cur", "index.php"], "folders first; a link to a folder counts as a folder");
    let cur = p.list_dir(Path::new("/home/u/site")).unwrap().into_iter().find(|e| e.name == "cur").unwrap();
    assert!(cur.is_dir && cur.is_symlink);
    assert_eq!(p.read(Path::new(FILE)).unwrap(), b"<?php\n");
    p.write(Path::new("/home/u/site/new.txt"), b"x").unwrap();
    p.mkdir(&RemotePath::new("/home/u/site/sub")).unwrap();
    p.rename(&RemotePath::new("/home/u/site/new.txt"), &RemotePath::new("/home/u/site/sub/n.txt")).unwrap();
    assert_eq!(p.rename(&RemotePath::new("/home/u/site/sub/n.txt"), &RemotePath::new(FILE)).unwrap_err().kind(), std::io::ErrorKind::AlreadyExists, "a rename never replaces");
    p.remove(&RemotePath::new("/home/u/site/sub/n.txt"), false).unwrap();
    p.remove(&RemotePath::new("/home/u/site/sub"), true).unwrap();
    assert_eq!(p.read(Path::new("/home/u/site/missing")).unwrap_err().kind(), std::io::ErrorKind::NotFound);
}

#[test]
fn hostile_listing_names_never_reach_the_explorer() {
    let faults = Faults { extra_names: vec![b"../../etc/passwd".to_vec(), b"a\x1b[31mb".to_vec(), b"ok.txt".to_vec()], ..Default::default() };
    let (_rig, p) = connected(site_fs(b"x"), faults);
    let names: Vec<String> = p.list_dir(Path::new("/home/u/site")).unwrap().into_iter().map(|e| e.name).collect();
    assert!(names.contains(&"ok.txt".to_string()));
    assert!(!names.iter().any(|n| n.contains("..") || n.contains('\x1b') || n.contains('/')), "{names:?}");
}

// ---------------------------------------------------------------- the safe save

#[test]
fn save_happy_path_is_verified_uploaded_and_leaves_nothing_behind() {
    let (rig, p) = connected(site_fs(b"<?php\n"), Faults::default());
    let s = service();
    open_tracked(&s, &p);
    let new = pattern(150_000);
    assert_eq!(s.svc.save(&key(), new.clone(), shared(&p)), Submit::Started);
    match wait_event(&s) {
        SaveEvent::Outcome { sent, outcome, .. } => {
            assert_eq!(sent, new);
            assert!(matches!(outcome, ss::SaveOutcome::Saved { non_atomic: false, .. }), "{outcome:?}");
            let v = view_of(&outcome);
            assert!(v.saved && v.state == super::state::RemoteSaveState::Synced);
        }
        other => panic!("{other:?}"),
    }
    assert_eq!(rig.data(FILE).unwrap(), new, "the server holds exactly the bytes we sent");
    assert!(leftovers(&rig).is_empty(), "no temp file left: {:?}", leftovers(&rig));
    assert!(s.svc.recoverable().is_empty(), "the journal entry is cleared after a verified save");
    // The replaced version went to the local backup first.
    let versions = ss::BackupStore::new(s.dir.path().join("backups")).list(SITE, FILE);
    assert_eq!(versions.len(), 1);
    // A second save works (the base was refreshed) and a touch-free server is not a conflict.
    let newer = b"third".to_vec();
    assert_eq!(s.svc.save(&key(), newer.clone(), shared(&p)), Submit::Started);
    assert!(matches!(wait_event(&s), SaveEvent::Outcome { outcome: ss::SaveOutcome::Saved { .. }, .. }));
    assert_eq!(rig.data(FILE).unwrap(), newer);
}

#[test]
fn a_conflict_writes_zero_bytes_and_keeps_the_buffer() {
    let (rig, p) = connected(site_fs(b"original\n"), Faults::default());
    let s = service();
    open_tracked(&s, &p);
    // Someone else edits the file on the server after we opened it.
    rig.fake().lock().unwrap().nodes.insert(FILE.into(), Node::File { data: b"edited by a colleague, longer\n".to_vec(), mode: 0o644, mtime: 5000 });
    let before = rig.server_log();
    assert_eq!(s.svc.save(&key(), b"my edit\n".to_vec(), shared(&p)), Submit::Started);
    let outcome = match wait_event(&s) {
        SaveEvent::Outcome { outcome, .. } => outcome,
        other => panic!("{other:?}"),
    };
    assert!(matches!(outcome, ss::SaveOutcome::Conflict { .. }), "{outcome:?}");
    let v = view_of(&outcome);
    assert!(!v.saved);
    assert_eq!(v.conflict, Some(ConflictKind::Changed));
    assert_eq!(rig.data(FILE).unwrap(), b"edited by a colleague, longer\n", "the colleague's file is untouched");
    assert_eq!(rig.server_log(), before, "not one mutating request reached the server");
    assert!(leftovers(&rig).is_empty());
    // The buffer is still held locally.
    assert_eq!(s.svc.recoverable().len(), 1);
}

#[test]
fn save_then_an_outside_change_then_save_again_is_a_conflict_with_zero_bytes_written() {
    let (rig, p) = connected(site_fs(b"original
"), Faults::default());
    let s = service();
    open_tracked(&s, &p);
    // 1. a normal verified save: the base moves to what we just wrote.
    assert_eq!(s.svc.save(&key(), b"first save
".to_vec(), shared(&p)), Submit::Started);
    assert!(matches!(wait_event(&s), SaveEvent::Outcome { outcome: ss::SaveOutcome::Saved { .. }, .. }));
    // 2. someone else changes the file behind our back (new bytes, newer mtime).
    rig.fake().lock().unwrap().nodes.insert(FILE.into(), Node::File { data: b"CHANGED BY SOMEONE ELSE
".to_vec(), mode: 0o644, mtime: 4_000_000_000 });
    let before = rig.server_log();
    // 3. save again: refused, nothing written.
    assert_eq!(s.svc.save(&key(), b"second save
".to_vec(), shared(&p)), Submit::Started);
    let outcome = match wait_event(&s) {
        SaveEvent::Outcome { outcome, .. } => outcome,
        other => panic!("{other:?}"),
    };
    assert!(matches!(outcome, ss::SaveOutcome::Conflict { .. }), "{outcome:?}");
    assert_eq!(view_of(&outcome).conflict, Some(ConflictKind::Changed));
    assert_eq!(rig.data(FILE).unwrap(), b"CHANGED BY SOMEONE ELSE
");
    assert_eq!(rig.server_log(), before, "not one mutating request reached the server");
    assert!(leftovers(&rig).is_empty());
}

#[test]
fn a_file_opened_without_a_stamp_still_never_overwrites_a_changed_server_file() {
    let (rig, p) = connected(site_fs(b"original
"), Faults::default());
    let s = service();
    let bytes = p.read(Path::new(FILE)).unwrap();
    s.svc.attach_unstamped(key(), &bytes);
    // Unchanged server file: the content hash matches, so the save goes through.
    assert_eq!(s.svc.save(&key(), b"mine
".to_vec(), shared(&p)), Submit::Started);
    assert!(matches!(wait_event(&s), SaveEvent::Outcome { outcome: ss::SaveOutcome::Saved { .. }, .. }));
    // Changed server file: conflict, zero bytes written.
    rig.fake().lock().unwrap().nodes.insert(FILE.into(), Node::File { data: b"theirs, different
".to_vec(), mode: 0o644, mtime: 4_000_000_000 });
    let before = rig.server_log();
    assert_eq!(s.svc.save(&key(), b"mine again
".to_vec(), shared(&p)), Submit::Started);
    assert!(matches!(wait_event(&s), SaveEvent::Outcome { outcome: ss::SaveOutcome::Conflict { .. }, .. }));
    assert_eq!(rig.data(FILE).unwrap(), b"theirs, different
");
    assert_eq!(rig.server_log(), before);

    // And a changed file right after an unstamped open (no verified save in between).
    let (rig2, p2) = connected(site_fs(b"original
"), Faults::default());
    let s2 = service();
    s2.svc.attach_unstamped(key(), b"original
");
    rig2.fake().lock().unwrap().nodes.insert(FILE.into(), Node::File { data: b"theirs
".to_vec(), mode: 0o644, mtime: 4_000_000_000 });
    assert_eq!(s2.svc.save(&key(), b"mine
".to_vec(), shared(&p2)), Submit::Started);
    assert!(matches!(wait_event(&s2), SaveEvent::Outcome { outcome: ss::SaveOutcome::Conflict { .. }, .. }));
    assert_eq!(rig2.data(FILE).unwrap(), b"theirs
");
}

#[test]
fn overwrite_after_a_conflict_backs_up_the_other_version_first() {
    let (rig, p) = connected(site_fs(b"original\n"), Faults::default());
    let s = service();
    open_tracked(&s, &p);
    rig.fake().lock().unwrap().nodes.insert(FILE.into(), Node::File { data: b"theirs\n".to_vec(), mode: 0o644, mtime: 5000 });
    s.svc.save(&key(), b"mine\n".to_vec(), shared(&p));
    wait_event(&s);
    s.svc.resolve(&key(), b"mine\n".to_vec(), ss::ConflictChoice::Overwrite, shared(&p));
    match wait_event(&s) {
        SaveEvent::Resolved { result: Ok(super::saving::ResolvedView::Saved(o)), .. } => assert!(matches!(o, ss::SaveOutcome::Saved { .. })),
        other => panic!("{other:?}"),
    }
    assert_eq!(rig.data(FILE).unwrap(), b"mine\n");
    let b = ss::BackupStore::new(s.dir.path().join("backups"));
    let mut found = false;
    for v in b.list(SITE, FILE) {
        found |= b.read(&v).unwrap() == b"theirs\n";
    }
    assert!(found, "their version was backed up before it was replaced");
}

#[test]
fn the_server_hanging_up_mid_upload_leaves_old_or_new_and_keeps_the_buffer() {
    let old = pattern(40_000);
    let faults = Faults { drop_after_write_bytes: Some(10_000), ..Default::default() };
    let (rig, p) = connected(site_fs(&old), faults);
    let s = service();
    open_tracked(&s, &p);
    let new = pattern(200_000).into_iter().map(|b| b ^ 0x55).collect::<Vec<u8>>();
    s.svc.save(&key(), new.clone(), shared(&p));
    let outcome = match wait_event(&s) {
        SaveEvent::Outcome { outcome, .. } => outcome,
        other => panic!("{other:?}"),
    };
    assert!(matches!(outcome, ss::SaveOutcome::Queued { .. } | ss::SaveOutcome::Failed(_)), "{outcome:?}");
    let v = view_of(&outcome);
    assert!(!v.saved && v.retry, "{v:?}");
    let on_server = rig.data(FILE).unwrap();
    assert!(on_server == old || on_server == new, "never a half-written target");
    // The buffer is safe: it is in the journal, byte for byte.
    let items = s.svc.recoverable();
    assert_eq!(items.len(), 1);
    let (path, bytes) = s.svc.adopt(SITE, &items[0].id).unwrap();
    assert_eq!((path.as_str(), bytes), (FILE, new.clone()));
    assert!(p.is_dead(), "a helper that lost its session is not reused");
}

#[test]
fn the_helper_process_dying_mid_upload_leaves_old_or_new_then_recovery_cleans_up() {
    let old = pattern(30_000);
    // Slow replies keep the upload in flight long enough to kill the helper in the middle of it.
    let faults = Faults { reply_delay: Duration::from_millis(15), ..Default::default() };
    let (rig, p) = connected(site_fs(&old), faults);
    let s = service();
    open_tracked(&s, &p);
    let new = pattern(600_000).into_iter().map(|b| b.wrapping_add(3)).collect::<Vec<u8>>();
    assert_eq!(s.svc.save(&key(), new.clone(), shared(&p)), Submit::Started);
    let client = rig.client.clone();
    let t = Instant::now();
    while !rig.fake().lock().unwrap().nodes.keys().any(|k| k.contains(".brouillon-")) {
        assert!(t.elapsed() < Duration::from_secs(20), "the upload never started");
        std::thread::sleep(Duration::from_millis(5));
    }
    client.kill("test: helper process killed");
    let outcome = match wait_event(&s) {
        SaveEvent::Outcome { outcome, .. } => outcome,
        other => panic!("{other:?}"),
    };
    let v = view_of(&outcome);
    assert!(!v.saved, "{outcome:?}");
    assert_eq!(v.state, super::state::RemoteSaveState::Pending);
    let on_server = rig.data(FILE).unwrap();
    assert!(on_server == old || on_server == new, "never a half-written target");
    assert!(p.is_dead());
    assert_eq!(p.list_dir(Path::new("/home/u")).unwrap_err().kind(), std::io::ErrorKind::BrokenPipe);
    assert_eq!(RemoteError::from_io(&p.list_dir(Path::new("/home/u")).unwrap_err()).kind, RemoteErrorKind::Dropped, "the explorer will mark the site Offline");
    // The buffer is still ours.
    assert_eq!(s.svc.recoverable().len(), 1);

    // Reconnect = a NEW helper in front of the same server contents. Recovery removes our orphan temp file.
    let server_nodes = rig.fake().lock().unwrap().nodes.clone();
    let mut fs2 = FakeFs::new();
    fs2.nodes = server_nodes;
    let (rig2, p2) = connected(fs2, Faults::default());
    s.svc.recover(SITE, shared(&p2));
    match wait_event(&s) {
        SaveEvent::Recovery { site, failed, .. } => assert!(site == SITE && failed.is_empty(), "{failed:?}"),
        other => panic!("{other:?}"),
    }
    assert!(leftovers(&rig2).is_empty(), "orphan temp removed: {:?}", leftovers(&rig2));
    let after = rig2.data(FILE).unwrap();
    assert!(after == old || after == new);
    // Recovery never uploads the buffer by itself; it is offered to the user.
    let items = s.svc.recoverable();
    assert_eq!(items.len(), 1);
    assert_eq!(s.svc.adopt(SITE, &items[0].id).unwrap().1, new);
    // After a reconnect the saved buffer goes through (the document was re-registered by `adopt`).
    s.svc.save(&key(), new.clone(), shared(&p2));
    match wait_event(&s) {
        SaveEvent::Outcome { outcome, .. } => assert!(matches!(outcome, ss::SaveOutcome::Saved { .. } | ss::SaveOutcome::Conflict { .. }), "{outcome:?}"),
        other => panic!("{other:?}"),
    }
}

#[test]
fn without_posix_rename_the_swap_is_reported_as_not_atomic() {
    let (rig, p) = connected(site_fs(b"a\n"), Faults { no_posix_rename: true, ..Default::default() });
    let s = service();
    open_tracked(&s, &p);
    s.svc.save(&key(), b"b\n".to_vec(), shared(&p));
    match wait_event(&s) {
        SaveEvent::Outcome { outcome: ss::SaveOutcome::Saved { non_atomic, .. }, .. } => assert!(non_atomic),
        other => panic!("{other:?}"),
    }
    assert_eq!(rig.data(FILE).unwrap(), b"b\n");
    assert!(leftovers(&rig).is_empty());
}

#[test]
fn a_second_save_while_one_runs_is_refused_not_queued_behind_the_ui() {
    let (_rig, p) = connected(site_fs(b"a\n"), Faults { reply_delay: Duration::from_millis(40), ..Default::default() });
    let s = service();
    open_tracked(&s, &p);
    assert_eq!(s.svc.save(&key(), b"one\n".to_vec(), shared(&p)), Submit::Started);
    assert_eq!(s.svc.save(&key(), b"two\n".to_vec(), shared(&p)), Submit::Busy);
    wait_event(&s);
    assert_eq!(s.svc.save(&("other".into(), "/x".into()), vec![], shared(&p)), Submit::NotTracked);
}

// ---------------------------------------------------------------- prompts and secrets

#[test]
fn a_typed_password_reaches_the_server_login_once_and_is_never_echoed_stored_or_logged() {
    const CANARY: &str = "CANARY-pw-7f3a91-do-not-leak";
    let broker = PromptBroker::new();
    struct ViaBroker(Arc<PromptBroker>);
    impl PromptHandler for ViaBroker {
        fn ask(&self, kind: brouillon_remote_proto::PromptKind, text: &str, gone: &AtomicBool) -> Option<brouillon_remote_proto::Secret> {
            self.0.ask("s1", "vps", kind.into(), text, gone, Duration::from_secs(30)).map(brouillon_remote_proto::Secret::new)
        }
    }
    let rig = start_client(site_fs(b"x"), Faults::default(), Some("deploy@vps.example.org's password: "), Arc::new(ViaBroker(broker.clone())), Timeouts::default());
    // The "UI": wait for the question, check what it would show, answer it.
    let ui = {
        let broker = broker.clone();
        std::thread::spawn(move || {
            let t = Instant::now();
            loop {
                if let Some(v) = broker.current() {
                    assert_eq!(v.kind, AskKind::Password);
                    assert!(v.kind.is_secret());
                    assert!(!format!("{v:?}").contains(CANARY));
                    assert!(broker.answer(v.id, Some(CANARY.to_string())));
                    return;
                }
                assert!(t.elapsed() < Duration::from_secs(10), "the question never reached the UI");
                std::thread::sleep(Duration::from_millis(5));
            }
        })
    };
    let p = HelperProvider::establish(rig.client.clone(), connect_params(&profile())).expect("connect after the prompt");
    ui.join().unwrap();
    // Positive control: the login side really received the secret (so the checks below are meaningful).
    assert_eq!(rig.asked.lock().unwrap().clone(), Some(Some(CANARY.to_string())));
    // Use the session a bit, including a failing call, a save and a timeout-free listing.
    let _ = p.read(Path::new("/nope"));
    let _ = p.list_dir(Path::new("/home/u"));
    let s = service();
    let _ = s.svc.recoverable();
    // Negative checks: nothing the client printed, received, returned or wrote contains the secret.
    assert!(!rig.log.lock().unwrap().iter().any(|l| l.contains(CANARY)), "client log");
    assert!(!String::from_utf8_lossy(&rig.wire_in.lock().unwrap()).contains(CANARY), "the helper never echoed it back");
    let err = p.read(Path::new("/nope")).unwrap_err().to_string();
    assert!(!err.contains(CANARY));
    assert!(!format!("{:?}", broker.current()).contains(CANARY));
    for e in walk(s.dir.path()) {
        assert!(!std::fs::read(&e).unwrap_or_default().windows(CANARY.len()).any(|w| w == CANARY.as_bytes()), "{}", e.display());
    }
}

fn walk(dir: &Path) -> Vec<std::path::PathBuf> {
    let mut out = vec![];
    if let Ok(rd) = std::fs::read_dir(dir) {
        for e in rd.flatten() {
            let p = e.path();
            if p.is_dir() {
                out.extend(walk(&p));
            } else {
                out.push(p);
            }
        }
    }
    out
}

#[test]
fn cancelling_a_prompt_fails_the_connect_without_hanging() {
    let broker = PromptBroker::new();
    struct ViaBroker(Arc<PromptBroker>);
    impl PromptHandler for ViaBroker {
        fn ask(&self, kind: brouillon_remote_proto::PromptKind, text: &str, gone: &AtomicBool) -> Option<brouillon_remote_proto::Secret> {
            self.0.ask("s1", "vps", kind.into(), text, gone, Duration::from_secs(30)).map(brouillon_remote_proto::Secret::new)
        }
    }
    let rig = start_client(FakeFs::new(), Faults::default(), Some("Are you sure you want to continue connecting (yes/no/[fingerprint])? "), Arc::new(ViaBroker(broker.clone())), Timeouts::default());
    let b2 = broker.clone();
    let ui = std::thread::spawn(move || {
        let t = Instant::now();
        loop {
            if let Some(v) = b2.current() {
                assert_eq!(v.kind, AskKind::HostKey, "a host key question is not a secret");
                assert!(!v.kind.is_secret());
                b2.answer(v.id, None);
                return;
            }
            assert!(t.elapsed() < Duration::from_secs(10));
            std::thread::sleep(Duration::from_millis(5));
        }
    });
    let e = HelperProvider::establish(rig.client.clone(), connect_params(&profile())).err().expect("cancelled");
    ui.join().unwrap();
    assert_eq!(e.kind, RemoteErrorKind::AuthFailed);
    assert!(rig.client.is_dead(), "a failed connect leaves no helper running");
}

#[test]
fn a_prompt_nobody_answers_is_withdrawn_when_the_helper_dies() {
    let broker = PromptBroker::new();
    let gone = Arc::new(AtomicBool::new(false));
    let (b, g) = (broker.clone(), gone.clone());
    let h = std::thread::spawn(move || b.ask("s", "vps", AskKind::Password, "pw: ", &g, Duration::from_secs(30)));
    let t = Instant::now();
    while broker.current().is_none() {
        assert!(t.elapsed() < Duration::from_secs(5));
        std::thread::sleep(Duration::from_millis(5));
    }
    gone.store(true, Ordering::SeqCst);
    assert_eq!(h.join().unwrap(), None);
    assert!(broker.current().is_none());
}

// ---------------------------------------------------------------- client robustness

#[test]
fn a_silent_helper_times_out_and_a_closed_pipe_is_a_dead_helper() {
    // Nothing answers on this pipe: the call must come back by itself.
    let (srv_in, _keep_open) = std::io::pipe().unwrap(); // helper stdout that never writes
    let (_cli_r, cli_w) = std::io::pipe().unwrap(); // helper stdin that nobody reads (stays open)
    let t = Timeouts { quick: Duration::from_millis(200), ..Timeouts::default() };
    let c = HelperClient::new(Box::new(srv_in), Box::new(cli_w), Box::new(|| {}), Arc::new(NoPrompts), t, Arc::new(|_| {}));
    let started = Instant::now();
    let e = c.ping().unwrap_err();
    assert_eq!(e, CallError::Timeout);
    assert!(started.elapsed() < Duration::from_secs(5));
    assert!(!c.is_dead(), "one slow answer does not kill the session");

    // The other end closes: EOF = dead, pending and later calls fail at once.
    let (r, w) = std::io::pipe().unwrap();
    let (_cr, cw) = std::io::pipe().unwrap();
    let c = HelperClient::new(Box::new(r), Box::new(cw), Box::new(|| {}), Arc::new(NoPrompts), Timeouts::default(), Arc::new(|_| {}));
    drop(w);
    let t = Instant::now();
    while !c.is_dead() {
        assert!(t.elapsed() < Duration::from_secs(5));
        std::thread::sleep(Duration::from_millis(5));
    }
    let t = Instant::now();
    assert!(matches!(c.ping(), Err(CallError::Dead(_))));
    assert!(t.elapsed() < Duration::from_millis(500), "a dead client fails immediately");
}

#[test]
fn garbage_from_the_helper_kills_the_session_instead_of_being_trusted() {
    let (r, mut w) = std::io::pipe().unwrap();
    let (_cr, cw) = std::io::pipe().unwrap();
    let c = HelperClient::new(Box::new(r), Box::new(cw), Box::new(|| {}), Arc::new(NoPrompts), Timeouts::default(), Arc::new(|_| {}));
    w.write_all(&[0xFF, 0xFF, 0xFF, 0xFF, 1]).unwrap(); // declares a 4 GiB frame
    let t = Instant::now();
    while !c.is_dead() {
        assert!(t.elapsed() < Duration::from_secs(5));
        std::thread::sleep(Duration::from_millis(5));
    }
    assert!(c.dead_reason().unwrap().contains("bad frame"));
}

#[test]
fn call_errors_are_classified_for_the_saver_and_the_explorer() {
    let transport = call_to_fs(CallError::Timeout);
    assert!(matches!(transport, ss::FsError::Transport(_)));
    assert!(matches!(call_to_fs(CallError::Dead("x".into())), ss::FsError::Transport(_)));
    let r = |k| call_to_fs(CallError::Remote(WireError::new(k, "m")));
    assert_eq!(r(ErrorKind::NotFound), ss::FsError::NotFound);
    assert_eq!(r(ErrorKind::AlreadyExists), ss::FsError::Exists);
    assert_eq!(r(ErrorKind::PermissionDenied), ss::FsError::PermissionDenied);
    assert!(matches!(r(ErrorKind::ConnectionLost), ss::FsError::Transport(_)));
    assert_eq!(call_to_fs(CallError::Remote(WireError::new(ErrorKind::Failure, "No space left on device"))), ss::FsError::NoSpace);
}

// ---------------------------------------------------------------- factory

#[test]
fn a_missing_helper_is_said_in_plain_words() {
    let f = HelperFactory::new(PromptBroker::new()).with_exe(std::path::PathBuf::from("C:/definitely/not/here/brouillon-remote.exe"));
    let e = f.connect(&profile()).err().unwrap();
    assert_eq!(e.kind, RemoteErrorKind::HelperMissing);
    assert!(e.plain().contains("brouillon-remote.exe") && e.plain().contains("Reinstall"));
    assert!(!e.plain().contains("definitely"), "the path is only in Details");
}

#[test]
fn the_fake_server_is_only_for_demo_and_the_flag() {
    struct Counting(std::sync::atomic::AtomicUsize);
    impl ProviderFactory for Counting {
        fn connect(&self, _: &SiteProfile) -> Result<SharedProvider, RemoteError> {
            self.0.fetch_add(1, Ordering::SeqCst);
            Err(RemoteError::new(RemoteErrorKind::Unreachable, "stub"))
        }
    }
    let real = Arc::new(Counting(Default::default()));
    let f = RealOrDemoFactory::new(real.clone(), false, None);
    let p = |h: &str| SiteProfile::from_quick(&format!("me@{h}"), AuthMethod::Agent, "").unwrap();
    assert!(f.connect(&p("demo")).is_ok());
    assert_eq!(real.0.load(Ordering::SeqCst), 0);
    assert!(f.connect(&p("example.org")).is_err());
    assert_eq!(real.0.load(Ordering::SeqCst), 1, "a real host goes to the real factory");
    let flagged = RealOrDemoFactory::new(real.clone(), true, None);
    assert!(flagged.connect(&p("example.org")).is_ok());
    assert_eq!(real.0.load(Ordering::SeqCst), 1);
}

#[test]
fn the_demo_password_hook_asks_through_the_broker_and_a_cancel_fails_the_connect() {
    let real = Arc::new(NeverFactory);
    let broker = PromptBroker::new();
    let f = RealOrDemoFactory::new(real, true, None).with_demo_password_prompt(Some(broker.clone()));
    let p = SiteProfile::from_quick("me@demo", AuthMethod::Agent, "").unwrap();
    // answered: the connect goes through and the question carried no value
    let b2 = broker.clone();
    let t = std::thread::spawn(move || loop {
        if let Some(q) = b2.current() {
            assert_eq!(q.kind, AskKind::Password);
            b2.answer(q.id, Some("x".into()));
            return;
        }
        std::thread::sleep(Duration::from_millis(10));
    });
    assert!(f.connect(&p).is_ok());
    t.join().unwrap();
    // cancelled: the connect fails
    let b3 = broker.clone();
    let t = std::thread::spawn(move || loop {
        if let Some(q) = b3.current() {
            b3.answer(q.id, None);
            return;
        }
        std::thread::sleep(Duration::from_millis(10));
    });
    assert!(f.connect(&p).is_err());
    t.join().unwrap();
    // without the hook the fake never asks
    let quiet = RealOrDemoFactory::new(Arc::new(NeverFactory), true, None);
    assert!(quiet.connect(&p).is_ok());
    assert!(broker.current().is_none());
}

struct NeverFactory;
impl ProviderFactory for NeverFactory {
    fn connect(&self, _: &SiteProfile) -> Result<SharedProvider, RemoteError> {
        Err(RemoteError::new(RemoteErrorKind::Unreachable, "never"))
    }
}

#[test]
fn connect_parameters_never_carry_a_secret_and_never_disable_host_key_checks() {
    let mut pr = profile();
    pr.auth = AuthMethod::KeyFile { path: "/keys/id".into() };
    let c = connect_params(&pr);
    assert_eq!(c.identity_file.as_deref(), Some("/keys/id"));
    assert_eq!(c.host_key_policy, brouillon_remote_proto::HostKeyPolicy::Ask);
    assert!(c.interactive);
    let json = serde_json::to_string(&c).unwrap();
    assert!(!json.to_lowercase().contains("password"));
}

// ---------------------------------------------------------------- the UI thread never waits on the server

struct GateFs {
    inner: ss::fake::FakeFs,
    open: Arc<(Mutex<bool>, std::sync::Condvar)>,
    threads: Arc<Mutex<Vec<ThreadId>>>,
}

impl GateFs {
    fn enter(&self) {
        self.threads.lock().unwrap().push(std::thread::current().id());
        let (m, cv) = &*self.open;
        let mut g = m.lock().unwrap();
        while !*g {
            g = cv.wait(g).unwrap();
        }
    }
}

impl ss::RemoteFs for GateFs {
    fn caps(&self) -> ss::Caps {
        self.inner.caps()
    }
    fn stat(&mut self, p: &str) -> ss::FsResult<Option<ss::Stat>> {
        self.enter();
        self.inner.stat(p)
    }
    fn realpath(&mut self, p: &str) -> ss::FsResult<String> {
        self.enter();
        self.inner.realpath(p)
    }
    fn read(&mut self, p: &str) -> ss::FsResult<Vec<u8>> {
        self.enter();
        self.inner.read(p)
    }
    fn write_new(&mut self, p: &str, d: &[u8], m: Option<u32>) -> ss::FsResult<()> {
        self.enter();
        self.inner.write_new(p, d, m)
    }
    fn write_truncate(&mut self, p: &str, d: &[u8]) -> ss::FsResult<()> {
        self.enter();
        self.inner.write_truncate(p, d)
    }
    fn set_mode(&mut self, p: &str, m: u32) -> ss::FsResult<()> {
        self.enter();
        self.inner.set_mode(p, m)
    }
    fn rename(&mut self, a: &str, b: &str) -> ss::FsResult<()> {
        self.enter();
        self.inner.rename(a, b)
    }
    fn rename_replace(&mut self, a: &str, b: &str) -> ss::FsResult<()> {
        self.enter();
        self.inner.rename_replace(a, b)
    }
    fn remove(&mut self, p: &str) -> ss::FsResult<()> {
        self.enter();
        self.inner.remove(p)
    }
}

struct GateProvider {
    fs: ss::fake::FakeFs,
    open: Arc<(Mutex<bool>, std::sync::Condvar)>,
    threads: Arc<Mutex<Vec<ThreadId>>>,
}

impl FsProvider for GateProvider {
    fn id(&self) -> &str {
        "gate"
    }
    fn list_dir(&self, _: &Path) -> std::io::Result<Vec<crate::fs_provider::FsEntry>> {
        Ok(vec![])
    }
    fn stat(&self, _: &Path) -> std::io::Result<crate::fs_provider::FsEntry> {
        Err(std::io::Error::other("unused"))
    }
    fn read(&self, _: &Path) -> std::io::Result<Vec<u8>> {
        Ok(vec![])
    }
    fn write(&self, _: &Path, _: &[u8]) -> std::io::Result<()> {
        Ok(())
    }
}

impl RemoteProvider for GateProvider {
    fn mkdir(&self, _: &RemotePath) -> std::io::Result<()> {
        Ok(())
    }
    fn rename(&self, _: &RemotePath, _: &RemotePath) -> std::io::Result<()> {
        Ok(())
    }
    fn remove(&self, _: &RemotePath, _: bool) -> std::io::Result<()> {
        Ok(())
    }
    fn safe_fs(&self) -> Option<Box<dyn ss::RemoteFs + Send>> {
        Some(Box::new(GateFs { inner: self.fs.clone(), open: self.open.clone(), threads: self.threads.clone() }))
    }
}

#[test]
fn no_remote_call_happens_on_the_ui_thread() {
    let fs = ss::fake::FakeFs::new(ss::Caps { posix_rename: true, hash: true, set_mode: true });
    fs.put_file(FILE, b"old\n", 0o644);
    let open = Arc::new((Mutex::new(false), std::sync::Condvar::new()));
    let threads = Arc::new(Mutex::new(Vec::new()));
    let p: SharedProvider = Arc::new(GateProvider { fs: fs.clone(), open: open.clone(), threads: threads.clone() });
    let s = service();
    s.svc.attach(key(), b"old\n", super::provider::OpenStamp { size: 4, mtime: fs_mtime(&fs, FILE), mode: 0o644 });
    let ui = std::thread::current().id();

    // The server is "blocked": the call must still return at once with the work running elsewhere.
    let t = Instant::now();
    assert_eq!(s.svc.save(&key(), b"new\n".to_vec(), p.clone()), Submit::Started);
    assert!(t.elapsed() < Duration::from_millis(500), "submitting a save does not wait on the server");
    assert!(s.svc.drain().is_empty(), "nothing finished while the server is stuck");
    let t = Instant::now();
    while threads.lock().unwrap().is_empty() {
        assert!(t.elapsed() < Duration::from_secs(5), "the worker never reached the server");
        std::thread::sleep(Duration::from_millis(5));
    }
    {
        let (m, cv) = &*open;
        *m.lock().unwrap() = true;
        cv.notify_all();
    }
    assert!(matches!(wait_event(&s), SaveEvent::Outcome { outcome: ss::SaveOutcome::Saved { .. }, .. }));
    let seen = threads.lock().unwrap().clone();
    assert!(!seen.is_empty() && seen.iter().all(|t| *t != ui), "every server call ran on the worker thread");
    assert_eq!(fs.get(FILE).unwrap(), b"new\n");
}

fn fs_mtime(fs: &ss::fake::FakeFs, path: &str) -> i64 {
    use ss::RemoteFs;
    let mut f = fs.clone();
    f.stat(path).unwrap().unwrap().mtime
}

// ---------------------------------------------------------------- pure mappings

#[test]
fn only_a_verified_save_marks_the_buffer_saved() {
    use ss::{Fail, FsError, SaveOutcome};
    let st = ss::Stat { kind: ss::Kind::File, size: 1, mtime: 1, mode: 0o644 };
    let all = [
        SaveOutcome::Conflict { remote: Some(st) },
        SaveOutcome::Stale { remote: st },
        SaveOutcome::RemoteGone,
        SaveOutcome::Queued { reason: "offline".into() },
        SaveOutcome::Failed(Fail::VerifyFailed),
        SaveOutcome::Failed(Fail::Fs(FsError::NoSpace)),
        SaveOutcome::Failed(Fail::Crashed),
    ];
    for o in &all {
        assert!(!view_of(o).saved, "{o:?}");
        assert_ne!(view_of(o).state, super::state::RemoteSaveState::Synced, "{o:?}");
    }
    assert_eq!(view_of(&all[2]).conflict, Some(ConflictKind::Gone));
    assert_eq!(view_of(&all[1]).conflict, Some(ConflictKind::Newer));
    assert!(view_of(&all[3]).retry);
    assert!(view_of(&SaveOutcome::Saved { verified: ss::Verification::Size, non_atomic: false }).saved);
}

#[test]
fn save_as_never_proposes_an_existing_name() {
    assert_eq!(save_as_suggestion("/var/www/index.php"), "/var/www/index.mine.php");
    assert_eq!(save_as_suggestion("/etc/hosts"), "/etc/hosts.mine");
    assert_eq!(save_as_suggestion("/a/.env"), "/a/.env.mine");
}

// ---------------------------------------------------------------- the real helper process (no ssh, no network)

/// `brouillon-remote.exe` built by the helper workspace into the same target dir (`cargo build` there first).
fn real_helper() -> Option<std::path::PathBuf> {
    let me = std::env::current_exe().ok()?;
    let p = me.parent()?.parent()?.join(if cfg!(windows) { "brouillon-remote.exe" } else { "brouillon-remote" });
    p.is_file().then_some(p)
}

#[test]
fn the_real_helper_process_speaks_the_protocol_over_real_pipes() {
    let Some(exe) = real_helper() else {
        eprintln!("SKIPPED: brouillon-remote.exe not built next to the test exe (build the helper workspace with the same CARGO_TARGET_DIR)");
        return;
    };
    let log = Arc::new(Mutex::new(Vec::<String>::new()));
    let l2 = log.clone();
    let client = super::helper::spawn_helper(&exe, Arc::new(NoPrompts), Timeouts::default(), Arc::new(move |m| l2.lock().unwrap().push(m.to_string()))).expect("spawn the helper");
    let hello = client.hello().expect("hello over real pipes");
    assert_eq!(hello.proto, 1);
    assert!(hello.caps.iter().any(|c| c == "sftp"));
    client.ping().expect("ping");
    // The helper refuses values ssh could read as options, before any process or connection is made.
    let mut bad = connect_params(&profile());
    bad.host = "-oProxyCommand=calc".into();
    match client.connect(bad) {
        Err(CallError::Remote(e)) => assert_eq!(e.kind, ErrorKind::InvalidArgument),
        other => panic!("{other:?}"),
    }
    // Not connected yet: file calls fail cleanly, the helper stays alive.
    assert!(matches!(client.stat(brouillon_remote_proto::RemotePath::root(), false), Err(CallError::Remote(e)) if e.kind == ErrorKind::NotConnected));
    assert!(!client.is_dead());
    // Dropping the client closes the helper's stdin (it then exits by itself, a reaper kills it after 3 s
    // if it does not). The test only proves the drop does not hang; the exit itself is not observed here.
    drop(client);
}

// ---------------------------------------------------------------- explorer + helper death

#[test]
fn a_dead_helper_marks_the_site_offline_once_and_a_reconnect_starts_a_fresh_helper() {
    use super::explorer::{Explorer, ExplorerNotice};
    use super::hub::RemoteHub;
    use super::state::ConnState;
    struct Seq(Mutex<Vec<Arc<HelperProvider>>>, Mutex<Vec<Rig>>);
    impl ProviderFactory for Seq {
        fn connect(&self, _: &SiteProfile) -> Result<SharedProvider, RemoteError> {
            let (rig, p) = connected(site_fs(b"x"), Faults::default());
            self.0.lock().unwrap().push(p.clone());
            self.1.lock().unwrap().push(rig);
            Ok(p)
        }
    }
    let seq = Arc::new(Seq(Mutex::new(vec![]), Mutex::new(vec![])));
    let mut ex = Explorer::new(RemoteHub::new(|| {}, seq.clone()));
    ex.connect(profile());
    let mut notices = Vec::new();
    let run = |ex: &mut Explorer, notices: &mut Vec<ExplorerNotice>, done: &dyn Fn(&mut Explorer) -> bool| {
        let t = Instant::now();
        loop {
            notices.extend(ex.pump());
            ex.rows();
            if done(ex) {
                return;
            }
            assert!(t.elapsed() < Duration::from_secs(10), "timed out: {notices:?}");
            std::thread::sleep(Duration::from_millis(2));
        }
    };
    run(&mut ex, &mut notices, &|ex| ex.rows().iter().any(|r| r.name == "site"));
    // The helper process dies.
    let first = seq.0.lock().unwrap()[0].clone();
    first.client().kill("test: helper killed");
    ex.refresh_dir(&RemotePath::new("/home/u"));
    ex.refresh_dir(&RemotePath::new("/home/u/site"));
    run(&mut ex, &mut notices, &|ex| matches!(ex.active_site().unwrap().conn, ConnState::Failed(_)));
    std::thread::sleep(Duration::from_millis(50));
    notices.extend(ex.pump());
    assert_eq!(notices.iter().filter(|n| matches!(n, ExplorerNotice::WentOffline { .. })).count(), 1, "{notices:?}");
    // Reconnect: a NEW helper (the factory is called again), the tree works again.
    ex.reconnect(&profile().id);
    run(&mut ex, &mut notices, &|ex| ex.active_site().unwrap().conn == ConnState::Connected && ex.rows().iter().any(|r| r.name == "site"));
    assert_eq!(seq.0.lock().unwrap().len(), 2);
    assert!(!seq.0.lock().unwrap()[1].is_dead());
}
