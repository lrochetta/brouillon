//! IPC protocol, relay and host: loopback fake plus one real local-socket round trip.

use std::io::{BufRead, BufReader, Cursor, Read, Write};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;

use brouillon_mcp::core::{McpConfig, McpCore};
use brouillon_mcp::fake::FakeEditor;
use brouillon_mcp::link::*;
use brouillon_mcp::policy::{Grant, Scope};
use brouillon_mcp::relay::{self, loopback};
use brouillon_mcp::settings::McpSettings;
use serde_json::{json, Value};

const TOKEN: &str = "0123456789012345678901234567890123456789abcdefgh";

fn core() -> Arc<McpCore<FakeEditor>> {
    let api = Arc::new(FakeEditor::new());
    api.add_folder("C:\\proj");
    api.add_doc(Some("C:\\proj\\a.rs"), "hello\n");
    let c = McpCore::new(api, McpConfig::default()).unwrap();
    c.grant(Grant { client: "claude".into(), folder: "C:\\proj".into(), scope: Scope::S2, expires_at: None, exe_hash: None });
    c
}

fn rpc(id: u32, method: &str, params: Value) -> String {
    json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}).to_string()
}

/// Wire a relay to a serving core over in-memory pipes. Returns (exit code, stdout text).
fn run_loopback(core: Arc<McpCore<FakeEditor>>, relay_token: &str, stdin: String, client: &str) -> (i32, String) {
    let (c2s_r, c2s_w) = loopback::pipe(); // relay -> editor
    let (s2c_r, s2c_w) = loopback::pipe(); // editor -> relay
    let server_token = TOKEN.to_string();
    let server = std::thread::spawn(move || {
        let _ = serve_connection(&*core, &server_token, c2s_r, s2c_w);
    });
    let sink = loopback::Sink::default();
    let hello = ClientHello { name: client.into(), parent_exe: None, pid: Some(1) };
    let code = relay::run(Cursor::new(stdin.into_bytes()), sink.clone(), s2c_r, c2s_w, relay_token, &hello);
    server.join().unwrap();
    (code, sink.text())
}

fn lines(s: &str) -> Vec<Value> {
    s.lines().map(|l| serde_json::from_str(l).unwrap_or_else(|_| panic!("stdout must be JSON only, got {l:?}"))).collect()
}

#[test]
fn relay_round_trip_over_loopback() {
    let input = [
        rpc(1, "initialize", json!({"protocolVersion": "2025-11-25", "clientInfo": {"name": "ignored"}})),
        json!({"jsonrpc": "2.0", "method": "notifications/initialized"}).to_string(),
        rpc(2, "tools/list", json!({})),
        rpc(3, "tools/call", json!({"name": "read", "arguments": {}})),
    ]
    .join("\n")
        + "\n";
    let (code, out) = run_loopback(core(), TOKEN, input, "claude");
    assert_eq!(code, 0);
    let l = lines(&out);
    assert_eq!(l.len(), 3, "the notification is silent");
    assert_eq!(l[0]["result"]["serverInfo"]["name"], "brouillon");
    assert_eq!(l[1]["result"]["tools"].as_array().unwrap().len(), 10);
    assert!(l[2]["result"]["structuredContent"]["content"].as_str().unwrap().contains("hello"));
}

#[test]
fn identity_comes_from_the_handshake_not_from_the_agent() {
    // The relay announced "claude" (granted). The agent claims to be someone else in initialize: ignored.
    let input = rpc(1, "initialize", json!({"clientInfo": {"name": "evil"}})) + "\n" + &rpc(2, "tools/call", json!({"name": "editor_state"})) + "\n";
    let (_, out) = run_loopback(core(), TOKEN, input, "claude");
    assert_eq!(lines(&out)[1]["result"]["isError"], false);
    // An unannounced client falls back to clientInfo, which has no grant: refused.
    let input = rpc(1, "initialize", json!({"clientInfo": {"name": "stranger"}})) + "\n" + &rpc(2, "tools/call", json!({"name": "editor_state"})) + "\n";
    let (_, out) = run_loopback(core(), TOKEN, input, "");
    assert_eq!(lines(&out)[1]["result"]["structuredContent"]["code"], "CONSENT_REQUIRED");
}

#[test]
fn wrong_token_is_refused_with_a_clean_json_rpc_error() {
    let (code, out) = run_loopback(core(), "wrong-token", rpc(7, "ping", json!({})) + "\n", "claude");
    assert_eq!(code, 1);
    let l = lines(&out);
    assert_eq!(l.len(), 1);
    assert_eq!(l[0]["id"], 7);
    assert_eq!(l[0]["error"]["code"], -32001);
    assert!(l[0]["error"]["message"].as_str().unwrap().contains("token"));
}

#[test]
fn killed_editor_refuses_new_connections() {
    let c = core();
    c.kill();
    let (code, out) = run_loopback(c, TOKEN, rpc(1, "ping", json!({})) + "\n", "claude");
    assert_eq!(code, 1);
    assert!(lines(&out)[0]["error"]["message"].as_str().unwrap().contains("stopped"));
}

