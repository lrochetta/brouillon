//! Idle memory trimmer and honest memory readout.
//!
//! * [`IdleGate`] is the pure state machine (fake-clock testable): "has the user been idle for
//!   N ms, and did we already trim since the last activity?".
//! * [`touch`] is the hot-path hook (key/mouse): one atomic store, plus an `unpark` only on the
//!   first activity after a trim. No timer, no repaint, no per-frame work.
//! * [`spawn`] starts ONE parked thread that sleeps until the idle deadline, trims once, then
//!   parks (zero wakeups) until the next [`touch`].
//! * [`trim_now`] returns freed pages to the OS: the allocator hook (mimalloc `mi_collect`),
//!   `HeapCompact` on every process heap (C code such as tree-sitter and the GPU/driver stack use
//!   the Windows heap, not our global allocator) and, only if asked, `EmptyWorkingSet`.
//! * [`MemStats`] reports working set AND private bytes, so the status bar can tell the truth.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering::SeqCst};
use std::sync::{Mutex, OnceLock};
use std::thread::Thread;
use std::time::{Duration, Instant};

// ---------------------------------------------------------------------------------------------
// Pure state machine
// ---------------------------------------------------------------------------------------------

/// What the trimmer thread should do next.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Decision {
    /// Not idle long enough: sleep this many ms, then ask again.
    WaitMs(u64),
    /// Idle long enough and not trimmed since the last activity: trim now.
    Trim,
    /// Already trimmed since the last activity: park until the next activity.
    Park,
}

#[derive(Debug, Clone)]
pub struct IdleGate {
    threshold_ms: u64,
    last_activity_ms: u64,
    trimmed: bool,
}

