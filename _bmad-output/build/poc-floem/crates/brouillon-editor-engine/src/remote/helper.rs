//! `HelperProvider`: the real SFTP provider. It speaks the helper protocol (see
//! `docs/REMOTE_HELPER_PROTOCOL.md`) through a [`HelperClient`].
//!
//! * [`HelperProvider`] implements the hub's [`RemoteProvider`] (explorer, plain upload).
//! * [`HelperFs`] implements the safe-save engine's `RemoteFs` over the SAME session
//!   (`RemoteProvider::safe_fs`).
//! * [`HelperFactory`] spawns `brouillon-remote.exe` next to the editor, says it in plain words
//!   when it is missing, and answers `prompt` events through a [`PromptBroker`] (the UI asks the
//!   user; the answer is used once and never stored here).
//! * [`RealOrDemoFactory`] keeps the in-memory fake only for host `demo` / `BROUILLON_REMOTE_FAKE=1`.
//!
//! Every method here is blocking and must run on a worker thread. A dead helper surfaces as
//! `BrokenPipe` / `ConnectionReset` (io) or `FsError::Transport` (saver): the explorer then marks
//! the site Offline and the saver queues the document.

use std::collections::{HashMap, VecDeque};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{channel, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime};

use brouillon_remote_proto::{ConnectParams, CreateMode, EntryKind, ErrorKind, HostKeyPolicy, PromptKind, RemotePath as WirePath, Request, Secret};
use brouillon_safesave::{Caps, FsError, FsResult, Kind, RemoteFs, Stat};

use super::client::{CallError, HelperClient, PromptHandler, Timeouts};
use super::error::{RemoteError, RemoteErrorKind};
use super::hub::{ProviderFactory, MAX_READ_BYTES};
use super::path::RemotePath;
use super::provider::{OpenStamp, RemoteProvider, SharedProvider};
use super::sites::{AuthMethod, SiteProfile};
use crate::fs_provider::{sort_entries, FsEntry, FsProvider};

// ---------------------------------------------------------------- error / path mapping

fn wire_path(p: &Path) -> io::Result<WirePath> {
    let s = p.to_string_lossy().replace('\\', "/");
    WirePath::parse(&s).map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e.to_string()))
}

fn wire_rp(p: &RemotePath) -> io::Result<WirePath> {
    WirePath::parse(p.as_str()).map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e.to_string()))
}

fn wire_str(p: &str) -> FsResult<WirePath> {
    WirePath::parse(p).map_err(|e| FsError::Other(e.to_string()))
}

/// The io error the explorer's bucket mapping (`RemoteError::from_io`) understands.
pub fn call_to_io(e: CallError) -> io::Error {
    use io::ErrorKind as K;
    match e {
        CallError::Dead(why) => io::Error::new(K::BrokenPipe, why),
        CallError::Timeout => io::Error::new(K::TimedOut, "the server did not answer in time"),
        CallError::Invalid(m) => io::Error::new(K::InvalidInput, m),
        CallError::Remote(r) => {
            let kind = match r.kind {
                ErrorKind::NotFound => K::NotFound,
                ErrorKind::PermissionDenied => K::PermissionDenied,
                ErrorKind::AlreadyExists => K::AlreadyExists,
                ErrorKind::ConnectionLost | ErrorKind::Protocol => K::ConnectionReset,
                ErrorKind::NotConnected => K::NotConnected,
                ErrorKind::Timeout => K::TimedOut,
                ErrorKind::ConnectFailed => K::ConnectionRefused,
                ErrorKind::Unsupported => K::Unsupported,
                ErrorKind::TooLarge | ErrorKind::LimitExceeded => K::FileTooLarge,
                _ => K::Other,
            };
            io::Error::new(kind, r.message)
        }
    }
}

/// Saver-side classification: network class errors are `Transport` (the saver queues the document).
pub fn call_to_fs(e: CallError) -> FsError {
    if e.is_transport() {
        return FsError::Transport(e.to_string());
    }
    match e {
        CallError::Remote(r) => match r.kind {
            ErrorKind::NotFound => FsError::NotFound,
            ErrorKind::AlreadyExists => FsError::Exists,
            ErrorKind::PermissionDenied => FsError::PermissionDenied,
            _ => {
                let low = r.message.to_lowercase();
                if low.contains("no space") || low.contains("quota") || low.contains("disk full") {
                    FsError::NoSpace
                } else {
                    FsError::Other(format!("{}: {}", format!("{:?}", r.kind).to_lowercase(), r.message))
                }
            }
        },
        other => FsError::Other(other.to_string()),
    }
}

