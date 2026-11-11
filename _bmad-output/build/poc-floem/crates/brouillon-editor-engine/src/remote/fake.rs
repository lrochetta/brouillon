//! `FakeRemoteProvider`: an in-memory "server" for tests and for the developer flag.
//!
//! It never opens a socket. It can be slowed down (`set_latency`), made to fail
//! (`fail_path`, `fail_next`), and can refuse calls made on a chosen thread (`forbid_thread`) so a
//! test can prove that the UI thread never talks to the server.

use std::collections::{BTreeMap, BTreeSet};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::ThreadId;
use std::time::{Duration, SystemTime};

use super::path::RemotePath;
use super::provider::{OpenStamp, RemoteProvider};
use brouillon_safesave::{Caps, FsError, FsResult, Kind, RemoteFs, Stat};
use crate::fs_provider::{FsEntry, FsProvider};

#[derive(Clone, Debug, Default)]
pub struct FakeConfig {
    pub latency: Duration,
}

#[derive(Clone)]
struct Node {
    is_dir: bool,
    data: Vec<u8>,
    mtime: SystemTime,
    mode: u32,
    is_symlink: bool,
}

#[derive(Default)]
struct Inner {
    nodes: BTreeMap<String, Node>,
    cfg: FakeConfig,
    fail_paths: BTreeMap<String, io::ErrorKind>,
    fail_next: Vec<io::ErrorKind>,
    raw_names: BTreeMap<String, Vec<String>>,
    forbidden: Option<ThreadId>,
    /// Dev hook (see [`FakeRemoteProvider::enable_safe`]): behave like a provider the safe saver can drive.
    safe: bool,
    /// Where the one-shot "someone else changed the file" trigger file is looked for.
    trigger_dir: Option<PathBuf>,
    stamps: BTreeMap<String, OpenStamp>,
}

#[derive(Default)]
pub struct FakeRemoteProvider {
    inner: Arc<Mutex<Inner>>,
    calls: AtomicUsize,
    forbidden_calls: AtomicUsize,
    concurrent: AtomicUsize,
    max_concurrent: AtomicUsize,
}

struct Guard<'a>(&'a FakeRemoteProvider);
impl Drop for Guard<'_> {
    fn drop(&mut self) {
        self.0.concurrent.fetch_sub(1, Ordering::SeqCst);
    }
}

fn key(p: &Path) -> String {
    RemotePath::new(&p.to_string_lossy()).to_string()
}

impl FakeRemoteProvider {
    pub fn new() -> Arc<Self> {
        let f = Self::default();
        f.inner.lock().unwrap().nodes.insert("/".into(), Node { is_dir: true, data: vec![], mtime: SystemTime::now(), mode: 0o755, is_symlink: false });
        Arc::new(f)
    }

    /// A small web server tree: `/var/www` with a few files and folders, `/etc`, a read-only file.
    pub fn demo() -> Arc<Self> {
        let f = Self::new();
        for d in ["/var", "/var/www", "/var/www/css", "/var/www/img", "/var/log", "/etc", "/home", "/home/deploy"] {
            f.put_dir(d);
        }
        f.put_file("/var/www/index.php", b"<?php\necho 'hello from the demo server';\n");
        f.put_file("/var/www/css/style.css", b"body { margin: 0; }\n");
        f.put_file("/var/www/img/logo.txt", b"not really an image\n");
        f.put_file("/var/www/robots.txt", b"User-agent: *\nDisallow:\n");
        f.put_file("/var/log/app.log", b"started\n");
        f.put_file("/etc/hostname", b"demo\n");
        f.put_file_mode("/var/www/.htaccess", b"Options -Indexes\n", 0o444);
        f.put_file("/home/deploy/notes.md", b"# notes\n");
        f
    }

    pub fn put_dir(&self, path: &str) {
        let mut g = self.inner.lock().unwrap();
        g.nodes.insert(RemotePath::new(path).to_string(), Node { is_dir: true, data: vec![], mtime: SystemTime::now(), mode: 0o755, is_symlink: false });
    }

    pub fn put_file(&self, path: &str, data: &[u8]) {
        self.put_file_mode(path, data, 0o644);
    }

    pub fn put_file_mode(&self, path: &str, data: &[u8], mode: u32) {
        let mut g = self.inner.lock().unwrap();
        g.nodes.insert(RemotePath::new(path).to_string(), Node { is_dir: false, data: data.to_vec(), mtime: SystemTime::now(), mode, is_symlink: false });
    }

