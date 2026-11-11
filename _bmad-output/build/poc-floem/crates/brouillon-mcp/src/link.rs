//! Editor-side IPC: the relay <-> editor protocol, the local-socket listener, the instance
//! registry and the opt-in host.
//!
//! Wire protocol (v1), newline-delimited UTF-8 over a per-user local socket (named pipe on
//! Windows, Unix socket elsewhere):
//!
//! 1. relay -> editor, one line: `{"link":"brouillon","v":1,"token":"..","client":{"name":"..","parent_exe":null,"pid":123}}`
//! 2. editor -> relay, one line: `{"ok":true,"v":1,"server":"brouillon","tool_set_hash":".."}`
//!    or `{"ok":false,"v":1,"error":"bad_token|disabled|killed|version|invalid"}` and the editor closes.
//! 3. After `ok`, both sides exchange raw MCP JSON-RPC lines (the relay never parses them).

use std::io::{self, BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use interprocess::local_socket::{prelude::*, GenericFilePath, GenericNamespaced, ListenerNonblockingMode, ListenerOptions, Name, Stream};
use serde::{Deserialize, Serialize};

use crate::api::EditorApi;
use crate::core::{McpCore, Session};
use crate::jsonrpc::{read_line_bounded, Line};
use crate::settings::McpSettings;
use crate::tools;

pub const LINK_VERSION: u32 = 1;
const MAX_HELLO_BYTES: usize = 16 * 1024;

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClientHello {
    pub name: String,
    pub parent_exe: Option<String>,
    pub pid: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Hello {
    pub link: String,
    pub v: u32,
    pub token: String,
    pub client: ClientHello,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HelloReply {
    pub ok: bool,
    pub v: u32,
    #[serde(default)]
    pub server: Option<String>,
    #[serde(default)]
    pub tool_set_hash: Option<String>,
    #[serde(default)]
    pub error: Option<String>,
}

fn reply_err(e: &str) -> HelloReply {
    HelloReply { ok: false, v: LINK_VERSION, server: None, tool_set_hash: None, error: Some(e.into()) }
}

/// Comparison that does not exit early on the first differing byte.
pub fn ct_eq(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

/// Server half of the handshake. On success returns the session to serve.
pub fn accept_handshake<A: EditorApi, R: BufRead, W: Write>(core: &McpCore<A>, token: &str, r: &mut R, w: &mut W) -> io::Result<Result<Session, HelloReply>> {
    accept_handshake_peer(core, token, r, w, None)
}

/// [`accept_handshake`] with the pid the OS reports for the connecting process. A relay that claims
/// a different pid than the one actually connected is refused (it is lying about who it is).
pub fn accept_handshake_peer<A: EditorApi, R: BufRead, W: Write>(core: &McpCore<A>, token: &str, r: &mut R, w: &mut W, peer_pid: Option<u32>) -> io::Result<Result<Session, HelloReply>> {
    let line = match read_line_bounded(r)? {
        Line::Text(t) if t.len() <= MAX_HELLO_BYTES => t,
        _ => return finish(w, reply_err("invalid")),
    };
    let hello: Hello = match serde_json::from_str(&line) {
        Ok(h) => h,
        Err(_) => return finish(w, reply_err("invalid")),
    };
    if hello.link != "brouillon" {
        return finish(w, reply_err("invalid"));
    }
    if !ct_eq(hello.token.as_bytes(), token.as_bytes()) {
        return finish(w, reply_err("bad_token"));
    }
    if hello.v != LINK_VERSION {
        return finish(w, reply_err("version"));
    }
    if let (Some(claimed), Some(actual)) = (hello.client.pid, peer_pid) {
        if claimed != actual {
            return finish(w, reply_err("invalid"));
        }
    }
    if core.is_killed() {
        return finish(w, reply_err("killed"));
    }
    let ok = HelloReply { ok: true, v: LINK_VERSION, server: Some(tools::SERVER_NAME.into()), tool_set_hash: Some(tools::tool_set_hash()), error: None };
    writeln!(w, "{}", serde_json::to_string(&ok).map_err(io::Error::other)?)?;
    w.flush()?;
    let name: String = hello.client.name.chars().filter(|c| !c.is_control()).take(64).collect();
    let mut s = Session::new(&name);
    s.transport = "ipc".into();
    s.exe_hash = hello.client.parent_exe.as_deref().map(crate::exe_fingerprint);
    s.exe_path = hello.client.parent_exe.as_deref().map(|p| p.chars().filter(|c| !c.is_control()).take(260).collect());
    Ok(Ok(s))
}

fn finish(w: &mut impl Write, r: HelloReply) -> io::Result<Result<Session, HelloReply>> {
    writeln!(w, "{}", serde_json::to_string(&r).map_err(io::Error::other)?)?;
    w.flush()?;
    Ok(Err(r))
}

/// Client (relay) half of the handshake.
pub fn client_handshake<R: BufRead, W: Write>(r: &mut R, w: &mut W, token: &str, client: &ClientHello) -> io::Result<HelloReply> {
    let hello = Hello { link: "brouillon".into(), v: LINK_VERSION, token: token.into(), client: client.clone() };
    writeln!(w, "{}", serde_json::to_string(&hello).map_err(io::Error::other)?)?;
    w.flush()?;
    match read_line_bounded(r)? {
        Line::Text(t) => serde_json::from_str(&t).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e)),
        _ => Err(io::Error::new(io::ErrorKind::UnexpectedEof, "editor closed during handshake")),
    }
}

/// Handshake then serve MCP on one accepted connection.
pub fn serve_connection<A: EditorApi, R: Read, W: Write>(core: &McpCore<A>, token: &str, reader: R, writer: W) -> io::Result<()> {
    serve_connection_peer(core, token, reader, writer, None)
}

pub fn serve_connection_peer<A: EditorApi, R: Read, W: Write>(core: &McpCore<A>, token: &str, reader: R, mut writer: W, peer_pid: Option<u32>) -> io::Result<()> {
    let mut r = BufReader::new(reader);
    match accept_handshake_peer(core, token, &mut r, &mut writer, peer_pid)? {
        Ok(mut session) => {
            core.conn_open();
            let res = core.serve(&mut session, r, writer);
            core.conn_close();
            res
        }
        Err(_) => Ok(()),
    }
}

// ---------- names, token, directories ----------

pub fn pipe_name(tag: &str) -> String {
    let user = std::env::var("USERNAME").or_else(|_| std::env::var("USER")).unwrap_or_default();
    let user: String = user.chars().filter(|c| c.is_ascii_alphanumeric()).collect();
    format!("brouillon-link-{user}-{tag}")
}

pub fn local_name(s: &str) -> io::Result<Name<'static>> {
    if cfg!(windows) {
        s.to_string().to_ns_name::<GenericNamespaced>().map(Name::into_owned)
    } else {
        let p = std::env::temp_dir().join(format!("{s}.sock"));
        p.to_str().ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "non-UTF-8 temp dir"))?.to_string().to_fs_name::<GenericFilePath>().map(Name::into_owned)
    }
}

