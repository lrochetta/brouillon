//! UI-thread bridge: the link threads call [`Bridge`] (an [`EditorApi`]); each call is queued to the
//! UI thread, which drains the queue with [`Pump::drain`] and answers through a [`UiOps`].
//!
//! * A request that the UI thread does not answer within the timeout fails (reads: `Io("editor busy")`)
//!   and is flagged cancelled, so a late UI thread skips it instead of applying it behind the
//!   caller's back. Best effort: a job already running when the timeout hits still completes.
//! * Notifications (`proposal_pending`, ...) are posted without waiting.
//! * The UI thread must never call a `Bridge` method and wait for itself: UI code that resolves a
//!   proposal spawns a thread for `McpCore::resolve_proposal` (which calls back into `apply_edits`).

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crate::api::*;

/// What the UI thread answers. Same shape as [`EditorApi`] but not `Send`/`Sync`: the real
/// implementation holds UI signals.
pub trait UiOps {
    fn docs(&self) -> Vec<DocInfo>;
    fn active_doc(&self) -> Option<String>;
    fn workspace_folders(&self) -> Vec<String>;
    fn snapshot(&self, doc: &str) -> Option<DocSnapshot>;
    fn apply_edits(&self, doc: &str, expected_version: u64, edits: &[TextEdit], label: &str) -> ApiResult<u64>;
    fn open(&self, path: &str, line: Option<usize>, col: Option<usize>, focus: bool) -> ApiResult<String>;
    fn diagnostics(&self, doc: &str) -> ApiResult<Vec<Diagnostic>>;
    fn commands(&self) -> Vec<CommandInfo>;
    fn run_command(&self, id: &str, args: &serde_json::Value) -> ApiResult<serde_json::Value>;
    fn save(&self, doc: &str, force: bool) -> ApiResult<()>;
    fn annotate(&self, doc: &str, line: usize, kind: &str, text: &str, ttl_s: u64, owner: &str) -> ApiResult<()>;
    fn unannotate(&self, doc: &str, owner: &str, line: Option<usize>) -> ApiResult<usize>;
    fn proposal_pending(&self, proposal: &ProposalView);
    fn proposal_closed(&self, id: &str, status: &str);
    fn consent_needed(&self, req: &ConsentRequest);
    fn notice(&self, msg: &str);
}

type Job = Box<dyn FnOnce(&dyn UiOps) + Send>;

/// Thread-safe front: implements [`EditorApi`] by queueing to the UI thread.
pub struct Bridge {
    tx: Mutex<Sender<Job>>,
    wake: Arc<dyn Fn() + Send + Sync>,
    timeout: Duration,
}

/// UI-thread end of the queue.
pub struct Pump {
    rx: Receiver<Job>,
}

/// `wake` must make the UI thread call [`Pump::drain`] soon (floem: `register_ext_trigger`).
pub fn channel_pair(wake: impl Fn() + Send + Sync + 'static, timeout: Duration) -> (Bridge, Pump) {
    let (tx, rx) = channel();
    (Bridge { tx: Mutex::new(tx), wake: Arc::new(wake), timeout }, Pump { rx })
}

impl Pump {
    /// Run every queued job on the calling (UI) thread; returns how many ran.
    pub fn drain(&self, ops: &dyn UiOps) -> usize {
        let mut n = 0;
        while let Ok(job) = self.rx.try_recv() {
            job(ops);
            n += 1;
        }
        n
    }
}

impl Bridge {
    fn post(&self, job: Job) -> bool {
        let ok = self.tx.lock().map(|t| t.send(job).is_ok()).unwrap_or(false);
        if ok {
            (self.wake)();
        }
        ok
    }

