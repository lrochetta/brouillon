//! The editor's text buffer: one API, two backings.
//!
//! * `Backing::Rope` (default, files below the lazy threshold): a `crop::Rope`, structurally shared, exactly the
//!   behaviour the editor always had.
//! * `Backing::Lazy` (files at or above `[files] lazy_threshold_mb`, default 16 MB): a read-only base file plus a
//!   piece table (`brouillon_textstore_spike::lazy::LazyStore`). RAM no longer depends on the file size: only the
//!   edits, the undo records (piece operations, never text), a 2 MB chunk cache and a sparse line index are held.
//!
//! The public surface is the same for both. The rope's conventions are kept (a line is "terminated": text ending in
//! a line feed has no extra empty last line, the empty text has 0 lines; `nth_line` has no terminator). Code that
//! used `rope()` to reach the whole text migrates to the adapters below (`slice`, `visit_str`, `byte_of_line`,
//! `line_of_byte`, `snapshot`, ...); on a lazy buffer `rope()` returns an EMPTY rope and counts the misuse in
//! [`lazy_rope_misuse`] (never the real text: that would be a hidden full copy).

use std::fs;
use std::io;
use std::ops::Range;
use std::path::Path;
use std::str::FromStr;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::OnceLock;

use crop::Rope;

use brouillon_core::contract::{EditError, EditSet, Region};
use brouillon_textstore_spike::lazy::{Config as LazyConfig, EditRec, LazyStore, LineLookup, StoreSnapshot, Utf8};
use brouillon_textstore_spike::store::TextStore;

use crate::edit_apply::{map_splice, Bias};
use crate::undo::{Origin, TypingKind, UndoEntry, UndoGroup, UndoHistory};

/// How many times code asked a lazy buffer for its `rope()` (diagnostics for the migration; should stay 0).
static ROPE_MISUSE: AtomicUsize = AtomicUsize::new(0);

pub fn lazy_rope_misuse() -> usize {
    ROPE_MISUSE.load(Ordering::Relaxed)
}

fn empty_rope() -> &'static Rope {
    static EMPTY: OnceLock<Rope> = OnceLock::new();
    EMPTY.get_or_init(|| Rope::from_str("").expect("infallible"))
}

/// The text container behind a [`Buffer`].
#[derive(Debug, Clone)]
enum Backing {
    Rope(Rope),
    Lazy(LazyStore),
}

/// Add-buffer size from which `Buffer` considers compacting it (4 MB).
const COMPACT_MIN: usize = 4 << 20;
/// A lazy document at least this big drops its undo history at save instead of keeping the old base file alive
/// (the replaced file would stay on disk, unnamed, until the history lets go of it).
const KEEP_HISTORY_ACROSS_SAVE_MAX: usize = 64 << 20;

/// A loaded text buffer. Cheap to clone (the rope is structurally shared; the lazy store shares its add buffer,
/// base file and piece blocks copy-on-write) — viewport rendering takes a `&Buffer` and iterates line slices.
///
/// `dirty` tracks whether the buffer has been mutated since the last save
/// (or since load). `Clone` propagates the flag verbatim: a cloned dirty
/// buffer is still dirty (consistent with "Clone reproduces state"). Callers
/// that want a "fresh" copy should call `mark_clean()` post-clone.
///
/// `history` records reverse-ops for multi-level undo / redo. It is also
/// cloned (a duplicated buffer keeps its history), but consumers wanting
/// a clean slate can call `clear_history()`.
///
/// Lot 1 API (stable): `apply_edits` / `transaction` (one undo group),
/// `typing_insert` / `typing_delete` (coalesced typing), `mark_saved`,
/// `undo_to_save` / `undo_all`, anchors, `version()` / `hash()`.
#[derive(Debug, Clone)]
pub struct Buffer {
    backing: Backing,
    dirty: bool,
    history: UndoHistory,
    crdt: Option<crate::crdt::CrdtSession>,
    /// Bumped on EVERY text change (edit, undo, redo); never reset.
    version: u64,
    anchors: Vec<Anchor>,
    next_anchor: u64,
    open: Option<OpenGroup>,
    /// Lazy only: add-buffer length from which the next compaction check is worth doing.
    compact_mark: usize,
}

/// Handle to a position that follows the text through edits.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct AnchorId(u64);

#[derive(Debug, Clone)]
struct Anchor {
    id: AnchorId,
    pos: usize,
    bias: Bias,
}

/// Metadata of one undo group (see `apply_edits_with` / `transaction_with`).
#[derive(Debug, Clone, Default)]
pub struct EditMeta {
    pub origin: Origin,
    /// Restored by undo.
    pub selection_before: Option<Vec<Region>>,
    /// Restored by redo.
    pub selection_after: Option<Vec<Region>>,
}

#[derive(Debug, Clone)]
struct OpenGroup {
    meta: EditMeta,
    entries: Vec<UndoEntry>,
    depth: usize,
}

/// What a splice removed: the text (rope) or the piece record (lazy; text is never copied).
struct Spliced {
    removed: String,
    rec: Option<Box<EditRec>>,
}

impl Spliced {
    /// The reverse-op entry: the piece record for a lazy buffer, else `f(removed text)`.
    fn entry(self, f: impl FnOnce(String) -> UndoEntry) -> UndoEntry {
        match self.rec {
            Some(r) => UndoEntry::Pieces(r),
            None => f(self.removed),
        }
    }
}

/// Immutable, cheaply clonable, `Send + Sync` view of a buffer's text for worker threads (Find, session
/// writer, save). Taken with [`Buffer::snapshot`]; reading it takes no lock on the live buffer.
#[derive(Debug, Clone)]
pub enum BufferSnapshot {
    Rope(Rope),
    Lazy(StoreSnapshot),
}

/// State of the background line index / UTF-8 validation of a lazy buffer.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LazyStatus {
    /// Bytes of the base file scanned so far / base file length.
    pub scanned: u64,
    pub total: u64,
    /// `scanned / total` (1.0 when done).
    pub fraction: f64,
    pub index_done: bool,
    /// Line count (exact when `lines_exact`, else an estimate from the newline density seen so far).
    pub lines: usize,
    pub lines_exact: bool,
    pub utf8: Utf8,
}

fn lazy_ends_with_nl(s: &LazyStore) -> bool {
    let len = s.byte_len();
    if len == 0 {
        return false;
    }
    let mut nl = false;
    s.visit(len - 1..len, &mut |c| {
        nl = c.first() == Some(&b'\n');
        false
    });
    nl
}

/// Rope convention: terminated lines (see the module doc).
fn lazy_line_len(s: &LazyStore) -> usize {
    if s.byte_len() == 0 {
        return 0;
    }
    let n = s.line_count();
    if lazy_ends_with_nl(s) {
        n - 1
    } else {
        n
    }
}

/// Feed `f` the UTF-8 pieces of raw byte chunks, carrying a character cut by a chunk edge into the next chunk.
fn visit_as_str(s: &dyn TextStore, r: Range<usize>, f: &mut dyn FnMut(&str) -> bool) {
    let mut carry: Vec<u8> = Vec::new();
    let mut stopped = false;
    s.visit(r, &mut |chunk| {
        let joined;
        let data: &[u8] = if carry.is_empty() {
            chunk
        } else {
            carry.extend_from_slice(chunk);
            joined = std::mem::take(&mut carry);
            &joined
        };
        let keep_going = match std::str::from_utf8(data) {
            Ok(t) => f(t),
            Err(e) if e.error_len().is_none() => {
                let (ok, rest) = data.split_at(e.valid_up_to());
                carry = rest.to_vec();
                ok.is_empty() || f(std::str::from_utf8(ok).expect("valid prefix"))
            }
            Err(_) => f(&String::from_utf8_lossy(data)),
        };
        stopped = !keep_going;
        keep_going
    });
    if !stopped && !carry.is_empty() {
        f(&String::from_utf8_lossy(&carry));
    }
}

fn strip_terminator(mut t: String) -> String {
    if t.ends_with('\n') {
        t.pop();
        if t.ends_with('\r') {
            t.pop();
        }
    }
    t
}

impl Buffer {
    fn fresh(backing: Backing) -> Self {
        Self { backing, dirty: false, history: UndoHistory::new(), crdt: None, version: 0, anchors: Vec::new(), next_anchor: 0, open: None, compact_mark: COMPACT_MIN }
    }

    /// Build a buffer from an owned `String`. `crop::Rope: FromStr` is
    /// infallible, but we keep the `?` propagation pattern in case we
    /// later validate UTF-8 or normalize line endings.
    pub fn from_str(src: &str) -> Self {
        // `Rope::from_str` returns `Result<Self, Infallible>` in crop 0.4.
        // Unwrap is safe; map_err would only be needed if the error type changed.
        let rope = Rope::from_str(src).expect("crop::Rope::from_str is infallible in 0.4");
        Self::fresh(Backing::Rope(rope))
    }

    /// Build a buffer from a rope that was assembled without a full-text `String` (huge files).
    pub fn from_rope(rope: Rope) -> Self {
        Self::fresh(Backing::Rope(rope))
    }

