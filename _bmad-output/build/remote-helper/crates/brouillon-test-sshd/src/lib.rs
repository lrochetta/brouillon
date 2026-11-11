//! DEV-ONLY user-space SSH/SFTP server used to prove the Brouillon remote helper against the real
//! OpenSSH client (`ssh.exe -s sftp`). It is never shipped and the shipped helper does not depend on it.
//!
//! * listens on 127.0.0.1 only, on an ephemeral port (nothing else is ever opened);
//! * ephemeral ed25519 host key and client keys, written to a temp directory in OpenSSH format;
//! * public-key auth (the generated client key only) plus an optional fixed password;
//! * serves a confined sandbox directory as the virtual root `/`: no shell, no exec, no pty, no
//!   forwarding (every such request is refused and logged), `..` is clamped at the root;
//! * SFTP v3 with `posix-rename@openssh.com` and `fsync@openssh.com`;
//! * symlinks and POSIX modes are EMULATED in memory (Windows has no cheap unprivileged symlinks and
//!   no POSIX modes): `add_symlink` registers a virtual link, `setstat`/`open` modes are remembered;
//! * fault injection (one-shot): drop the TCP connection after N written bytes, stop answering after
//!   N written bytes, drop on a given operation, delay every reply.

use russh::keys::ssh_key::{Algorithm, LineEnding, PrivateKey, PublicKey};
use russh::server::{run_stream, Auth, ChannelOpenHandle, Config, Handler, Msg, Session};
use russh::{Channel, ChannelId, MethodKind, MethodSet};
use russh_sftp::protocol::{Attrs, Data, File, FileAttributes, Handle, Name, OpenFlags, Packet, Status, StatusCode, Version};
use std::collections::HashMap;
use std::net::{Shutdown, SocketAddr};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

pub const POSIX_RENAME: &str = "posix-rename@openssh.com";
pub const FSYNC: &str = "fsync@openssh.com";
/// The password accepted when password auth is enabled (a test value, not a secret).
pub const TEST_PASSWORD: &str = "t3st-Pa55 w0rd";

const S_REG: u32 = 0o100000;
const S_DIR: u32 = 0o040000;
const S_LNK: u32 = 0o120000;

/// Fault injection knobs. All "after" faults are one-shot (they disarm when they fire) so a
/// reconnect after the fault works.
#[derive(Default)]
pub struct Faults {
    /// Cut the TCP connection once this many payload bytes were written (0 = off).
    pub drop_after_write_bytes: AtomicU64,
    /// Keep the connection open but stop answering once this many bytes were written (0 = off).
    pub stall_after_write_bytes: AtomicU64,
    /// Delay before every SFTP reply, in milliseconds.
    pub slow_ms: AtomicU64,
    /// Do not advertise `posix-rename@openssh.com`.
    pub hide_posix_rename: AtomicBool,
    /// Cut the connection when this SFTP operation arrives (e.g. `"rename"`, `"posix-rename"`); one-shot.
    pub drop_on_op: Mutex<Option<String>>,
    written: AtomicU64,
    stalled: AtomicBool,
    /// Server-side timing of WRITE requests: (calls, nanoseconds spent handling, nanoseconds between the end of one and the start of the next).
    pub write_calls: AtomicU64,
    pub write_busy_ns: AtomicU64,
    pub write_gap_ns: AtomicU64,
    last_write_end: Mutex<Option<std::time::Instant>>,
}

impl Faults {
    pub fn reset(&self) {
        self.drop_after_write_bytes.store(0, Ordering::SeqCst);
        self.stall_after_write_bytes.store(0, Ordering::SeqCst);
        self.slow_ms.store(0, Ordering::SeqCst);
        self.hide_posix_rename.store(false, Ordering::SeqCst);
        *self.drop_on_op.lock().unwrap() = None;
        self.written.store(0, Ordering::SeqCst);
        self.stalled.store(false, Ordering::SeqCst);
        self.write_calls.store(0, Ordering::SeqCst);
        self.write_busy_ns.store(0, Ordering::SeqCst);
        self.write_gap_ns.store(0, Ordering::SeqCst);
        *self.last_write_end.lock().unwrap() = None;
    }
}

struct Shared {
    root: PathBuf,
    faults: Arc<Faults>,
    log: Mutex<Vec<String>>,
    links: Mutex<HashMap<String, String>>,
    modes: Mutex<HashMap<PathBuf, u32>>,
    client_keys: Vec<PublicKey>,
    password: Option<String>,
    connections: AtomicU64,
    killers: Mutex<Vec<Arc<std::net::TcpStream>>>,
    methods: MethodSet,
}