    /// Run `f` on the UI thread and wait for its answer; `None` on timeout or if the UI is gone.
    fn call<T: Send + 'static>(&self, f: impl FnOnce(&dyn UiOps) -> T + Send + 'static) -> Option<T> {
        let (rtx, rrx) = channel();
        let cancelled = Arc::new(AtomicBool::new(false));
        let c2 = cancelled.clone();
        let posted = self.post(Box::new(move |ops| {
            if c2.load(Ordering::SeqCst) {
                return;
            }
            let _ = rtx.send(f(ops));
        }));
        if !posted {
            return None;
        }
        match rrx.recv_timeout(self.timeout) {
            Ok(v) => Some(v),
            Err(_) => {
                cancelled.store(true, Ordering::SeqCst);
                None
            }
        }
    }

    fn busy<T>() -> ApiResult<T> {
        Err(ApiError::Io("the editor did not answer in time (it may be showing a dialog)".into()))
    }
}

/// Reads of an unsaved file never touch the UI; cap them like the core does.
const MAX_FILE_READ: u64 = 8 * 1024 * 1024;

impl EditorApi for Bridge {
    fn docs(&self) -> Vec<DocInfo> {
        self.call(|u| u.docs()).unwrap_or_default()
    }
    fn active_doc(&self) -> Option<String> {
        self.call(|u| u.active_doc()).flatten()
    }
    fn workspace_folders(&self) -> Vec<String> {
        self.call(|u| u.workspace_folders()).unwrap_or_default()
    }
    fn snapshot(&self, doc: &str) -> Option<DocSnapshot> {
        let d = doc.to_string();
        self.call(move |u| u.snapshot(&d)).flatten()
    }
    fn read_file(&self, path: &str) -> ApiResult<String> {
        let md = std::fs::metadata(path).map_err(|e| ApiError::Io(e.to_string()))?;
        if !md.is_file() || md.len() > MAX_FILE_READ {
            return Err(ApiError::Invalid("not a regular file, or larger than 8 MiB".into()));
        }
        std::fs::read_to_string(path).map_err(|e| ApiError::Io(e.to_string()))
    }
    fn apply_edits(&self, doc: &str, expected_version: u64, edits: &[TextEdit], label: &str) -> ApiResult<u64> {
        let (d, e, l) = (doc.to_string(), edits.to_vec(), label.to_string());
        self.call(move |u| u.apply_edits(&d, expected_version, &e, &l)).unwrap_or_else(Self::busy)
    }
    fn open(&self, path: &str, line: Option<usize>, col: Option<usize>, focus: bool) -> ApiResult<String> {
        let p = path.to_string();
        self.call(move |u| u.open(&p, line, col, focus)).unwrap_or_else(Self::busy)
    }
    fn diagnostics(&self, doc: &str) -> ApiResult<Vec<Diagnostic>> {
        let d = doc.to_string();
        self.call(move |u| u.diagnostics(&d)).unwrap_or_else(Self::busy)
    }
    fn commands(&self) -> Vec<CommandInfo> {
        self.call(|u| u.commands()).unwrap_or_default()
    }
    fn run_command(&self, id: &str, args: &serde_json::Value) -> ApiResult<serde_json::Value> {
        let (i, a) = (id.to_string(), args.clone());
        self.call(move |u| u.run_command(&i, &a)).unwrap_or_else(Self::busy)
    }
    fn save(&self, doc: &str, force: bool) -> ApiResult<()> {
        let d = doc.to_string();
        self.call(move |u| u.save(&d, force)).unwrap_or_else(Self::busy)
    }
    fn annotate(&self, doc: &str, line: usize, kind: &str, text: &str, ttl_s: u64, owner: &str) -> ApiResult<()> {
        let (d, k, t, o) = (doc.to_string(), kind.to_string(), text.to_string(), owner.to_string());
        self.call(move |u| u.annotate(&d, line, &k, &t, ttl_s, &o)).unwrap_or_else(Self::busy)
    }
    fn unannotate(&self, doc: &str, owner: &str, line: Option<usize>) -> ApiResult<usize> {
        let (d, o) = (doc.to_string(), owner.to_string());
        self.call(move |u| u.unannotate(&d, &o, line)).unwrap_or_else(Self::busy)
    }
    fn proposal_pending(&self, proposal: &ProposalView) {
        let p = proposal.clone();
        self.post(Box::new(move |u| u.proposal_pending(&p)));
    }
    fn proposal_closed(&self, id: &str, status: &str) {
        let (i, s) = (id.to_string(), status.to_string());
        self.post(Box::new(move |u| u.proposal_closed(&i, &s)));
    }
    fn consent_needed(&self, req: &ConsentRequest) {
        let r = req.clone();
        self.post(Box::new(move |u| u.consent_needed(&r)));
    }
    fn notice(&self, msg: &str) {
        let m = msg.to_string();
        self.post(Box::new(move |u| u.notice(&m)));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fake::FakeEditor;
    use std::sync::atomic::AtomicUsize;

    /// Test UI: a `FakeEditor` plus a record of notifications.
    struct Ui {
        ed: FakeEditor,
        pending: std::cell::RefCell<Vec<String>>,
        closed: std::cell::RefCell<Vec<(String, String)>>,
        consent: std::cell::RefCell<Vec<String>>,
        delay: Duration,
    }
    impl Ui {
        fn new(delay: Duration) -> Self {
            Self { ed: FakeEditor::new(), pending: Default::default(), closed: Default::default(), consent: Default::default(), delay }
        }
    }
    impl UiOps for Ui {
        fn docs(&self) -> Vec<DocInfo> {
            std::thread::sleep(self.delay);
            self.ed.docs()
        }
        fn active_doc(&self) -> Option<String> {
            self.ed.active_doc()
        }
        fn workspace_folders(&self) -> Vec<String> {
            self.ed.workspace_folders()
        }
        fn snapshot(&self, d: &str) -> Option<DocSnapshot> {
            self.ed.snapshot(d)
        }
        fn apply_edits(&self, d: &str, v: u64, e: &[TextEdit], l: &str) -> ApiResult<u64> {
            self.ed.apply_edits(d, v, e, l)
        }
        fn open(&self, p: &str, l: Option<usize>, c: Option<usize>, f: bool) -> ApiResult<String> {
            self.ed.open(p, l, c, f)
        }
        fn diagnostics(&self, d: &str) -> ApiResult<Vec<Diagnostic>> {
            self.ed.diagnostics(d)
        }
        fn commands(&self) -> Vec<CommandInfo> {
            self.ed.commands()
        }
        fn run_command(&self, i: &str, a: &serde_json::Value) -> ApiResult<serde_json::Value> {
            self.ed.run_command(i, a)
        }
        fn save(&self, d: &str, f: bool) -> ApiResult<()> {
            self.ed.save(d, f)
        }
        fn annotate(&self, d: &str, l: usize, k: &str, t: &str, s: u64, o: &str) -> ApiResult<()> {
            self.ed.annotate(d, l, k, t, s, o)
        }
        fn unannotate(&self, d: &str, o: &str, l: Option<usize>) -> ApiResult<usize> {
            self.ed.unannotate(d, o, l)
        }
        fn notice(&self, _m: &str) {}
        fn proposal_pending(&self, p: &ProposalView) {
            self.pending.borrow_mut().push(p.id.clone());
        }
        fn proposal_closed(&self, id: &str, status: &str) {
            self.closed.borrow_mut().push((id.into(), status.into()));
        }
        fn consent_needed(&self, r: &ConsentRequest) {
            self.consent.borrow_mut().push(r.client.clone());
        }
    }

    /// Runs a "UI thread" that drains the pump until told to stop.
    fn ui_thread(pump: Pump, ui: Ui) -> (Arc<AtomicBool>, std::thread::JoinHandle<Ui>) {
        let stop = Arc::new(AtomicBool::new(false));
        let s2 = stop.clone();
        let h = std::thread::spawn(move || {
            while !s2.load(Ordering::SeqCst) {
                pump.drain(&ui);
                std::thread::sleep(Duration::from_millis(2));
            }
            pump.drain(&ui);
            ui
        });
        (stop, h)
    }

    #[test]
    fn calls_are_marshalled_to_the_ui_thread_and_answered() {
        let wakes = Arc::new(AtomicUsize::new(0));
        let w2 = wakes.clone();
        let (bridge, pump) = channel_pair(move || { w2.fetch_add(1, Ordering::SeqCst); }, Duration::from_secs(2));
        let ui = Ui::new(Duration::ZERO);
        let id = ui.ed.add_doc(Some("/w/a.txt"), "hello");
        let (stop, h) = ui_thread(pump, ui);
        assert_eq!(bridge.docs().len(), 1);
        assert_eq!(bridge.snapshot(&id).unwrap().text, "hello");
        let v = bridge.snapshot(&id).unwrap().info.version;
        let edits = [TextEdit { start: 0, end: 5, new_text: "bye".into() }];
        bridge.apply_edits(&id, v, &edits, "AI edit: test").unwrap();
        assert_eq!(bridge.snapshot(&id).unwrap().text, "bye");
        assert_eq!(bridge.apply_edits(&id, v, &edits, "x"), Err(ApiError::Stale { current: bridge.snapshot(&id).unwrap().info.version }));
        assert!(wakes.load(Ordering::SeqCst) >= 5, "the UI thread is woken for every request");
        stop.store(true, Ordering::SeqCst);
        h.join().unwrap();
    }

    #[test]
    fn a_blocked_ui_times_out_and_the_late_request_is_not_applied() {
        let (bridge, pump) = channel_pair(|| {}, Duration::from_millis(80));
        let ui = Ui::new(Duration::ZERO);
        let id = ui.ed.add_doc(None, "abc");
        let v = ui.ed.snapshot(&id).unwrap().info.version;
        let edits = [TextEdit { start: 0, end: 3, new_text: "XYZ".into() }];
        // Nobody drains: the call must come back with a timeout error.
        let t0 = std::time::Instant::now();
        let r = bridge.apply_edits(&id, v, &edits, "AI edit");
        assert!(matches!(r, Err(ApiError::Io(_))), "{r:?}");
        assert!(t0.elapsed() < Duration::from_secs(2));
        assert!(bridge.docs().is_empty(), "list calls degrade to empty, not a hang");
        // The UI wakes up late: the cancelled edit is skipped.
        pump.drain(&ui);
        assert_eq!(ui.ed.snapshot(&id).unwrap().text, "abc");
    }

    #[test]
    fn a_dropped_ui_fails_fast() {
        let (bridge, pump) = channel_pair(|| {}, Duration::from_secs(5));
        drop(pump);
        let t0 = std::time::Instant::now();
        assert!(bridge.active_doc().is_none());
        assert!(t0.elapsed() < Duration::from_secs(1));
    }

    #[test]
    fn notifications_do_not_wait_and_reach_the_ui() {
        let (bridge, pump) = channel_pair(|| {}, Duration::from_secs(1));
        let ui = Ui::new(Duration::ZERO);
        let view = ProposalView { id: "p1".into(), client: "c".into(), doc: "d".into(), summary: "s".into(), hunks: vec![] };
        let t0 = std::time::Instant::now();
        bridge.proposal_pending(&view);
        bridge.proposal_closed("p1", "accepted");
        bridge.consent_needed(&ConsentRequest { client: "claude-code".into(), ..Default::default() });
        assert!(t0.elapsed() < Duration::from_millis(100), "no waiting for a UI that is not draining");
        assert_eq!(pump.drain(&ui), 3);
        assert_eq!(*ui.pending.borrow(), vec!["p1".to_string()]);
        assert_eq!(*ui.closed.borrow(), vec![("p1".to_string(), "accepted".to_string())]);
        assert_eq!(*ui.consent.borrow(), vec!["claude-code".to_string()]);
    }

    #[test]
    fn read_file_is_capped_and_never_needs_the_ui() {
        let (bridge, _pump) = channel_pair(|| {}, Duration::from_millis(50));
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("a.txt");
        std::fs::write(&p, "hi").unwrap();
        assert_eq!(bridge.read_file(p.to_str().unwrap()).unwrap(), "hi");
        assert!(bridge.read_file(dir.path().to_str().unwrap()).is_err());
        assert!(matches!(bridge.read_file(dir.path().join("nope").to_str().unwrap()), Err(ApiError::Io(_))));
    }
}