    pub fn put_symlink(&self, path: &str) {
        let mut g = self.inner.lock().unwrap();
        g.nodes.insert(RemotePath::new(path).to_string(), Node { is_dir: false, data: vec![], mtime: SystemTime::now(), mode: 0o777, is_symlink: true });
    }

    pub fn file_text(&self, path: &str) -> Option<String> {
        let g = self.inner.lock().unwrap();
        g.nodes.get(&RemotePath::new(path).to_string()).filter(|n| !n.is_dir).map(|n| String::from_utf8_lossy(&n.data).into_owned())
    }

    pub fn exists(&self, path: &str) -> bool {
        self.inner.lock().unwrap().nodes.contains_key(&RemotePath::new(path).to_string())
    }

    /// Dev/smoke hook: make this fake a "safe" provider (exclusive create, atomic replace, server hash,
    /// open stamps), so Ctrl+S goes through the same verified-save engine as a real SFTP site and a
    /// conflict can be provoked. With `trigger_dir`, a file `conflict-once.txt` dropped there
    /// (first line = remote path, rest = the new server text) changes that file on the "server"
    /// the next time the saver looks at it, exactly once, as if another person had saved it.
    pub fn enable_safe(&self, trigger_dir: Option<PathBuf>) {
        let mut g = self.inner.lock().unwrap();
        g.safe = true;
        g.trigger_dir = trigger_dir;
    }

    pub fn set_latency(&self, d: Duration) {
        self.inner.lock().unwrap().cfg.latency = d;
    }

    /// Every call on `path` (exact match) fails with `kind` until cleared.
    pub fn fail_path(&self, path: &str, kind: io::ErrorKind) {
        self.inner.lock().unwrap().fail_paths.insert(RemotePath::new(path).to_string(), kind);
    }

    pub fn clear_failures(&self) {
        let mut g = self.inner.lock().unwrap();
        g.fail_paths.clear();
        g.fail_next.clear();
    }

    /// The next `n` calls (any path) fail with `kind`, then calls succeed again.
    pub fn fail_next(&self, n: usize, kind: io::ErrorKind) {
        let mut g = self.inner.lock().unwrap();
        for _ in 0..n {
            g.fail_next.push(kind);
        }
    }

    /// A listing of `dir` will also contain an entry called `name` verbatim (hostile server).
    pub fn inject_raw_entry(&self, dir: &str, name: &str) {
        self.inner.lock().unwrap().raw_names.entry(RemotePath::new(dir).to_string()).or_default().push(name.to_string());
    }

    /// Calls made on this thread are counted in [`Self::forbidden_calls`].
    pub fn forbid_thread(&self, id: ThreadId) {
        self.inner.lock().unwrap().forbidden = Some(id);
    }

    pub fn calls(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }

    pub fn forbidden_calls(&self) -> usize {
        self.forbidden_calls.load(Ordering::SeqCst)
    }

    pub fn max_concurrent(&self) -> usize {
        self.max_concurrent.load(Ordering::SeqCst)
    }

    /// Common prologue: count, track concurrency, sleep, apply configured failures.
    fn enter(&self, path: &str) -> io::Result<Guard<'_>> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        let n = self.concurrent.fetch_add(1, Ordering::SeqCst) + 1;
        self.max_concurrent.fetch_max(n, Ordering::SeqCst);
        let guard = Guard(self);
        let (latency, forbidden, failure) = {
            let mut g = self.inner.lock().unwrap();
            let failure = if !g.fail_next.is_empty() { Some(g.fail_next.remove(0)) } else { g.fail_paths.get(path).copied() };
            (g.cfg.latency, g.forbidden, failure)
        };
        if forbidden == Some(std::thread::current().id()) {
            self.forbidden_calls.fetch_add(1, Ordering::SeqCst);
        }
        if !latency.is_zero() {
            std::thread::sleep(latency);
        }
        match failure {
            Some(k) => Err(io::Error::new(k, format!("injected failure on {path}"))),
            None => Ok(guard),
        }
    }

    fn entry(path: &str, n: &Node) -> FsEntry {
        let name = path.rsplit('/').next().unwrap_or("").to_string();
        FsEntry { mtime: Some(n.mtime), mode: Some(n.mode), is_symlink: n.is_symlink, ..FsEntry::new(name, path, n.is_dir, if n.is_dir { 0 } else { n.data.len() as u64 }) }
    }
}

