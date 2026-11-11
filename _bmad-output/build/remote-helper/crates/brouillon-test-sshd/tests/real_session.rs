//! Real-session tests: the shipped `brouillon-remote` executable, speaking its framed protocol on
//! stdio, drives the machine's REAL OpenSSH client (`ssh.exe -s sftp`) against the in-process
//! user-space test server (127.0.0.1 only, ephemeral port, generated keys, temp known_hosts and
//! ssh_config). Nothing outside loopback is contacted.
//!
//! All tests are `#[ignore]`. Run (single-threaded, one shared server):
//!
//! ```text
//! cargo build -p brouillon-remote
//! cargo test -p brouillon-test-sshd --test real_session -- --ignored --test-threads=1
//! ```
//!
//! (the harness also runs `cargo build -p brouillon-remote` itself so the exe is never stale).
//! Environment note: a security product on the dev machine may delete freshly built test
//! executables and delay their first launch; the timeouts below are generous on purpose.

use brouillon_remote_proto::*;
use brouillon_test_sshd::{generate_keypair, Options, TestSshd, TEST_PASSWORD};
use serde_json::Value;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{channel, Receiver};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock};
use std::time::{Duration, Instant};

// ------------------------------------------------------------------ shared environment

struct Env {
    srv: TestSshd,
    dir: PathBuf,
    helper_exe: PathBuf,
    ssh_config: PathBuf,
}

static ENV: OnceLock<Env> = OnceLock::new();
static SERIAL: Mutex<()> = Mutex::new(());