/// Connect-time error in the explorer's plain-language buckets.
pub fn connect_error(e: &CallError) -> RemoteError {
    match e {
        CallError::Remote(r) => {
            let kind = match r.kind {
                ErrorKind::NoSsh => RemoteErrorKind::NoSsh,
                ErrorKind::AuthFailed => RemoteErrorKind::AuthFailed,
                ErrorKind::HostKeyChanged => RemoteErrorKind::HostKeyChanged,
                ErrorKind::HostKeyRejected => RemoteErrorKind::HostKeyRejected,
                ErrorKind::ConnectFailed | ErrorKind::Timeout => RemoteErrorKind::Unreachable,
                ErrorKind::ConnectionLost | ErrorKind::NotConnected => RemoteErrorKind::Dropped,
                _ => RemoteErrorKind::Other,
            };
            RemoteError::new(kind, r.message.clone())
        }
        CallError::Dead(why) => RemoteError::new(RemoteErrorKind::Dropped, why.clone()),
        CallError::Timeout => RemoteError::new(RemoteErrorKind::Unreachable, "no answer in time"),
        CallError::Invalid(m) => RemoteError::new(RemoteErrorKind::Other, m.clone()),
    }
}

fn secs_to_time(s: Option<i64>) -> Option<SystemTime> {
    s.filter(|s| *s >= 0).map(|s| SystemTime::UNIX_EPOCH + Duration::from_secs(s as u64))
}

// ---------------------------------------------------------------- provider

pub struct HelperProvider {
    client: Arc<HelperClient>,
    posix_rename: bool,
    stamps: Mutex<HashMap<String, OpenStamp>>,
}

/// Symlinks to folders must be recognisable in a listing; bounded so a hostile folder of links stays cheap.
const MAX_LINK_PROBES: usize = 64;

impl HelperProvider {
    /// `client` must already be connected (hello + connect done). Tests build the client themselves.
    pub fn new(client: Arc<HelperClient>, posix_rename: bool) -> Self {
        Self { client, posix_rename, stamps: Mutex::new(HashMap::new()) }
    }

    pub fn client(&self) -> &Arc<HelperClient> {
        &self.client
    }

    pub fn is_dead(&self) -> bool {
        self.client.is_dead()
    }

    /// Hello + connect on a fresh client; the client is killed on failure.
    pub fn establish(client: Arc<HelperClient>, params: ConnectParams) -> Result<Self, RemoteError> {
        let r = client.hello().map_err(|e| connect_error(&e)).and_then(|_| client.connect(params).map_err(|e| connect_error(&e)));
        match r {
            Ok(c) => Ok(Self::new(client, c.posix_rename)),
            Err(e) => {
                client.kill("connect failed");
                Err(e)
            }
        }
    }

    fn stat_wire(&self, p: &Path, follow: bool) -> io::Result<brouillon_remote_proto::StatResult> {
        self.client.stat(wire_path(p)?, follow).map_err(call_to_io)
    }
}

impl FsProvider for HelperProvider {
    fn id(&self) -> &str {
        "sftp"
    }

    fn list_dir(&self, dir: &Path) -> io::Result<Vec<FsEntry>> {
        let base = wire_path(dir)?;
        let (entries, _) = self.client.list_dir(base.clone()).map_err(call_to_io)?;
        let mut out = Vec::with_capacity(entries.len());
        let mut probes = 0;
        for e in entries {
            let name = e.name.to_string();
            let Ok(child) = base.join(&e.name) else { continue };
            let mut is_dir = e.kind == EntryKind::Dir;
            let is_symlink = e.kind == EntryKind::Symlink;
            if is_symlink && probes < MAX_LINK_PROBES {
                probes += 1;
                if let Ok(st) = self.client.stat(child.clone(), true) {
                    is_dir = st.kind == EntryKind::Dir;
                }
            }
            out.push(FsEntry {
                name,
                path: PathBuf::from(child.as_wire_str()),
                is_dir,
                size: if is_dir { 0 } else { e.size.unwrap_or(0) },
                mtime: secs_to_time(e.mtime),
                mode: e.mode,
                is_symlink,
            });
        }
        sort_entries(&mut out);
        Ok(out)
    }

