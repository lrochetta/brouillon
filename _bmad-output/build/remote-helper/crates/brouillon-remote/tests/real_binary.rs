//! Tests of the real `brouillon-remote` executable (no ssh server, no network).

use brouillon_remote::askpass::{AskpassServer, ENV_ADDR, ENV_TOKEN};
use brouillon_remote::transport::Prompter;
use brouillon_remote_proto::*;
use std::io::Write;
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::time::{Duration, Instant};

const EXE: &str = env!("CARGO_BIN_EXE_brouillon-remote");

#[test]
fn version_flag() {
    let o = Command::new(EXE).arg("--version").output().unwrap();
    let s = String::from_utf8_lossy(&o.stdout);
    assert!(o.status.success() && s.contains("brouillon-remote") && s.contains("protocol 1"), "{s}");
}

fn read_msg(r: &mut impl std::io::Read) -> ServerMessage {
    match read_frame(r).unwrap().unwrap() {
        Frame::Control(b) => ServerMessage::from_slice(&b).unwrap(),
        f => panic!("unexpected {f:?}"),
    }
}

#[test]
fn serves_hello_reports_missing_ssh_and_exits_when_stdin_closes() {
    let mut child = Command::new(EXE).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::null()).spawn().unwrap();
    let mut stdin = child.stdin.take().unwrap();
    let mut stdout = child.stdout.take().unwrap();
    let send = |w: &mut std::process::ChildStdin, id, request| write_control(w, &RequestFrame { id, request }).unwrap();
    send(&mut stdin, 1, Request::Hello { proto: 1, client: "t".into() });
    match read_msg(&mut stdout) {
        ServerMessage::Result { id: 1, result } => assert_eq!(result["proto"], 1),
        m => panic!("{m:?}"),
    }
    send(
        &mut stdin,
        2,
        Request::Connect(ConnectParams {
            host: "example.invalid".into(),
            port: None,
            user: None,
            identity_file: None,
            proxy_jump: None,
            host_key_policy: HostKeyPolicy::Strict,
            known_hosts_file: None,
            ssh_config_file: None,
            interactive: false,
            connect_timeout_s: None,
            ssh_path: Some("Z:/definitely/not/here/ssh.exe".into()),
            root: None,
            protocol: Protocol::Sftp,
            ftp: None,
        }),
    );
    match read_msg(&mut stdout) {
        ServerMessage::Error { id: Some(2), error } => assert_eq!(error.kind, ErrorKind::NoSsh),
        m => panic!("{m:?}"),
    }
    stdin.flush().unwrap();
    drop(stdin); // the editor went away
    let t = Instant::now();
    loop {
        if let Some(st) = child.try_wait().unwrap() {
            assert!(st.success());
            break;
        }
        assert!(t.elapsed() < Duration::from_secs(5), "helper must exit when stdin closes");
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[test]
fn garbage_on_stdin_ends_the_helper_with_a_protocol_error() {
    let mut child = Command::new(EXE).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::null()).spawn().unwrap();
    let mut stdin = child.stdin.take().unwrap();
    let mut stdout = child.stdout.take().unwrap();
    stdin.write_all(b"GET / HTTP/1.1\r\n\r\n").unwrap();
    stdin.flush().unwrap();
    match read_msg(&mut stdout) {
        ServerMessage::Error { id: None, error } => assert_eq!(error.kind, ErrorKind::Protocol),
        m => panic!("{m:?}"),
    }
    assert!(child.wait().unwrap().success());
}

struct Fixed(&'static str);
impl Prompter for Fixed {
    fn ask(&self, _: PromptKind, _: &str) -> Option<Secret> {
        Some(Secret::new(self.0))
    }
}

/// Passes when run (verified once); ignored by default because a test executable that spawns a
/// process which connects back over loopback is deleted by the security product on the dev machine
/// (the file vanishes after the run and the next relink fails with LNK1104).
#[test]
#[ignore = "loopback client/server across processes; see doc comment"]
fn askpass_mode_prints_the_answer_and_nothing_else() {
    let srv = AskpassServer::start(Arc::new(Fixed("correct horse"))).unwrap();
    let o = Command::new(EXE)
        .arg("Enter passphrase for key 'k': ")
        .env(ENV_ADDR, srv.addr())
        .env(ENV_TOKEN, srv.token())
        .output()
        .unwrap();
    assert!(o.status.success());
    assert_eq!(String::from_utf8_lossy(&o.stdout), "correct horse\n");
}

#[test]
fn a_stray_argument_is_rejected_and_prints_nothing_on_stdout() {
    // (no socket involved: security products on the dev machine delete test executables that do many loopback cycles)
    let o = Command::new(EXE).arg("password: ").env_remove(ENV_ADDR).env(ENV_TOKEN, "x").output().unwrap();
    assert_eq!(o.status.code(), Some(2));
    assert!(o.stdout.is_empty(), "stdout is reserved for protocol frames");
}