/// `<dir>/link.token`, created random (owner-only on Unix) when missing.
pub fn load_or_create_token(dir: &Path) -> io::Result<String> {
    let path = dir.join("link.token");
    if let Ok(t) = std::fs::read_to_string(&path) {
        if t.trim().len() >= 32 {
            let _ = crate::os_guard::restrict_to_owner(&path); // also fixes a file made by an older version
            return Ok(t.trim().to_string());
        }
    }
    std::fs::create_dir_all(dir)?;
    let token = crate::http::random_token();
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create(true).truncate(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut opts, 0o600);
    opts.open(&path)?.write_all(token.as_bytes())?;
    let _ = crate::os_guard::restrict_to_owner(&path); // Windows ACL (Unix already 0600); best effort
    Ok(token)
}

/// `%LOCALAPPDATA%\Brouillon\link` on Windows, `$XDG_RUNTIME_DIR/brouillon` (or temp) elsewhere.
pub fn default_link_dir() -> PathBuf {
    if cfg!(windows) {
        std::env::var_os("LOCALAPPDATA").map(PathBuf::from).unwrap_or_else(std::env::temp_dir).join("Brouillon").join("link")
    } else {
        std::env::var_os("XDG_RUNTIME_DIR").map(PathBuf::from).unwrap_or_else(std::env::temp_dir).join("brouillon")
    }
}

