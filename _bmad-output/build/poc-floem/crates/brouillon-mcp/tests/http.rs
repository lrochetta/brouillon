//! Loopback Streamable HTTP: same permission core, plus Host / Origin / token / framing checks.

use std::io::{Read, Write};
use std::net::TcpStream;
use std::sync::Arc;
use std::time::Duration;

use brouillon_mcp::core::{McpConfig, McpCore};
use brouillon_mcp::fake::FakeEditor;
use brouillon_mcp::http::{HttpConfig, HttpServer};
use brouillon_mcp::policy::{Grant, Scope};
use serde_json::{json, Value};

struct Env {
    core: Arc<McpCore<FakeEditor>>,
    srv: HttpServer,
}

fn env() -> Env {
    let api = Arc::new(FakeEditor::new());
    api.add_folder("C:\\proj");
    api.add_doc(Some("C:\\proj\\a.rs"), "hello\n");
    let core = McpCore::new(api, McpConfig::default()).unwrap();
    let srv = HttpServer::start(core.clone(), HttpConfig { io_timeout: Duration::from_millis(600) }).unwrap();
    Env { core, srv }
}

struct Reply {
    status: u16,
    headers: Vec<(String, String)>,
    body: String,
}

impl Reply {
    fn header(&self, k: &str) -> Option<&str> {
        self.headers.iter().find(|(n, _)| n.eq_ignore_ascii_case(k)).map(|(_, v)| v.as_str())
    }
    fn json(&self) -> Value {
        serde_json::from_str(&self.body).unwrap_or_else(|_| panic!("not JSON: {:?}", self.body))
    }
}

fn send_raw(port: u16, raw: &[u8]) -> Reply {
    let mut s = TcpStream::connect(("127.0.0.1", port)).unwrap();
    s.set_read_timeout(Some(Duration::from_secs(3))).unwrap();
    s.write_all(raw).unwrap();
    let mut buf = Vec::new();
    let _ = s.read_to_end(&mut buf);
    let text = String::from_utf8_lossy(&buf).into_owned();
    let (head, body) = text.split_once("\r\n\r\n").unwrap_or((&text, ""));
    let mut lines = head.split("\r\n");
    let status = lines.next().and_then(|l| l.split(' ').nth(1)).and_then(|c| c.parse().ok()).unwrap_or(0);
    let headers = lines.filter_map(|l| l.split_once(": ").map(|(k, v)| (k.to_string(), v.to_string()))).collect();
    Reply { status, headers, body: body.to_string() }
}

struct Req<'a> {
    method: &'a str,
    path: &'a str,
    host: Option<String>,
    token: Option<String>,
    origin: Option<&'a str>,
    content_type: Option<&'a str>,
    session: Option<String>,
    body: String,
}

impl<'a> Req<'a> {
    fn ok(env: &Env, body: Value) -> Req<'a> {
        Req { method: "POST", path: "/mcp", host: Some(format!("127.0.0.1:{}", env.srv.port())), token: Some(env.srv.token().to_string()), origin: None, content_type: Some("application/json"), session: None, body: body.to_string() }
    }
    fn raw(&self) -> Vec<u8> {
        let mut s = format!("{} {} HTTP/1.1\r\n", self.method, self.path);
        if let Some(h) = &self.host {
            s += &format!("Host: {h}\r\n");
        }
        if let Some(t) = &self.token {
            s += &format!("Authorization: Bearer {t}\r\n");
        }
        if let Some(o) = self.origin {
            s += &format!("Origin: {o}\r\n");
        }
        if let Some(c) = self.content_type {
            s += &format!("Content-Type: {c}\r\n");
        }
        if let Some(id) = &self.session {
            s += &format!("Mcp-Session-Id: {id}\r\n");
        }
        s += &format!("Content-Length: {}\r\nConnection: close\r\n\r\n{}", self.body.len(), self.body);
        s.into_bytes()
    }
    fn send(&self, env: &Env) -> Reply {
        send_raw(env.srv.port(), &self.raw())
    }
}

fn rpc(id: u32, method: &str, params: Value) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params})
}