#[test]
fn handshake_rejects_garbage_wrong_magic_and_wrong_version() {
    let c = core();
    let cases = [
        ("not json".to_string(), "invalid"),
        (json!({"link": "other", "v": 1, "token": TOKEN, "client": {"name": "x"}}).to_string(), "invalid"),
        (json!({"link": "brouillon", "v": 99, "token": TOKEN, "client": {"name": "x"}}).to_string(), "version"),
        (json!({"link": "brouillon", "v": 1, "token": "nope", "client": {"name": "x"}}).to_string(), "bad_token"),
    ];
    for (line, want) in cases {
        let mut out = Vec::new();
        let r = accept_handshake(&*c, TOKEN, &mut Cursor::new(format!("{line}\n").into_bytes()), &mut out).unwrap();
        let reply = r.err().expect("must be refused");
        assert_eq!(reply.error.as_deref(), Some(want));
        let wire: HelloReply = serde_json::from_slice(&out).unwrap();
        assert!(!wire.ok);
    }
    // An oversize hello is refused without being buffered.
    let big = "x".repeat(2 * 1024 * 1024) + "\n";
    let mut out = Vec::new();
    assert!(accept_handshake(&*c, TOKEN, &mut Cursor::new(big.into_bytes()), &mut out).unwrap().is_err());
}

#[test]
fn handshake_success_announces_tool_set_hash_and_sanitises_the_name() {
    let c = core();
    let hello = json!({"link": "brouillon", "v": 1, "token": TOKEN, "client": {"name": "cl\u{0007}aude"}}).to_string();
    let mut out = Vec::new();
    let s = accept_handshake(&*c, TOKEN, &mut Cursor::new(format!("{hello}\n").into_bytes()), &mut out).unwrap().unwrap();
    assert_eq!(s.client, "claude");
    let reply: HelloReply = serde_json::from_slice(&out).unwrap();
    assert!(reply.ok);
    assert_eq!(reply.tool_set_hash.unwrap(), brouillon_mcp::tools::tool_set_hash());
}

#[test]
fn relay_never_prints_non_json_even_for_hostile_input() {
    let input = "garbage line\n{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"ping\"}\n\n[1,2]\n".to_string();
    let (code, out) = run_loopback(core(), TOKEN, input, "claude");
    assert_eq!(code, 0);
    let l = lines(&out);
    assert_eq!(l.len(), 3);
    assert_eq!(l[0]["error"]["code"], -32700);
    assert_eq!(l[1]["id"], 1);
    assert_eq!(l[2]["error"]["code"], -32600);
}

#[test]
fn relay_survives_frames_split_across_writes() {
    // Relay reads lines, so a request split across arbitrary read boundaries still arrives whole.
    struct Drip(Vec<u8>, usize);
    impl Read for Drip {
        fn read(&mut self, b: &mut [u8]) -> std::io::Result<usize> {
            if self.1 >= self.0.len() {
                return Ok(0);
            }
            b[0] = self.0[self.1];
            self.1 += 1;
            Ok(1)
        }
    }
    let (c2s_r, c2s_w) = loopback::pipe();
    let (s2c_r, s2c_w) = loopback::pipe();
    let c = core();
    let server = std::thread::spawn(move || {
        let _ = serve_connection(&*c, TOKEN, c2s_r, s2c_w);
    });
    let sink = loopback::Sink::default();
    let input = BufReader::with_capacity(1, Drip((rpc(5, "ping", json!({})) + "\n").into_bytes(), 0));
    let code = relay::run(input, sink.clone(), s2c_r, c2s_w, TOKEN, &ClientHello { name: "claude".into(), ..Default::default() });
    server.join().unwrap();
    assert_eq!(code, 0);
    assert_eq!(lines(&sink.text())[0]["id"], 5);
}

#[test]
fn unreachable_editor_prints_error_for_the_first_request_id() {
    let mut out = Vec::new();
    let code = relay::fail_with_error(Cursor::new((rpc(42, "initialize", json!({})) + "\n").into_bytes()), &mut out, "Brouillon is not running.");
    assert_eq!(code, 1);
    let v: Value = serde_json::from_slice(&out).unwrap();
    assert_eq!((v["id"].clone(), v["error"]["code"].clone()), (json!(42), json!(-32001)));
    // No request at all: id null, still one clean error line.
    let mut out = Vec::new();
    relay::fail_with_error(Cursor::new(Vec::new()), &mut out, "x");
    assert_eq!(serde_json::from_slice::<Value>(&out).unwrap()["id"], Value::Null);
}

