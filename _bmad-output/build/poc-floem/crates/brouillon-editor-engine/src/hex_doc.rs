//! Hex mode for huge documents (lazy-store task 2): paged reads, byte-level edits through a piece
//! table, byte/hex search. No whole-file copy anywhere.
//!
//! # Design
//!
//! * `HexDoc` is a byte-level piece table over a read-only base FILE (positional reads through a
//!   64 KiB x 8 LRU, large reads bypass it) plus an append-only add buffer. It sees the file's REAL
//!   bytes (BOM included, any encoding), which is what a hex view must show; it is independent of
//!   the UTF-8 text store (whose view strips a UTF-8 BOM and refuses edits on invalid UTF-8).
//! * A view needs only [`read_page`]: `rows x bytes_per_row` bytes starting at a row. Cost is
//!   proportional to the page, not to the file or to the number of edits (binary search in the
//!   piece layout).
//! * Edits: `replace(range, bytes)` (insert = empty range, delete = empty bytes, overwrite = same
//!   length), one undo record of ~100 bytes per edit, groups, undo/redo. Cap: [`MAX_PIECES`] pieces
//!   and [`MAX_ADD_BYTES`] of inserted bytes between saves; beyond that `HexError::TooManyEdits`
//!   (save, and the piece table collapses to one piece again). So hex editing of a 1 GB file is
//!   possible, not read-only, but bounded: a user typing hex digits is nowhere near the caps.
//! * Save streams the pieces to a temp file in the same directory (atomic rename, via
//!   `durable_write_with`) and rebases the document on the new file. Older generations stay alive
//!   only while undo/redo still refer to them (undo across a save works).
//! * Snapshots: [`HexDoc::snapshot`] is `Send + Sync` and O(1) (an `Arc` of the layout); it is what
//!   Find hands to a worker thread while the user keeps editing.
//! * Search in hex: [`BytePattern`] (exact bytes, `DE AD ?? EF` with wildcards, or text in a
//!   codepage) over any [`ByteSource`] in 1 MiB windows overlapping by `len-1` (matches straddling
//!   a window edge are found; tested at tiny windows). Next, previous, count, find-all (capped).

use std::fs::File;
use std::io::{self, Write};
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, RwLock};
use std::time::SystemTime;

use crate::byte_source::{ByteSource, Ctl};
use crate::hex_regions::EditDelta;
use crate::search::Cancelled;

pub const BYTES_PER_ROW: usize = 16;
/// Piece-table cap between saves (24 bytes per piece in memory).
pub const MAX_PIECES: usize = 200_000;
/// Inserted-bytes cap between saves.
pub const MAX_ADD_BYTES: usize = 64 << 20;
/// Largest `fill` / `insert_fill` in one call (a transient buffer of this size).
pub const MAX_FILL_BYTES: u64 = 16 << 20;
/// Edit deltas kept for `drain_edits` before the oldest are dropped.
const MAX_LOGGED_EDITS: usize = 65_536;

#[derive(Debug)]
pub enum HexError {
    OutOfRange,
    /// Empty fill pattern, or a fill above `MAX_FILL_BYTES`.
    BadFill,
    /// Too many edits since the last save: save first.
    TooManyEdits,
    Io(io::Error),
}
impl std::fmt::Display for HexError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            HexError::OutOfRange => write!(f, "range out of bounds"),
            HexError::BadFill => write!(f, "empty fill pattern or fill too large"),
            HexError::TooManyEdits => write!(f, "too many hex edits since the last save: save first"),
            HexError::Io(e) => write!(f, "{e}"),
        }
    }
}
impl std::error::Error for HexError {}
impl From<io::Error> for HexError {
    fn from(e: io::Error) -> Self {
        HexError::Io(e)
    }
}

// ------------------------------------------------------------------------------------------ base file

#[cfg(windows)]
fn pread(f: &File, buf: &mut [u8], off: u64) -> io::Result<usize> {
    use std::os::windows::fs::FileExt;
    f.seek_read(buf, off)
}
#[cfg(unix)]
fn pread(f: &File, buf: &mut [u8], off: u64) -> io::Result<usize> {
    use std::os::unix::fs::FileExt;
    f.read_at(buf, off)
}

const CHUNK: usize = 64 << 10;
const LRU: usize = 8;

struct HexBase {
    file: File,
    len: u64,
    path: PathBuf,
    stamp: (u64, Option<SystemTime>),
    cache: Mutex<Vec<(u64, Arc<[u8]>)>>,
}

impl HexBase {
    fn open(path: &Path) -> io::Result<Arc<Self>> {
        let file = File::open(path)?;
        let md = file.metadata()?;
        Ok(Arc::new(Self { file, len: md.len(), path: path.to_path_buf(), stamp: (md.len(), md.modified().ok()), cache: Mutex::new(Vec::new()) }))
    }
    fn read_direct(&self, buf: &mut [u8], off: u64) -> usize {
        let want = (self.len.saturating_sub(off) as usize).min(buf.len());
        let mut done = 0;
        while done < want {
            match pread(&self.file, &mut buf[done..want], off + done as u64) {
                Ok(0) => break,
                Ok(n) => done += n,
                Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                Err(_) => break,
            }
        }
        done
    }
    fn chunk(&self, idx: u64) -> Arc<[u8]> {
        let mut c = self.cache.lock().unwrap();
        if let Some(p) = c.iter().position(|(i, _)| *i == idx) {
            let e = c.remove(p);
            let a = e.1.clone();
            c.push(e);
            return a;
        }
        let mut v = vec![0u8; CHUNK];
        let n = self.read_direct(&mut v, idx * CHUNK as u64);
        v.truncate(n);
        let a: Arc<[u8]> = v.into();
        if c.len() >= LRU {
            c.remove(0);
        }
        c.push((idx, a.clone()));
        a
    }
    fn read(&self, off: u64, buf: &mut [u8]) -> usize {
        if buf.len() >= CHUNK {
            return self.read_direct(buf, off);
        }
        let mut done = 0;
        while done < buf.len() {
            let pos = off + done as u64;
            let ch = self.chunk(pos / CHUNK as u64);
            let ch: &[u8] = &ch;
            let o = (pos % CHUNK as u64) as usize;
            if o >= ch.len() {
                break;
            }
            let n = (ch.len() - o).min(buf.len() - done);
            buf[done..done + n].copy_from_slice(&ch[o..o + n]);
            done += n;
        }
        done
    }
    fn changed(&self) -> io::Result<bool> {
        let md = std::fs::metadata(&self.path)?;
        Ok((md.len(), md.modified().ok()) != self.stamp)
    }
}

