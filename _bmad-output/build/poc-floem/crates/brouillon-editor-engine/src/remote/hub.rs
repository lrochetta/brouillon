//! `RemoteHub`: a request queue served by a small, lazily started worker pool.
//!
//! * The UI thread only calls [`RemoteHub::submit`], [`RemoteHub::cancel`] and
//!   [`RemoteHub::drain`]; none of them waits on the network.
//! * Workers call the blocking [`RemoteProvider`]. No thread exists until the first job is
//!   submitted, so a session without any site costs nothing.
//! * Results are queued and `wake` is called (floem: `register_ext_trigger`); the UI then calls
//!   `drain` from an effect. This is the Link `Bridge` / `Pump` shape in the other direction.
//! * Limits (spec 04): at most [`SITE_CAP`] jobs in flight per site, [`MAX_WORKERS`] threads in all.
//! * A cancelled job never reports a success: its event is `Err(Cancelled)`.

use std::collections::{HashMap, VecDeque};
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex};

use super::error::{RemoteError, RemoteErrorKind};
use super::path::RemotePath;
use super::provider::{RemoteEntry, SharedProvider};
use super::sites::{SiteId, SiteProfile};

pub const SITE_CAP: usize = 2;
pub const MAX_WORKERS: usize = 4;
/// Hard byte cap on a file read, enforced on what actually arrived (the server may lie in `stat`).
pub const MAX_READ_BYTES: usize = 10 * 1024 * 1024;

pub type JobId = u64;

#[derive(Clone, Debug)]
pub enum RemoteOp {
    Connect(SiteProfile),
    List(RemotePath),
    Read(RemotePath),
    Write { path: RemotePath, data: Vec<u8> },
    Mkdir(RemotePath),
    CreateFile(RemotePath),
    Rename { from: RemotePath, to: RemotePath },
    Remove { path: RemotePath, is_dir: bool },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum OpKind {
    Connect,
    List,
    Read,
    Write,
    Mkdir,
    CreateFile,
    Rename,
    Remove,
}

impl RemoteOp {
    pub fn kind(&self) -> OpKind {
        match self {
            RemoteOp::Connect(_) => OpKind::Connect,
            RemoteOp::List(_) => OpKind::List,
            RemoteOp::Read(_) => OpKind::Read,
            RemoteOp::Write { .. } => OpKind::Write,
            RemoteOp::Mkdir(_) => OpKind::Mkdir,
            RemoteOp::CreateFile(_) => OpKind::CreateFile,
            RemoteOp::Rename { .. } => OpKind::Rename,
            RemoteOp::Remove { .. } => OpKind::Remove,
        }
    }

