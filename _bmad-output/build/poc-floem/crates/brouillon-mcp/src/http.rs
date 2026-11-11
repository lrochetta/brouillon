//! Loopback Streamable HTTP transport (spec 60 section 3.2, V2, off by default).
//!
//! A deliberately small HTTP/1.1 server on `127.0.0.1` (never another interface), one request per
//! connection, behind exactly the same [`McpCore`] and permission engine as the pipe. What it checks,
//! in this order, before it reads a single byte of body:
//!
//! 1. `Host` must be `127.0.0.1:<port>` or `localhost:<port>` (DNS rebinding: a rebound hostname
//!    carries the attacker's name in `Host`);
//! 2. an `Origin` header is refused outright (browsers always send one; no Origin is allowlisted
//!    and CORS is never answered: no `Access-Control-*` header exists anywhere in this file);
//! 3. the path is exactly `/mcp`, the method is `POST` (no GET stream, no SSE);
//! 4. `Authorization: Bearer <token>` compared in constant time (the token is random per start and
//!    lives in the instance file, never in a URL);
//! 5. `Content-Type: application/json` (so a `text/plain` cross-site form post cannot reach the core)
//!    and a `Content-Length` within the 1 MiB message cap (no chunked bodies).
//!
//! Identity is the client name from `initialize` (`Mcp-Session-Id` carries it to later requests);
//! consent, scopes, rate limits and the kill switch are the core's, unchanged.

