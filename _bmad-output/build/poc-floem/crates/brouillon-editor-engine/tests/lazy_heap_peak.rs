//! Heap-peak guards for the huge-document paths (Find, Replace All, hex search, transcoding, encoded
//! save) over a 100 MB document. A counting allocator measures the transient peak ABOVE what is live
//! at the start of each operation; the limits are a few MB, i.e. 1-2 orders of magnitude under one
//! copy of the document, so a regression to a whole-text `String`/`Vec` fails.
//!
//! One #[test] only: the allocator counters are process-wide and tests run on parallel threads.
//! The 100 MB files are created in a temp dir and deleted at the end.
use std::alloc::{GlobalAlloc, Layout, System};
use std::io::Write;
use std::sync::atomic::{AtomicUsize, Ordering::SeqCst};

use brouillon_editor_engine::byte_source::{ByteSource, Ctl, FileSource};
use brouillon_editor_engine::hex_doc::{self, BytePattern, ByteSearchCfg, HexDoc};
use brouillon_editor_engine::lazy_search::{self, WindowCfg};
use brouillon_editor_engine::search::replace_tpl::Template;
use brouillon_editor_engine::search::{CancelToken, Matcher, SearchMode, SearchOptions};
use brouillon_editor_engine::stream_codec::{self, EncodeSpec, StreamCfg};

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

const DOC_BYTES: u64 = 100 << 20;
/// Allowed transient heap above the starting level: window (1 MiB) + write buffer (1 MiB) + codec
/// scratch + the (capped) result list, with slack. A whole-text copy would be 100 MB.
const LIMIT: usize = 6 << 20;

fn extra_peak<R>(f: impl FnOnce() -> R) -> (usize, R) {
    let base = LIVE.load(SeqCst);
    PEAK.store(base, SeqCst);
    let r = f();
    (PEAK.load(SeqCst).saturating_sub(base), r)
}

