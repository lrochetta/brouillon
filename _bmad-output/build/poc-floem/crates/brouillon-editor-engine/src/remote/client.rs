//! `HelperClient`: the editor side of `docs/REMOTE_HELPER_PROTOCOL.md`.
//!
//! One client = one helper process = one SFTP session. It is transport-agnostic (any `Read` +
//! `Write` pair), so the tests run the REAL helper code in-process over pipes while production
//! spawns `brouillon-remote.exe` (see [`super::helper`]).
//!
//! Rules this file keeps:
//! * Every call is BLOCKING and has a timeout (the hub has none). It is meant to be called from a
//!   worker thread; nothing here is ever called from the UI thread.
//! * A dead helper (process killed, pipe closed, protocol error, `state lost`) fails every pending
//!   call at once with a `Dead` error and every later call immediately. The owner maps that to
//!   "Offline" and reconnects with a NEW client; a dead client is never revived.
//! * Prompt events (password, passphrase, host key) are handed to a [`PromptHandler`] on their own
//!   thread so the reader keeps running. The answer is written once, as a `prompt_answer` frame,
//!   and is never logged, stored or put in an error message.

use std::collections::HashMap;
use std::io::{Read, Write};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::mpsc::{channel, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use brouillon_remote_proto::{
    read_frame, write_control, write_data, ConnState as ProtoConn, ConnectParams, ConnectResult, CreateMode, Entry, ErrorKind, Event, Frame, HelloResult, Limits, ListResult, PromptKind, ReadResult, RemotePath as WirePath,
    RemoteError as WireError, Request, RequestFrame, Secret, ServerMessage, StatResult, MAX_CHUNK, PROTO_VERSION, WRITE_WINDOW,
};
use serde::de::DeserializeOwned;
use serde_json::Value;

/// Blocks until the user answered a prompt; `None` = cancelled (or the helper went away: `gone`).
pub trait PromptHandler: Send + Sync {
    fn ask(&self, kind: PromptKind, text: &str, gone: &AtomicBool) -> Option<Secret>;
}

/// Never answers: for sessions that must not prompt (the helper is then started non-interactive).
pub struct NoPrompts;
impl PromptHandler for NoPrompts {
    fn ask(&self, _: PromptKind, _: &str, _: &AtomicBool) -> Option<Secret> {
        None
    }
}

#[derive(Clone, Debug)]
pub struct Timeouts {
    /// stat, mkdir, rename, remove, chmod, realpath, ping: no byte for this long = failure.
    pub quick: Duration,
    /// Silence tolerated while a listing / read / write streams.
    pub io_idle: Duration,
    /// Whole-call cap for a read / write / listing.
    pub io_total: Duration,
    /// `hello` and `connect` (suspended while a prompt waits for the user).
    pub connect: Duration,
}

impl Default for Timeouts {
    fn default() -> Self {
        Timeouts { quick: Duration::from_secs(30), io_idle: Duration::from_secs(30), io_total: Duration::from_secs(15 * 60), connect: Duration::from_secs(60) }
    }
}

/// Why a call did not produce a result.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CallError {
    /// The helper answered with an error.
    Remote(WireError),
    /// The helper is gone (killed, closed, protocol failure, session lost). Reconnect = new client.
    Dead(String),
    /// No answer in time; a `cancel` was sent.
    Timeout,
    /// The request could not even be built (bad path, frame too large).
    Invalid(String),
}

impl CallError {
    pub fn is_transport(&self) -> bool {
        match self {
            CallError::Dead(_) | CallError::Timeout => true,
            CallError::Remote(e) => matches!(e.kind, ErrorKind::ConnectionLost | ErrorKind::NotConnected | ErrorKind::Timeout | ErrorKind::Protocol),
            CallError::Invalid(_) => false,
        }
    }
}

impl std::fmt::Display for CallError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CallError::Remote(e) => write!(f, "{e}"),
            CallError::Dead(r) => write!(f, "helper gone: {r}"),
            CallError::Timeout => f.write_str("no answer from the helper in time"),
            CallError::Invalid(m) => write!(f, "invalid request: {m}"),
        }
    }
}

enum Msg {
    Data { offset: u64, bytes: Vec<u8> },
    Progress(u64),
    Page(Vec<Entry>),
    Done(Result<Value, WireError>),
    Dead(String),
}

/// Everything a finished call produced besides the result value.
#[derive(Default)]
pub struct Collected {
    pub result: Value,
    pub data: Vec<u8>,
    pub entries: Vec<Entry>,
}

