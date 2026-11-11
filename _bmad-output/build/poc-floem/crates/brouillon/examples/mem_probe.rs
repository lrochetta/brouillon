//! Headless memory probe (no window): heap bytes held by a large document and the transient peak of the
//! per-keystroke paths, with a counting allocator. Run:
//!   cargo run --release -p brouillon --example mem_probe -- <size-mb> [file]
//! "live" = bytes currently allocated, "peak" = high-water mark since the last reset. Heap only (no GPU, no
//! thread stacks, no code pages), so it explains the text-related part of the working set, not the whole.
use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicUsize, Ordering::Relaxed};

use brouillon::wiring::{self, Window};
use brouillon_editor_engine::autoindent::{on_enter, IndentProfile};
use brouillon_editor_engine::buffer::Buffer;

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

fn mb(n: usize) -> f64 {
    n as f64 / 1048576.0
}
fn reset_peak() {
    PEAK.store(LIVE.load(Relaxed), Relaxed);
}
fn report(what: &str) {
    println!("{what:<46} live {:>8.1} MB   peak {:>8.1} MB", mb(LIVE.load(Relaxed)), mb(PEAK.load(Relaxed)));
}

fn main() {
    let mut args = std::env::args().skip(1);
    let size_mb: usize = args.next().and_then(|a| a.parse().ok()).unwrap_or(100);
    let path = args.next().map(std::path::PathBuf::from).unwrap_or_else(|| std::env::temp_dir().join(format!("mem_probe_{size_mb}.txt")));
    if !path.exists() {
        let line: String = "abcdefghij klmnopqrst uvwxyz 0123456789 ".repeat(3).chars().take(98).collect::<String>() + "\n";
        let mut f = std::io::BufWriter::new(std::fs::File::create(&path).unwrap());
        for _ in 0..(size_mb * 1048576 / 99) {
            std::io::Write::write_all(&mut f, line.as_bytes()).unwrap();
        }
    }
    report("start");
    // `-- <size-mb> <file> lazy`: large file mode (the lazy text store). Heap from the counting allocator, and the
    // OS "private bytes" of the process (what Task Manager shows as commit), at each stage.
    if std::env::args().any(|a| a == "lazy") {
        lazy_probe(&path);
        return;
    }
    reset_peak();
    {
        let s = brouillon_editor_engine::file_io::read_utf8_rope(&path).unwrap().expect("utf-8");
        let b = Buffer::from_rope(s.rope);
        report("load, new (streamed into the rope)");
        drop(b);
    }
    reset_peak();
    // Startup: file -> text -> rope (what main() does), then the text is dropped.
    let loaded = brouillon_editor_engine::file_io::read_file(&path, "windows-1252").unwrap();
    report("load, old: after read+decode (String alive)");
    let buf = std::sync::Arc::new(Buffer::from_str(&loaded.text));
    report("after rope built (String still alive)");
    drop(loaded);
    report("after dropping the String");
    println!("   rope text {:.1} MB, {} lines\n", mb(buf.byte_len()), buf.line_count());

    let profile = IndentProfile::for_language("txt");
    let cursor = (buf.line_count() / 2, 5);
    // Enter, old path: full to_string for the handler + again in apply_ui_outcome.
    reset_peak();
    {
        let text = buf.rope().to_string();
        let regions = wiring::regions_from(&text, cursor, None);
        let out = on_enter(&text, regions[0].head, &profile);
        let nb = wiring::apply_outcome(&buf, &out, &regions).unwrap();
        let text_now = nb.rope().to_string();
        let _ = wiring::split_regions(&text_now, out.selection.as_ref().unwrap());
    }
    report("Enter, old (2 full copies)");
    // Enter, new path.
    reset_peak();
    {
        let regions = wiring::regions_from_buf(&buf, cursor, None);
        let win = Window::around(&buf, regions[0].head);
        let out = on_enter(&win.text, regions[0].head - win.base, &profile);
        let out = win.abs_outcome(out);
        let nb = wiring::apply_outcome(&buf, &out, &regions).unwrap();
        let _ = wiring::split_regions_buf(&nb, out.selection.as_ref().unwrap());
    }
    report("Enter, new (window)");
    // Plain typing: clone + insert.
    reset_peak();
    {
        let mut nb = (*buf).clone();
        let at = wiring::byte_of_line_col(&nb, cursor);
        nb.typing_insert(at, "a", 1);
    }
    report("typing one char (clone + insert)");

    // ---- the other operations of the leak.ps1 'mixed' cycle: save, quick open (Ctrl+P), find ----
    let dir = path.parent().map(|p| p.join(format!("mem_probe_out_{size_mb}"))).unwrap();
    std::fs::create_dir_all(&dir).unwrap();
    let target = dir.join("saved.txt");
    let meta = brouillon::tabs::DocMeta::default();
    let opts = brouillon::backup::BackupOptions { enabled: false, ..Default::default() };
    let spec = brouillon::file_io::SaveSpec { codepage: meta.codepage, bom: meta.bom, eol: None };

    // Save, old: rope -> String, disk gate (disk unchanged: no read), prepare_save (encode copy + BOM copy), write.
    reset_peak();
    {
        let text = buf.rope().to_string();
        let _ = brouillon::external::save_gate(&target, &text, &spec);
        match wiring::save_document(&target, &text, &meta, false, &opts) {
            wiring::SaveResult::Saved { .. } => {}
            other => panic!("{other:?}"),
        }
    }
    report("Save, old (String + encoded copy)");
    let old_file = dir.join("saved_old.txt");
    std::fs::rename(&target, &old_file).unwrap();
    // Save, new: chunks encoded straight into the atomic temp file.
    reset_peak();
    match wiring::save_document_rope(&target, buf.rope(), &meta, false, &opts) {
        wiring::SaveResult::Saved { .. } => {}
        other => panic!("{other:?}"),
    }
    report("Save, new (streamed rope chunks)");
    {
        use std::io::Read;
        let (mut a, mut b) = (std::io::BufReader::new(std::fs::File::open(&old_file).unwrap()), std::io::BufReader::new(std::fs::File::open(&target).unwrap()));
        let (mut ba, mut bb) = (vec![0u8; 1 << 16], vec![0u8; 1 << 16]);
        loop {
            let n = a.read(&mut ba).unwrap();
            if n == 0 {
                assert_eq!(b.read(&mut bb).unwrap(), 0, "streamed save is longer");
                break;
            }
            b.read_exact(&mut bb[..n]).unwrap();
            assert!(ba[..n] == bb[..n], "the streamed save must write the same bytes");
        }
    }
    let _ = std::fs::remove_file(&old_file);
    // Save when the disk changed behind us: the gate compares content (worst case of the gate).
    {
        let f = std::fs::OpenOptions::new().write(true).open(&target).unwrap();
        f.set_modified(std::time::SystemTime::now() + std::time::Duration::from_secs(9)).unwrap();
    }
    reset_peak();
    {
        let text = buf.rope().to_string();
        let _ = brouillon::external::save_gate(&target, &text, &spec);
    }
    report("Save gate on a changed disk, old (String+encode+read)");
    reset_peak();
    let _ = brouillon::external::save_gate_pieces(&target, || buf.rope().chunks(), &spec);
    report("Save gate on a changed disk, new (streamed compare)");
    // After a save the editor clones the Buffer to mark it saved.
    reset_peak();
    {
        let mut nb = (*buf).clone();
        nb.mark_saved();
    }
    report("clone + mark_saved after a save");

    // Quick open / Go to function: outline of the document.
    reset_peak();
    {
        let text = buf.rope().to_string();
        let _ = brouillon::quick_open_view::extract_symbols(&text, Some(std::path::Path::new("mem_probe.txt")));
    }
    report("Ctrl+P outline, old (String + line index)");
    reset_peak();
    let _ = brouillon::quick_open_view::extract_symbols_rope(buf.rope(), Some(std::path::Path::new("mem_probe.txt")));
    report("Ctrl+P outline, new (line by line)");
    let md = std::path::Path::new("mem_probe.md");
    reset_peak();
    let n_md = brouillon::quick_open_view::extract_symbols_rope(buf.rope(), Some(md)).len();
    report("Ctrl+P outline as Markdown, new");
    println!("   markdown symbols found: {n_md}");

    // Find: count + first match over the rope (what the worker thread does).
    {
        use brouillon_editor_engine::search::{CancelToken, Matcher, SearchMode, SearchOptions};
        let m = Matcher::new("(?i)klm", &SearchOptions { mode: SearchMode::Regex, ..SearchOptions::default() }).unwrap();
        reset_peak();
        let n = m.count(buf.rope(), &[0..buf.byte_len()], &CancelToken::new()).unwrap();
        let _ = m.find_next(buf.rope(), 0, true, &CancelToken::new());
        report("Find 'klm': count all + first match");
        println!("   matches: {n}");
    }
    let _ = std::fs::remove_dir_all(&dir);
}