// ------------------------------------------------------------------------------------------ layout

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Piece {
    /// 0 = add buffer, i>0 = base generation i.
    src: u16,
    start: u64,
    len: u64,
}

#[derive(Clone, Default)]
struct Layout {
    pieces: Vec<Piece>,
    /// starts[i] = document offset of pieces[i].
    starts: Vec<u64>,
}

impl Layout {
    fn reindex(&mut self) {
        self.starts.clear();
        let mut o = 0;
        for p in &self.pieces {
            self.starts.push(o);
            o += p.len;
        }
    }
    /// Index of the piece containing `off` (or pieces.len() at the end).
    fn locate(&self, off: u64) -> usize {
        self.starts.partition_point(|&s| s <= off).saturating_sub(1)
    }
    /// Make `off` a piece boundary; returns the index of the piece starting at `off`.
    fn split_at(&mut self, off: u64, total: u64) -> usize {
        if off >= total {
            return self.pieces.len();
        }
        let i = self.locate(off);
        let s = self.starts[i];
        if s == off {
            return i;
        }
        let p = self.pieces[i];
        let l = off - s;
        self.pieces[i] = Piece { len: l, ..p };
        self.pieces.insert(i + 1, Piece { src: p.src, start: p.start + l, len: p.len - l });
        self.starts.insert(i + 1, off);
        i + 1
    }
    fn mergeable(a: &Piece, b: &Piece) -> bool {
        a.src == b.src && a.start + a.len == b.start
    }
    /// Replace `[a, b)` by `ins`; returns the removed pieces.
    fn splice(&mut self, a: u64, b: u64, ins: &[Piece], total: u64) -> Vec<Piece> {
        let ia = self.split_at(a, total);
        let ib = self.split_at(b, total);
        let removed: Vec<Piece> = self.pieces.drain(ia..ib).collect();
        let at = ia;
        self.pieces.splice(at..at, ins.iter().copied().filter(|p| p.len > 0));
        // merge around the insertion
        let hi = (at + ins.len() + 1).min(self.pieces.len());
        let mut i = at.saturating_sub(1);
        while i + 1 < hi.min(self.pieces.len()) {
            if Self::mergeable(&self.pieces[i], &self.pieces[i + 1]) {
                self.pieces[i].len += self.pieces[i + 1].len;
                self.pieces.remove(i + 1);
            } else {
                i += 1;
            }
        }
        self.reindex();
        removed
    }
}

#[derive(Clone)]
struct Rec {
    pos: u64,
    removed: Vec<Piece>,
    inserted: Vec<Piece>,
}
impl Rec {
    fn removed_len(&self) -> u64 {
        self.removed.iter().map(|p| p.len).sum()
    }
    fn inserted_len(&self) -> u64 {
        self.inserted.iter().map(|p| p.len).sum()
    }
}

type Bases = Arc<Vec<Option<Arc<HexBase>>>>;

fn read_pieces(layout: &Layout, bases: &Bases, add: &RwLock<Vec<u8>>, mut off: u64, buf: &mut [u8]) -> usize {
    let total: u64 = layout.pieces.last().map_or(0, |p| layout.starts[layout.pieces.len() - 1] + p.len);
    if off >= total {
        return 0;
    }
    let want = (buf.len() as u64).min(total - off) as usize;
    let mut done = 0;
    let mut i = layout.locate(off);
    while done < want && i < layout.pieces.len() {
        let p = layout.pieces[i];
        let in_p = off - layout.starts[i];
        let n = ((p.len - in_p) as usize).min(want - done);
        let dst = &mut buf[done..done + n];
        let got = if p.src == 0 {
            let a = add.read().unwrap();
            let s = (p.start + in_p) as usize;
            dst.copy_from_slice(&a[s..s + n]);
            n
        } else {
            match bases[p.src as usize - 1].as_ref() {
                Some(b) => b.read(p.start + in_p, dst),
                None => 0,
            }
        };
        done += got;
        if got < n {
            break;
        }
        off += n as u64;
        i += 1;
    }
    done
}

// ------------------------------------------------------------------------------------------ the document

pub struct HexDoc {
    layout: Arc<Layout>,
    bases: Bases,
    add: Arc<RwLock<Vec<u8>>>,
    len: u64,
    version: u64,
    saved_version: u64,
    undo: Vec<Vec<Rec>>,
    redo: Vec<Vec<Rec>>,
    open_group: Option<Vec<Rec>>,
    max_pieces: usize,
    max_add: usize,
    last_edit: Option<EditDelta>,
    track_edits: bool,
    edit_log: Vec<EditDelta>,
    /// (offset, version) of a high nibble just written, so the low nibble joins its undo step.
    nibble_pending: Option<(u64, u64)>,
}

/// An immutable view of a `HexDoc` at one moment: `Send + Sync`, O(1) to take.
#[derive(Clone)]
pub struct HexSnapshot {
    layout: Arc<Layout>,
    bases: Bases,
    add: Arc<RwLock<Vec<u8>>>,
    len: u64,
}

impl ByteSource for HexSnapshot {
    fn len(&self) -> u64 {
        self.len
    }
    fn read_at(&self, off: u64, buf: &mut [u8]) -> usize {
        read_pieces(&self.layout, &self.bases, &self.add, off, buf)
    }
}
impl ByteSource for HexDoc {
    fn len(&self) -> u64 {
        self.len
    }
    fn read_at(&self, off: u64, buf: &mut [u8]) -> usize {
        read_pieces(&self.layout, &self.bases, &self.add, off, buf)
    }
}

impl HexDoc {
    /// Open without reading the file (O(1)).
    pub fn open(path: &Path) -> io::Result<Self> {
        let base = HexBase::open(path)?;
        let mut layout = Layout::default();
        if base.len > 0 {
            layout.pieces.push(Piece { src: 1, start: 0, len: base.len });
        }
        layout.reindex();
        Ok(Self {
            len: base.len,
            layout: Arc::new(layout),
            bases: Arc::new(vec![Some(base)]),
            add: Arc::new(RwLock::new(Vec::new())),
            version: 0,
            saved_version: 0,
            undo: Vec::new(),
            redo: Vec::new(),
            open_group: None,
            max_pieces: MAX_PIECES,
            max_add: MAX_ADD_BYTES,
            last_edit: None,
            track_edits: false,
            edit_log: Vec::new(),
            nibble_pending: None,
        })
    }