struct Inner {
    writer: Mutex<Option<Box<dyn Write + Send>>>,
    pending: Mutex<HashMap<u64, Sender<Msg>>>,
    next_id: AtomicU64,
    dead: Mutex<Option<String>>,
    gone: Arc<AtomicBool>,
    killer: Mutex<Option<Box<dyn FnOnce() + Send>>>,
    prompts_open: AtomicUsize,
    prompts: Arc<dyn PromptHandler>,
    timeouts: Timeouts,
    limits: Mutex<Limits>,
    /// Counting gate: the helper refuses the 5th request in flight, so we queue instead.
    gate: (Mutex<usize>, Condvar),
    /// Diagnostics only. Never receives a secret, a prompt answer or file content.
    log: Arc<dyn Fn(&str) + Send + Sync>,
}

const MAX_CALLS: usize = 3;

pub struct HelperClient {
    inner: Arc<Inner>,
}

impl HelperClient {
    /// Start the reader thread on `reader`; requests go to `writer`. `killer` ends the process (or
    /// whatever sits behind the pipes) and must be safe to call from any thread.
    pub fn new(
        reader: Box<dyn Read + Send>,
        writer: Box<dyn Write + Send>,
        killer: Box<dyn FnOnce() + Send>,
        prompts: Arc<dyn PromptHandler>,
        timeouts: Timeouts,
        log: Arc<dyn Fn(&str) + Send + Sync>,
    ) -> HelperClient {
        let inner = Arc::new(Inner {
            writer: Mutex::new(Some(writer)),
            pending: Mutex::new(HashMap::new()),
            next_id: AtomicU64::new(1),
            dead: Mutex::new(None),
            gone: Arc::new(AtomicBool::new(false)),
            killer: Mutex::new(Some(killer)),
            prompts_open: AtomicUsize::new(0),
            prompts,
            timeouts,
            limits: Mutex::new(Limits::default()),
            gate: (Mutex::new(0), Condvar::new()),
            log,
        });
        let r = inner.clone();
        let spawned = std::thread::Builder::new().name("brouillon-helper-reader".into()).spawn(move || reader_loop(r, reader));
        if spawned.is_err() {
            inner.die("could not start the reader thread");
        }
        HelperClient { inner }
    }

    pub fn is_dead(&self) -> bool {
        self.inner.gone.load(Ordering::SeqCst)
    }

    pub fn dead_reason(&self) -> Option<String> {
        self.inner.dead.lock().ok().and_then(|d| d.clone())
    }

    /// Terminate the helper now (idempotent). Pending calls fail with `Dead`.
    pub fn kill(&self, reason: &str) {
        self.inner.die(reason);
    }

    pub fn limits(&self) -> Limits {
        self.inner.limits.lock().map(|l| l.clone()).unwrap_or_default()
    }

    // ------------------------------------------------------------------ session

    pub fn hello(&self) -> Result<HelloResult, CallError> {
        let c = self.call(Request::Hello { proto: PROTO_VERSION, client: "brouillon".into() }, self.inner.timeouts.connect, self.inner.timeouts.connect, false)?;
        let h: HelloResult = decode(c.result)?;
        if let Ok(mut l) = self.inner.limits.lock() {
            *l = h.limits.clone();
        }
        Ok(h)
    }

    /// May wait on the user (prompts): the timer is suspended while a prompt is open.
    pub fn connect(&self, p: ConnectParams) -> Result<ConnectResult, CallError> {
        let c = self.call(Request::Connect(p), self.inner.timeouts.connect, Duration::from_secs(3600), true)?;
        decode(c.result)
    }

    pub fn ping(&self) -> Result<(), CallError> {
        self.quick(Request::Ping {}).map(|_| ())
    }

    // ------------------------------------------------------------------ operations

    /// Every call that is one request, one answer.
    pub fn quick(&self, req: Request) -> Result<Value, CallError> {
        let t = self.inner.timeouts.quick;
        Ok(self.call(req, t, t, false)?.result)
    }

    pub fn list_dir(&self, path: WirePath) -> Result<(Vec<Entry>, ListResult), CallError> {
        let t = &self.inner.timeouts;
        let c = self.call(Request::ListDir { path }, t.io_idle, t.io_total, false)?;
        let r: ListResult = decode(c.result)?;
        Ok((c.entries, r))
    }

    pub fn stat(&self, path: WirePath, follow: bool) -> Result<StatResult, CallError> {
        decode(self.quick(Request::Stat { path, follow })?)
    }