impl FsProvider for FakeRemoteProvider {
    fn id(&self) -> &str {
        "fake-remote"
    }

    fn list_dir(&self, dir: &Path) -> io::Result<Vec<FsEntry>> {
        let d = RemotePath::new(&dir.to_string_lossy());
        let _g = self.enter(d.as_str())?;
        let g = self.inner.lock().unwrap();
        match g.nodes.get(d.as_str()) {
            None => return Err(io::Error::new(io::ErrorKind::NotFound, format!("no such folder {d}"))),
            Some(n) if !n.is_dir => return Err(io::Error::new(io::ErrorKind::Other, "not a folder")),
            _ => {}
        }
        let mut out: Vec<FsEntry> = g
            .nodes
            .iter()
            .filter(|(k, _)| k.as_str() != "/" && RemotePath::new(k).parent().as_ref() == Some(&d))
            .map(|(k, n)| Self::entry(k, n))
            .collect();
        for raw in g.raw_names.get(d.as_str()).into_iter().flatten() {
            out.push(FsEntry::new(raw.clone(), format!("{}/{raw}", d), false, 1));
        }
        Ok(out)
    }

    fn stat(&self, path: &Path) -> io::Result<FsEntry> {
        let k = key(path);
        let _g = self.enter(&k)?;
        let g = self.inner.lock().unwrap();
        g.nodes.get(&k).map(|n| Self::entry(&k, n)).ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, format!("no such file {k}")))
    }

    fn read(&self, path: &Path) -> io::Result<Vec<u8>> {
        let k = key(path);
        let _g = self.enter(&k)?;
        let mut g = self.inner.lock().unwrap();
        match g.nodes.get(&k).cloned() {
            Some(n) if !n.is_dir => {
                if g.safe {
                    g.stamps.insert(k, OpenStamp { size: n.data.len() as u64, mtime: secs(n.mtime), mode: n.mode });
                }
                Ok(n.data)
            }
            Some(_) => Err(io::Error::new(io::ErrorKind::Other, "is a folder")),
            None => Err(io::Error::new(io::ErrorKind::NotFound, format!("no such file {k}"))),
        }
    }

    fn write(&self, path: &Path, data: &[u8]) -> io::Result<()> {
        let k = key(path);
        let _g = self.enter(&k)?;
        let mut g = self.inner.lock().unwrap();
        let parent = RemotePath::new(&k).parent().map(|p| p.to_string()).unwrap_or_else(|| "/".into());
        if !g.nodes.get(&parent).is_some_and(|n| n.is_dir) {
            return Err(io::Error::new(io::ErrorKind::NotFound, format!("no such folder {parent}")));
        }
        if let Some(n) = g.nodes.get(&k) {
            if n.is_dir {
                return Err(io::Error::new(io::ErrorKind::Other, "is a folder"));
            }
            if n.mode & 0o200 == 0 {
                return Err(io::Error::new(io::ErrorKind::PermissionDenied, "read-only file"));
            }
        }
        let mode = g.nodes.get(&k).map(|n| n.mode).unwrap_or(0o644);
        g.nodes.insert(k, Node { is_dir: false, data: data.to_vec(), mtime: SystemTime::now(), mode, is_symlink: false });
        Ok(())
    }
}

impl RemoteProvider for FakeRemoteProvider {
    fn safe_fs(&self) -> Option<Box<dyn RemoteFs + Send>> {
        self.inner.lock().unwrap().safe.then(|| Box::new(FakeSafeFs { inner: self.inner.clone() }) as Box<dyn RemoteFs + Send>)
    }

    fn take_open_stamp(&self, path: &RemotePath) -> Option<OpenStamp> {
        self.inner.lock().unwrap().stamps.remove(path.as_str())
    }

    fn mkdir(&self, path: &RemotePath) -> io::Result<()> {
        let _g = self.enter(path.as_str())?;
        let mut g = self.inner.lock().unwrap();
        if g.nodes.contains_key(path.as_str()) {
            return Err(io::Error::new(io::ErrorKind::AlreadyExists, "exists"));
        }
        let parent = path.parent().map(|p| p.to_string()).unwrap_or_else(|| "/".into());
        if !g.nodes.get(&parent).is_some_and(|n| n.is_dir) {
            return Err(io::Error::new(io::ErrorKind::NotFound, "no such folder"));
        }
        g.nodes.insert(path.to_string(), Node { is_dir: true, data: vec![], mtime: SystemTime::now(), mode: 0o755, is_symlink: false });
        Ok(())
    }

