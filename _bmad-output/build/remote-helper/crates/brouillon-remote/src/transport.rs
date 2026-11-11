//! Spawning the OS `ssh` with `-s sftp`: hardened argument list, lookup, stderr diagnosis, kill on drop.
//!
//! STATUS: the argument builder, validation and stderr classification are unit-tested, and the
//! argument list is accepted by the machine's real `ssh -G` (ignored integration test). Nothing has
//! also exercised end to end through the real ssh.exe against the dev-only user-space SSH/SFTP
//! server (`crates/brouillon-test-sshd`, `tests/real_session.rs`, all `#[ignore]`); never against an
//! external host.

use brouillon_remote_proto::*;
use std::io::{Read, Write};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// A live connection to the transport (ssh child, or a test fake).
pub struct Link {
    pub reader: Box<dyn Read + Send>,
    pub writer: Box<dyn Write + Send>,
    pub guard: Box<dyn Killer>,
    /// Tail of the transport's stderr (ssh diagnostics), for error classification.
    pub diag: Arc<Mutex<Vec<u8>>>,
}

pub trait Killer: Send {
    /// Terminate the transport and reap it. Idempotent.
    fn kill(&mut self);
}

pub trait Prompter: Send + Sync {
    /// Blocks until the user answered (or cancelled: `None`).
    fn ask(&self, kind: PromptKind, text: &str) -> Option<Secret>;
}

pub trait Connector: Send + Sync {
    fn connect(&self, p: &ConnectParams, prompter: Arc<dyn Prompter>) -> Result<Link, RemoteError>;

    /// Network environment of FTP/FTPS connections (real sockets and the OS trust store by default;
    /// tests inject an in-memory dialer and their own roots).
    fn ftp_env(&self) -> crate::ftp::FtpEnv {
        crate::ftp::FtpEnv::default()
    }
}

// ---------------------------------------------------------------- validation

fn bad(msg: &str) -> RemoteError {
    RemoteError::new(ErrorKind::InvalidArgument, msg)
}

fn host_like(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 255
        && !s.starts_with('-')
        && s.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-' | ':' | '%' | '[' | ']'))
}

fn user_like(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 128
        && !s.starts_with('-')
        && s.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-' | '@' | '\\' | '$'))
}

fn jump_like(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 512
        && !s.starts_with('-')
        && s.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-' | ':' | '%' | '[' | ']' | '@' | ',' | '\\'))
}

fn path_arg_ok(s: &str) -> bool {
    !s.is_empty() && s.len() <= 1024 && !s.chars().any(|c| c.is_control() || c == '"')
}