    /// Whole file, capped at `max_bytes` on what really arrives.
    pub fn read(&self, path: WirePath, max_bytes: Option<u64>) -> Result<Vec<u8>, CallError> {
        let t = &self.inner.timeouts;
        let c = self.call(Request::Read { path, offset: 0, len: None, max_bytes, allow_symlink: false }, t.io_idle, t.io_total, false)?;
        let r: ReadResult = decode(c.result)?;
        if r.bytes != c.data.len() as u64 || !r.eof {
            return Err(CallError::Remote(WireError::new(ErrorKind::TooLarge, "the file was not read completely")));
        }
        Ok(c.data)
    }

    /// Send `data` for a `write` request, keeping at most `write_window` bytes unacknowledged.
    pub fn write(&self, path: WirePath, data: &[u8], create: CreateMode, perm: Option<u32>, fsync: bool) -> Result<(), CallError> {
        let _permit = self.acquire(self.inner.timeouts.io_idle)?;
        let (id, rx) = self.register()?;
        let req = Request::Write { path, size: data.len() as u64, create, perm, expect: None, fsync, allow_symlink: false };
        if let Err(e) = self.send(id, req) {
            self.unregister(id);
            return Err(e);
        }
        let window = self.limits().write_window.min(WRITE_WINDOW).max(MAX_CHUNK as u64);
        let (mut sent, mut acked) = (0u64, 0u64);
        let mut off = 0usize;
        let idle = self.inner.timeouts.io_idle;
        let started = Instant::now();
        let total = self.inner.timeouts.io_total;
        let mut finish: Option<Result<Value, WireError>> = None;
        while finish.is_none() {
            // Take whatever the helper already said (acks, an early refusal).
            while let Ok(m) = rx.try_recv() {
                match m {
                    Msg::Progress(done) => acked = acked.max(done),
                    Msg::Done(r) => finish = Some(r),
                    Msg::Dead(why) => {
                        self.unregister(id);
                        return Err(CallError::Dead(why));
                    }
                    _ => {}
                }
            }
            if finish.is_some() || off >= data.len() {
                break;
            }
            let n = (data.len() - off).min(MAX_CHUNK);
            if sent - acked + n as u64 > window {
                match rx.recv_timeout(idle) {
                    Ok(Msg::Progress(done)) => acked = acked.max(done),
                    Ok(Msg::Done(r)) => finish = Some(r),
                    Ok(Msg::Dead(why)) => {
                        self.unregister(id);
                        return Err(CallError::Dead(why));
                    }
                    Ok(_) => {}
                    Err(RecvTimeoutError::Timeout) => return Err(self.timed_out(id)),
                    Err(RecvTimeoutError::Disconnected) => return Err(CallError::Dead(self.dead_reason().unwrap_or_else(|| "helper gone".into()))),
                }
                if started.elapsed() > total {
                    return Err(self.timed_out(id));
                }
                continue;
            }
            if let Err(e) = self.send_data(id, off as u64, &data[off..off + n]) {
                self.unregister(id);
                return Err(e);
            }
            sent += n as u64;
            off += n;
        }
        let r = match finish {
            Some(r) => r,
            None => self.wait_done(id, &rx, idle, total.saturating_sub(started.elapsed()).max(idle), false, &mut Collected::default())?,
        };
        self.unregister(id);
        r.map(|_| ()).map_err(CallError::Remote)
    }

    // ------------------------------------------------------------------ plumbing

