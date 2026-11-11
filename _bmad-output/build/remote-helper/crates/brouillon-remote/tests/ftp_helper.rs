//! FTP/FTPS through the real stdio protocol: an editor-side driver speaks frames to an in-process
//! `Helper` whose FTP network is the in-memory fake server. Same protocol as SFTP, no new messages.

mod ftp_fake;

use brouillon_remote::ftp::FtpEnv;
use brouillon_remote::helper::Helper;
use brouillon_remote::transport::{Connector, Link, Prompter};
use brouillon_remote_proto::*;
use ftp_fake::*;
use serde_json::Value;
use std::io::PipeWriter;
use std::sync::mpsc::{channel, Receiver};
use std::sync::{Arc, Mutex};
use std::time::Duration;

fn p(s: &str) -> RemotePath {
    RemotePath::parse(s).unwrap()
}

struct FtpOnly(FtpEnv);
impl Connector for FtpOnly {
    fn connect(&self, _p: &ConnectParams, _pr: Arc<dyn Prompter>) -> Result<Link, RemoteError> {
        Err(RemoteError::new(ErrorKind::Internal, "this test connector has no ssh"))
    }
    fn ftp_env(&self) -> FtpEnv {
        self.0.clone()
    }
}

enum Item {
    Msg(ServerMessage),
    Data { id: u64, bytes: Vec<u8> },
}

struct Driver {
    tx: PipeWriter,
    rx: Receiver<Item>,
    serve: Option<std::thread::JoinHandle<()>>,
    next: u64,
    raw: Arc<Mutex<Vec<u8>>>,
}

impl Driver {
    fn start(fake: &Fake) -> Driver {
        let (in_r, in_w) = std::io::pipe().unwrap();
        let (out_r, out_w) = std::io::pipe().unwrap();
        let limits = Limits { op_idle_timeout_s: 2, ..Limits::default() };
        let helper = Helper::new(Box::new(out_w), Arc::new(FtpOnly(fake.env.clone())), limits);
        let serve = std::thread::spawn(move || helper.serve(in_r));
        let (tx, rx) = channel();
        let raw = Arc::new(Mutex::new(Vec::new()));
        let log = raw.clone();
        std::thread::spawn(move || {
            let mut r = out_r;
            while let Ok(Some(f)) = read_frame(&mut r) {
                let item = match f {
                    Frame::Control(b) => {
                        log.lock().unwrap().extend_from_slice(&b);
                        Item::Msg(ServerMessage::from_slice(&b).expect("valid message"))
                    }
                    Frame::Data { id, bytes, .. } => Item::Data { id, bytes },
                };
                if tx.send(item).is_err() {
                    break;
                }
            }
        });
        let mut d = Driver { tx: in_w, rx, serve: Some(serve), next: 1, raw };
        let id = d.id();
        d.send(id, Request::Hello { proto: PROTO_VERSION, client: "t".into() });
        let (r, _, _) = d.wait(id, None);
        let hello: HelloResult = serde_json::from_value(r.unwrap()).unwrap();
        assert!(hello.caps.iter().any(|c| c == "ftps"));
        d
    }
    fn id(&mut self) -> u64 {
        self.next += 1;
        self.next
    }
    fn send(&mut self, id: u64, request: Request) {
        write_control(&mut self.tx, &RequestFrame { id, request }).unwrap();
    }
    /// Wait for the answer to `id`; `answer` responds to prompt events.
    fn wait(&mut self, id: u64, answer: Option<&str>) -> (Result<Value, RemoteError>, Vec<Event>, Vec<u8>) {
        let (mut events, mut data) = (vec![], vec![]);
        loop {
            match self.rx.recv_timeout(Duration::from_secs(10)).expect("helper went silent") {
                Item::Data { id: d, bytes } if d == id => data.extend(bytes),
                Item::Data { .. } => {}
                Item::Msg(ServerMessage::Event(e)) => {
                    if let Event::Prompt { prompt_id, .. } = &e {
                        let pid = *prompt_id;
                        let a = self.id();
                        self.send(a, Request::PromptAnswer { prompt_id: pid, secret: answer.map(Secret::new) });
                    }
                    events.push(e)
                }
                Item::Msg(ServerMessage::Result { id: r, result }) if r == id => return (Ok(result), events, data),
                Item::Msg(ServerMessage::Error { id: Some(r), error }) if r == id => return (Err(error), events, data),
                Item::Msg(_) => {}
            }
        }
    }
    fn call(&mut self, request: Request) -> (Result<Value, RemoteError>, Vec<Event>, Vec<u8>) {
        let id = self.id();
        self.send(id, request);
        self.wait(id, None)
    }
    fn output(&self) -> String {
        String::from_utf8_lossy(&self.raw.lock().unwrap()).into_owned()
    }
    fn finish(mut self) {
        drop(self.tx);
        self.serve.take().unwrap().join().unwrap();
    }
}