    fn rename(&self, from: &RemotePath, to: &RemotePath) -> io::Result<()> {
        let _g = self.enter(from.as_str())?;
        let mut g = self.inner.lock().unwrap();
        if g.nodes.contains_key(to.as_str()) {
            return Err(io::Error::new(io::ErrorKind::AlreadyExists, "target exists"));
        }
        if !g.nodes.contains_key(from.as_str()) {
            return Err(io::Error::new(io::ErrorKind::NotFound, "no such file"));
        }
        let moved: BTreeSet<String> = g.nodes.keys().filter(|k| RemotePath::new(k).starts_with(from)).cloned().collect();
        for k in moved {
            let n = g.nodes.remove(&k).unwrap();
            let nk = format!("{}{}", to, &k[from.as_str().len()..]);
            g.nodes.insert(nk, n);
        }
        Ok(())
    }

    fn remove(&self, path: &RemotePath, is_dir: bool) -> io::Result<()> {
        let _g = self.enter(path.as_str())?;
        let mut g = self.inner.lock().unwrap();
        match g.nodes.get(path.as_str()) {
            None => return Err(io::Error::new(io::ErrorKind::NotFound, "no such file")),
            Some(n) if n.is_dir != is_dir => return Err(io::Error::new(io::ErrorKind::Other, "wrong kind")),
            _ => {}
        }
        if is_dir && g.nodes.keys().any(|k| k != path.as_str() && RemotePath::new(k).starts_with(path)) {
            return Err(io::Error::new(io::ErrorKind::Other, "folder is not empty"));
        }
        g.nodes.remove(path.as_str());
        Ok(())
    }
}

fn secs(t: SystemTime) -> i64 {
    t.duration_since(SystemTime::UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0)
}

/// The blocking file contract of the safe saver, over the fake server's own tree (dev hook only).
struct FakeSafeFs {
    inner: Arc<Mutex<Inner>>,
}

impl FakeSafeFs {
    /// Every call pays the configured latency, so the "Saving..." state can be seen.
    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        let latency = self.inner.lock().unwrap().cfg.latency;
        if !latency.is_zero() {
            std::thread::sleep(latency);
        }
        self.inner.lock().unwrap()
    }

    fn parent_is_dir(g: &Inner, path: &str) -> bool {
        let parent = RemotePath::new(path).parent().map(|p| p.to_string()).unwrap_or_else(|| "/".into());
        g.nodes.get(&parent).is_some_and(|n| n.is_dir)
    }
}

/// One-shot trigger: see [`FakeRemoteProvider::enable_safe`].
fn apply_trigger(g: &mut Inner, path: &str) {
    let Some(dir) = g.trigger_dir.clone() else { return };
    let file = dir.join("conflict-once.txt");
    let Ok(text) = std::fs::read_to_string(&file) else { return };
    let (target, body) = text.split_once('\n').unwrap_or((text.as_str(), ""));
    if RemotePath::new(target.trim()).as_str() != path {
        return;
    }
    let _ = std::fs::remove_file(&file);
    if let Some(n) = g.nodes.get_mut(path) {
        n.data = body.as_bytes().to_vec();
        n.mtime += Duration::from_secs(60);
    }
}

impl RemoteFs for FakeSafeFs {
    fn caps(&self) -> Caps {
        Caps { posix_rename: true, hash: true, set_mode: true }
    }

    fn stat(&mut self, path: &str) -> FsResult<Option<Stat>> {
        let mut g = self.lock();
        let k = RemotePath::new(path).to_string();
        apply_trigger(&mut g, &k);
        Ok(g.nodes.get(&k).map(|n| Stat { kind: if n.is_dir { Kind::Dir } else if n.is_symlink { Kind::Symlink } else { Kind::File }, size: n.data.len() as u64, mtime: secs(n.mtime), mode: n.mode }))
    }

    fn realpath(&mut self, path: &str) -> FsResult<String> {
        let g = self.lock();
        let k = RemotePath::new(path).to_string();
        if g.nodes.contains_key(&k) {
            Ok(k)
        } else {
            Err(FsError::NotFound)
        }
    }