    fn stat(&self, path: &Path) -> io::Result<FsEntry> {
        let st = self.stat_wire(path, true)?;
        let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "/".into());
        let is_dir = st.kind == EntryKind::Dir;
        Ok(FsEntry { mtime: secs_to_time(st.mtime), mode: st.mode, ..FsEntry::new(name, path, is_dir, st.size.unwrap_or(0)) })
    }

    fn read(&self, path: &Path) -> io::Result<Vec<u8>> {
        // stat / read / stat: when both stats agree with the bytes we got, that stamp is a sound base for a later safe save.
        let before = self.stat_wire(path, true).ok();
        let bytes = self.client.read(wire_path(path)?, Some(MAX_READ_BYTES as u64)).map_err(call_to_io)?;
        if let Some(b) = before {
            if let Ok(a) = self.stat_wire(path, true) {
                let same = (b.size, b.mtime) == (a.size, a.mtime) && a.size == Some(bytes.len() as u64);
                if same {
                    if let (Some(size), Some(mtime)) = (a.size, a.mtime) {
                        if let Ok(mut m) = self.stamps.lock() {
                            m.insert(RemotePath::new(&path.to_string_lossy()).to_string(), OpenStamp { size, mtime, mode: a.mode.unwrap_or(0o644) & 0o7777 });
                        }
                    }
                }
            }
        }
        Ok(bytes)
    }

    fn write(&self, path: &Path, data: &[u8]) -> io::Result<()> {
        self.client.write(wire_path(path)?, data, CreateMode::Truncate, None, false).map_err(call_to_io)
    }
}

impl RemoteProvider for HelperProvider {
    fn mkdir(&self, path: &RemotePath) -> io::Result<()> {
        self.client.quick(Request::Mkdir { path: wire_rp(path)?, mode: None }).map(|_| ()).map_err(call_to_io)
    }

    fn rename(&self, from: &RemotePath, to: &RemotePath) -> io::Result<()> {
        self.client.quick(Request::Rename { from: wire_rp(from)?, to: wire_rp(to)?, overwrite: false, allow_nonatomic: false }).map(|_| ()).map_err(call_to_io)
    }

    fn remove(&self, path: &RemotePath, is_dir: bool) -> io::Result<()> {
        let p = wire_rp(path)?;
        let req = if is_dir { Request::Rmdir { path: p } } else { Request::Remove { path: p } };
        self.client.quick(req).map(|_| ()).map_err(call_to_io)
    }

    fn safe_fs(&self) -> Option<Box<dyn RemoteFs + Send>> {
        Some(Box::new(HelperFs { client: self.client.clone(), posix_rename: self.posix_rename }))
    }

    fn take_open_stamp(&self, path: &RemotePath) -> Option<OpenStamp> {
        self.stamps.lock().ok().and_then(|mut m| m.remove(path.as_str()))
    }
}

// ---------------------------------------------------------------- RemoteFs for the SafeSaver

pub struct HelperFs {
    client: Arc<HelperClient>,
    posix_rename: bool,
}

impl HelperFs {
    pub fn new(client: Arc<HelperClient>, posix_rename: bool) -> Self {
        Self { client, posix_rename }
    }
}

fn kind_of(k: EntryKind) -> Kind {
    match k {
        EntryKind::File => Kind::File,
        EntryKind::Dir => Kind::Dir,
        EntryKind::Symlink => Kind::Symlink,
        EntryKind::Other => Kind::Other,
    }
}

impl RemoteFs for HelperFs {
    fn caps(&self) -> Caps {
        // The protocol has no server-side hash: the saver verifies size (and re-reads when it must).
        Caps { posix_rename: self.posix_rename, hash: false, set_mode: true }
    }