    /// Build a buffer on a lazy store (huge files: RAM independent of the file size).
    pub fn from_lazy(store: LazyStore) -> Self {
        Self::fresh(Backing::Lazy(store))
    }

    /// Open `path` lazily (O(1): nothing is read; the line index and the UTF-8 check run in the background).
    /// `Err(Unsupported)` for UTF-16/32: the caller uses the decode path.
    pub fn open_lazy(path: &Path) -> io::Result<Self> {
        Ok(Self::from_lazy(LazyStore::open(path, LazyConfig::default())?))
    }

    /// Read the file at `path` into a buffer. Reads the whole file at once —
    /// day-1 acceptable (sample-100k.txt is ~5MB); day-2+ should stream.
    pub fn from_path<P: AsRef<Path>>(path: P) -> io::Result<Self> {
        let text = fs::read_to_string(path)?;
        Ok(Self::from_str(&text))
    }

    // ---------- backing ----------

    /// The lazy store behind a "large file mode" buffer (`None` for a rope): persistence code (piece journal,
    /// external-change checks) works on it directly.
    pub fn lazy_store(&self) -> Option<&LazyStore> {
        match &self.backing {
            Backing::Lazy(s) => Some(s),
            Backing::Rope(_) => None,
        }
    }

    /// True when the text lives in a lazy store (a "large file mode" document).
    pub fn is_lazy(&self) -> bool {
        matches!(self.backing, Backing::Lazy(_))
    }

    /// True when edits are refused (lazy document whose file turned out not to be valid UTF-8). Such a document
    /// is for reading, the hex view, or reopening through a codepage; every mutation is a no-op.
    pub fn is_read_only(&self) -> bool {
        match &self.backing {
            Backing::Rope(_) => false,
            Backing::Lazy(s) => s.is_read_only(),
        }
    }

    /// Background-index state of a lazy buffer (`None` for a rope).
    pub fn lazy_status(&self) -> Option<LazyStatus> {
        let Backing::Lazy(s) = &self.backing else { return None };
        let (scanned, total) = s.index_progress();
        let (n, exact) = s.estimated_line_count();
        let lines = if exact { lazy_line_len(s) } else { n as usize };
        Some(LazyStatus { scanned, total, fraction: s.index_fraction(), index_done: s.index_done(), lines, lines_exact: exact, utf8: s.utf8_status() })
    }

    /// `(add_buffer_bytes, pieces, undo_bytes)` of a lazy buffer (memory diagnostics and tests).
    pub fn lazy_stats(&self) -> Option<(usize, usize, usize)> {
        match &self.backing {
            Backing::Lazy(s) => Some((s.add_len(), s.piece_count(), self.history.bytes())),
            Backing::Rope(_) => None,
        }
    }

    /// `(scanned, total)` bytes of the background pass; `(1, 1)` for a rope.
    pub fn index_progress(&self) -> (u64, u64) {
        match &self.backing {
            Backing::Rope(_) => (1, 1),
            Backing::Lazy(s) => s.index_progress(),
        }
    }

    /// Immutable snapshot for worker threads. O(1) for a rope; O(blocks) for a lazy store.
    pub fn snapshot(&self) -> BufferSnapshot {
        match &self.backing {
            Backing::Rope(r) => BufferSnapshot::Rope(r.clone()),
            Backing::Lazy(s) => BufferSnapshot::Lazy(s.snapshot()),
        }
    }

    /// Total byte length (UTF-8). Used for the status line "file size" field.
    pub fn byte_len(&self) -> usize {
        match &self.backing {
            Backing::Rope(r) => r.byte_len(),
            Backing::Lazy(s) => s.byte_len(),
        }
    }

    /// Number of lines (terminated-record convention, see the module doc). Never blocks: on a lazy buffer whose
    /// background index is still running it is an ESTIMATE (newline density of the part scanned so far), exact
    /// once [`Buffer::index_progress`] is complete (30-120 ms for 100 MB). [`Buffer::line_count_exact`] waits.
    pub fn line_count(&self) -> usize {
        self.line_count_hint().0
    }

    /// Exact line count; on a lazy buffer this waits for (and drives) the background index.
    pub fn line_count_exact(&self) -> usize {
        match &self.backing {
            Backing::Rope(r) => r.line_len(),
            Backing::Lazy(s) => lazy_line_len(s),
        }
    }

    /// Block until a lazy buffer's background index and UTF-8 check are finished (no-op for a rope).
    pub fn lazy_wait_index(&self) {
        if let Backing::Lazy(s) = &self.backing {
            s.wait_index();
        }
    }

    /// `(lines, exact)`: never blocks. Exact for a rope and once a lazy index is complete, else an estimate from
    /// the newline density seen so far (enough for a scrollbar or a far jump).
    pub fn line_count_hint(&self) -> (usize, bool) {
        match &self.backing {
            Backing::Rope(r) => (r.line_len(), true),
            Backing::Lazy(s) => {
                let (n, exact) = s.estimated_line_count();
                if exact {
                    (lazy_line_len(s), true)
                } else {
                    (n as usize, false)
                }
            }
        }
    }

    /// Byte range `[start..end)` of line `idx` including its terminator; `None` past the last line.
    fn line_bounds(&self, idx: usize) -> Option<Range<usize>> {
        match &self.backing {
            Backing::Rope(r) => {
                let n = r.line_len();
                if idx >= n {
                    return None;
                }
                let start = r.byte_of_line(idx);
                let end = if idx + 1 < n { r.byte_of_line(idx + 1) } else { r.byte_len() };
                Some(start..end)
            }
            Backing::Lazy(s) => {
                let len = s.byte_len();
                let a = s.byte_of_line(idx)?;
                if a >= len {
                    return None; // the empty line after a final line feed (or an empty text) does not exist
                }
                let b = s.byte_of_line(idx + 1).unwrap_or(len);
                Some(a..b)
            }
        }
    }

    /// Return the Nth line as an owned `String`, WITHOUT its line terminator. `None` if out of range.
    ///
    /// Allocating per line is fine at viewport sizes (~50 lines visible).
    pub fn nth_line(&self, idx: usize) -> Option<String> {
        match &self.backing {
            Backing::Rope(r) => {
                if idx >= r.line_len() {
                    return None;
                }
                // RopeSlice prints as its concatenated string via Display.
                Some(r.line(idx).to_string())
            }
            Backing::Lazy(s) => {
                let range = self.line_bounds(idx)?;
                Some(strip_terminator(s.slice_string(range)))
            }
        }
    }

    /// Iterate visible lines in `[start, end)`. End is clamped to line_count.
    pub fn visible_lines(&self, start: usize, end: usize) -> Vec<String> {
        match &self.backing {
            Backing::Rope(r) => {
                let end = end.min(r.line_len());
                if start >= end {
                    return Vec::new();
                }
                (start..end).map(|i| r.line(i).to_string()).collect()
            }
            Backing::Lazy(_) => {
                if start >= end {
                    return Vec::new();
                }
                let mut out = Vec::with_capacity((end - start).min(512));
                for i in start..end {
                    match self.nth_line(i) {
                        Some(l) => out.push(l),
                        None => break,
                    }
                }
                out
            }
        }
    }

    /// Borrow the underlying rope for read-only operations (highlight, search).
    ///
    /// Only a rope-backed buffer has one. On a LAZY buffer this returns an empty rope (never the text) and
    /// bumps [`lazy_rope_misuse`]: such a caller must use `slice` / `visit_str` / `snapshot` / the line adapters.
    pub fn rope(&self) -> &Rope {
        match &self.backing {
            Backing::Rope(r) => r,
            Backing::Lazy(_) => {
                ROPE_MISUSE.fetch_add(1, Ordering::Relaxed);
                empty_rope()
            }
        }
    }

    /// Byte offset where line `line` starts; the byte length for `line >= line_count()`. May wait for the lazy
    /// index (see [`Buffer::byte_of_line_ready`]).
    pub fn byte_of_line(&self, line: usize) -> usize {
        match &self.backing {
            Backing::Rope(r) => {
                if line >= r.line_len() {
                    r.byte_len()
                } else {
                    r.byte_of_line(line)
                }
            }
            Backing::Lazy(s) => s.byte_of_line(line).unwrap_or(s.byte_len()),
        }
    }

    /// Like [`Buffer::byte_of_line`] but never waits: `None` while the lazy index has not reached `line` (use
    /// [`Buffer::line_count_hint`] to place a far jump meanwhile and retry once `index_progress` advances).
    pub fn byte_of_line_ready(&self, line: usize) -> Option<usize> {
        match &self.backing {
            Backing::Rope(_) => Some(self.byte_of_line(line)),
            Backing::Lazy(s) => match s.line_start_ready(line as u64) {
                LineLookup::Found(x) => Some(x as usize),
                LineLookup::PastEnd => Some(s.byte_len()),
                LineLookup::NotReady => None,
            },
        }
    }

    /// 0-based line containing `byte` (clamped to the end).
    pub fn line_of_byte(&self, byte: usize) -> usize {
        let byte = byte.min(self.byte_len());
        match &self.backing {
            Backing::Rope(r) => r.line_of_byte(byte),
            Backing::Lazy(s) => s.line_of_byte(byte),
        }
    }

