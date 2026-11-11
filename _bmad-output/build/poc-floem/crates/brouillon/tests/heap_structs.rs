//! Heap guards for the structures that live as long as a document is open (syntax lexer text and states, fold
//! regions, undo history clones, diff rows). A counting allocator measures bytes held / transient peak against
//! the size of the document. One #[test] only: the allocator counters are process-wide.
use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicUsize, Ordering::SeqCst};
use std::sync::Arc;

struct Counting;
static LIVE: AtomicUsize = AtomicUsize::new(0);
static PEAK: AtomicUsize = AtomicUsize::new(0);
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, l: Layout) -> *mut u8 {
        let p = System.alloc(l);
        if !p.is_null() {
            PEAK.fetch_max(LIVE.fetch_add(l.size(), SeqCst) + l.size(), SeqCst);
        }
        p
    }
    unsafe fn dealloc(&self, p: *mut u8, l: Layout) {
        System.dealloc(p, l);
        LIVE.fetch_sub(l.size(), SeqCst);
    }
    unsafe fn realloc(&self, p: *mut u8, l: Layout, new: usize) -> *mut u8 {
        let q = System.realloc(p, l, new);
        if !q.is_null() {
            if new >= l.size() {
                PEAK.fetch_max(LIVE.fetch_add(new - l.size(), SeqCst) + (new - l.size()), SeqCst);
            } else {
                LIVE.fetch_sub(l.size() - new, SeqCst);
            }
        }
        q
    }
}
#[global_allocator]
static A: Counting = Counting;

/// (value, bytes still held after `f`, transient peak above the start).
fn measure<R>(f: impl FnOnce() -> R) -> (R, usize, usize) {
    let base = LIVE.load(SeqCst);
    PEAK.store(base, SeqCst);
    let r = f();
    (r, LIVE.load(SeqCst).saturating_sub(base), PEAK.load(SeqCst).saturating_sub(base))
}

fn code_text(bytes: usize) -> String {
    let mut s = String::with_capacity(bytes + 100);
    let mut i = 0;
    while s.len() < bytes {
        let ind = "    ".repeat((i / 3) % 4);
        match i % 6 {
            0 => s.push_str(&format!("{ind}int handler_{i}(int value) {{\n")),
            1 => s.push_str(&format!("{ind}    int total = value + {i};\n")),
            2 => s.push_str(&format!("{ind}    // compute the running total for item {i}\n")),
            3 => s.push_str(&format!("{ind}    if (total > {}) {{ return total; }}\n", i % 97)),
            4 => s.push_str(&format!("{ind}    printf(\"item %d\", total);\n")),
            _ => s.push_str(&format!("{ind}}}\n")),
        }
        i += 1;
    }
    s
}

fn scroll_all(d: &brouillon::syntax::SyntaxDoc, lines: usize, len: usize) -> usize {
    // Cover the whole document, 200 lines per request, so every line's lexer state gets cached.
    let _ = lines;
    let mut n = 0;
    let step = 8 * 1024;
    let mut at = 0;
    while at < len {
        n += d.spans_in_range(at..(at + step).min(len)).len();
        at += step;
    }
    n
}