    fn stat(&mut self, path: &str) -> FsResult<Option<Stat>> {
        match self.client.stat(wire_str(path)?, false) {
            Ok(s) => Ok(Some(Stat { kind: kind_of(s.kind), size: s.size.unwrap_or(0), mtime: s.mtime.unwrap_or(0), mode: s.mode.unwrap_or(0o644) & 0o7777 })),
            Err(CallError::Remote(r)) if r.kind == ErrorKind::NotFound => Ok(None),
            Err(e) => Err(call_to_fs(e)),
        }
    }

    fn realpath(&mut self, path: &str) -> FsResult<String> {
        let v = self.client.quick(Request::Realpath { path: wire_str(path)? }).map_err(call_to_fs)?;
        v.get("path").and_then(|p| p.as_str()).map(str::to_string).ok_or_else(|| FsError::Other("realpath: unexpected answer".into()))
    }

    fn read(&mut self, path: &str) -> FsResult<Vec<u8>> {
        let cap = self.client.limits().max_read_bytes;
        self.client.read(wire_str(path)?, Some(cap)).map_err(call_to_fs)
    }

    fn write_new(&mut self, path: &str, data: &[u8], mode: Option<u32>) -> FsResult<()> {
        self.client.write(wire_str(path)?, data, CreateMode::New, mode, true).map_err(call_to_fs)
    }

    fn write_truncate(&mut self, path: &str, data: &[u8]) -> FsResult<()> {
        self.client.write(wire_str(path)?, data, CreateMode::Truncate, None, true).map_err(call_to_fs)
    }

    fn set_mode(&mut self, path: &str, mode: u32) -> FsResult<()> {
        self.client.quick(Request::Chmod { path: wire_str(path)?, mode }).map(|_| ()).map_err(call_to_fs)
    }

    fn rename(&mut self, from: &str, to: &str) -> FsResult<()> {
        self.client.quick(Request::Rename { from: wire_str(from)?, to: wire_str(to)?, overwrite: false, allow_nonatomic: false }).map(|_| ()).map_err(call_to_fs)
    }

    fn rename_replace(&mut self, from: &str, to: &str) -> FsResult<()> {
        // Atomic only: `allow_nonatomic` stays false, the saver decides what to do without posix-rename.
        self.client.quick(Request::Rename { from: wire_str(from)?, to: wire_str(to)?, overwrite: true, allow_nonatomic: false }).map(|_| ()).map_err(call_to_fs)
    }

    fn remove(&mut self, path: &str) -> FsResult<()> {
        self.client.quick(Request::Remove { path: wire_str(path)? }).map(|_| ()).map_err(call_to_fs)
    }
}

// ---------------------------------------------------------------- prompts (editor side)

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AskKind {
    HostKey,
    Passphrase,
    Password,
    KeyboardInteractive,
}

impl AskKind {
    /// Text typed here is a secret: show dots, never echo.
    pub fn is_secret(self) -> bool {
        !matches!(self, AskKind::HostKey)
    }
}

impl From<PromptKind> for AskKind {
    fn from(k: PromptKind) -> Self {
        match k {
            PromptKind::HostKey => AskKind::HostKey,
            PromptKind::Passphrase => AskKind::Passphrase,
            PromptKind::Password => AskKind::Password,
            PromptKind::KeyboardInteractive => AskKind::KeyboardInteractive,
        }
    }
}

/// One question for the user. `text` is ssh's own (sanitised) prompt, shown verbatim.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AskView {
    pub id: u64,
    pub site_id: String,
    pub site_name: String,
    pub kind: AskKind,
    pub text: String,
}

/// The vault side of a question (implemented by `Credentials`). `resolve` may answer a password or
/// passphrase question from a saved secret WITHOUT showing a dialog; `typed` sees what the user typed
/// so it can offer to keep it after a successful connect. Neither is called for host-key or
/// keyboard-interactive questions.
pub trait PromptVault: Send + Sync {
    fn resolve(&self, site_id: &str, kind: AskKind) -> Option<String>;
    fn typed(&self, site_id: &str, kind: AskKind, answer: &str);
}

struct AskEntry {
    view: AskView,
    tx: Sender<Option<String>>,
}

#[derive(Default)]
struct BrokerState {
    next: u64,
    queue: VecDeque<AskEntry>,
}