    pub fn is_char_boundary(&self, byte: usize) -> bool {
        match &self.backing {
            Backing::Rope(r) => byte <= r.byte_len() && r.is_char_boundary(byte),
            Backing::Lazy(s) => byte <= s.byte_len() && s.is_char_boundary(byte),
        }
    }

    /// The text of `range` (clamped to the document) as an owned `String`. Both ends must be char boundaries.
    pub fn slice(&self, range: Range<usize>) -> String {
        let len = self.byte_len();
        let (a, b) = (range.start.min(len), range.end.min(len));
        if a >= b {
            return String::new();
        }
        match &self.backing {
            Backing::Rope(r) => r.byte_slice(a..b).to_string(),
            Backing::Lazy(s) => s.slice_string(a..b),
        }
    }

    /// Stream the text of `range` as `&str` pieces (return false to stop). No whole-text copy on either backing.
    pub fn visit_str(&self, range: Range<usize>, f: &mut dyn FnMut(&str) -> bool) {
        let len = self.byte_len();
        let (a, b) = (range.start.min(len), range.end.min(len));
        if a >= b {
            return;
        }
        match &self.backing {
            Backing::Rope(r) => {
                for c in r.byte_slice(a..b).chunks() {
                    if !f(c) {
                        return;
                    }
                }
            }
            Backing::Lazy(s) => visit_as_str(s, a..b, f),
        }
    }

    /// Stream raw bytes of `range` (hash, save, hex).
    pub fn visit_bytes(&self, range: Range<usize>, f: &mut dyn FnMut(&[u8]) -> bool) {
        let len = self.byte_len();
        let (a, b) = (range.start.min(len), range.end.min(len));
        if a >= b {
            return;
        }
        match &self.backing {
            Backing::Rope(r) => {
                // A rope slice must start and end on character boundaries (crop panics otherwise, which crashed the
                // app when a hex row began inside a multi-byte character): widen to them, then trim the extra bytes.
                let (mut a2, mut b2) = (a, b);
                while !r.is_char_boundary(a2) {
                    a2 -= 1;
                }
                while !r.is_char_boundary(b2) {
                    b2 += 1;
                }
                let mut pos = a2;
                for c in r.byte_slice(a2..b2).chunks() {
                    let bytes = c.as_bytes();
                    let (cs, ce) = (pos, pos + bytes.len());
                    pos = ce;
                    let (lo, hi) = (a.max(cs), b.min(ce));
                    if lo < hi && !f(&bytes[lo - cs..hi - cs]) {
                        return;
                    }
                }
            }
            Backing::Lazy(s) => s.visit(a..b, f),
        }
    }

    /// The whole text as one `String` (a full copy: only for small documents or code that truly needs it;
    /// on a lazy buffer this is the very cost the lazy store exists to avoid).
    pub fn text(&self) -> String {
        self.slice(0..self.byte_len())
    }

    /// Stream the whole document to `w` (no BOM: see [`Buffer::lazy_has_bom`]).
    pub fn write_to(&self, w: &mut dyn io::Write) -> io::Result<()> {
        let mut err = None;
        self.visit_bytes(0..self.byte_len(), &mut |c| match w.write_all(c) {
            Ok(()) => true,
            Err(e) => {
                err = Some(e);
                false
            }
        });
        err.map_or(Ok(()), Err)
    }

    /// True for a lazy document whose file starts with a UTF-8 BOM (kept out of the text, rewritten on save).
    pub fn lazy_has_bom(&self) -> bool {
        match &self.backing {
            Backing::Lazy(s) => s.has_bom(),
            Backing::Rope(_) => false,
        }
    }

    /// Manually mark the buffer dirty. Used by callers that mutated the
    /// buffer through an out-of-band path (e.g. swapping in a brand-new
    /// `Buffer::from_str(...)` after Find/Replace commit). The mutation
    /// methods (`insert_at` / `remove_range` / `replace_range`) set this
    /// flag automatically.
    pub fn mark_dirty(&mut self) {
        self.dirty = true;
    }

    /// Return the byte range `[start..end)` covered by the Nth line in the
    /// rope, inclusive of the trailing `\n` (if any). Returns `None` if out
    /// of range. Used by the syntax painter to scope `spans_in_range` per
    /// line.
    pub fn line_byte_range(&self, idx: usize) -> Option<std::ops::Range<usize>> {
        self.line_bounds(idx)
    }

    // ---------- mutation API ----------

    /// Add-buffer housekeeping of a lazy buffer, run before each edit (consistent state: the previous edit is in
    /// the history). When the add buffer is big and mostly dead (the history dropped the groups that used it),
    /// rebuild it with the live bytes only.
    fn lazy_maintain(&mut self) {
        if self.open.is_some() {
            return; // entries of the open group are not in the history yet
        }
        let Backing::Lazy(s) = &mut self.backing else { return };
        let add = s.add_len();
        if add < self.compact_mark {
            return;
        }
        let mut hist = 0usize;
        self.history.visit_pieces(&mut |p| {
            if p.src == 0 {
                hist += p.len as usize;
            }
        });
        let live = s.doc_add_bytes() + hist;
        if live * 2 < add {
            let history = &mut self.history;
            s.compact_add(&mut |f| history.visit_pieces_mut(f));
        }
        self.compact_mark = (s.add_len() * 2).max(COMPACT_MIN);
    }

    /// Single primitive every text change goes through: syncs the CRDT,
    /// mutates the text, shifts anchors, bumps `version`. Returns what was
    /// removed (`None` = refused: read-only lazy document or an offset that is not a char boundary; nothing
    /// changed). Does NOT touch history.
    fn splice(&mut self, start: usize, end: usize, text: &str) -> Option<Spliced> {
        self.lazy_maintain();
        let spliced = match &mut self.backing {
            Backing::Rope(rope) => {
                if let Some(ref session) = self.crdt {
                    let cs = byte_to_char_idx(rope, start);
                    let ce = byte_to_char_idx(rope, end);
                    if ce > cs {
                        let _ = session.delete(cs, ce - cs);
                    }
                    if !text.is_empty() {
                        let _ = session.insert(cs, text);
                    }
                }
                let removed = if end > start { rope.byte_slice(start..end).to_string() } else { String::new() };
                if end > start {
                    rope.delete(start..end);
                }
                if !text.is_empty() {
                    rope.insert(start, text);
                }
                Spliced { removed, rec: None }
            }
            Backing::Lazy(store) => {
                let rec = store.edit(start as u64, end as u64, text).ok()?;
                Spliced { removed: String::new(), rec: Some(Box::new(rec)) }
            }
        };
        for a in &mut self.anchors {
            a.pos = map_splice(a.pos, start, end, text.len(), a.bias);
        }
        self.version += 1;
        self.dirty = true;
        Some(spliced)
    }

    /// Record a reverse entry: into the open transaction, else as its own group.
    fn record(&mut self, entry: UndoEntry) {
        match self.open.as_mut() {
            Some(g) => g.entries.push(entry),
            None => {
                self.history.push_undo(entry);
            }
        }
    }

    /// Insert UTF-8 text at the given byte position. Bumps the dirty flag.
    ///
    /// `byte_pos` is clamped to `byte_len()` (inserting past EOF is treated as
    /// "append"). Note: `crop::Rope::insert` panics if `byte_pos` is not on a
    /// UTF-8 char boundary — that panic propagates unchanged. A lazy buffer instead ignores the edit. We do NOT
    /// attempt to round to the nearest boundary; the caller (cursor/find-replace) owns boundary discipline.
    ///
    /// Each call is its own undo group unless inside `transaction`.
    pub fn insert_at(&mut self, byte_pos: usize, text: &str) {
        let pos = byte_pos.min(self.byte_len());
        let Some(sp) = self.splice(pos, pos, text) else { return };
        // Recorded even for empty `text`, for symmetry; undoing it is a no-op.
        let entry = sp.entry(|_| UndoEntry::RemoveRange { byte_range: pos..pos + text.len() });
        self.record(entry);
    }

    /// Remove the byte range. Bumps the dirty flag. End is clamped to
    /// `byte_len()`. Empty / inverted ranges are a no-op and do NOT mark dirty
    /// (preserves "dirty == has real edits" semantics).
    pub fn remove_range(&mut self, byte_range: std::ops::Range<usize>) {
        let max = self.byte_len();
        let start = byte_range.start.min(max);
        let end = byte_range.end.min(max);
        if start >= end {
            return;
        }
        let Some(sp) = self.splice(start, end, "") else { return };
        let entry = sp.entry(|removed| UndoEntry::InsertAt { byte_pos: start, text: removed });
        self.record(entry);
    }