impl IdleGate {
    pub fn new(threshold_ms: u64, now_ms: u64) -> Self {
        Self { threshold_ms: threshold_ms.max(1), last_activity_ms: now_ms, trimmed: false }
    }
    /// Record activity at `now_ms` (never moves time back). Returns true when the gate was
    /// parked after a trim, i.e. the caller must wake the trimmer thread.
    pub fn touch(&mut self, now_ms: u64) -> bool {
        self.last_activity_ms = self.last_activity_ms.max(now_ms);
        std::mem::replace(&mut self.trimmed, false)
    }
    pub fn poll(&mut self, now_ms: u64) -> Decision {
        if self.trimmed {
            return Decision::Park;
        }
        let idle = now_ms.saturating_sub(self.last_activity_ms);
        if idle >= self.threshold_ms {
            self.trimmed = true;
            Decision::Trim
        } else {
            Decision::WaitMs(self.threshold_ms - idle)
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Global hook + the single parked thread
// ---------------------------------------------------------------------------------------------

static EPOCH: OnceLock<Instant> = OnceLock::new();
static LAST_ACTIVITY_MS: AtomicU64 = AtomicU64::new(0);
/// True while the trimmer thread is parked after a trim (touch must wake it).
static PARKED: AtomicBool = AtomicBool::new(false);
static THREAD: OnceLock<Thread> = OnceLock::new();
static ALLOCATOR_COLLECT: OnceLock<fn()> = OnceLock::new();
static LAST_TRIM: Mutex<Option<TrimReport>> = Mutex::new(None);

fn now_ms() -> u64 {
    EPOCH.get_or_init(Instant::now).elapsed().as_millis() as u64
}

/// Register the allocator's "return free pages to the OS" call (e.g. mimalloc's `mi_collect(true)`).
pub fn set_allocator_collect(f: fn()) {
    let _ = ALLOCATOR_COLLECT.set(f);
}

/// Activity hook for key/mouse events. Cheap enough for a per-event call.
#[inline]
pub fn touch() {
    LAST_ACTIVITY_MS.store(now_ms(), SeqCst);
    if PARKED.load(SeqCst) && PARKED.swap(false, SeqCst) {
        if let Some(t) = THREAD.get() {
            t.unpark();
        }
    }
}

/// Start the one trimmer thread (idempotent). `working_set` also empties the working set.
pub fn spawn(idle: Duration, working_set: bool) {
    if THREAD.get().is_some() {
        return;
    }
    LAST_ACTIVITY_MS.store(now_ms(), SeqCst);
    let threshold = (idle.as_millis() as u64).max(1);
    let (tx, rx) = std::sync::mpsc::channel();
    let spawned = std::thread::Builder::new().name("idle-trim".into()).spawn(move || {
        let _ = tx.send(std::thread::current());
        let mut gate = IdleGate::new(threshold, now_ms());
        loop {
            // Reaching this line means: start, or a wake-up caused by activity. Fast-forward the gate.
            gate.touch(LAST_ACTIVITY_MS.load(SeqCst));
            match gate.poll(now_ms()) {
                Decision::WaitMs(ms) => std::thread::park_timeout(Duration::from_millis(ms)),
                Decision::Trim => {
                    let report = trim_now(working_set);
                    *LAST_TRIM.lock().unwrap_or_else(|e| e.into_inner()) = Some(report);
                    PARKED.store(true, SeqCst);
                    // Activity that landed between the idle check and the store above saw
                    // PARKED == false and sent no wake-up: re-check before parking.
                    if LAST_ACTIVITY_MS.load(SeqCst).saturating_add(threshold) > now_ms() {
                        PARKED.store(false, SeqCst);
                        continue;
                    }
                    while PARKED.load(SeqCst) {
                        std::thread::park();
                    }
                }
                Decision::Park => std::thread::park(),
            }
        }
    });
    if spawned.is_ok() {
        if let Ok(t) = rx.recv() {
            let _ = THREAD.set(t);
        }
    }
}

/// True once the idle trimmer was started (i.e. `[perf] idle_trim` is on).
pub fn enabled() -> bool {
    THREAD.get().is_some()
}

/// Bytes the process must have grown by since the last trim before a growth-triggered trim is worth its pause.
pub const GROWTH_TRIGGER: u64 = 32 * 1024 * 1024;
/// Samples of (almost) unchanged memory that count as "steady".
pub const STILL_TICKS: u32 = 3;
/// Change below this between two samples counts as unchanged.
pub const STILL_BAND: u64 = 2 * 1024 * 1024;

/// Second trigger (from the metrics tick): memory grew by [`GROWTH_TRIGGER`] and then stood still, i.e. a big
/// transient (open, reload, Replace All, diff) ended. Feeds the same [`trim_now`].
#[derive(Debug, Default)]
pub struct GrowthTrim {
    floor: Option<u64>,
    last: u64,
    still: u32,
}

impl GrowthTrim {
    /// Feed one sample (bytes). Returns true when the caller should trim now.
    pub fn tick(&mut self, bytes: u64) -> bool {
        let floor = *self.floor.get_or_insert(bytes);
        if bytes.abs_diff(self.last) <= STILL_BAND {
            self.still = self.still.saturating_add(1);
        } else {
            self.still = 0;
        }
        self.last = bytes;
        if bytes >= floor + GROWTH_TRIGGER && self.still >= STILL_TICKS {
            self.still = 0;
            self.floor = None; // the next sample (after the trim) becomes the new floor
            return true;
        }
        // A steady fall (the OS trimmed, a file was closed) lowers the floor so a later regrowth is seen.
        if self.still >= STILL_TICKS && bytes < floor {
            self.floor = Some(bytes);
        }
        false
    }
}

/// Report of the last idle trim, if one ran.
pub fn last_trim() -> Option<TrimReport> {
    *LAST_TRIM.lock().unwrap_or_else(|e| e.into_inner())
}

// ---------------------------------------------------------------------------------------------
// Memory readout and trim (Windows implementation; other OS: allocator hook only)
// ---------------------------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct MemStats {
    /// Resident pages (what Task Manager shows by default).
    pub working_set: u64,
    /// Committed private memory (`PrivateMemorySize64`): the truthful footprint.
    pub private_bytes: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TrimReport {
    pub before: MemStats,
    pub after: MemStats,
    pub heaps_compacted: u32,
    pub working_set_emptied: bool,
}

#[cfg(windows)]
pub fn mem_stats() -> MemStats {
    use windows_sys::Win32::System::ProcessStatus::{K32GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS_EX};
    use windows_sys::Win32::System::Threading::GetCurrentProcess;
    // SAFETY: plain out-struct filled by the OS; the size field is set as the API requires.
    unsafe {
        let mut c: PROCESS_MEMORY_COUNTERS_EX = std::mem::zeroed();
        c.cb = std::mem::size_of::<PROCESS_MEMORY_COUNTERS_EX>() as u32;
        if K32GetProcessMemoryInfo(GetCurrentProcess(), &mut c as *mut _ as *mut _, c.cb) != 0 {
            MemStats { working_set: c.WorkingSetSize as u64, private_bytes: c.PrivateUsage as u64 }
        } else {
            MemStats::default()
        }
    }
}

#[cfg(not(windows))]
pub fn mem_stats() -> MemStats {
    MemStats::default()
}

/// Ask the allocator and the OS to give back free pages now. Never frees live data.
pub fn trim_now(working_set: bool) -> TrimReport {
    let before = mem_stats();
    if let Some(f) = ALLOCATOR_COLLECT.get() {
        f();
    }
    let heaps = compact_process_heaps();
    #[cfg(windows)]
    if working_set {
        use windows_sys::Win32::System::ProcessStatus::K32EmptyWorkingSet;
        use windows_sys::Win32::System::Threading::GetCurrentProcess;
        // SAFETY: no pointers; pages fault back in on demand.
        unsafe {
            K32EmptyWorkingSet(GetCurrentProcess());
        }
    }
    TrimReport { before, after: mem_stats(), heaps_compacted: heaps, working_set_emptied: cfg!(windows) && working_set }
}

#[cfg(windows)]
fn compact_process_heaps() -> u32 {
    use windows_sys::Win32::System::Memory::{GetProcessHeaps, HeapCompact};
    // SAFETY: GetProcessHeaps fills at most `len` handles; HeapCompact on a heap destroyed in
    // between fails (returns 0) instead of crashing.
    unsafe {
        let mut handles = [std::ptr::null_mut(); 256];
        let n = (GetProcessHeaps(handles.len() as u32, handles.as_mut_ptr()) as usize).min(handles.len());
        let mut done = 0;
        for h in &handles[..n] {
            if HeapCompact(*h, 0) != 0 {
                done += 1;
            }
        }
        done
    }
}

#[cfg(not(windows))]
fn compact_process_heaps() -> u32 {
    0
}

pub fn format_mb(b: u64) -> String {
    format!("{:.1} MB", b as f64 / 1024.0 / 1024.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn waits_then_trims_once_then_parks() {
        let mut g = IdleGate::new(30_000, 0);
        assert_eq!(g.poll(1_000), Decision::WaitMs(29_000));
        assert_eq!(g.poll(29_999), Decision::WaitMs(1));
        assert_eq!(g.poll(30_000), Decision::Trim);
        assert_eq!(g.poll(31_000), Decision::Park, "one trim per idle period");
        assert_eq!(g.poll(900_000), Decision::Park);
    }

    #[test]
    fn activity_rearms_and_reports_the_wake() {
        let mut g = IdleGate::new(10_000, 0);
        assert!(!g.touch(5_000), "not parked yet: no wake needed");
        assert_eq!(g.poll(12_000), Decision::WaitMs(3_000), "deadline moved with the activity");
        assert_eq!(g.poll(15_000), Decision::Trim);
        assert!(g.touch(20_000), "was parked after the trim: wake the thread");
        assert_eq!(g.poll(25_000), Decision::WaitMs(5_000));
        assert_eq!(g.poll(30_000), Decision::Trim, "second idle period trims again");
    }

    const MB: u64 = 1024 * 1024;

    #[test]
    fn growth_trim_fires_once_memory_grew_and_then_stood_still() {
        let mut t = GrowthTrim::default();
        assert!(!t.tick(60 * MB));
        assert!(!t.tick(61 * MB));
        for m in [100, 160, 220, 260] {
            assert!(!t.tick(m * MB), "still growing at {m} MB");
        }
        assert!(!t.tick(261 * MB));
        assert!(!t.tick(261 * MB));
        assert!(t.tick(261 * MB), "steady after growth");
        for _ in 0..10 {
            assert!(!t.tick(120 * MB));
        }
    }

    #[test]
    fn growth_trim_ignores_small_changes_and_steady_state() {
        let mut t = GrowthTrim::default();
        for i in 0..50u64 {
            assert!(!t.tick(80 * MB + (i % 3) * MB), "sample {i}");
        }
        let mut t = GrowthTrim::default();
        assert!(!t.tick(50 * MB));
        for _ in 0..10 {
            assert!(!t.tick(70 * MB));
        }
    }

    #[test]
    fn trim_now_is_callable() {
        trim_now(false);
        drop(vec![7u8; 1 << 20]);
        trim_now(false);
    }

    #[test]
    fn activity_never_moves_time_back_and_threshold_is_nonzero() {
        let mut g = IdleGate::new(0, 100);
        g.touch(50);
        assert_eq!(g.poll(100), Decision::WaitMs(1));
    }

    #[cfg(windows)]
    #[test]
    fn reports_plausible_numbers_and_trim_does_not_grow_private() {
        let s = mem_stats();
        assert!(s.working_set > 1 << 20 && s.private_bytes > 1 << 20);
        let r = trim_now(false);
        assert!(r.after.private_bytes <= r.before.private_bytes + (4 << 20));
    }
}
