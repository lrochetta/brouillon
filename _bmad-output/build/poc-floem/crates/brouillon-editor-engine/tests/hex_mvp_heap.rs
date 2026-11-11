//! Heap-peak guard for the hex MVP engine (hashes, Copy as, inspector, regions, nibble edit) on a
//! 100 MB document. A counting allocator measures the transient peak above what is live at the start
//! of each operation: a few MB at most, so a regression to a whole-file or whole-selection copy fails.
//!
//! One #[test] only (the allocator counters are process-wide). The file is sparse where the OS
//! allows it and is deleted at the end.
use std::alloc::{GlobalAlloc, Layout, System};
use std::io::{Seek, SeekFrom, Write};
use std::sync::atomic::{AtomicUsize, Ordering::SeqCst};

use brouillon_editor_engine::byte_source::{ByteSource, Ctl};
use brouillon_editor_engine::copy_as::{copy_range_as, CopyFormat};
use brouillon_editor_engine::hex_doc::HexDoc;
use brouillon_editor_engine::hex_hashes::{hash_range, HashAlgo};
use brouillon_editor_engine::hex_inspect::{inspect, Endian};
use brouillon_editor_engine::hex_regions::{RegionColor, RegionSet};
use brouillon_editor_engine::search::CancelToken;

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
/// Hash window (1 MiB) + one more for slack; far below one copy of the document.
const HASH_LIMIT: usize = 3 << 20;
/// 64 KiB selection: source buffer + text (spaced hex is 3x) + a little.
const COPY_LIMIT: usize = 1 << 20;

fn extra_peak<R>(f: impl FnOnce() -> R) -> (usize, R) {
    let base = LIVE.load(SeqCst);
    PEAK.store(base, SeqCst);
    let r = f();
    (PEAK.load(SeqCst).saturating_sub(base), r)
}

#[test]
fn hex_mvp_operations_stay_within_a_few_mb_on_a_100mb_document() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("big.bin");
    {
        let mut f = std::fs::File::create(&path).unwrap();
        f.write_all(&(0..=255u8).cycle().take(1 << 20).collect::<Vec<u8>>()).unwrap();
        f.seek(SeekFrom::Start(DOC_BYTES - 4)).unwrap();
        f.write_all(&[1, 2, 3, 4]).unwrap();
    }
    let cancel = CancelToken::new();
    let mut doc = HexDoc::open(&path).unwrap();
    assert_eq!(ByteSource::len(&doc), DOC_BYTES);

    // ---- hash of a 64 KiB selection, every algorithm
    let (peak, r) = extra_peak(|| hash_range(&doc, 4096..4096 + 65_536, &HashAlgo::ALL, &mut Ctl::new(&cancel)).unwrap());
    assert_eq!(r.len(), 4);
    assert!(peak < HASH_LIMIT, "64 KiB hash peak {peak}");

    // ---- hash of the whole 100 MB (CRC-32 + SHA-256), streamed
    let (peak, r) = extra_peak(|| hash_range(&doc, 0..DOC_BYTES, &[HashAlgo::Crc32, HashAlgo::Sha256], &mut Ctl::new(&cancel)).unwrap());
    assert_eq!(r.len(), 2);
    assert!(peak < HASH_LIMIT, "100 MB hash peak {peak}");

    // ---- Copy as of 64 KiB, every format
    for f in CopyFormat::ALL {
        let (peak, s) = extra_peak(|| copy_range_as(&doc, 8192..8192 + 65_536, f).unwrap());
        assert!(!s.is_empty());
        assert!(peak < COPY_LIMIT, "copy as {f:?} peak {peak}");
    }

    // ---- inspector at the end of the document, regions, nibble edit
    let mut tail = [0u8; 16];
    let n = doc.read_at(DOC_BYTES - 4, &mut tail);
    let (peak, rows) = extra_peak(|| inspect(&tail[..n], Endian::Big));
    assert_eq!(rows.len(), 19);
    assert!(peak < 64 << 10, "inspector peak {peak}");
    doc.set_track_edits(true);
    let mut regions = RegionSet::new();
    regions.add(1000..2000, "block", RegionColor::Green, DOC_BYTES).unwrap();
    let (peak, _) = extra_peak(|| {
        doc.set_nibble(1500, true, 0xA).unwrap();
        doc.set_nibble(1500, false, 0xB).unwrap();
        doc.drain_edits().into_iter().for_each(|d| regions.apply_edit(d));
    });
    assert!(peak < 256 << 10, "nibble edit peak {peak} (cache chunks of 64 KiB for the two reads)");
    let mut b = [0u8; 1];
    doc.read_at(1500, &mut b);
    assert_eq!(b[0], 0xAB);
    assert!(doc.undo());
    doc.read_at(1500, &mut b);
    assert_eq!(b[0], (1500 % 256) as u8);
}
