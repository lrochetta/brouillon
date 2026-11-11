//! Headless amplification probe (no window): bytes of heap held by every long-lived structure that scales
//! with the size of the open document, as a ratio of the source bytes, plus the transient peak of the
//! one-shot operations. Counting global allocator; heap only (no GPU, stacks, code pages).
//!   cargo run --release -p brouillon --example mem_structs -- <size-mb> [code|prose|flat]
//! "live" = bytes still allocated after the structure is built (what stays resident while it exists);
//! "peak" = high-water mark above the live baseline while it is built.
use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicUsize, Ordering::Relaxed};
use std::sync::Arc;

use brouillon::grammars::LanguageId;
use brouillon::highlight::Highlighter;
use brouillon_editor_engine::buffer::Buffer;

struct Counting;
static LIVE: AtomicUsize = AtomicUsize::new(0);
static PEAK: AtomicUsize = AtomicUsize::new(0);
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, l: Layout) -> *mut u8 {
        let p = System.alloc(l);
        if !p.is_null() {
            PEAK.fetch_max(LIVE.fetch_add(l.size(), Relaxed) + l.size(), Relaxed);
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
                PEAK.fetch_max(LIVE.fetch_add(new - l.size(), Relaxed) + (new - l.size()), Relaxed);
            } else {
                LIVE.fetch_sub(l.size() - new, Relaxed);
            }
        }
        q
    }
}
#[global_allocator]
static A: Counting = Counting;

fn live() -> usize {
    LIVE.load(Relaxed)
}
fn mb(n: usize) -> f64 {
    n as f64 / 1048576.0
}

/// Run `f`; return (value, live bytes added that stay held by the value, transient peak above the baseline).
fn measure<R>(f: impl FnOnce() -> R) -> (R, usize, usize) {
    let base = live();
    PEAK.store(base, Relaxed);
    let r = f();
    (r, live().saturating_sub(base), PEAK.load(Relaxed).saturating_sub(base))
}

fn row(name: &str, src: usize, held: usize, peak: usize) {
    println!("{name:<50} held {:>8.2} MB ({:>5.2}x)   peak {:>8.2} MB ({:>5.2}x)", mb(held), held as f64 / src as f64, mb(peak), peak as f64 / src as f64);
}

fn build(variant: &str, bytes: usize) -> crop::Rope {
    let mut b = crop::RopeBuilder::new();
    let mut total = 0usize;
    let mut i = 0usize;
    let words = ["alpha", "beta", "gamma", "delta", "epsilon", "zeta", "theta", "kappa", "lambda", "sigma", "document", "editor", "buffer", "caf\u{e9}", "memory"];
    while total < bytes {
        let s = match variant {
            "code" => {
                let depth = (i / 3) % 4;
                let ind = "    ".repeat(depth);
                match i % 6 {
                    0 => format!("{ind}fn handler_{i}(ctx: &mut Context, value: u32) -> u32 {{\n"),
                    1 => format!("{ind}    let total = value + {i};\n"),
                    2 => format!("{ind}    // compute the running total for item {i}\n"),
                    3 => format!("{ind}    if total > {} {{ return total; }}\n", i % 97),
                    4 => format!("{ind}    println!(\"item {{}}\", total);\n"),
                    _ => format!("{ind}}}\n"),
                }
            }
            "prose" => {
                let mut s = String::new();
                let mut k = i;
                while s.len() < 280 + (i % 7) * 40 {
                    s.push_str(words[k % words.len()]);
                    s.push(' ');
                    k = k * 7 + 3;
                }
                s.push_str("end.\n");
                s
            }
            _ => "abcdefghij klmnopqrst uvwxyz 0123456789 abcdefghij klmnopqrst uvwxyz 0123456789 abcdefghij klmnopq\n".to_string(),
        };
        total += s.len();
        b.append(&s);
        i += 1;
    }
    b.build()
}