    /// Replace a byte range with `replacement`. Atomic from the caller's view
    /// (single dirty bump even if the underlying rope does two ops). Bumps the
    /// dirty flag unconditionally — even an empty replacement of an empty range
    /// is treated as "caller intended an edit". (Find/Replace never calls this
    /// with empty/empty.)
    pub fn replace_range(&mut self, byte_range: std::ops::Range<usize>, replacement: &str) {
        let max = self.byte_len();
        let start = byte_range.start.min(max);
        let end = byte_range.end.min(max);
        let range = if start >= end { start..start } else { start..end };
        let Some(sp) = self.splice(range.start, range.end, replacement) else { return };
        let entry = sp.entry(|removed| UndoEntry::ReplaceRange { byte_range: range.start..range.start + replacement.len(), text: removed });
        self.record(entry);
    }

    // ---------- atomic edits & transactions (BK-001/004) ----------

    /// Apply an `EditSet` (offsets in the ORIGINAL text, ascending,
    /// non-overlapping) as ONE undo group, atomically: the whole set is
    /// validated first (order, overlap, bounds, char boundaries) and on `Err`
    /// the buffer is untouched. An empty set is a no-op (no group, not dirty).
    /// The result is identical to `EditSet::apply_to_str`.
    pub fn apply_edits(&mut self, edits: &EditSet) -> Result<(), EditError> {
        self.apply_edits_with(edits, EditMeta::default())
    }

    /// `apply_edits` with an origin tag (e.g. `Origin::Ai("claude")`) and
    /// selections to restore on undo / redo. Inside a `transaction` the edits
    /// join the open group (its meta wins). On a read-only lazy document every non-empty set is refused
    /// with `OutOfRange` (nothing changes).
    pub fn apply_edits_with(&mut self, edits: &EditSet, meta: EditMeta) -> Result<(), EditError> {
        edits.validate()?;
        let len = self.byte_len();
        for (i, e) in edits.0.iter().enumerate() {
            if e.end_byte > len || !self.is_char_boundary(e.start_byte) || !self.is_char_boundary(e.end_byte) {
                return Err(EditError::OutOfRange { index: i });
            }
        }
        if edits.is_empty() {
            return Ok(());
        }
        if self.is_read_only() {
            return Err(EditError::OutOfRange { index: 0 });
        }
        self.begin_group(meta);
        // Back to front: earlier offsets stay valid without shifting.
        for e in edits.0.iter().rev() {
            if let Some(sp) = self.splice(e.start_byte, e.end_byte, &e.text) {
                let (start, tlen) = (e.start_byte, e.text.len());
                let entry = sp.entry(|removed| UndoEntry::ReplaceRange { byte_range: start..start + tlen, text: removed });
                self.record(entry);
            }
        }
        self.end_group();
        Ok(())
    }

    /// Replace the whole text by `new_text`, applied as the minimal local
    /// edits (`edit_apply::diff_edits`) in one undo group, so anchors and the
    /// caret stay put. Returns the number of edits. (On a lazy buffer the old text is read in full for the
    /// diff: acceptable for the rare reload, not for a hot path.)
    pub fn apply_text_diff(&mut self, new_text: &str) -> usize {
        let set = crate::edit_apply::diff_edits(&self.text(), new_text);
        let n = set.len();
        self.apply_edits(&set).expect("diff_edits yields valid edits");
        n
    }

    /// Run `f` so that every mutation inside becomes ONE undo group (nests:
    /// inner transactions join the outer one). An empty transaction pushes
    /// nothing. Not unwind-safe: if `f` panics the group stays open.
    pub fn transaction<R>(&mut self, f: impl FnOnce(&mut Buffer) -> R) -> R {
        self.transaction_with(EditMeta::default(), f)
    }

    pub fn transaction_with<R>(&mut self, meta: EditMeta, f: impl FnOnce(&mut Buffer) -> R) -> R {
        self.begin_group(meta);
        let r = f(self);
        self.end_group();
        r
    }

    fn begin_group(&mut self, meta: EditMeta) {
        match self.open.as_mut() {
            Some(g) => g.depth += 1,
            None => self.open = Some(OpenGroup { meta, entries: Vec::new(), depth: 1 }),
        }
    }

    fn end_group(&mut self) {
        let g = self.open.as_mut().expect("end_group without begin_group");
        g.depth -= 1;
        if g.depth > 0 {
            return;
        }
        let g = self.open.take().expect("checked above");
        if !g.entries.is_empty() {
            self.history.push_group(g.meta.origin, g.entries, g.meta.selection_before, g.meta.selection_after);
        }
    }

    // ---------- typing coalescing (BK-002) ----------

    /// Type `text` at `pos`, merging into the previous undo group while the
    /// run continues: contiguous position, <= 1 s since the last keystroke
    /// (`now_ms` is caller-supplied, so tests inject a clock), and no word
    /// boundary (a word char typed after punctuation / space starts a new
    /// group). A text containing a newline (Enter) is its own group and closes
    /// the run. Inside a `transaction` this is a plain `insert_at`.
    pub fn typing_insert(&mut self, pos: usize, text: &str, now_ms: u64) {
        if self.open.is_some() {
            return self.insert_at(pos, text);
        }
        let pos = pos.min(self.byte_len());
        let enter = text.contains('\n');
        let merge = !enter && self.history.typing_mergeable(TypingKind::Insert, pos, text.chars().next(), now_ms);
        let Some(sp) = self.splice(pos, pos, text) else { return };
        let entry = sp.entry(|_| UndoEntry::RemoveRange { byte_range: pos..pos + text.len() });
        if merge {
            self.history.extend_top(entry, None);
        } else {
            self.history.push_undo(entry);
        }
        if enter {
            self.history.break_typing();
        } else {
            self.history.set_typing(TypingKind::Insert, pos + text.len(), text.chars().last(), now_ms);
        }
    }

    /// Coalesced delete: `forward = false` is Backspace, `true` is Delete.
    /// Same merge rules as `typing_insert` (consecutive deletes in one run).
    pub fn typing_delete(&mut self, byte_range: std::ops::Range<usize>, forward: bool, now_ms: u64) {
        if self.open.is_some() {
            return self.remove_range(byte_range);
        }
        let max = self.byte_len();
        let (start, end) = (byte_range.start.min(max), byte_range.end.min(max));
        if start >= end {
            return;
        }
        let (kind, at) = if forward { (TypingKind::DeleteFwd, start) } else { (TypingKind::DeleteBack, end) };
        let merge = self.history.typing_mergeable(kind, at, None, now_ms);
        let Some(sp) = self.splice(start, end, "") else { return };
        let entry = sp.entry(|removed| UndoEntry::InsertAt { byte_pos: start, text: removed });
        if merge {
            self.history.extend_top(entry, None);
        } else {
            self.history.push_undo(entry);
        }
        self.history.set_typing(kind, start, None, now_ms);
    }

    /// Close the current typing run (caret jump, any non-typing command).
    pub fn break_typing(&mut self) {
        self.history.break_typing();
    }

    // ---------- anchors (BK-005) ----------

    /// Track a byte position (clamped). It shifts through every later change:
    /// edits, `apply_edits`, undo and redo. A position inside a deleted /
    /// replaced range collapses to the range start (`Bias::Left`) or the end
    /// of the replacement (`Bias::Right`); never out of range.
    pub fn add_anchor(&mut self, pos: usize, bias: Bias) -> AnchorId {
        let id = AnchorId(self.next_anchor);
        self.next_anchor += 1;
        let pos = pos.min(self.byte_len());
        self.anchors.push(Anchor { id, pos, bias });
        id
    }

    pub fn anchor_pos(&self, id: AnchorId) -> Option<usize> {
        self.anchors.iter().find(|a| a.id == id).map(|a| a.pos)
    }

    /// 0-based line of the anchor.
    pub fn anchor_line(&self, id: AnchorId) -> Option<usize> {
        self.anchor_pos(id).map(|p| self.line_of_byte(p))
    }

    pub fn remove_anchor(&mut self, id: AnchorId) {
        self.anchors.retain(|a| a.id != id);
    }

    // ---------- identity for external clients (MCP guard) ----------

    /// Counter bumped on every text change (edit, undo, redo). Two reads with
    /// the same value mean the text did not change in between.
    pub fn version(&self) -> u64 {
        self.version
    }

    /// FNV-1a 64-bit content hash (stable, not cryptographic). Guards against
    /// disk reloads that reset `version`. Streams the text (a full pass over a lazy file).
    pub fn hash(&self) -> u64 {
        let mut h: u64 = 0xcbf2_9ce4_8422_2325;
        self.visit_bytes(0..self.byte_len(), &mut |chunk| {
            for b in chunk {
                h ^= u64::from(*b);
                h = h.wrapping_mul(0x0000_0100_0000_01b3);
            }
            true
        });
        h
    }

    // ---------- persistence ----------

    /// Write the text to `path`. Clears the dirty flag on success.
    /// On I/O failure the dirty flag is preserved (caller can retry).
    ///
    /// A lazy buffer streams its pieces into a temp file next to `path`, fsyncs and renames it over `path`
    /// (the saved file becomes the new base of the piece table; no copy of the document in RAM).
    pub fn save_to_path(&mut self, path: impl AsRef<Path>) -> io::Result<()> {
        match &mut self.backing {
            Backing::Rope(rope) => {
                // `Rope` Displays as its concatenated content — cheaper than a custom
                // chunk walker at day-1 sizes.
                let contents = rope.to_string();
                crate::durable::durable_write(path.as_ref(), contents.as_bytes())?;
            }
            Backing::Lazy(store) => {
                if store.is_read_only() {
                    return Err(io::Error::new(io::ErrorKind::PermissionDenied, "the file is not valid UTF-8: this document is read-only"));
                }
                store.save_keep_bases(path.as_ref())?;
            }
        }
        self.dirty = false;
        self.history.mark_saved();
        self.lazy_after_save();
        Ok(())
    }