    fn read(&mut self, path: &str) -> FsResult<Vec<u8>> {
        let g = self.lock();
        g.nodes.get(&RemotePath::new(path).to_string()).filter(|n| !n.is_dir).map(|n| n.data.clone()).ok_or(FsError::NotFound)
    }

    fn write_new(&mut self, path: &str, data: &[u8], mode: Option<u32>) -> FsResult<()> {
        let mut g = self.lock();
        let k = RemotePath::new(path).to_string();
        if g.nodes.contains_key(&k) {
            return Err(FsError::Exists);
        }
        if !Self::parent_is_dir(&g, &k) {
            return Err(FsError::NotFound);
        }
        g.nodes.insert(k, Node { is_dir: false, data: data.to_vec(), mtime: SystemTime::now(), mode: mode.unwrap_or(0o600), is_symlink: false });
        Ok(())
    }

    fn write_truncate(&mut self, path: &str, data: &[u8]) -> FsResult<()> {
        let mut g = self.lock();
        let n = g.nodes.get_mut(&RemotePath::new(path).to_string()).filter(|n| !n.is_dir).ok_or(FsError::NotFound)?;
        if n.mode & 0o200 == 0 {
            return Err(FsError::PermissionDenied);
        }
        n.data = data.to_vec();
        n.mtime = SystemTime::now();
        Ok(())
    }

    fn set_mode(&mut self, path: &str, mode: u32) -> FsResult<()> {
        let mut g = self.lock();
        g.nodes.get_mut(&RemotePath::new(path).to_string()).ok_or(FsError::NotFound)?.mode = mode;
        Ok(())
    }

    fn rename(&mut self, from: &str, to: &str) -> FsResult<()> {
        let mut g = self.lock();
        let (f, t) = (RemotePath::new(from).to_string(), RemotePath::new(to).to_string());
        if g.nodes.contains_key(&t) {
            return Err(FsError::Exists);
        }
        let n = g.nodes.remove(&f).ok_or(FsError::NotFound)?;
        g.nodes.insert(t, n);
        Ok(())
    }

    fn rename_replace(&mut self, from: &str, to: &str) -> FsResult<()> {
        let mut g = self.lock();
        let (f, t) = (RemotePath::new(from).to_string(), RemotePath::new(to).to_string());
        let n = g.nodes.remove(&f).ok_or(FsError::NotFound)?;
        g.nodes.insert(t, n);
        Ok(())
    }

    fn remove(&mut self, path: &str) -> FsResult<()> {
        let mut g = self.lock();
        g.nodes.remove(&RemotePath::new(path).to_string()).map(|_| ()).ok_or(FsError::NotFound)
    }

    fn hash(&mut self, path: &str) -> FsResult<Option<String>> {
        let g = self.lock();
        Ok(g.nodes.get(&RemotePath::new(path).to_string()).filter(|n| !n.is_dir).map(|n| brouillon_safesave::sha256_hex(&n.data)))
    }
}

/// Developer flag: the only "server" this build can reach. A real helper-backed factory replaces
/// it later. `demo` (as host name) or `BROUILLON_REMOTE_FAKE=1` selects the built-in fake server;
/// any other host gets a plain "not available yet" error instead of a pretend connection.
/// `BROUILLON_REMOTE_FAKE_LATENCY_MS` slows every fake call so loading rows can be seen.
pub struct DemoFactory {
    fake: Arc<FakeRemoteProvider>,
    any_host: bool,
}

impl DemoFactory {
    pub fn from_env() -> Self {
        let any_host = std::env::var("BROUILLON_REMOTE_FAKE").map(|v| v == "1").unwrap_or(false);
        Self::new(any_host, std::env::var("BROUILLON_REMOTE_FAKE_LATENCY_MS").ok().and_then(|v| v.parse().ok()).map(Duration::from_millis)).with_dev_hooks_from_env()
    }

    /// Smoke-test hooks from the environment: `BROUILLON_REMOTE_FAKE_SAFE=1` makes the fake a safe
    /// provider (Ctrl+S runs the verified-save engine); `BROUILLON_REMOTE_FAKE_TRIGGER_DIR` is where a
    /// `conflict-once.txt` file provokes one conflict. Nothing here touches a network.
    pub fn with_dev_hooks_from_env(self) -> Self {
        if std::env::var("BROUILLON_REMOTE_FAKE_SAFE").map(|v| v == "1").unwrap_or(false) {
            self.fake.enable_safe(std::env::var_os("BROUILLON_REMOTE_FAKE_TRIGGER_DIR").map(PathBuf::from));
        }
        self
    }