/// Hand-over point between helper threads (which block waiting for an answer) and the UI thread
/// (which shows one question at a time). The answer is moved to the waiting thread and dropped;
/// the broker keeps no copy.
#[derive(Default)]
pub struct PromptBroker {
    st: Mutex<BrokerState>,
    wake: Mutex<Option<Arc<dyn Fn() + Send + Sync>>>,
    vault: Mutex<Option<Arc<dyn PromptVault>>>,
}

impl PromptBroker {
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    pub fn set_waker(&self, f: impl Fn() + Send + Sync + 'static) {
        if let Ok(mut w) = self.wake.lock() {
            *w = Some(Arc::new(f));
        }
    }

    /// Plug in the saved-password service. Without it every question is shown (use this time only).
    pub fn set_vault(&self, v: Arc<dyn PromptVault>) {
        if let Ok(mut w) = self.vault.lock() {
            *w = Some(v);
        }
    }

    fn vault(&self) -> Option<Arc<dyn PromptVault>> {
        self.vault.lock().ok().and_then(|v| v.clone())
    }

    fn wake(&self) {
        let f = self.wake.lock().ok().and_then(|w| w.clone());
        if let Some(f) = f {
            f();
        }
    }

    /// UI thread: the question to show now (the oldest), if any.
    pub fn current(&self) -> Option<AskView> {
        self.st.lock().ok().and_then(|s| s.queue.front().map(|e| e.view.clone()))
    }

    /// UI thread: answer a question (`None` = the user cancelled). `false` = it was no longer pending.
    pub fn answer(&self, id: u64, answer: Option<String>) -> bool {
        let e = {
            let Ok(mut s) = self.st.lock() else { return false };
            s.queue.iter().position(|e| e.view.id == id).and_then(|i| s.queue.remove(i))
        };
        match e {
            Some(e) => {
                if let (Some(a), true) = (&answer, matches!(e.view.kind, AskKind::Password | AskKind::Passphrase)) {
                    if let Some(v) = self.vault() {
                        v.typed(&e.view.site_id, e.view.kind, a);
                    }
                }
                let _ = e.tx.send(answer);
                true
            }
            None => false,
        }
    }

    /// Helper thread: block until the user answered, the helper went away (`gone`) or `max_wait` passed.
    pub fn ask(&self, site_id: &str, site_name: &str, kind: AskKind, text: &str, gone: &AtomicBool, max_wait: Duration) -> Option<String> {
        if matches!(kind, AskKind::Password | AskKind::Passphrase) {
            if let Some(a) = self.vault().and_then(|v| v.resolve(site_id, kind)) {
                return Some(a);
            }
        }
        let (tx, rx) = channel();
        let id = {
            let Ok(mut s) = self.st.lock() else { return None };
            s.next += 1;
            let id = s.next;
            s.queue.push_back(AskEntry { view: AskView { id, site_id: site_id.into(), site_name: site_name.into(), kind, text: text.into() }, tx });
            id
        };
        self.wake();
        let started = Instant::now();
        loop {
            match rx.recv_timeout(Duration::from_millis(250)) {
                Ok(a) => return a,
                Err(RecvTimeoutError::Timeout) => {
                    if gone.load(Ordering::SeqCst) || started.elapsed() > max_wait {
                        self.withdraw(id);
                        return None;
                    }
                }
                Err(RecvTimeoutError::Disconnected) => return None,
            }
        }
    }

    fn withdraw(&self, id: u64) {
        if let Ok(mut s) = self.st.lock() {
            s.queue.retain(|e| e.view.id != id);
        }
        self.wake();
    }
}

struct SitePrompts {
    broker: Arc<PromptBroker>,
    site_id: String,
    site_name: String,
}

impl PromptHandler for SitePrompts {
    fn ask(&self, kind: PromptKind, text: &str, gone: &AtomicBool) -> Option<Secret> {
        self.broker.ask(&self.site_id, &self.site_name, kind.into(), text, gone, Duration::from_secs(600)).map(Secret::new)
    }
}

// ---------------------------------------------------------------- spawning the helper

#[cfg(windows)]
const HELPER_NAME: &str = "brouillon-remote.exe";
#[cfg(not(windows))]
const HELPER_NAME: &str = "brouillon-remote";