    /// Lower the edit caps (tests, or a stricter policy for a low-memory profile).
    pub fn set_limits(&mut self, max_pieces: usize, max_add_bytes: usize) {
        self.max_pieces = max_pieces;
        self.max_add = max_add_bytes;
    }

    pub fn snapshot(&self) -> HexSnapshot {
        HexSnapshot { layout: self.layout.clone(), bases: self.bases.clone(), add: self.add.clone(), len: self.len }
    }
    pub fn version(&self) -> u64 {
        self.version
    }
    pub fn is_dirty(&self) -> bool {
        self.version != self.saved_version
    }
    pub fn piece_count(&self) -> usize {
        self.layout.pieces.len()
    }
    /// The file changed on disk since it was opened (length or mtime).
    pub fn externally_changed(&self) -> io::Result<bool> {
        match self.bases.iter().rev().flatten().next() {
            Some(b) => b.changed(),
            None => Ok(false),
        }
    }
    pub fn heap_estimate(&self) -> usize {
        self.layout.pieces.capacity() * (std::mem::size_of::<Piece>() + 8)
            + self.add.read().unwrap().capacity()
            + self.undo.iter().chain(self.redo.iter()).flatten().map(|r| std::mem::size_of::<Rec>() + (r.removed.len() + r.inserted.len()) * std::mem::size_of::<Piece>()).sum::<usize>()
            + self.bases.iter().flatten().map(|b| b.cache.lock().unwrap().iter().map(|(_, a)| <[u8]>::len(a)).sum::<usize>()).sum::<usize>()
    }

    fn check(&self, r: &Range<u64>) -> Result<(), HexError> {
        if r.start > r.end || r.end > self.len {
            return Err(HexError::OutOfRange);
        }
        Ok(())
    }

    /// Replace `range` by `bytes` (insert: empty range; delete: no bytes; overwrite: same length).
    pub fn replace(&mut self, range: Range<u64>, bytes: &[u8]) -> Result<(), HexError> {
        self.check(&range)?;
        if self.layout.pieces.len() + 3 > self.max_pieces || self.add.read().unwrap().len() + bytes.len() > self.max_add {
            return Err(HexError::TooManyEdits);
        }
        let ins = if bytes.is_empty() {
            Vec::new()
        } else {
            let mut a = self.add.write().unwrap();
            let start = a.len() as u64;
            a.extend_from_slice(bytes);
            vec![Piece { src: 0, start, len: bytes.len() as u64 }]
        };
        let total = self.len;
        let removed = Arc::make_mut(&mut self.layout).splice(range.start, range.end, &ins, total);
        self.len = self.len - (range.end - range.start) + bytes.len() as u64;
        self.version += 1;
        let rec = Rec { pos: range.start, removed, inserted: ins };
        self.note_edit(EditDelta { pos: range.start, removed: range.end - range.start, inserted: bytes.len() as u64 });
        match self.open_group.as_mut() {
            Some(g) => g.push(rec),
            None => self.undo.push(vec![rec]),
        }
        self.redo.clear();
        Ok(())
    }
    fn note_edit(&mut self, d: EditDelta) {
        self.last_edit = Some(d);
        if self.track_edits {
            if self.edit_log.len() >= MAX_LOGGED_EDITS {
                self.edit_log.remove(0);
            }
            self.edit_log.push(d);
        }
    }
    /// The last change to the document, also set by undo (the inverse delta) and redo.
    pub fn last_edit(&self) -> Option<EditDelta> {
        self.last_edit
    }
    /// Start or stop logging every delta (for `drain_edits`; off by default).
    pub fn set_track_edits(&mut self, on: bool) {
        self.track_edits = on;
        if !on {
            self.edit_log.clear();
        }
    }
    /// The deltas since the last call, oldest first (regions replay them through `apply_edit`).
    pub fn drain_edits(&mut self) -> Vec<EditDelta> {
        std::mem::take(&mut self.edit_log)
    }
    /// Replace `range` by `n` copies of `pattern` cut to length: one undo step.
    pub fn fill(&mut self, range: Range<u64>, pattern: &[u8]) -> Result<(), HexError> {
        self.check(&range)?;
        let n = range.end - range.start;
        if pattern.is_empty() || n > MAX_FILL_BYTES {
            return Err(HexError::BadFill);
        }
        let bytes: Vec<u8> = pattern.iter().copied().cycle().take(n as usize).collect();
        self.replace(range, &bytes)
    }
    /// Insert `n` bytes of `pattern` (repeated) at `at`: one undo step.
    pub fn insert_fill(&mut self, at: u64, n: u64, pattern: &[u8]) -> Result<(), HexError> {
        self.check(&(at..at))?;
        if pattern.is_empty() || n > MAX_FILL_BYTES {
            return Err(HexError::BadFill);
        }
        let bytes: Vec<u8> = pattern.iter().copied().cycle().take(n as usize).collect();
        self.replace(at..at, &bytes)
    }
    /// Type one hex digit (`value` 0..=15) into the high or low nibble of the byte at `at`. The two
    /// nibbles of one byte typed in a row form a single undo step.
    pub fn set_nibble(&mut self, at: u64, high: bool, value: u8) -> Result<(), HexError> {
        if at >= self.len || value > 15 {
            return Err(HexError::OutOfRange);
        }
        let mut cur = [0u8; 1];
        if self.read_at(at, &mut cur) != 1 {
            return Err(HexError::OutOfRange);
        }
        let new = if high { (value << 4) | (cur[0] & 0x0F) } else { (cur[0] & 0xF0) | value };
        let join = !high && self.open_group.is_none() && self.nibble_pending == Some((at, self.version));
        if join {
            // reopen the previous undo step so both nibbles undo together
            if let Some(g) = self.undo.pop() {
                self.open_group = Some(g);
            }
        }
        let r = self.overwrite(at, &[new]);
        if join {
            self.end_group();
        }
        self.nibble_pending = if r.is_ok() && high { Some((at, self.version)) } else { None };
        r
    }
    pub fn insert(&mut self, at: u64, bytes: &[u8]) -> Result<(), HexError> {
        self.replace(at..at, bytes)
    }
    pub fn delete(&mut self, r: Range<u64>) -> Result<(), HexError> {
        self.replace(r, &[])
    }
    /// Overwrite `bytes.len()` bytes at `at` (the classic hex-editor keystroke).
    pub fn overwrite(&mut self, at: u64, bytes: &[u8]) -> Result<(), HexError> {
        self.replace(at..at + bytes.len() as u64, bytes)
    }

