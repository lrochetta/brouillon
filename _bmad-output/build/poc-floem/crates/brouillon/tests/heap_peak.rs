//! Heap-peak guards for the whole-document operations of the soak 'mixed' cycle (Save, save gate, outline).
//! A counting allocator measures the transient peak ABOVE what the 16 MB rope already holds. The limits
//! are far under one copy of the document, so a regression to a String / encoded copy fails.
use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicUsize, Ordering::SeqCst};

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

const DOC_MB: usize = 16;

/// Peak heap above the live heap at the start of `f`.
fn extra_peak<R>(f: impl FnOnce() -> R) -> (usize, R) {
    let base = LIVE.load(SeqCst);
    PEAK.store(base, SeqCst);
    let r = f();
    (PEAK.load(SeqCst).saturating_sub(base), r)
}

fn doc() -> crop::Rope {
    let mut b = crop::RopeBuilder::new();
    let line = "abcdefghij klmnopqrst uvwxyz 0123456789 caf\u{e9} \u{20ac}\n";
    for _ in 0..(DOC_MB << 20) / line.len() {
        b.append(line);
    }
    b.build()
}

/// The allocator counters are process-wide and tests run on parallel threads: tests that read them take this lock.
static SERIAL: std::sync::Mutex<()> = std::sync::Mutex::new(());

#[test]
fn whole_document_operations_do_not_copy_the_document() {
    let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let rope = doc();
    let one_copy = rope.byte_len();
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("big.txt");
    let meta = brouillon::tabs::DocMeta::default();
    let opts = brouillon::backup::BackupOptions { enabled: false, ..Default::default() };
    let spec = brouillon::file_io::SaveSpec { codepage: meta.codepage, bom: meta.bom, eol: None };
    let limit = one_copy / 8; // 2 MB for a 16 MB document

    // Reference: the old path really cost several copies (proves the threshold is meaningful).
    let (old, _) = extra_peak(|| {
        let text = rope.to_string();
        brouillon::wiring::save_document(&target, &text, &meta, false, &opts)
    });
    assert!(old > 2 * one_copy, "old save peak {old} should exceed two document copies ({one_copy})");
    let old_bytes = std::fs::read(&target).unwrap();

    let (save, r) = extra_peak(|| brouillon::wiring::save_document_rope(&target, &rope, &meta, false, &opts));
    assert!(matches!(r, brouillon::wiring::SaveResult::Saved { .. }));
    assert!(save < limit, "streamed save peak {save} bytes must stay under {limit}");
    assert!(std::fs::read(&target).unwrap() == old_bytes, "same bytes as the String save");
    drop(old_bytes);

    // Save gate against a changed disk: compared while streaming.
    brouillon::external::note(&target);
    {
        let f = std::fs::OpenOptions::new().write(true).open(&target).unwrap();
        f.set_modified(std::time::SystemTime::now() + std::time::Duration::from_secs(9)).unwrap();
    }
    let (gate, g) = extra_peak(|| brouillon::external::save_gate_pieces(&target, || rope.chunks(), &spec));
    assert_eq!(g, brouillon::external::SaveGate::Write, "identical content: no conflict");
    assert!(gate < limit, "save gate peak {gate} must stay under {limit}");

    // Outline for Ctrl+P.
    let path = std::path::Path::new("x.txt");
    let (outline, _) = extra_peak(|| brouillon::quick_open_view::extract_symbols_rope(&rope, Some(path)));
    assert!(outline < limit, "outline peak {outline} must stay under {limit}");
}

// ---------------------------------------------------------------------------------------------------------------
// Large file mode: a 100 MB document on the lazy store must cost (almost) no memory, whatever is done to it.
// ---------------------------------------------------------------------------------------------------------------

/// Generate a ~100 MB code-like UTF-8 file (LF, a few multi-byte characters) with a marker every 4000 lines.
fn write_big_file(path: &std::path::Path, mb: usize) -> u64 {
    use std::io::Write;
    let mut w = std::io::BufWriter::with_capacity(1 << 20, std::fs::File::create(path).unwrap());
    let mut total = 0u64;
    let mut i = 0u64;
    while total < (mb as u64) << 20 {
        let line = if i % 4000 == 0 {
            format!("// MARKER-{i} caf\u{e9} \u{20ac}\n")
        } else {
            format!("    let value_{i} = compute({i}, \"text\"); // line {i}\n")
        };
        w.write_all(line.as_bytes()).unwrap();
        total += line.len() as u64;
        i += 1;
    }
    w.flush().unwrap();
    total
}

