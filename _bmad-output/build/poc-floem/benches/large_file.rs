//! Large-file editing benchmark (hand-rolled harness, no criterion: each
//! scenario is slow enough that a handful of timed runs is representative).
//!
//! Run (release, a few minutes, needs ~1.5 GB of RAM for the default size):
//!
//! ```text
//! cargo bench -p brouillon --bench large_file
//! BENCH_MB=20 cargo bench -p brouillon --bench large_file   # smaller / faster
//! ```
//!
//! Every scenario prints `min / median / max` over its runs, in milliseconds
//! (or microseconds for per-operation numbers). The text is a deterministic
//! synthetic source (`sample::generate`, repeated), so results are comparable
//! between runs and machines.
//!
//! Scenarios: open (read + decode + rope), insert in the middle, undo/redo,
//! search (literal + regex, count and find_next), scroll (a window of 80
//! lines: fetch + a layout proxy), highlighting (lexical spans of a window,
//! tree-sitter parse + spans of a 8 MB Rust slice), and the full-text copy
//! `rope().to_string()` that several UI handlers still perform.

use std::path::Path;
use std::time::{Duration, Instant};

use brouillon::buffer::Buffer;
use brouillon::grammars::LanguageId;
use brouillon::highlight::Highlighter;
use brouillon::sample;
use brouillon::search::{CancelToken, Matcher, SearchMode, SearchOptions};

const SEED: u64 = 0xED17_2026;
const WINDOW: usize = 80;

struct Stats(Vec<Duration>);

impl Stats {
    fn report(mut self, name: &str, unit_us: bool) {
        self.0.sort();
        let f = |d: Duration| {
            if unit_us {
                format!("{:.1} us", d.as_secs_f64() * 1e6)
            } else {
                format!("{:.2} ms", d.as_secs_f64() * 1e3)
            }
        };
        let n = self.0.len();
        println!(
            "{name:<46} n={n:<4} min {:>12}  med {:>12}  max {:>12}",
            f(self.0[0]),
            f(self.0[(n - 1) / 2]),
            f(self.0[n - 1])
        );
    }
}

fn time<R>(runs: usize, mut f: impl FnMut() -> R) -> Stats {
    let mut v = Vec::with_capacity(runs);
    for _ in 0..runs {
        let t = Instant::now();
        std::hint::black_box(f());
        v.push(t.elapsed());
    }
    Stats(v)
}

/// Deterministic text of about `mb` MiB.
fn build_text(mb: usize) -> String {
    let chunk = sample::generate(100_000, SEED); // ~7.3 MB
    let mut s = String::with_capacity(mb * 1024 * 1024 + chunk.len());
    while s.len() < mb * 1024 * 1024 {
        s.push_str(&chunk);
    }
    s
}

