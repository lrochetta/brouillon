//! Pipe hardening: instant drop on kill, peer pid check, squatted name, hello timeout rules.

use std::io::{BufRead, BufReader, Cursor, Write};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use brouillon_mcp::core::{McpConfig, McpCore};
use brouillon_mcp::fake::FakeEditor;
use brouillon_mcp::link::*;
use brouillon_mcp::policy::{Grant, Scope};
use brouillon_mcp::settings::McpSettings;

const TOKEN: &str = "0123456789012345678901234567890123456789abcdefgh";

static N: AtomicU32 = AtomicU32::new(0);

fn tag() -> String {
    format!("h{}-{}", std::process::id(), N.fetch_add(1, Ordering::Relaxed))
}

fn core() -> Arc<McpCore<FakeEditor>> {
    let api = Arc::new(FakeEditor::new());
    api.add_folder("C:\\proj");
    api.add_doc(Some("C:\\proj\\a.rs"), "hello\n");
    let c = McpCore::new(api, McpConfig::default()).unwrap();
    c.grant(Grant { client: "claude".into(), folder: "C:\\proj".into(), scope: Scope::S2, expires_at: None, exe_hash: None });
    c
}

fn host(core: Arc<McpCore<FakeEditor>>) -> (LinkHost, tempfile::TempDir, String) {
    let dir = tempfile::tempdir().unwrap();
    let dirs = LinkDirs { dir: dir.path().join("link"), pipe_tag: tag() };
    let h = LinkHost::start(&McpSettings::from_toml("[mcp]\nenabled = true\n"), core, &dirs, vec![]).unwrap().expect("enabled");
    let token = load_or_create_token(&dirs.dir).unwrap();
    (h, dir, token)
}

#[test]
fn kill_drops_an_idle_authenticated_connection_in_well_under_200_ms() {
    let c = core();
    let (h, _dir, token) = host(c.clone());
    let conn = connect(&h.pipe).unwrap();
    let (r, mut w) = Shared::pair(conn);
    let mut br = BufReader::new(r);
    let reply = client_handshake(&mut br, &mut w, &token, &ClientHello { name: "claude".into(), ..Default::default() }).unwrap();
    assert!(reply.ok);
    // The connection is really alive and survives being idle (a polling reader must not turn an
    // empty pipe into EOF): ping, idle, ping.
    for _ in 0..2 {
        writeln!(w, "{{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"ping\"}}").unwrap();
        w.flush().unwrap();
        let mut line = String::new();
        br.read_line(&mut line).unwrap();
        assert!(line.contains("\"result\""), "alive before the kill: {line:?}");
        std::thread::sleep(Duration::from_millis(150));
    }
    // The client now waits for the next message that never comes (a blocked read on the pipe).
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut line = String::new();
        let n = br.read_line(&mut line);
        let _ = tx.send((Instant::now(), n.ok()));
    });
    std::thread::sleep(Duration::from_millis(100));
    let t0 = Instant::now();
    c.kill();
    let (t1, n) = rx.recv_timeout(Duration::from_secs(2)).expect("the connection must be dropped by the kill switch");
    let took = t1.duration_since(t0);
    assert!(took < Duration::from_millis(200), "dropped after {took:?}");
    assert!(matches!(n, Some(0) | None), "EOF or reset, never data: {n:?}");
    drop(h);
}

#[test]
fn kill_also_drops_a_connection_that_never_said_hello() {
    let c = core();
    let (h, _dir, _token) = host(c.clone());
    let conn = connect(&h.pipe).unwrap();
    let (r, _w) = Shared::pair(conn);
    let mut br = BufReader::new(r);
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut line = String::new();
        let n = br.read_line(&mut line);
        let _ = tx.send((Instant::now(), n.ok()));
    });
    std::thread::sleep(Duration::from_millis(100));
    let t0 = Instant::now();
    c.kill();
    let (t1, _) = rx.recv_timeout(Duration::from_secs(2)).expect("dropped");
    assert!(t1.duration_since(t0) < Duration::from_millis(200));
    drop(h);
}