    pub fn new(any_host: bool, latency: Option<Duration>) -> Self {
        let fake = FakeRemoteProvider::demo();
        if let Some(l) = latency {
            fake.set_latency(l);
        }
        Self { fake, any_host }
    }
}

impl super::hub::ProviderFactory for DemoFactory {
    fn connect(&self, profile: &super::sites::SiteProfile) -> Result<super::provider::SharedProvider, super::error::RemoteError> {
        if self.any_host || profile.host == "demo" {
            Ok(self.fake.clone())
        } else {
            Err(super::error::RemoteError::new(
                super::error::RemoteErrorKind::Unsupported,
                "real SFTP is not available in this build yet; use the host name `demo` to try the explorer with the built-in fake server",
            ))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_dev_factory_only_serves_the_demo_host_unless_the_flag_is_set() {
        use crate::remote::hub::ProviderFactory;
        use crate::remote::sites::{AuthMethod, SiteProfile};
        let p = |h: &str| SiteProfile::from_quick(&format!("me@{h}"), AuthMethod::Agent, "").unwrap();
        let f = DemoFactory::new(false, None);
        assert!(f.connect(&p("demo")).is_ok());
        let err = f.connect(&p("example.org")).err().unwrap();
        assert_eq!(err.kind, crate::remote::error::RemoteErrorKind::Unsupported);
        assert!(DemoFactory::new(true, None).connect(&p("example.org")).is_ok());
    }

    #[test]
    fn demo_tree_lists_and_reads() {
        let f = FakeRemoteProvider::demo();
        let names: Vec<String> = f.list_dir(Path::new("/var/www")).unwrap().into_iter().map(|e| e.name).collect();
        assert!(names.contains(&"index.php".to_string()) && names.contains(&"css".to_string()));
        assert!(f.read(Path::new("/var/www/index.php")).unwrap().starts_with(b"<?php"));
        assert_eq!(f.list_dir(Path::new("/nope")).unwrap_err().kind(), io::ErrorKind::NotFound);
    }

    #[test]
    fn mutations_never_replace_and_respect_modes() {
        let f = FakeRemoteProvider::demo();
        let p = |s: &str| RemotePath::new(s);
        assert!(f.mkdir(&p("/var/www/new")).is_ok());
        assert_eq!(f.mkdir(&p("/var/www/new")).unwrap_err().kind(), io::ErrorKind::AlreadyExists);
        assert_eq!(f.rename(&p("/var/www/robots.txt"), &p("/var/www/index.php")).unwrap_err().kind(), io::ErrorKind::AlreadyExists);
        f.rename(&p("/var/www/css"), &p("/var/www/styles")).unwrap();
        assert!(f.exists("/var/www/styles/style.css") && !f.exists("/var/www/css"));
        assert_eq!(f.write(Path::new("/var/www/.htaccess"), b"x").unwrap_err().kind(), io::ErrorKind::PermissionDenied);
        assert!(f.remove(&p("/var/www"), true).is_err(), "non-empty folder");
        f.remove(&p("/var/www/robots.txt"), false).unwrap();
        assert!(!f.exists("/var/www/robots.txt"));
    }

    #[test]
    fn the_safe_dev_hook_provokes_exactly_one_conflict_and_keeps_the_default_unsafe() {
        let plain = FakeRemoteProvider::demo();
        assert!(plain.safe_fs().is_none(), "without the hook the demo server stays a plain provider");

        let dir = std::env::temp_dir().join(format!("brouillon-fake-trigger-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let f = FakeRemoteProvider::demo();
        f.enable_safe(Some(dir.clone()));
        let path = RemotePath::new("/var/www/index.php");
        f.read(Path::new("/var/www/index.php")).unwrap();
        let stamp = f.take_open_stamp(&path).expect("a safe provider hands out the stamp taken at read time");
        assert!(f.take_open_stamp(&path).is_none(), "the stamp is consumed");

        let mut fs = f.safe_fs().expect("safe");
        let before = fs.stat("/var/www/index.php").unwrap().unwrap();
        assert_eq!((before.size, before.mtime), (stamp.size, stamp.mtime), "untouched server file matches the open stamp");

        std::fs::write(dir.join("conflict-once.txt"), "/var/www/index.php\nsomeone else was here\n").unwrap();
        let other = fs.stat("/var/www/robots.txt").unwrap().unwrap();
        assert!(other.size > 0 && dir.join("conflict-once.txt").exists(), "another path does not consume the trigger");
        let after = fs.stat("/var/www/index.php").unwrap().unwrap();
        assert_ne!((after.size, after.mtime), (stamp.size, stamp.mtime), "the server file now differs from what was opened");
        assert_eq!(f.file_text("/var/www/index.php").unwrap(), "someone else was here\n");
        assert!(!dir.join("conflict-once.txt").exists(), "one shot: the trigger file is gone");
        assert_eq!(fs.stat("/var/www/index.php").unwrap().unwrap(), after, "and it does not fire twice");

        // The safe contract the saver relies on.
        fs.write_new("/var/www/.tmp1", b"abc", Some(0o600)).unwrap();
        assert_eq!(fs.write_new("/var/www/.tmp1", b"x", None).unwrap_err(), FsError::Exists);
        assert_eq!(fs.hash("/var/www/.tmp1").unwrap().unwrap(), brouillon_safesave::sha256_hex(b"abc"));
        fs.rename_replace("/var/www/.tmp1", "/var/www/index.php").unwrap();
        assert_eq!(f.file_text("/var/www/index.php").unwrap(), "abc");
        assert!(!f.exists("/var/www/.tmp1"));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn save_then_someone_else_changes_the_file_then_save_again_is_a_conflict_with_zero_bytes_written() {
        use crate::remote::saving::{SaveEvent, SaveService, Submit};
        use brouillon_safesave::SaveOutcome;
        let dir = std::env::temp_dir().join(format!("brouillon-fake-flow-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let trig = dir.join("trigger");
        std::fs::create_dir_all(&trig).unwrap();
        let f = FakeRemoteProvider::demo();
        f.enable_safe(Some(trig.clone()));
        let svc = SaveService::new(dir.join("journal"), dir.join("backups"));
        let key = ("demo".to_string(), "/var/www/index.php".to_string());
        let bytes = f.read(Path::new("/var/www/index.php")).unwrap();
        let stamp = f.take_open_stamp(&RemotePath::new("/var/www/index.php")).unwrap();
        svc.attach(key.clone(), &bytes, stamp);
        let wait = || loop {
            if let Some(e) = svc.drain().into_iter().next() {
                return e;
            }
            std::thread::sleep(Duration::from_millis(5));
        };
        let shared: crate::remote::provider::SharedProvider = f.clone();

        let mut first = b"A-".to_vec();
        first.extend_from_slice(&bytes);
        assert_eq!(svc.save(&key, first.clone(), shared.clone()), Submit::Started);
        match wait() {
            SaveEvent::Outcome { outcome, .. } => assert!(matches!(outcome, SaveOutcome::Saved { .. }), "{outcome:?}"),
            o => panic!("{o:?}"),
        }
        assert_eq!(f.file_text("/var/www/index.php").unwrap().as_bytes(), &first[..]);

        std::fs::write(trig.join("conflict-once.txt"), "/var/www/index.php
CHANGED BY SOMEONE ELSE
").unwrap();
        let mut second = b"B-".to_vec();
        second.extend_from_slice(&first);
        assert_eq!(svc.save(&key, second, shared), Submit::Started);
        match wait() {
            SaveEvent::Outcome { outcome, .. } => assert!(matches!(outcome, SaveOutcome::Conflict { .. }), "must be a conflict: {outcome:?}"),
            o => panic!("{o:?}"),
        }
        assert!(!trig.join("conflict-once.txt").exists(), "trigger consumed");
        assert_eq!(f.file_text("/var/www/index.php").unwrap(), "CHANGED BY SOMEONE ELSE
", "the other person's change is untouched");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn failure_injection_is_one_shot_or_per_path() {
        let f = FakeRemoteProvider::demo();
        f.fail_next(1, io::ErrorKind::TimedOut);
        assert_eq!(f.list_dir(Path::new("/")).unwrap_err().kind(), io::ErrorKind::TimedOut);
        assert!(f.list_dir(Path::new("/")).is_ok());
        f.fail_path("/etc", io::ErrorKind::PermissionDenied);
        assert!(f.list_dir(Path::new("/etc")).is_err());
        f.clear_failures();
        assert!(f.list_dir(Path::new("/etc")).is_ok());
    }
}