/// `BROUILLON_REMOTE_HELPER` (developer override) or the helper next to the editor executable.
pub fn helper_path() -> Result<PathBuf, RemoteError> {
    let p = match std::env::var_os("BROUILLON_REMOTE_HELPER") {
        Some(p) => PathBuf::from(p),
        None => std::env::current_exe().ok().and_then(|e| e.parent().map(|d| d.join(HELPER_NAME))).unwrap_or_else(|| PathBuf::from(HELPER_NAME)),
    };
    if p.is_file() {
        Ok(p)
    } else {
        Err(RemoteError::new(RemoteErrorKind::HelperMissing, format!("{} not found", p.display())))
    }
}

/// Start the helper process and wrap it in a client (no hello yet).
pub fn spawn_helper(exe: &Path, prompts: Arc<dyn PromptHandler>, timeouts: Timeouts, log: Arc<dyn Fn(&str) + Send + Sync>) -> io::Result<HelperClient> {
    use std::io::Read;
    use std::process::{Command, Stdio};
    let mut cmd = Command::new(exe);
    cmd.arg("serve").stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    }
    let mut child = cmd.spawn()?;
    let stdin = child.stdin.take().ok_or_else(|| io::Error::other("no stdin"))?;
    let stdout = child.stdout.take().ok_or_else(|| io::Error::other("no stdout"))?;
    if let Some(mut err) = child.stderr.take() {
        let log = log.clone();
        let _ = std::thread::Builder::new().name("brouillon-helper-stderr".into()).spawn(move || {
            // Diagnostics only (the protocol says no secret ever appears here). Bounded.
            let mut buf = [0u8; 1024];
            let mut total = 0usize;
            while let Ok(n) = err.read(&mut buf) {
                if n == 0 || total > 64 * 1024 {
                    break;
                }
                total += n;
                log(&format!("helper: {}", String::from_utf8_lossy(&buf[..n]).trim()));
            }
        });
    }
    let killer: Box<dyn FnOnce() + Send> = Box::new(move || {
        // The client already closed stdin, which makes the helper kill its ssh child and exit. Give it a moment, then end it.
        let _ = std::thread::Builder::new().name("brouillon-helper-reaper".into()).spawn(move || {
            let t = Instant::now();
            while t.elapsed() < Duration::from_secs(3) {
                if matches!(child.try_wait(), Ok(Some(_))) {
                    return;
                }
                std::thread::sleep(Duration::from_millis(50));
            }
            let _ = child.kill();
            let _ = child.wait();
        });
    });
    Ok(HelperClient::new(Box::new(stdout), Box::new(stdin), killer, prompts, timeouts, log))
}

/// Home-relative key paths (`~/.ssh/id_ed25519`) are expanded here so the helper only sees a real path.
fn expand_tilde(p: &str) -> String {
    match (p.strip_prefix("~/"), dirs::home_dir()) {
        (Some(rest), Some(h)) => h.join(rest).to_string_lossy().into_owned(),
        _ => p.to_string(),
    }
}

/// Connect parameters for a profile. Host keys are the editor's decision: `ask` (the helper emits a
/// `host_key` prompt; a CHANGED key is a hard failure with no override).
pub fn connect_params(profile: &SiteProfile) -> ConnectParams {
    ConnectParams {
        host: profile.host.clone(),
        port: Some(profile.port),
        user: Some(profile.user.clone()),
        identity_file: match &profile.auth {
            AuthMethod::KeyFile { path } => Some(expand_tilde(path)),
            _ => None,
        },
        proxy_jump: None,
        host_key_policy: HostKeyPolicy::Ask,
        known_hosts_file: None,
        ssh_config_file: None,
        interactive: true,
        connect_timeout_s: Some(20),
        ssh_path: None,
        root: None,
        protocol: brouillon_remote_proto::Protocol::Sftp,
        ftp: None,
    }
}

pub struct HelperFactory {
    exe: Option<PathBuf>,
    prompts: Arc<PromptBroker>,
    timeouts: Timeouts,
    log: Arc<dyn Fn(&str) + Send + Sync>,
}

impl HelperFactory {
    pub fn new(prompts: Arc<PromptBroker>) -> Self {
        Self { exe: None, prompts, timeouts: Timeouts::default(), log: Arc::new(|m| eprintln!("[remote] {m}")) }
    }

    pub fn with_exe(mut self, exe: PathBuf) -> Self {
        self.exe = Some(exe);
        self
    }
}