fn env() -> &'static Env {
    ENV.get_or_init(|| {
        let dir = std::env::temp_dir().join(format!("brouillon-sshd-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let srv = TestSshd::start(Options { root: dir.join("sandbox"), key_dir: dir.join("keys"), password_auth: true }).expect("test sshd starts");
        // The helper exe must match the sources: build it (no-op when fresh).
        let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let ws = manifest.parent().unwrap().parent().unwrap();
        let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".into());
        let st = Command::new(cargo).args(["build", "-j", "2", "-p", "brouillon-remote"]).current_dir(ws).status().expect("cargo build");
        assert!(st.success(), "cargo build -p brouillon-remote failed");
        let me = std::env::current_exe().unwrap();
        let helper_exe = me.parent().unwrap().parent().unwrap().join(if cfg!(windows) { "brouillon-remote.exe" } else { "brouillon-remote" });
        assert!(helper_exe.is_file(), "helper exe not found at {}", helper_exe.display());
        let empty = dir.join("empty_global_known_hosts");
        std::fs::write(&empty, "").unwrap();
        let ssh_config = dir.join("ssh_config");
        std::fs::write(
            &ssh_config,
            format!(
                "Host *\n  GlobalKnownHostsFile {}\n  UpdateHostKeys no\n  IdentityAgent none\n  CheckHostIP no\n  PreferredAuthentications publickey,password\n",
                empty.to_string_lossy().replace('\\', "/")
            ),
        )
        .unwrap();
        Env { srv, dir, helper_exe, ssh_config }
    })
}

/// Serialise the tests (one server, one-shot faults) and reset server state.
fn lock() -> (MutexGuard<'static, ()>, &'static Env) {
    let g = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let e = env();
    e.srv.faults.reset();
    e.srv.clear_log();
    (g, e)
}

fn p(s: &str) -> RemotePath {
    RemotePath::parse(s).unwrap()
}

fn sandbox_dir(e: &Env, name: &str) -> PathBuf {
    let d = e.srv.real(&format!("/{name}"));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn fwd(p: &Path) -> String {
    p.to_string_lossy().replace('\\', "/")
}

fn known_hosts_real(e: &Env, name: &str) -> PathBuf {
    let f = e.dir.join(format!("kh_{name}"));
    std::fs::write(&f, e.srv.known_hosts_line()).unwrap();
    f
}
fn known_hosts_empty(e: &Env, name: &str) -> PathBuf {
    let f = e.dir.join(format!("kh_{name}"));
    std::fs::write(&f, "").unwrap();
    f
}
fn known_hosts_changed(e: &Env, name: &str) -> PathBuf {
    let f = e.dir.join(format!("kh_{name}"));
    let (_, other_pub) = generate_keypair();
    std::fs::write(&f, format!("[127.0.0.1]:{} {}\n", e.srv.port(), other_pub)).unwrap();
    f
}

fn params(e: &Env, kh: &Path, root: Option<&str>) -> ConnectParams {
    ConnectParams {
        host: "127.0.0.1".into(),
        port: Some(e.srv.port()),
        user: Some("tester".into()),
        identity_file: Some(e.srv.client_key.to_string_lossy().into_owned()),
        proxy_jump: None,
        host_key_policy: HostKeyPolicy::Strict,
        known_hosts_file: Some(kh.to_string_lossy().into_owned()),
        ssh_config_file: Some(e.ssh_config.to_string_lossy().into_owned()),
        interactive: false,
        connect_timeout_s: Some(10),
        ssh_path: None,
        root: root.map(p),
    }
}

// ------------------------------------------------------------------ the editor-side driver

enum Item {
    Msg(ServerMessage),
    DataBytes { id: u64, offset: u64, bytes: Vec<u8> },
    Eof,
}

struct Drv {
    child: Child,
    tx: Option<ChildStdin>,
    rx: Receiver<Item>,
    next: u64,
    raw: Arc<Mutex<Vec<u8>>>,
    keep_data: bool,
    stderr_path: PathBuf,
}

struct Reply {
    events: Vec<Event>,
    data: Vec<(u64, Vec<u8>)>,
    out: Result<Value, RemoteError>,
}

impl Reply {
    fn ok<T: serde::de::DeserializeOwned>(&self) -> T {
        serde_json::from_value(self.out.clone().unwrap_or_else(|e| panic!("expected success, got {e:?}; events: {:?}", self.events))).unwrap()
    }
    fn kind(&self) -> ErrorKind {
        match &self.out {
            Err(e) => e.kind,
            Ok(v) => panic!("expected an error, got {v}"),
        }
    }
    fn prompts(&self) -> Vec<(PromptKind, String)> {
        self.events.iter().filter_map(|e| if let Event::Prompt { kind, text, .. } = e { Some((*kind, text.clone())) } else { None }).collect()
    }
    fn bytes(&self) -> Vec<u8> {
        let mut v = vec![];
        let start = self.data.first().map(|(o, _)| *o as usize).unwrap_or(0);
        for (o, b) in &self.data {
            assert_eq!(*o as usize, start + v.len(), "DATA frames must be contiguous");
            v.extend_from_slice(b);
        }
        v
    }
}

/// How the scripted "user" answers prompts: `Some(Some(s))` answer, `Some(None)` cancel, `None` = fail the test.
type Answerer<'a> = &'a mut dyn FnMut(PromptKind, &str) -> Option<Option<String>>;

impl Drv {
    fn start(e: &Env, name: &str) -> Drv {
        let stderr_path = e.dir.join(format!("helper_stderr_{name}.txt"));
        let mut child = Command::new(&e.helper_exe)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::from(std::fs::File::create(&stderr_path).unwrap()))
            .spawn()
            .expect("start helper");
        let tx = child.stdin.take().unwrap();
        let mut out = child.stdout.take().unwrap();
        let (txc, rx) = channel();
        let raw = Arc::new(Mutex::new(Vec::new()));
        let raw2 = raw.clone();
        std::thread::spawn(move || loop {
            match read_frame(&mut out) {
                Ok(Some(Frame::Control(b))) => {
                    raw2.lock().unwrap().extend_from_slice(&b);
                    let m = ServerMessage::from_slice(&b).expect("helper emits valid messages");
                    if txc.send(Item::Msg(m)).is_err() {
                        break;
                    }
                }
                Ok(Some(Frame::Data { id, offset, bytes })) => {
                    if txc.send(Item::DataBytes { id, offset, bytes }).is_err() {
                        break;
                    }
                }
                _ => {
                    let _ = txc.send(Item::Eof);
                    break;
                }
            }
        });
        let mut d = Drv { child, tx: Some(tx), rx, next: 1, raw, keep_data: true, stderr_path };
        let id = d.new_id();
        d.send(id, Request::Hello { proto: PROTO_VERSION, client: "real-session-test".into() });
        d.wait(id, &mut |_, _| None, 60).out.unwrap();
        d
    }

    fn new_id(&mut self) -> u64 {
        self.next += 1;
        self.next
    }
    fn send(&mut self, id: u64, request: Request) {
        write_control(self.tx.as_mut().unwrap(), &RequestFrame { id, request }).unwrap();
    }

    fn wait(&mut self, id: u64, answer: Answerer, timeout_s: u64) -> Reply {
        let (mut events, mut data) = (vec![], vec![]);
        let deadline = Instant::now() + Duration::from_secs(timeout_s);
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            let item = self.rx.recv_timeout(left).unwrap_or_else(|_| panic!("helper went silent for request {id}; events so far: {events:?}"));
            match item {
                Item::DataBytes { id: d, offset, bytes } => {
                    if d == id && self.keep_data {
                        data.push((offset, bytes));
                    }
                }
                Item::Eof => panic!("helper closed stdout while waiting for {id}; events: {events:?}"),
                Item::Msg(ServerMessage::Event(ev)) => {
                    if let Event::Prompt { prompt_id, kind, text } = &ev {
                        match answer(*kind, text) {
                            Some(a) => {
                                let aid = self.new_id();
                                self.send(aid, Request::PromptAnswer { prompt_id: *prompt_id, secret: a.map(Secret::new) });
                            }
                            None => panic!("unexpected prompt {kind:?}: {text}"),
                        }
                    }
                    events.push(ev);
                }
                Item::Msg(ServerMessage::Result { id: r, result }) if r == id => return Reply { events, data, out: Ok(result) },
                Item::Msg(ServerMessage::Error { id: Some(r), error }) if r == id => return Reply { events, data, out: Err(error) },
                Item::Msg(_) => {}
            }
        }
    }

    fn call(&mut self, request: Request) -> Reply {
        self.call_with(request, &mut |_, _| None, 60)
    }
    fn call_with(&mut self, request: Request, answer: Answerer, timeout_s: u64) -> Reply {
        let id = self.new_id();
        self.send(id, request);
        self.wait(id, answer, timeout_s)
    }

    fn connect(&mut self, cp: ConnectParams) -> Reply {
        self.call_with(Request::Connect(cp), &mut |_, _| None, 90)
    }
    fn connect_with(&mut self, cp: ConnectParams, answer: Answerer) -> Reply {
        self.call_with(Request::Connect(cp), answer, 90)
    }

    /// Upload `data` honouring the editor-side flow control (<= write_window unacknowledged bytes).
    fn write_file(&mut self, path: &str, data: &[u8], create: CreateMode, perm: Option<u32>) -> Reply {
        let id = self.new_id();
        self.send(id, Request::Write { path: p(path), size: data.len() as u64, create, perm, expect: None, fsync: false, allow_symlink: false });
        self.stream_payload(id, data.len() as u64, &mut |off, n| data[off as usize..off as usize + n].to_vec())
    }

    fn stream_payload(&mut self, id: u64, size: u64, gen: &mut dyn FnMut(u64, usize) -> Vec<u8>) -> Reply {
        let (mut sent, mut acked) = (0u64, 0u64);
        let mut events = vec![];
        let deadline = Instant::now() + Duration::from_secs(300);
        while sent < size {
            let n = (size - sent).min(128 * 1024) as usize;
            if sent - acked + n as u64 > WRITE_WINDOW {
                let left = deadline.saturating_duration_since(Instant::now());
                match self.rx.recv_timeout(left).expect("helper went silent during upload") {
                    Item::Msg(ServerMessage::Event(Event::Progress { id: i, done, .. })) if i == id => acked = done,
                    Item::Msg(ServerMessage::Error { id: Some(i), error }) if i == id => return Reply { events, data: vec![], out: Err(error) },
                    Item::Msg(ServerMessage::Result { id: i, result }) if i == id => return Reply { events, data: vec![], out: Ok(result) },
                    Item::Msg(ServerMessage::Event(ev)) => events.push(ev),
                    Item::Eof => panic!("helper died during upload"),
                    _ => {}
                }
                continue;
            }
            let chunk = gen(sent, n);
            if write_data(self.tx.as_mut().unwrap(), id, sent, &chunk).is_err() {
                break; // the helper stopped reading (error already queued): collect it below
            }
            sent += n as u64;
        }
        let mut r = self.wait(id, &mut |_, _| None, 300);
        events.append(&mut r.events);
        r.events = events;
        r
    }

    fn raw_text(&self) -> String {
        String::from_utf8_lossy(&self.raw.lock().unwrap()).into_owned()
    }
    fn stderr_text(&self) -> String {
        std::fs::read_to_string(&self.stderr_path).unwrap_or_default()
    }

    fn close(mut self) {
        drop(self.tx.take());
        let t = Instant::now();
        loop {
            if let Ok(Some(_)) = self.child.try_wait() {
                return;
            }
            if t.elapsed() > Duration::from_secs(20) {
                let _ = self.child.kill();
                let _ = self.child.wait();
                panic!("helper did not exit after stdin closed");
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }
}

impl Drop for Drv {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn find_ssh() -> PathBuf {
    if let Some(r) = std::env::var_os("SystemRoot") {
        let c = PathBuf::from(r).join("System32").join("OpenSSH").join("ssh.exe");
        if c.is_file() {
            return c;
        }
    }
    PathBuf::from("ssh")
}

#[allow(dead_code)]
fn never(_: PromptKind, t: &str) -> Option<Option<String>> {
    panic!("unexpected prompt: {t}")
}

fn pattern_byte(i: u64) -> u8 {
    ((i.wrapping_mul(7)) ^ (i >> 8)) as u8
}
fn pattern(n: usize) -> Vec<u8> {
    (0..n as u64).map(pattern_byte).collect()
}

// ------------------------------------------------------------------ 1. key auth + read-only operations

#[test]
#[ignore = "real ssh.exe against the user-space test sshd"]
fn t01_key_auth_list_stat_read() {
    let (_g, e) = lock();
    let dir = sandbox_dir(e, "t01");
    std::fs::write(dir.join("hello.txt"), b"hello real sftp\n").unwrap();
    std::fs::create_dir(dir.join("sub")).unwrap();
    std::fs::write(dir.join("big.bin"), pattern(300_000)).unwrap();
    // many entries to exercise READDIR paging (server pages by 100)
    for i in 0..250 {
        std::fs::write(dir.join(format!("f{i:03}.txt")), b"x").unwrap();
    }
    let mut d = Drv::start(e, "t01");
    let kh = known_hosts_real(e, "t01");
    let r = d.connect(params(e, &kh, Some("/t01")));
    let c: ConnectResult = r.ok();
    assert_eq!(c.sftp_version, 3);
    assert!(c.posix_rename && c.fsync, "{c:?}");
    assert_eq!(c.home.as_wire_str(), "/t01", "root-relative home");
    assert!(r.events.contains(&Event::State { state: ConnState::Connected, reason: None }));

    let r = d.call(Request::ListDir { path: p("/t01") });
    let l: ListResult = r.ok();
    assert_eq!(l.count, 253, "{l:?}");
    let names: Vec<String> = r
        .events
        .iter()
        .filter_map(|e| if let Event::ListPage { entries, .. } = e { Some(entries.iter().map(|x| x.name.as_wire_str().to_string()).collect::<Vec<_>>()) } else { None })
        .flatten()
        .collect();
    assert!(names.contains(&"hello.txt".to_string()) && names.contains(&"sub".to_string()) && names.contains(&"f249.txt".to_string()));

    let st: StatResult = d.call(Request::Stat { path: p("/t01/hello.txt"), follow: false }).ok();
    assert_eq!((st.kind, st.size), (EntryKind::File, Some(16)));
    assert_eq!(st.mode, Some(0o644));
    let st: StatResult = d.call(Request::Stat { path: p("/t01/sub"), follow: false }).ok();
    assert_eq!(st.kind, EntryKind::Dir);
    assert_eq!(d.call(Request::Stat { path: p("/t01/nope"), follow: false }).kind(), ErrorKind::NotFound);

    let r = d.call(Request::Read { path: p("/t01/hello.txt"), offset: 0, len: None, max_bytes: None, allow_symlink: false });
    assert_eq!(r.bytes(), b"hello real sftp\n");
    let r = d.call(Request::Read { path: p("/t01/big.bin"), offset: 0, len: None, max_bytes: None, allow_symlink: false });
    assert_eq!(r.bytes(), pattern(300_000));
    let r = d.call(Request::Read { path: p("/t01/big.bin"), offset: 1000, len: Some(5000), max_bytes: None, allow_symlink: false });
    assert_eq!(r.bytes(), pattern(300_000)[1000..6000]);
    // outside the helper's root: refused locally, never sent
    assert_eq!(d.call(Request::Stat { path: p("/"), follow: false }).kind(), ErrorKind::OutsideRoot);

    let log = e.srv.log();
    assert!(log.iter().any(|l| l == "subsystem: sftp"), "{log:?}");
    assert!(log.iter().any(|l| l.starts_with("auth: publickey") && l.ends_with("accepted=true")), "{log:?}");
    assert!(!log.iter().any(|l| l.starts_with("REFUSED")), "ssh asked for more than sftp: {log:?}");
    d.close();
}

// ------------------------------------------------------------------ 2. mutations

#[test]
#[ignore = "real ssh.exe against the user-space test sshd"]
fn t02_write_overwrite_rename_remove_mkdir_modes() {
    let (_g, e) = lock();
    let dir = sandbox_dir(e, "t02");
    let kh = known_hosts_real(e, "t02");
    let mut d = Drv::start(e, "t02");
    d.connect(params(e, &kh, Some("/t02"))).ok::<ConnectResult>();

    // new file with an explicit mode
    let v1 = pattern(70_000);
    let w: WriteResult = d.write_file("/t02/a.txt", &v1, CreateMode::New, Some(0o600)).ok();
    assert_eq!(w.size, 70_000);
    assert_eq!(std::fs::read(dir.join("a.txt")).unwrap(), v1);
    let st: StatResult = d.call(Request::Stat { path: p("/t02/a.txt"), follow: false }).ok();
    assert_eq!(st.mode, Some(0o600), "mode set at creation survives a stat");
    // CreateMode::New refuses an existing file
    assert_eq!(d.write_file("/t02/a.txt", b"zzz", CreateMode::New, None).kind(), ErrorKind::AlreadyExists);

    // safe save: temp file + posix-rename over the target (mode of the temp file is what the target gets)
    let v2: Vec<u8> = pattern(130_000).iter().map(|b| b.wrapping_add(1)).collect();
    d.write_file("/t02/.a.txt.tmp", &v2, CreateMode::New, Some(0o600)).ok::<WriteResult>();
    let rn: RenameResult = d.call(Request::Rename { from: p("/t02/.a.txt.tmp"), to: p("/t02/a.txt"), overwrite: true, allow_nonatomic: false }).ok();
    assert!(rn.atomic);
    assert_eq!(std::fs::read(dir.join("a.txt")).unwrap(), v2);
    assert!(!dir.join(".a.txt.tmp").exists());
    assert!(e.srv.log().iter().any(|l| l == "sftp posix-rename"), "atomic replace must use posix-rename: {:?}", e.srv.log());
    let st: StatResult = d.call(Request::Stat { path: p("/t02/a.txt"), follow: false }).ok();
    assert_eq!((st.size, st.mode), (Some(130_000), Some(0o600)), "mode preserved across the atomic replace");

    // chmod
    d.call(Request::Chmod { path: p("/t02/a.txt"), mode: 0o755 }).ok::<Value>();
    let st: StatResult = d.call(Request::Stat { path: p("/t02/a.txt"), follow: false }).ok();
    assert_eq!(st.mode, Some(0o755));

    // plain rename (no overwrite) refuses an existing target, works on a free one
    std::fs::write(dir.join("b.txt"), b"b").unwrap();
    assert_eq!(d.call(Request::Rename { from: p("/t02/b.txt"), to: p("/t02/a.txt"), overwrite: false, allow_nonatomic: false }).kind(), ErrorKind::AlreadyExists);
    d.call(Request::Rename { from: p("/t02/b.txt"), to: p("/t02/c.txt"), overwrite: false, allow_nonatomic: false }).ok::<RenameResult>();
    assert!(dir.join("c.txt").exists() && !dir.join("b.txt").exists());

    // mkdir / rmdir / remove
    d.call(Request::Mkdir { path: p("/t02/d"), mode: Some(0o700) }).ok::<Value>();
    assert!(dir.join("d").is_dir());
    assert_eq!(d.call(Request::Mkdir { path: p("/t02/d"), mode: None }).kind(), ErrorKind::AlreadyExists);
    let st: StatResult = d.call(Request::Stat { path: p("/t02/d"), follow: false }).ok();
    assert_eq!((st.kind, st.mode), (EntryKind::Dir, Some(0o700)));
    d.call(Request::Rmdir { path: p("/t02/d") }).ok::<Value>();
    assert!(!dir.join("d").exists());
    d.call(Request::Remove { path: p("/t02/c.txt") }).ok::<Value>();
    assert!(!dir.join("c.txt").exists());
    assert_eq!(d.call(Request::Remove { path: p("/t02/c.txt") }).kind(), ErrorKind::NotFound);
    assert_eq!(d.call(Request::Remove { path: p("/t02/../etc") }).kind(), ErrorKind::OutsideRoot);

    // server without posix-rename: overwrite is refused unless the caller accepts a non-atomic replace
    d.close();
    e.srv.faults.hide_posix_rename.store(true, std::sync::atomic::Ordering::SeqCst);
    let mut d = Drv::start(e, "t02b");
    let c: ConnectResult = d.connect(params(e, &kh, Some("/t02"))).ok();
    assert!(!c.posix_rename);
    std::fs::write(dir.join("n1"), b"1").unwrap();
    std::fs::write(dir.join("n2"), b"2").unwrap();
    assert_eq!(d.call(Request::Rename { from: p("/t02/n1"), to: p("/t02/n2"), overwrite: true, allow_nonatomic: false }).kind(), ErrorKind::Unsupported);
    let rn: RenameResult = d.call(Request::Rename { from: p("/t02/n1"), to: p("/t02/n2"), overwrite: true, allow_nonatomic: true }).ok();
    assert!(!rn.atomic);
    assert_eq!(std::fs::read(dir.join("n2")).unwrap(), b"1");
    d.close();
}

// ------------------------------------------------------------------ 3. symlink confinement

#[test]
#[ignore = "real ssh.exe against the user-space test sshd"]
fn t03_symlink_confinement() {
    let (_g, e) = lock();
    let dir = sandbox_dir(e, "t03");
    let secret = sandbox_dir(e, "t03_secret");
    std::fs::write(secret.join("passwd"), b"top secret").unwrap();
    std::fs::write(dir.join("inside.txt"), b"inside").unwrap();
    e.srv.add_symlink("/t03/escape", "/t03_secret/passwd");
    e.srv.add_symlink("/t03/dotdot", "../t03_secret/passwd");
    e.srv.add_symlink("/t03/ok", "/t03/inside.txt");
    let kh = known_hosts_real(e, "t03");
    let mut d = Drv::start(e, "t03");
    d.connect(params(e, &kh, Some("/t03"))).ok::<ConnectResult>();

    // listing shows the links as symlinks
    let r = d.call(Request::ListDir { path: p("/t03") });
    let kinds: Vec<(String, EntryKind)> = r
        .events
        .iter()
        .filter_map(|e| if let Event::ListPage { entries, .. } = e { Some(entries.iter().map(|x| (x.name.as_wire_str().to_string(), x.kind)).collect::<Vec<_>>()) } else { None })
        .flatten()
        .collect();
    assert!(kinds.contains(&("escape".into(), EntryKind::Symlink)), "{kinds:?}");

    // lstat shows the link, stat(follow) reports where it goes
    let st: StatResult = d.call(Request::Stat { path: p("/t03/escape"), follow: false }).ok();
    assert_eq!(st.kind, EntryKind::Symlink);
    assert!(st.outside_root, "{st:?}");
    let st: StatResult = d.call(Request::Stat { path: p("/t03/ok"), follow: true }).ok();
    assert_eq!((st.kind, st.outside_root), (EntryKind::File, false));
    assert_eq!(st.resolved.as_ref().map(|r| r.as_wire_str()), Some("/t03/inside.txt"));

    // reading / writing through an escaping link is refused before the open
    for l in ["escape", "dotdot"] {
        let path = format!("/t03/{l}");
        assert_eq!(d.call(Request::Read { path: p(&path), offset: 0, len: None, max_bytes: None, allow_symlink: false }).kind(), ErrorKind::SymlinkRefused, "{l}");
        assert_eq!(d.write_file(&path, b"pwned", CreateMode::Truncate, None).kind(), ErrorKind::SymlinkRefused, "{l}");
    }
    assert_eq!(std::fs::read(secret.join("passwd")).unwrap(), b"top secret");
    // an inside link is fine
    assert_eq!(d.call(Request::Read { path: p("/t03/ok"), offset: 0, len: None, max_bytes: None, allow_symlink: false }).bytes(), b"inside");
    // with the explicit opt-in the caller may follow it
    assert_eq!(d.call(Request::Read { path: p("/t03/escape"), offset: 0, len: None, max_bytes: None, allow_symlink: true }).bytes(), b"top secret");
    // realpath of an escaping link is itself refused (the result is outside the root)
    assert_eq!(d.call(Request::Realpath { path: p("/t03/escape") }).kind(), ErrorKind::OutsideRoot);
    // `..` in the protocol is rejected before it can be sent
    assert!(RemotePath::parse("/t03/../t03_secret/passwd").is_err() || d.call(Request::Read { path: p("/t03/../t03_secret/passwd"), offset: 0, len: None, max_bytes: None, allow_symlink: false }).kind() == ErrorKind::OutsideRoot);
    d.close();
}

// ------------------------------------------------------------------ 4. host keys

fn ask_yes(kind: PromptKind, text: &str) -> Option<Option<String>> {
    assert_eq!(kind, PromptKind::HostKey, "{text}");
    Some(Some("yes".into()))
}

#[test]
#[ignore = "real ssh.exe against the user-space test sshd"]
fn t04_host_key_policies() {
    let (_g, e) = lock();
    sandbox_dir(e, "t04");

    // strict + unknown host key: hard failure, no prompt, nothing written
    let kh = known_hosts_empty(e, "t04a");
    let mut d = Drv::start(e, "t04a");
    let mut cp = params(e, &kh, Some("/t04"));
    let r = d.connect(cp.clone());
    assert_eq!(r.kind(), ErrorKind::HostKeyRejected, "{:?}", r.out);
    assert!(r.prompts().is_empty());
    assert_eq!(std::fs::read_to_string(&kh).unwrap(), "", "strict must not learn a key");

    // accept-new + unknown: trusted on first use, key written, second connection (strict) works
    let kh = known_hosts_empty(e, "t04b");
    cp.known_hosts_file = Some(kh.to_string_lossy().into_owned());
    cp.host_key_policy = HostKeyPolicy::AcceptNew;
    let r = d.connect(cp.clone());
    r.ok::<ConnectResult>();
    assert!(r.prompts().is_empty(), "accept-new never prompts");
    let learned = std::fs::read_to_string(&kh).unwrap();
    assert!(learned.contains("127.0.0.1") && learned.contains(e.srv.host_key_line.split(' ').nth(1).unwrap()), "known_hosts now: {learned}");
    d.call(Request::Disconnect {}).ok::<Value>();
    cp.host_key_policy = HostKeyPolicy::Strict;
    d.connect(cp.clone()).ok::<ConnectResult>();
    d.close();

    // ask + interactive + unknown: a HostKey prompt event reaches the editor; "yes" accepts and records it
    let kh = known_hosts_empty(e, "t04c");
    let mut d = Drv::start(e, "t04c");
    cp.known_hosts_file = Some(kh.to_string_lossy().into_owned());
    cp.host_key_policy = HostKeyPolicy::Ask;
    cp.interactive = true;
    let r = d.connect_with(cp.clone(), &mut ask_yes);
    r.ok::<ConnectResult>();
    let pr = r.prompts();
    assert_eq!(pr.len(), 1, "{pr:?}");
    assert!(pr[0].1.contains("ED25519") || pr[0].1.to_lowercase().contains("fingerprint"), "{pr:?}");
    assert!(std::fs::read_to_string(&kh).unwrap().contains("127.0.0.1"));
    d.close();

    // ask + refusal (editor cancels): connection fails with HostKeyRejected, nothing recorded
    let kh = known_hosts_empty(e, "t04d");
    let mut d = Drv::start(e, "t04d");
    cp.known_hosts_file = Some(kh.to_string_lossy().into_owned());
    let r = d.connect_with(cp.clone(), &mut |_, _| Some(None));
    assert_eq!(r.kind(), ErrorKind::HostKeyRejected, "{:?}", r.out);
    assert_eq!(r.prompts().len(), 1);
    assert_eq!(std::fs::read_to_string(&kh).unwrap(), "");
    d.close();

    // ask + the editor types "no": must also be a refusal
    let kh = known_hosts_empty(e, "t04e");
    let mut d = Drv::start(e, "t04e");
    cp.known_hosts_file = Some(kh.to_string_lossy().into_owned());
    let r = d.connect_with(cp.clone(), &mut |_, _| Some(Some("no".into())));
    assert_eq!(r.kind(), ErrorKind::HostKeyRejected, "answering `no` must refuse: {:?}", r.out);
    assert_eq!(std::fs::read_to_string(&kh).unwrap(), "");
    d.close();
}

#[test]
#[ignore = "real ssh.exe against the user-space test sshd"]
fn t05_changed_host_key_is_a_hard_stop() {
    let (_g, e) = lock();
    sandbox_dir(e, "t05");
    let mut d = Drv::start(e, "t05");
    for (i, policy) in [HostKeyPolicy::Strict, HostKeyPolicy::AcceptNew, HostKeyPolicy::Ask].into_iter().enumerate() {
        let kh = known_hosts_changed(e, &format!("t05_{i}"));
        let before = std::fs::read_to_string(&kh).unwrap();
        let mut cp = params(e, &kh, Some("/t05"));
        cp.host_key_policy = policy;
        cp.interactive = policy == HostKeyPolicy::Ask;
        // even if a (malicious) prompt arrived, answering yes must not get us in
        let r = d.connect_with(cp, &mut |_, _| Some(Some("yes".into())));
        assert_eq!(r.kind(), ErrorKind::HostKeyChanged, "{policy:?}: {:?}", r.out);
        assert!(r.prompts().is_empty(), "{policy:?}: a changed key must not even prompt: {:?}", r.prompts());
        assert_eq!(std::fs::read_to_string(&kh).unwrap(), before, "known_hosts must be untouched");
    }
    assert!(!e.srv.log().iter().any(|l| l == "subsystem: sftp"), "no SFTP session may start");
    d.close();
}

// ------------------------------------------------------------------ 6. authentication

#[test]
#[ignore = "real ssh.exe against the user-space test sshd"]
fn t06_wrong_key_refused_and_password_through_askpass() {
    let (_g, e) = lock();
    sandbox_dir(e, "t06");
    let kh = known_hosts_real(e, "t06");
    let mut d = Drv::start(e, "t06");

    // wrong key, non-interactive (BatchMode): refused, never asks anything
    let mut cp = params(e, &kh, Some("/t06"));
    cp.identity_file = Some(e.srv.wrong_key.to_string_lossy().into_owned());
    let r = d.connect(cp.clone());
    assert_eq!(r.kind(), ErrorKind::AuthFailed, "{:?}", r.out);
    assert!(!e.srv.log().iter().any(|l| l == "subsystem: sftp"));

    // wrong key + interactive: ssh falls back to the password, asks through SSH_ASKPASS -> prompt event
    cp.interactive = true;
    let secret = TEST_PASSWORD;
    // wrong password twice (NumberOfPasswordPrompts=2) -> AuthFailed, two prompts
    let r = d.connect_with(cp.clone(), &mut |k, _| {
        assert_eq!(k, PromptKind::Password);
        Some(Some("not the password".into()))
    });
    assert_eq!(r.kind(), ErrorKind::AuthFailed, "{:?}", r.out);
    assert_eq!(r.prompts().len(), 2, "{:?} {:?} server log {:?}", r.prompts(), r.out, e.srv.log());

    // user cancels the prompt
    let r = d.connect_with(cp.clone(), &mut |_, _| Some(None));
    assert_eq!(r.kind(), ErrorKind::AuthFailed, "{:?}", r.out);

    // right password: connected; while ssh waits for the answer, inspect the live process command lines
    let mut seen_cmdlines = String::new();
    let r = d.connect_with(cp.clone(), &mut |k, text| {
        assert_eq!(k, PromptKind::Password, "{text}");
        assert!(text.to_lowercase().contains("password"), "{text}");
        seen_cmdlines = process_command_lines(&["ssh.exe", "brouillon-remote.exe"]);
        Some(Some(secret.into()))
    });
    let c: ConnectResult = r.ok();
    assert_eq!(c.sftp_version, 3);
    assert_eq!(r.prompts().len(), 1);
    assert!(e.srv.log().iter().any(|l| l.starts_with("auth: password") && l.ends_with("accepted=true")), "{:?}", e.srv.log());
    // the session works afterwards
    d.call(Request::ListDir { path: p("/t06") }).ok::<ListResult>();

    // the secret must not appear: in argv of any process we could see, in helper stdout frames, in helper stderr
    assert!(!seen_cmdlines.is_empty(), "could not read process command lines");
    assert!(seen_cmdlines.contains("-oStrictHostKeyChecking") || seen_cmdlines.contains("ssh"), "{seen_cmdlines}");
    assert!(!seen_cmdlines.contains(secret) && !seen_cmdlines.contains("t3st-Pa55"), "secret in a command line:\n{seen_cmdlines}");
    let frames = d.raw_text();
    assert!(!frames.contains(secret) && !frames.contains("Pa55"), "secret in helper output frames");
    assert!(!d.stderr_text().contains("Pa55"), "secret in helper stderr");
    assert!(!e.srv.log().iter().any(|l| l.contains("Pa55")), "server logged the password");
    println!("--- live command lines while ssh waited for the password ---\n{seen_cmdlines}");
    d.close();
}

/// Command lines of live processes with the given image names (Windows only; empty elsewhere).
fn process_command_lines(names: &[&str]) -> String {
    if !cfg!(windows) {
        return String::new();
    }
    let filter = names.iter().map(|n| format!("Name='{n}'")).collect::<Vec<_>>().join(" OR ");
    let o = Command::new("powershell")
        .args(["-NoProfile", "-NonInteractive", "-Command", &format!("Get-CimInstance Win32_Process -Filter \"{filter}\" | ForEach-Object {{ $_.Name + ' :: ' + $_.CommandLine }}")])
        .output();
    match o {
        Ok(o) => String::from_utf8_lossy(&o.stdout).into_owned(),
        Err(_) => String::new(),
    }
}

// ------------------------------------------------------------------ 7. faults

#[test]
#[ignore = "real ssh.exe against the user-space test sshd"]
fn t07_connection_dropped_mid_upload_leaves_old_or_new_only() {
    let (_g, e) = lock();
    let dir = sandbox_dir(e, "t07");
    let kh = known_hosts_real(e, "t07");
    let old = pattern(50_000);
    std::fs::write(dir.join("doc.txt"), &old).unwrap();
    let mut d = Drv::start(e, "t07");
    d.connect(params(e, &kh, Some("/t07"))).ok::<ConnectResult>();

    // the safe-save sequence (temp file, then posix-rename) with the connection cut after 300 KB
    let new: Vec<u8> = pattern(1_500_000).iter().map(|b| !b).collect();
    e.srv.faults.drop_after_write_bytes.store(300_000, std::sync::atomic::Ordering::SeqCst);
    let r = d.write_file("/t07/.doc.txt.tmp", &new, CreateMode::New, None);
    let k = r.kind();
    assert!(matches!(k, ErrorKind::ConnectionLost | ErrorKind::Timeout | ErrorKind::Protocol), "unexpected error kind {k:?}: {:?}", r.out);
    assert!(r.events.iter().any(|ev| matches!(ev, Event::State { state: ConnState::Lost, .. })) || {
        // the Lost event may arrive just after the error
        std::thread::sleep(Duration::from_millis(300));
        true
    });
    let tmp = std::fs::read(dir.join(".doc.txt.tmp")).unwrap();
    assert!(tmp.len() < new.len(), "the temp file must be partial ({} of {})", tmp.len(), new.len());
    // the target was never touched
    assert_eq!(std::fs::read(dir.join("doc.txt")).unwrap(), old, "old content must be intact");
    // the connection is gone: later operations say so, they do not hang
    assert!(matches!(d.call(Request::Stat { path: p("/t07/doc.txt"), follow: false }).kind(), ErrorKind::NotConnected | ErrorKind::ConnectionLost));

    // reconnect, redo: the leftover temp file blocks CreateMode::New (clear error), the editor removes it and retries
    d.connect(params(e, &kh, Some("/t07"))).ok::<ConnectResult>();
    assert_eq!(d.write_file("/t07/.doc.txt.tmp", &new, CreateMode::New, None).kind(), ErrorKind::AlreadyExists);
    d.call(Request::Remove { path: p("/t07/.doc.txt.tmp") }).ok::<Value>();
    d.write_file("/t07/.doc.txt.tmp", &new, CreateMode::New, None).ok::<WriteResult>();
    // a drop exactly at the rename: target is still old OR new, never partial
    *e.srv.faults.drop_on_op.lock().unwrap() = Some("posix-rename".into());
    let r = d.call(Request::Rename { from: p("/t07/.doc.txt.tmp"), to: p("/t07/doc.txt"), overwrite: true, allow_nonatomic: false });
    assert!(r.out.is_err(), "{:?}", r.out);
    let now = std::fs::read(dir.join("doc.txt")).unwrap();
    assert!(now == old || now == new, "target must be old or new, got {} bytes", now.len());
    d.connect(params(e, &kh, Some("/t07"))).ok::<ConnectResult>();
    d.call(Request::Rename { from: p("/t07/.doc.txt.tmp"), to: p("/t07/doc.txt"), overwrite: true, allow_nonatomic: false }).ok::<RenameResult>();
    assert_eq!(std::fs::read(dir.join("doc.txt")).unwrap(), new);
    d.close();
}

#[test]
#[ignore = "real ssh.exe against the user-space test sshd; takes ~25 s (idle timeout)"]
fn t08_stalled_server_times_out_and_idle_connection_survives() {
    let (_g, e) = lock();
    let dir = sandbox_dir(e, "t08");
    std::fs::write(dir.join("f"), b"data").unwrap();
    let kh = known_hosts_real(e, "t08");
    let mut d = Drv::start(e, "t08");
    d.connect(params(e, &kh, Some("/t08"))).ok::<ConnectResult>();

    // idle for longer than ServerAliveInterval (15 s): the keepalive exchange must keep the link up
    std::thread::sleep(Duration::from_secs(20));
    assert_eq!(d.call(Request::Read { path: p("/t08/f"), offset: 0, len: None, max_bytes: None, allow_symlink: false }).bytes(), b"data");

    // slow replies (400 ms each) still complete
    e.srv.faults.slow_ms.store(400, std::sync::atomic::Ordering::SeqCst);
    let t = Instant::now();
    d.call(Request::Stat { path: p("/t08/f"), follow: false }).ok::<StatResult>();
    assert!(t.elapsed() >= Duration::from_millis(350), "{:?}", t.elapsed());
    e.srv.faults.slow_ms.store(0, std::sync::atomic::Ordering::SeqCst);

    // server stops answering mid-upload but keeps the TCP/SSH connection open: the helper's idle timeout fires
    e.srv.faults.stall_after_write_bytes.store(100_000, std::sync::atomic::Ordering::SeqCst);
    let t = Instant::now();
    let r = d.write_file("/t08/stall.bin", &pattern(1_000_000), CreateMode::New, None);
    assert_eq!(r.kind(), ErrorKind::Timeout, "{:?}", r.out);
    let took = t.elapsed();
    assert!(took >= Duration::from_secs(15) && took < Duration::from_secs(60), "idle timeout took {took:?}");
    std::thread::sleep(Duration::from_millis(500));
    assert!(matches!(d.call(Request::Stat { path: p("/t08/f"), follow: false }).kind(), ErrorKind::NotConnected | ErrorKind::ConnectionLost));
    d.close();

    // a network drop while idle shows up as ConnectionLost / Lost on the next use, then reconnect works
    e.srv.faults.reset();
    let mut d = Drv::start(e, "t08b");
    d.connect(params(e, &kh, Some("/t08"))).ok::<ConnectResult>();
    e.srv.kill_all_connections();
    std::thread::sleep(Duration::from_millis(500));
    let k = d.call(Request::Stat { path: p("/t08/f"), follow: false }).kind();
    assert!(matches!(k, ErrorKind::ConnectionLost | ErrorKind::NotConnected | ErrorKind::Protocol), "{k:?}");
    d.connect(params(e, &kh, Some("/t08"))).ok::<ConnectResult>();
    d.call(Request::Stat { path: p("/t08/f"), follow: false }).ok::<StatResult>();
    d.close();
}

// ------------------------------------------------------------------ 9. large file

#[cfg(windows)]
fn peak_working_set(pid_child: &Child) -> Option<u64> {
    use std::os::windows::io::AsRawHandle;
    #[repr(C)]
    struct Pmc {
        cb: u32,
        page_faults: u32,
        peak_ws: usize,
        ws: usize,
        a: usize,
        b: usize,
        c: usize,
        d: usize,
        pagefile: usize,
        peak_pagefile: usize,
    }
    #[link(name = "kernel32")]
    extern "system" {
        fn K32GetProcessMemoryInfo(h: *mut std::ffi::c_void, p: *mut Pmc, cb: u32) -> i32;
    }
    let mut m = Pmc { cb: std::mem::size_of::<Pmc>() as u32, page_faults: 0, peak_ws: 0, ws: 0, a: 0, b: 0, c: 0, d: 0, pagefile: 0, peak_pagefile: 0 };
    let ok = unsafe { K32GetProcessMemoryInfo(pid_child.as_raw_handle() as *mut _, &mut m, m.cb) };
    (ok != 0).then_some(m.peak_ws as u64)
}
#[cfg(not(windows))]
fn peak_working_set(_: &Child) -> Option<u64> {
    None
}

#[test]
#[ignore = "real ssh.exe against the user-space test sshd; moves 2 x 50 MB"]
fn t09_large_file_throughput_and_helper_memory() {
    let (_g, e) = lock();
    let dir = sandbox_dir(e, "t09");
    let kh = known_hosts_real(e, "t09");
    let mut d = Drv::start(e, "t09");
    d.keep_data = false; // do not hold 50 MB in the test; verify on the fly instead
    d.connect(params(e, &kh, Some("/t09"))).ok::<ConnectResult>();
    let size: u64 = 50 * 1024 * 1024;

    // upload
    let id = d.new_id();
    d.send(id, Request::Write { path: p("/t09/big.bin"), size, create: CreateMode::New, perm: None, expect: None, fsync: false, allow_symlink: false });
    let t = Instant::now();
    let mut gen_time = Duration::ZERO;
    let r = d.stream_payload(id, size, &mut |off, n| {
        let g = Instant::now();
        let v = (off..off + n as u64).map(pattern_byte).collect();
        gen_time += g.elapsed();
        v
    });
    let w: WriteResult = r.ok();
    let up = t.elapsed();
    assert_eq!(w.size, size);
    assert_eq!(std::fs::metadata(dir.join("big.bin")).unwrap().len(), size);
    // spot-check content on disk
    {
        use std::io::{Read, Seek, SeekFrom};
        let mut f = std::fs::File::open(dir.join("big.bin")).unwrap();
        for off in [0u64, 1, 32767, 32768, 1_000_003, size / 2, size - 4096] {
            f.seek(SeekFrom::Start(off)).unwrap();
            let mut b = [0u8; 1000];
            let n = f.read(&mut b).unwrap();
            for (i, x) in b[..n].iter().enumerate() {
                assert_eq!(*x, pattern_byte(off + i as u64), "mismatch at {}", off + i as u64);
            }
        }
    }
    let ws_up = peak_working_set(&d.child);

    // download (the helper streams DATA frames; the test counts bytes and checks the pattern)
    let id = d.new_id();
    d.send(id, Request::Read { path: p("/t09/big.bin"), offset: 0, len: None, max_bytes: Some(size + 1), allow_symlink: false });
    let t = Instant::now();
    let mut got = 0u64;
    let mut bad = 0u64;
    let rr: ReadResult = loop {
        match d.rx.recv_timeout(Duration::from_secs(120)).expect("helper went silent during download") {
            Item::DataBytes { id: i, offset, bytes } if i == id => {
                assert_eq!(offset, got);
                // check a sample of each chunk (every 61st byte) to keep the test cheap
                for k in (0..bytes.len()).step_by(61) {
                    if bytes[k] != pattern_byte(offset + k as u64) {
                        bad += 1;
                    }
                }
                got += bytes.len() as u64;
            }
            Item::Msg(ServerMessage::Result { id: i, result }) if i == id => break serde_json::from_value(result).unwrap(),
            Item::Msg(ServerMessage::Error { id: Some(i), error }) if i == id => panic!("download failed: {error:?}"),
            Item::Eof => panic!("helper died"),
            _ => {}
        }
    };
    let down = t.elapsed();
    assert_eq!((got, rr.bytes, bad), (size, size, 0));
    let ws = peak_working_set(&d.child);
    let mb = size as f64 / 1_048_576.0;
    {
        use std::sync::atomic::Ordering::Relaxed;
        let f = &e.srv.faults;
        println!("server saw {} WRITE calls: busy {} ms, gaps between calls {} ms", f.write_calls.load(Relaxed), f.write_busy_ns.load(Relaxed) / 1_000_000, f.write_gap_ns.load(Relaxed) / 1_000_000);
    }
    println!("test-side payload generation took {gen_time:?} of the upload time");
    println!("THROUGHPUT upload: {:.1} MB/s ({:?}), download: {:.1} MB/s ({:?})", mb / up.as_secs_f64(), up, mb / down.as_secs_f64(), down);
    println!("HELPER peak working set: after upload {:?} MB, after download {:?} MB", ws_up.map(|x| x / 1_048_576), ws.map(|x| x / 1_048_576));
    if let Some(ws) = ws {
        assert!(ws < 64 * 1024 * 1024, "helper peak working set {} MB for a 50 MB transfer: it must stream, not buffer", ws / 1_048_576);
    }
    d.close();
}

// ------------------------------------------------------------------ 10. what ssh.exe really does with SSH_ASKPASS

struct Probe {
    exit_ok: bool,
    got_sftp_version: bool,
    stderr: String,
    recorded: String,
}

/// Run the real `ssh.exe -s sftp` directly (no helper) with a given environment and the
/// askpass-recorder as SSH_ASKPASS; password auth only (wrong key), stdin/stdout are pipes (no tty).
fn probe_ssh(e: &Env, name: &str, envs: &[(&str, &str)], askpass: &Path, extra: &[&str], kh: &Path) -> Probe {
    let rec = e.dir.join(format!("rec_{name}.txt"));
    let _ = std::fs::remove_file(&rec);
    let mut c = Command::new(find_ssh());
    c.args(["-F", &fwd(&e.ssh_config), "-a", "-x", "-T", "-oBatchMode=no", "-oConnectTimeout=10", "-oNumberOfPasswordPrompts=1", "-oStrictHostKeyChecking=yes"]);
    c.arg(format!("-oUserKnownHostsFile={}", fwd(kh)));
    c.args(["-i", &fwd(&e.srv.wrong_key), "-oIdentitiesOnly=yes", "-p", &e.srv.port().to_string(), "-l", "tester"]);
    c.args(extra);
    c.args(["-s", "--", "127.0.0.1", "sftp"]);
    c.env_remove("SSH_ASKPASS").env_remove("SSH_ASKPASS_REQUIRE").env_remove("DISPLAY");
    c.env("SSH_ASKPASS", askpass).env("REC_FILE", &rec).env("REC_ANSWER", TEST_PASSWORD);
    for (k, v) in envs {
        c.env(k, v);
    }
    c.stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        c.creation_flags(0x0800_0000);
    }
    let mut ch = c.spawn().unwrap();
    let mut stdin = ch.stdin.take().unwrap();
    let _ = stdin.write_all(&[0, 0, 0, 5, 1, 0, 0, 0, 3]); // SSH_FXP_INIT v3
    let _ = stdin.flush();
    let mut out = ch.stdout.take().unwrap();
    let (tx, rx) = channel();
    std::thread::spawn(move || {
        use std::io::Read;
        let mut b = [0u8; 64];
        let n = out.read(&mut b).unwrap_or(0);
        let _ = tx.send(b[..n].to_vec());
    });
    let first = rx.recv_timeout(Duration::from_secs(30)).unwrap_or_default();
    drop(stdin);
    let t = Instant::now();
    while ch.try_wait().unwrap().is_none() && t.elapsed() < Duration::from_secs(10) {
        std::thread::sleep(Duration::from_millis(20));
    }
    let _ = ch.kill();
    let st = ch.wait().unwrap();
    let mut err = String::new();
    use std::io::Read;
    let _ = ch.stderr.take().unwrap().read_to_string(&mut err);
    Probe { exit_ok: st.success(), got_sftp_version: first.len() >= 5 && first[4] == 2, stderr: err, recorded: std::fs::read_to_string(&rec).unwrap_or_default() }
}

#[test]
#[ignore = "real ssh.exe against the user-space test sshd; documents SSH_ASKPASS behaviour"]
fn t10_ssh_exe_askpass_behaviour_without_a_tty() {
    let (_g, e) = lock();
    let kh = known_hosts_real(e, "t10");
    let rec_exe = PathBuf::from(env!("CARGO_BIN_EXE_askpass-recorder"));
    let mut report = String::new();
    let mut run = |name: &str, envs: &[(&str, &str)], exe: &Path| {
        let pr = probe_ssh(e, name, envs, exe, &[], &kh);
        report.push_str(&format!(
            "[{name}] askpass called: {} | sftp session: {} | ssh exit ok: {}\n  recorded: {}\n  stderr: {}\n",
            !pr.recorded.is_empty(),
            pr.got_sftp_version,
            pr.exit_ok,
            pr.recorded.replace('\n', " / "),
            pr.stderr.trim().replace('\n', " / ")
        ));
        pr
    };
    // A: SSH_ASKPASS only, no tty, no DISPLAY (the default Windows situation)
    let a = run("A-no-REQUIRE-no-DISPLAY", &[], &rec_exe);
    // B: SSH_ASKPASS_REQUIRE=force (what the helper sets)
    let b = run("B-REQUIRE=force", &[("SSH_ASKPASS_REQUIRE", "force")], &rec_exe);
    // C: prefer
    let c = run("C-REQUIRE=prefer", &[("SSH_ASKPASS_REQUIRE", "prefer")], &rec_exe);
    // D: askpass in a directory whose name has a space
    let spaced = e.dir.join("dir with space");
    std::fs::create_dir_all(&spaced).unwrap();
    let rec_spaced = spaced.join(rec_exe.file_name().unwrap());
    std::fs::copy(&rec_exe, &rec_spaced).unwrap();
    let dd = run("D-REQUIRE=force-path-with-space", &[("SSH_ASKPASS_REQUIRE", "force")], &rec_spaced);
    println!("{report}");
    std::fs::write(e.dir.join("t10_report.txt"), &report).unwrap();
    let _ = (&a, &c);
    assert!(b.recorded.contains("SSH_ASKPASS_REQUIRE=Some(\"force\")"), "force mode must reach askpass:\n{report}");
    assert!(b.got_sftp_version, "with force the password must be obtained and the session opened:\n{report}");
    assert!(dd.got_sftp_version, "a path with a space must work (or the helper must be installed elsewhere):\n{report}");
}

// ------------------------------------------------------------------ 11. diagnostic: raw ssh.exe upload speed without the helper

fn sftp_pkt(ty: u8, body: &[u8]) -> Vec<u8> {
    let mut v = ((body.len() + 1) as u32).to_be_bytes().to_vec();
    v.push(ty);
    v.extend_from_slice(body);
    v
}
fn sstr(v: &mut Vec<u8>, s: &[u8]) {
    v.extend_from_slice(&(s.len() as u32).to_be_bytes());
    v.extend_from_slice(s);
}

#[test]
#[ignore = "real ssh.exe against the user-space test sshd; diagnostic: is ssh.exe's stdin the upload bottleneck?"]
fn t11_raw_ssh_exe_pipelined_upload_speed() {
    let (_g, e) = lock();
    sandbox_dir(e, "t11");
    let kh = known_hosts_real(e, "t11");
    let mut c = Command::new(find_ssh());
    c.args(["-F", &fwd(&e.ssh_config), "-a", "-x", "-T", "-oBatchMode=yes", "-oStrictHostKeyChecking=yes"]);
    c.arg(format!("-oUserKnownHostsFile={}", fwd(&kh)));
    c.args(["-i", &fwd(&e.srv.client_key), "-oIdentitiesOnly=yes", "-p", &e.srv.port().to_string(), "-l", "tester", "-s", "--", "127.0.0.1", "sftp"]);
    c.stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::null());
    let mut ch = c.spawn().unwrap();
    let mut stdin = ch.stdin.take().unwrap();
    let mut stdout = ch.stdout.take().unwrap();
    let chunk: usize = std::env::var("RAW_CHUNK").ok().and_then(|v| v.parse().ok()).unwrap_or(32 * 1024);
    let n_chunks = (50 * 1024 * 1024 / chunk) as u32; // 50 MiB
    let reader = std::thread::spawn(move || {
        use std::io::Read;
        let mut got = 0u32;
        let mut hdr = [0u8; 5];
        loop {
            if stdout.read_exact(&mut hdr).is_err() {
                break;
            }
            let len = u32::from_be_bytes(hdr[..4].try_into().unwrap()) as usize - 1;
            let mut body = vec![0u8; len];
            stdout.read_exact(&mut body).unwrap();
            if hdr[4] == 101 {
                got += 1;
                if got >= n_chunks {
                    break;
                }
            }
        }
        got
    });
    stdin.write_all(&sftp_pkt(1, &3u32.to_be_bytes())).unwrap();
    // OPEN id=1 "/t11/raw.bin" WRITE|CREAT|TRUNC
    let mut b = 1u32.to_be_bytes().to_vec();
    sstr(&mut b, b"/t11/raw.bin");
    b.extend_from_slice(&(2u32 | 8 | 0x10).to_be_bytes());
    b.extend_from_slice(&0u32.to_be_bytes());
    stdin.write_all(&sftp_pkt(3, &b)).unwrap();
    std::thread::sleep(Duration::from_millis(1500)); // the server's first handle is "h1"
    let payload = vec![0x5au8; chunk];
    let t = Instant::now();
    for i in 0..n_chunks {
        let mut b = (i + 2).to_be_bytes().to_vec();
        sstr(&mut b, b"h1");
        b.extend_from_slice(&(i as u64 * chunk as u64).to_be_bytes());
        sstr(&mut b, &payload);
        stdin.write_all(&sftp_pkt(6, &b)).unwrap();
    }
    stdin.flush().unwrap();
    let sent = t.elapsed();
    let got = reader.join().unwrap();
    let total = t.elapsed();
    println!("RAW ssh.exe pipelined 50 MiB upload with {chunk}-byte WRITEs: all written to ssh stdin after {sent:?}, all acknowledged after {total:?} ({:.1} MB/s), statuses={got}", 50.0 / total.as_secs_f64());
    drop(stdin);
    let _ = ch.kill();
    let _ = ch.wait();
}