/// Process working set (bytes) via sysinfo: sees allocations that bypass the Rust allocator (tree-sitter's C code).
fn working_set() -> usize {
    let mut sys = sysinfo::System::new();
    let pid = sysinfo::get_current_pid().unwrap();
    sys.refresh_processes(sysinfo::ProcessesToUpdate::Some(&[pid]), true);
    sys.process(pid).map(|p| p.memory() as usize).unwrap_or(0)
}

fn main() {
    let mut args = std::env::args().skip(1);
    let size_mb: usize = args.next().and_then(|a| a.parse().ok()).unwrap_or(10);
    let variant = args.next().unwrap_or_else(|| "code".into());
    let size = size_mb * 1048576;

    let (rope, rope_held, _) = measure(|| build(&variant, size));
    let n = rope.byte_len();
    let lines = rope.line_len();
    println!("== {size_mb} MB {variant}: {:.1} MB, {lines} lines ({:.1} B/line); rope held {:.1} MB ({:.3}x)", mb(n), n as f64 / lines as f64, mb(rope_held), rope_held as f64 / n as f64);
    let buf = Arc::new(Buffer::from_rope(rope.clone()));

    // ---- syntax highlighter (the editor builds one only up to 16 MB) ----
    if n <= 16 * 1048576 {
        let langs: Vec<(&str, LanguageId)> = match variant.as_str() {
            "code" => vec![("Rust (tree-sitter)", LanguageId::Rust), ("JavaScript (table lexer)", LanguageId::JavaScript)],
            "prose" => vec![("Markdown (table lexer)", LanguageId::Markdown)],
            _ => vec![("Plain Text", LanguageId::PlainText)],
        };
        for (name, lang) in langs {
            let ws0 = working_set();
            let (hl, held, peak) = measure(|| {
                let text = rope.to_string();
                let mut hl = Highlighter::new(lang);
                hl.parse(&text);
                hl
            });
            row(&format!("Highlighter {name}: after parse"), n, held, peak);
            if lang == LanguageId::Rust {
                println!("   (tree-sitter allocates in C, invisible above) working set delta incl. String: {:.1} MB ({:.2}x)", mb(working_set().saturating_sub(ws0)), working_set().saturating_sub(ws0) as f64 / n as f64);
            }
            // scroll through the whole document, 60 lines at a time (forces every lexer state)
            let ((), held2, _) = measure(|| {
                let mut line = 0;
                while line < lines {
                    let a = rope.byte_of_line(line);
                    let b = rope.byte_of_line((line + 60).min(lines - 1).max(line));
                    let _ = hl.spans_in_range(a..b.max(a + 1).min(n));
                    line += 60;
                }
            });
            row(&format!("Highlighter {name}: + scrolled whole doc"), n, held + held2, 0);
            // live re-parse after a keystroke (what the 150 ms worker does)
            let (_, _, peak) = measure(|| {
                let text = rope.to_string();
                let _ = &text;
            });
            row(&format!("Highlighter {name}: reparse worker String"), n, 0, peak);
            drop(hl);
            let ws1 = working_set();
            let (hl2, held, peak) = measure(|| {
                let mut hl = Highlighter::new(lang);
                hl.parse_rope(&rope);
                hl
            });
            row(&format!("Highlighter {name}: NEW parse_rope"), n, held, peak);
            let _ = ws1;
            let ((), held2, _) = measure(|| {
                let mut line = 0;
                while line < lines {
                    let a = rope.byte_of_line(line);
                    let b = rope.byte_of_line((line + 60).min(lines - 1).max(line));
                    let _ = hl2.spans_in_range(a..b.max(a + 1).min(n));
                    line += 60;
                }
            });
            row(&format!("Highlighter {name}: NEW + scrolled whole doc"), n, held + held2, 0);
            // one keystroke: the editor worker's incremental update
            let mut edited = rope.clone();
            let at = edited.byte_of_line(lines / 2);
            edited.insert(at, "x = 1;\n");
            let mut hl2 = hl2;
            let (_, _, peak) = measure(|| hl2.apply_rope(&edited));
            row(&format!("Highlighter {name}: NEW apply_rope (one line)"), n, 0, peak);
            drop(hl2);
        }
    }

    // ---- fold regions (editor: computed on open at ANY size, refreshed on edit up to 2 MB) ----
    {
        let (fr, held, peak) = measure(|| {
            let text = rope.to_string();
            brouillon::folding::compute_fold_regions(&text)
        });
        row(&format!("FoldState regions on open ({} regions)", fr.len()), n, held, peak);
        let (fr2, held2, peak2) = measure(|| brouillon::folding::compute_fold_regions_rope(&rope));
        assert_eq!(fr.len(), fr2.len());
        row("FoldState regions on open: NEW from the rope", n, held2, peak2);
        drop(fr2);
        let mut fs = brouillon::folding::FoldState::new(fr);
        if let Some(r) = fs.regions.first() {
            let l = r.start_line;
            fs.fold(l);
        }
        let (_, _, peak) = measure(|| fs.visible_lines(lines).len());
        row("FoldState.visible_lines (one fold open)", n, 0, peak);
    }

    // ---- wrap map (editor: only up to 50k lines) ----
    if lines <= brouillon_editor_engine::wrap::WRAP_MAX_LINES {
        let fs = brouillon::folding::FoldState::empty();
        let (m, held, peak) = measure(|| brouillon_editor_engine::wrap::WrapMap::build(lines, &fs, 80, |i| buf.nth_line(i).unwrap_or_default()));
        row(&format!("WrapMap @80 cols ({} rows)", m.rows.len()), n, held, peak);
    } else {
        println!("{:<50} (not built: {lines} lines > {})", "WrapMap", brouillon_editor_engine::wrap::WRAP_MAX_LINES);
    }

    // ---- undo history: 1000 typed characters, then a 1 MB replace done 5 times ----
    {
        let mut cur = buf.clone();
        let (_, held, _) = measure(|| {
            for k in 0..1000usize {
                let at = (k * 7919) % cur.byte_len();
                let at = {
                    let mut a = at;
                    while !cur.rope().is_char_boundary(a) {
                        a -= 1;
                    }
                    a
                };
                let mut nb = (*cur).clone();
                nb.typing_insert(at, "x", (k as u64) * 2000);
                cur = Arc::new(nb);
            }
        });
        row("after 1000 typed chars: held (history+diverged rope)", n, held, 0);
        let (_, held_c, peak_c) = measure(|| (*cur).clone());
        row("Buffer::clone once, with that history", n, held_c, peak_c);
        // old Arc<Buffer> kept by the recovery journal while the editor moves on
        let old = cur.clone();
        let mut nb = (*cur).clone();
        let (_, held_k, _) = measure(|| {
            for k in 0..50usize {
                let mut at = (k * 131) % nb.byte_len().max(1);
                while !nb.rope().is_char_boundary(at) {
                    at -= 1;
                }
                nb.typing_insert(at, "y", 5_000_000 + k as u64 * 2000);
            }
        });
        cur = Arc::new(nb);
        let _ = (&old, &cur);
        row("50 more chars while an old Arc<Buffer> is kept", n, held_k, 0);
        drop(old);
        // big replacements (Replace All style): each pushes the removed text to history
        let big = (n / 20).min(1 << 20);
        let (heavy, held_r, peak_r) = measure(|| {
            let mut b = (*cur).clone();
            for k in 0..5usize {
                let at = k * big;
                let end = (at + big).min(b.byte_len());
                let (mut s, mut e) = (at, end);
                while !b.rope().is_char_boundary(s) {
                    s -= 1;
                }
                while !b.rope().is_char_boundary(e) {
                    e -= 1;
                }
                b.replace_range(s..e, "REPLACED\n");
            }
            b
        });
        row(&format!("5 replacements of ~{} KB each (undo text)", big / 1024), n, held_r, peak_r);
        // The editor clones the Buffer on every edit and the journal keeps old ones: with the groups shared,
        // a clone costs pointers. Before, it was a deep copy of every undo string (about undo_bytes).
        let (_, held_c, peak_c) = measure(|| heavy.clone());
        println!("   undo text held by the buffer: {:.2} MB", mb(heavy.undo_bytes()));
        row("Buffer::clone with that history (NEW: shared groups)", n, held_c, peak_c);
    }

    // ---- word completion (one call, prefix 'ha' at the middle) ----
    {
        let (_, _, peak) = measure(|| {
            brouillon::auto_complete::complete_from_lines(rope.lines().map(|l| l.to_string()), rope.line_len(), "ha", lines / 2, &brouillon::auto_complete::CompletionConfig::default())
        });
        row("word completion, one keystroke (whole doc, old)", n, 0, peak);
        let (_, _, peak) = measure(|| {
            let (first, last) = brouillon::wiring::completion_window(&rope, lines / 2, 2 * 1048576);
            brouillon::auto_complete::complete_from_lines(
                rope.line_slice(first..last).lines().map(|l| l.to_string()),
                last - first,
                "ha",
                lines / 2 - first,
                &brouillon::auto_complete::CompletionConfig::default(),
            )
        });
        row("word completion, one keystroke (NEW 2 MB window)", n, 0, peak);
    }
    // ---- minimap ----
    {
        let (rows, held, peak) = measure(|| brouillon::minimap::sample(lines, 600, 4, &|i| buf.nth_line(i)));
        row(&format!("minimap sample ({} rows)", rows.len()), n, held, peak);
    }
    // ---- hex mode (hex_view_panel: String -> Vec<u8> + one im::Vector entry per 16-byte row) ----
    {
        let ((bytes, idx), held, peak) = measure(|| {
            let s = buf.rope().to_string();
            let bytes = Arc::new(s.into_bytes());
            let rows = bytes.len().div_ceil(16);
            let v: im::Vector<usize> = (0..rows).collect();
            (bytes, v)
        });
        let _ = (&bytes, &idx);
        row("hex mode, old (bytes copy + row index)", n, held, peak);
        drop((bytes, idx));
        let (rows, held, peak) = measure(|| {
            // new: nothing is built up front; each visible row reads 16 bytes from the rope
            let total = brouillon::hex_view::total_hex_lines(buf.byte_len(), 16);
            let mut got = 0usize;
            for r in [0, total / 2, total.saturating_sub(1)] {
                got += brouillon::hex_view::rope_bytes(buf.rope(), r * 16, 16).len();
            }
            got
        });
        let _ = rows;
        row("hex mode, NEW (rows read from the rope)", n, held, peak);
    }
    // ---- diff (editor: up to 8 MB per side) ----
    if n <= 8 * 1048576 {
        let (_, held, peak) = measure(|| {
            let old = rope.to_string();
            let mut new = old.clone();
            new.insert_str(new.len() / 2, "CHANGED LINE\n");
            brouillon::diff_view::DiffData::new("a", "b", &old, &new)
        });
        row("DiffData NEW, one diff pass (old+new vs one line)", n, held, peak);
        let (_, held, peak) = measure(|| {
            let old = rope.to_string();
            let mut new = old.clone();
            new.insert_str(new.len() / 2, "CHANGED LINE\n");
            // the old constructor ran the line diff three times
            let i = brouillon::diff::inline_rows(&old, &new);
            let s = brouillon::diff::side_by_side(&old, &new);
            let st = brouillon::diff::stats(&old, &new);
            (i, s, st)
        });
        row("DiffData old, three diff passes", n, held, peak);
    }
    // ---- session snapshot of a scratch tab holding this text ----
    {
        let dir = std::env::temp_dir().join(format!("mem_structs_{size_mb}_{variant}"));
        let mut tm = brouillon::tabs::TabManager::new();
        tm.current_mut().buffer = buf.clone();
        let (_, _, peak) = measure(|| brouillon::session::save_to_dir(&tm, &dir).unwrap());
        row("session save, scratch tab holding the text", n, 0, peak);
        let j = brouillon::recovery::Journal::new(dir.join("rec"));
        tm.current_mut().dirty = true;
        let (_, held, peak) = measure(|| j.snapshot_tabs(&tm).unwrap());
        row("recovery snapshot_tabs (streamed)", n, held, peak);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