    fn acquire(&self, wait: Duration) -> Result<Permit, CallError> {
        let (m, cv) = &self.inner.gate;
        let deadline = Instant::now() + wait;
        let mut n = m.lock().map_err(|_| CallError::Dead("poisoned".into()))?;
        while *n >= MAX_CALLS {
            if self.is_dead() {
                return Err(CallError::Dead(self.dead_reason().unwrap_or_default()));
            }
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return Err(CallError::Timeout);
            }
            let (g, _) = cv.wait_timeout(n, left.min(Duration::from_millis(200))).map_err(|_| CallError::Dead("poisoned".into()))?;
            n = g;
        }
        *n += 1;
        Ok(Permit { inner: self.inner.clone() })
    }

    fn register(&self) -> Result<(u64, Receiver<Msg>), CallError> {
        if let Some(why) = self.dead_reason() {
            return Err(CallError::Dead(why));
        }
        let id = self.inner.next_id.fetch_add(1, Ordering::SeqCst);
        let (tx, rx) = channel();
        self.inner.pending.lock().map_err(|_| CallError::Dead("poisoned".into()))?.insert(id, tx);
        // Closed between the check and the insert: the death sweep may have missed us.
        if let Some(why) = self.dead_reason() {
            self.unregister(id);
            return Err(CallError::Dead(why));
        }
        Ok((id, rx))
    }

    fn unregister(&self, id: u64) {
        if let Ok(mut p) = self.inner.pending.lock() {
            p.remove(&id);
        }
    }

    fn send(&self, id: u64, request: Request) -> Result<(), CallError> {
        let mut w = self.inner.writer.lock().map_err(|_| CallError::Dead("poisoned".into()))?;
        let Some(w) = w.as_mut() else { return Err(CallError::Dead(self.dead_reason().unwrap_or_else(|| "closed".into()))) };
        write_control(w, &RequestFrame { id, request }).map_err(|e| {
            self.inner.die(&format!("write to the helper failed: {e}"));
            CallError::Dead(format!("write to the helper failed: {e}"))
        })
    }

    fn send_data(&self, id: u64, offset: u64, bytes: &[u8]) -> Result<(), CallError> {
        let mut w = self.inner.writer.lock().map_err(|_| CallError::Dead("poisoned".into()))?;
        let Some(w) = w.as_mut() else { return Err(CallError::Dead(self.dead_reason().unwrap_or_else(|| "closed".into()))) };
        write_data(w, id, offset, bytes).map_err(|e| {
            self.inner.die(&format!("write to the helper failed: {e}"));
            CallError::Dead(format!("write to the helper failed: {e}"))
        })
    }

    fn timed_out(&self, id: u64) -> CallError {
        self.unregister(id);
        // Best effort: ask the helper to stop. Its answer (an unknown id) is ignored.
        let cid = self.inner.next_id.fetch_add(1, Ordering::SeqCst);
        let _ = self.send(cid, Request::Cancel { target: id });
        (self.inner.log)(&format!("call {id} timed out"));
        CallError::Timeout
    }

    fn wait_done(&self, id: u64, rx: &Receiver<Msg>, idle: Duration, total: Duration, suspend_on_prompt: bool, out: &mut Collected) -> Result<Result<Value, WireError>, CallError> {
        let started = Instant::now();
        loop {
            match rx.recv_timeout(idle) {
                Ok(Msg::Done(r)) => return Ok(r),
                Ok(Msg::Dead(why)) => return Err(CallError::Dead(why)),
                Ok(Msg::Data { offset, bytes }) => {
                    if offset != out.data.len() as u64 {
                        self.unregister(id);
                        return Err(CallError::Remote(WireError::new(ErrorKind::Protocol, "data frames out of order")));
                    }
                    out.data.extend_from_slice(&bytes);
                }
                Ok(Msg::Page(mut v)) => out.entries.append(&mut v),
                Ok(Msg::Progress(_)) => {}
                Err(RecvTimeoutError::Timeout) => {
                    if suspend_on_prompt && self.inner.prompts_open.load(Ordering::SeqCst) > 0 {
                        continue;
                    }
                    return Err(self.timed_out(id));
                }
                Err(RecvTimeoutError::Disconnected) => return Err(CallError::Dead(self.dead_reason().unwrap_or_else(|| "helper gone".into()))),
            }
            if started.elapsed() > total && !(suspend_on_prompt && self.inner.prompts_open.load(Ordering::SeqCst) > 0) {
                return Err(self.timed_out(id));
            }
        }
    }

    fn call(&self, req: Request, idle: Duration, total: Duration, suspend_on_prompt: bool) -> Result<Collected, CallError> {
        let _permit = self.acquire(idle)?;
        let (id, rx) = self.register()?;
        if let Err(e) = self.send(id, req) {
            self.unregister(id);
            return Err(e);
        }
        let mut out = Collected::default();
        let r = self.wait_done(id, &rx, idle, total, suspend_on_prompt, &mut out);
        self.unregister(id);
        match r? {
            Ok(v) => {
                out.result = v;
                Ok(out)
            }
            Err(e) => Err(CallError::Remote(e)),
        }
    }
}

impl Drop for HelperClient {
    /// Closing the helper's stdin makes it kill its ssh child and exit; the killer is the backstop.
    fn drop(&mut self) {
        self.inner.die("client dropped");
    }
}

struct Permit {
    inner: Arc<Inner>,
}
impl Drop for Permit {
    fn drop(&mut self) {
        let (m, cv) = &self.inner.gate;
        if let Ok(mut n) = m.lock() {
            *n = n.saturating_sub(1);
        }
        cv.notify_one();
    }
}