impl ProviderFactory for HelperFactory {
    fn connect(&self, profile: &SiteProfile) -> Result<SharedProvider, RemoteError> {
        if profile.protocol != super::sites::Protocol::Sftp {
            return Err(RemoteError::new(RemoteErrorKind::Unsupported, "only SFTP is available for now"));
        }
        let exe = match &self.exe {
            Some(e) if e.is_file() => e.clone(),
            Some(e) => return Err(RemoteError::new(RemoteErrorKind::HelperMissing, format!("{} not found", e.display()))),
            None => helper_path()?,
        };
        let prompts: Arc<dyn PromptHandler> = Arc::new(SitePrompts { broker: self.prompts.clone(), site_id: profile.id.clone(), site_name: profile.name.clone() });
        let client = spawn_helper(&exe, prompts, self.timeouts.clone(), self.log.clone()).map_err(|e| RemoteError::new(RemoteErrorKind::HelperMissing, format!("could not start the helper: {e}")))?;
        let p = HelperProvider::establish(Arc::new(client), connect_params(profile))?;
        Ok(Arc::new(p))
    }
}

/// Real SFTP for real hosts; the in-memory fake only for host `demo` or `BROUILLON_REMOTE_FAKE=1`.
pub struct RealOrDemoFactory {
    real: Arc<dyn ProviderFactory>,
    demo: super::fake::DemoFactory,
    any_host_fake: bool,
    /// Dev-only (`BROUILLON_REMOTE_FAKE_ASK_PASSWORD=1`): the demo connect asks for a password through
    /// the real prompt path, so the "Keep this password?" card can be reached without an SSH server.
    ask_password: Option<Arc<PromptBroker>>,
}

impl RealOrDemoFactory {
    pub fn new(real: Arc<dyn ProviderFactory>, any_host_fake: bool, latency: Option<Duration>) -> Self {
        Self { real, demo: super::fake::DemoFactory::new(true, latency), any_host_fake, ask_password: None }
    }

    pub fn from_env(real: Arc<dyn ProviderFactory>) -> Self {
        let any = std::env::var("BROUILLON_REMOTE_FAKE").map(|v| v == "1").unwrap_or(false);
        let latency = std::env::var("BROUILLON_REMOTE_FAKE_LATENCY_MS").ok().and_then(|v| v.parse().ok()).map(Duration::from_millis);
        let mut s = Self::new(real, any, latency);
        s.demo = s.demo.with_dev_hooks_from_env();
        s
    }

    /// Dev hook: every demo connect first asks for a password on `broker` (a saved one answers silently,
    /// exactly like a real site). `None` / not called: the fake never asks.
    pub fn with_demo_password_prompt(mut self, broker: Option<Arc<PromptBroker>>) -> Self {
        self.ask_password = broker;
        self
    }

    /// True when `BROUILLON_REMOTE_FAKE_ASK_PASSWORD=1` (dev builds only; ignored by release builds).
    pub fn ask_password_env() -> bool {
        // Only a debug build or one built with `test-input` honours it: a shipped build ignores the variable.
        #[cfg(any(debug_assertions, feature = "test-input"))]
        {
            std::env::var("BROUILLON_REMOTE_FAKE_ASK_PASSWORD").map(|v| v == "1").unwrap_or(false)
        }
        #[cfg(not(any(debug_assertions, feature = "test-input")))]
        {
            false
        }
    }

    pub fn is_demo(&self, profile: &SiteProfile) -> bool {
        self.any_host_fake || profile.host == "demo"
    }
}

impl ProviderFactory for RealOrDemoFactory {
    fn connect(&self, profile: &SiteProfile) -> Result<SharedProvider, RemoteError> {
        if self.is_demo(profile) {
            if let Some(b) = &self.ask_password {
                let gone = AtomicBool::new(false);
                let answer = b.ask(&profile.id, &profile.name, AskKind::Password, "Password for the demo server (any text works)", &gone, Duration::from_secs(120));
                if answer.is_none() {
                    return Err(RemoteError::new(RemoteErrorKind::Unreachable, "sign-in cancelled"));
                }
            }
            self.demo.connect(profile)
        } else {
            self.real.connect(profile)
        }
    }
}