    pub fn begin_group(&mut self) {
        self.open_group.get_or_insert_with(Vec::new);
    }
    pub fn end_group(&mut self) {
        if let Some(g) = self.open_group.take() {
            if !g.is_empty() {
                self.undo.push(g);
            }
        }
    }
    pub fn can_undo(&self) -> bool {
        !self.undo.is_empty()
    }
    pub fn can_redo(&self) -> bool {
        !self.redo.is_empty()
    }
    fn apply(&mut self, rec: &Rec, forward: bool) {
        let (remove, insert) = if forward { (rec.removed_len(), &rec.inserted) } else { (rec.inserted_len(), &rec.removed) };
        let (old, new): (u64, u64) = if forward { (rec.removed_len(), rec.inserted_len()) } else { (rec.inserted_len(), rec.removed_len()) };
        let total = self.len;
        Arc::make_mut(&mut self.layout).splice(rec.pos, rec.pos + remove, insert, total);
        self.len = self.len - old + new;
        self.note_edit(EditDelta { pos: rec.pos, removed: old, inserted: new });
    }
    pub fn undo(&mut self) -> bool {
        self.end_group();
        let Some(g) = self.undo.pop() else { return false };
        for rec in g.iter().rev() {
            self.apply(rec, false);
        }
        self.redo.push(g);
        self.version += 1;
        true
    }
    pub fn redo(&mut self) -> bool {
        let Some(g) = self.redo.pop() else { return false };
        for rec in g.iter() {
            self.apply(rec, true);
        }
        self.undo.push(g);
        self.version += 1;
        true
    }

    /// Streaming atomic save to `path`; the document is rebased on the saved file. The old add
    /// buffer and generations are released unless undo/redo still need them.
    pub fn save(&mut self, path: &Path, ctl: &mut Ctl) -> io::Result<()> {
        let snap = self.snapshot();
        let total = snap.len;
        crate::durable::durable_write_with(path, |f| {
            let mut w = io::BufWriter::with_capacity(1 << 20, &mut *f);
            let mut buf = vec![0u8; 1 << 20];
            let mut pos = 0u64;
            while pos < total {
                let want = ((total - pos) as usize).min(buf.len());
                let n = snap.read_at(pos, &mut buf[..want]);
                if n == 0 {
                    return Err(io::Error::new(io::ErrorKind::UnexpectedEof, "source shrank during save"));
                }
                w.write_all(&buf[..n])?;
                pos += n as u64;
                if !ctl.tick(pos, total) {
                    return Err(io::Error::new(io::ErrorKind::Interrupted, "cancelled"));
                }
            }
            w.flush()
        })?;
        let base = HexBase::open(path)?;
        let mut bases: Vec<Option<Arc<HexBase>>> = (*self.bases).clone();
        bases.push(Some(base.clone()));
        let gen = bases.len() as u16;
        self.bases = Arc::new(bases);
        let mut layout = Layout::default();
        if base.len > 0 {
            layout.pieces.push(Piece { src: gen, start: 0, len: base.len });
        }
        layout.reindex();
        self.layout = Arc::new(layout);
        self.len = base.len;
        self.saved_version = self.version;
        self.gc();
        Ok(())
    }

    /// Release generations and add-buffer bytes nothing refers to any more.
    fn gc(&mut self) {
        let mut used = vec![false; self.bases.len() + 1];
        let mark = |p: &Piece, used: &mut Vec<bool>| used[p.src as usize] = true;
        self.layout.pieces.iter().for_each(|p| mark(p, &mut used));
        for g in self.undo.iter().chain(self.redo.iter()).chain(self.open_group.iter()) {
            for r in g {
                r.removed.iter().chain(r.inserted.iter()).for_each(|p| mark(p, &mut used));
            }
        }
        let mut bases: Vec<Option<Arc<HexBase>>> = (*self.bases).clone();
        for (i, b) in bases.iter_mut().enumerate() {
            if !used[i + 1] {
                *b = None;
            }
        }
        self.bases = Arc::new(bases);
        if !used[0] {
            // no piece or history record refers to the add buffer: start over
            if Arc::strong_count(&self.add) == 1 {
                *self.add.write().unwrap() = Vec::new();
            }
        }
    }
    pub fn clear_history(&mut self) {
        self.undo.clear();
        self.redo.clear();
        self.gc();
    }
}

// ------------------------------------------------------------------------------------------ paging

/// Number of rows needed for `len` bytes.
pub fn row_count(len: u64, bytes_per_row: usize) -> u64 {
    len.div_ceil(bytes_per_row as u64)
}

/// The bytes of `rows` rows starting at `first_row` (shorter at the end of the source). One read of
/// at most `rows * bytes_per_row` bytes: what a viewport needs, whatever the file size.
pub fn read_page(src: &dyn ByteSource, first_row: u64, rows: usize, bytes_per_row: usize) -> Vec<u8> {
    let off = first_row.saturating_mul(bytes_per_row as u64);
    let want = ((src.len().saturating_sub(off)) as usize).min(rows * bytes_per_row);
    let mut v = vec![0u8; want];
    let n = src.read_at(off, &mut v);
    v.truncate(n);
    v
}

// ------------------------------------------------------------------------------------------ search

/// What to look for in hex mode.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BytePattern {
    bytes: Vec<u8>,
    /// 0xFF = must match, 0x00 = wildcard; empty = no wildcard.
    mask: Vec<u8>,
}

#[derive(Debug, PartialEq, Eq)]
pub enum PatternError {
    Empty,
    /// A token that is not a hex byte or `??`.
    BadToken(String),
}

