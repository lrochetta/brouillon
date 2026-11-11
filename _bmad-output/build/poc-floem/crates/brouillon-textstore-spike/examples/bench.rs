//! One store x one file, one process (so OS memory counters are clean). Release only.
//!   cargo run --release -p brouillon-textstore-spike --example bench -- <rope|mmap|chunked> <file> [edits]
//!   EARLY=1 ... : only measure "edit immediately after open" (index not finished yet)
//! Prints `R|store|file|stage|ms|heap_live|heap_peak|working_set|private|note` lines (MB) for the driver script.
use brouillon_textstore_spike::lazy::{Config, LazyStore, Mode};
use brouillon_textstore_spike::os_mem;
use brouillon_textstore_spike::rope_store::RopeStore;
use brouillon_textstore_spike::store::{count_matches, hash_all, viewport, TextStore};
use rand::{Rng, SeedableRng};
use rand_chacha::ChaCha8Rng;
use std::alloc::{GlobalAlloc, Layout, System};
use std::io::Read;
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering::Relaxed};
use std::time::Instant;

struct Counting;
static LIVE: AtomicUsize = AtomicUsize::new(0);
static PEAK: AtomicUsize = AtomicUsize::new(0);
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, l: Layout) -> *mut u8 {
        let p = System.alloc(l);
        if !p.is_null() {
            let now = LIVE.fetch_add(l.size(), Relaxed) + l.size();
            PEAK.fetch_max(now, Relaxed);
        }
        p
    }
    unsafe fn dealloc(&self, p: *mut u8, l: Layout) {
        System.dealloc(p, l);
        LIVE.fetch_sub(l.size(), Relaxed);
    }
    unsafe fn realloc(&self, p: *mut u8, l: Layout, new: usize) -> *mut u8 {
        let q = System.realloc(p, l, new);
        if !q.is_null() {
            if new >= l.size() {
                let now = LIVE.fetch_add(new - l.size(), Relaxed) + (new - l.size());
                PEAK.fetch_max(now, Relaxed);
            } else {
                LIVE.fetch_sub(l.size() - new, Relaxed);
            }
        }
        q
    }
}
#[global_allocator]
static A: Counting = Counting;

fn mb(n: u64) -> f64 {
    n as f64 / 1048576.0
}
fn reset_peak() {
    PEAK.store(LIVE.load(Relaxed), Relaxed);
}
struct Out {
    store: String,
    file: String,
}
impl Out {
    fn row(&self, stage: &str, ms: f64, note: &str) {
        let m = os_mem::sample();
        println!(
            "R|{}|{}|{}|{:.1}|{:.1}|{:.1}|{:.1}|{:.1}|{}",
            self.store,
            self.file,
            stage,
            ms,
            mb(LIVE.load(Relaxed) as u64),
            mb(PEAK.load(Relaxed) as u64),
            mb(m.working_set),
            mb(m.private),
            note
        );
        reset_peak();
    }
}
fn pct(v: &mut Vec<f64>, p: f64) -> f64 {
    if v.is_empty() {
        return 0.0;
    }
    v.sort_by(|a, b| a.partial_cmp(b).unwrap());
    v[((v.len() as f64 - 1.0) * p).round() as usize]
}
fn file_hash(path: &Path) -> u64 {
    let mut f = std::fs::File::open(path).unwrap();
    let mut buf = vec![0u8; 1 << 20];
    let mut h = 0xcbf29ce484222325u64;
    loop {
        let n = f.read(&mut buf).unwrap();
        if n == 0 {
            return h;
        }
        for &b in &buf[..n] {
            h ^= b as u64;
            h = h.wrapping_mul(0x100000001b3);
        }
    }
}
fn open(kind: &str, path: &Path) -> Box<dyn TextStore> {
    match kind {
        "rope" => Box::new(RopeStore::open(path).unwrap()),
        "mmap" => Box::new(LazyStore::open(path, Config { mode: Mode::Mmap, ..Config::default() }).unwrap()),
        "chunked" => Box::new(LazyStore::open(path, Config { mode: Mode::Chunked, ..Config::default() }).unwrap()),
        _ => panic!("store must be rope|mmap|chunked"),
    }
}