#[test]
fn long_lived_structures_stay_small() {
    const MB: usize = 1 << 20;
    let text = code_text(4 * MB);
    let rope: crop::Rope = text.parse().unwrap();
    let doc_len = text.len();
    let lines = rope.line_len();
    let reg = Arc::new(brouillon::syntax::Registry::build(&[]));
    let c = reg.by_name("C").expect("C definition");

    // ---- syntax document: String-backed (old) versus rope-backed (new), fully scrolled ----
    let (old_doc, old_held, _) = measure(|| brouillon::syntax::SyntaxDoc::new(reg.clone(), c, &text));
    let (n_old, old_states, _) = measure(|| scroll_all(&old_doc, lines, doc_len));
    let old_total = old_held + old_states;
    drop(old_doc);
    let (new_doc, new_held, new_peak) = measure(|| brouillon::syntax::SyntaxDoc::new_rope(reg.clone(), c, rope.clone()));
    let (n_new, new_states, _) = measure(|| scroll_all(&new_doc, lines, doc_len));
    assert_eq!(n_old, n_new, "same spans");
    let new_total = new_held + new_states;
    println!("syntax doc 4 MB code: old {old_total} B ({:.2}x), new {new_total} B ({:.2}x), build peak {new_peak}", old_total as f64 / doc_len as f64, new_total as f64 / doc_len as f64);
    assert!(old_total > doc_len, "the String copy alone is one document ({old_total})");
    assert!(new_total < doc_len / 3, "rope-backed lexer must hold well under a third of the document, got {new_total} B");

    // Keystroke on the rope-backed document: no allocation proportional to the document.
    let mut edited = rope.clone();
    let at = edited.byte_of_line(lines / 2);
    edited.insert(at, "int inserted = 1;\n");
    let mut new_doc = new_doc;
    let (_, _, edit_peak) = measure(|| new_doc.apply_rope(edited.clone()));
    println!("apply_rope after one inserted line: peak {edit_peak} B");
    assert!(edit_peak < doc_len / 8, "incremental update must not copy the document, peak {edit_peak} B");
    let (_, _, string_peak) = measure(|| {
        let s = edited.to_string(); // what the old worker did before every incremental update
        s.len()
    });
    assert!(string_peak > doc_len, "reference: the old path really copied the document ({string_peak})");
    drop(new_doc);

    // ---- fold regions: String scan (old) versus rope scan (new) ----
    let (r_old, _, old_peak) = measure(|| {
        let t = rope.to_string();
        brouillon::folding::compute_fold_regions(&t)
    });
    let (r_new, _, new_peak) = measure(|| brouillon::folding::compute_fold_regions_rope(&rope));
    assert_eq!(r_old, r_new);
    println!("fold scan 4 MB code: old peak {old_peak} B, new peak {new_peak} B ({} regions)", r_new.len());
    assert!(old_peak > doc_len, "reference: the String scan copies the document");
    let region_bytes = r_new.len() * std::mem::size_of::<brouillon::folding::FoldRegion>();
    assert!(new_peak < region_bytes * 2 + lines * 8,"rope scan peak {new_peak} B = regions {region_bytes} B + per-line levels");
    assert!(brouillon::folding::detect_for_open(&crop::Rope::new()).regions.is_empty());

    // ---- undo history: cloning a buffer with a heavy history copies pointers, not the undo text ----
    let mut buf = brouillon_editor_engine::buffer::Buffer::from_rope(rope.clone());
    for k in 0..40usize {
        // 40 groups that each remove ~64 KB: ~2.5 MB of undo text
        let start = k * 20_000;
        let mut s = start;
        while !buf.rope().is_char_boundary(s) {
            s -= 1;
        }
        buf.remove_range(s..s + 64_000);
        buf.break_typing();
    }
    assert!(buf.undo_bytes() > 2 * MB, "history holds {} B", buf.undo_bytes());
    let (clones, held, peak) = measure(|| (0..10).map(|_| buf.clone()).collect::<Vec<_>>());
    println!("10 clones of a buffer with {} B of undo text: held {held} B, peak {peak} B", buf.undo_bytes());
    assert!(held < 100 * 1024 && peak < 100 * 1024, "clones must share the undo groups (held {held}, peak {peak})");
    drop(clones);
    // And an edit on the clone only copies the group it extends, not the history.
    let (_, _, edit_peak) = measure(|| {
        let mut nb = buf.clone();
        let at = nb.byte_len() / 2;
        let at = {
            let mut a = at;
            while !nb.rope().is_char_boundary(a) {
                a -= 1;
            }
            a
        };
        nb.typing_insert(at, "x", 10_000);
        nb
    });
    assert!(edit_peak < 200 * 1024, "one keystroke on a clone peaked at {edit_peak} B");

    // ---- diff: one line diff feeds every view ----
    let a: String = text.chars().take(MB).collect();
    let mut b = a.clone();
    let at = b.len() / 2 + b[b.len() / 2..].find('\n').unwrap() + 1; // a line boundary: one whole added line
    b.insert_str(at, "CHANGED LINE\n");
    let (_, _, old_peak) = measure(|| {
        let i = brouillon::diff::inline_rows(&a, &b);
        let s = brouillon::diff::side_by_side(&a, &b);
        let st = brouillon::diff::stats(&a, &b);
        (i, s, st)
    });
    let (d, _, new_peak) = measure(|| brouillon::diff_view::DiffData::new("a", "b", &a, &b));
    println!("diff 1 MB: old (3 diffs) peak {old_peak} B, new peak {new_peak} B");
    assert!(new_peak < old_peak * 3 / 4, "one pass must be clearly lighter than three ({new_peak} vs {old_peak})");
    assert_eq!((d.added, d.removed, d.hunks), (1, 0, 1));
}