impl BytePattern {
    pub fn exact(bytes: &[u8]) -> Result<Self, PatternError> {
        if bytes.is_empty() {
            return Err(PatternError::Empty);
        }
        Ok(Self { bytes: bytes.to_vec(), mask: Vec::new() })
    }
    /// `"DE AD ?? EF"`, `"deadbeef"`, `"0xDE 0xAD"`: hex bytes separated by spaces or not; `??`
    /// (or `?`) is a one-byte wildcard.
    pub fn parse_hex(s: &str) -> Result<Self, PatternError> {
        let mut bytes = Vec::new();
        let mut mask = Vec::new();
        let mut any_wild = false;
        for tok in s.split(|c: char| c.is_whitespace() || c == ',') .filter(|t| !t.is_empty()) {
            let tok = tok.strip_prefix("0x").or_else(|| tok.strip_prefix("0X")).unwrap_or(tok);
            if tok == "?" || tok == "??" {
                bytes.push(0);
                mask.push(0);
                any_wild = true;
                continue;
            }
            if tok.len() % 2 != 0 || !tok.bytes().all(|b| b.is_ascii_hexdigit()) {
                return Err(PatternError::BadToken(tok.to_string()));
            }
            for pair in tok.as_bytes().chunks(2) {
                bytes.push(u8::from_str_radix(std::str::from_utf8(pair).unwrap(), 16).unwrap());
                mask.push(0xFF);
            }
        }
        if bytes.is_empty() {
            return Err(PatternError::Empty);
        }
        Ok(Self { bytes, mask: if any_wild { mask } else { Vec::new() } })
    }
    /// A text, encoded in `codepage_id` (`utf-8`, `utf-16le`, `windows-1252`, ...), no BOM.
    pub fn from_text(text: &str, codepage_id: &str) -> Result<Self, PatternError> {
        let cp = brouillon_core::codepages::supported(codepage_id).map_err(|e| PatternError::BadToken(e.to_string()))?;
        let enc = brouillon_core::codepages::encode(cp, text).map_err(|e| PatternError::BadToken(e.to_string()))?;
        Self::exact(&enc.bytes)
    }
    pub fn len(&self) -> usize {
        self.bytes.len()
    }
    pub fn is_empty(&self) -> bool {
        self.bytes.is_empty()
    }
    fn has_wild(&self) -> bool {
        !self.mask.is_empty()
    }
    fn matches_at(&self, hay: &[u8], i: usize) -> bool {
        let n = self.bytes.len();
        if i + n > hay.len() {
            return false;
        }
        (0..n).all(|k| self.mask[k] == 0 || hay[i + k] == self.bytes[k])
    }
}

/// Compiled searcher for one pattern.
struct Finder<'p> {
    pat: &'p BytePattern,
    exact: Option<memchr::memmem::Finder<'p>>,
    /// first non-wildcard byte index (masked patterns)
    anchor: usize,
}
impl<'p> Finder<'p> {
    fn new(pat: &'p BytePattern) -> Self {
        let exact = (!pat.has_wild()).then(|| memchr::memmem::Finder::new(&pat.bytes));
        let anchor = pat.mask.iter().position(|&m| m != 0).unwrap_or(0);
        Self { pat, exact, anchor }
    }
    fn find(&self, hay: &[u8], from: usize) -> Option<usize> {
        if let Some(f) = &self.exact {
            return f.find(&hay[from.min(hay.len())..]).map(|k| from + k);
        }
        let n = self.pat.len();
        if hay.len() < n {
            return None;
        }
        let last_start = hay.len() - n;
        if self.pat.mask.iter().all(|&m| m == 0) {
            return (from <= last_start).then_some(from);
        }
        let a = self.anchor;
        let ab = self.pat.bytes[a];
        let lo = from + a;
        let hi = last_start + a + 1;
        if lo >= hi {
            return None;
        }
        memchr::memchr_iter(ab, &hay[lo..hi]).map(|k| k + from).find(|&s| self.pat.matches_at(hay, s))
    }
    fn rfind(&self, hay: &[u8]) -> Option<usize> {
        if self.exact.is_some() {
            return memchr::memmem::rfind(hay, &self.pat.bytes);
        }
        let n = self.pat.len();
        if hay.len() < n {
            return None;
        }
        (0..=hay.len() - n).rev().find(|&s| self.pat.matches_at(hay, s))
    }
}

/// Window size of the byte search (tests shrink it).
#[derive(Clone, Copy, Debug)]
pub struct ByteSearchCfg {
    pub window: usize,
}
impl Default for ByteSearchCfg {
    fn default() -> Self {
        Self { window: 1 << 20 }
    }
}

/// Non-overlapping matches of `pat` in `range`, in order; `on_hit(start)` returns false to stop.
pub fn scan_bytes(
    src: &dyn ByteSource,
    range: Range<u64>,
    pat: &BytePattern,
    cfg: ByteSearchCfg,
    ctl: &mut Ctl,
    on_hit: &mut dyn FnMut(u64) -> bool,
) -> Result<bool, Cancelled> {
    let n = pat.len();
    let end = range.end.min(src.len());
    if n == 0 || range.start >= end {
        return Ok(true);
    }
    let finder = Finder::new(pat);
    let window = cfg.window.max(n);
    let total = end - range.start;
    let mut buf: Vec<u8> = Vec::with_capacity(window + n);
    let mut base = range.start; // absolute offset of buf[0]
    let mut pos = range.start; // next unread
    let mut next_ok = range.start; // matches must start at or after this
    while pos < end {
        if ctl.cancel.is_cancelled() {
            return Err(Cancelled);
        }
        let old = buf.len();
        let want = ((end - pos) as usize).min(window);
        buf.resize(old + want, 0);
        let got = src.read_at(pos, &mut buf[old..old + want]);
        buf.truncate(old + got);
        if got == 0 {
            break;
        }
        pos += got as u64;
        let mut i = (next_ok.saturating_sub(base)) as usize;
        while let Some(k) = finder.find(&buf, i) {
            let abs = base + k as u64;
            if !on_hit(abs) {
                return Ok(false);
            }
            next_ok = abs + n as u64;
            i = k + n;
        }
        // keep the last n-1 bytes: a match may straddle into the next window
        let keep = (n - 1).min(buf.len());
        let drop = buf.len() - keep;
        buf.drain(..drop);
        base += drop as u64;
        if !ctl.tick(pos - range.start, total) {
            return Err(Cancelled);
        }
    }
    Ok(true)
}

pub fn count_bytes(src: &dyn ByteSource, pat: &BytePattern, cfg: ByteSearchCfg, ctl: &mut Ctl) -> Result<u64, Cancelled> {
    let mut n = 0;
    scan_bytes(src, 0..src.len(), pat, cfg, ctl, &mut |_| {
        n += 1;
        true
    })?;
    Ok(n)
}

/// Offsets of the first `max_results` matches and whether there are more.
pub fn find_bytes_all(src: &dyn ByteSource, pat: &BytePattern, max_results: usize, cfg: ByteSearchCfg, ctl: &mut Ctl) -> Result<(Vec<u64>, bool), Cancelled> {
    let mut out = Vec::new();
    let mut trunc = false;
    scan_bytes(src, 0..src.len(), pat, cfg, ctl, &mut |p| {
        if out.len() >= max_results {
            trunc = true;
            return false;
        }
        out.push(p);
        true
    })?;
    Ok((out, trunc))
}