impl Shared {
    fn log(&self, s: impl Into<String>) {
        self.log.lock().unwrap().push(s.into());
    }
}

/// A running test server. Dropping it shuts it down.
pub struct TestSshd {
    pub addr: SocketAddr,
    /// Sandbox directory served as `/`.
    pub root: PathBuf,
    /// Directory holding the generated keys.
    pub key_dir: PathBuf,
    /// Private key (OpenSSH format) that the server accepts.
    pub client_key: PathBuf,
    /// A private key the server does not know.
    pub wrong_key: PathBuf,
    /// The server host key as an `ssh-ed25519 AAAA...` line.
    pub host_key_line: String,
    pub faults: Arc<Faults>,
    shared: Arc<Shared>,
    stop: Option<tokio::sync::oneshot::Sender<()>>,
    thread: Option<std::thread::JoinHandle<()>>,
}

pub struct Options {
    /// Directory to serve (created if missing).
    pub root: PathBuf,
    /// Directory for the generated key files (created if missing).
    pub key_dir: PathBuf,
    /// Accept [`TEST_PASSWORD`] as a password.
    pub password_auth: bool,
}

/// A fresh ed25519 key pair: (private key in OpenSSH format, public key as an `ssh-ed25519 AAAA...` line).
pub fn generate_keypair() -> (String, String) {
    let k = PrivateKey::random(&mut rand::rng(), Algorithm::Ed25519).expect("key generation");
    let public = k.public_key().to_openssh().expect("public key encoding");
    (k.to_openssh(LineEnding::LF).expect("private key encoding").to_string(), public)
}

/// OpenSSH refuses a private key whose file is readable by anyone else ("UNPROTECTED PRIVATE KEY FILE").
/// A file created in %TEMP% inherits a permissive ACL on Windows (observed: an orphan SID in the
/// inherited ACL), so cut the inheritance and grant read to the current user only. Unix: mode 0600.
fn restrict_to_current_user(path: &Path) {
    #[cfg(windows)]
    {
        let user = std::env::var("USERNAME").unwrap_or_default();
        let _ = std::process::Command::new("icacls")
            .arg(path)
            .args(["/inheritance:r", "/grant:r"])
            .arg(format!("{user}:(R)"))
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status();
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
    }
}