    /// A lazy buffer whose text was just written to `path` by someone else (a backup-aware save from the bytes of
    /// [`Buffer::write_to`]): make the file the new base of the piece table. No-op for a rope.
    pub fn lazy_rebase_after_external_save(&mut self, path: &Path) -> io::Result<()> {
        if let Backing::Lazy(store) = &mut self.backing {
            store.rebase_to_file(path)?;
            self.lazy_after_save();
        }
        Ok(())
    }

    /// After a save the old base generation is only needed by undo records. Big documents drop their history
    /// (the replaced file would otherwise stay on disk until the history ages out); smaller ones keep it, and
    /// every generation no record refers to is released.
    fn lazy_after_save(&mut self) {
        let Backing::Lazy(store) = &mut self.backing else { return };
        if store.byte_len() > KEEP_HISTORY_ACROSS_SAVE_MAX {
            self.history.clear();
            self.history.mark_saved();
        }
        let history = &self.history;
        store.gc_bases_with(&mut |f| history.visit_pieces(f));
        self.compact_mark = (store.add_len() * 2).max(COMPACT_MIN);
    }

    // ---------- dirty inspection ----------

    /// True iff the buffer has been mutated since the last save / load.
    pub fn is_dirty(&self) -> bool {
        self.dirty
    }

    /// Force the dirty flag to false and record the save marker (history is
    /// kept). Used when an external code path persisted the buffer (e.g. via
    /// `TabState` calling its own writer).
    pub fn mark_clean(&mut self) {
        self.dirty = false;
        self.history.mark_saved();
    }

    /// Record the current state as the saved one without touching history.
    pub fn mark_saved(&mut self) {
        self.mark_clean();
    }

    /// True when the text equals the last saved text (as far as history knows).
    pub fn is_at_save(&self) -> bool {
        self.history.is_at_save()
    }

    // ---------- undo / redo ----------

    /// True iff at least one undo step is available.
    pub fn can_undo(&self) -> bool {
        self.history.can_undo()
    }

    /// True iff at least one redo step is available.
    pub fn can_redo(&self) -> bool {
        self.history.can_redo()
    }

    /// Apply one undo step (one GROUP). Returns `true` if a step was applied,
    /// `false` when the undo stack was empty (caller can no-op the keystroke).
    pub fn undo(&mut self) -> bool {
        self.undo_restoring().is_some()
    }

    /// Apply one redo step. Returns `true` if a step was applied.
    pub fn redo(&mut self) -> bool {
        self.redo_restoring().is_some()
    }

    /// Undo one group; `Some(selection)` carries the selection saved with the
    /// group (`None` inside = none recorded). Outer `None` = nothing to undo.
    pub fn undo_restoring(&mut self) -> Option<Option<Vec<Region>>> {
        let g = self.history.pop_undo()?;
        let sel = g.selection_before.clone();
        let inverse = self.apply_group(g);
        self.history.push_redo(inverse);
        self.dirty = !self.history.is_at_save();
        Some(sel)
    }

    /// Redo one group; same return convention as `undo_restoring`.
    pub fn redo_restoring(&mut self) -> Option<Option<Vec<Region>>> {
        let g = self.history.pop_redo()?;
        let sel = g.selection_after.clone();
        let inverse = self.apply_group(g);
        self.history.push_undo_no_redo_clear(inverse);
        self.dirty = !self.history.is_at_save();
        Some(sel)
    }

    /// Undo every group ("Undo All"). Returns how many were undone.
    pub fn undo_all(&mut self) -> usize {
        let mut n = 0;
        while self.undo() {
            n += 1;
        }
        n
    }

    /// Travel (undo, or redo if the user already went past it) to the text of
    /// the last save, keeping history. Returns false if the saved state is no
    /// longer reachable (oldest groups dropped) or we are already there.
    pub fn undo_to_save(&mut self) -> bool {
        let Some(undo) = self.history.save_direction() else {
            return false;
        };
        while !self.history.is_at_save() && if undo { self.undo() } else { self.redo() } {}
        true
    }

    /// Origin of the group the next undo would revert.
    pub fn top_undo_origin(&self) -> Option<&Origin> {
        self.history.top_origin()
    }

    /// Change history bounds (default 1000 groups / 64 MB).
    pub fn set_undo_limits(&mut self, max_groups: usize, max_bytes: usize) {
        self.history.set_limits(max_groups, max_bytes);
    }

    /// Undo text currently held, for a global budget across buffers.
    pub fn undo_bytes(&self) -> usize {
        self.history.bytes()
    }

    /// Drop the oldest undo group (global budget enforcement).
    pub fn drop_oldest_undo(&mut self) -> bool {
        self.history.drop_oldest()
    }

    /// Apply a whole group in reverse recording order; returns its inverse
    /// (same id / origin / selections) for the opposite stack.
    fn apply_group(&mut self, mut g: UndoGroup) -> UndoGroup {
        let entries = std::mem::take(&mut g.entries);
        g.entries = entries.into_iter().rev().map(|e| self.apply_entry_capturing_inverse(e)).collect();
        g
    }

    /// Drop all undo / redo history. Called after a buffer "becomes a
    /// different document" — e.g. when the host swaps in a fresh
    /// `Buffer::from_str(...)` post-reload and wants to reset undo too.
    pub fn clear_history(&mut self) {
        self.history.clear();
        if !self.dirty {
            self.history.mark_saved();
        }
        if let Backing::Lazy(store) = &mut self.backing {
            let history = &self.history;
            store.gc_bases_with(&mut |f| history.visit_pieces(f));
        }
    }

    /// Apply an `UndoEntry` directly on the text (without recording it on
    /// history) and return the inverse entry so the caller can push it
    /// onto the other stack. Private to keep the public surface minimal.
    fn apply_entry_capturing_inverse(&mut self, entry: UndoEntry) -> UndoEntry {
        match entry {
            UndoEntry::RemoveRange { byte_range } => {
                // Inverse of a remove is an insert of the removed text.
                let removed = self.splice(byte_range.start, byte_range.end, "").map(|s| s.removed).unwrap_or_default();
                UndoEntry::InsertAt { byte_pos: byte_range.start, text: removed }
            }
            UndoEntry::InsertAt { byte_pos, text } => {
                self.splice(byte_pos, byte_pos, &text);
                UndoEntry::RemoveRange { byte_range: byte_pos..byte_pos + text.len() }
            }
            UndoEntry::ReplaceRange { byte_range, text } => {
                let removed = self.splice(byte_range.start, byte_range.end, &text).map(|s| s.removed).unwrap_or_default();
                UndoEntry::ReplaceRange { byte_range: byte_range.start..byte_range.start + text.len(), text: removed }
            }
            UndoEntry::Pieces(rec) => {
                let (pos, ins, rem) = (rec.pos as usize, rec.inserted_len() as usize, rec.removed_len() as usize);
                if let Backing::Lazy(store) = &mut self.backing {
                    store.apply_rec_inverse(&rec);
                }
                for a in &mut self.anchors {
                    a.pos = map_splice(a.pos, pos, pos + ins, rem, a.bias);
                }
                self.version += 1;
                self.dirty = true;
                UndoEntry::Pieces(Box::new(rec.swapped()))
            }
        }
    }

    /// Associate a CRDT session with this buffer. (A lazy buffer is not collaborative: the session is dropped.)
    pub fn with_crdt(mut self, crdt: crate::crdt::CrdtSession) -> Self {
        if self.is_lazy() {
            return self;
        }
        let text = self.text();
        let _ = crdt.insert(0, &text);
        self.crdt = Some(crdt);
        self
    }

    /// Reference to the associated CRDT session, if any
    pub fn crdt(&self) -> Option<&crate::crdt::CrdtSession> {
        self.crdt.as_ref()
    }
}

