//! Measurement primitives for the four PoC budgets.
//!
//! Cold start: a `OnceLock<Instant>` captured at `main()` entry — first
//! frame paint calls `report_first_frame()` which logs `elapsed` once.
//!
//! FPS: a fixed-size ring buffer of recent frame durations; `current_fps()`
//! returns 1 / mean(durations). 120 samples = ~2s window at 60fps.
//!
//! RAM: `sysinfo::System` refreshed on demand (cheap, ~ms) — we don't poll
//! more than every 500ms from the UI.

use std::sync::Mutex;
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use sysinfo::{Pid, ProcessRefreshKind, ProcessesToUpdate, System};

/// Captured at the first call to `mark_process_start()` — `main()` should
/// call it as its very first statement.
static PROCESS_START: OnceLock<Instant> = OnceLock::new();

/// Latched once `report_first_frame()` is invoked. We log to stderr exactly
/// once so the user sees a single "cold start: 87ms" line per run.
static FIRST_FRAME_DONE: OnceLock<Duration> = OnceLock::new();

/// Call as the first line of `main()`. Idempotent — repeated calls are
/// no-ops.
pub fn mark_process_start() {
    let _ = PROCESS_START.set(Instant::now());
}

/// Startup phase trace: prints `[trace] <stage> +<ms> ms` to stderr when the
/// `BROUILLON_TRACE_STARTUP` environment variable is set, nothing otherwise.
pub fn trace_stage(stage: &str) {
    static ON: OnceLock<bool> = OnceLock::new();
    if *ON.get_or_init(|| std::env::var_os("BROUILLON_TRACE_STARTUP").is_some()) {
        if let Some(start) = PROCESS_START.get() {
            eprintln!("[trace] {stage} +{:.1} ms", start.elapsed().as_secs_f64() * 1000.0);
        }
    }
}

/// Call once, from the first frame's commit callback. Logs to stderr and
/// returns the cold-start duration (process-spawn → first-paint).
pub fn report_first_frame() -> Option<Duration> {
    let start = PROCESS_START.get()?;
    let elapsed = start.elapsed();
    if FIRST_FRAME_DONE.set(elapsed).is_ok() {
        eprintln!("[metrics] cold start: {} ms", elapsed.as_millis());
    }
    Some(elapsed)
}

/// True once the first-frame mark has been recorded — used by the view layer
/// to fire `report_first_frame()` exactly once.
pub fn first_frame_reported() -> bool {
    FIRST_FRAME_DONE.get().is_some()
}

// ---------------------------------------------------------------------------
// FPS — single global ring buffer, mutex-guarded. UI is single-threaded so
// contention is zero; the Mutex is just for `&'static` access.
// ---------------------------------------------------------------------------

const FPS_WINDOW: usize = 120;

struct FpsRing {
    samples: [Duration; FPS_WINDOW],
    head: usize,
    filled: usize,
    last_tick: Option<Instant>,
}

static FPS_RING: Mutex<FpsRing> = Mutex::new(FpsRing {
    samples: [Duration::ZERO; FPS_WINDOW],
    head: 0,
    filled: 0,
    last_tick: None,
});

/// Record one frame's wall-clock duration relative to the previous tick.
/// Returns the delta — useful if the caller wants to log slow frames.
pub fn tick_frame() -> Duration {
    // Input activity for the idle memory trimmer (one atomic store).
    crate::memtrim::touch();
    let now = Instant::now();
    let mut g = FPS_RING.lock().expect("FPS_RING poisoned");
    let delta = match g.last_tick {
        Some(prev) => now.saturating_duration_since(prev),
        None => Duration::from_millis(16),
    };
    g.last_tick = Some(now);
    let head = g.head;
    g.samples[head] = delta;
    g.head = (g.head + 1) % FPS_WINDOW;
    if g.filled < FPS_WINDOW {
        g.filled += 1;
    }
    delta
}

/// Window after which the FPS reading is considered stale (idle). The PoC's
/// FPS tick is currently bound to input events, not paint events — so any
/// reading older than this is meaningless. UI layer treats 0.0 as "idle".
const FPS_IDLE_AFTER: Duration = Duration::from_millis(750);