#[test]
fn instance_selection_prefers_flag_then_cwd_then_recent() {
    let rec = |pid: u32, at: u64, root: &str| InstanceRecord { http_port: None, http_token: None, pid, started_at: at, pipe: format!("p{pid}"), protocol_versions: vec![], workspace_roots: vec![root.into()], tool_set_hash: String::new() };
    let all = vec![rec(1, 10, "C:\\a"), rec(2, 30, "C:\\b"), rec(3, 20, "C:\\c")];
    assert_eq!(select_instances(all.clone(), Some(3), Some("C:\\a")).iter().map(|r| r.pid).collect::<Vec<_>>(), [3]);
    assert_eq!(select_instances(all.clone(), None, Some("C:\\a\\src")).iter().map(|r| r.pid).collect::<Vec<_>>(), [1, 2, 3]);
    assert_eq!(select_instances(all.clone(), None, Some("C:\\ab")).iter().map(|r| r.pid).collect::<Vec<_>>(), [2, 3, 1], "ab is not inside a");
    assert_eq!(select_instances(all, None, None).iter().map(|r| r.pid).collect::<Vec<_>>(), [2, 3, 1]);
}

#[test]
fn token_is_stable_long_and_compared_in_constant_time_helper() {
    let d = tempfile::tempdir().unwrap();
    let a = load_or_create_token(d.path()).unwrap();
    assert!(a.len() >= 32);
    assert_eq!(a, load_or_create_token(d.path()).unwrap());
    assert!(ct_eq(b"abc", b"abc") && !ct_eq(b"abc", b"abd") && !ct_eq(b"abc", b"ab"));
}

// ---------------------------------------------------------------- host: off by default, real socket when on

static N: AtomicU32 = AtomicU32::new(0);

fn tag() -> String {
    format!("t{}-{}", std::process::id(), N.fetch_add(1, Ordering::Relaxed))
}

#[test]
fn disabled_host_listens_on_nothing_and_writes_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let dirs = LinkDirs { dir: dir.path().join("link"), pipe_tag: tag() };
    let host = LinkHost::start(&McpSettings::default(), core(), &dirs, vec![]).unwrap();
    assert!(host.is_none());
    assert!(!dirs.dir.exists(), "no token, no instance file");
    assert!(connect(&pipe_name(&dirs.pipe_tag)).is_err(), "nothing is listening");
}

#[test]
fn enabled_host_serves_the_real_relay_over_a_local_socket_and_cleans_up() {
    let dir = tempfile::tempdir().unwrap();
    let dirs = LinkDirs { dir: dir.path().join("link"), pipe_tag: tag() };
    let settings = McpSettings::from_toml("[mcp]\nenabled = true\n");
    let host = LinkHost::start(&settings, core(), &dirs, vec!["C:\\proj".into()]).unwrap().expect("enabled");
    let recs = read_instances(&dirs.dir);
    assert_eq!(recs.len(), 1);
    assert_eq!(recs[0].pipe, host.pipe);
    assert_eq!(recs[0].tool_set_hash, brouillon_mcp::tools::tool_set_hash());
    let token = load_or_create_token(&dirs.dir).unwrap();

    let conn = connect(&recs[0].pipe).unwrap();
    let (r, w) = Shared::pair(conn);
    let sink = loopback::Sink::default();
    let input = rpc(1, "tools/call", json!({"name": "editor_state"})) + "\n";
    let code = relay::run(Cursor::new(input.into_bytes()), sink.clone(), r, w, &token, &ClientHello { name: "claude".into(), ..Default::default() });
    assert_eq!(code, 0);
    let l = lines(&sink.text());
    assert_eq!(l[0]["result"]["isError"], false);
    assert_eq!(l[0]["result"]["structuredContent"]["docs"][0]["path"], "C:\\proj\\a.rs");

    // A wrong token on the real socket is rejected at the handshake.
    let conn = connect(&recs[0].pipe).unwrap();
    let (r, mut w) = Shared::pair(conn);
    let mut br = BufReader::new(r);
    let reply = client_handshake(&mut br, &mut w, "nope", &ClientHello::default()).unwrap();
    assert_eq!(reply.error.as_deref(), Some("bad_token"));
    let mut rest = String::new();
    assert_eq!(br.read_line(&mut rest).unwrap(), 0, "editor closed the connection");
    let _ = w.flush();

    host.stop();
    assert!(read_instances(&dirs.dir).is_empty(), "instance file removed on stop");
}

#[test]
fn self_test_counts_tools_without_needing_consent_and_reports_failures() {
    let dir = tempfile::tempdir().unwrap();
    let dirs = LinkDirs { dir: dir.path().join("link"), pipe_tag: tag() };
    let settings = McpSettings::from_toml("[mcp]\nenabled = true\n");
    let host = LinkHost::start(&settings, core(), &dirs, vec![]).unwrap().expect("enabled");
    let token = load_or_create_token(&dirs.dir).unwrap();
    let n = self_test(&host.pipe, &token, std::time::Duration::from_secs(5)).unwrap();
    assert_eq!(n, 10, "the ten core tools");
    let bad = self_test(&host.pipe, "wrong-token", std::time::Duration::from_secs(5)).unwrap_err();
    assert!(bad.contains("bad_token"), "{bad}");
    let pipe = host.pipe.clone();
    host.stop();
    assert!(self_test(&pipe, &token, std::time::Duration::from_secs(2)).is_err(), "nothing listens after stop");
}