impl BufferSnapshot {
    pub fn is_lazy(&self) -> bool {
        matches!(self, Self::Lazy(_))
    }
    pub fn byte_len(&self) -> usize {
        match self {
            Self::Rope(r) => r.byte_len(),
            Self::Lazy(s) => s.byte_len(),
        }
    }
    /// Terminated-line count (see the module doc); waits for a lazy index.
    pub fn line_count(&self) -> usize {
        match self {
            Self::Rope(r) => r.line_len(),
            Self::Lazy(s) => lazy_line_len(s),
        }
    }
    pub fn byte_of_line(&self, line: usize) -> usize {
        match self {
            Self::Rope(r) => {
                if line >= r.line_len() {
                    r.byte_len()
                } else {
                    r.byte_of_line(line)
                }
            }
            Self::Lazy(s) => s.byte_of_line(line).unwrap_or(s.byte_len()),
        }
    }
    pub fn line_of_byte(&self, byte: usize) -> usize {
        let byte = byte.min(self.byte_len());
        match self {
            Self::Rope(r) => r.line_of_byte(byte),
            Self::Lazy(s) => s.line_of_byte(byte),
        }
    }
    pub fn is_char_boundary(&self, byte: usize) -> bool {
        match self {
            Self::Rope(r) => byte <= r.byte_len() && r.is_char_boundary(byte),
            Self::Lazy(s) => byte <= s.byte_len() && s.is_char_boundary(byte),
        }
    }
    pub fn slice(&self, range: Range<usize>) -> String {
        let len = self.byte_len();
        let (a, b) = (range.start.min(len), range.end.min(len));
        if a >= b {
            return String::new();
        }
        match self {
            Self::Rope(r) => r.byte_slice(a..b).to_string(),
            Self::Lazy(s) => s.slice_string(a..b),
        }
    }
    /// Stream `&str` pieces of `range` (return false to stop).
    pub fn visit_str(&self, range: Range<usize>, f: &mut dyn FnMut(&str) -> bool) {
        let len = self.byte_len();
        let (a, b) = (range.start.min(len), range.end.min(len));
        if a >= b {
            return;
        }
        match self {
            Self::Rope(r) => {
                for c in r.byte_slice(a..b).chunks() {
                    if !f(c) {
                        return;
                    }
                }
            }
            Self::Lazy(s) => visit_as_str(s, a..b, f),
        }
    }
    pub fn visit_bytes(&self, range: Range<usize>, f: &mut dyn FnMut(&[u8]) -> bool) {
        let len = self.byte_len();
        let (a, b) = (range.start.min(len), range.end.min(len));
        if a >= b {
            return;
        }
        match self {
            Self::Rope(r) => {
                for c in r.byte_slice(a..b).chunks() {
                    if !f(c.as_bytes()) {
                        return;
                    }
                }
            }
            Self::Lazy(s) => s.visit(a..b, f),
        }
    }
    /// The rope of a rope-backed snapshot (`None` for a lazy one): lets the existing rope-based search keep
    /// its zero-copy path for small files.
    pub fn as_rope(&self) -> Option<&Rope> {
        match self {
            Self::Rope(r) => Some(r),
            Self::Lazy(_) => None,
        }
    }
}

/// Helper function to convert a byte index to Unicode character index in crop::Rope
fn byte_to_char_idx(rope: &Rope, byte_pos: usize) -> usize {
    let mut char_count = 0;
    let mut current_byte = 0;
    for chunk in rope.chunks() {
        if current_byte + chunk.len() > byte_pos {
            let slice_len = byte_pos - current_byte;
            char_count += chunk[..slice_len].chars().count();
            break;
        }
        char_count += chunk.chars().count();
        current_byte += chunk.len();
    }
    char_count
}

/// Helper function to convert a Unicode character index to byte index in crop::Rope
#[allow(dead_code)]
fn char_to_byte_idx(rope: &Rope, char_pos: usize) -> usize {
    let mut current_char = 0;
    let mut byte_offset = 0;
    for chunk in rope.chunks() {
        let chunk_chars = chunk.chars().count();
        if current_char + chunk_chars > char_pos {
            let needed_chars = char_pos - current_char;
            for c in chunk.chars().take(needed_chars) {
                byte_offset += c.len_utf8();
            }
            return byte_offset;
        }
        current_char += chunk_chars;
        byte_offset += chunk.len();
    }
    byte_offset
}

#[cfg(test)]
mod tests {
    /// `visit_bytes` on a rope must not panic when the range starts or ends inside a multi-byte character
    /// (a hex row boundary does that); it yields exactly those bytes of the UTF-8 text.
    #[test]
    fn visit_bytes_is_safe_inside_multibyte_characters() {
        let text = "caf\u{e9} \u{20ac}uro \u{1f600} smile\nline \u{e9}\u{e9}\u{e9}\n".repeat(40);
        let buf = Buffer::from_str(&text);
        let bytes = text.as_bytes();
        for start in (0..bytes.len()).step_by(7) {
            for len in [1usize, 3, 16, 33] {
                let end = (start + len).min(bytes.len());
                let mut got = Vec::new();
                buf.visit_bytes(start..end, &mut |c| {
                    got.extend_from_slice(c);
                    true
                });
                assert_eq!(got, bytes[start..end], "range {start}..{end}");
            }
        }
    }

    use super::*;

    // ---------- existing read-only behavior is covered in crate::tests ----------
    // These tests focus on the mutation + dirty + save API.

    #[test]
    fn fresh_buffer_is_not_dirty() {
        let buf = Buffer::from_str("hello\nworld\n");
        assert!(!buf.is_dirty(), "freshly constructed buffer must not be dirty");
    }

    #[test]
    fn insert_at_beginning_appends_text() {
        let mut buf = Buffer::from_str("world");
        buf.insert_at(0, "hello ");
        assert_eq!(buf.rope().to_string(), "hello world");
        assert!(buf.is_dirty(), "insert must bump dirty");
    }

    #[test]
    fn insert_at_end_appends_text() {
        let mut buf = Buffer::from_str("hello");
        let end = buf.byte_len();
        buf.insert_at(end, " world");
        assert_eq!(buf.rope().to_string(), "hello world");
        assert!(buf.is_dirty());
    }

    #[test]
    fn insert_at_middle_inserts_in_position() {
        let mut buf = Buffer::from_str("helloworld");
        buf.insert_at(5, " ");
        assert_eq!(buf.rope().to_string(), "hello world");
        assert!(buf.is_dirty());
    }

    #[test]
    fn insert_past_end_clamps_to_byte_len() {
        let mut buf = Buffer::from_str("abc");
        buf.insert_at(9999, "!");
        assert_eq!(buf.rope().to_string(), "abc!");
        assert!(buf.is_dirty());
    }

    #[test]
    fn remove_range_from_beginning() {
        let mut buf = Buffer::from_str("hello world");
        buf.remove_range(0..6);
        assert_eq!(buf.rope().to_string(), "world");
        assert!(buf.is_dirty());
    }

    #[test]
    fn remove_range_from_end() {
        let mut buf = Buffer::from_str("hello world");
        buf.remove_range(5..11);
        assert_eq!(buf.rope().to_string(), "hello");
        assert!(buf.is_dirty());
    }

    #[test]
    fn remove_range_from_middle() {
        let mut buf = Buffer::from_str("hello cruel world");
        buf.remove_range(5..11);
        assert_eq!(buf.rope().to_string(), "hello world");
        assert!(buf.is_dirty());
    }

    #[test]
    fn remove_empty_range_is_noop_and_clean() {
        let mut buf = Buffer::from_str("abc");
        buf.remove_range(1..1);
        assert_eq!(buf.rope().to_string(), "abc");
        assert!(!buf.is_dirty(), "no-op range must not mark dirty");
    }

    #[test]
    fn replace_range_same_length() {
        let mut buf = Buffer::from_str("hello world");
        buf.replace_range(0..5, "HELLO");
        assert_eq!(buf.rope().to_string(), "HELLO world");
        assert!(buf.is_dirty());
    }

    #[test]
    fn replace_range_longer_than_original() {
        let mut buf = Buffer::from_str("hi world");
        buf.replace_range(0..2, "hello");
        assert_eq!(buf.rope().to_string(), "hello world");
        assert!(buf.is_dirty());
    }

    #[test]
    fn replace_range_shorter_than_original() {
        let mut buf = Buffer::from_str("hello world");
        buf.replace_range(0..5, "hi");
        assert_eq!(buf.rope().to_string(), "hi world");
        assert!(buf.is_dirty());
    }