// ---------- instance registry ----------

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InstanceRecord {
    pub pid: u32,
    pub started_at: u64,
    pub pipe: String,
    pub protocol_versions: Vec<String>,
    pub workspace_roots: Vec<String>,
    pub tool_set_hash: String,
    /// Loopback HTTP port, only while the user enabled the HTTP transport.
    #[serde(default)]
    pub http_port: Option<u16>,
    /// Bearer token of the HTTP transport: random per start, in this user-private file only.
    #[serde(default)]
    pub http_token: Option<String>,
}

fn instances_dir(dir: &Path) -> PathBuf {
    dir.join("instances")
}

pub fn write_instance(dir: &Path, rec: &InstanceRecord) -> io::Result<()> {
    let d = instances_dir(dir);
    std::fs::create_dir_all(&d)?;
    let tmp = d.join(format!("{}.json.tmp", rec.pid));
    // The file may carry the HTTP bearer token: owner-only on Unix (on Windows it inherits the
    // user-profile ACL of %LOCALAPPDATA%).
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create(true).truncate(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut opts, 0o600);
    opts.open(&tmp)?.write_all(&serde_json::to_vec(rec).map_err(io::Error::other)?)?;
    let _ = crate::os_guard::restrict_to_owner(&tmp); // keeps its protected ACL across the rename
    std::fs::rename(tmp, d.join(format!("{}.json", rec.pid)))
}

pub fn remove_instance(dir: &Path, pid: u32) {
    let _ = std::fs::remove_file(instances_dir(dir).join(format!("{pid}.json")));
}

pub fn read_instances(dir: &Path) -> Vec<InstanceRecord> {
    let Ok(rd) = std::fs::read_dir(instances_dir(dir)) else { return vec![] };
    let mut v: Vec<InstanceRecord> = rd
        .flatten()
        .filter(|e| e.path().extension().map_or(false, |x| x == "json"))
        .filter_map(|e| std::fs::read(e.path()).ok())
        .filter_map(|b| serde_json::from_slice(&b).ok())
        .collect();
    v.sort_by(|a, b| b.started_at.cmp(&a.started_at));
    v
}

/// Shim instance selection: `--instance` > the instance whose workspace contains the CWD >
/// the most recently started one. Returns candidates in the order to try.
pub fn select_instances(mut all: Vec<InstanceRecord>, wanted_pid: Option<u32>, cwd: Option<&str>) -> Vec<InstanceRecord> {
    all.sort_by(|a, b| b.started_at.cmp(&a.started_at));
    if let Some(p) = wanted_pid {
        return all.into_iter().filter(|r| r.pid == p).collect();
    }
    if let Some(cwd) = cwd.and_then(|c| crate::policy::normalize(c).ok()) {
        let (mut hit, rest): (Vec<_>, Vec<_>) = all.into_iter().partition(|r| {
            r.workspace_roots.iter().filter_map(|w| crate::policy::normalize(w).ok()).any(|w| cwd.starts_with(&w))
        });
        hit.extend(rest);
        return hit;
    }
    all
}

// ---------- listener ----------

/// Accept loop on a local socket. One thread per connection, each runs the handshake + MCP.
pub struct LinkListener {
    stop: Arc<AtomicBool>,
}