/// ssh option values: forward slashes (Windows OpenSSH accepts them and it avoids backslash escapes),
/// `%` doubled (ssh expands `%d`-style tokens), quoted when they contain a blank.
fn option_path(s: &str) -> String {
    let t = s.replace('\\', "/").replace('%', "%%");
    if t.contains(' ') {
        format!("\"{t}\"")
    } else {
        t
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArgsMode {
    /// `-s host sftp`
    Sftp,
    /// `-G host`: print the resolved configuration and exit (no connection). Used to check our options.
    PrintConfig,
}

/// Build the argument list for `ssh`. No secret is ever put in argv. `-o StrictHostKeyChecking=no`
/// is impossible by construction (the policy enum has no such value).
pub fn build_args(p: &ConnectParams, mode: ArgsMode) -> Result<Vec<String>, RemoteError> {
    if !host_like(&p.host) {
        return Err(bad("invalid host"));
    }
    if let Some(u) = &p.user {
        if !user_like(u) {
            return Err(bad("invalid user"));
        }
    }
    if let Some(j) = &p.proxy_jump {
        if !jump_like(j) {
            return Err(bad("invalid proxy jump"));
        }
    }
    for f in [&p.identity_file, &p.known_hosts_file, &p.ssh_config_file].into_iter().flatten() {
        if !path_arg_ok(f) {
            return Err(bad("invalid file path argument"));
        }
    }
    if p.port == Some(0) {
        return Err(bad("invalid port"));
    }
    let interactive = p.interactive;
    // `ask` needs a way to prompt; without it fall back to strict (never to a silent accept).
    let shk = match p.host_key_policy {
        HostKeyPolicy::Strict => "yes",
        HostKeyPolicy::AcceptNew => "accept-new",
        HostKeyPolicy::Ask if interactive => "ask",
        HostKeyPolicy::Ask => "yes",
    };
    let mut a: Vec<String> = Vec::new();
    let mut o = |s: String| a.push(s);
    if let Some(f) = &p.ssh_config_file {
        o("-F".into());
        o(f.replace('\\', "/"));
    }
    for f in ["-a", "-x", "-T"] {
        o(f.into());
    }
    for kv in [
        "ClearAllForwardings=yes",
        "ForwardAgent=no",
        "ForwardX11=no",
        "PermitLocalCommand=no",
        "NumberOfPasswordPrompts=2",
        "ServerAliveInterval=15",
        "ServerAliveCountMax=3",
    ] {
        o(format!("-o{kv}"));
    }
    o(format!("-oBatchMode={}", if interactive { "no" } else { "yes" }));
    o(format!("-oConnectTimeout={}", p.connect_timeout_s.unwrap_or(10).clamp(1, 120)));
    o(format!("-oStrictHostKeyChecking={shk}"));
    if let Some(k) = &p.known_hosts_file {
        o(format!("-oUserKnownHostsFile={}", option_path(k)));
    }
    if let Some(port) = p.port {
        o("-p".into());
        o(port.to_string());
    }
    if let Some(u) = &p.user {
        o("-l".into());
        o(u.clone());
    }
    if let Some(i) = &p.identity_file {
        o("-i".into());
        o(i.replace('\\', "/"));
        o("-oIdentitiesOnly=yes".into());
    }
    if let Some(j) = &p.proxy_jump {
        o("-J".into());
        o(j.clone());
    }
    match mode {
        ArgsMode::Sftp => {
            o("-s".into());
            o("--".into());
            o(p.host.clone());
            o("sftp".into());
        }
        ArgsMode::PrintConfig => {
            o("-G".into());
            o("--".into());
            o(p.host.clone());
        }
    }
    Ok(a)
}

// ---------------------------------------------------------------- lookup

/// Explicit path, then Windows OpenSSH, then `PATH`.
pub fn find_ssh(explicit: Option<&str>) -> Result<PathBuf, RemoteError> {
    let no_ssh = || RemoteError::new(ErrorKind::NoSsh, "the OpenSSH client (ssh) was not found; install the Windows 'OpenSSH Client' feature or set its path");
    if let Some(e) = explicit {
        let p = PathBuf::from(e);
        return if p.is_file() { Ok(p) } else { Err(no_ssh()) };
    }
    if let Some(root) = std::env::var_os("SystemRoot") {
        let p = PathBuf::from(root).join("System32").join("OpenSSH").join("ssh.exe");
        if p.is_file() {
            return Ok(p);
        }
    }
    if let Some(path) = std::env::var_os("PATH") {
        for dir in std::env::split_paths(&path) {
            for name in ["ssh.exe", "ssh"] {
                let c = dir.join(name);
                if c.is_file() {
                    return Ok(c);
                }
            }
        }
    }
    Err(no_ssh())
}

// ---------------------------------------------------------------- stderr diagnosis

/// Map ssh's stderr to a provider-neutral error. The text is untrusted and is sanitised again by `RemoteError`.
pub fn classify_stderr(stderr: &str) -> RemoteError {
    let l = stderr.to_ascii_lowercase();
    let kind = if l.contains("remote host identification has changed") || l.contains("host key has changed") {
        ErrorKind::HostKeyChanged
    } else if l.contains("host key verification failed") {
        ErrorKind::HostKeyRejected
    } else if l.contains("permission denied") || l.contains("too many authentication failures") || l.contains("authentication failed") {
        ErrorKind::AuthFailed
    } else if l.contains("timed out") {
        ErrorKind::Timeout
    } else {
        ErrorKind::ConnectFailed
    };
    let msg = stderr.trim();
    RemoteError::new(kind, if msg.is_empty() { "ssh exited without a message" } else { msg })
}

/// Map an ssh prompt to the kind of question the editor should show.
pub fn classify_prompt(text: &str) -> PromptKind {
    let l = text.to_ascii_lowercase();
    if l.contains("continue connecting") || l.contains("fingerprint") {
        PromptKind::HostKey
    } else if l.contains("passphrase") {
        PromptKind::Passphrase
    } else if l.contains("password") {
        PromptKind::Password
    } else {
        PromptKind::KeyboardInteractive
    }
}

// ---------------------------------------------------------------- the real connector

pub struct SshGuard {
    child: Option<Child>,
    diag_done: Arc<AtomicBool>,
}

impl Killer for SshGuard {
    fn kill(&mut self) {
        if let Some(mut c) = self.child.take() {
            let _ = c.kill();
            let _ = c.wait();
            // give the stderr reader a moment to drain what ssh printed before dying
            let t = Instant::now();
            while !self.diag_done.load(Ordering::Relaxed) && t.elapsed() < Duration::from_millis(300) {
                std::thread::sleep(Duration::from_millis(5));
            }
        }
    }
}

impl Drop for SshGuard {
    fn drop(&mut self) {
        self.kill();
    }
}

pub struct SshConnector {
    /// Path of this executable, used as `SSH_ASKPASS` helper.
    pub askpass_exe: PathBuf,
}

const DIAG_CAP: usize = 4096;

impl Connector for SshConnector {
    fn connect(&self, p: &ConnectParams, prompter: Arc<dyn Prompter>) -> Result<Link, RemoteError> {
        let args = build_args(p, ArgsMode::Sftp)?;
        let ssh = find_ssh(p.ssh_path.as_deref())?;
        let mut cmd = Command::new(&ssh);
        cmd.args(&args).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped());
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
        }
        // The askpass server lives as long as the Link's guard (dropped with it).
        let askpass = if p.interactive {
            let srv = crate::askpass::AskpassServer::start(prompter).map_err(|e| RemoteError::new(ErrorKind::Internal, format!("askpass listener: {e}")))?;
            cmd.env("SSH_ASKPASS", &self.askpass_exe)
                .env("SSH_ASKPASS_REQUIRE", "force")
                .env(crate::askpass::ENV_ADDR, srv.addr())
                .env(crate::askpass::ENV_TOKEN, srv.token());
            Some(srv)
        } else {
            None
        };
        let mut child = cmd.spawn().map_err(|e| RemoteError::new(ErrorKind::NoSsh, format!("cannot start ssh: {e}")))?;
        let reader = child.stdout.take().ok_or_else(|| RemoteError::new(ErrorKind::Internal, "no ssh stdout"))?;
        let writer = child.stdin.take().ok_or_else(|| RemoteError::new(ErrorKind::Internal, "no ssh stdin"))?;
        let diag = Arc::new(Mutex::new(Vec::<u8>::new()));
        let diag_done = Arc::new(AtomicBool::new(false));
        if let Some(mut err) = child.stderr.take() {
            let (d, done) = (diag.clone(), diag_done.clone());
            std::thread::spawn(move || {
                let mut buf = [0u8; 512];
                while let Ok(n) = err.read(&mut buf) {
                    if n == 0 {
                        break;
                    }
                    let mut v = d.lock().unwrap();
                    v.extend_from_slice(&buf[..n]);
                    if v.len() > DIAG_CAP {
                        let cut = v.len() - DIAG_CAP;
                        v.drain(..cut);
                    }
                }
                done.store(true, Ordering::Relaxed);
            });
        } else {
            diag_done.store(true, Ordering::Relaxed);
        }
        struct Guard(SshGuard, Option<crate::askpass::AskpassServer>);
        impl Killer for Guard {
            fn kill(&mut self) {
                self.0.kill();
                self.1.take();
            }
        }
        Ok(Link { reader: Box::new(reader), writer: Box::new(writer), guard: Box::new(Guard(SshGuard { child: Some(child), diag_done }, askpass)), diag })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn params() -> ConnectParams {
        ConnectParams {
            host: "myvps".into(),
            port: None,
            user: None,
            identity_file: None,
            proxy_jump: None,
            host_key_policy: HostKeyPolicy::Strict,
            known_hosts_file: None,
            ssh_config_file: None,
            interactive: false,
            connect_timeout_s: None,
            ssh_path: None,
            root: None,
            protocol: Protocol::Sftp,
            ftp: None,
        }
    }

    #[test]
    fn hardened_baseline() {
        let a = build_args(&params(), ArgsMode::Sftp).unwrap();
        for must in ["-a", "-x", "-T", "-oForwardAgent=no", "-oForwardX11=no", "-oClearAllForwardings=yes", "-oBatchMode=yes", "-oStrictHostKeyChecking=yes", "-oConnectTimeout=10", "-oServerAliveInterval=15", "-s"] {
            assert!(a.iter().any(|x| x == must), "missing {must}: {a:?}");
        }
        assert_eq!(&a[a.len() - 3..], ["--", "myvps", "sftp"]);
        assert!(!a.iter().any(|x| x.to_ascii_lowercase().contains("stricthostkeychecking=no")));
    }

    #[test]
    fn host_key_policies() {
        let mut p = params();
        p.host_key_policy = HostKeyPolicy::AcceptNew;
        assert!(build_args(&p, ArgsMode::Sftp).unwrap().contains(&"-oStrictHostKeyChecking=accept-new".to_string()));
        p.host_key_policy = HostKeyPolicy::Ask;
        // not interactive: ask degrades to strict, never to "no"
        assert!(build_args(&p, ArgsMode::Sftp).unwrap().contains(&"-oStrictHostKeyChecking=yes".to_string()));
        p.interactive = true;
        let a = build_args(&p, ArgsMode::Sftp).unwrap();
        assert!(a.contains(&"-oStrictHostKeyChecking=ask".to_string()));
        assert!(a.contains(&"-oBatchMode=no".to_string()));
    }

    #[test]
    fn options_are_passed() {
        let mut p = params();
        p.port = Some(2222);
        p.user = Some("deploy".into());
        p.identity_file = Some(r"C:\Users\me\.ssh\id ed25519".into());
        p.proxy_jump = Some("bastion,other@h2:2200".into());
        p.known_hosts_file = Some(r"C:\Users\me\AppData\Brouillon\known hosts".into());
        p.ssh_config_file = Some("C:/cfg/ssh_config".into());
        p.connect_timeout_s = Some(5);
        let a = build_args(&p, ArgsMode::Sftp).unwrap();
        let has = |x: &str| a.iter().any(|s| s == x);
        assert!(has("-p") && has("2222") && has("-l") && has("deploy") && has("-J") && has("-oConnectTimeout=5"));
        assert!(has("C:/Users/me/.ssh/id ed25519") && has("-oIdentitiesOnly=yes"));
        assert!(has("-oUserKnownHostsFile=\"C:/Users/me/AppData/Brouillon/known hosts\""));
        assert_eq!(a[0], "-F");
    }

    #[test]
    fn argument_injection_is_refused() {
        for h in ["-oProxyCommand=calc", "", "a b", "host;rm", "ho\nst", "$(x)", "-J"] {
            let mut p = params();
            p.host = h.into();
            assert!(build_args(&p, ArgsMode::Sftp).is_err(), "{h:?}");
        }
        let mut p = params();
        p.user = Some("-oProxyCommand=x".into());
        assert!(build_args(&p, ArgsMode::Sftp).is_err());
        let mut p = params();
        p.proxy_jump = Some("-oProxyCommand=x".into());
        assert!(build_args(&p, ArgsMode::Sftp).is_err());
        let mut p = params();
        p.identity_file = Some("a\"b".into());
        assert!(build_args(&p, ArgsMode::Sftp).is_err());
        p.identity_file = Some("a\nb".into());
        assert!(build_args(&p, ArgsMode::Sftp).is_err());
        let mut p = params();
        p.port = Some(0);
        assert!(build_args(&p, ArgsMode::Sftp).is_err());
    }

    #[test]
    fn password_never_in_args() {
        // there is no field that could carry a secret into argv; the only places for text are validated
        let a = build_args(&params(), ArgsMode::Sftp).unwrap().join(" ");
        assert!(!a.to_ascii_lowercase().contains("password=") && !a.contains("sshpass"));
    }

    #[test]
    fn stderr_classification() {
        use ErrorKind::*;
        let k = |s: &str| classify_stderr(s).kind;
        assert_eq!(k("user@h: Permission denied (publickey,password)."), AuthFailed);
        assert_eq!(k("Host key verification failed."), HostKeyRejected);
        assert_eq!(k("@@@ WARNING: REMOTE HOST IDENTIFICATION HAS CHANGED! @@@"), HostKeyChanged);
        assert_eq!(k("ssh: connect to host x port 22: Connection timed out"), Timeout);
        assert_eq!(k("ssh: Could not resolve hostname nope: No such host is known."), ConnectFailed);
        assert_eq!(k("ssh: connect to host 127.0.0.1 port 1: Connection refused"), ConnectFailed);
        assert_eq!(k(""), ConnectFailed);
    }

    #[test]
    fn prompt_classification() {
        assert_eq!(classify_prompt("Are you sure you want to continue connecting (yes/no/[fingerprint])?"), PromptKind::HostKey);
        assert_eq!(classify_prompt("Enter passphrase for key '/home/u/.ssh/id_ed25519': "), PromptKind::Passphrase);
        assert_eq!(classify_prompt("u@h's password: "), PromptKind::Password);
        assert_eq!(classify_prompt("Verification code: "), PromptKind::KeyboardInteractive);
    }

    #[test]
    fn missing_explicit_ssh_is_reported() {
        assert_eq!(find_ssh(Some("Z:/definitely/not/here/ssh.exe")).unwrap_err().kind, ErrorKind::NoSsh);
    }
}