#[test]
fn lazy_buffer_of_100_mb_stays_under_20_mb_private_after_open_and_10k_edits() {
    use brouillon_editor_engine::buffer::Buffer;
    use brouillon_textstore_spike::os_mem::sample;
    use std::sync::Arc;
    let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("huge.txt");
    let size = write_big_file(&path, 100);
    assert!(size >= 100 << 20);
    let mb = |n: u64| n as f64 / 1048576.0;
    let limit = 20u64 << 20;

    let heap0 = LIVE.load(SeqCst);
    let private0 = sample().private;
    let buf = Arc::new(Buffer::open_lazy(&path).unwrap());
    assert!(buf.is_lazy());
    // the first viewport: top of the file is available at once
    assert!(buf.nth_line(0).unwrap().starts_with("// MARKER-0"));
    let after_open = sample().private;
    eprintln!("[lazy] private after open + first viewport: {:.1} MB (before the open: {:.1})", mb(after_open), mb(private0));
    assert!(after_open < limit, "private bytes after open: {:.1} MB", mb(after_open));
    assert!(LIVE.load(SeqCst).saturating_sub(heap0) < 4 << 20, "heap held by an opened lazy buffer");

    // 10,000 scattered edits the way the UI makes them: clone the buffer, edit, publish the new Arc.
    let mut cur = buf.clone();
    let len0 = cur.byte_len();
    let mut seed = 0x9E37_79B9_7F4A_7C15u64;
    let mut rnd = move || {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        seed
    };
    let t = std::time::Instant::now();
    for i in 0..10_000u64 {
        let mut nb = (*cur).clone();
        let line = (rnd() % 900_000) as usize;
        let at = nb.byte_of_line(line);
        match i % 10 {
            0..=5 => nb.typing_insert(at, "x", i * 2000),
            6..=8 => {
                let end = nb.byte_of_line(line + 1).min(at + 17);
                let end = (at..=end).rev().find(|&b| nb.is_char_boundary(b)).unwrap_or(at);
                nb.remove_range(at..end);
            }
            _ => nb.replace_range(at..at, "inserted line\n"),
        }
        cur = Arc::new(nb);
    }
    let edit_time = t.elapsed();
    assert!(cur.is_dirty() && cur.byte_len() != len0);
    let after_edits = sample().private;
    eprintln!("[lazy] private after 10k edits: {:.1} MB ({} ms for the 10k edits, {:?} per edit)", mb(after_edits), edit_time.as_millis(), edit_time / 10_000);
    assert!(after_edits < limit, "private bytes after 10k edits: {:.1} MB", mb(after_edits));

    // Find over the whole file on a snapshot (what the worker thread does), then undo everything.
    let snap = cur.snapshot();
    let m = brouillon_editor_engine::search::Matcher::new("MARKER-", &brouillon_editor_engine::search::SearchOptions::default()).unwrap();
    let mut count = 0;
    brouillon_editor_engine::search::live::run(
        &snap,
        &m,
        &brouillon_editor_engine::search::live::Job { dir: brouillon_editor_engine::search::live::Direction::Next(0), count: true, count_delay: std::time::Duration::ZERO },
        &brouillon_editor_engine::search::CancelToken::new(),
        &mut |p| count = p.count,
    )
    .unwrap();
    assert!(count > 100, "markers found: {count}");
    let after_find = sample().private;
    eprintln!("[lazy] private after a Find over the whole file: {:.1} MB ({count} matches)", mb(after_find));
    assert!(after_find < limit, "private bytes after Find: {:.1} MB", mb(after_find));

    let mut undo = (*cur).clone();
    undo.undo_all();
    let after_undo = sample().private;
    eprintln!("[lazy] private after undo-all: {:.1} MB, heap held by the buffer: {:.1} MB", mb(after_undo), mb(LIVE.load(SeqCst).saturating_sub(heap0) as u64));
    assert!(after_undo < limit, "private bytes after undo-all: {:.1} MB", mb(after_undo));
}
