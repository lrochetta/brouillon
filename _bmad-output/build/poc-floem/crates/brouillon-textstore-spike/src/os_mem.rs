//! Process memory as the OS sees it (no GUI needed). Windows: K32GetProcessMemoryInfo, declared by hand so
//! the spike pulls no extra crate. Elsewhere: /proc/self/statm.

#[derive(Debug, Clone, Copy, Default)]
pub struct ProcMem {
    /// Resident pages incl. file-backed mapped pages (what Task Manager "Working set" shows).
    pub working_set: u64,
    /// Private committed bytes (Windows PrivateUsage; excludes mapped file views).
    pub private: u64,
    pub peak_working_set: u64,
    /// Peak commit charge (Windows PeakPagefileUsage).
    pub peak_private: u64,
}

#[cfg(windows)]
pub fn sample() -> ProcMem {
    #[repr(C)]
    #[derive(Default)]
    struct Counters {
        cb: u32,
        page_fault_count: u32,
        peak_working_set_size: usize,
        working_set_size: usize,
        quota_peak_paged_pool_usage: usize,
        quota_paged_pool_usage: usize,
        quota_peak_non_paged_pool_usage: usize,
        quota_non_paged_pool_usage: usize,
        pagefile_usage: usize,
        peak_pagefile_usage: usize,
        private_usage: usize,
    }
    #[link(name = "kernel32")]
    extern "system" {
        fn GetCurrentProcess() -> isize;
        fn K32GetProcessMemoryInfo(h: isize, c: *mut Counters, cb: u32) -> i32;
    }
    let mut c = Counters::default();
    c.cb = std::mem::size_of::<Counters>() as u32;
    // SAFETY: plain FFI call with a correctly sized, zero-initialised out struct.
    let ok = unsafe { K32GetProcessMemoryInfo(GetCurrentProcess(), &mut c, c.cb) };
    if ok == 0 {
        return ProcMem::default();
    }
    ProcMem {
        working_set: c.working_set_size as u64,
        private: c.private_usage as u64,
        peak_working_set: c.peak_working_set_size as u64,
        peak_private: c.peak_pagefile_usage as u64,
    }
}

#[cfg(not(windows))]
pub fn sample() -> ProcMem {
    let s = std::fs::read_to_string("/proc/self/statm").unwrap_or_default();
    let mut it = s.split_whitespace().skip(1);
    let rss = it.next().and_then(|v| v.parse::<u64>().ok()).unwrap_or(0) * 4096;
    let shared = it.next().and_then(|v| v.parse::<u64>().ok()).unwrap_or(0) * 4096;
    ProcMem { working_set: rss, private: rss.saturating_sub(shared), peak_working_set: rss, peak_private: rss }
}
