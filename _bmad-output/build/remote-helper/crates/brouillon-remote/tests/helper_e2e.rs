//! End-to-end protocol tests: an editor-side driver speaks the framed protocol to an in-process
//! `Helper` whose transport is the fake SFTP server.

use brouillon_remote::helper::Helper;
use brouillon_remote::testkit::*;
use brouillon_remote::transport::{Connector, Killer, Link, Prompter};
use brouillon_remote_proto::*;
use serde_json::Value;
use std::io::{PipeWriter, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{channel, Receiver, RecvTimeoutError};
use std::sync::{Arc, Mutex};
use std::time::Duration;

fn p(s: &str) -> RemotePath {
    RemotePath::parse(s).unwrap()
}

struct Noop(Arc<AtomicBool>);
impl Killer for Noop {
    fn kill(&mut self) {
        self.0.store(true, Ordering::Relaxed);
    }
}

#[derive(Clone)]
struct FakeConnector {
    fs: Arc<Mutex<Option<FakeFs>>>,
    faults: Faults,
    ask: Option<String>,
    asked: Arc<Mutex<Option<Option<String>>>>,
    killed: Arc<AtomicBool>,
    handle: Arc<Mutex<Option<FakeHandle>>>,
}

impl FakeConnector {
    fn new(fs: FakeFs, faults: Faults) -> FakeConnector {
        FakeConnector {
            fs: Arc::new(Mutex::new(Some(fs))),
            faults,
            ask: None,
            asked: Arc::new(Mutex::new(None)),
            killed: Arc::new(AtomicBool::new(false)),
            handle: Arc::new(Mutex::new(None)),
        }
    }
}

impl Connector for FakeConnector {
    fn connect(&self, _p: &ConnectParams, prompter: Arc<dyn Prompter>) -> Result<Link, RemoteError> {
        if let Some(text) = &self.ask {
            let a = prompter.ask(brouillon_remote::transport::classify_prompt(text), text);
            *self.asked.lock().unwrap() = Some(a.as_ref().map(|s| s.expose().to_string()));
            if a.is_none() {
                return Err(RemoteError::new(ErrorKind::AuthFailed, "cancelled"));
            }
        }
        let fs = self.fs.lock().unwrap().take().unwrap_or_else(FakeFs::new);
        let ((r, w), h) = spawn_fake(fs, self.faults.clone());
        *self.handle.lock().unwrap() = Some(h);
        Ok(Link { reader: r, writer: w, guard: Box::new(Noop(self.killed.clone())), diag: Arc::new(Mutex::new(vec![])) })
    }
}

enum Item {
    Msg(ServerMessage),
    Data { id: u64, offset: u64, bytes: Vec<u8> },
}

struct Driver {
    tx: PipeWriter,
    rx: Receiver<Item>,
    serve: Option<std::thread::JoinHandle<()>>,
    next: u64,
    raw_log: Arc<Mutex<Vec<u8>>>,
}

struct Reply {
    events: Vec<Event>,
    data: Vec<(u64, Vec<u8>)>,
    out: Result<Value, RemoteError>,
}

impl Driver {
    fn start(conn: FakeConnector, limits: Limits) -> Driver {
        let (in_r, in_w) = std::io::pipe().unwrap();
        let (out_r, out_w) = std::io::pipe().unwrap();
        let cfg = brouillon_remote::sftp::ClientConfig {
            idle_timeout: Duration::from_millis(1500),
            handshake_timeout: Duration::from_secs(5),
            window: 8,
            chunk: 4096,
            ..Default::default()
        };
        let helper = Helper::with_client_config(Box::new(out_w), Arc::new(conn), limits, cfg);
        let serve = std::thread::spawn(move || helper.serve(in_r));
        let (tx, rx) = channel();
        let raw_log = Arc::new(Mutex::new(Vec::new()));
        let log = raw_log.clone();
        std::thread::spawn(move || {
            let mut r = out_r;
            while let Ok(Some(f)) = read_frame(&mut r) {
                let item = match f {
                    Frame::Control(b) => {
                        log.lock().unwrap().extend_from_slice(&b);
                        Item::Msg(ServerMessage::from_slice(&b).expect("helper emits valid messages"))
                    }
                    Frame::Data { id, offset, bytes } => Item::Data { id, offset, bytes },
                };
                if tx.send(item).is_err() {
                    break;
                }
            }
        });
        Driver { tx: in_w, rx, serve: Some(serve), next: 1, raw_log }
    }

    fn new_id(&mut self) -> u64 {
        self.next += 1;
        self.next
    }

    fn send(&mut self, id: u64, request: Request) {
        write_control(&mut self.tx, &RequestFrame { id, request }).unwrap();
    }

    fn hello(&mut self) {
        let id = self.new_id();
        self.send(id, Request::Hello { proto: PROTO_VERSION, client: "test".into() });
        self.wait(id).out.unwrap();
    }

    fn call(&mut self, request: Request) -> Reply {
        let id = self.new_id();
        self.send(id, request);
        self.wait(id)
    }

    fn recv(&self) -> Item {
        self.rx.recv_timeout(Duration::from_secs(10)).expect("helper went silent")
    }

    /// Collect everything until the result/error for `id`.
    fn wait(&mut self, id: u64) -> Reply {
        let (mut events, mut data) = (vec![], vec![]);
        loop {
            match self.recv() {
                Item::Data { id: d, offset, bytes } if d == id => data.push((offset, bytes)),
                Item::Data { .. } => {}
                Item::Msg(ServerMessage::Event(e)) => events.push(e),
                Item::Msg(ServerMessage::Result { id: r, result }) if r == id => return Reply { events, data, out: Ok(result) },
                Item::Msg(ServerMessage::Error { id: Some(r), error }) if r == id => return Reply { events, data, out: Err(error) },
                Item::Msg(_) => {}
            }
        }
    }

    fn connect(&mut self) -> Reply {
        self.call(Request::Connect(connect_params(None)))
    }

    /// Write `data` honouring the editor-side flow-control rule (<= write_window unacknowledged).
    fn write_file(&mut self, path: &str, data: &[u8], create: CreateMode, expect: Option<Expect>) -> Reply {
        let id = self.new_id();
        self.send(
            id,
            Request::Write { path: p(path), size: data.len() as u64, create, perm: Some(0o644), expect, fsync: false, allow_symlink: false },
        );
        let (mut sent, mut acked) = (0u64, 0u64);
        let mut events = vec![];
        let mut off = 0usize;
        while off < data.len() {
            let n = (data.len() - off).min(100_000);
            if sent - acked + n as u64 > WRITE_WINDOW {
                match self.recv() {
                    Item::Msg(ServerMessage::Event(Event::Progress { id: i, done, .. })) if i == id => acked = done,
                    Item::Msg(ServerMessage::Error { id: Some(i), error }) if i == id => return Reply { events, data: vec![], out: Err(error) },
                    Item::Msg(ServerMessage::Event(e)) => events.push(e),
                    _ => {}
                }
                continue;
            }
            write_data(&mut self.tx, id, off as u64, &data[off..off + n]).unwrap();
            sent += n as u64;
            off += n;
        }
        let mut r = self.wait(id);
        events.append(&mut r.events);
        r.events = events;
        r
    }

    fn all_output(&self) -> String {
        String::from_utf8_lossy(&self.raw_log.lock().unwrap()).into_owned()
    }

    fn finish(mut self) {
        drop(self.tx);
        self.serve.take().unwrap().join().unwrap();
    }
}

fn connect_params(root: Option<&str>) -> ConnectParams {
    ConnectParams {
        host: "fake".into(),
        port: None,
        user: None,
        identity_file: None,
        proxy_jump: None,
        host_key_policy: HostKeyPolicy::Strict,
        known_hosts_file: None,
        ssh_config_file: None,
        interactive: true,
        connect_timeout_s: None,
        ssh_path: None,
        root: root.map(p),
        protocol: Protocol::Sftp,
        ftp: None,
    }
}

fn pattern(n: usize) -> Vec<u8> {
    (0..n).map(|i| (i.wrapping_mul(7) % 253) as u8).collect()
}

fn fs_with(files: &[(&str, Vec<u8>)]) -> FakeFs {
    let mut fs = FakeFs::new();
    for (n, d) in files {
        fs.file(n, d);
    }
    fs
}

fn err_kind(r: &Reply) -> ErrorKind {
    r.out.as_ref().unwrap_err().kind
}

// ------------------------------------------------------------------ tests

#[test]
fn hello_gate_and_version_negotiation() {
    let mut d = Driver::start(FakeConnector::new(FakeFs::new(), Faults::default()), Limits::default());
    let r = d.call(Request::Ping {});
    assert_eq!(err_kind(&r), ErrorKind::InvalidArgument, "hello first");
    let r = d.call(Request::Hello { proto: 99, client: "t".into() });
    assert_eq!(err_kind(&r), ErrorKind::Unsupported);
    let r = d.call(Request::Hello { proto: PROTO_VERSION, client: "t".into() });
    let h: HelloResult = serde_json::from_value(r.out.unwrap()).unwrap();
    assert_eq!(h.proto, 1);
    assert_eq!(h.limits, Limits::default());
    assert!(h.caps.iter().any(|c| c == "sftp"));
    d.call(Request::Ping {}).out.unwrap();
    d.finish();
}

#[test]
fn full_session_list_read_write_rename() {
    let big = pattern(300_000);
    let conn = FakeConnector::new(fs_with(&[("/home/u/a.txt", b"hello".to_vec()), ("/home/u/big.bin", big.clone())]), Faults::default());
    let handle = conn.handle.clone();
    let killed = conn.killed.clone();
    let mut d = Driver::start(conn, Limits::default());
    d.hello();
    assert_eq!(err_kind(&d.call(Request::ListDir { path: p("/home/u") })), ErrorKind::NotConnected);
    let r = d.connect();
    assert!(r.events.contains(&Event::State { state: ConnState::Connected, reason: None }));
    let c: ConnectResult = serde_json::from_value(r.out.unwrap()).unwrap();
    assert_eq!(c.home, p("/home/u"));
    assert!(c.posix_rename && c.fsync && c.sftp_version == 3);
    assert_eq!(err_kind(&d.connect()), ErrorKind::InvalidArgument, "second connect");

    // list: pages + result
    let r = d.call(Request::ListDir { path: p("/home/u") });
    let l: ListResult = serde_json::from_value(r.out.unwrap()).unwrap();
    let names: Vec<String> = r
        .events
        .iter()
        .filter_map(|e| if let Event::ListPage { entries, .. } = e { Some(entries.iter().map(|x| x.name.as_wire_str().to_string()).collect::<Vec<_>>()) } else { None })
        .flatten()
        .collect();
    assert_eq!(names, ["a.txt", "big.bin"]);
    assert_eq!(l.count, 2);

    // read: DATA frames in order, result agrees
    let r = d.call(Request::Read { path: p("/home/u/big.bin"), offset: 0, len: None, max_bytes: None, allow_symlink: false });
    let rr: ReadResult = serde_json::from_value(r.out.unwrap()).unwrap();
    assert_eq!((rr.bytes, rr.eof), (300_000, true));
    let mut got = vec![];
    for (off, b) in &r.data {
        assert_eq!(*off as usize, got.len());
        got.extend_from_slice(b);
    }
    assert_eq!(got, big);

    // write with flow control, then conflict, then atomic replace
    let payload = pattern(900_000);
    let r = d.write_file("/home/u/.big.tmp", &payload, CreateMode::New, None);
    let w: WriteResult = serde_json::from_value(r.out.unwrap()).unwrap();
    assert_eq!(w.size, 900_000);
    let progress: Vec<u64> = r.events.iter().filter_map(|e| if let Event::Progress { done, .. } = e { Some(*done) } else { None }).collect();
    assert!(progress.windows(2).all(|w| w[0] <= w[1]), "progress is monotonic: {progress:?}");
    {
        let h = handle.lock().unwrap();
        assert_eq!(h.as_ref().unwrap().fs.lock().unwrap().data("/home/u/.big.tmp").unwrap(), payload);
    }
    let st = d.call(Request::Stat { path: p("/home/u/.big.tmp"), follow: false });
    let st: StatResult = serde_json::from_value(st.out.unwrap()).unwrap();
    assert_eq!(st.size, Some(900_000));
    let stale = d.write_file("/home/u/a.txt", b"new", CreateMode::Truncate, Some(Expect { size: 1, mtime: 1 }));
    assert_eq!(err_kind(&stale), ErrorKind::Conflict);
    // the rejected write's trailing DATA frames must not produce noise
    d.call(Request::Ping {}).out.unwrap();
    let r = d.call(Request::Rename { from: p("/home/u/.big.tmp"), to: p("/home/u/big.bin"), overwrite: true, allow_nonatomic: false });
    let rn: RenameResult = serde_json::from_value(r.out.unwrap()).unwrap();
    assert!(rn.atomic);
    {
        let h = handle.lock().unwrap();
        assert_eq!(h.as_ref().unwrap().fs.lock().unwrap().data("/home/u/big.bin").unwrap(), payload);
    }
    // disconnect, then everything says NotConnected
    d.call(Request::Disconnect {}).out.unwrap();
    assert!(killed.load(Ordering::Relaxed));
    assert_eq!(err_kind(&d.call(Request::Stat { path: p("/"), follow: false })), ErrorKind::NotConnected);
    d.finish();
}

#[test]
fn closing_stdin_kills_the_transport() {
    let conn = FakeConnector::new(FakeFs::new(), Faults::default());
    let killed = conn.killed.clone();
    let mut d = Driver::start(conn, Limits::default());
    d.hello();
    d.connect().out.unwrap();
    d.finish();
    assert!(killed.load(Ordering::Relaxed), "no orphan ssh process");
}

#[test]
fn write_window_violation_is_a_protocol_error() {
    let faults = Faults { reply_delay: Duration::from_millis(15), ..Faults::default() };
    let mut d = Driver::start(FakeConnector::new(FakeFs::new(), faults), Limits::default());
    d.hello();
    d.connect().out.unwrap();
    let id = d.new_id();
    let total = 8 << 20;
    d.send(id, Request::Write { path: p("/home/u/x"), size: total, create: CreateMode::New, perm: None, expect: None, fsync: false, allow_symlink: false });
    let chunk = vec![1u8; MAX_CHUNK];
    for i in 0..(total as usize / MAX_CHUNK) {
        if write_data(&mut d.tx, id, (i * MAX_CHUNK) as u64, &chunk).is_err() {
            break;
        }
    }
    let r = d.wait(id);
    assert_eq!(err_kind(&r), ErrorKind::Protocol);
    d.finish();
}

#[test]
fn busy_when_too_many_requests_are_in_flight() {
    let mut fs = FakeFs::new();
    for i in 0..5 {
        fs.file(&format!("/home/u/s{i}"), b"x");
    }
    let mut faults = Faults::default();
    for i in 0..5 {
        faults.slow_paths.insert(format!("/home/u/s{i}"), Duration::from_millis(150));
    }
    let mut d = Driver::start(FakeConnector::new(fs, faults), Limits::default());
    d.hello();
    d.connect().out.unwrap();
    let ids: Vec<u64> = (0..5).map(|_| d.new_id()).collect();
    for (i, id) in ids.iter().enumerate() {
        d.send(*id, Request::Stat { path: p(&format!("/home/u/s{i}")), follow: false });
    }
    // the fifth is refused at once, the others complete (they share one SFTP connection: replies are serialised)
    let mut outcome = std::collections::HashMap::new();
    while outcome.len() < 5 {
        match d.recv() {
            Item::Msg(ServerMessage::Result { id, .. }) => {
                outcome.insert(id, None);
            }
            Item::Msg(ServerMessage::Error { id: Some(id), error }) => {
                outcome.insert(id, Some(error.kind));
            }
            _ => {}
        }
    }
    assert_eq!(outcome[&ids[4]], Some(ErrorKind::Busy));
    for id in &ids[..4] {
        assert_eq!(outcome[id], None, "request {id} must succeed");
    }
    d.finish();
}

#[test]
fn cancel_stops_a_running_read() {
    let faults = Faults { reply_delay: Duration::from_millis(10), ..Faults::default() };
    let conn = FakeConnector::new(fs_with(&[("/home/u/big", pattern(2_000_000))]), faults);
    let mut d = Driver::start(conn, Limits::default());
    d.hello();
    d.connect().out.unwrap();
    let id = d.new_id();
    d.send(id, Request::Read { path: p("/home/u/big"), offset: 0, len: None, max_bytes: None, allow_symlink: false });
    loop {
        if let Item::Data { id: i, .. } = d.recv() {
            if i == id {
                break;
            }
        }
    }
    let cid = d.new_id();
    d.send(cid, Request::Cancel { target: id });
    let c = d.wait(cid);
    assert_eq!(c.out.unwrap()["found"], true);
    assert_eq!(err_kind(&d.wait(id)), ErrorKind::Cancelled);
    // cancelling must not poison the session
    d.call(Request::Stat { path: p("/home/u/big"), follow: false }).out.unwrap();
    d.finish();
}

#[test]
fn malformed_input_is_answered_and_survived() {
    let mut d = Driver::start(FakeConnector::new(FakeFs::new(), Faults::default()), Limits::default());
    d.hello();
    write_frame(&mut d.tx, &Frame::Control(b"{ not json".to_vec())).unwrap();
    match d.recv() {
        Item::Msg(ServerMessage::Error { error, .. }) => assert_eq!(error.kind, ErrorKind::InvalidArgument),
        _ => panic!("expected an error"),
    }
    // a path that is not absolute / has a NUL is rejected while parsing the request
    write_frame(&mut d.tx, &Frame::Control(br#"{"id":77,"method":"stat","params":{"path":"../x"}}"#.to_vec())).unwrap();
    match d.recv() {
        Item::Msg(ServerMessage::Error { id, error }) => {
            assert_eq!((id, error.kind), (Some(77), ErrorKind::InvalidArgument));
        }
        _ => panic!("expected an error"),
    }
    // data for a request that does not exist
    write_data(&mut d.tx, 4242, 0, b"x").unwrap();
    match d.recv() {
        Item::Msg(ServerMessage::Error { error, .. }) => assert_eq!(error.kind, ErrorKind::Protocol),
        _ => panic!("expected an error"),
    }
    d.call(Request::Ping {}).out.unwrap();
    d.finish();
}

#[test]
fn oversized_frame_is_fatal_and_ends_the_helper() {
    let mut d = Driver::start(FakeConnector::new(FakeFs::new(), Faults::default()), Limits::default());
    d.hello();
    d.tx.write_all(&[0xff, 0xff, 0xff, 0x7f, 1]).unwrap();
    d.tx.flush().unwrap();
    match d.recv() {
        Item::Msg(ServerMessage::Error { id: None, error }) => assert_eq!(error.kind, ErrorKind::Protocol),
        _ => panic!("expected a fatal protocol error"),
    }
    d.serve.take().unwrap().join().unwrap(); // the loop exited by itself
}

#[test]
fn password_prompt_round_trip_and_the_secret_is_not_echoed() {
    let mut conn = FakeConnector::new(FakeFs::new(), Faults::default());
    conn.ask = Some("Enter passphrase for key '/home/u/.ssh/id_ed25519': ".into());
    let asked = conn.asked.clone();
    let mut d = Driver::start(conn, Limits::default());
    d.hello();
    let cid = d.new_id();
    d.send(cid, Request::Connect(connect_params(None)));
    let (prompt_id, kind) = loop {
        match d.recv() {
            Item::Msg(ServerMessage::Event(Event::Prompt { prompt_id, kind, text })) => {
                assert!(text.contains("passphrase"));
                break (prompt_id, kind);
            }
            _ => {}
        }
    };
    assert_eq!(kind, PromptKind::Passphrase);
    let aid = d.new_id();
    d.send(aid, Request::PromptAnswer { prompt_id, secret: Some(Secret::new("hunter2-xyz")) });
    d.wait(aid).out.unwrap();
    d.wait(cid).out.unwrap();
    assert_eq!(asked.lock().unwrap().clone(), Some(Some("hunter2-xyz".to_string())));
    assert!(!d.all_output().contains("hunter2-xyz"), "the secret never travels back to the editor");
    // answering an unknown prompt is an error
    let r = d.call(Request::PromptAnswer { prompt_id: 999, secret: None });
    assert_eq!(err_kind(&r), ErrorKind::InvalidArgument);
    d.finish();
}

#[test]
fn cancelled_prompt_fails_the_connect() {
    let mut conn = FakeConnector::new(FakeFs::new(), Faults::default());
    conn.ask = Some("u@h's password: ".into());
    let mut d = Driver::start(conn, Limits::default());
    d.hello();
    let cid = d.new_id();
    d.send(cid, Request::Connect(connect_params(None)));
    let prompt_id = loop {
        if let Item::Msg(ServerMessage::Event(Event::Prompt { prompt_id, kind, .. })) = d.recv() {
            assert_eq!(kind, PromptKind::Password);
            break prompt_id;
        }
    };
    let aid = d.new_id();
    d.send(aid, Request::PromptAnswer { prompt_id, secret: None });
    d.wait(aid).out.unwrap();
    assert_eq!(err_kind(&d.wait(cid)), ErrorKind::AuthFailed);
    // the failed connect leaves the helper usable
    assert_eq!(err_kind(&d.call(Request::Stat { path: p("/"), follow: false })), ErrorKind::NotConnected);
    d.finish();
}

#[test]
fn connection_dropped_mid_write_reports_state_lost() {
    let mut fs = FakeFs::new();
    fs.file("/home/u/live", b"ORIGINAL");
    let faults = Faults { drop_after_write_bytes: Some(20_000), ..Faults::default() };
    let conn = FakeConnector::new(fs, faults);
    let handle = conn.handle.clone();
    let killed = conn.killed.clone();
    let mut d = Driver::start(conn, Limits::default());
    d.hello();
    d.connect().out.unwrap();
    let r = d.write_file("/home/u/.live.tmp", &pattern(400_000), CreateMode::New, None);
    assert!(matches!(err_kind(&r), ErrorKind::ConnectionLost | ErrorKind::Timeout), "{:?}", r.out);
    // a State{Lost} event is delivered; subsequent calls fail cleanly
    let mut lost = r.events.iter().any(|e| matches!(e, Event::State { state: ConnState::Lost, .. }));
    let t = std::time::Instant::now();
    while !lost && t.elapsed() < Duration::from_secs(3) {
        match d.rx.recv_timeout(Duration::from_millis(100)) {
            Ok(Item::Msg(ServerMessage::Event(Event::State { state: ConnState::Lost, .. }))) => lost = true,
            Ok(_) | Err(RecvTimeoutError::Timeout) => {}
            Err(_) => break,
        }
    }
    assert!(lost, "state lost event");
    assert!(killed.load(Ordering::Relaxed));
    assert_eq!(err_kind(&d.call(Request::Stat { path: p("/"), follow: false })), ErrorKind::NotConnected);
    let h = handle.lock().unwrap();
    assert_eq!(h.as_ref().unwrap().fs.lock().unwrap().data("/home/u/live").unwrap(), b"ORIGINAL");
    drop(h);
    d.finish();
}

#[test]
fn root_is_enforced_through_the_protocol() {
    let mut d = Driver::start(FakeConnector::new(fs_with(&[("/etc/passwd", b"x".to_vec())]), Faults::default()), Limits::default());
    d.hello();
    let r = d.call(Request::Connect(connect_params(Some("/home/u"))));
    r.out.unwrap();
    let r = d.call(Request::Read { path: p("/etc/passwd"), offset: 0, len: None, max_bytes: None, allow_symlink: true });
    assert_eq!(err_kind(&r), ErrorKind::OutsideRoot);
    assert_eq!(err_kind(&d.call(Request::ListDir { path: p("/home/u/../../etc") })), ErrorKind::OutsideRoot);
    d.finish();
}