#[test]
fn binds_loopback_only_on_a_random_port() {
    let e = env();
    assert!(e.srv.addr().ip().is_loopback() && e.srv.addr().is_ipv4());
    assert_ne!(e.srv.port(), 0);
    assert!(e.srv.token().len() >= 32);
    let other = env();
    assert_ne!(e.srv.port(), other.srv.port(), "not a fixed port");
    assert_ne!(e.srv.token(), other.srv.token(), "token is per server");
    assert_eq!(e.srv.url(), format!("http://127.0.0.1:{}/mcp", e.srv.port()));
}

#[test]
fn a_valid_request_reaches_the_core() {
    let e = env();
    let r = Req::ok(&e, rpc(1, "tools/list", json!({}))).send(&e);
    assert_eq!(r.status, 200, "{}", r.body);
    assert_eq!(r.header("content-type"), Some("application/json"));
    assert_eq!(r.json()["result"]["tools"].as_array().unwrap().len(), 10);
    // A notification gets 202 and no body.
    let n = Req::ok(&e, json!({"jsonrpc": "2.0", "method": "notifications/initialized"})).send(&e);
    assert_eq!(n.status, 202);
    assert!(n.body.is_empty());
}

#[test]
fn missing_or_wrong_token_is_401_and_reveals_nothing() {
    let e = env();
    let mut none = Req::ok(&e, rpc(1, "tools/list", json!({})));
    none.token = None;
    let r = none.send(&e);
    assert_eq!(r.status, 401);
    assert_eq!(r.header("www-authenticate"), Some("Bearer"));
    assert!(!r.body.contains("tools"));
    for bad in ["wrong", "", &e.srv.token()[..e.srv.token().len() - 1], &format!("{}x", e.srv.token())] {
        let mut q = Req::ok(&e, rpc(1, "tools/list", json!({})));
        q.token = Some(bad.to_string());
        assert_eq!(q.send(&e).status, 401, "token {bad:?}");
    }
    // The token in the URL is not a thing.
    let mut q = Req::ok(&e, rpc(1, "tools/list", json!({})));
    q.token = None;
    q.path = "/mcp?token=abc";
    assert_ne!(q.send(&e).status, 200);
}

#[test]
fn hostile_origin_is_refused_even_with_the_right_token() {
    let e = env();
    for origin in ["http://evil.example", "https://claude.ai", "null", "http://127.0.0.1", &format!("http://127.0.0.1:{}", e.srv.port()), "http://localhost"] {
        let mut q = Req::ok(&e, rpc(1, "tools/list", json!({})));
        q.origin = Some(origin);
        let r = q.send(&e);
        assert_eq!(r.status, 403, "Origin {origin:?} must be refused");
        assert!(!r.body.contains("tools"));
    }
}

#[test]
fn dns_rebinding_host_header_is_refused() {
    let e = env();
    let port = e.srv.port();
    for host in [
        Some("evil.example".to_string()),
        Some(format!("evil.example:{port}")),
        Some(format!("127.0.0.1.evil.example:{port}")),
        Some(format!("localhost.evil.example:{port}")),
        Some("127.0.0.1".to_string()), // wrong: no port
        Some(format!("127.0.0.1:{}", port.wrapping_add(1))),
        Some(format!("[::1]:{port}")),
        Some(format!("0.0.0.0:{port}")),
        None,
    ] {
        let mut q = Req::ok(&e, rpc(1, "tools/list", json!({})));
        q.host = host.clone();
        assert_eq!(q.send(&e).status, 403, "Host {host:?} must be refused");
    }
    // The legitimate names work, in any case.
    for host in [format!("127.0.0.1:{port}"), format!("localhost:{port}"), format!("LocalHost:{port}")] {
        let mut q = Req::ok(&e, rpc(1, "tools/list", json!({})));
        q.host = Some(host.clone());
        assert_eq!(q.send(&e).status, 200, "Host {host:?}");
    }
}