fn params(proto: Protocol, f: impl FnOnce(&mut FtpOptions)) -> ConnectParams {
    let mut o = FtpOptions { password: Some(Secret::new(PASSWORD)), fingerprint_policy: FingerprintPolicy::Strict, ..FtpOptions::default() };
    f(&mut o);
    ConnectParams {
        host: "localhost".into(),
        port: None,
        user: Some("u".into()),
        identity_file: None,
        proxy_jump: None,
        host_key_policy: HostKeyPolicy::Strict,
        known_hosts_file: None,
        ssh_config_file: None,
        interactive: true,
        connect_timeout_s: Some(2),
        ssh_path: None,
        root: None,
        protocol: proto,
        ftp: Some(o),
    }
}

#[test]
fn ftps_session_over_the_stdio_protocol() {
    let fake = Fake::new(Opts::default());
    fake.world.put("/home/u/a.txt", b"hello");
    fake.world.dir("/home/u/sub");
    let mut d = Driver::start(&fake);
    let (r, events, _) = d.call(Request::Connect(params(Protocol::Ftps, |_| {})));
    let res: ConnectResult = serde_json::from_value(r.unwrap()).unwrap();
    assert_eq!((res.home, res.protocol, res.insecure), (p("/home/u"), Protocol::Ftps, false));
    assert!(events.iter().any(|e| matches!(e, Event::State { state: ConnState::Connected, .. })));

    let (r, events, _) = d.call(Request::ListDir { path: p("/home/u") });
    assert_eq!(serde_json::from_value::<ListResult>(r.unwrap()).unwrap().count, 2);
    assert!(events.iter().any(|e| matches!(e, Event::ListPage { entries, .. } if entries.len() == 2)));

    let (r, _, data) = d.call(Request::Read { path: p("/home/u/a.txt"), offset: 0, len: None, max_bytes: None, allow_symlink: false });
    assert!(r.is_ok());
    assert_eq!(data, b"hello");

    let id = d.id();
    d.send(id, Request::Write { path: p("/home/u/a.txt"), size: 3, create: CreateMode::Truncate, perm: None, expect: Some(Expect { size: 5, mtime: 1_700_000_000 }), fsync: false, allow_symlink: false });
    write_data(&mut d.tx, id, 0, b"new").unwrap();
    let (r, events, _) = d.wait(id, None);
    let w: WriteResult = serde_json::from_value(r.unwrap()).unwrap();
    assert!(!w.atomic && !w.swapped);
    assert!(events.iter().any(|e| matches!(e, Event::Progress { done: 3, .. })));
    assert_eq!(fake.world.file("/home/u/a.txt").unwrap(), b"new");

    // a conflicting write is refused
    let id = d.id();
    d.send(id, Request::Write { path: p("/home/u/a.txt"), size: 1, create: CreateMode::Truncate, perm: None, expect: Some(Expect { size: 5, mtime: 1_700_000_000 }), fsync: false, allow_symlink: false });
    write_data(&mut d.tx, id, 0, b"x").unwrap();
    assert_eq!(d.wait(id, None).0.unwrap_err().kind, ErrorKind::Conflict);

    // posix rename does not exist on FTP: overwrite is reported non-atomic
    fake.world.put("/home/u/b.txt", b"b");
    let (r, _, _) = d.call(Request::Rename { from: p("/home/u/b.txt"), to: p("/home/u/a.txt"), overwrite: true, allow_nonatomic: true });
    assert!(!serde_json::from_value::<RenameResult>(r.unwrap()).unwrap().atomic);

    let (r, _, _) = d.call(Request::Readlink { path: p("/home/u/a.txt") });
    assert_eq!(r.unwrap_err().kind, ErrorKind::Unsupported);

    let out = d.output();
    assert!(!out.contains(PASSWORD), "the password never appears on the protocol");
    assert!(!out.contains("PASS "), "no command text either");
    let (r, _, _) = d.call(Request::Disconnect {});
    assert!(r.is_ok());
    d.finish();
}