/// Most simultaneous pipe connections (each is a thread).
const MAX_PIPE_CONNECTIONS: usize = 32;
/// A client that connects but never sends its hello is dropped after this long.
const HELLO_TIMEOUT: Duration = Duration::from_secs(5);
/// How often a blocked connection looks at the kill switch: the bound on "drop on kill".
const POLL_STEP: Duration = Duration::from_millis(5);

/// One accepted connection, read and written in non-blocking mode and polled every few
/// milliseconds, so that the kill switch (or the listener stopping) ends it at once instead of
/// waiting for a client that sends nothing. EOF is what the connection loop sees on kill.
struct PollConn {
    stream: Arc<Stream>,
    dead: Arc<dyn Fn() -> bool + Send + Sync>,
    /// Absolute deadline while the hello is pending.
    deadline: Arc<Mutex<Option<Instant>>>,
}

impl PollConn {
    fn pair(s: Stream, dead: Arc<dyn Fn() -> bool + Send + Sync>) -> io::Result<(PollConn, PollConn, Arc<Mutex<Option<Instant>>>)> {
        // Unix sockets support a real non-blocking mode. Windows pipes do not (see os_guard):
        // there the reader peeks for data before each blocking read.
        #[cfg(not(windows))]
        s.set_nonblocking(true)?;
        let a = Arc::new(s);
        let deadline = Arc::new(Mutex::new(Some(Instant::now() + HELLO_TIMEOUT)));
        Ok((PollConn { stream: a.clone(), dead: dead.clone(), deadline: deadline.clone() }, PollConn { stream: a, dead, deadline: deadline.clone() }, deadline))
    }
}

impl Read for PollConn {
    fn read(&mut self, b: &mut [u8]) -> io::Result<usize> {
        loop {
            if (self.dead)() {
                return Ok(0);
            }
            if let Some(d) = *self.deadline.lock().unwrap_or_else(|p| p.into_inner()) {
                if Instant::now() >= d {
                    return Err(io::Error::new(io::ErrorKind::TimedOut, "no hello in time"));
                }
            }
            #[cfg(windows)]
            match crate::os_guard::pipe_bytes_available(&self.stream) {
                Ok(0) => {
                    std::thread::sleep(POLL_STEP);
                    continue;
                }
                Ok(_) => {}
                Err(_) => return Ok(0), // the client is gone: EOF
            }
            match (&*self.stream).read(b) {
                Ok(n) => return Ok(n),
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => std::thread::sleep(POLL_STEP),
                Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                Err(e) => return Err(e),
            }
        }
    }
}

impl Write for PollConn {
    fn write(&mut self, b: &[u8]) -> io::Result<usize> {
        loop {
            if (self.dead)() {
                return Err(io::Error::new(io::ErrorKind::ConnectionAborted, "stopped"));
            }
            match (&*self.stream).write(b) {
                Ok(n) => return Ok(n),
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => std::thread::sleep(POLL_STEP),
                Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                Err(e) => return Err(e),
            }
        }
    }
    fn flush(&mut self) -> io::Result<()> {
        match (&*self.stream).flush() {
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => Ok(()),
            r => r,
        }
    }
}

/// A [`Write`] wrapper that clears the hello deadline once the handshake reply went out.
struct HelloDone<W: Write> {
    inner: W,
    deadline: Arc<Mutex<Option<Instant>>>,
}
impl<W: Write> Write for HelloDone<W> {
    fn write(&mut self, b: &[u8]) -> io::Result<usize> {
        // The first reply line (ok or error) ends the hello phase.
        *self.deadline.lock().unwrap_or_else(|p| p.into_inner()) = None;
        self.inner.write(b)
    }
    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}