    /// The path the operation is about (the root for `Connect`).
    pub fn path(&self) -> RemotePath {
        match self {
            RemoteOp::Connect(_) => RemotePath::root(),
            RemoteOp::List(p) | RemoteOp::Read(p) | RemoteOp::Mkdir(p) | RemoteOp::CreateFile(p) => p.clone(),
            RemoteOp::Write { path, .. } | RemoteOp::Remove { path, .. } => path.clone(),
            RemoteOp::Rename { from, .. } => from.clone(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RemoteReply {
    Connected,
    Listed(Vec<RemoteEntry>),
    Read(Vec<u8>),
    Done,
}

#[derive(Clone, Debug)]
pub struct RemoteEvent {
    pub job: JobId,
    pub site: SiteId,
    pub kind: OpKind,
    pub path: RemotePath,
    /// For `Rename`: the new path.
    pub to: Option<RemotePath>,
    pub result: Result<RemoteReply, RemoteError>,
}

/// Turns a profile into a connected provider. Runs on a worker thread (it may block on the network).
/// The helper-backed SFTP client plugs in here; the dev flag plugs in a [`super::FakeRemoteProvider`].
pub trait ProviderFactory: Send + Sync {
    fn connect(&self, profile: &SiteProfile) -> Result<SharedProvider, RemoteError>;
}

struct Job {
    id: JobId,
    site: SiteId,
    op: RemoteOp,
    cancel: Arc<AtomicBool>,
}

#[derive(Default)]
struct State {
    queue: VecDeque<Job>,
    providers: HashMap<SiteId, SharedProvider>,
    in_flight: HashMap<SiteId, usize>,
    live: HashMap<JobId, (SiteId, Arc<AtomicBool>)>,
    workers: usize,
    idle: usize,
    shutdown: bool,
}

struct Inner {
    state: Mutex<State>,
    cv: Condvar,
    out: Mutex<VecDeque<RemoteEvent>>,
    wake: Box<dyn Fn() + Send + Sync>,
    factory: Arc<dyn ProviderFactory>,
    next_id: AtomicU64,
}

pub struct RemoteHub {
    inner: Arc<Inner>,
}

impl RemoteHub {
    /// `wake` is called (from a worker thread) after results were queued.
    pub fn new(wake: impl Fn() + Send + Sync + 'static, factory: Arc<dyn ProviderFactory>) -> Self {
        Self { inner: Arc::new(Inner { state: Mutex::new(State::default()), cv: Condvar::new(), out: Mutex::new(VecDeque::new()), wake: Box::new(wake), factory, next_id: AtomicU64::new(1) }) }
    }

    /// Number of worker threads started so far (0 until the first job).
    pub fn worker_count(&self) -> usize {
        self.inner.state.lock().map(|s| s.workers).unwrap_or(0)
    }

    /// Attach an already connected provider (tests, or a provider built elsewhere).
    pub fn register_provider(&self, site: &str, p: SharedProvider) {
        if let Ok(mut s) = self.inner.state.lock() {
            s.providers.insert(site.to_string(), p);
        }
    }

    /// The connected provider of `site`, if any (a cheap lookup, no I/O).
    pub fn provider(&self, site: &str) -> Option<SharedProvider> {
        self.inner.state.lock().ok().and_then(|s| s.providers.get(site).cloned())
    }

    pub fn is_connected(&self, site: &str) -> bool {
        self.inner.state.lock().map(|s| s.providers.contains_key(site)).unwrap_or(false)
    }

    /// Queue `op`; returns at once. Starts a worker if none is free (never more than [`MAX_WORKERS`]).
    pub fn submit(&self, site: &str, op: RemoteOp) -> JobId {
        let id = self.inner.next_id.fetch_add(1, Ordering::Relaxed);
        let cancel = Arc::new(AtomicBool::new(false));
        let spawn = {
            let Ok(mut s) = self.inner.state.lock() else { return id };
            if s.shutdown {
                return id;
            }
            s.live.insert(id, (site.to_string(), cancel.clone()));
            s.queue.push_back(Job { id, site: site.to_string(), op, cancel });
            let spawn = s.idle == 0 && s.workers < MAX_WORKERS;
            if spawn {
                s.workers += 1;
            }
            spawn
        };
        if spawn {
            let inner = self.inner.clone();
            if std::thread::Builder::new().name("brouillon-remote".into()).spawn(move || worker(inner)).is_err() {
                if let Ok(mut s) = self.inner.state.lock() {
                    s.workers -= 1;
                }
            }
        }
        self.inner.cv.notify_one();
        id
    }

    /// Cancel one job: not started = dropped; running = its result is discarded.
    pub fn cancel(&self, id: JobId) {
        if let Ok(s) = self.inner.state.lock() {
            if let Some((_, c)) = s.live.get(&id) {
                c.store(true, Ordering::SeqCst);
            }
        }
        self.inner.cv.notify_all();
    }

    /// Cancel every job of `site` and forget its connection (close of a site tab / disconnect).
    pub fn disconnect(&self, site: &str) {
        if let Ok(mut s) = self.inner.state.lock() {
            for (sid, c) in s.live.values() {
                if sid == site {
                    c.store(true, Ordering::SeqCst);
                }
            }
            s.providers.remove(site);
        }
        self.inner.cv.notify_all();
    }

    /// UI thread: take every finished event.
    pub fn drain(&self) -> Vec<RemoteEvent> {
        self.inner.out.lock().map(|mut o| o.drain(..).collect()).unwrap_or_default()
    }
}

impl Drop for RemoteHub {
    fn drop(&mut self) {
        if let Ok(mut s) = self.inner.state.lock() {
            s.shutdown = true;
            s.queue.clear();
        }
        self.inner.cv.notify_all();
    }
}

fn worker(inner: Arc<Inner>) {
    loop {
        let job = {
            let Ok(mut s) = inner.state.lock() else { return };
            loop {
                if s.shutdown {
                    s.workers -= 1;
                    return;
                }
                let pos = s.queue.iter().position(|j| s.in_flight.get(&j.site).copied().unwrap_or(0) < SITE_CAP);
                if let Some(i) = pos {
                    let j = s.queue.remove(i).expect("position is in range");
                    *s.in_flight.entry(j.site.clone()).or_insert(0) += 1;
                    break j;
                }
                s.idle += 1;
                s = match inner.cv.wait(s) {
                    Ok(g) => g,
                    Err(_) => return,
                };
                s.idle -= 1;
            }
        };
        let event = run_job(&inner, &job);
        if let Ok(mut s) = inner.state.lock() {
            if let Some(n) = s.in_flight.get_mut(&job.site) {
                *n = n.saturating_sub(1);
            }
            s.live.remove(&job.id);
        }
        if let Ok(mut o) = inner.out.lock() {
            o.push_back(event);
        }
        (inner.wake)();
        inner.cv.notify_all();
    }
}

fn run_job(inner: &Inner, job: &Job) -> RemoteEvent {
    let kind = job.op.kind();
    let path = job.op.path();
    let to = if let RemoteOp::Rename { to, .. } = &job.op { Some(to.clone()) } else { None };
    let result = if job.cancel.load(Ordering::SeqCst) {
        Err(RemoteError::cancelled())
    } else {
        let r = catch_unwind(AssertUnwindSafe(|| execute(inner, job))).unwrap_or_else(|_| Err(RemoteError::new(RemoteErrorKind::Other, "the remote worker panicked")));
        if job.cancel.load(Ordering::SeqCst) {
            Err(RemoteError::cancelled())
        } else {
            r
        }
    };
    RemoteEvent { job: job.id, site: job.site.clone(), kind, path, to, result }
}

fn provider_of(inner: &Inner, site: &str) -> Result<SharedProvider, RemoteError> {
    inner.state.lock().ok().and_then(|s| s.providers.get(site).cloned()).ok_or_else(|| RemoteError::new(RemoteErrorKind::Unreachable, "not connected"))
}

fn io(e: std::io::Error) -> RemoteError {
    RemoteError::from_io(&e)
}

fn execute(inner: &Inner, job: &Job) -> Result<RemoteReply, RemoteError> {
    if let RemoteOp::Connect(profile) = &job.op {
        let p = inner.factory.connect(profile)?;
        // Prove the session works before reporting success: a bad start folder is an error here.
        p.list_dir(&RemotePath::new(&profile.root_path).to_path()).map_err(io)?;
        inner.state.lock().map_err(|_| RemoteError::new(RemoteErrorKind::Other, "poisoned"))?.providers.insert(job.site.clone(), p);
        return Ok(RemoteReply::Connected);
    }
    let p = provider_of(inner, &job.site)?;
    match &job.op {
        RemoteOp::Connect(_) => unreachable!("handled above"),
        RemoteOp::List(dir) => {
            let mut v: Vec<RemoteEntry> = p.list_dir(&dir.to_path()).map_err(io)?.iter().filter_map(RemoteEntry::from_fs).collect();
            sort_entries(&mut v);
            Ok(RemoteReply::Listed(v))
        }
        RemoteOp::Read(path) => {
            let data = p.read(&path.to_path()).map_err(io)?;
            if data.len() > MAX_READ_BYTES {
                return Err(RemoteError::new(RemoteErrorKind::TooLarge, format!("{} bytes", data.len())));
            }
            Ok(RemoteReply::Read(data))
        }
        RemoteOp::Write { path, data } => p.write(&path.to_path(), data).map(|_| RemoteReply::Done).map_err(io),
        RemoteOp::Mkdir(path) => p.mkdir(path).map(|_| RemoteReply::Done).map_err(io),
        RemoteOp::CreateFile(path) => {
            // Never replaces: an existing name is an error, not a truncation.
            if p.stat(&path.to_path()).is_ok() {
                return Err(RemoteError::new(RemoteErrorKind::AlreadyExists, path.to_string()));
            }
            p.write(&path.to_path(), b"").map(|_| RemoteReply::Done).map_err(io)
        }
        RemoteOp::Rename { from, to } => p.rename(from, to).map(|_| RemoteReply::Done).map_err(io),
        RemoteOp::Remove { path, is_dir } => p.remove(path, *is_dir).map(|_| RemoteReply::Done).map_err(io),
    }
}

/// Folders first, then case-insensitive name (the same rule as every other provider).
pub fn sort_entries(v: &mut [RemoteEntry]) {
    v.sort_by(|a, b| b.is_dir.cmp(&a.is_dir).then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase())).then_with(|| a.name.cmp(&b.name)));
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::remote::fake::FakeRemoteProvider;
    use std::sync::atomic::AtomicUsize;
    use std::time::{Duration, Instant};

    struct Factory(Arc<FakeRemoteProvider>);
    impl ProviderFactory for Factory {
        fn connect(&self, _: &SiteProfile) -> Result<SharedProvider, RemoteError> {
            Ok(self.0.clone())
        }
    }

    fn hub_with(fake: Arc<FakeRemoteProvider>) -> (RemoteHub, Arc<AtomicUsize>) {
        let wakes = Arc::new(AtomicUsize::new(0));
        let w = wakes.clone();
        let hub = RemoteHub::new(move || { w.fetch_add(1, Ordering::SeqCst); }, Arc::new(Factory(fake)));
        (hub, wakes)
    }

    fn wait_events(hub: &RemoteHub, n: usize) -> Vec<RemoteEvent> {
        let t = Instant::now();
        let mut all = Vec::new();
        while all.len() < n && t.elapsed() < Duration::from_secs(10) {
            all.extend(hub.drain());
            std::thread::sleep(Duration::from_millis(2));
        }
        all
    }

    #[test]
    fn no_thread_exists_until_a_job_is_submitted() {
        let (hub, wakes) = hub_with(FakeRemoteProvider::demo());
        assert_eq!(hub.worker_count(), 0);
        assert!(hub.drain().is_empty());
        assert_eq!(wakes.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn list_runs_off_the_calling_thread_and_wakes_the_ui() {
        let fake = FakeRemoteProvider::demo();
        fake.set_latency(Duration::from_millis(60));
        fake.forbid_thread(std::thread::current().id());
        let (hub, wakes) = hub_with(fake.clone());
        hub.register_provider("s", fake.clone());
        let t = Instant::now();
        let id = hub.submit("s", RemoteOp::List(RemotePath::new("/var/www")));
        assert!(t.elapsed() < Duration::from_millis(30), "submit must not wait for the server");
        let ev = wait_events(&hub, 1);
        assert_eq!(ev.len(), 1);
        assert_eq!(ev[0].job, id);
        let Ok(RemoteReply::Listed(v)) = &ev[0].result else { panic!("{:?}", ev[0].result) };
        assert!(v.iter().any(|e| e.name == "index.php"));
        assert!(wakes.load(Ordering::SeqCst) >= 1);
        assert_eq!(fake.forbidden_calls(), 0, "the UI thread made a remote call");
        assert!(fake.calls() >= 1);
    }

    #[test]
    fn connect_registers_the_provider_and_a_bad_start_folder_fails() {
        let fake = FakeRemoteProvider::demo();
        let (hub, _) = hub_with(fake);
        let mut p = SiteProfile::from_quick("me@demo", crate::remote::AuthMethod::Agent, "/var/www").unwrap();
        hub.submit(&p.id.clone(), RemoteOp::Connect(p.clone()));
        let ev = wait_events(&hub, 1);
        assert_eq!(ev[0].result, Ok(RemoteReply::Connected));
        assert!(hub.is_connected(&p.id));
        p.id = "other".into();
        p.root_path = "/nope".into();
        hub.submit("other", RemoteOp::Connect(p));
        let ev = wait_events(&hub, 1);
        assert_eq!(ev[0].result.as_ref().unwrap_err().kind, RemoteErrorKind::NotFound);
        assert!(!hub.is_connected("other"));
    }

    #[test]
    fn a_queued_job_can_be_cancelled_and_reports_cancelled() {
        let fake = FakeRemoteProvider::demo();
        fake.set_latency(Duration::from_millis(150));
        let (hub, _) = hub_with(fake.clone());
        hub.register_provider("s", fake.clone());
        // SITE_CAP jobs occupy the site; the third waits in the queue and is cancelled.
        let _a = hub.submit("s", RemoteOp::List(RemotePath::root()));
        let _b = hub.submit("s", RemoteOp::List(RemotePath::root()));
        let c = hub.submit("s", RemoteOp::List(RemotePath::new("/var")));
        hub.cancel(c);
        let ev = wait_events(&hub, 3);
        assert_eq!(ev.len(), 3);
        let cev = ev.iter().find(|e| e.job == c).unwrap();
        assert_eq!(cev.result.as_ref().unwrap_err().kind, RemoteErrorKind::Cancelled);
        // the cancelled job never reached the provider
        assert_eq!(fake.calls(), 2);
    }

    #[test]
    fn a_running_job_that_is_cancelled_discards_its_result() {
        let fake = FakeRemoteProvider::demo();
        fake.set_latency(Duration::from_millis(100));
        let (hub, _) = hub_with(fake.clone());
        hub.register_provider("s", fake);
        let id = hub.submit("s", RemoteOp::Read(RemotePath::new("/var/www/index.php")));
        std::thread::sleep(Duration::from_millis(20));
        hub.cancel(id);
        let ev = wait_events(&hub, 1);
        assert_eq!(ev[0].result.as_ref().unwrap_err().kind, RemoteErrorKind::Cancelled);
    }

    #[test]
    fn per_site_concurrency_is_capped() {
        let fake = FakeRemoteProvider::demo();
        fake.set_latency(Duration::from_millis(60));
        let (hub, _) = hub_with(fake.clone());
        hub.register_provider("s", fake.clone());
        for _ in 0..6 {
            hub.submit("s", RemoteOp::List(RemotePath::root()));
        }
        let ev = wait_events(&hub, 6);
        assert_eq!(ev.len(), 6);
        assert!(fake.max_concurrent() <= SITE_CAP, "max concurrent was {}", fake.max_concurrent());
        assert!(hub.worker_count() <= MAX_WORKERS);
    }

    #[test]
    fn a_failure_is_reported_in_plain_buckets_and_other_jobs_still_run() {
        let fake = FakeRemoteProvider::demo();
        fake.fail_path("/var/www/css", std::io::ErrorKind::PermissionDenied);
        let (hub, _) = hub_with(fake.clone());
        hub.register_provider("s", fake);
        hub.submit("s", RemoteOp::List(RemotePath::new("/var/www/css")));
        hub.submit("s", RemoteOp::List(RemotePath::new("/var/www")));
        let ev = wait_events(&hub, 2);
        let bad = ev.iter().find(|e| e.path.as_str() == "/var/www/css").unwrap();
        assert_eq!(bad.result.as_ref().unwrap_err().kind, RemoteErrorKind::PermissionDenied);
        assert!(ev.iter().any(|e| e.path.as_str() == "/var/www" && e.result.is_ok()));
    }

    #[test]
    fn a_hostile_listing_cannot_inject_paths_and_a_huge_read_is_refused() {
        let fake = FakeRemoteProvider::demo();
        fake.inject_raw_entry("/var/www", "../../etc/passwd");
        fake.put_file("/var/big.bin", &vec![b'x'; MAX_READ_BYTES + 1]);
        let (hub, _) = hub_with(fake.clone());
        hub.register_provider("s", fake);
        hub.submit("s", RemoteOp::List(RemotePath::new("/var/www")));
        hub.submit("s", RemoteOp::Read(RemotePath::new("/var/big.bin")));
        let ev = wait_events(&hub, 2);
        for e in &ev {
            match &e.result {
                Ok(RemoteReply::Listed(v)) => assert!(v.iter().all(|x| !x.name.contains("..") && !x.name.contains('/'))),
                Err(err) => assert_eq!(err.kind, RemoteErrorKind::TooLarge),
                other => panic!("{other:?}"),
            }
        }
    }

    #[test]
    fn create_file_never_truncates_an_existing_file() {
        let fake = FakeRemoteProvider::demo();
        let (hub, _) = hub_with(fake.clone());
        hub.register_provider("s", fake.clone());
        hub.submit("s", RemoteOp::CreateFile(RemotePath::new("/var/www/index.php")));
        let ev = wait_events(&hub, 1);
        assert_eq!(ev[0].result.as_ref().unwrap_err().kind, RemoteErrorKind::AlreadyExists);
        assert!(!fake.file_text("/var/www/index.php").unwrap().is_empty());
    }

    #[test]
    fn dropping_the_hub_stops_the_workers() {
        let fake = FakeRemoteProvider::demo();
        let (hub, _) = hub_with(fake.clone());
        hub.register_provider("s", fake);
        hub.submit("s", RemoteOp::List(RemotePath::root()));
        wait_events(&hub, 1);
        let inner = hub.inner.clone();
        drop(hub);
        let t = Instant::now();
        while inner.state.lock().unwrap().workers > 0 && t.elapsed() < Duration::from_secs(5) {
            std::thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(inner.state.lock().unwrap().workers, 0);
    }
}