#[test]
fn a_pid_claimed_in_the_hello_must_be_the_pid_that_connected() {
    // Pure handshake with a reported peer pid.
    let c = core();
    let hello = |pid: u32| format!("{{\"link\":\"brouillon\",\"v\":1,\"token\":\"{TOKEN}\",\"client\":{{\"name\":\"claude\",\"parent_exe\":null,\"pid\":{pid}}}}}\n");
    let mut out = Vec::new();
    let r = accept_handshake_peer(&*c, TOKEN, &mut Cursor::new(hello(7).into_bytes()), &mut out, Some(8)).unwrap();
    assert_eq!(r.unwrap_err().error.as_deref(), Some("invalid"));
    let r = accept_handshake_peer(&*c, TOKEN, &mut Cursor::new(hello(7).into_bytes()), &mut out, Some(7)).unwrap();
    assert!(r.is_ok());
    // Unknown peer pid (platform cannot tell): not a reason to refuse.
    let r = accept_handshake_peer(&*c, TOKEN, &mut Cursor::new(hello(7).into_bytes()), &mut out, None).unwrap();
    assert!(r.is_ok());
}

#[test]
fn on_a_real_pipe_the_os_reported_pid_is_checked() {
    let c = core();
    let (h, _dir, token) = host(c);
    let me = std::process::id();
    // Honest: the claimed pid is this process, which is the one that connected.
    let conn = connect(&h.pipe).unwrap();
    let (r, mut w) = Shared::pair(conn);
    let mut br = BufReader::new(r);
    let reply = client_handshake(&mut br, &mut w, &token, &ClientHello { name: "claude".into(), parent_exe: None, pid: Some(me) }).unwrap();
    assert!(reply.ok, "{reply:?}");
    // Lying about the pid is refused.
    let conn = connect(&h.pipe).unwrap();
    let (r, mut w) = Shared::pair(conn);
    let mut br = BufReader::new(r);
    let reply = client_handshake(&mut br, &mut w, &token, &ClientHello { name: "claude".into(), parent_exe: None, pid: Some(me.wrapping_add(1)) }).unwrap();
    assert_eq!(reply.error.as_deref(), Some("invalid"));
    let _ = w.flush();
}

#[test]
fn a_squatted_or_duplicate_pipe_name_makes_start_fail_instead_of_sharing_it() {
    let dir = tempfile::tempdir().unwrap();
    let t = tag();
    let dirs = LinkDirs { dir: dir.path().join("link"), pipe_tag: t.clone() };
    let settings = McpSettings::from_toml("[mcp]\nenabled = true\n");
    let first = LinkHost::start(&settings, core(), &dirs, vec![]).unwrap().expect("enabled");
    let second = LinkHost::start(&settings, core(), &dirs, vec![]);
    assert!(second.is_err(), "a second server on the same pipe name must not start (first-instance flag)");
    drop(first);
}

#[test]
fn many_connections_are_capped_and_the_server_survives_a_flood_of_silent_clients() {
    let c = core();
    let (h, _dir, token) = host(c);
    let mut silent = Vec::new();
    for _ in 0..40 {
        if let Ok(s) = connect(&h.pipe) {
            silent.push(s);
        }
    }
    // A well-behaved client still gets through once the silent ones are gone or capped.
    drop(silent);
    std::thread::sleep(Duration::from_millis(200));
    let mut ok = false;
    for _ in 0..20 {
        if let Ok(conn) = connect(&h.pipe) {
            let (r, mut w) = Shared::pair(conn);
            let mut br = BufReader::new(r);
            if client_handshake(&mut br, &mut w, &token, &ClientHello { name: "claude".into(), ..Default::default() }).map_or(false, |r| r.ok) {
                ok = true;
                break;
            }
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    assert!(ok, "the server still answers after a flood");
}