impl TestSshd {
    pub fn start(opts: Options) -> std::io::Result<TestSshd> {
        std::fs::create_dir_all(&opts.root)?;
        std::fs::create_dir_all(&opts.key_dir)?;
        let host = PrivateKey::random(&mut rand::rng(), Algorithm::Ed25519).expect("host key");
        let host_key_line = host.public_key().to_openssh().expect("host key line");
        let client = PrivateKey::random(&mut rand::rng(), Algorithm::Ed25519).expect("client key");
        let wrong = PrivateKey::random(&mut rand::rng(), Algorithm::Ed25519).expect("wrong key");
        let client_path = opts.key_dir.join("client_ed25519");
        let wrong_path = opts.key_dir.join("wrong_ed25519");
        std::fs::write(&client_path, client.to_openssh(LineEnding::LF).expect("enc").as_bytes())?;
        std::fs::write(&wrong_path, wrong.to_openssh(LineEnding::LF).expect("enc").as_bytes())?;
        restrict_to_current_user(&client_path);
        restrict_to_current_user(&wrong_path);
        std::fs::write(opts.key_dir.join("client_ed25519.pub"), client.public_key().to_openssh().expect("enc"))?;

        let mut methods = MethodSet::empty();
        methods.push(MethodKind::PublicKey);
        if opts.password_auth {
            methods.push(MethodKind::Password);
        }
        let faults = Arc::new(Faults::default());
        let root = std::fs::canonicalize(&opts.root)?;
        let shared = Arc::new(Shared {
            root: root.clone(),
            faults: faults.clone(),
            log: Mutex::new(Vec::new()),
            links: Mutex::new(HashMap::new()),
            modes: Mutex::new(HashMap::new()),
            client_keys: vec![client.public_key().clone()],
            password: opts.password_auth.then(|| TEST_PASSWORD.to_string()),
            connections: AtomicU64::new(0),
            killers: Mutex::new(Vec::new()),
            methods: methods.clone(),
        });
        let cfg = Arc::new(Config {
            methods,
            auth_rejection_time: Duration::from_millis(50),
            auth_rejection_time_initial: Some(Duration::from_millis(0)),
            keys: vec![host],
            inactivity_timeout: Some(Duration::from_secs(120)),
            max_auth_attempts: 6,
            nodelay: true,
            window_size: 16 * 1024 * 1024,
            ..Default::default()
        });
        let (addr_tx, addr_rx) = std::sync::mpsc::channel();
        let (stop_tx, mut stop_rx) = tokio::sync::oneshot::channel::<()>();
        let sh = shared.clone();
        let thread = std::thread::Builder::new().name("test-sshd".into()).spawn(move || {
            let rt = tokio::runtime::Builder::new_multi_thread().worker_threads(2).enable_all().build().expect("tokio runtime");
            rt.block_on(async move {
                let listener = match tokio::net::TcpListener::bind("127.0.0.1:0").await {
                    Ok(l) => l,
                    Err(e) => {
                        let _ = addr_tx.send(Err(e));
                        return;
                    }
                };
                let _ = addr_tx.send(Ok(listener.local_addr().expect("addr")));
                loop {
                    tokio::select! {
                        _ = &mut stop_rx => break,
                        acc = listener.accept() => {
                            let Ok((sock, peer)) = acc else { break };
                            if !peer.ip().is_loopback() {
                                continue; // belt and braces: the socket is bound to loopback anyway
                            }
                            let Ok(std_sock) = sock.into_std() else { continue };
                            let Ok(killer) = std_sock.try_clone() else { continue };
                            let Ok(sock) = tokio::net::TcpStream::from_std(std_sock) else { continue };
                            sh.connections.fetch_add(1, Ordering::SeqCst);
                            let killer = Arc::new(killer);
                            sh.killers.lock().unwrap().push(killer.clone());
                            let handler = SshHandler { shared: sh.clone(), killer, channels: HashMap::new() };
                            let cfg = cfg.clone();
                            tokio::spawn(async move {
                                if let Ok(s) = run_stream(cfg, sock, handler).await {
                                    let _ = s.await;
                                }
                            });
                        }
                    }
                }
            });
        })?;
        let addr = addr_rx.recv().map_err(|_| std::io::Error::other("server thread died"))??;
        Ok(TestSshd {
            addr,
            root,
            key_dir: opts.key_dir,
            client_key: client_path,
            wrong_key: wrong_path,
            host_key_line,
            faults,
            shared,
            stop: Some(stop_tx),
            thread: Some(thread),
        })
    }

    pub fn port(&self) -> u16 {
        self.addr.port()
    }

    /// `known_hosts` line for this server (OpenSSH syntax for a non-default port).
    pub fn known_hosts_line(&self) -> String {
        format!("[127.0.0.1]:{} {}\n", self.port(), self.host_key_line)
    }

    /// Register an emulated symlink: `link` and `target` are virtual absolute paths (`/work/l` -> `/secret/x`).
    pub fn add_symlink(&self, link: &str, target: &str) {
        self.shared.links.lock().unwrap().insert(norm(link), target.to_string());
    }

    /// Number of TCP connections accepted so far.
    pub fn connections(&self) -> u64 {
        self.shared.connections.load(Ordering::SeqCst)
    }

    /// Simulate a network failure: abruptly cut every open connection.
    pub fn kill_all_connections(&self) {
        for k in self.shared.killers.lock().unwrap().iter() {
            let _ = k.shutdown(Shutdown::Both);
        }
    }

    /// Server-side event log (auth, requests refused, SFTP operations).
    pub fn log(&self) -> Vec<String> {
        self.shared.log.lock().unwrap().clone()
    }
    pub fn clear_log(&self) {
        self.shared.log.lock().unwrap().clear();
    }

    /// Real path of a virtual path (for assertions on disk).
    pub fn real(&self, virt: &str) -> PathBuf {
        self.root.join(norm(virt).trim_start_matches('/'))
    }
}