/// First match starting at or after `from`; with `wrap`, then from 0. Returns `(start, wrapped)`.
pub fn find_bytes_next(src: &dyn ByteSource, pat: &BytePattern, from: u64, wrap: bool, cfg: ByteSearchCfg, ctl: &mut Ctl) -> Result<Option<(u64, bool)>, Cancelled> {
    let mut hit = None;
    let len = src.len();
    scan_bytes(src, from.min(len)..len, pat, cfg, ctl, &mut |p| {
        hit = Some(p);
        false
    })?;
    if let Some(h) = hit {
        return Ok(Some((h, false)));
    }
    if wrap {
        scan_bytes(src, 0..len, pat, cfg, ctl, &mut |p| {
            hit = Some(p);
            false
        })?;
        return Ok(hit.map(|h| (h, true)));
    }
    Ok(None)
}

fn last_before(src: &dyn ByteSource, pat: &BytePattern, limit: u64, cfg: ByteSearchCfg, ctl: &mut Ctl) -> Result<Option<u64>, Cancelled> {
    let n = pat.len() as u64;
    let finder = Finder::new(pat);
    let window = cfg.window.max(pat.len()) as u64;
    let mut end = limit.min(src.len());
    let limit = end;
    let mut buf: Vec<u8> = Vec::new();
    while end > 0 {
        if ctl.cancel.is_cancelled() {
            return Err(Cancelled);
        }
        let s = end.saturating_sub(window);
        // extend by n-1 so a match starting inside [s, end) and ending just after `end` is seen
        let e2 = (end + n - 1).min(limit);
        buf.resize((e2 - s) as usize, 0);
        let got = src.read_at(s, &mut buf);
        buf.truncate(got);
        if let Some(k) = finder.rfind(&buf) {
            return Ok(Some(s + k as u64));
        }
        if s == 0 {
            break;
        }
        end = s;
        if !ctl.tick(limit - end, limit) {
            return Err(Cancelled);
        }
    }
    Ok(None)
}

/// Last match lying entirely before `before`; with `wrap`, then the last match of the document.
pub fn find_bytes_prev(src: &dyn ByteSource, pat: &BytePattern, before: u64, wrap: bool, cfg: ByteSearchCfg, ctl: &mut Ctl) -> Result<Option<(u64, bool)>, Cancelled> {
    if let Some(p) = last_before(src, pat, before, cfg, ctl)? {
        return Ok(Some((p, false)));
    }
    if wrap {
        return Ok(last_before(src, pat, src.len(), cfg, ctl)?.map(|p| (p, true)));
    }
    Ok(None)
}

// ------------------------------------------------------------------------------------------ tests

#[cfg(test)]
mod tests {
    use super::*;
    use crate::search::CancelToken;
    use rand::{Rng, SeedableRng};
    use rand_chacha::ChaCha8Rng;

    fn rand_bytes(rng: &mut ChaCha8Rng, n: usize, alphabet: u8) -> Vec<u8> {
        (0..n).map(|_| rng.gen_range(0..alphabet)).collect()
    }

    fn write(dir: &Path, name: &str, bytes: &[u8]) -> PathBuf {
        let p = dir.join(name);
        std::fs::write(&p, bytes).unwrap();
        p
    }

    fn all(doc: &dyn ByteSource) -> Vec<u8> {
        let mut v = vec![0u8; doc.len() as usize];
        let n = doc.read_at(0, &mut v);
        v.truncate(n);
        v
    }

    #[test]
    fn paging_reads_exactly_the_page() {
        let dir = tempfile::tempdir().unwrap();
        let bytes: Vec<u8> = (0..=255u8).cycle().take(100_003).collect();
        let p = write(dir.path(), "b.bin", &bytes);
        let doc = HexDoc::open(&p).unwrap();
        assert_eq!(row_count(doc.len(), 16), 6251);
        let page = read_page(&doc, 10, 4, 16);
        assert_eq!(page, bytes[160..224]);
        let last = read_page(&doc, 6250, 5, 16);
        assert_eq!(last, bytes[100_000..]);
        assert!(read_page(&doc, 7000, 5, 16).is_empty());
        // pages straddling the 64 KiB cache chunk edges
        for first in [4095u64, 4096, 4097] {
            assert_eq!(read_page(&doc, first, 3, 16), bytes[first as usize * 16..first as usize * 16 + 48]);
        }
    }

    #[test]
    fn edits_match_a_vec_model_with_undo_redo_groups_and_save() {
        let dir = tempfile::tempdir().unwrap();
        let mut rng = ChaCha8Rng::seed_from_u64(5);
        let original = rand_bytes(&mut rng, 3000, 255);
        let p = write(dir.path(), "m.bin", &original);
        let mut doc = HexDoc::open(&p).unwrap();
        let mut model = original.clone();
        let mut history: Vec<Vec<u8>> = Vec::new();
        let cancel = CancelToken::new();
        for step in 0..600 {
            history.push(model.clone());
            let len = model.len() as u64;
            let a = rng.gen_range(0..=len);
            let b = (a + rng.gen_range(0..20)).min(len);
            let il = rng.gen_range(0..12);
            let ins = rand_bytes(&mut rng, il, 255);
            match rng.gen_range(0..4) {
                0 => {
                    doc.insert(a, &ins).unwrap();
                    model.splice(a as usize..a as usize, ins.iter().copied());
                }
                1 => {
                    doc.delete(a..b).unwrap();
                    model.drain(a as usize..b as usize);
                }
                2 => {
                    let n = ins.len().min((len - a) as usize);
                    doc.overwrite(a, &ins[..n]).unwrap();
                    model.splice(a as usize..a as usize + n, ins[..n].iter().copied());
                }
                _ => {
                    doc.replace(a..b, &ins).unwrap();
                    model.splice(a as usize..b as usize, ins.iter().copied());
                }
            }
            assert_eq!(all(&doc), model, "step {step}");
            assert_eq!(ByteSource::len(&doc), model.len() as u64);
            if step % 150 == 149 {
                doc.save(&p, &mut Ctl::new(&cancel)).unwrap();
                assert_eq!(std::fs::read(&p).unwrap(), model);
                assert!(!doc.is_dirty());
            }
        }
        // undo everything step by step: each state equals the recorded one (also across the saves)
        for want in history.iter().rev() {
            assert!(doc.undo());
            assert_eq!(&all(&doc), want);
        }
        assert!(!doc.undo());
        assert_eq!(all(&doc), original);
        for _ in 0..history.len() {
            assert!(doc.redo());
        }
        assert_eq!(all(&doc), model);
        // groups: one undo step
        doc.begin_group();
        doc.insert(0, b"AB").unwrap();
        doc.overwrite(5, b"ZZ").unwrap();
        doc.end_group();
        let before = model.clone();
        assert!(doc.undo());
        assert_eq!(all(&doc), before);
    }