#[test]
fn csrf_style_content_types_and_wrong_methods_never_reach_the_core() {
    let e = env();
    for ct in ["text/plain", "application/x-www-form-urlencoded", "multipart/form-data; boundary=x"] {
        let mut q = Req::ok(&e, rpc(1, "tools/list", json!({})));
        q.content_type = Some(ct);
        assert_eq!(q.send(&e).status, 415, "{ct}");
    }
    let mut q = Req::ok(&e, rpc(1, "tools/list", json!({})));
    q.content_type = None;
    assert_eq!(q.send(&e).status, 415);
    for m in ["GET", "PUT", "OPTIONS", "PATCH", "HEAD"] {
        let mut q = Req::ok(&e, json!({}));
        q.method = m;
        let r = q.send(&e);
        assert_eq!(r.status, 405, "{m}");
    }
    let mut q = Req::ok(&e, rpc(1, "tools/list", json!({})));
    q.path = "/other";
    assert_eq!(q.send(&e).status, 404);
}

#[test]
fn no_cors_header_is_ever_sent() {
    let e = env();
    let mut replies = vec![Req::ok(&e, rpc(1, "tools/list", json!({}))).send(&e)];
    let mut o = Req::ok(&e, json!({}));
    o.method = "OPTIONS";
    o.origin = Some("http://evil.example");
    replies.push(o.send(&e));
    let mut b = Req::ok(&e, rpc(1, "tools/list", json!({})));
    b.origin = Some("http://evil.example");
    replies.push(b.send(&e));
    let mut u = Req::ok(&e, rpc(1, "tools/list", json!({})));
    u.token = None;
    replies.push(u.send(&e));
    for r in replies {
        assert!(r.headers.iter().all(|(k, _)| !k.to_ascii_lowercase().starts_with("access-control")), "{:?}", r.headers);
    }
}

#[test]
fn framing_limits_are_enforced_before_the_body_is_read() {
    let e = env();
    // Oversized Content-Length: 413 without sending the body.
    let mut raw = format!("POST /mcp HTTP/1.1\r\nHost: 127.0.0.1:{}\r\nAuthorization: Bearer {}\r\nContent-Type: application/json\r\nContent-Length: 99999999\r\n\r\n", e.srv.port(), e.srv.token()).into_bytes();
    raw.extend_from_slice(b"{}");
    assert_eq!(send_raw(e.srv.port(), &raw).status, 413);
    // Chunked bodies are not accepted; a missing length is refused.
    let chunked = format!("POST /mcp HTTP/1.1\r\nHost: 127.0.0.1:{}\r\nAuthorization: Bearer {}\r\nContent-Type: application/json\r\nTransfer-Encoding: chunked\r\n\r\n0\r\n\r\n", e.srv.port(), e.srv.token());
    assert_eq!(send_raw(e.srv.port(), chunked.as_bytes()).status, 411);
    // Garbage and duplicate headers: 400.
    assert_eq!(send_raw(e.srv.port(), b"NOT HTTP\r\n\r\n").status, 400);
    let dup = format!("POST /mcp HTTP/1.1\r\nHost: 127.0.0.1:{0}\r\nHost: evil.example\r\nAuthorization: Bearer {1}\r\nContent-Type: application/json\r\nContent-Length: 2\r\n\r\n{{}}", e.srv.port(), e.srv.token());
    assert_eq!(send_raw(e.srv.port(), dup.as_bytes()).status, 400, "ambiguous Host is refused, not guessed");
    // A body that is not JSON-RPC is the core's -32700 / -32600, not a crash.
    let mut q = Req::ok(&e, json!({}));
    q.body = "{not json".into();
    assert_eq!(q.send(&e).json()["error"]["code"], -32700);
}

#[test]
fn a_silent_connection_is_dropped_by_the_timeout() {
    let e = env();
    let mut s = TcpStream::connect(("127.0.0.1", e.srv.port())).unwrap();
    s.set_read_timeout(Some(Duration::from_secs(3))).unwrap();
    let t0 = std::time::Instant::now();
    let mut buf = [0u8; 16];
    let n = s.read(&mut buf).unwrap_or(0);
    assert_eq!(n, 0, "the server closes a client that sends nothing");
    assert!(t0.elapsed() < Duration::from_secs(2));
    // And the server still works.
    assert_eq!(Req::ok(&e, rpc(1, "ping", json!({}))).send(&e).status, 200);
}