impl Drop for TestSshd {
    fn drop(&mut self) {
        if let Some(s) = self.stop.take() {
            let _ = s.send(());
        }
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

// ------------------------------------------------------------------ path helpers

/// Lexical normalisation of a virtual path: absolute, no `.`/`..`, `..` clamped at the root.
fn norm(p: &str) -> String {
    let mut out: Vec<&str> = Vec::new();
    let p = p.replace('\\', "/");
    for c in p.split('/') {
        match c {
            "" | "." => {}
            ".." => {
                out.pop();
            }
            c => out.push(c),
        }
    }
    // the slices borrow from a temporary; rebuild owned
    let mut s = String::new();
    for c in &out {
        s.push('/');
        s.push_str(c);
    }
    if s.is_empty() {
        "/".into()
    } else {
        s
    }
}

fn parent_of(v: &str) -> String {
    match v.rfind('/') {
        Some(0) | None => "/".into(),
        Some(i) => v[..i].to_string(),
    }
}

fn component_ok(c: &str) -> bool {
    !c.contains([':', '\\', '<', '>', '"', '|', '?', '*']) && c != "." && c != ".."
}

// ------------------------------------------------------------------ ssh layer

struct SshHandler {
    shared: Arc<Shared>,
    killer: Arc<std::net::TcpStream>,
    channels: HashMap<ChannelId, Channel<Msg>>,
}

impl Handler for SshHandler {
    type Error = russh::Error;

    async fn auth_publickey_offered(&mut self, user: &str, offer: russh::server::PublicKeyOffer<'_>) -> Result<Auth, Self::Error> {
        let _ = user;
        let known = self.shared.client_keys.iter().any(|k| k.key_data() == offer.public_key.key_data());
        self.shared.log(format!("auth: publickey offered, known={known}"));
        Ok(if known { Auth::Accept } else { Auth::reject() })
    }

    async fn auth_publickey(&mut self, user: &str, key: russh::server::VerifiedPublicKey<'_>) -> Result<Auth, Self::Error> {
        let ok = self.shared.client_keys.iter().any(|k| k.key_data() == key.public_key.key_data());
        self.shared.log(format!("auth: publickey user={user} accepted={ok}"));
        Ok(if ok { Auth::Accept } else { Auth::reject() })
    }

    async fn auth_password(&mut self, user: &str, password: &str) -> Result<Auth, Self::Error> {
        let ok = self.shared.password.as_deref() == Some(password);
        // the attempted password is deliberately NOT logged
        self.shared.log(format!("auth: password user={user} accepted={ok}"));
        // keep `password` on offer after a failure so the client can retry (like sshd does)
        Ok(if ok { Auth::Accept } else { Auth::Reject { proceed_with_methods: Some(self.shared.methods.clone()), partial_success: false } })
    }

    async fn channel_open_session(&mut self, channel: Channel<Msg>, reply: ChannelOpenHandle, _session: &mut Session) -> Result<(), Self::Error> {
        self.channels.insert(channel.id(), channel);
        reply.accept().await;
        Ok(())
    }

    async fn subsystem_request(&mut self, channel: ChannelId, name: &str, session: &mut Session) -> Result<(), Self::Error> {
        self.shared.log(format!("subsystem: {name}"));
        match (name, self.channels.remove(&channel)) {
            ("sftp", Some(ch)) => {
                session.channel_success(channel)?;
                let sftp = Sftp { shared: self.shared.clone(), killer: self.killer.clone(), next: 0, handles: HashMap::new() };
                russh_sftp::server::run(ch.into_stream(), sftp).await;
            }
            _ => session.channel_failure(channel)?,
        }
        Ok(())
    }

    async fn exec_request(&mut self, channel: ChannelId, data: &[u8], session: &mut Session) -> Result<(), Self::Error> {
        self.shared.log(format!("REFUSED exec ({} bytes)", data.len()));
        session.channel_failure(channel)
    }
    async fn shell_request(&mut self, channel: ChannelId, session: &mut Session) -> Result<(), Self::Error> {
        self.shared.log("REFUSED shell");
        session.channel_failure(channel)
    }
    #[allow(clippy::too_many_arguments)]
    async fn pty_request(
        &mut self,
        channel: ChannelId,
        _term: &str,
        _c: u32,
        _r: u32,
        _pw: u32,
        _ph: u32,
        _modes: &[(russh::Pty, u32)],
        session: &mut Session,
    ) -> Result<(), Self::Error> {
        self.shared.log("REFUSED pty");
        session.channel_failure(channel)
    }
    async fn env_request(&mut self, channel: ChannelId, name: &str, _value: &str, session: &mut Session) -> Result<(), Self::Error> {
        self.shared.log(format!("REFUSED env {name}"));
        session.channel_failure(channel)
    }
    async fn agent_request(&mut self, _channel: ChannelId, _session: &mut Session) -> Result<bool, Self::Error> {
        self.shared.log("REFUSED agent forwarding");
        Ok(false)
    }
    async fn tcpip_forward(&mut self, address: &str, _port: &mut u32, _session: &mut Session) -> Result<bool, Self::Error> {
        self.shared.log(format!("REFUSED tcpip-forward {address}"));
        Ok(false)
    }
}

// ------------------------------------------------------------------ sftp layer

enum H {
    File { f: tokio::fs::File, real: PathBuf, pos: u64 },
    Dir { entries: Vec<File>, pos: usize },
}

struct Sftp {
    shared: Arc<Shared>,
    killer: Arc<std::net::TcpStream>,
    next: u64,
    handles: HashMap<String, H>,
}

type Res<T> = Result<T, StatusCode>;

fn ok_status(id: u32) -> Status {
    Status { id, status_code: StatusCode::Ok, error_message: "Ok".into(), language_tag: "en-US".into() }
}

fn io_code(e: &std::io::Error) -> StatusCode {
    use std::io::ErrorKind::*;
    match e.kind() {
        NotFound => StatusCode::NoSuchFile,
        PermissionDenied => StatusCode::PermissionDenied,
        _ => StatusCode::Failure, // AlreadyExists, NotADirectory, DirectoryNotEmpty... as OpenSSH does
    }
}

fn unix_secs(t: SystemTime) -> u32 {
    t.duration_since(UNIX_EPOCH).map(|d| d.as_secs() as u32).unwrap_or(0)
}

impl Sftp {
    fn fault_kill(&self, why: &str) {
        self.shared.log(format!("FAULT: {why}: connection cut"));
        let _ = self.killer.shutdown(Shutdown::Both);
    }

    /// Per-request hook: stalls, slow replies, drop-on-op faults.
    async fn pre(&mut self, op: &str) -> Res<()> {
        self.shared.log(format!("sftp {op}"));
        let f = &self.shared.faults;
        if f.stalled.load(Ordering::SeqCst) {
            std::future::pending::<()>().await;
        }
        let hit = {
            let mut g = f.drop_on_op.lock().unwrap();
            if g.as_deref() == Some(op) {
                *g = None;
                true
            } else {
                false
            }
        };
        if hit {
            self.fault_kill(&format!("drop on {op}"));
            std::future::pending::<()>().await;
        }
        let ms = f.slow_ms.load(Ordering::SeqCst);
        if ms > 0 {
            tokio::time::sleep(Duration::from_millis(ms)).await;
        }
        Ok(())
    }

    /// Virtual path -> real path, with the final component's virtual symlinks followed.
    fn resolve(&self, p: &str, follow: bool) -> Res<(String, PathBuf)> {
        let mut v = norm(p);
        if follow {
            let links = self.shared.links.lock().unwrap();
            for _ in 0..8 {
                match links.get(&v) {
                    Some(t) => {
                        v = if t.starts_with('/') { norm(t) } else { norm(&format!("{}/{}", parent_of(&v), t)) };
                    }
                    None => break,
                }
            }
        }
        self.real_of(&v).map(|r| (v, r))
    }

    fn real_of(&self, v: &str) -> Res<PathBuf> {
        let rel = v.trim_start_matches('/');
        if rel.split('/').any(|c| !c.is_empty() && !component_ok(c)) {
            return Err(StatusCode::PermissionDenied);
        }
        Ok(self.shared.root.join(rel))
    }

    fn mode_of(&self, real: &Path, is_dir: bool) -> u32 {
        self.shared.modes.lock().unwrap().get(real).copied().unwrap_or(if is_dir { 0o755 } else { 0o644 })
    }

    fn attrs_of(&self, real: &Path, md: &std::fs::Metadata) -> FileAttributes {
        let is_dir = md.is_dir();
        FileAttributes {
            size: Some(md.len()),
            uid: Some(1000),
            gid: Some(1000),
            permissions: Some(self.mode_of(real, is_dir) | if is_dir { S_DIR } else { S_REG }),
            atime: Some(unix_secs(md.accessed().unwrap_or(UNIX_EPOCH))),
            mtime: Some(unix_secs(md.modified().unwrap_or(UNIX_EPOCH))),
            ..Default::default()
        }
    }

    fn link_attrs() -> FileAttributes {
        FileAttributes { size: Some(0), uid: Some(1000), gid: Some(1000), permissions: Some(0o777 | S_LNK), atime: Some(0), mtime: Some(0), ..Default::default() }
    }

    fn new_handle(&mut self, h: H) -> String {
        self.next += 1;
        let k = format!("h{}", self.next);
        self.handles.insert(k.clone(), h);
        k
    }

    fn set_mode(&self, real: &Path, attrs: &FileAttributes) {
        if let Some(p) = attrs.permissions {
            self.shared.modes.lock().unwrap().insert(real.to_path_buf(), p & 0o7777);
        }
    }
}

impl russh_sftp::server::Handler for Sftp {
    type Error = StatusCode;

    fn unimplemented(&self) -> Self::Error {
        StatusCode::OpUnsupported
    }

    async fn init(&mut self, _version: u32, _ext: HashMap<String, String>) -> Res<Version> {
        let mut v = Version::new();
        if !self.shared.faults.hide_posix_rename.load(Ordering::SeqCst) {
            v.extensions.insert(POSIX_RENAME.into(), "1".into());
        }
        v.extensions.insert(FSYNC.into(), "1".into());
        Ok(v)
    }

    async fn realpath(&mut self, id: u32, path: String) -> Res<Name> {
        self.pre("realpath").await?;
        let (v, _) = self.resolve(&path, true)?;
        Ok(Name { id, files: vec![File::dummy(v)] })
    }

    async fn lstat(&mut self, id: u32, path: String) -> Res<Attrs> {
        self.pre("lstat").await?;
        let v = norm(&path);
        if self.shared.links.lock().unwrap().contains_key(&v) {
            return Ok(Attrs { id, attrs: Self::link_attrs() });
        }
        let real = self.real_of(&v)?;
        let md = tokio::fs::metadata(&real).await.map_err(|e| io_code(&e))?;
        Ok(Attrs { id, attrs: self.attrs_of(&real, &md) })
    }

    async fn stat(&mut self, id: u32, path: String) -> Res<Attrs> {
        self.pre("stat").await?;
        let (_, real) = self.resolve(&path, true)?;
        let md = tokio::fs::metadata(&real).await.map_err(|e| io_code(&e))?;
        Ok(Attrs { id, attrs: self.attrs_of(&real, &md) })
    }

    async fn fstat(&mut self, id: u32, handle: String) -> Res<Attrs> {
        self.pre("fstat").await?;
        use tokio::io::AsyncWriteExt;
        let real = match self.handles.get_mut(&handle) {
            Some(H::File { real, f, .. }) => {
                let _ = f.flush().await; // writes are not flushed one by one (speed); make them visible here
                real.clone()
            }
            _ => return Err(StatusCode::Failure),
        };
        let md = tokio::fs::metadata(&real).await.map_err(|e| io_code(&e))?;
        Ok(Attrs { id, attrs: self.attrs_of(&real, &md) })
    }

    async fn setstat(&mut self, id: u32, path: String, attrs: FileAttributes) -> Res<Status> {
        self.pre("setstat").await?;
        let (_, real) = self.resolve(&path, true)?;
        tokio::fs::metadata(&real).await.map_err(|e| io_code(&e))?;
        self.set_mode(&real, &attrs);
        Ok(ok_status(id))
    }

    async fn fsetstat(&mut self, id: u32, handle: String, attrs: FileAttributes) -> Res<Status> {
        self.pre("fsetstat").await?;
        let real = match self.handles.get(&handle) {
            Some(H::File { real, .. }) => real.clone(),
            _ => return Err(StatusCode::Failure),
        };
        self.set_mode(&real, &attrs);
        Ok(ok_status(id))
    }

    async fn opendir(&mut self, id: u32, path: String) -> Res<Handle> {
        self.pre("opendir").await?;
        let (v, real) = self.resolve(&path, true)?;
        let mut rd = tokio::fs::read_dir(&real).await.map_err(|e| io_code(&e))?;
        let mut entries = Vec::new();
        while let Some(e) = rd.next_entry().await.map_err(|e| io_code(&e))? {
            let name = e.file_name().to_string_lossy().into_owned();
            if let Ok(md) = e.metadata().await {
                entries.push(File::new(name, self.attrs_of(&e.path(), &md)));
            }
        }
        for (l, _) in self.shared.links.lock().unwrap().iter() {
            if parent_of(l) == v {
                entries.push(File::new(l.rsplit('/').next().unwrap_or("").to_string(), Self::link_attrs()));
            }
        }
        entries.sort_by(|a, b| a.filename.cmp(&b.filename));
        Ok(Handle { id, handle: self.new_handle(H::Dir { entries, pos: 0 }) })
    }

    async fn readdir(&mut self, id: u32, handle: String) -> Res<Name> {
        self.pre("readdir").await?;
        match self.handles.get_mut(&handle) {
            Some(H::Dir { entries, pos }) => {
                if *pos >= entries.len() {
                    return Err(StatusCode::Eof);
                }
                let end = (*pos + 100).min(entries.len()); // small pages on purpose
                let files = entries[*pos..end].to_vec();
                *pos = end;
                Ok(Name { id, files })
            }
            _ => Err(StatusCode::Failure),
        }
    }

    async fn open(&mut self, id: u32, filename: String, pflags: OpenFlags, attrs: FileAttributes) -> Res<Handle> {
        self.pre("open").await?;
        let (_, real) = self.resolve(&filename, true)?;
        let existed = tokio::fs::metadata(&real).await.is_ok();
        let mut oo = tokio::fs::OpenOptions::new();
        oo.read(pflags.contains(OpenFlags::READ)).write(pflags.contains(OpenFlags::WRITE));
        if pflags.contains(OpenFlags::CREATE) {
            if pflags.contains(OpenFlags::EXCLUDE) {
                oo.create_new(true);
            } else {
                oo.create(true);
            }
        }
        if pflags.contains(OpenFlags::TRUNCATE) {
            oo.truncate(true);
        }
        let f = oo.open(&real).await.map_err(|e| io_code(&e))?;
        if !existed {
            self.set_mode(&real, &attrs); // like OpenSSH: the mode applies to newly created files only
        }
        Ok(Handle { id, handle: self.new_handle(H::File { f, real, pos: 0 }) })
    }

    async fn close(&mut self, id: u32, handle: String) -> Res<Status> {
        self.pre("close").await?;
        if let Some(H::File { mut f, .. }) = self.handles.remove(&handle) {
            use tokio::io::AsyncWriteExt;
            let _ = f.flush().await;
        }
        Ok(ok_status(id))
    }

    async fn read(&mut self, id: u32, handle: String, offset: u64, len: u32) -> Res<Data> {
        self.pre("read").await?;
        use tokio::io::{AsyncReadExt, AsyncSeekExt};
        let Some(H::File { f, pos, .. }) = self.handles.get_mut(&handle) else { return Err(StatusCode::Failure) };
        if *pos != offset {
            f.seek(std::io::SeekFrom::Start(offset)).await.map_err(|e| io_code(&e))?;
        }
        let mut buf = vec![0u8; len.min(256 * 1024) as usize];
        let n = f.read(&mut buf).await.map_err(|e| io_code(&e))?;
        *pos = offset + n as u64;
        if n == 0 {
            return Err(StatusCode::Eof);
        }
        buf.truncate(n);
        Ok(Data { id, data: buf })
    }

    async fn write(&mut self, id: u32, handle: String, offset: u64, data: Vec<u8>) -> Res<Status> {
        self.pre("write").await?;
        use tokio::io::{AsyncSeekExt, AsyncWriteExt};
        let started = std::time::Instant::now();
        if let Some(prev) = *self.shared.faults.last_write_end.lock().unwrap() {
            self.shared.faults.write_gap_ns.fetch_add(started.duration_since(prev).as_nanos() as u64, Ordering::Relaxed);
        }
        let total = {
            let f = &self.shared.faults;
            f.written.fetch_add(data.len() as u64, Ordering::SeqCst) + data.len() as u64
        };
        {
            let Some(H::File { f, pos, .. }) = self.handles.get_mut(&handle) else { return Err(StatusCode::Failure) };
            if *pos != offset {
                f.seek(std::io::SeekFrom::Start(offset)).await.map_err(|e| io_code(&e))?;
            }
            f.write_all(&data).await.map_err(|e| io_code(&e))?;
            *pos = offset + data.len() as u64;
        }
        let faults = self.shared.faults.clone();
        faults.write_calls.fetch_add(1, Ordering::Relaxed);
        faults.write_busy_ns.fetch_add(started.elapsed().as_nanos() as u64, Ordering::Relaxed);
        *faults.last_write_end.lock().unwrap() = Some(std::time::Instant::now());
        let d = faults.drop_after_write_bytes.load(Ordering::SeqCst);
        if d > 0 && total >= d {
            faults.drop_after_write_bytes.store(0, Ordering::SeqCst);
            self.fault_kill(&format!("drop after {total} written bytes"));
            std::future::pending::<()>().await;
        }
        let s = faults.stall_after_write_bytes.load(Ordering::SeqCst);
        if s > 0 && total >= s {
            faults.stall_after_write_bytes.store(0, Ordering::SeqCst);
            faults.stalled.store(true, Ordering::SeqCst);
            self.shared.log(format!("FAULT: stalling after {total} written bytes"));
            std::future::pending::<()>().await;
        }
        Ok(ok_status(id))
    }

    async fn remove(&mut self, id: u32, filename: String) -> Res<Status> {
        self.pre("remove").await?;
        let v = norm(&filename);
        if self.shared.links.lock().unwrap().remove(&v).is_some() {
            return Ok(ok_status(id));
        }
        let real = self.real_of(&v)?;
        let md = tokio::fs::metadata(&real).await.map_err(|e| io_code(&e))?;
        if md.is_dir() {
            return Err(StatusCode::Failure);
        }
        tokio::fs::remove_file(&real).await.map_err(|e| io_code(&e))?;
        self.shared.modes.lock().unwrap().remove(&real);
        Ok(ok_status(id))
    }

    async fn mkdir(&mut self, id: u32, path: String, attrs: FileAttributes) -> Res<Status> {
        self.pre("mkdir").await?;
        let real = self.real_of(&norm(&path))?;
        tokio::fs::create_dir(&real).await.map_err(|e| io_code(&e))?;
        self.set_mode(&real, &attrs);
        Ok(ok_status(id))
    }

    async fn rmdir(&mut self, id: u32, path: String) -> Res<Status> {
        self.pre("rmdir").await?;
        let real = self.real_of(&norm(&path))?;
        tokio::fs::remove_dir(&real).await.map_err(|e| io_code(&e))?;
        Ok(ok_status(id))
    }

    async fn rename(&mut self, id: u32, oldpath: String, newpath: String) -> Res<Status> {
        self.pre("rename").await?;
        let (a, b) = (self.real_of(&norm(&oldpath))?, self.real_of(&norm(&newpath))?);
        // plain SFTP RENAME: OpenSSH refuses to replace an existing target
        if tokio::fs::symlink_metadata(&b).await.is_ok() {
            return Err(StatusCode::Failure);
        }
        tokio::fs::rename(&a, &b).await.map_err(|e| io_code(&e))?;
        self.move_mode(&a, &b);
        Ok(ok_status(id))
    }

    async fn readlink(&mut self, id: u32, path: String) -> Res<Name> {
        self.pre("readlink").await?;
        match self.shared.links.lock().unwrap().get(&norm(&path)) {
            Some(t) => Ok(Name { id, files: vec![File::dummy(t.clone())] }),
            None => Err(StatusCode::Failure),
        }
    }

    async fn extended(&mut self, id: u32, request: String, data: Vec<u8>) -> Res<Packet> {
        match request.as_str() {
            POSIX_RENAME if !self.shared.faults.hide_posix_rename.load(Ordering::SeqCst) => {
                self.pre("posix-rename").await?;
                let (a, b) = parse_two_strings(&data).ok_or(StatusCode::BadMessage)?;
                let (a, b) = (self.real_of(&norm(&a))?, self.real_of(&norm(&b))?);
                tokio::fs::rename(&a, &b).await.map_err(|e| io_code(&e))?; // replaces an existing file
                self.move_mode(&a, &b);
                Ok(Packet::Status(ok_status(id)))
            }
            FSYNC => {
                self.pre("fsync").await?;
                Ok(Packet::Status(ok_status(id)))
            }
            _ => Err(StatusCode::OpUnsupported),
        }
    }
}

impl Sftp {
    fn move_mode(&self, a: &Path, b: &Path) {
        let mut m = self.shared.modes.lock().unwrap();
        match m.remove(a) {
            Some(x) => {
                m.insert(b.to_path_buf(), x);
            }
            None => {
                m.remove(b);
            }
        }
    }
}

fn parse_two_strings(d: &[u8]) -> Option<(String, String)> {
    fn one(d: &[u8]) -> Option<(String, &[u8])> {
        let n = u32::from_be_bytes(d.get(..4)?.try_into().ok()?) as usize;
        let s = d.get(4..4 + n)?;
        Some((String::from_utf8(s.to_vec()).ok()?, &d[4 + n..]))
    }
    let (a, rest) = one(d)?;
    let (b, _) = one(rest)?;
    Some((a, b))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paths_are_clamped_at_the_root() {
        assert_eq!(norm("/a/../../etc/passwd"), "/etc/passwd");
        assert_eq!(norm("../.."), "/");
        assert_eq!(norm("a//b/./c"), "/a/b/c");
        assert_eq!(norm("/a\\..\\b"), "/b");
        assert_eq!(parent_of("/a/b"), "/a");
        assert_eq!(parent_of("/a"), "/");
    }

    #[test]
    fn two_strings_are_parsed() {
        let mut d = Vec::new();
        for s in ["/x/a", "/x/b"] {
            d.extend_from_slice(&(s.len() as u32).to_be_bytes());
            d.extend_from_slice(s.as_bytes());
        }
        assert_eq!(parse_two_strings(&d), Some(("/x/a".into(), "/x/b".into())));
        assert_eq!(parse_two_strings(&d[..6]), None);
    }
}