use std::collections::HashMap;
use std::io::{self, Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use rand::{Rng, SeedableRng};
use serde_json::Value;

use crate::api::EditorApi;
use crate::core::{McpCore, Session};
use crate::jsonrpc::MAX_MESSAGE_BYTES;
use crate::link::ct_eq;

const MAX_HEADER_BYTES: usize = 16 * 1024;
const MAX_CONNECTIONS: usize = 16;
const MAX_SESSIONS: usize = 64;
const HTTP_PREFIX_ANON: &str = "http:client";

#[derive(Debug, Clone)]
pub struct HttpConfig {
    /// Read/write timeout of one connection (slow-loris guard).
    pub io_timeout: Duration,
}

impl Default for HttpConfig {
    fn default() -> Self {
        Self { io_timeout: Duration::from_secs(10) }
    }
}

pub struct HttpServer {
    stop: Arc<AtomicBool>,
    addr: SocketAddr,
    token: String,
}

pub fn random_token() -> String {
    let mut rng = rand::rngs::StdRng::from_entropy();
    (0..48).map(|_| rng.sample(rand::distributions::Alphanumeric) as char).collect()
}

struct Shared<A: EditorApi> {
    core: Arc<McpCore<A>>,
    token: String,
    port: u16,
    cfg: HttpConfig,
    sessions: Mutex<(HashMap<String, Session>, Vec<String>)>,
    live: AtomicUsize,
}

impl HttpServer {
    /// Bind `127.0.0.1:<random port>` and serve until [`HttpServer::stop`] (or drop).
    pub fn start<A: EditorApi + 'static>(core: Arc<McpCore<A>>, cfg: HttpConfig) -> io::Result<Self> {
        let listener = TcpListener::bind(("127.0.0.1", 0))?;
        listener.set_nonblocking(true)?;
        let addr = listener.local_addr()?;
        let token = random_token();
        let stop = Arc::new(AtomicBool::new(false));
        let shared = Arc::new(Shared { core, token: token.clone(), port: addr.port(), cfg, sessions: Mutex::new((HashMap::new(), Vec::new())), live: AtomicUsize::new(0) });
        let stop2 = stop.clone();
        std::thread::spawn(move || {
            while !stop2.load(Ordering::Relaxed) {
                match listener.accept() {
                    Ok((stream, _)) => {
                        if shared.live.fetch_add(1, Ordering::SeqCst) >= MAX_CONNECTIONS {
                            shared.live.fetch_sub(1, Ordering::SeqCst);
                            let _ = write_response(stream, 503, "Service Unavailable", &[], b"{\"error\":\"busy\"}");
                            continue;
                        }
                        let sh = shared.clone();
                        std::thread::spawn(move || {
                            handle(&sh, stream);
                            sh.live.fetch_sub(1, Ordering::SeqCst);
                        });
                    }
                    Err(e) if e.kind() == io::ErrorKind::WouldBlock => std::thread::sleep(Duration::from_millis(15)),
                    Err(_) => break,
                }
            }
            // `listener` drops here: the port stops answering.
        });
        Ok(Self { stop, addr, token })
    }

    pub fn addr(&self) -> SocketAddr {
        self.addr
    }
    pub fn port(&self) -> u16 {
        self.addr.port()
    }
    pub fn token(&self) -> &str {
        &self.token
    }
    pub fn url(&self) -> String {
        format!("http://127.0.0.1:{}/mcp", self.addr.port())
    }
    pub fn stop(&self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

impl Drop for HttpServer {
    fn drop(&mut self) {
        self.stop();
    }
}

struct Request {
    method: String,
    path: String,
    headers: HashMap<String, String>,
}

enum ParseError {
    Bad,
    TooLarge,
    Io,
}

/// Read up to the blank line that ends the headers. Returns the parsed head and any body bytes
/// that arrived in the same reads.
fn read_head(stream: &mut TcpStream) -> Result<(Request, Vec<u8>), ParseError> {
    let mut buf: Vec<u8> = Vec::new();
    let mut chunk = [0u8; 2048];
    let end = loop {
        if let Some(i) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
            break i;
        }
        if buf.len() > MAX_HEADER_BYTES {
            return Err(ParseError::TooLarge);
        }
        match stream.read(&mut chunk) {
            Ok(0) => return Err(ParseError::Io),
            Ok(n) => buf.extend_from_slice(&chunk[..n]),
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(_) => return Err(ParseError::Io),
        }
    };
    let head = std::str::from_utf8(&buf[..end]).map_err(|_| ParseError::Bad)?;
    let mut lines = head.split("\r\n");
    let first = lines.next().ok_or(ParseError::Bad)?;
    let mut parts = first.split(' ');
    let (method, path, version) = (parts.next(), parts.next(), parts.next());
    let (Some(method), Some(path), Some(version)) = (method, path, version) else { return Err(ParseError::Bad) };
    if !version.starts_with("HTTP/1.") || parts.next().is_some() {
        return Err(ParseError::Bad);
    }
    let mut headers = HashMap::new();
    for l in lines {
        let (k, v) = l.split_once(':').ok_or(ParseError::Bad)?;
        // A repeated security-relevant header is ambiguous: refuse rather than pick one.
        if headers.insert(k.trim().to_ascii_lowercase(), v.trim().to_string()).is_some() {
            return Err(ParseError::Bad);
        }
    }
    Ok((Request { method: method.to_string(), path: path.to_string(), headers }, buf[end + 4..].to_vec()))
}

fn write_response(mut stream: TcpStream, code: u16, reason: &str, extra: &[(&str, String)], body: &[u8]) -> io::Result<()> {
    let mut head = format!("HTTP/1.1 {code} {reason}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nCache-Control: no-store\r\nConnection: close\r\nX-Content-Type-Options: nosniff\r\n", body.len());
    for (k, v) in extra {
        // Header values come from our own code, but never let a CR/LF through.
        head.push_str(&format!("{k}: {}\r\n", v.replace(['\r', '\n'], " ")));
    }
    head.push_str("\r\n");
    stream.write_all(head.as_bytes())?;
    stream.write_all(body)?;
    stream.flush()
}

fn err_body(msg: &str) -> Vec<u8> {
    serde_json::json!({"error": msg}).to_string().into_bytes()
}

fn handle<A: EditorApi + 'static>(sh: &Shared<A>, mut stream: TcpStream) {
    // An accepted socket can inherit the listener's non-blocking flag (Windows does): undo it.
    let _ = stream.set_nonblocking(false);
    let _ = stream.set_read_timeout(Some(sh.cfg.io_timeout));
    let _ = stream.set_write_timeout(Some(sh.cfg.io_timeout));
    let _ = stream.set_nodelay(true);
    let (req, mut body) = match read_head(&mut stream) {
        Ok(x) => x,
        Err(ParseError::Bad) => return drop(write_response(stream, 400, "Bad Request", &[], &err_body("bad request"))),
        Err(ParseError::TooLarge) => return drop(write_response(stream, 431, "Request Header Fields Too Large", &[], &err_body("headers too large"))),
        Err(ParseError::Io) => return,
    };
    let reply = |stream: TcpStream, code: u16, reason: &str, msg: &str| drop(write_response(stream, code, reason, &[], &err_body(msg)));

    // 1. Host: only our own loopback names and port.
    let host_ok = req.headers.get("host").map_or(false, |h| {
        let h = h.to_ascii_lowercase();
        h == format!("127.0.0.1:{}", sh.port) || h == format!("localhost:{}", sh.port)
    });
    if !host_ok {
        return reply(stream, 403, "Forbidden", "bad host");
    }
    // 2. Origin: browsers send one, local programs do not. None is allowed.
    if req.headers.contains_key("origin") {
        return reply(stream, 403, "Forbidden", "cross-origin requests are refused");
    }
    // 3. Path and method.
    if req.path != "/mcp" {
        return reply(stream, 404, "Not Found", "not found");
    }
    if req.method != "POST" && req.method != "DELETE" {
        return drop(write_response(stream, 405, "Method Not Allowed", &[("Allow", "POST, DELETE".into())], &err_body("method not allowed")));
    }
    // 4. Bearer token, constant time.
    let given = req.headers.get("authorization").and_then(|a| a.strip_prefix("Bearer ")).unwrap_or("");
    if !ct_eq(given.as_bytes(), sh.token.as_bytes()) {
        return drop(write_response(stream, 401, "Unauthorized", &[("WWW-Authenticate", "Bearer".into())], &err_body("missing or wrong token")));
    }
    if sh.core.is_killed() {
        return reply(stream, 503, "Service Unavailable", "stopped by the user");
    }
    let sid = req.headers.get("mcp-session-id").cloned();
    if req.method == "DELETE" {
        if let Some(s) = &sid {
            let mut g = sh.sessions.lock().unwrap_or_else(|p| p.into_inner());
            g.0.remove(s);
            g.1.retain(|x| x != s);
        }
        return drop(write_response(stream, 204, "No Content", &[], b""));
    }
    // 5. Content type and length.
    let ct = req.headers.get("content-type").map(|c| c.to_ascii_lowercase()).unwrap_or_default();
    if !ct.starts_with("application/json") {
        return reply(stream, 415, "Unsupported Media Type", "content-type must be application/json");
    }
    if req.headers.contains_key("transfer-encoding") {
        return reply(stream, 411, "Length Required", "chunked bodies are not supported");
    }
    let Some(len) = req.headers.get("content-length").and_then(|l| l.parse::<usize>().ok()) else {
        return reply(stream, 411, "Length Required", "content-length is required");
    };
    if len > MAX_MESSAGE_BYTES {
        return reply(stream, 413, "Payload Too Large", "message too large");
    }
    while body.len() < len {
        let mut chunk = vec![0u8; (len - body.len()).min(64 * 1024)];
        match stream.read(&mut chunk) {
            Ok(0) | Err(_) => return reply(stream, 408, "Request Timeout", "incomplete body"),
            Ok(n) => body.extend_from_slice(&chunk[..n]),
        }
    }
    body.truncate(len);
    let Ok(text) = String::from_utf8(body) else { return reply(stream, 400, "Bad Request", "body is not UTF-8") };
    // A JSON-RPC body is one line: a newline inside would be framing trouble for no benefit.
    let text = text.trim().to_string();

    let is_initialize = serde_json::from_str::<Value>(&text).ok().and_then(|v| v.get("method").and_then(Value::as_str).map(|m| m == "initialize")).unwrap_or(false);
    let mut session = match sid.as_deref().and_then(|s| sh.sessions.lock().unwrap_or_else(|p| p.into_inner()).0.get(s).cloned()) {
        Some(s) => s,
        None => {
            // `initialize` fills the name from clientInfo; any other first call is an anonymous client.
            let mut s = Session::new(if is_initialize { "" } else { HTTP_PREFIX_ANON });
            s.transport = "http".into();
            s
        }
    };
    sh.core.conn_open();
    let out = sh.core.handle_line(&mut session, &text);
    sh.core.conn_close();
    // Consent is per client NAME, and an HTTP client can claim any name: keep its namespace apart
    // from the pipe's, so it can never ride on the consent a pipe client got.
    if !session.client.starts_with("http:") {
        session.client = if session.client.is_empty() { HTTP_PREFIX_ANON.to_string() } else { format!("http:{}", session.client) };
    }
    let mut extra: Vec<(&str, String)> = Vec::new();
    if is_initialize {
        let id = random_token();
        let mut g = sh.sessions.lock().unwrap_or_else(|p| p.into_inner());
        if g.1.len() >= MAX_SESSIONS {
            let old = g.1.remove(0);
            g.0.remove(&old);
        }
        g.0.insert(id.clone(), session);
        g.1.push(id.clone());
        extra.push(("Mcp-Session-Id", id));
    } else if let Some(s) = &sid {
        let mut g = sh.sessions.lock().unwrap_or_else(|p| p.into_inner());
        if g.0.contains_key(s) {
            g.0.insert(s.clone(), session);
        }
    }
    match out {
        Some(r) => drop(write_response(stream, 200, "OK", &extra, r.as_bytes())),
        None => drop(write_response(stream, 202, "Accepted", &extra, b"")),
    }
}