/// Large file mode: open lazily, edit 10,000 times, Find, save, undo everything; report heap + private bytes.
fn lazy_probe(path: &std::path::Path) {
    use brouillon_textstore_spike::os_mem::sample;
    let private = |what: &str| {
        let m = sample();
        println!("   {what:<44} private {:>7.1} MB   working set {:>7.1} MB", mb(m.private as usize), mb(m.working_set as usize));
    };
    private("process start");
    let t = std::time::Instant::now();
    let buf = std::sync::Arc::new(Buffer::open_lazy(path).unwrap());
    println!("   open: {:?}, first line {:?}", t.elapsed(), buf.nth_line(0).map(|l| l.chars().take(30).collect::<String>()));
    report("lazy open (heap)");
    private("after open + first viewport");
    let mut cur = buf.clone();
    let mut seed = 0x9E37_79B9_7F4A_7C15u64;
    let mut rnd = move || {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        seed
    };
    let lines = buf.line_count_exact().max(1);
    let t = std::time::Instant::now();
    for i in 0..10_000u64 {
        let mut nb = (*cur).clone();
        let at = nb.byte_of_line((rnd() as usize) % lines);
        if i % 3 == 2 {
            let end = (at..=at + 9).rev().find(|&b| nb.is_char_boundary(b)).unwrap_or(at);
            nb.remove_range(at..end);
        } else {
            nb.typing_insert(at, "x", i * 2000);
        }
        cur = std::sync::Arc::new(nb);
    }
    println!("   10,000 edits (clone + edit each): {:?} ({:?} per edit)", t.elapsed(), t.elapsed() / 10_000);
    report("after 10k edits (heap)");
    private("after 10k edits");
    let snap = cur.snapshot();
    let t = std::time::Instant::now();
    let mut n = 0usize;
    snap.visit_str(0..snap.byte_len(), &mut |c| {
        n += c.matches("klm").count();
        true
    });
    println!("   Find 'klm' over the whole file via the snapshot: {n} matches in {:?}", t.elapsed());
    private("after Find");
    let out = path.with_extension("probe-out");
    let t = std::time::Instant::now();
    let mut saved = (*cur).clone();
    saved.save_to_path(&out).unwrap();
    println!("   save: {:?}", t.elapsed());
    private("after save");
    let t = std::time::Instant::now();
    let undone = saved.undo_all();
    println!("   undo all ({undone} groups): {:?}", t.elapsed());
    private("after undo-all");
    let _ = std::fs::remove_file(&out);
}