    #[test]
    fn snapshot_is_immutable_while_the_document_changes() {
        let dir = tempfile::tempdir().unwrap();
        let p = write(dir.path(), "s.bin", b"0123456789");
        let mut doc = HexDoc::open(&p).unwrap();
        doc.insert(3, b"xx").unwrap();
        let snap = doc.snapshot();
        doc.delete(0..5).unwrap();
        doc.overwrite(0, b"QQ").unwrap();
        assert_eq!(all(&snap), b"012xx3456789");
        assert_eq!(all(&doc), b"QQ56789".to_vec());
        fn is_send_sync<T: Send + Sync>() {}
        is_send_sync::<HexSnapshot>();
    }

    #[test]
    fn caps_report_too_many_edits() {
        let dir = tempfile::tempdir().unwrap();
        let p = write(dir.path(), "c.bin", &vec![0u8; 1000]);
        let mut doc = HexDoc::open(&p).unwrap();
        // a piece-heavy pattern: single-byte inserts at scattered positions
        doc.set_limits(100, 1 << 20);
        let mut err = None;
        for i in 0..1000u64 {
            if let Err(e) = doc.insert((i * 37) % (doc.len() + 1), &[1]) {
                err = Some(e);
                break;
            }
        }
        assert!(matches!(err, Some(HexError::TooManyEdits)));
        // saving collapses the table and edits work again
        doc.save(&p, &mut Ctl::new(&CancelToken::new())).unwrap();
        assert_eq!(doc.piece_count(), 1);
        doc.insert(0, &[9]).unwrap();
    }

    fn naive_all(hay: &[u8], pat: &BytePattern) -> Vec<u64> {
        let mut out = Vec::new();
        let mut i = 0;
        while i + pat.len() <= hay.len() {
            let ok = (0..pat.len()).all(|k| pat.mask.is_empty() || pat.mask[k] == 0 || hay[i + k] == pat.bytes[k]) && (!pat.mask.is_empty() || hay[i..i + pat.len()] == pat.bytes[..]);
            if ok {
                out.push(i as u64);
                i += pat.len();
            } else {
                i += 1;
            }
        }
        out
    }

    #[test]
    fn hex_search_equals_a_naive_scan_at_tiny_windows() {
        let cancel = CancelToken::new();
        let mut rng = ChaCha8Rng::seed_from_u64(9);
        for round in 0..300 {
            let hl = rng.gen_range(0..200);
            let hay = rand_bytes(&mut rng, hl, 3);
            let n = rng.gen_range(1..5);
            let mut pat_bytes = rand_bytes(&mut rng, n, 3);
            let wild = round % 3 == 0;
            let pat = if wild {
                let s: Vec<String> = pat_bytes.iter().map(|b| if rng.gen_bool(0.3) { "??".to_string() } else { format!("{b:02X}") }).collect();
                BytePattern::parse_hex(&s.join(" ")).unwrap()
            } else {
                pat_bytes.truncate(n);
                BytePattern::exact(&pat_bytes).unwrap()
            };
            let want = naive_all(&hay, &pat);
            for window in [1usize, 2, 3, 5, 8, 64, 1 << 20] {
                let cfg = ByteSearchCfg { window };
                let ctl = &mut Ctl::new(&cancel);
                let (got, trunc) = find_bytes_all(&hay, &pat, usize::MAX, cfg, ctl).unwrap();
                assert!(!trunc);
                assert_eq!(got, want, "all window {window} pat {:?} hay {hay:?}", pat);
                assert_eq!(count_bytes(&hay, &pat, cfg, ctl).unwrap(), want.len() as u64);
                let from = rng.gen_range(0..=hay.len() as u64);
                let exp_next = want.iter().copied().find(|&p| p >= from);
                let nx = find_bytes_next(&hay, &pat, from, false, cfg, ctl).unwrap();
                // non-overlap makes `next` from the middle of a run depend on the start: compare with a naive from-scan
                let naive_from = naive_all(&hay[from as usize..], &pat).first().map(|p| p + from);
                assert_eq!(nx.map(|x| x.0), naive_from, "next from {from} window {window}");
                let _ = exp_next;
                let before = rng.gen_range(0..=hay.len() as u64);
                let exp_prev = (0..hay.len()).rev().find(|&s| s as u64 + pat.len() as u64 <= before && pat.matches_naive(&hay, s)).map(|s| s as u64);
                let pv = find_bytes_prev(&hay, &pat, before, false, cfg, ctl).unwrap();
                assert_eq!(pv.map(|x| x.0), exp_prev, "prev before {before} window {window} pat {pat:?} hay {hay:?}");
                let wrapped = find_bytes_next(&hay, &pat, hay.len() as u64, true, cfg, ctl).unwrap();
                assert_eq!(wrapped.map(|x| x.0), want.first().copied());
            }
        }
    }

    impl BytePattern {
        fn matches_naive(&self, hay: &[u8], i: usize) -> bool {
            i + self.len() <= hay.len() && (0..self.len()).all(|k| (!self.mask.is_empty() && self.mask[k] == 0) || hay[i + k] == self.bytes[k])
        }
    }

    #[test]
    fn hex_search_works_through_edits_and_text_patterns() {
        let dir = tempfile::tempdir().unwrap();
        let p = write(dir.path(), "t.bin", b"....hello....hello....");
        let mut doc = HexDoc::open(&p).unwrap();
        doc.insert(2, b"hel").unwrap();
        doc.overwrite(11, b"lo").unwrap();
        let cancel = CancelToken::new();
        let pat = BytePattern::from_text("hello", "utf-8").unwrap();
        let (got, _) = find_bytes_all(&doc, &pat, 10, ByteSearchCfg { window: 4 }, &mut Ctl::new(&cancel)).unwrap();
        let text = String::from_utf8(all(&doc)).unwrap();
        let want: Vec<u64> = text.match_indices("hello").map(|(i, _)| i as u64).collect();
        assert_eq!(got, want);
        let u16pat = BytePattern::from_text("hi", "utf-16le").unwrap();
        assert_eq!(u16pat.len(), 4);
        assert_eq!(BytePattern::parse_hex("zz"), Err(PatternError::BadToken("zz".into())));
        assert_eq!(BytePattern::parse_hex("  "), Err(PatternError::Empty));
        assert_eq!(BytePattern::parse_hex("0xDE ad").unwrap().bytes, vec![0xDE, 0xAD]);
    }