fn decode<T: DeserializeOwned>(v: Value) -> Result<T, CallError> {
    serde_json::from_value(v).map_err(|e| CallError::Remote(WireError::new(ErrorKind::Protocol, format!("unexpected result shape: {e}"))))
}

impl Inner {
    /// Mark the session dead exactly once: fail pending calls, close the pipe, end the process.
    fn die(&self, reason: &str) {
        {
            let Ok(mut d) = self.dead.lock() else { return };
            if d.is_some() {
                return;
            }
            *d = Some(reason.to_string());
        }
        self.gone.store(true, Ordering::SeqCst);
        (self.log)(&format!("helper session ended: {reason}"));
        let pending: Vec<Sender<Msg>> = self.pending.lock().map(|mut p| p.drain().map(|(_, s)| s).collect()).unwrap_or_default();
        for s in pending {
            let _ = s.send(Msg::Dead(reason.to_string()));
        }
        // Dropping the writer closes the helper's stdin: it kills its ssh child and exits by itself.
        if let Ok(mut w) = self.writer.lock() {
            *w = None;
        }
        if let Ok(mut k) = self.killer.lock() {
            if let Some(kill) = k.take() {
                kill();
            }
        }
        self.gate.1.notify_all();
    }

    fn route(&self, id: u64, m: Msg, finish: bool) {
        let tx = self.pending.lock().ok().and_then(|mut p| if finish { p.remove(&id) } else { p.get(&id).cloned() });
        if let Some(tx) = tx {
            let _ = tx.send(m);
        }
    }
}

fn reader_loop(inner: Arc<Inner>, mut reader: Box<dyn Read + Send>) {
    loop {
        match read_frame(&mut reader) {
            Ok(None) => return inner.die("the helper closed its output"),
            Err(e) => return inner.die(&format!("bad frame from the helper: {e}")),
            Ok(Some(Frame::Data { id, offset, bytes })) => inner.route(id, Msg::Data { offset, bytes }, false),
            Ok(Some(Frame::Control(b))) => match ServerMessage::from_slice(&b) {
                Err(e) => return inner.die(&format!("unreadable message from the helper: {e}")),
                Ok(ServerMessage::Result { id, result }) => inner.route(id, Msg::Done(Ok(result)), true),
                Ok(ServerMessage::Error { id: Some(id), error }) => inner.route(id, Msg::Done(Err(error)), true),
                Ok(ServerMessage::Error { id: None, error }) => return inner.die(&format!("helper protocol error: {}", error.message)),
                Ok(ServerMessage::Event(Event::Progress { id, done, .. })) => inner.route(id, Msg::Progress(done), false),
                Ok(ServerMessage::Event(Event::ListPage { id, entries })) => inner.route(id, Msg::Page(entries), false),
                Ok(ServerMessage::Event(Event::State { state: ProtoConn::Lost, reason })) => return inner.die(&format!("connection lost{}", reason.map(|r| format!(": {r}")).unwrap_or_default())),
                Ok(ServerMessage::Event(Event::State { .. })) => {}
                Ok(ServerMessage::Event(Event::Prompt { prompt_id, kind, text })) => {
                    inner.prompts_open.fetch_add(1, Ordering::SeqCst);
                    let i2 = inner.clone();
                    let ok = std::thread::Builder::new().name("brouillon-helper-prompt".into()).spawn(move || answer_prompt(i2, prompt_id, kind, text));
                    if ok.is_err() {
                        inner.prompts_open.fetch_sub(1, Ordering::SeqCst);
                    }
                }
            },
        }
    }
}

fn answer_prompt(inner: Arc<Inner>, prompt_id: u64, kind: PromptKind, text: String) {
    let secret = inner.prompts.ask(kind, &text, &inner.gone);
    inner.prompts_open.fetch_sub(1, Ordering::SeqCst);
    if inner.gone.load(Ordering::SeqCst) {
        return;
    }
    let id = inner.next_id.fetch_add(1, Ordering::SeqCst);
    // The answer is written exactly once. No log line, no error text, no copy kept.
    let written = match inner.writer.lock() {
        Ok(mut w) => match w.as_mut() {
            Some(w) => write_control(w, &RequestFrame { id, request: Request::PromptAnswer { prompt_id, secret } }),
            None => return,
        },
        Err(_) => return,
    };
    if written.is_err() {
        inner.die("write of a prompt answer failed");
    }
}