#[test]
fn trust_on_first_use_goes_through_the_prompt_event() {
    let fake = Fake::new(Opts::default());
    fake.world.set_cert(Cert::Other);
    let mut d = Driver::start(&fake);
    let id = d.id();
    d.send(id, Request::Connect(params(Protocol::Ftps, |o| o.fingerprint_policy = FingerprintPolicy::Ask)));
    let (r, events, _) = d.wait(id, Some("yes"));
    let res: ConnectResult = serde_json::from_value(r.unwrap()).unwrap();
    let prompts: Vec<&Event> = events.iter().filter(|e| matches!(e, Event::Prompt { .. })).collect();
    assert_eq!(prompts.len(), 1);
    let Event::Prompt { kind, text, .. } = prompts[0] else { unreachable!() };
    assert_eq!(*kind, PromptKind::HostKey);
    assert!(text.contains("SHA-256"));
    let caps = res.ftp.unwrap();
    assert_eq!(caps.cert_trust.as_deref(), Some("user_confirmed"));
    // the editor stores caps.cert_fingerprint; the next connect uses it as the pin and needs no prompt
    let fp = caps.cert_fingerprint.unwrap();
    let (r, _, _) = d.call(Request::Disconnect {});
    assert!(r.is_ok());
    let id = d.id();
    d.send(id, Request::Connect(params(Protocol::Ftps, |o| {
        o.fingerprint_policy = FingerprintPolicy::Ask;
        o.pinned_fingerprint = Some(fp.clone());
    })));
    let (r, events, _) = d.wait(id, None);
    assert_eq!(serde_json::from_value::<ConnectResult>(r.unwrap()).unwrap().ftp.unwrap().cert_trust.as_deref(), Some("pinned"));
    assert!(!events.iter().any(|e| matches!(e, Event::Prompt { .. })));
    let (r, _, _) = d.call(Request::Disconnect {});
    assert!(r.is_ok());

    // the server's certificate changes: hard stop, no prompt, no credential
    fake.world.set_cert(Cert::Leaf);
    let before = fake.world.log_count("PASS");
    let id = d.id();
    d.send(id, Request::Connect(params(Protocol::Ftps, |o| {
        o.fingerprint_policy = FingerprintPolicy::Ask;
        o.pinned_fingerprint = Some(fp.clone());
    })));
    let (r, events, _) = d.wait(id, Some("yes"));
    assert_eq!(r.unwrap_err().kind, ErrorKind::HostKeyChanged);
    assert!(!events.iter().any(|e| matches!(e, Event::Prompt { .. })), "a changed fingerprint cannot be overridden by a prompt");
    assert_eq!(fake.world.log_count("PASS"), before);
    assert!(!d.output().contains(PASSWORD));
    d.finish();
}

#[test]
fn plain_ftp_needs_the_explicit_opt_in_and_is_flagged() {
    let fake = Fake::new(Opts { tls: false, ..Opts::default() });
    let mut d = Driver::start(&fake);
    let (r, _, _) = d.call(Request::Connect(params(Protocol::Ftp, |_| {})));
    let e = r.unwrap_err();
    assert_eq!(e.kind, ErrorKind::InvalidArgument);
    assert!(e.message.contains("plain FTP"));
    let (r, _, _) = d.call(Request::Connect(params(Protocol::Ftp, |o| o.allow_plain = true)));
    let res: ConnectResult = serde_json::from_value(r.unwrap()).unwrap();
    assert!(res.insecure);
    assert!(res.ftp.unwrap().warnings.iter().any(|w| w.contains("unencrypted")));
    d.finish();
}