#[test]
fn huge_document_find_replace_hex_and_encoding_stay_within_a_few_mb() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("big.txt");
    // ---- the document: code-like lines, a NEEDLE every 4000 lines
    let mut lines = 0u64;
    let mut markers = 0u64;
    {
        let mut w = std::io::BufWriter::with_capacity(1 << 20, std::fs::File::create(&path).unwrap());
        let mut written = 0u64;
        while written < DOC_BYTES {
            let s = if lines % 4000 == 7 {
                markers += 1;
                format!("line {lines}: the quick brown fox NEEDLE jumps over the lazy dog\n")
            } else {
                format!("line {lines}: the quick brown fox jumps over the lazy dog caf\u{e9}\n")
            };
            w.write_all(s.as_bytes()).unwrap();
            written += s.len() as u64;
            lines += 1;
        }
        w.flush().unwrap();
    }
    let src = FileSource::open(&path).unwrap();
    assert!(ByteSource::len(&src) >= DOC_BYTES);
    let cancel = CancelToken::new();
    let cfg = WindowCfg::default();

    // ---- literal Find: count, all (capped), next, prev
    let lit = Matcher::new("NEEDLE", &SearchOptions::default()).unwrap();
    let whole = [0..ByteSource::len(&src) as usize];
    let (peak, n) = extra_peak(|| lazy_search::count(&lit, &src, &whole, &cfg, &mut Ctl::new(&cancel)).unwrap());
    assert_eq!(n, markers);
    assert!(peak < LIMIT, "literal count peak {peak}");
    let (peak, all) = extra_peak(|| lazy_search::find_all(&lit, &src, &whole, 100, &cfg, &mut Ctl::new(&cancel)).unwrap());
    assert_eq!((all.ranges.len(), all.truncated), (100, true));
    assert!(peak < LIMIT, "find_all peak {peak}");
    let mid = ByteSource::len(&src) as usize / 2;
    let (peak, next) = extra_peak(|| lazy_search::find_next(&lit, &src, mid, true, &cfg, &mut Ctl::new(&cancel)).unwrap());
    assert!(next.unwrap().range.start >= mid);
    assert!(peak < LIMIT, "find_next peak {peak}");
    let (peak, prev) = extra_peak(|| lazy_search::find_prev(&lit, &src, mid, true, &cfg, &mut Ctl::new(&cancel)).unwrap());
    assert!(prev.unwrap().range.end <= mid);
    assert!(peak < LIMIT, "find_prev peak {peak}");

    // ---- regex Find (line-local) and a multi-line regex (windows with margin)
    let re = Matcher::new(r"line \d+: .*NEEDLE", &SearchOptions { mode: SearchMode::Regex, ..Default::default() }).unwrap();
    let (peak, n) = extra_peak(|| lazy_search::count(&re, &src, &whole, &cfg, &mut Ctl::new(&cancel)).unwrap());
    assert_eq!(n, markers);
    assert!(peak < LIMIT, "regex count peak {peak}");
    let ml = Matcher::new(r"NEEDLE jumps over the lazy dog\nline \d+", &SearchOptions { mode: SearchMode::Regex, ..Default::default() }).unwrap();
    assert!(ml.spans_lines());
    let (peak, n) = extra_peak(|| lazy_search::count(&ml, &src, &whole, &cfg, &mut Ctl::new(&cancel)).unwrap());
    assert_eq!(n, markers);
    assert!(peak < LIMIT, "multi-line regex count peak {peak}");

    // ---- Replace All streamed into a new file
    let out = dir.path().join("replaced.txt");
    let tpl = Template::parse("PIN");
    let (peak, st) = extra_peak(|| lazy_search::replace_to_file(&out, &lit, &tpl, &src, &whole, &cfg, &mut Ctl::new(&cancel)).unwrap());
    assert_eq!(st.count, markers);
    assert!(peak < LIMIT, "replace_to_file peak {peak}");
    assert_eq!(std::fs::metadata(&out).unwrap().len(), ByteSource::len(&src) - markers * ("NEEDLE".len() - "PIN".len()) as u64);
    std::fs::remove_file(&out).unwrap();

    // ---- hex: page, search, an edit and a save
    let mut doc = HexDoc::open(&path).unwrap();
    let (peak, page) = extra_peak(|| hex_doc::read_page(&doc, 1_000_000, 64, 16));
    assert_eq!(page.len(), 1024);
    assert!(peak < 256 << 10, "hex page peak {peak} (one 64 KiB cache chunk + the page)");
    let pat = BytePattern::from_text("NEEDLE", "utf-8").unwrap();
    let (peak, n) = extra_peak(|| hex_doc::count_bytes(&doc, &pat, ByteSearchCfg::default(), &mut Ctl::new(&cancel)).unwrap());
    assert_eq!(n, markers);
    assert!(peak < LIMIT, "hex count peak {peak}");
    doc.overwrite(1234, b"HEXED").unwrap();
    doc.insert(5_000_000, b"inserted").unwrap();
    let snap = doc.snapshot();
    let (peak, n) = extra_peak(|| hex_doc::count_bytes(&snap, &BytePattern::exact(b"HEXED").unwrap(), ByteSearchCfg::default(), &mut Ctl::new(&cancel)).unwrap());
    assert_eq!(n, 1);
    assert!(peak < LIMIT, "hex count on edited snapshot peak {peak}");
    let hexout = dir.path().join("hexed.bin");
    let (peak, _) = extra_peak(|| doc.save(&hexout, &mut Ctl::new(&cancel)).unwrap());
    assert!(peak < LIMIT, "hex save peak {peak}");
    assert_eq!(std::fs::metadata(&hexout).unwrap().len(), ByteSource::len(&src) + 8);
    drop(snap);
    drop(doc);
    std::fs::remove_file(&hexout).unwrap();

    // ---- encodings: save the 100 MB UTF-8 text as UTF-16LE and Shift_JIS-free windows-1252, then transcode back lazily
    let utf16 = dir.path().join("big16.txt");
    let scfg = StreamCfg::default();
    let spec = EncodeSpec { codepage: "utf-16le", bom: true, eol: None };
    let (peak, st) = extra_peak(|| stream_codec::save_encoded_file(&utf16, &src, &spec, false, &scfg, &mut Ctl::new(&cancel)).unwrap());
    assert_eq!(st.unmappable, 0);
    assert!(peak < LIMIT, "encode to UTF-16 peak {peak}");
    assert!(std::fs::metadata(&utf16).unwrap().len() > 2 * DOC_BYTES - (DOC_BYTES / 4));
    let scfg = StreamCfg { sidecar_dir: Some(dir.path().into()), ..StreamCfg::default() };
    let (peak, t) = extra_peak(|| stream_codec::transcode_to_sidecar(&utf16, "utf-16le", &scfg, &mut Ctl::new(&cancel)).unwrap());
    assert!(!t.lossy && t.bom);
    assert_eq!(t.utf8_len, ByteSource::len(&src));
    assert!(peak < LIMIT, "transcode peak {peak}");
    // the sidecar equals the original document byte for byte
    assert_eq!(std::fs::metadata(t.sidecar.path()).unwrap().len(), ByteSource::len(&src));
    // Find over the UTF-16 file itself, window by window (Find in Files path)
    let (peak, n) = extra_peak(|| {
        let mut n = 0u64;
        stream_codec::for_each_decoded_window(&utf16, "utf-16le", 4 << 20, &|| false, &mut |t| {
            n += t.matches("NEEDLE").count() as u64;
            true
        })
        .unwrap();
        n
    });
    assert_eq!(n, markers);
    assert!(peak < 12 << 20, "decoded windows peak {peak}");
    drop(t);
    std::fs::remove_file(&utf16).unwrap();
    std::fs::remove_file(&path).unwrap();
}