    #[test]
    fn last_edit_and_drained_deltas_cover_edits_undo_and_redo() {
        let dir = tempfile::tempdir().unwrap();
        let p = write(dir.path(), "d.bin", &[0u8; 64]);
        let mut doc = HexDoc::open(&p).unwrap();
        assert_eq!(doc.last_edit(), None);
        doc.set_track_edits(true);
        doc.insert(4, &[1, 2, 3]).unwrap();
        assert_eq!(doc.last_edit(), Some(EditDelta { pos: 4, removed: 0, inserted: 3 }));
        doc.delete(10..12).unwrap();
        assert!(doc.undo());
        assert_eq!(doc.last_edit(), Some(EditDelta { pos: 10, removed: 0, inserted: 2 }), "inverse delta");
        assert!(doc.redo());
        assert_eq!(doc.last_edit(), Some(EditDelta { pos: 10, removed: 2, inserted: 0 }));
        let log = doc.drain_edits();
        assert_eq!(log.len(), 4);
        assert!(doc.drain_edits().is_empty());
        doc.set_track_edits(false);
        doc.insert(0, &[9]).unwrap();
        assert!(doc.drain_edits().is_empty());
    }

    #[test]
    fn regions_follow_hexdoc_edits_through_undo_redo() {
        use crate::hex_regions::{RegionColor, RegionSet};
        let dir = tempfile::tempdir().unwrap();
        let p = write(dir.path(), "r.bin", &[0u8; 100]);
        let mut doc = HexDoc::open(&p).unwrap();
        doc.set_track_edits(true);
        let mut set = RegionSet::new();
        let id = set.add(50..60, "r", RegionColor::Blue, 100).unwrap();
        let sync = |doc: &mut HexDoc, set: &mut RegionSet| doc.drain_edits().into_iter().for_each(|d| set.apply_edit(d));
        doc.insert(0, &[1, 2, 3, 4]).unwrap();
        sync(&mut doc, &mut set);
        assert_eq!(set.get(id).unwrap().start, 54);
        doc.undo();
        sync(&mut doc, &mut set);
        assert_eq!(set.get(id).unwrap().start, 50);
        doc.redo();
        sync(&mut doc, &mut set);
        assert_eq!(set.get(id).unwrap().start, 54);
        doc.delete(40..80).unwrap();
        sync(&mut doc, &mut set);
        assert!(set.get(id).unwrap().orphaned);
    }

    #[test]
    fn fill_and_insert_fill_are_single_undo_steps() {
        let dir = tempfile::tempdir().unwrap();
        let bytes: Vec<u8> = (0..32u8).collect();
        let p = write(dir.path(), "f.bin", &bytes);
        let mut doc = HexDoc::open(&p).unwrap();
        doc.fill(4..11, &[0xAB, 0xCD, 0xEF]).unwrap();
        let after = all(&doc);
        assert_eq!(&after[4..11], &[0xAB, 0xCD, 0xEF, 0xAB, 0xCD, 0xEF, 0xAB]);
        assert_eq!(after.len(), 32);
        doc.insert_fill(2, 5, &[0]).unwrap();
        assert_eq!(doc.len(), 37);
        assert_eq!(&all(&doc)[..8], &[0, 1, 0, 0, 0, 0, 0, 2]);
        assert!(doc.undo());
        assert_eq!(all(&doc), after);
        assert!(doc.undo());
        assert_eq!(all(&doc), bytes);
        assert!(matches!(doc.fill(0..4, &[]), Err(HexError::BadFill)));
        assert!(matches!(doc.fill(30..40, &[1]), Err(HexError::OutOfRange)));
        assert!(matches!(doc.insert_fill(99, 1, &[1]), Err(HexError::OutOfRange)));
        assert!(matches!(doc.insert_fill(0, MAX_FILL_BYTES + 1, &[1]), Err(HexError::BadFill)));
    }

    #[test]
    fn nibble_edit_two_digits_undo_as_one_step() {
        let dir = tempfile::tempdir().unwrap();
        let p = write(dir.path(), "n.bin", &[0x12, 0x34, 0x56]);
        let mut doc = HexDoc::open(&p).unwrap();
        doc.set_nibble(1, true, 0xA).unwrap();
        assert_eq!(all(&doc), vec![0x12, 0xA4, 0x56]);
        doc.set_nibble(1, false, 0xB).unwrap();
        assert_eq!(all(&doc), vec![0x12, 0xAB, 0x56]);
        assert!(doc.undo());
        assert_eq!(all(&doc), vec![0x12, 0x34, 0x56], "one undo reverts both digits");
        assert!(!doc.undo());
        assert!(doc.redo());
        assert_eq!(all(&doc), vec![0x12, 0xAB, 0x56]);
        // a low nibble alone, or after an unrelated edit, is its own step
        doc.set_nibble(2, false, 0xF).unwrap();
        doc.set_nibble(0, true, 0xE).unwrap();
        doc.set_nibble(0, false, 0x0).unwrap();
        assert_eq!(all(&doc), vec![0xE0, 0xAB, 0x5F]);
        assert!(doc.undo());
        assert_eq!(all(&doc), vec![0x12, 0xAB, 0x5F]);
        assert!(doc.undo());
        assert_eq!(all(&doc), vec![0x12, 0xAB, 0x56]);
        // out of range and bad digit
        assert!(matches!(doc.set_nibble(3, true, 1), Err(HexError::OutOfRange)));
        assert!(matches!(doc.set_nibble(0, true, 16), Err(HexError::OutOfRange)));
    }

    #[test]
    fn nibble_low_after_an_intervening_edit_does_not_merge() {
        let dir = tempfile::tempdir().unwrap();
        let p = write(dir.path(), "m.bin", &[0u8; 4]);
        let mut doc = HexDoc::open(&p).unwrap();
        doc.set_nibble(0, true, 1).unwrap();
        doc.set_nibble(3, true, 2).unwrap();
        doc.set_nibble(0, false, 3).unwrap();
        assert_eq!(all(&doc), vec![0x13, 0, 0, 0x20]);
        assert!(doc.undo());
        assert_eq!(all(&doc), vec![0x10, 0, 0, 0x20]);
        assert!(doc.undo());
        assert_eq!(all(&doc), vec![0x10, 0, 0, 0]);
    }
}