fn main() {
    let mut a = std::env::args().skip(1);
    let kind = a.next().expect("store");
    let path = std::path::PathBuf::from(a.next().expect("file"));
    let edits: usize = a.next().and_then(|x| x.parse().ok()).unwrap_or(10_000);
    let fname = path.file_name().unwrap().to_string_lossy().to_string();
    let out = Out { store: kind.clone(), file: fname };
    let len = std::fs::metadata(&path).unwrap().len();

    // average line length from the first 64 KB, identical for every store -> same target line N
    let head = {
        let mut b = vec![0u8; 65536];
        let n = std::fs::File::open(&path).unwrap().read(&mut b).unwrap();
        b.truncate(n);
        b
    };
    let nl_head = head.iter().filter(|&&c| c == b'\n').count().max(1);
    let avg = head.len() / nl_head;
    let target_line = (len as usize / 2) / avg.max(1);

    if std::env::var("EARLY").is_ok() {
        let t = Instant::now();
        let mut s = open(&kind, &path);
        let t_open = t.elapsed();
        let t = Instant::now();
        let mid = (len / 2) as usize;
        let mut p = mid;
        while !s.is_char_boundary(p) {
            p += 1;
        }
        s.insert(p, "early").unwrap();
        out.row("early-first-edit", t.elapsed().as_secs_f64() * 1e3, &format!("open_ms={:.2}", t_open.as_secs_f64() * 1e3));
        return;
    }

    let h_orig = file_hash(&path); // also warms the OS file cache equally for every store
    reset_peak();
    out.row("0 process start", 0.0, "");

    // 1. open + first viewport
    let t = Instant::now();
    let mut s = open(&kind, &path);
    let t_open = t.elapsed().as_secs_f64() * 1e3;
    let t = Instant::now();
    let got = viewport(&*s, target_line, 60);
    let t_view = t.elapsed().as_secs_f64() * 1e3;
    out.row("1 open", t_open, "");
    out.row("1b first viewport (line N, 60 rows)", t_view, &format!("N={target_line} bytes={got}"));

    // 2. wait for the lazy index
    let t = Instant::now();
    let lines = s.line_count();
    out.row("2 line_count (index complete)", t.elapsed().as_secs_f64() * 1e3, &format!("lines={lines}"));

    // 3. random viewport jumps
    let mut rng = ChaCha8Rng::seed_from_u64(42);
    let mut v: Vec<f64> = Vec::new();
    for _ in 0..300 {
        let n = rng.gen_range(0..lines.saturating_sub(70).max(1));
        let t = Instant::now();
        viewport(&*s, n, 60);
        v.push(t.elapsed().as_secs_f64() * 1e3);
    }
    let (p50, p99) = (pct(&mut v, 0.5), pct(&mut v, 0.99));
    out.row("3 300 random jumps", p50, &format!("p50_ms={p50:.3} p99_ms={p99:.3}"));

    // 4. scattered edits
    let mut v: Vec<f64> = Vec::new();
    let t_all = Instant::now();
    for i in 0..edits {
        let l = s.byte_len();
        let mut p = rng.gen_range(0..l);
        let t = Instant::now();
        while !s.is_char_boundary(p) {
            p += 1;
        }
        match i % 10 {
            0..=5 => s.insert(p, ["x", "ab", "é", "hello ", "→→"][i % 5]).unwrap(),
            6..=8 => {
                let mut e = (p + 1 + i % 17).min(l);
                while !s.is_char_boundary(e) {
                    e += 1;
                }
                s.delete(p..e).unwrap()
            }
            _ => {
                let mut e = (p + 20).min(l);
                while !s.is_char_boundary(e) {
                    e += 1;
                }
                s.replace_range(p..e, "replaced text\n").unwrap()
            }
        }
        v.push(t.elapsed().as_secs_f64() * 1e3);
    }
    let (p50, p99) = (pct(&mut v, 0.5), pct(&mut v, 0.99));
    let mx = v.iter().cloned().fold(0.0, f64::max);
    out.row(&format!("4 {edits} scattered edits"), t_all.elapsed().as_secs_f64() * 1e3, &format!("p50_ms={p50:.4} p99_ms={p99:.4} max_ms={mx:.2} heap_est={:.2}MB", mb(s.heap_estimate() as u64)));
    let mut v: Vec<f64> = Vec::new();
    let lines2 = s.line_count();
    for _ in 0..300 {
        let n = rng.gen_range(0..lines2.saturating_sub(70).max(1));
        let t = Instant::now();
        viewport(&*s, n, 60);
        v.push(t.elapsed().as_secs_f64() * 1e3);
    }
    let (p50, p99) = (pct(&mut v, 0.5), pct(&mut v, 0.99));
    out.row("4b 300 jumps after edits", p50, &format!("p50_ms={p50:.3} p99_ms={p99:.3}"));
    let h_edit = hash_all(&*s);
    reset_peak();

    // 5. Find over the whole file (streaming)
    let t = Instant::now();
    let c = count_matches(&*s, b"ZQXJ_MARK");
    out.row("5 Find (count ZQXJ_MARK, whole file)", t.elapsed().as_secs_f64() * 1e3, &format!("matches={c}"));

    // 6. Save (streaming, temp + rename)
    let t = Instant::now();
    s.save(&path).unwrap();
    out.row("6 Save (stream + fsync + rename)", t.elapsed().as_secs_f64() * 1e3, "");
    let h_saved = file_hash(&path);
    assert_eq!(h_saved, h_edit, "saved file differs from the document");
    reset_peak();

    // 7. undo everything, then redo everything (undo crosses the save boundary)
    let mut v: Vec<f64> = Vec::new();
    let t_all = Instant::now();
    while {
        let t = Instant::now();
        let ok = s.undo();
        if ok {
            v.push(t.elapsed().as_secs_f64() * 1e3);
        }
        ok
    } {}
    let (p50, p99) = (pct(&mut v, 0.5), pct(&mut v, 0.99));
    let n = v.len();
    let h_undo = hash_all(&*s);
    out.row("7 undo all", t_all.elapsed().as_secs_f64() * 1e3, &format!("ops={n} p50_ms={p50:.4} p99_ms={p99:.4} identical_to_original={}", h_undo == h_orig));
    assert_eq!(h_undo, h_orig, "undo-all must restore the original bytes");
    let mut v: Vec<f64> = Vec::new();
    let t_all = Instant::now();
    while {
        let t = Instant::now();
        let ok = s.redo();
        if ok {
            v.push(t.elapsed().as_secs_f64() * 1e3);
        }
        ok
    } {}
    let (p50, p99) = (pct(&mut v, 0.5), pct(&mut v, 0.99));
    let h_redo = hash_all(&*s);
    out.row("8 redo all", t_all.elapsed().as_secs_f64() * 1e3, &format!("ops={} p50_ms={p50:.4} p99_ms={p99:.4} identical_to_edited={}", v.len(), h_redo == h_edit));
    assert_eq!(h_redo, h_edit);
    let m = os_mem::sample();
    println!("PEAK|{}|{}|peak_ws={:.1}|peak_commit={:.1}", out.store, out.file, mb(m.peak_working_set), mb(m.peak_private));
}