fn main() {
    let mb: usize = std::env::var("BENCH_MB").ok().and_then(|v| v.parse().ok()).unwrap_or(100);
    println!("== large_file bench: ~{mb} MiB synthetic text (release build expected) ==");
    let text = build_text(mb);
    let lines = text.bytes().filter(|&b| b == b'\n').count();
    println!("text: {} bytes, {} lines", text.len(), lines);

    // ---- open: read + decode + rope --------------------------------------
    let dir = std::env::temp_dir().join(format!("brouillon-bench-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("big.txt");
    std::fs::write(&path, &text).unwrap();
    time(3, || brouillon::wiring::open_file(Path::new(&path)).unwrap().text.len())
        .report("open: read+decode (wiring::open_file)", false);
    time(3, || Buffer::from_str(&text).byte_len()).report("open: Buffer::from_str (rope build)", false);
    let _ = std::fs::remove_dir_all(&dir);

    let mut buf = Buffer::from_str(&text);
    let mid = text.len() / 2;
    // Keep the insertion point on a char boundary.
    let mid = (mid..mid + 4).find(|&i| text.is_char_boundary(i)).unwrap_or(mid);

    // ---- insert in the middle ---------------------------------------------
    {
        let mut v = Vec::new();
        let mut t_ms = 0u64;
        for i in 0..200 {
            // Spaced > 1 s apart so every keystroke is its own undo group (worst case).
            t_ms += 1500;
            let t = Instant::now();
            buf.typing_insert(mid + i, "x", t_ms);
            v.push(t.elapsed());
        }
        Stats(v).report("insert 1 char in the middle (per op)", true);
    }

    // ---- undo / redo ------------------------------------------------------
    {
        let mut v = Vec::new();
        for _ in 0..100 {
            let t = Instant::now();
            buf.undo();
            v.push(t.elapsed());
        }
        Stats(v).report("undo (per op)", true);
        let mut v = Vec::new();
        for _ in 0..100 {
            let t = Instant::now();
            buf.redo();
            v.push(t.elapsed());
        }
        Stats(v).report("redo (per op)", true);
    }

    // ---- search -----------------------------------------------------------
    let cancel = CancelToken::new();
    let all = [0..buf.byte_len()];
    let literal = Matcher::new("fn main", &SearchOptions::default()).unwrap();
    let rx = SearchOptions { mode: SearchMode::Regex, ..SearchOptions::default() };
    let regex = Matcher::new(r"\bfn\s+\w+\(", &rx).unwrap();
    time(3, || literal.count(buf.rope(), &all, &cancel).unwrap()).report("search: literal count over whole file", false);
    time(3, || regex.count(buf.rope(), &all, &cancel).unwrap()).report("search: regex count over whole file", false);
    time(5, || literal.find_next(buf.rope(), mid, true, &cancel).unwrap().is_some())
        .report("search: find_next from the middle (literal)", false);
    time(5, || regex.find_next(buf.rope(), 0, true, &cancel).unwrap().is_some())
        .report("search: find_next from the start (regex)", false);

    // ---- scroll: a window of lines ----------------------------------------
    let total_lines = buf.line_count();
    {
        let mut off = 0usize;
        time(300, || {
            off = (off + 9_973) % (total_lines - WINDOW);
            let w = buf.visible_lines(off, off + WINDOW);
            // Layout proxy: columns with tab expansion (what a monospace layout needs per line).
            let mut cols = 0usize;
            for l in &w {
                for ch in l.chars() {
                    cols += if ch == '\t' { 4 } else { 1 };
                }
            }
            cols
        })
        .report("scroll: fetch 80 lines + column layout", true);
    }
    {
        // Per-line fetch the way the renderer does it (nth_line + line_byte_range).
        let mut line = 0usize;
        time(2000, || {
            line = (line + 4_999) % total_lines;
            (buf.nth_line(line).map(|s| s.len()), buf.line_byte_range(line))
        })
        .report("scroll: one line (nth_line + byte_range)", true);
    }

    // ---- highlighting -----------------------------------------------------
    {
        // Lexical (non-Rust) spans: current implementation scans the WHOLE source
        // for each requested range, so use a 2 MiB document to keep the run bounded.
        let small: String = text.chars().take(2 * 1024 * 1024).collect();
        let mut hl = Highlighter::new(LanguageId::Json);
        hl.parse(&small);
        let mid_s = small.len() / 2;
        time(20, || hl.spans_in_range(mid_s..mid_s + 120).len())
            .report("highlight: lexical spans of ONE line (cached)", true);
        // What every rendered line used to cost: a regex scan of the whole document.
        time(3, || brouillon::highlight::highlight_lexical(&small, LanguageId::Json, mid_s..mid_s + 120).len())
            .report("highlight: lexical, old per-line full scan", false);
        // Rust tree-sitter on REAL Rust source (this crate's own files, repeated to ~8 MiB).
        let mut real = String::new();
        for f in ["wiring.rs", "main.rs", "ipc.rs", "cli.rs"] {
            let p = Path::new(env!("CARGO_MANIFEST_DIR")).join("src").join(f);
            if let Ok(s) = std::fs::read_to_string(p) {
                // Drop inner attributes / inner doc comments: invalid once repeated mid-file.
                real.extend(s.lines().filter(|l| !l.starts_with("//!") && !l.starts_with("#![")).map(|l| format!("{l}\n")));
            }
        }
        let unit = real.clone();
        while real.len() < 8 * 1024 * 1024 {
            real.push_str(&unit);
        }
        let mut hl = Highlighter::new(LanguageId::Rust);
        time(3, || {
            hl.parse(&real);
            hl.is_parsed()
        })
        .report("highlight: tree-sitter full parse (8 MiB real Rust)", false);
        let m = real.len() / 2;
        time(200, || hl.spans_in_range(m..m + 120).len()).report("highlight: tree-sitter spans of ONE line", true);
        // The random synthetic text is not Rust: shows the error-recovery worst case.
        let junk: String = text.chars().take(2 * 1024 * 1024).collect();
        let mut hl = Highlighter::new(LanguageId::Rust);
        time(2, || {
            hl.parse(&junk);
            hl.is_parsed()
        })
        .report("highlight: tree-sitter parse (2 MiB non-Rust text)", false);
    }

    // ---- folding on the 100k-line sample (start-up path of a file opened from the command line) --
    {
        let s = sample::generate(100_000, SEED);
        time(3, || brouillon::folding::compute_fold_regions(&s).len()).report("fold: compute regions (100k-line sample)", false);
        let st = brouillon::folding::FoldState::new(brouillon::folding::compute_fold_regions(&s));
        println!("  ({} fold regions)", st.regions.len());
        time(3, || st.visible_lines(100_000).len()).report("fold: visible_lines (100k lines)", false);
    }

    // ---- full-text copy (done by bracket match / completion / reparse handlers) --
    time(3, || buf.rope().to_string().len()).report("rope().to_string() (full copy, per call)", false);

    println!("== done ==");
}
