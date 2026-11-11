//! Integration tests against the machine's real OpenSSH client. All `#[ignore]`: run with
//! `cargo test -- --ignored`. They never contact an external host: `ssh -G` only prints the resolved
//! configuration, and the single connection attempt goes to 127.0.0.1 port 1 (closed).
//!
//! NOT covered (needs a real sshd, none is installed): the SFTP session through a real ssh, askpass
//! invocation by ssh.exe, host-key prompts, agent / key auth.

use brouillon_remote::transport::{build_args, find_ssh, ArgsMode, Connector, Prompter, SshConnector};
use brouillon_remote_proto::*;
use std::process::Command;
use std::sync::Arc;

fn params(host: &str) -> ConnectParams {
    ConnectParams {
        host: host.into(),
        port: None,
        user: None,
        identity_file: None,
        proxy_jump: None,
        host_key_policy: HostKeyPolicy::Strict,
        known_hosts_file: None,
        ssh_config_file: None,
        interactive: false,
        connect_timeout_s: Some(3),
        ssh_path: None,
        root: None,
        protocol: Protocol::Sftp,
        ftp: None,
    }
}

/// `ssh -G` output as lowercase "key value" lines.
fn resolve(p: &ConnectParams) -> Vec<String> {
    let ssh = find_ssh(None).expect("ssh installed");
    let args = build_args(p, ArgsMode::PrintConfig).unwrap();
    let o = Command::new(ssh).args(&args).output().unwrap();
    assert!(o.status.success(), "ssh rejected our arguments: {}\nargs: {args:?}", String::from_utf8_lossy(&o.stderr));
    String::from_utf8_lossy(&o.stdout).lines().map(|l| l.to_string()).collect()
}

fn has(lines: &[String], l: &str) -> bool {
    lines.iter().any(|x| x == l)
}

#[test]
#[ignore = "needs the OS OpenSSH client"]
fn ssh_is_found_and_reports_a_version() {
    let ssh = find_ssh(None).unwrap();
    let o = Command::new(ssh).arg("-V").output().unwrap();
    let v = String::from_utf8_lossy(&o.stderr).to_string() + &String::from_utf8_lossy(&o.stdout);
    assert!(v.contains("OpenSSH"), "{v}");
}

#[test]
#[ignore = "needs the OS OpenSSH client"]
fn our_hardened_options_are_what_ssh_resolves() {
    let mut p = params("203.0.113.7"); // TEST-NET-3, never contacted by -G
    p.port = Some(2222);
    p.user = Some("deploy".into());
    p.proxy_jump = Some("bastion.example".into());
    p.known_hosts_file = Some(r"C:\Users\x\AppData\Local\Brouillon\known hosts".into());
    let l = resolve(&p);
    for want in [
        "stricthostkeychecking true",
        "forwardagent no",
        "forwardx11 no",
        "clearallforwardings yes",
        "batchmode yes",
        "port 2222",
        "user deploy",
        "proxyjump bastion.example",
        "serveraliveinterval 15",
        "serveralivecountmax 3",
        "connecttimeout 3",
        "permitlocalcommand no",
    ] {
        assert!(has(&l, want), "missing `{want}` in:\n{}", l.join("\n"));
    }
    assert!(l.iter().any(|x| x.starts_with("userknownhostsfile") && x.contains("known hosts")), "{l:?}");
    assert!(!l.iter().any(|x| x == "stricthostkeychecking false"));
}

#[test]
#[ignore = "needs the OS OpenSSH client"]
fn policies_map_to_the_expected_ssh_values() {
    let mut p = params("203.0.113.7");
    p.host_key_policy = HostKeyPolicy::AcceptNew;
    assert!(has(&resolve(&p), "stricthostkeychecking accept-new"));
    p.host_key_policy = HostKeyPolicy::Ask;
    p.interactive = true;
    let l = resolve(&p);
    assert!(has(&l, "stricthostkeychecking ask") && has(&l, "batchmode no"));
}

#[test]
#[ignore = "needs the OS OpenSSH client"]
fn a_user_ssh_config_alias_is_honoured() {
    let dir = std::env::temp_dir().join(format!("brouillon-remote-test-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let cfg = dir.join("ssh config");
    std::fs::write(&cfg, "Host myalias\n  HostName 203.0.113.9\n  User bob\n  Port 2200\n  ForwardAgent yes\n").unwrap();
    let mut p = params("myalias");
    p.ssh_config_file = Some(cfg.to_string_lossy().into_owned());
    let l = resolve(&p);
    assert!(has(&l, "hostname 203.0.113.9"), "{l:?}");
    assert!(has(&l, "user bob") && has(&l, "port 2200"), "{l:?}");
    // our hardening wins over the user's `ForwardAgent yes`
    assert!(has(&l, "forwardagent no"), "{l:?}");
    // an explicit user/port from the editor overrides the alias
    p.user = Some("alice".into());
    p.port = Some(2300);
    let l = resolve(&p);
    assert!(has(&l, "user alice") && has(&l, "port 2300"), "{l:?}");
    let _ = std::fs::remove_dir_all(dir);
}

struct Never;
impl Prompter for Never {
    fn ask(&self, _: PromptKind, _: &str) -> Option<Secret> {
        None
    }
}

#[test]
#[ignore = "needs the OS OpenSSH client; connects to 127.0.0.1:1 only"]
fn connecting_to_a_closed_local_port_fails_cleanly() {
    let conn = SshConnector { askpass_exe: std::env::current_exe().unwrap() };
    let mut p = params("127.0.0.1");
    p.port = Some(1);
    let link = conn.connect(&p, Arc::new(Never)).expect("ssh starts");
    // the SFTP handshake cannot succeed: the helper classifies ssh's stderr
    let helper = brouillon_remote::sftp::SftpClient::connect(link.reader, link.writer, brouillon_remote::sftp::ClientConfig {
        handshake_timeout: std::time::Duration::from_secs(10),
        ..Default::default()
    });
    assert!(helper.is_err());
    let mut g = link.guard;
    g.kill();
    let text = String::from_utf8_lossy(&link.diag.lock().unwrap()).to_string();
    let e = brouillon_remote::transport::classify_stderr(&text);
    assert_eq!(e.kind, ErrorKind::ConnectFailed, "{text}");
    assert!(text.to_lowercase().contains("refused") || text.to_lowercase().contains("connect"), "{text}");
}