impl LinkListener {
    pub fn start<A: EditorApi + 'static>(core: Arc<McpCore<A>>, pipe: &str, token: &str) -> io::Result<Self> {
        Self::start_in(core, pipe, token, None)
    }

    /// `own_dir`: the user-owned link directory (reference owner for the Unix peer check).
    pub fn start_in<A: EditorApi + 'static>(core: Arc<McpCore<A>>, pipe: &str, token: &str, own_dir: Option<&Path>) -> io::Result<Self> {
        #[allow(unused_mut)]
        let mut opts = ListenerOptions::new().name(local_name(pipe)?).nonblocking(ListenerNonblockingMode::Accept);
        // Windows: a DACL for the current user only, instead of the default (which lets every local
        // account read the pipe). If it cannot be built the listener is NOT started: no silent
        // fallback to the open default.
        #[cfg(windows)]
        {
            use interprocess::os::windows::local_socket::ListenerOptionsExt;
            opts = opts.security_descriptor(crate::os_guard::owner_only_descriptor()?);
        }
        let listener = opts.create_sync()?;
        let stop = Arc::new(AtomicBool::new(false));
        let (stop2, token) = (stop.clone(), token.to_string());
        let own_dir = own_dir.map(Path::to_path_buf).unwrap_or_else(default_link_dir);
        let live = Arc::new(AtomicUsize::new(0));
        std::thread::spawn(move || {
            while !stop2.load(Ordering::Relaxed) {
                match listener.accept() {
                    Ok(conn) => {
                        if live.fetch_add(1, Ordering::SeqCst) >= MAX_PIPE_CONNECTIONS {
                            live.fetch_sub(1, Ordering::SeqCst);
                            continue; // dropped: the client sees EOF
                        }
                        let creds = conn.peer_creds().ok();
                        if let Some(c) = &creds {
                            if crate::os_guard::peer_user_matches(c, &own_dir) == crate::os_guard::Peer::OtherUser {
                                live.fetch_sub(1, Ordering::SeqCst);
                                continue; // another account: refused before a single byte is read
                            }
                        }
                        let peer_pid = creds.as_ref().and_then(crate::os_guard::peer_pid);
                        let (core, token, stop3, live2) = (core.clone(), token.clone(), stop2.clone(), live.clone());
                        std::thread::spawn(move || {
                            let c2 = core.clone();
                            let dead: Arc<dyn Fn() -> bool + Send + Sync> = Arc::new(move || stop3.load(Ordering::Relaxed) || c2.is_killed());
                            if let Ok((r, w, deadline)) = PollConn::pair(conn, dead) {
                                let w = HelloDone { inner: w, deadline };
                                let _ = serve_connection_peer(&*core, &token, r, w, peer_pid);
                            }
                            live2.fetch_sub(1, Ordering::SeqCst);
                        });
                    }
                    Err(e) if e.kind() == io::ErrorKind::WouldBlock => std::thread::sleep(Duration::from_millis(20)),
                    Err(_) => break,
                }
            }
        });
        Ok(Self { stop })
    }

    pub fn stop(&self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

impl Drop for LinkListener {
    fn drop(&mut self) {
        self.stop();
    }
}

/// Both halves of one local-socket stream (`&Stream` is Read + Write).
pub struct Shared(Arc<Stream>);

impl Shared {
    pub fn pair(s: Stream) -> (Shared, Shared) {
        let a = Arc::new(s);
        (Shared(a.clone()), Shared(a))
    }
}

impl Read for Shared {
    fn read(&mut self, b: &mut [u8]) -> io::Result<usize> {
        (&*self.0).read(b)
    }
}

impl Write for Shared {
    fn write(&mut self, b: &[u8]) -> io::Result<usize> {
        (&*self.0).write(b)
    }
    fn flush(&mut self) -> io::Result<()> {
        (&*self.0).flush()
    }
}

/// Connect to a running editor's pipe (relay side).
pub fn connect(pipe: &str) -> io::Result<Stream> {
    Stream::connect(local_name(pipe)?)
}

pub struct LinkDirs {
    pub dir: PathBuf,
    pub pipe_tag: String,
}

/// The running server. Exists only when the user enabled Link.
pub struct LinkHost {
    listener: LinkListener,
    http: Option<crate::http::HttpServer>,
    dir: PathBuf,
    pid: u32,
    pub pipe: String,
}

impl LinkHost {
    /// `Ok(None)` when disabled: no thread, no socket, no file. Nothing happens until enabled.
    /// The loopback HTTP listener exists only when `settings.http` is also set.
    pub fn start<A: EditorApi + 'static>(settings: &McpSettings, core: Arc<McpCore<A>>, dirs: &LinkDirs, workspace_roots: Vec<String>) -> io::Result<Option<Self>> {
        if !settings.enabled {
            return Ok(None);
        }
        let token = load_or_create_token(&dirs.dir)?;
        let pipe = pipe_name(&dirs.pipe_tag);
        let http = if settings.http { Some(crate::http::HttpServer::start(core.clone(), crate::http::HttpConfig::default())?) } else { None };
        let listener = LinkListener::start_in(core, &pipe, &token, Some(&dirs.dir))?;
        let pid = std::process::id();
        let rec = InstanceRecord {
            pid,
            started_at: std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0),
            pipe: pipe.clone(),
            protocol_versions: crate::core::SUPPORTED_PROTOCOLS.iter().map(|s| s.to_string()).collect(),
            workspace_roots,
            tool_set_hash: tools::tool_set_hash(),
            http_port: http.as_ref().map(|h| h.port()),
            http_token: http.as_ref().map(|h| h.token().to_string()),
        };
        write_instance(&dirs.dir, &rec)?;
        Ok(Some(Self { listener, http, dir: dirs.dir.clone(), pid, pipe }))
    }

    /// `http://127.0.0.1:<port>/mcp` while the HTTP transport runs.
    pub fn http_url(&self) -> Option<String> {
        self.http.as_ref().map(|h| h.url())
    }

    pub fn stop(self) {
        drop(self);
    }
}

