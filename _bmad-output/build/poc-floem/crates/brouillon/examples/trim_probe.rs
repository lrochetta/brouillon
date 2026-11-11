//! Headless allocator / trimmer probe (no window). Uses the SAME global allocator selection as the app
//! (default = mimalloc; `--no-default-features` = Windows system heap), so the two variants can be compared:
//!   cargo run --release -p brouillon --example trim_probe                          (mimalloc 3.x)
//!   cargo run --release -p brouillon --example trim_probe --no-default-features    (system heap)
//!   cargo run --release -p brouillon --example trim_probe --features mimalloc-v2   (mimalloc 2.x)
//! Prints working set / private bytes after each phase. Phases model the editor's transient buffers:
//! a big one-shot buffer (file load), many mid-size blocks with a few survivors (find results, spans),
//! blocks allocated on a worker thread and freed on the main thread, and C-style system-heap use.
use std::alloc::{GlobalAlloc, Layout, System};
use std::time::Instant;

use brouillon::memtrim::{format_mb, mem_stats, set_allocator_collect, trim_now};

#[cfg(feature = "alloc-mimalloc")]
#[global_allocator]
static A: mimalloc::MiMalloc = mimalloc::MiMalloc;

fn show(what: &str) {
    let m = mem_stats();
    println!("{what:<58} ws {:>10}   private {:>10}", format_mb(m.working_set), format_mb(m.private_bytes));
}

fn main() {
    #[cfg(feature = "alloc-mimalloc")]
    {
        println!("allocator: mimalloc ({})", if cfg!(feature = "mimalloc-v2") { "2.x" } else { "3.x" });
        set_allocator_collect(|| unsafe { libmimalloc_sys::mi_collect(true) });
    }
    #[cfg(not(feature = "alloc-mimalloc"))]
    println!("allocator: Windows system heap (LFH)");

    show("start");

    // Phase 1: one 256 MB buffer (file load), touched, then freed.
    {
        // black_box: without it the optimizer removes the whole buffer under the system allocator.
        let mut big = std::hint::black_box(vec![0u8; 256 << 20]);
        for i in (0..big.len()).step_by(4096) {
            big[i] = 1;
        }
        std::hint::black_box(&big);
        show("1. holding 256 MB buffer");
    }
    show("1. buffer freed");

    // Phase 2: 150k blocks of 1..2 KB (find results / span caches), keep every 20th (fragmentation).
    let mut keep: Vec<Vec<u8>> = Vec::new();
    {
        let mut all: Vec<Vec<u8>> = Vec::with_capacity(150_000);
        for i in 0..150_000usize {
            let mut v = vec![0u8; 1024 + (i % 1024)];
            v[0] = 1;
            all.push(v);
        }
        show("2. holding 150k mid-size blocks (~225 MB)");
        for (i, v) in all.into_iter().enumerate() {
            if i % 20 == 0 {
                keep.push(v);
            }
        }
    }
    show("2. freed 95%, 5% survive (fragmented)");

    // Phase 3: allocated on a worker thread, freed on the main thread (cross-thread free).
    let worker: Vec<Vec<u8>> = std::thread::spawn(|| (0..40_000).map(|i| vec![1u8; 2048 + (i % 512)]).collect()).join().unwrap();
    show("3. holding 40k blocks made by a worker thread");
    drop(worker);
    show("3. freed on the main thread (worker thread gone)");

    // Phase 4: C-style use of the process heap (tree-sitter, GPU driver): bypasses the global allocator.
    let layout = Layout::from_size_align(512, 16).unwrap();
    let sys: Vec<*mut u8> = (0..300_000).map(|_| unsafe { let p = System.alloc(layout); *p = 1; p }).collect();
    show("4. holding 300k x 512 B on the Windows heap (~150 MB)");
    for (i, p) in sys.iter().enumerate() {
        if i % 10 != 0 {
            unsafe { System.dealloc(*p, layout) };
        }
    }
    show("4. freed 90% of them");

    println!("--- trim (collect + HeapCompact), measured ---");
    let t = Instant::now();
    let r = trim_now(false);
    println!("trim took {:?}, heaps compacted: {}", t.elapsed(), r.heaps_compacted);
    show("after trim_now(working_set = false)");
    let t = Instant::now();
    trim_now(true);
    println!("trim+EmptyWorkingSet took {:?}", t.elapsed());
    show("after trim_now(working_set = true)");
    // Touch the survivors: shows the pages that fault back.
    let sum: usize = keep.iter().map(|v| v[0] as usize).sum();
    show(&format!("after touching survivors again (sum {sum})"));
    for p in sys.iter().step_by(10) {
        unsafe { System.dealloc(*p, layout) };
    }
}