    #[test]
    fn save_to_path_roundtrip_creates_file_with_content() {
        let mut buf = Buffer::from_str("roundtrip payload\nline 2\n");
        let mut path = std::env::temp_dir();
        path.push(format!(
            "brouillon-buffer-save-{}-{}.txt",
            std::process::id(),
            // tiny uniqueness suffix to avoid collision under parallel test
            // runs without bringing in a new dep
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));

        buf.save_to_path(&path).expect("save must succeed in temp_dir");
        let read_back = std::fs::read_to_string(&path).expect("written file must be readable");
        assert_eq!(read_back, "roundtrip payload\nline 2\n");

        // cleanup — ignore errors (test already passed if we got here)
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn save_clears_dirty_flag() {
        let mut buf = Buffer::from_str("seed");
        buf.insert_at(4, " more");
        assert!(buf.is_dirty());

        let mut path = std::env::temp_dir();
        path.push(format!(
            "brouillon-buffer-save-clean-{}-{}.txt",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));

        buf.save_to_path(&path).expect("save must succeed");
        assert!(!buf.is_dirty(), "successful save must clear dirty");

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn mark_clean_resets_flag() {
        let mut buf = Buffer::from_str("seed");
        buf.insert_at(0, "x");
        assert!(buf.is_dirty());
        buf.mark_clean();
        assert!(!buf.is_dirty(), "mark_clean must reset dirty");
    }

    #[test]
    fn clone_preserves_dirty_state() {
        let mut buf = Buffer::from_str("orig");
        buf.insert_at(0, "!");
        let cloned = buf.clone();
        assert!(cloned.is_dirty(), "clone preserves dirty flag (documented)");
        assert_eq!(cloned.rope().to_string(), buf.rope().to_string());
    }

    // ---------- undo / redo behavior ----------

    #[test]
    fn from_str_starts_empty_history() {
        let buf = Buffer::from_str("hello");
        assert!(!buf.can_undo(), "fresh buffer must have no undo");
        assert!(!buf.can_redo(), "fresh buffer must have no redo");
    }

    #[test]
    fn insert_then_undo_reverses() {
        let mut buf = Buffer::from_str("world");
        buf.insert_at(0, "hello ");
        assert_eq!(buf.rope().to_string(), "hello world");
        assert!(buf.can_undo());

        let applied = buf.undo();
        assert!(applied, "undo must report it did work");
        assert_eq!(buf.rope().to_string(), "world", "undo must restore original text");
        assert!(buf.can_redo(), "redo must be available after an undo");
    }

    #[test]
    fn remove_then_undo_restores_text() {
        let mut buf = Buffer::from_str("hello world");
        buf.remove_range(5..11); // removes " world"
        assert_eq!(buf.rope().to_string(), "hello");

        assert!(buf.undo(), "undo must succeed");
        assert_eq!(buf.rope().to_string(), "hello world", "removed text must be restored verbatim");
    }

    #[test]
    fn replace_then_undo_restores_original() {
        let mut buf = Buffer::from_str("hello world");
        buf.replace_range(0..5, "HOWDY");
        assert_eq!(buf.rope().to_string(), "HOWDY world");

        assert!(buf.undo());
        assert_eq!(buf.rope().to_string(), "hello world");
    }

    #[test]
    fn undo_then_redo_returns_to_post_edit_state() {
        let mut buf = Buffer::from_str("abc");
        buf.insert_at(3, "DEF");
        assert_eq!(buf.rope().to_string(), "abcDEF");

        assert!(buf.undo());
        assert_eq!(buf.rope().to_string(), "abc");

        assert!(buf.can_redo());
        assert!(buf.redo());
        assert_eq!(buf.rope().to_string(), "abcDEF", "redo must reapply the original edit");
    }

    #[test]
    fn multiple_inserts_then_three_undos() {
        let mut buf = Buffer::from_str("");
        buf.insert_at(0, "a");
        buf.insert_at(1, "b");
        buf.insert_at(2, "c");
        assert_eq!(buf.rope().to_string(), "abc");

        assert!(buf.undo());
        assert_eq!(buf.rope().to_string(), "ab");
        assert!(buf.undo());
        assert_eq!(buf.rope().to_string(), "a");
        assert!(buf.undo());
        assert_eq!(buf.rope().to_string(), "");
        assert!(!buf.can_undo(), "empty history after draining");
    }

    #[test]
    fn undo_marks_dirty() {
        let mut buf = Buffer::from_str("seed");
        buf.insert_at(0, "x");
        buf.mark_clean();
        assert!(!buf.is_dirty(), "precondition: marked clean");

        assert!(buf.undo());
        assert!(buf.is_dirty(), "undo must re-dirty the buffer (it's a mutation)");
    }

    #[test]
    fn new_edit_after_undo_truncates_redo() {
        let mut buf = Buffer::from_str("abc");
        buf.insert_at(3, "DEF");
        assert!(buf.undo());
        assert!(buf.can_redo());

        // A brand-new edit while redo exists must drop the redo branch.
        buf.insert_at(3, "Z");
        assert!(!buf.can_redo(), "fresh edit must invalidate redo");
        assert_eq!(buf.rope().to_string(), "abcZ");
    }

    #[test]
    fn undo_on_empty_history_returns_false() {
        let mut buf = Buffer::from_str("abc");
        assert!(!buf.undo(), "undo on empty history must return false");
        assert_eq!(buf.rope().to_string(), "abc", "empty undo must not mutate rope");
    }

    #[test]
    fn redo_on_empty_redo_returns_false() {
        let mut buf = Buffer::from_str("abc");
        buf.insert_at(0, "!");
        // never undo'd → redo stack is empty
        assert!(!buf.redo());
        assert_eq!(buf.rope().to_string(), "!abc");
    }

    #[test]
    fn clear_history_resets_undo_redo() {
        let mut buf = Buffer::from_str("abc");
        buf.insert_at(3, "DEF");
        assert!(buf.can_undo());
        buf.clear_history();
        assert!(!buf.can_undo());
        assert!(!buf.can_redo());
    }

    #[test]
    fn undo_redo_chain_with_replace() {
        let mut buf = Buffer::from_str("hello world");
        buf.replace_range(6..11, "RUST");
        assert_eq!(buf.rope().to_string(), "hello RUST");
        assert!(buf.undo());
        assert_eq!(buf.rope().to_string(), "hello world");
        assert!(buf.redo());
        assert_eq!(buf.rope().to_string(), "hello RUST");
        assert!(buf.undo());
        assert_eq!(buf.rope().to_string(), "hello world");
    }

    #[test]
    fn remove_noop_does_not_record_undo() {
        let mut buf = Buffer::from_str("abc");
        // Empty range is a no-op AND must not push to history (preserves
        // "dirty == real edits" semantics).
        buf.remove_range(1..1);
        assert!(!buf.can_undo());
        assert!(!buf.is_dirty());
    }

    #[test]
    fn insert_empty_string_noop() {
        let mut buf = Buffer::from_str("abc");
        buf.insert_at(1, "");
        assert!(buf.can_undo());
        assert!(buf.is_dirty());
    }

    #[test]
    fn test_buffer_crdt_sync() {
        let crdt = crate::crdt::CrdtSession::new();
        let mut buf = Buffer::from_str("hello").with_crdt(crdt.clone());
        buf.insert_at(5, " world");
        assert_eq!(buf.rope().to_string(), "hello world");
        assert_eq!(crdt.text(), "hello world");

        buf.remove_range(5..11);
        assert_eq!(buf.rope().to_string(), "hello");
        assert_eq!(crdt.text(), "hello");

        buf.replace_range(0..5, "hi");
        assert_eq!(buf.rope().to_string(), "hi");
        assert_eq!(crdt.text(), "hi");
    }

    // ---------- Lot 1: groups, atomic edits, typing, save marker, anchors ----------

    use brouillon_core::contract::TextEdit;

    fn text(b: &Buffer) -> String {
        b.rope().to_string()
    }

    /// BK-001: 50 edits given in any order, one undo restores, redo reapplies.
    #[test]
    fn bk001_fifty_edits_one_group() {
        let src: String = (0..50).map(|i| format!("line{i:02}\n")).collect();
        let mut edits: Vec<TextEdit> = (0..50).map(|i| TextEdit::new(i * 7, i * 7 + 4, "LN")).collect();
        edits.reverse(); // any input order
        let set = EditSet::from_unsorted(edits).unwrap();
        let expect = set.apply_to_str(&src).unwrap();

        let mut b = Buffer::from_str(&src);
        b.apply_edits_with(
            &set,
            EditMeta { selection_before: Some(vec![Region::caret(3)]), selection_after: Some(vec![Region::caret(9)]), ..Default::default() },
        )
        .unwrap();
        assert_eq!(text(&b), expect, "agrees with the reference semantics");
        assert_eq!(b.undo_restoring(), Some(Some(vec![Region::caret(3)])));
        assert_eq!(text(&b), src);
        assert!(!b.can_undo(), "exactly one group");
        assert_eq!(b.redo_restoring(), Some(Some(vec![Region::caret(9)])));
        assert_eq!(text(&b), expect);
        assert!(b.undo() && text(&b) == src);
    }

    #[test]
    fn invalid_edit_set_leaves_buffer_untouched() {
        let mut b = Buffer::from_str("héllo");
        let overlap = EditSet(vec![TextEdit::new(0, 3, "a"), TextEdit::new(2, 4, "b")]);
        assert!(b.apply_edits(&overlap).is_err());
        assert!(b.apply_edits(&EditSet(vec![TextEdit::insert(0, "ok"), TextEdit::delete(1, 99)])).is_err());
        assert!(b.apply_edits(&EditSet(vec![TextEdit::delete(2, 3)])).is_err(), "inside 'é'");
        assert_eq!(text(&b), "héllo");
        assert!(!b.is_dirty() && !b.can_undo());
        b.apply_edits(&EditSet::new()).unwrap();
        assert!(!b.is_dirty() && !b.can_undo(), "empty set is a no-op");
    }

    #[test]
    fn transaction_groups_and_nests() {
        let mut b = Buffer::from_str("abc");
        b.transaction(|t| {
            t.insert_at(0, "1");
            t.transaction(|t| t.insert_at(4, "2"));
            t.remove_range(1..2);
        });
        assert_eq!(text(&b), "1bc2");
        assert!(b.undo() && text(&b) == "abc" && !b.can_undo());
        assert!(b.redo() && text(&b) == "1bc2");
        b.transaction(|_| {});
        assert_eq!(b.history.undo_depth(), 1, "empty transaction pushes nothing");
    }

    #[test]
    fn origin_tag_on_group() {
        let mut b = Buffer::from_str("x");
        b.insert_at(1, "y");
        assert_eq!(b.top_undo_origin(), Some(&Origin::User));
        let set = EditSet(vec![TextEdit::insert(0, "ai ")]);
        b.apply_edits_with(&set, EditMeta { origin: Origin::Ai("claude".into()), ..Default::default() }).unwrap();
        assert_eq!(b.top_undo_origin(), Some(&Origin::Ai("claude".into())));
    }

    #[test]
    fn version_and_hash_track_text() {
        let mut b = Buffer::from_str("abc");
        let (v0, h0) = (b.version(), b.hash());
        assert_eq!(h0, Buffer::from_str("abc").hash(), "hash depends on content only");
        b.insert_at(3, "d");
        assert!(b.version() > v0 && b.hash() != h0);
        let v1 = b.version();
        b.undo();
        assert!(b.version() > v1, "undo is a change too");
        assert_eq!(b.hash(), h0);
    }

    /// BK-002
    #[test]
    fn bk002_typing_coalescing() {
        let mut b = Buffer::from_str("");
        for (i, c) in "abcdefghijklmnopqrst".chars().enumerate() {
            b.typing_insert(i, &c.to_string(), (i as u64) * 100);
        }
        assert_eq!(text(&b), "abcdefghijklmnopqrst");
        assert!(b.undo() && text(&b) == "" && !b.can_undo(), "20 chars = 1 undo");
        assert!(b.redo());

        // typing, caret jump, typing = 2 undos
        let mut b = Buffer::from_str("0123456789");
        b.typing_insert(10, "ab", 0);
        b.typing_insert(12, "c", 10);
        b.typing_insert(0, "X", 20); // caret jump
        b.typing_insert(1, "Y", 30);
        assert!(b.undo());
        assert_eq!(text(&b), "0123456789abc");
        assert!(b.undo() && text(&b) == "0123456789" && !b.can_undo());
    }

    #[test]
    fn typing_breaks_on_pause_enter_boundary_and_command() {
        let mut b = Buffer::from_str("");
        b.typing_insert(0, "a", 0);
        b.typing_insert(1, "b", 1000); // exactly 1 s: still merged
        b.typing_insert(2, "c", 2001); // > 1 s: new group
        assert_eq!(b.history.undo_depth(), 2);

        let mut b = Buffer::from_str("");
        b.typing_insert(0, "ab", 0);
        b.typing_insert(2, "\n", 1); // Enter: own group, closes the run
        b.typing_insert(3, "c", 2);
        assert_eq!(b.history.undo_depth(), 3);

        let mut b = Buffer::from_str("");
        b.typing_insert(0, "ab", 0);
        b.typing_insert(2, " ", 1);
        b.typing_insert(3, "c", 2); // word char after space: new group
        assert_eq!(b.history.undo_depth(), 2);

        let mut b = Buffer::from_str("");
        b.typing_insert(0, "a", 0);
        b.replace_range(0..1, "z"); // non-typing command
        b.typing_insert(1, "b", 5);
        assert_eq!(b.history.undo_depth(), 3);
    }

    #[test]
    fn typing_delete_coalesces_backspace_and_delete() {
        let mut b = Buffer::from_str("hello");
        b.typing_delete(4..5, false, 0);
        b.typing_delete(3..4, false, 10);
        b.typing_delete(2..3, false, 20);
        assert_eq!(text(&b), "he");
        assert!(b.undo() && text(&b) == "hello" && !b.can_undo());
        let mut b = Buffer::from_str("hello");
        b.typing_delete(1..2, true, 0);
        b.typing_delete(1..2, true, 10);
        assert_eq!(text(&b), "hlo");
        assert!(b.undo() && text(&b) == "hello" && !b.can_undo());
    }

    /// BK-003
    #[test]
    fn bk003_save_marker_undo_to_save_and_all() {
        let mut b = Buffer::from_str("base");
        b.insert_at(4, "1");
        b.mark_saved();
        assert!(b.is_at_save() && !b.is_dirty());
        let saved = text(&b);
        b.insert_at(0, "a");
        b.insert_at(0, "b");
        b.typing_insert(0, "c", 0);
        assert!(b.is_dirty());
        assert!(b.undo_to_save());
        assert_eq!(text(&b), saved);
        assert!(!b.is_dirty(), "back at the saved text is clean");
        assert!(b.can_undo(), "history survives the save");
        assert!(b.can_redo());
        assert!(!b.undo_to_save(), "already there");
        // go past the save point, then 'undo to save' travels forward
        assert!(b.undo());
        assert_eq!(text(&b), "base");
        assert!(b.undo_to_save());
        assert_eq!(text(&b), saved);
        assert_eq!(b.undo_all(), 1);
        assert_eq!(text(&b), "base");
        assert!(!b.can_undo());
    }

    #[test]
    fn typing_after_save_does_not_merge_into_saved_group() {
        let mut b = Buffer::from_str("");
        b.typing_insert(0, "ab", 0);
        b.mark_saved();
        b.typing_insert(2, "c", 10);
        assert!(b.undo_to_save());
        assert_eq!(text(&b), "ab");
    }

    #[test]
    fn exceeding_cap_drops_oldest_and_keeps_consistent() {
        let mut b = Buffer::from_str("");
        b.set_undo_limits(1000, 1_000);
        for i in 0..50 {
            b.insert_at(0, &format!("{i:0>100}"));
        }
        assert!(b.undo_bytes() <= 1_000 + 300);
        let depth = b.history.undo_depth();
        assert!(depth >= 1 && depth < 50);
        let mut n = 0;
        while b.undo() {
            n += 1;
        }
        assert_eq!(n, depth);
        assert!(!b.can_undo());
        assert_eq!(b.undo_bytes(), 0);
        b.set_undo_limits(1000, 1_000);
        b.mark_saved();
        for _ in 0..50 {
            b.insert_at(0, &"y".repeat(100));
        }
        assert!(!b.undo_to_save(), "saved state was dropped from history");
    }

    /// BK-004
    #[test]
    fn bk004_replace_all_as_local_edits() {
        let src: String = (0..100_000).map(|i| if i % 1000 == 0 { "foo bar\n" } else { "plain line\n" }).collect();
        let new_text = src.replace("foo", "baz");
        let mut b = Buffer::from_str(&src);
        let caret = b.add_anchor(src.find("plain").unwrap(), Bias::Left);
        let n = b.apply_text_diff(&new_text);
        assert_eq!(n, 100, "edit count == match count");
        assert_eq!(text(&b), new_text);
        assert_eq!(b.history.undo_depth(), 1, "one undo step");
        assert_eq!(b.anchor_line(caret), Some(1));
        assert!(b.undo() && text(&b) == src);
    }

    /// BK-005
    #[test]
    fn bk005_anchor_follows_inserted_lines_and_deleted_line_clamps() {
        let mut b = Buffer::from_str("l0\nl1\nl2\nl3\n");
        let m = b.add_anchor(b.rope().byte_of_line(2), Bias::Left);
        assert_eq!(b.anchor_line(m), Some(2));
        let k = 3;
        let ins = "new\n".repeat(k);
        b.apply_edits(&EditSet(vec![TextEdit::insert(0, ins)])).unwrap();
        assert_eq!(b.anchor_line(m), Some(2 + k));
        // delete the anchor's line: clamps to the start of the deleted range
        let r = b.line_byte_range(2 + k).unwrap();
        b.apply_edits(&EditSet(vec![TextEdit::delete(r.start, r.end)])).unwrap();
        assert_eq!(b.anchor_pos(m), Some(r.start));
        assert_eq!(b.anchor_line(m), Some(2 + k), "now points at the following line");
        b.undo();
        assert_eq!(b.anchor_line(m), Some(2 + k), "restored text keeps it on that line");
        b.remove_anchor(m);
        assert_eq!(b.anchor_pos(m), None);
    }

    #[test]
    fn anchors_bias_on_insert_and_undo_redo() {
        let mut b = Buffer::from_str("abcdef");
        let l = b.add_anchor(3, Bias::Left);
        let r = b.add_anchor(3, Bias::Right);
        b.insert_at(3, "XY");
        assert_eq!((b.anchor_pos(l), b.anchor_pos(r)), (Some(3), Some(5)));
        b.undo();
        assert_eq!((b.anchor_pos(l), b.anchor_pos(r)), (Some(3), Some(3)));
        b.redo();
        assert_eq!(b.anchor_pos(r), Some(5));
    }

    /// Redo then new edit truncates redo; group-level dirty tracking.
    #[test]
    fn new_group_after_undo_clears_redo_and_dirty_follows_save() {
        let mut b = Buffer::from_str("a");
        b.insert_at(1, "b");
        b.mark_saved();
        b.insert_at(2, "c");
        assert!(b.is_dirty());
        b.undo();
        assert!(!b.is_dirty());
        b.insert_at(0, "z");
        assert!(!b.can_redo());
        assert!(b.is_dirty());
    }
}