impl Drop for LinkHost {
    fn drop(&mut self) {
        self.listener.stop();
        if let Some(h) = &self.http {
            h.stop();
        }
        remove_instance(&self.dir, self.pid);
    }
}

/// What "Test" in Preferences does: act as a minimal client against `pipe` (handshake, `initialize`,
/// `tools/list`) and report how many tools the editor offers. Never calls a tool, so it needs no
/// consent. Gives up after `timeout`.
pub fn self_test(pipe: &str, token: &str, timeout: Duration) -> Result<usize, String> {
    let pipe = pipe.to_string();
    let token = token.to_string();
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let run = || -> Result<usize, String> {
            let conn = connect(&pipe).map_err(|e| format!("cannot connect: {e}"))?;
            let (r, mut w) = Shared::pair(conn);
            let mut r = BufReader::new(r);
            let hello = ClientHello { name: "brouillon-self-test".into(), parent_exe: None, pid: Some(std::process::id()) };
            let reply = client_handshake(&mut r, &mut w, &token, &hello).map_err(|e| format!("handshake failed: {e}"))?;
            if !reply.ok {
                return Err(format!("refused: {}", reply.error.unwrap_or_default()));
            }
            let init = r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-11-25","capabilities":{},"clientInfo":{"name":"brouillon-self-test","version":"1"}}}"#;
            let list = r#"{"jsonrpc":"2.0","id":2,"method":"tools/list"}"#;
            writeln!(w, "{init}\n{list}").and_then(|_| w.flush()).map_err(|e| e.to_string())?;
            let mut count = None;
            for _ in 0..2 {
                match read_line_bounded(&mut r).map_err(|e| e.to_string())? {
                    Line::Text(t) => {
                        let v: serde_json::Value = serde_json::from_str(&t).map_err(|e| e.to_string())?;
                        if v["id"] == 2 {
                            count = v.pointer("/result/tools").and_then(|t| t.as_array()).map(|a| a.len());
                        }
                    }
                    _ => return Err("the editor closed the connection".into()),
                }
            }
            count.ok_or_else(|| "no tool list in the answer".to_string())
        };
        let _ = tx.send(run());
    });
    rx.recv_timeout(timeout).unwrap_or_else(|_| Err("no answer in time".into()))
}