#[test]
fn consent_and_scopes_apply_exactly_as_on_the_pipe_and_names_are_namespaced() {
    let e = env();
    let init = Req::ok(&e, rpc(1, "initialize", json!({"protocolVersion": "2025-11-25", "clientInfo": {"name": "claude"}}))).send(&e);
    assert_eq!(init.status, 200);
    let sid = init.header("mcp-session-id").expect("a session id").to_string();
    let call = |e: &Env, sid: &str, name: &str, args: Value| {
        let mut q = Req::ok(e, rpc(2, "tools/call", json!({"name": name, "arguments": args})));
        q.session = Some(sid.to_string());
        q.send(e).json()["result"]["structuredContent"].clone()
    };
    // No consent yet.
    assert_eq!(call(&e, &sid, "editor_state", json!({}))["code"], "CONSENT_REQUIRED");
    // A grant given to the PIPE client of the same name does not carry over to HTTP.
    e.core.grant(Grant { client: "claude".into(), folder: "C:\\proj".into(), scope: Scope::S2, expires_at: None, exe_hash: None });
    assert_eq!(call(&e, &sid, "editor_state", json!({}))["code"], "CONSENT_REQUIRED", "http clients live in their own namespace");
    // The user allows the HTTP client by its own name.
    e.core.grant(Grant { client: "http:claude".into(), folder: "C:\\proj".into(), scope: Scope::S1, expires_at: None, exe_hash: None });
    assert_eq!(call(&e, &sid, "editor_state", json!({}))["docs"][0]["path"], "C:\\proj\\a.rs");
    // S1: no edits.
    let denied = call(&e, &sid, "edit", json!({"base_revision": 1, "edits": [{"match": {"old": "hello", "new": "x"}}]}));
    assert_eq!(denied["code"], "OUT_OF_SCOPE");
    // An anonymous (no session) caller is yet another client with no consent.
    let mut q = Req::ok(&e, rpc(3, "tools/call", json!({"name": "editor_state", "arguments": {}})));
    q.session = None;
    assert_eq!(q.send(&e).json()["result"]["structuredContent"]["code"], "CONSENT_REQUIRED");
}

#[test]
fn the_kill_switch_stops_http_at_once_and_stop_closes_the_port() {
    let e = env();
    assert_eq!(Req::ok(&e, rpc(1, "ping", json!({}))).send(&e).status, 200);
    e.core.kill();
    assert_eq!(Req::ok(&e, rpc(2, "ping", json!({}))).send(&e).status, 503);
    let port = e.srv.port();
    e.srv.stop();
    // The accept loop notices within a few milliseconds and the listener is dropped.
    let mut closed = false;
    for _ in 0..50 {
        std::thread::sleep(Duration::from_millis(30));
        if TcpStream::connect(("127.0.0.1", port)).is_err() {
            closed = true;
            break;
        }
    }
    assert!(closed, "the port must stop accepting after stop()");
}

#[test]
fn delete_ends_a_session() {
    let e = env();
    let init = Req::ok(&e, rpc(1, "initialize", json!({"protocolVersion": "2025-11-25", "clientInfo": {"name": "c"}}))).send(&e);
    let sid = init.header("mcp-session-id").unwrap().to_string();
    e.core.grant(Grant { client: "http:c".into(), folder: "C:\\proj".into(), scope: Scope::S1, expires_at: None, exe_hash: None });
    let state = |sid: &str| {
        let mut q = Req::ok(&e, rpc(2, "tools/call", json!({"name": "editor_state", "arguments": {}})));
        q.session = Some(sid.to_string());
        q.send(&e).json()["result"]["structuredContent"]["code"].clone()
    };
    assert_eq!(state(&sid), Value::Null, "allowed");
    let mut d = Req::ok(&e, json!({}));
    d.method = "DELETE";
    d.session = Some(sid.clone());
    assert_eq!(d.send(&e).status, 204);
    assert_eq!(state(&sid), "CONSENT_REQUIRED", "the session is gone, the caller is anonymous again");
}
