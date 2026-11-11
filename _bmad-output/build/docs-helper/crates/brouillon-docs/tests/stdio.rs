//! End-to-end tests against the real `brouillon-docs` executable over stdio.

use brouillon_docs_proto::{read_frame, write_control, Frame};
use serde_json::{json, Value};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};

struct Helper {
    child: Child,
    stdin: ChildStdin,
    stdout: ChildStdout,
}

impl Helper {
    fn spawn() -> Self {
        let mut child = Command::new(env!("CARGO_BIN_EXE_brouillon-docs"))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let stdin = child.stdin.take().unwrap();
        let stdout = child.stdout.take().unwrap();
        Helper { child, stdin, stdout }
    }
    fn send(&mut self, id: u64, method: &str, params: Value) {
        write_control(&mut self.stdin, &json!({"id": id, "method": method, "params": params})).unwrap();
    }
    fn recv(&mut self) -> Option<Value> {
        match read_frame(&mut self.stdout).unwrap()? {
            Frame::Control(b) => Some(serde_json::from_slice(&b).unwrap()),
            Frame::Data { .. } => panic!("unexpected data frame"),
        }
    }
    /// Next reply (skipping events) for `id`; returns the events seen on the way.
    fn reply(&mut self, id: u64) -> (Value, Vec<Value>) {
        let mut events = vec![];
        loop {
            let m = self.recv().expect("helper closed the stream");
            if m.get("event").is_some() {
                events.push(m);
            } else {
                assert_eq!(m["id"], id);
                return (m, events);
            }
        }
    }
    fn hello(&mut self) -> Value {
        self.send(1, "hello", json!({"proto": 1, "client": "test"}));
        self.reply(1).0
    }
}

impl Drop for Helper {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[test]
fn hello_then_registry_and_unsupported() {
    let mut h = Helper::spawn();
    let r = h.hello();
    assert_eq!(r["result"]["proto"], 1);
    assert_eq!(r["result"]["limits"]["max_frame"], 1 << 20);

    h.send(2, "operations", json!({}));
    let (r, _) = h.reply(2);
    let ops = r["result"].as_array().unwrap();
    assert!(ops.iter().any(|o| o["name"] == "apply_ops" && o["destructive"] == true));

    h.send(3, "open", json!({"doc_id": "d", "size": 0}));
    assert_eq!(h.reply(3).0["error"]["code"], "unsupported");
    h.send(4, "nope", json!({}));
    assert_eq!(h.reply(4).0["error"]["code"], "unknown_method");

    h.send(5, "shutdown", json!({}));
    assert!(h.reply(5).0.get("result").is_some());
    assert!(h.child.wait().unwrap().success());
}

#[test]
fn first_request_must_be_hello_and_version_checked() {
    let mut h = Helper::spawn();
    h.send(1, "ping", json!({}));
    assert_eq!(h.reply(1).0["error"]["code"], "protocol");
    assert_eq!(h.child.wait().unwrap().code(), Some(2));

    let mut h = Helper::spawn();
    h.send(1, "hello", json!({"proto": 99}));
    assert_eq!(h.reply(1).0["error"]["code"], "bad_request");
}

#[test]
fn bad_frame_is_fatal() {
    use std::io::Write;
    let mut h = Helper::spawn();
    h.stdin.write_all(&u32::MAX.to_le_bytes()).unwrap();
    h.stdin.flush().unwrap();
    assert_eq!(h.reply(0).0["error"]["code"], "protocol");
    assert_eq!(h.child.wait().unwrap().code(), Some(2));
}

#[test]
fn progress_events_then_result() {
    let mut h = Helper::spawn();
    h.hello();
    h.send(2, "debug.spin", json!({"steps": 3, "step_ms": 1}));
    let (r, events) = h.reply(2);
    assert_eq!(r["result"]["steps"], 3);
    assert_eq!(events.len(), 3);
    assert_eq!(events[2]["done"], 3);
}

#[test]
fn cancel_stops_a_running_request() {
    let mut h = Helper::spawn();
    h.hello();
    h.send(2, "debug.spin", json!({"steps": 100000, "step_ms": 5}));
    // wait for the first progress event so the request is certainly running
    assert_eq!(h.recv().unwrap()["event"], "progress");
    h.send(3, "cancel", json!({"id": 2}));
    let mut cancelled = false;
    let mut cancel_ack = false;
    while !(cancelled && cancel_ack) {
        let m = h.recv().expect("stream ended");
        if m.get("event").is_some() {
            continue;
        }
        match m["id"].as_u64().unwrap() {
            2 => {
                assert_eq!(m["error"]["code"], "cancelled");
                cancelled = true;
            }
            3 => cancel_ack = true,
            other => panic!("unexpected id {other}"),
        }
    }
}

/// G-nonet (runtime half): while the helper is alive and working, the OS reports no TCP/UDP endpoint
/// owned by its process.
#[cfg(windows)]
#[test]
fn helper_owns_no_sockets() {
    let mut h = Helper::spawn();
    h.hello();
    h.send(2, "debug.spin", json!({"steps": 100000, "step_ms": 5}));
    assert_eq!(h.recv().unwrap()["event"], "progress");
    let pid = h.child.id().to_string();
    let out = Command::new("netstat").arg("-ano").output().expect("netstat");
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(text.contains("Proto"), "netstat output not understood");
    let owned: Vec<&str> = text.lines().filter(|l| l.split_whitespace().last() == Some(pid.as_str())).collect();
    assert!(owned.is_empty(), "helper owns sockets: {owned:?}");
}

fn rs_files(dir: &std::path::Path, out: &mut Vec<std::path::PathBuf>) {
    for e in std::fs::read_dir(dir).unwrap() {
        let p = e.unwrap().path();
        if p.is_dir() {
            rs_files(&p, out);
        } else if p.extension().is_some_and(|x| x == "rs") {
            out.push(p);
        }
    }
}

/// G-nonet (static half): no network crate in the dependency tree, no socket API in the sources.
#[test]
fn no_network_crates_or_socket_apis() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("..");
    let lock = std::fs::read_to_string(root.join("Cargo.lock")).unwrap();
    let banned = ["tokio", "mio", "socket2", "reqwest", "ureq", "hyper", "rustls", "ring", "native-tls", "openssl", "h2", "curl", "isahc", "winapi", "windows-sys", "windows"];
    for line in lock.lines() {
        if let Some(name) = line.strip_prefix("name = \"") {
            let name = name.trim_end_matches('"');
            assert!(!banned.contains(&name), "banned crate in the dependency tree: {name}");
        }
    }
    let needles = [["std::", "net"].concat(), "TcpStream".into(), "TcpListener".into(), "UdpSocket".into(), "ToSocketAddrs".into()];
    let mut files = vec![];
    rs_files(&root.join("crates"), &mut files);
    for f in files.iter().filter(|f| !f.components().any(|c| c.as_os_str() == "tests")) {
        let src = std::fs::read_to_string(f).unwrap();
        for n in &needles {
            assert!(!src.contains(n.as_str()), "{} uses {n}", f.display());
        }
    }
}
