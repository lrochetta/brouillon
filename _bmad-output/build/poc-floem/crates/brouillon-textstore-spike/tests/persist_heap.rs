//! Heap-peak assertions on a 100 MB lazy document: save, autosave (journal), journal replay and close/reopen must
//! stay within a few MB of heap above the baseline (a rope-based path would need >= 100 MB for each).
//! One test only, so no other thread of this binary allocates while we measure.
use brouillon_textstore_spike::lazy::persist::*;
use brouillon_textstore_spike::lazy::{Config, LazyStore};
use brouillon_textstore_spike::store::TextStore;
use std::alloc::{GlobalAlloc, Layout, System};
use std::io::Write;
use std::sync::atomic::{AtomicUsize, Ordering::SeqCst};
use std::time::Instant;

struct Counting;
static CUR: AtomicUsize = AtomicUsize::new(0);
static PEAK: AtomicUsize = AtomicUsize::new(0);
fn bump(n: usize) {
    let c = CUR.fetch_add(n, SeqCst) + n;
    PEAK.fetch_max(c, SeqCst);
}
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, l: Layout) -> *mut u8 {
        bump(l.size());
        System.alloc(l)
    }
    unsafe fn dealloc(&self, p: *mut u8, l: Layout) {
        CUR.fetch_sub(l.size(), SeqCst);
        System.dealloc(p, l)
    }
    unsafe fn realloc(&self, p: *mut u8, l: Layout, n: usize) -> *mut u8 {
        if n >= l.size() {
            bump(n - l.size());
        } else {
            CUR.fetch_sub(l.size() - n, SeqCst);
        }
        System.realloc(p, l, n)
    }
}
#[global_allocator]
static A: Counting = Counting;

const LIMIT: usize = 6 << 20;

fn peak_since(base: usize) -> usize {
    PEAK.load(SeqCst).saturating_sub(base)
}
fn reset() -> usize {
    let c = CUR.load(SeqCst);
    PEAK.store(c, SeqCst);
    c
}

#[test]
fn hundred_mb_save_autosave_replay_and_reopen_stay_flat() {
    let d = tempfile::tempdir().unwrap();
    let p = d.path().join("huge.log");
    {
        let mut w = std::io::BufWriter::new(std::fs::File::create(&p).unwrap());
        let block: String = (0..14_000).map(|i| format!("2026-11-10 12:00:{:02} INFO request {i} handled in {} ms é→\n", i % 60, i % 997)).collect();
        let n = (100 << 20) / block.len() + 1;
        for _ in 0..n {
            w.write_all(block.as_bytes()).unwrap();
        }
        w.flush().unwrap();
    }
    let size = std::fs::metadata(&p).unwrap().len();
    assert!(size >= 100 << 20);

    let mut s = LazyStore::open(&p, Config::default()).unwrap();
    s.wait_index();
    for k in 0..12u64 {
        let at = (size / 13) * (k + 1);
        let at = (at..at + 8).find(|&i| s.is_char_boundary(i as usize)).unwrap() as usize;
        s.insert(at, &format!("<edit {k}>")).unwrap();
    }
    s.delete(1_000_000..1_050_000).unwrap();

    // ---- save
    let base = reset();
    let t = Instant::now();
    s.save_with(&p, SaveOptions::default()).unwrap();
    let save_peak = peak_since(base);
    let save_ms = t.elapsed().as_millis();
    let after_save = CUR.load(SeqCst).saturating_sub(base);
    assert_eq!(std::fs::metadata(&p).unwrap().len(), s.byte_len() as u64);

    // ---- autosave of a dirty doc
    s.insert(5_000_000, "typed after save").unwrap();
    s.delete(40_000_000..40_000_100).unwrap();
    let jr = LazyJournals::new(d.path().join("rec"));
    let mut a = Autosaver::new(jr, "huge");
    let base2 = reset();
    let t = Instant::now();
    let out = a.tick(&s, "huge.log").unwrap();
    let auto_peak = peak_since(base2);
    let auto_ms = t.elapsed().as_millis();
    let AutosaveOutcome::Written(st) = out else { panic!("{out:?}") };

    // ---- crash recovery: rebuild from the journal
    let base3 = reset();
    let t = Instant::now();
    let r = a.journals.restore("huge", Config::default()).unwrap();
    let replay_peak = peak_since(base3);
    let replay_ms = t.elapsed().as_millis();
    assert_eq!(r.byte_len(), s.byte_len());
    assert!(r.is_dirty());
    assert_eq!(r.slice_string(5_000_000 - 8..5_000_000 + 24), s.slice_string(5_000_000 - 8..5_000_000 + 24));
    drop(r);

    // ---- close + reopen (session restore) without reading the file
    drop(s);
    let base4 = reset();
    let t = Instant::now();
    let s2 = LazyStore::open(&p, Config { background_index: false, ..Config::default() }).unwrap();
    let cursor = s2.restore_cursor(300_000, 10);
    let reopen_peak = peak_since(base4);
    let reopen_ms = t.elapsed().as_millis();
    assert!(cursor > 0);

    println!(
        "100 MB doc ({} bytes): save peak {} KB ({} ms, resident after {} KB) | autosave peak {} KB ({} ms, journal {} bytes) | replay peak {} KB ({} ms) | reopen+restore_cursor peak {} KB ({} ms)",
        size,
        save_peak / 1024,
        save_ms,
        after_save / 1024,
        auto_peak / 1024,
        auto_ms,
        st.bytes,
        replay_peak / 1024,
        replay_ms,
        reopen_peak / 1024,
        reopen_ms
    );
    assert!(save_peak < LIMIT, "save peak {save_peak}");
    assert!(auto_peak < LIMIT, "autosave peak {auto_peak}");
    assert!(replay_peak < LIMIT, "replay peak {replay_peak}");
    assert!(reopen_peak < LIMIT, "reopen peak {reopen_peak}");
    assert!(st.bytes < 4096);
}
