//! The helper's request loop: reads frames from the editor, runs each request on a worker thread
//! (at most `max_in_flight`), writes results / data / events back. One SFTP session per process.

use crate::backend::Backend;
use crate::ops::{Session, WriteParams};
use crate::sftp::{ClientConfig, SftpClient, SftpError};
use crate::transport::{Connector, Killer, Link, Prompter};
use brouillon_remote_proto::*;
use std::collections::HashMap;
use std::io::{Read, Write};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{channel, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

type Out = Arc<Mutex<Box<dyn Write + Send>>>;

struct Conn {
    session: Backend,
    killer: Mutex<Box<dyn Killer>>,
    lost: AtomicBool,
}

struct Inflight {
    cancel: AtomicBool,
    /// Present for `write`: DATA frames are routed here.
    data_tx: Option<Sender<(u64, Vec<u8>)>>,
    received: AtomicU64,
    acked: AtomicU64,
    violation: AtomicBool,
}

#[derive(Default)]
struct State {
    conn: Option<Arc<Conn>>,
    inflight: HashMap<u64, Arc<Inflight>>,
    prompts: HashMap<u64, Sender<Option<Secret>>>,
    next_prompt: u64,
    hello: bool,
    connecting: bool,
    /// Ids of recently finished requests: late DATA frames for them are dropped silently
    /// (a write can be refused before the editor has sent all its data).
    finished: std::collections::VecDeque<u64>,
}

pub struct Helper {
    out: Out,
    connector: Arc<dyn Connector>,
    limits: Limits,
    client_cfg: ClientConfig,
    state: Mutex<State>,
}

fn cfg_for(l: &Limits) -> ClientConfig {
    ClientConfig { idle_timeout: Duration::from_secs(l.op_idle_timeout_s.max(1)), ..ClientConfig::default() }
}

impl Helper {
    pub fn new(out: Box<dyn Write + Send>, connector: Arc<dyn Connector>, limits: Limits) -> Arc<Helper> {
        let client_cfg = cfg_for(&limits);
        Arc::new(Helper { out: Arc::new(Mutex::new(out)), connector, limits, client_cfg, state: Mutex::new(State::default()) })
    }

    /// Override the SFTP client tuning (tests use short timeouts).
    pub fn with_client_config(out: Box<dyn Write + Send>, connector: Arc<dyn Connector>, limits: Limits, cfg: ClientConfig) -> Arc<Helper> {
        Arc::new(Helper { out: Arc::new(Mutex::new(out)), connector, limits, client_cfg: cfg, state: Mutex::new(State::default()) })
    }

    fn send(&self, m: &ServerMessage) {
        let mut o = self.out.lock().unwrap();
        let _ = write_frame(&mut *o,&Frame::Control(m.to_bytes()));
    }
    fn send_data(&self, id: u64, offset: u64, bytes: &[u8]) -> Result<(), SftpError> {
        let mut o = self.out.lock().unwrap();
        for (i, c) in bytes.chunks(MAX_CHUNK).enumerate() {
            write_data(&mut *o,id, offset + (i * MAX_CHUNK) as u64, c).map_err(|e| SftpError::Io(e.to_string()))?;
        }
        Ok(())
    }
    fn event(&self, e: Event) {
        self.send(&ServerMessage::Event(e));
    }
    fn reply<T: serde::Serialize>(&self, id: u64, r: Result<T, RemoteError>) {
        match r {
            Ok(v) => self.send(&ServerMessage::ok(id, &v)),
            Err(e) => self.send(&ServerMessage::err(Some(id), e)),
        }
    }

    /// Serve until the editor closes the pipe. Kills the transport on exit.
    pub fn serve<R: Read>(self: &Arc<Self>, mut input: R) {
        loop {
            match read_frame(&mut input) {
                Ok(None) => break,
                Ok(Some(Frame::Control(b))) => self.on_control(&b),
                Ok(Some(Frame::Data { id, offset, bytes })) => self.on_data(id, offset, bytes),
                Err(e) => {
                    self.send(&ServerMessage::err(None, RemoteError::new(ErrorKind::Protocol, format!("bad frame: {e}"))));
                    break;
                }
            }
        }
        self.shutdown();
    }

    fn shutdown(&self) {
        let (conn, inflight) = {
            let mut st = self.state.lock().unwrap();
            (st.conn.take(), st.inflight.values().cloned().collect::<Vec<_>>())
        };
        for i in inflight {
            i.cancel.store(true, Ordering::Relaxed);
        }
        if let Some(c) = conn {
            c.killer.lock().unwrap().kill();
        }
    }

    fn on_data(&self, id: u64, offset: u64, bytes: Vec<u8>) {
        let inf = self.state.lock().unwrap().inflight.get(&id).cloned();
        match inf {
            Some(i) if i.data_tx.is_some() => {
                let rec = i.received.fetch_add(bytes.len() as u64, Ordering::Relaxed) + bytes.len() as u64;
                if rec.saturating_sub(i.acked.load(Ordering::Relaxed)) > self.limits.write_window + MAX_CHUNK as u64 {
                    i.violation.store(true, Ordering::Relaxed);
                }
                let _ = i.data_tx.as_ref().unwrap().send((offset, bytes));
            }
            None if self.state.lock().unwrap().finished.contains(&id) => {}
            _ => self.send(&ServerMessage::err(None, RemoteError::new(ErrorKind::Protocol, format!("data frame for request {id} which is not a write")))),
        }
    }

    fn on_control(self: &Arc<Self>, b: &[u8]) {
        let frame: RequestFrame = match serde_json::from_slice(b) {
            Ok(f) => f,
            Err(e) => {
                let id = serde_json::from_slice::<serde_json::Value>(b).ok().and_then(|v| v.get("id").and_then(|i| i.as_u64()));
                self.send(&ServerMessage::err(id, RemoteError::new(ErrorKind::InvalidArgument, format!("bad request: {e}"))));
                return;
            }
        };
        let id = frame.id;
        // handled inline: cheap and must work while workers are busy
        match &frame.request {
            Request::Hello { proto, .. } => {
                if *proto != PROTO_VERSION {
                    return self.send(&ServerMessage::err(Some(id), RemoteError::new(ErrorKind::Unsupported, format!("protocol {proto} not supported, this helper speaks {PROTO_VERSION}"))));
                }
                self.state.lock().unwrap().hello = true;
                let r = HelloResult {
                    proto: PROTO_VERSION,
                    helper_version: env!("CARGO_PKG_VERSION").to_string(),
                    caps: ["sftp", "posix_rename", "list_paging", "write_expect", "symlink_gate", "ftps", "ftp_plain_optin"].iter().map(|s| s.to_string()).collect(),
                    limits: self.limits.clone(),
                };
                return self.send(&ServerMessage::ok(id, &r));
            }
            _ if !self.state.lock().unwrap().hello => {
                return self.send(&ServerMessage::err(Some(id), RemoteError::new(ErrorKind::InvalidArgument, "hello must be the first request")));
            }
            Request::Ping {} => return self.send(&ServerMessage::ok(id, &serde_json::json!({}))),
            Request::Cancel { target } => {
                let found = match self.state.lock().unwrap().inflight.get(target) {
                    Some(i) => {
                        i.cancel.store(true, Ordering::Relaxed);
                        true
                    }
                    None => false,
                };
                return self.send(&ServerMessage::ok(id, &serde_json::json!({ "found": found })));
            }
            Request::PromptAnswer { prompt_id, secret } => {
                let tx = self.state.lock().unwrap().prompts.remove(prompt_id);
                return match tx {
                    Some(tx) => {
                        let _ = tx.send(secret.clone());
                        self.send(&ServerMessage::ok(id, &serde_json::json!({})))
                    }
                    None => self.send(&ServerMessage::err(Some(id), RemoteError::new(ErrorKind::InvalidArgument, "no such prompt"))),
                };
            }
            Request::Disconnect {} => {
                let conn = self.state.lock().unwrap().conn.take();
                if let Some(c) = conn {
                    c.lost.store(true, Ordering::Relaxed);
                    c.killer.lock().unwrap().kill();
                }
                return self.send(&ServerMessage::ok(id, &serde_json::json!({})));
            }
            _ => {}
        }

        // worker requests: concurrency cap, registration before any DATA frame can arrive
        let inf = {
            let mut st = self.state.lock().unwrap();
            if st.inflight.contains_key(&id) {
                drop(st);
                return self.send(&ServerMessage::err(Some(id), RemoteError::new(ErrorKind::InvalidArgument, "request id already in flight")));
            }
            if st.inflight.len() >= self.limits.max_in_flight {
                drop(st);
                return self.send(&ServerMessage::err(Some(id), RemoteError::new(ErrorKind::Busy, "too many requests in flight")));
            }
            let (tx, rx) = if matches!(frame.request, Request::Write { .. }) {
                let (tx, rx) = channel();
                (Some(tx), Some(rx))
            } else {
                (None, None)
            };
            let inf = Arc::new(Inflight { cancel: AtomicBool::new(false), data_tx: tx, received: AtomicU64::new(0), acked: AtomicU64::new(0), violation: AtomicBool::new(false) });
            st.inflight.insert(id, inf.clone());
            (inf, rx)
        };
        let (inf, rx) = inf;
        let me = self.clone();
        std::thread::Builder::new()
            .name(format!("req-{id}"))
            .spawn(move || {
                me.run(id, frame.request, &inf, rx);
                let mut st = me.state.lock().unwrap();
                st.inflight.remove(&id);
                st.finished.push_back(id);
                if st.finished.len() > 256 {
                    st.finished.pop_front();
                }
            })
            .expect("spawn worker");
    }

    fn conn(&self) -> Result<Arc<Conn>, RemoteError> {
        self.state.lock().unwrap().conn.clone().ok_or_else(|| RemoteError::new(ErrorKind::NotConnected, "not connected"))
    }

    /// A fatal transport error or a timeout poisons the connection: kill the transport and tell the editor.
    fn after_error(&self, conn: &Arc<Conn>, e: &RemoteError) {
        if matches!(e.kind, ErrorKind::ConnectionLost | ErrorKind::Protocol | ErrorKind::Timeout) && !conn.lost.swap(true, Ordering::Relaxed) {
            {
                let mut st = self.state.lock().unwrap();
                if st.conn.as_ref().is_some_and(|c| Arc::ptr_eq(c, conn)) {
                    st.conn = None;
                }
            }
            conn.killer.lock().unwrap().kill();
            self.event(Event::State { state: ConnState::Lost, reason: Some(e.message.clone()) });
        }
    }

    fn run(self: &Arc<Self>, id: u64, req: Request, inf: &Arc<Inflight>, rx: Option<Receiver<(u64, Vec<u8>)>>) {
        if let Request::Connect(p) = req {
            let r = self.connect(id, p, inf);
            return self.reply(id, r);
        }
        let conn = match self.conn() {
            Ok(c) => c,
            Err(e) => return self.reply::<()>(id, Err(e)),
        };
        let s = &conn.session;
        let cancel = &inf.cancel;
        macro_rules! done {
            ($r:expr) => {{
                let r = $r;
                if let Err(e) = &r {
                    self.after_error(&conn, e);
                }
                self.reply(id, r)
            }};
        }
        match req {
            Request::ListDir { path } => {
                let r = s.list_dir(&path, cancel, |entries| self.event(Event::ListPage { id, entries }));
                done!(r)
            }
            Request::Stat { path, follow } => done!(s.stat(&path, follow)),
            Request::Read { path, offset, len, max_bytes, allow_symlink } => {
                done!(s.read(&path, offset, len, max_bytes, allow_symlink, cancel, |off, data| self.send_data(id, off, data)))
            }
            Request::Write { path, size, create, perm, expect, fsync, allow_symlink } => {
                let rx = rx.expect("write has a data channel");
                let p = WriteParams { path, size, create, perm, expect, fsync, allow_symlink };
                let mut expect_off = 0u64;
                let mut last_progress = u64::MAX;
                let idle = Duration::from_secs(self.limits.op_idle_timeout_s.max(1));
                let r = s.write(
                    &p,
                    cancel,
                    || {
                        let started = Instant::now();
                        loop {
                            if cancel.load(Ordering::Relaxed) {
                                return Err(RemoteError::new(ErrorKind::Cancelled, "cancelled"));
                            }
                            if inf.violation.load(Ordering::Relaxed) {
                                return Err(RemoteError::new(ErrorKind::Protocol, "write window exceeded: wait for progress events before sending more data"));
                            }
                            match rx.recv_timeout(Duration::from_millis(50)) {
                                Ok((off, bytes)) => {
                                    if off != expect_off {
                                        return Err(RemoteError::new(ErrorKind::Protocol, format!("data offset {off}, expected {expect_off}")));
                                    }
                                    expect_off += bytes.len() as u64;
                                    return Ok(Some(bytes));
                                }
                                Err(RecvTimeoutError::Timeout) => {
                                    if started.elapsed() > idle {
                                        return Err(RemoteError::new(ErrorKind::Timeout, "the editor stopped sending data"));
                                    }
                                }
                                Err(RecvTimeoutError::Disconnected) => return Err(RemoteError::new(ErrorKind::Internal, "data channel closed")),
                            }
                        }
                    },
                    |done| {
                        inf.acked.store(done, Ordering::Relaxed);
                        if done != last_progress {
                            last_progress = done;
                            self.event(Event::Progress { id, done, total: Some(size) });
                        }
                    },
                );
                // a failure caused by the editor (cancel / protocol) must not poison the SFTP session
                done!(r)
            }
            Request::Rename { from, to, overwrite, allow_nonatomic } => done!(s.rename(&from, &to, overwrite, allow_nonatomic)),
            Request::Remove { path } => done!(s.remove(&path).map(|_| serde_json::json!({}))),
            Request::Rmdir { path } => done!(s.rmdir(&path).map(|_| serde_json::json!({}))),
            Request::Mkdir { path, mode } => done!(s.mkdir(&path, mode).map(|_| serde_json::json!({}))),
            Request::Chmod { path, mode } => done!(s.chmod(&path, mode).map(|_| serde_json::json!({}))),
            Request::Realpath { path } => done!(s.realpath(&path).map(|path| RealpathResult { path })),
            Request::Readlink { path } => done!(s.readlink(&path).map(|target| ReadlinkResult { target })),
            // handled inline, never reaches a worker
            Request::Hello { .. } | Request::Ping {} | Request::Connect(_) | Request::Disconnect {} | Request::Cancel { .. } | Request::PromptAnswer { .. } => {
                self.reply::<()>(id, Err(RemoteError::new(ErrorKind::Internal, "misrouted request")))
            }
        }
    }

    fn connect(self: &Arc<Self>, id: u64, p: ConnectParams, inf: &Arc<Inflight>) -> Result<ConnectResult, RemoteError> {
        {
            let mut st = self.state.lock().unwrap();
            if st.conn.is_some() || st.connecting {
                return Err(RemoteError::new(ErrorKind::InvalidArgument, "already connected or connecting"));
            }
            st.connecting = true;
        }
        let r = self.connect_inner(id, p, inf);
        self.state.lock().unwrap().connecting = false;
        let (res, conn) = r?;
        self.state.lock().unwrap().conn = Some(conn);
        self.event(Event::State { state: ConnState::Connected, reason: None });
        Ok(res)
    }

    fn connect_inner(self: &Arc<Self>, _id: u64, p: ConnectParams, inf: &Arc<Inflight>) -> Result<(ConnectResult, Arc<Conn>), RemoteError> {
        let prompter: Arc<dyn Prompter> = Arc::new(EditorPrompter { helper: self.clone(), cancel: inf.clone() });
        if p.protocol != Protocol::Sftp {
            let (session, res) = crate::ftp::connect(&p, &self.connector.ftp_env(), &prompter, &self.limits)?;
            let guard = session.killer();
            let conn = Arc::new(Conn { session: Backend::Ftp(session), killer: Mutex::new(guard), lost: AtomicBool::new(false) });
            return Ok((res, conn));
        }
        let Link { reader, writer, mut guard, diag } = self.connector.connect(&p, prompter)?;
        let client = match SftpClient::connect(reader, writer, self.client_cfg.clone()) {
            Ok(c) => c,
            Err(e) => {
                guard.kill();
                let text = String::from_utf8_lossy(&diag.lock().unwrap()).into_owned();
                return Err(if text.trim().is_empty() { e.into() } else { crate::transport::classify_stderr(&text) });
            }
        };
        let exts: Vec<String> = client.extensions.iter().map(|(n, _)| n.clone()).collect();
        let (posix_rename, fsync, version) = (client.has_extension(crate::sftp::packet::EXT_POSIX_RENAME), client.has_extension(crate::sftp::packet::EXT_FSYNC), client.version);
        let (session, home) = match Session::open(client, p.root.clone(), self.limits.clone()) {
            Ok(x) => x,
            Err(e) => {
                guard.kill();
                return Err(e);
            }
        };
        let conn = Arc::new(Conn { session: Backend::Sftp(session), killer: Mutex::new(guard), lost: AtomicBool::new(false) });
        Ok((ConnectResult { home, sftp_version: version, extensions: exts, posix_rename, fsync, protocol: Protocol::Sftp, insecure: false, ftp: None }, conn))
    }
}

/// Forwards ssh prompts to the editor and waits for `prompt_answer`.
struct EditorPrompter {
    helper: Arc<Helper>,
    cancel: Arc<Inflight>,
}

impl Prompter for EditorPrompter {
    fn ask(&self, kind: PromptKind, text: &str) -> Option<Secret> {
        let (tx, rx) = channel();
        let prompt_id = {
            let mut st = self.helper.state.lock().unwrap();
            st.next_prompt += 1;
            let id = st.next_prompt;
            st.prompts.insert(id, tx);
            id
        };
        self.helper.event(Event::Prompt { prompt_id, kind, text: sanitize_text(text) });
        let started = Instant::now();
        loop {
            if self.cancel.cancel.load(Ordering::Relaxed) || started.elapsed() > Duration::from_secs(300) {
                break;
            }
            match rx.recv_timeout(Duration::from_millis(100)) {
                Ok(a) => return a,
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => break,
            }
        }
        self.helper.state.lock().unwrap().prompts.remove(&prompt_id);
        None
    }
}