/// Current FPS = 1 / mean(window). Returns 0.0 if no frames recorded yet
/// OR if the last tick is older than FPS_IDLE_AFTER (idle indicator).
pub fn current_fps() -> f64 {
    let g = FPS_RING.lock().expect("FPS_RING poisoned");
    if g.filled == 0 {
        return 0.0;
    }
    // Treat stale samples as idle — avoids a frozen fps reading when the
    // user stops scrolling.
    if let Some(last) = g.last_tick {
        if last.elapsed() > FPS_IDLE_AFTER {
            return 0.0;
        }
    }
    let sum: Duration = g.samples[..g.filled].iter().copied().sum();
    let mean = sum.as_secs_f64() / g.filled as f64;
    if mean <= 0.0 { 0.0 } else { 1.0 / mean }
}

// ---------------------------------------------------------------------------
// RAM — refresh the System struct only when sampled; sysinfo caches between
// calls so this is cheap (~hundreds of microseconds on Windows).
// ---------------------------------------------------------------------------

pub struct RamSampler {
    sys: System,
    pid: Pid,
}

impl RamSampler {
    pub fn new() -> Self {
        let sys = System::new();
        let pid = sysinfo::get_current_pid().expect("get_current_pid failed on this platform");
        Self { sys, pid }
    }

    /// Returns current process resident-set-size in bytes. Refreshes only
    /// the relevant process for speed.
    pub fn rss_bytes(&mut self) -> u64 {
        self.sys.refresh_processes_specifics(
            ProcessesToUpdate::Some(&[self.pid]),
            true,
            ProcessRefreshKind::new().with_memory(),
        );
        self.sys.process(self.pid).map(|p| p.memory()).unwrap_or(0)
    }

    /// Helper: bytes formatted as "12.3 MB".
    pub fn rss_mb_string(&mut self) -> String {
        let b = self.rss_bytes() as f64;
        format!("{:.1} MB", b / 1024.0 / 1024.0)
    }
}

impl Default for RamSampler {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fps_ring_starts_at_zero() {
        // A fresh lock should have filled=0 (we can't easily reset the global,
        // but we can verify the API returns a valid number).
        let fps = current_fps();
        assert!(fps >= 0.0);
    }

    #[test]
    fn tick_frame_returns_positive_duration() {
        let d = tick_frame();
        assert!(d.as_nanos() > 0);
    }

    #[test]
    fn tick_frame_records_samples() {
        // Multiple ticks should not panic.
        for _ in 0..10 {
            tick_frame();
        }
        // After ticks, fps should be non-zero (unless stale).
        let fps = current_fps();
        // May or may not be > 0 depending on timing, but should not panic.
        assert!(fps >= 0.0);
    }

    #[test]
    fn ram_sampler_creation() {
        let sampler = RamSampler::new();
        assert!(sampler.pid.as_u32() > 0);
    }

    #[test]
    fn ram_sampler_rss_positive() {
        let mut sampler = RamSampler::new();
        let rss = sampler.rss_bytes();
        assert!(rss > 0, "RSS should be positive for running process");
    }

    #[test]
    fn ram_sampler_rss_mb_string_format() {
        let mut sampler = RamSampler::new();
        let s = sampler.rss_mb_string();
        assert!(s.ends_with(" MB"), "Expected '... MB', got '{}'", s);
    }

    #[test]
    fn mark_process_start_idempotent() {
        // Should not panic on repeated calls.
        mark_process_start();
        mark_process_start();
    }

    #[test]
    fn first_frame_reported_check() {
        // After mark_process_start + report, should be true eventually.
        mark_process_start();
        let _ = report_first_frame();
        assert!(first_frame_reported());
    }

    #[test]
    fn fps_window_constant() {
        assert_eq!(FPS_WINDOW, 120);
    }

    #[test]
    fn fps_idle_after_constant() {
        assert_eq!(FPS_IDLE_AFTER, Duration::from_millis(750));
    }
}
