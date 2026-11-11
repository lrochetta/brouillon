//! Lazy text store: read-only base file (memory-mapped OR buffered pread/ReadFile with a small chunk LRU) + piece
//! table (add buffer + two-level piece list) + sparse lazy line index (checkpoints every `block` bytes, built
//! by a background thread, exact on demand). Memory is proportional to the viewed region plus the edits.
//!
//! Offsets are bytes. All edit positions must be UTF-8 boundaries of the *document*. Lines end at `\n`
//! (a CRLF line keeps its `\r`), same convention as the reference model and crop.
use crate::store::{StoreError, TextStore};
use memmap2::Mmap;
use std::fs::File;
use std::io::{self, Write};
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering::Relaxed};
use std::sync::{Arc, Mutex, RwLock, Weak};
use std::time::SystemTime;

const UNKNOWN: u64 = u64::MAX;

// Persistence side (streaming save, piece journal, external-change detection): child module so it can use the
// private piece table; the only coupling to this file is the handful of items it touches.
#[path = "lazy_persist.rs"]
pub mod persist;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    /// Read-only `memmap2` view of the file.
    Mmap,
    /// `seek_read`/`read_at` of 64 KB chunks through a small LRU (viewport) or 1 MB windows (bulk).
    Chunked,
}

#[derive(Clone, Debug)]
pub struct Config {
    pub mode: Mode,
    /// Line-index checkpoint spacing in bytes (64 KB: 16 K entries for 1 GB = 128 KB of RAM).
    pub block: usize,
    /// LRU chunk size and capacity (Chunked mode): 32 x 64 KB = 2 MB.
    pub chunk: usize,
    pub lru_chunks: usize,
    /// Pieces per block of the two-level piece list.
    pub max_pieces_per_block: usize,
    pub background_index: bool,
    /// The first edit waits until the whole base file is validated as UTF-8 (normally already done).
    pub validate_before_edit: bool,
}
impl Default for Config {
    fn default() -> Self {
        Config { mode: Mode::Chunked, block: 64 << 10, chunk: 64 << 10, lru_chunks: 32, max_pieces_per_block: 256, background_index: true, validate_before_edit: true }
    }
}

// ------------------------------------------------------------------------------------------ file access

#[cfg(windows)]
fn read_at(f: &File, buf: &mut [u8], off: u64) -> io::Result<usize> {
    use std::os::windows::fs::FileExt;
    f.seek_read(buf, off)
}
#[cfg(unix)]
fn read_at(f: &File, buf: &mut [u8], off: u64) -> io::Result<usize> {
    use std::os::unix::fs::FileExt;
    f.read_at(buf, off)
}
fn read_full_at(f: &File, buf: &mut [u8], mut off: u64) -> io::Result<usize> {
    let mut done = 0;
    while done < buf.len() {
        let n = read_at(f, &mut buf[done..], off)?;
        if n == 0 {
            break;
        }
        done += n;
        off += n as u64;
    }
    Ok(done)
}

// ------------------------------------------------------------------------------------------ line index

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Utf8 {
    Pending,
    Valid,
    Invalid(u64),
}

struct IdxSt {
    /// cum[i] = number of '\n' before byte i*block. cum.len() = fully-scanned blocks + 1 (+1 more at EOF for a partial tail).
    cum: Vec<u64>,
    scanned: u64,
    cur_nl: u64,
    invalid_at: Option<u64>,
    carry: Vec<u8>, // up to 3 bytes of an incomplete UTF-8 sequence at the end of the last fed data
}

impl IdxSt {
    fn new() -> Self {
        IdxSt { cum: vec![0], scanned: 0, cur_nl: 0, invalid_at: None, carry: Vec::new() }
    }
    fn char_len(lead: u8) -> usize {
        match lead {
            0xC2..=0xDF => 2,
            0xE0..=0xEF => 3,
            0xF0..=0xF4 => 4,
            _ => 1,
        }
    }
    fn validate(&mut self, mut data: &[u8], start: u64) {
        if self.invalid_at.is_some() || data.is_empty() {
            return;
        }
        let mut pos = start;
        if !self.carry.is_empty() {
            let need = Self::char_len(self.carry[0]).saturating_sub(self.carry.len());
            if data.len() < need {
                self.carry.extend_from_slice(data);
                return;
            }
            let mut tmp = std::mem::take(&mut self.carry);
            tmp.extend_from_slice(&data[..need]);
            if std::str::from_utf8(&tmp).is_err() {
                self.invalid_at = Some(pos - (tmp.len() - need) as u64);
                return;
            }
            data = &data[need..];
            pos += need as u64;
        }
        if let Err(e) = std::str::from_utf8(data) {
            if e.error_len().is_none() {
                self.carry = data[e.valid_up_to()..].to_vec();
            } else {
                self.invalid_at = Some(pos + e.valid_up_to() as u64);
            }
        }
    }
    fn feed(&mut self, block: u64, mut data: &[u8]) {
        let start = self.scanned;
        self.validate(data, start);
        while !data.is_empty() {
            let room = (block - self.scanned % block) as usize;
            let take = room.min(data.len());
            self.cur_nl += memchr::memchr_iter(b'\n', &data[..take]).count() as u64;
            self.scanned += take as u64;
            data = &data[take..];
            if self.scanned % block == 0 {
                let last = *self.cum.last().unwrap();
                self.cum.push(last + self.cur_nl);
                self.cur_nl = 0;
            }
        }
    }
    /// EOF: close the partial block, flag a dangling incomplete UTF-8 sequence.
    fn finish(&mut self, block: u64) {
        if self.scanned % block != 0 {
            let last = *self.cum.last().unwrap();
            self.cum.push(last + self.cur_nl);
            self.cur_nl = 0;
        }
        if !self.carry.is_empty() && self.invalid_at.is_none() {
            self.invalid_at = Some(self.scanned - self.carry.len() as u64);
        }
    }
}

pub struct LineIndex {
    st: Mutex<IdxSt>,
    done: AtomicBool,
    pub cancel: AtomicBool,
    block: u64,
}

impl LineIndex {
    fn new(block: usize) -> Self {
        LineIndex { st: Mutex::new(IdxSt::new()), done: AtomicBool::new(false), cancel: AtomicBool::new(false), block: block as u64 }
    }
    fn from_state(block: usize, mut st: IdxSt) -> Self {
        st.finish(block as u64);
        LineIndex { st: Mutex::new(st), done: AtomicBool::new(true), cancel: AtomicBool::new(false), block: block as u64 }
    }
    pub fn is_done(&self) -> bool {
        self.done.load(Relaxed)
    }
    pub fn utf8(&self) -> Utf8 {
        let g = self.st.lock().unwrap();
        match g.invalid_at {
            Some(a) => Utf8::Invalid(a),
            None if self.is_done() => Utf8::Valid,
            None => Utf8::Pending,
        }
    }
    pub fn progress(&self) -> u64 {
        self.st.lock().unwrap().scanned
    }
    /// Scan up to `budget` more bytes from the file. Returns true when the whole file has been scanned.
    fn step(&self, file: &File, len: u64, budget: u64) -> bool {
        let mut g = self.st.lock().unwrap();
        if self.is_done() {
            return true;
        }
        let mut buf = vec![0u8; 1 << 20];
        let mut left = budget;
        while left > 0 && g.scanned < len {
            let want = ((len - g.scanned).min(buf.len() as u64).min(left)) as usize;
            let off = g.scanned;
            let got = read_full_at(file, &mut buf[..want], off).unwrap_or(0);
            if got == 0 {
                // truncated behind our back: stop here, the caller sees short reads
                g.scanned = len;
                break;
            }
            g.feed(self.block, &buf[..got]);
            left -= got as u64;
        }
        if g.scanned >= len {
            g.finish(self.block);
            self.done.store(true, Relaxed);
        }
        self.is_done()
    }
    fn ready_prefix(&self, x: u64) -> bool {
        self.is_done() || self.st.lock().unwrap().cum.len() as u64 > x / self.block
    }
    fn ensure_prefix(&self, file: &File, len: u64, x: u64) {
        while !self.ready_prefix(x) {
            self.step(file, len, 4 << 20);
        }
    }
    fn ensure_nth(&self, file: &File, len: u64, k: u64) {
        loop {
            {
                let g = self.st.lock().unwrap();
                if self.is_done() || *g.cum.last().unwrap() > k {
                    return;
                }
            }
            self.step(file, len, 4 << 20);
        }
    }
}

// ------------------------------------------------------------------------------------------ base file

struct Lru {
    items: Vec<(u64, Arc<Vec<u8>>)>,
}

pub struct BaseFile {
    file: File,
    map: Option<Mmap>,
    pub len: u64,
    cfg: Config,
    lru: Mutex<Lru>,
    pub idx: LineIndex,
    stamp: (u64, Option<SystemTime>),
    pub path: PathBuf,
    /// Identity of the file as opened: len + head/tail 64 KB hash (see `persist::fingerprint`).
    fp: u64,
    /// Opening order: the newest generation of a path is the one with the highest number.
    gen: u64,
    /// Set once the file changed in place under us: every read is refused until the user chooses.
    stale: AtomicBool,
    /// Bytes whose read was refused because the base is stale.
    pub refused_reads: AtomicU64,
    /// Bytes requested but not delivered (file truncated/changed behind our back).
    pub short_reads: AtomicU64,
}

impl BaseFile {
    fn open(path: &Path, cfg: &Config, idx: Option<LineIndex>) -> io::Result<Arc<BaseFile>> {
        let file = File::open(path)?;
        let meta = file.metadata()?;
        let len = meta.len();
        let map = if cfg.mode == Mode::Mmap && len > 0 {
            // SAFETY: read-only view. Windows refuses truncation of a mapped file (os error 1224); in-place
            // external writes are visible (detected by `externally_changed`); on Unix a truncation can SIGBUS.
            Some(unsafe { Mmap::map(&file)? })
        } else {
            None
        };
        let idx = idx.unwrap_or_else(|| LineIndex::new(cfg.block));
        let fp = persist::fingerprint(&file, len);
        Ok(Arc::new(BaseFile {
            file,
            map,
            len,
            cfg: cfg.clone(),
            lru: Mutex::new(Lru { items: Vec::new() }),
            idx,
            stamp: (len, meta.modified().ok()),
            path: path.to_path_buf(),
            fp,
            gen: persist::next_gen(),
            stale: AtomicBool::new(false),
            refused_reads: AtomicU64::new(0),
            short_reads: AtomicU64::new(0),
        }))
    }
    pub fn externally_changed(&self) -> io::Result<bool> {
        let m = std::fs::metadata(&self.path)?;
        Ok(m.len() != self.stamp.0 || m.modified().ok() != self.stamp.1)
    }
    pub fn is_stale(&self) -> bool {
        self.stale.load(Relaxed)
    }
    /// Cheap consistency probe on the open handle (one fstat): the file we are reading from still has the
    /// length and mtime it had when opened. A mismatch means someone wrote into it in place.
    fn handle_ok(&self) -> bool {
        if self.is_stale() {
            return false;
        }
        match self.file.metadata() {
            Ok(m) if m.len() == self.stamp.0 && m.modified().ok() == self.stamp.1 => true,
            _ => {
                self.stale.store(true, Relaxed);
                false
            }
        }
    }
    fn lru_has(&self, ci: u64) -> bool {
        self.lru.lock().unwrap().items.iter().any(|(k, _)| *k == ci)
    }
    fn chunk(&self, ci: u64) -> Arc<Vec<u8>> {
        {
            let mut l = self.lru.lock().unwrap();
            if let Some(i) = l.items.iter().position(|(k, _)| *k == ci) {
                let it = l.items.remove(i);
                let a = it.1.clone();
                l.items.insert(0, it);
                return a;
            }
        }
        let cs = self.cfg.chunk as u64;
        let off = ci * cs;
        let n = cs.min(self.len.saturating_sub(off)) as usize;
        let mut v = vec![0u8; n];
        let got = read_full_at(&self.file, &mut v, off).unwrap_or(0);
        v.truncate(got);
        let a = Arc::new(v);
        let mut l = self.lru.lock().unwrap();
        l.items.insert(0, (ci, a.clone()));
        l.items.truncate(self.cfg.lru_chunks);
        a
    }
    /// Deliver bytes [a,b) in order to `f` (false = stop). `bulk`: bypass the LRU (streaming Find/Save).
    fn visit(&self, a: u64, b: u64, bulk: bool, f: &mut dyn FnMut(&[u8]) -> bool) -> bool {
        let b = b.min(self.len);
        if a >= b {
            return true;
        }
        if self.is_stale() {
            self.refused_reads.fetch_add(b - a, Relaxed);
            return true;
        }
        if let Some(m) = &self.map {
            return f(&m[a as usize..b as usize]);
        }
        let mut p = a;
        if bulk {
            let mut buf = vec![0u8; ((b - a).min(1 << 20)) as usize];
            while p < b {
                if !self.handle_ok() {
                    self.refused_reads.fetch_add(b - p, Relaxed);
                    return true;
                }
                let n = (b - p).min(buf.len() as u64) as usize;
                let got = read_full_at(&self.file, &mut buf[..n], p).unwrap_or(0);
                if got < n {
                    self.short_reads.fetch_add((n - got) as u64, Relaxed);
                }
                if got == 0 {
                    return true;
                }
                if !f(&buf[..got]) {
                    return false;
                }
                p += got as u64;
            }
        } else {
            let cs = self.cfg.chunk as u64;
            while p < b {
                let ci = p / cs;
                if !self.lru_has(ci) && !self.handle_ok() {
                    self.refused_reads.fetch_add(b - p, Relaxed);
                    return true;
                }
                let c = self.chunk(ci);
                let s = (p - ci * cs) as usize;
                let e = ((b - ci * cs) as usize).min(c.len());
                if s >= e {
                    self.short_reads.fetch_add(b - p, Relaxed);
                    return true;
                }
                if !f(&c[s..e]) {
                    return false;
                }
                p = ci * cs + e as u64;
            }
        }
        true
    }
    /// '\n' count in [0, x).
    fn prefix(&self, x: u64) -> u64 {
        let x = x.min(self.len);
        self.idx.ensure_prefix(&self.file, self.len, x);
        let b = x / self.idx.block;
        let base = self.idx.st.lock().unwrap().cum[b as usize];
        let mut n = 0u64;
        self.visit(b * self.idx.block, x, false, &mut |c| {
            n += memchr::memchr_iter(b'\n', c).count() as u64;
            true
        });
        base + n
    }
    /// Byte offset of the k-th (0-based) '\n' of the file.
    fn nth_newline(&self, k: u64) -> Option<u64> {
        self.idx.ensure_nth(&self.file, self.len, k);
        let (b, base) = {
            let g = self.idx.st.lock().unwrap();
            if *g.cum.last().unwrap() <= k {
                return None; // fewer than k+1 newlines in the whole file
            }
            let b = g.cum.partition_point(|&c| c <= k) - 1;
            (b as u64, g.cum[b])
        };
        let start = b * self.idx.block;
        let mut want = k - base;
        let mut found = None;
        let mut off = start;
        self.visit(start, (start + self.idx.block).min(self.len), false, &mut |c| {
            for p in memchr::memchr_iter(b'\n', c) {
                if want == 0 {
                    found = Some(off + p as u64);
                    return false;
                }
                want -= 1;
            }
            off += c.len() as u64;
            true
        });
        found
    }
}

// ------------------------------------------------------------------------------------------ piece table

/// Lazily resolved newline count (atomic so that pieces and blocks are `Sync`; relaxed: it is a pure cache of a
/// value that is a function of immutable bytes, so racing writers store the same number).
#[derive(Debug)]
pub struct NlCache(AtomicU64);
impl NlCache {
    fn new(v: u64) -> Self {
        NlCache(AtomicU64::new(v))
    }
    fn get(&self) -> u64 {
        self.0.load(Relaxed)
    }
    fn set(&self, v: u64) {
        self.0.store(v, Relaxed)
    }
}
impl Clone for NlCache {
    fn clone(&self) -> Self {
        NlCache::new(self.get())
    }
}

/// A slice of one source (add buffer or a base generation) that is part of the document.
#[derive(Clone, Debug)]
pub struct Piece {
    /// 0 = add buffer, i>0 = bases[i-1]
    pub src: u16,
    pub start: u64,
    pub len: u64,
    /// '\n' count, UNKNOWN until resolved (lazy for base pieces: resolving needs the line index)
    nl: NlCache,
}

#[derive(Debug, Clone)]
struct Block {
    pieces: Vec<Piece>,
    len: u64,
    nl: NlCache, // cached sum, UNKNOWN if stale/unresolved
}

/// One recorded edit as piece operations: at `pos`, `removed` pieces left the document and `inserted` pieces
/// entered it. Text is never copied (the bytes live in the add buffer / base files). Used by the store's own
/// undo log and, additively, by the engine's `Buffer` history (see `LazyStore::edit`).
#[derive(Clone, Debug)]
pub struct EditRec {
    pub pos: u64,
    pub removed: Vec<Piece>,
    pub inserted: Vec<Piece>,
}
impl EditRec {
    pub fn removed_len(&self) -> u64 {
        self.removed.iter().map(|p| p.len).sum()
    }
    pub fn inserted_len(&self) -> u64 {
        self.inserted.iter().map(|p| p.len).sum()
    }
    /// The record that undoes this one (and whose swapped record is this one).
    pub fn swapped(&self) -> EditRec {
        EditRec { pos: self.pos, removed: self.inserted.clone(), inserted: self.removed.clone() }
    }
    /// Add-buffer bytes this record keeps alive (for a history byte budget).
    pub fn add_bytes(&self) -> usize {
        self.removed.iter().chain(self.inserted.iter()).filter(|p| p.src == 0).map(|p| p.len as usize).sum()
    }
    /// Rough heap size of the record itself.
    pub fn heap_bytes(&self) -> usize {
        std::mem::size_of::<EditRec>() + (self.removed.len() + self.inserted.len()) * std::mem::size_of::<Piece>()
    }
    /// Every piece of the record (for remapping after `compact_add`, or for base-generation bookkeeping).
    pub fn pieces_mut(&mut self) -> impl Iterator<Item = &mut Piece> {
        self.removed.iter_mut().chain(self.inserted.iter_mut())
    }
    pub fn pieces(&self) -> impl Iterator<Item = &Piece> {
        self.removed.iter().chain(self.inserted.iter())
    }
}

/// Append-only add buffer, shared by every clone of a store. Pieces refer to absolute offsets, so a clone made
/// before an append keeps working (it never references the new bytes). Readers copy out under a short read lock,
/// so a long consumer (Save, Find) never blocks typing.
pub struct AddBuf {
    v: RwLock<Vec<u8>>,
}
impl AddBuf {
    fn new() -> Arc<AddBuf> {
        Arc::new(AddBuf { v: RwLock::new(Vec::new()) })
    }
    fn push(&self, bytes: &[u8]) -> u64 {
        let mut g = self.v.write().unwrap();
        let s = g.len() as u64;
        g.extend_from_slice(bytes);
        s
    }
    pub fn len(&self) -> usize {
        self.v.read().unwrap().len()
    }
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
    fn capacity(&self) -> usize {
        self.v.read().unwrap().capacity()
    }
    fn with<R>(&self, a: u64, b: u64, f: impl FnOnce(&[u8]) -> R) -> R {
        let g = self.v.read().unwrap();
        f(&g[a as usize..b as usize])
    }
}

#[derive(Debug, Default, Clone, Copy)]
pub struct Stats {
    pub pieces: usize,
    pub blocks: usize,
    pub add_bytes: usize,
    pub undo_records: usize,
    pub index_bytes: usize,
    pub lru_bytes: usize,
}

/// Answer of [`LazyStore::line_start_ready`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LineLookup {
    Found(u64),
    /// The document has fewer lines than asked for.
    PastEnd,
    /// The background index has not reached that line yet.
    NotReady,
}

/// Immutable `Arc` snapshot of a [`LazyStore`] for worker threads (Find, session writer, save): `Send + Sync`,
/// cloning is a pointer copy, creating one (`LazyStore::snapshot`) is O(blocks). The piece list is never
/// mutated after the snapshot is taken (copy-on-write blocks, append-only add buffer), so reading it takes no
/// lock; the only shared locks left are inside the base file (chunk LRU, line-index state), held for
/// microseconds, and the add buffer's read lock held during a memcpy.
#[derive(Clone, Debug)]
pub struct StoreSnapshot(Arc<LazyStore>);
impl std::ops::Deref for StoreSnapshot {
    type Target = LazyStore;
    fn deref(&self) -> &LazyStore {
        &self.0
    }
}

pub struct LazyStore {
    cfg: Config,
    add: Arc<AddBuf>,
    bases: Vec<Option<Arc<BaseFile>>>,
    blocks: Vec<Arc<Block>>,
    len: u64,
    version: u64,
    saved_version: u64,
    undo: Vec<Vec<EditRec>>,
    redo: Vec<Vec<EditRec>>,
    open_group: Option<Vec<EditRec>>,
    bom: bool,
}

/// Cheap clone (O(blocks) pointer copies; the add buffer, the base files and the piece blocks are shared and
/// copy-on-write), so an editor that keeps one immutable document value per keystroke can clone per edit. The
/// store's own undo/redo logs are NOT cloned: a clone starts with empty logs (the engine's `Buffer` keeps its
/// own history of `EditRec`s).
impl Clone for LazyStore {
    fn clone(&self) -> Self {
        LazyStore {
            cfg: self.cfg.clone(),
            add: self.add.clone(),
            bases: self.bases.clone(),
            blocks: self.blocks.clone(),
            len: self.len,
            version: self.version,
            saved_version: self.saved_version,
            undo: Vec::new(),
            redo: Vec::new(),
            open_group: None,
            bom: self.bom,
        }
    }
}

impl std::fmt::Debug for LazyStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LazyStore").field("len", &self.len).field("blocks", &self.blocks.len()).field("version", &self.version).finish_non_exhaustive()
    }
}

fn new_block() -> Arc<Block> {
    Arc::new(Block { pieces: Vec::new(), len: 0, nl: NlCache::new(0) })
}

impl LazyStore {
    /// Background line-index + UTF-8 scan. The thread holds only a weak reference between steps: when the last
    /// owner of the base (the store, its clones and snapshots) goes away the thread ends by itself.
    fn spawn_scan(base: &Arc<BaseFile>) -> io::Result<()> {
        let w: Weak<BaseFile> = Arc::downgrade(base);
        std::thread::Builder::new().name("lazy-index".into()).spawn(move || loop {
            let Some(b) = w.upgrade() else { break };
            if b.idx.cancel.load(Relaxed) || b.idx.step(&b.file, b.len, 4 << 20) {
                break;
            }
        })?;
        Ok(())
    }

    /// Open without reading the file (O(1)): the line index and the UTF-8 validation run in a background thread.
    pub fn open(path: &Path, cfg: Config) -> io::Result<Self> {
        let mut head = [0u8; 4];
        let n = read_full_at(&File::open(path)?, &mut head, 0)?;
        let head = &head[..n];
        if head.starts_with(&[0xFF, 0xFE]) || head.starts_with(&[0xFE, 0xFF]) || head.starts_with(&[0, 0, 0xFE, 0xFF]) {
            return Err(io::Error::new(io::ErrorKind::Unsupported, "UTF-16/32: use the decode path (rope)"));
        }
        let bom = head.starts_with(&[0xEF, 0xBB, 0xBF]);
        let base = BaseFile::open(path, &cfg, None)?;
        if cfg.background_index {
            Self::spawn_scan(&base)?;
        }
        let bom_len = if bom { 3 } else { 0 };
        let mut s = LazyStore { cfg, add: AddBuf::new(), bases: vec![Some(base.clone())], blocks: vec![new_block()], len: 0, version: 0, saved_version: 0, undo: vec![], redo: vec![], open_group: None, bom };
        if base.len > bom_len {
            let p = Piece { src: 1, start: bom_len, len: base.len - bom_len, nl: NlCache::new(UNKNOWN) };
            s.blocks[0] = Arc::new(Block { len: p.len, nl: NlCache::new(UNKNOWN), pieces: vec![p] });
            s.len = base.len - bom_len;
        }
        Ok(s)
    }

    pub fn utf8_status(&self) -> Utf8 {
        let mut pending = false;
        for b in self.bases.iter().flatten() {
            match b.idx.utf8() {
                Utf8::Invalid(a) => return Utf8::Invalid(a),
                Utf8::Pending => pending = true,
                Utf8::Valid => {}
            }
        }
        if pending {
            Utf8::Pending
        } else {
            Utf8::Valid
        }
    }
    pub fn index_done(&self) -> bool {
        self.bases.iter().flatten().all(|b| b.idx.is_done())
    }
    pub fn index_progress(&self) -> (u64, u64) {
        self.main_base().map(|b| (b.idx.progress(), b.len)).unwrap_or((0, 0))
    }
    /// Block until the background scan is finished (drives it on the calling thread if needed).
    pub fn wait_index(&self) {
        for b in self.bases.iter().flatten() {
            while !b.idx.step(&b.file, b.len, 4 << 20) {}
        }
    }
    pub fn externally_changed(&self) -> io::Result<bool> {
        match self.main_base() {
            Some(b) => b.externally_changed(),
            None => Ok(false),
        }
    }
    pub fn short_reads(&self) -> u64 {
        self.bases.iter().flatten().map(|b| b.short_reads.load(Relaxed)).sum()
    }
    pub fn stats(&self) -> Stats {
        let mut s = Stats { blocks: self.blocks.len(), add_bytes: self.add.capacity(), ..Default::default() };
        s.pieces = self.blocks.iter().map(|b| b.pieces.len()).sum();
        s.undo_records = self.undo.iter().chain(self.redo.iter()).map(|g| g.len()).sum();
        for b in self.bases.iter().flatten() {
            s.index_bytes += b.idx.st.lock().unwrap().cum.capacity() * 8;
            s.lru_bytes += b.lru.lock().unwrap().items.iter().map(|(_, a)| a.len()).sum::<usize>();
        }
        s
    }

    // ---- low level ------------------------------------------------------------------------------

    fn base(&self, src: u16) -> &Arc<BaseFile> {
        self.bases[src as usize - 1].as_ref().expect("piece refers to a collected base")
    }
    fn src_visit(&self, p: &Piece, a: u64, b: u64, bulk: bool, f: &mut dyn FnMut(&[u8]) -> bool) -> bool {
        if p.src == 0 {
            // copy out of the shared add buffer (short read lock) so `f` may run long
            let (mut pos, end) = (p.start + a, p.start + b);
            let mut stack = [0u8; 512];
            let mut heap: Vec<u8> = Vec::new();
            while pos < end {
                let n = (end - pos).min(64 << 10);
                if n as usize <= stack.len() {
                    self.add.with(pos, pos + n, |s| stack[..s.len()].copy_from_slice(s));
                    if !f(&stack[..n as usize]) {
                        return false;
                    }
                } else {
                    heap.clear();
                    self.add.with(pos, pos + n, |s| heap.extend_from_slice(s));
                    if !f(&heap) {
                        return false;
                    }
                }
                pos += n;
            }
            true
        } else {
            self.base(p.src).visit(p.start + a, p.start + b, bulk, f)
        }
    }
    /// '\n' in the piece-relative range [a,b).
    fn count_nl_range(&self, p: &Piece, a: u64, b: u64) -> u64 {
        if a >= b {
            return 0;
        }
        if p.src == 0 {
            self.add.with(p.start + a, p.start + b, |s| memchr::memchr_iter(b'\n', s).count() as u64)
        } else {
            let bf = self.base(p.src);
            bf.prefix(p.start + b) - bf.prefix(p.start + a)
        }
    }
    fn piece_nl(&self, p: &Piece) -> u64 {
        let k = p.nl.get();
        if k != UNKNOWN {
            return k;
        }
        let k = self.count_nl_range(p, 0, p.len);
        p.nl.set(k);
        k
    }
    fn block_nl(&self, b: &Block) -> u64 {
        let k = b.nl.get();
        if k != UNKNOWN {
            return k;
        }
        let k = b.pieces.iter().map(|p| self.piece_nl(p)).sum();
        b.nl.set(k);
        k
    }
    fn block_nl_if_known(&self, b: &Block) -> Option<u64> {
        let k = b.nl.get();
        if k != UNKNOWN {
            return Some(k);
        }
        let mut s = 0;
        for p in &b.pieces {
            let n = p.nl.get();
            if n == UNKNOWN {
                return None;
            }
            s += n;
        }
        b.nl.set(s);
        Some(s)
    }

    fn locate(&self, pos: u64) -> (usize, usize, u64) {
        assert!(pos <= self.len, "position {pos} past the end {}", self.len);
        let mut acc = 0u64;
        let last = self.blocks.len() - 1;
        for (bi, b) in self.blocks.iter().enumerate() {
            if pos < acc + b.len || bi == last {
                let mut a2 = acc;
                for (pi, p) in b.pieces.iter().enumerate() {
                    if pos < a2 + p.len {
                        return (bi, pi, pos - a2);
                    }
                    a2 += p.len;
                }
                return (bi, b.pieces.len(), 0);
            }
            acc += b.len;
        }
        unreachable!()
    }

    /// Make `pos` a piece boundary; returns (block, index) of the piece that starts at `pos` (or the end slot).
    fn split_at(&mut self, pos: u64) -> (usize, usize) {
        let (bi, pi, off) = self.locate(pos);
        if off == 0 {
            return (bi, pi);
        }
        let p = self.blocks[bi].pieces[pi].clone();
        let known = p.nl.get();
        let (ln, rn) = if known == UNKNOWN {
            (UNKNOWN, UNKNOWN)
        } else {
            let l = self.count_nl_range(&p, 0, off);
            (l, known - l)
        };
        let left = Piece { src: p.src, start: p.start, len: off, nl: NlCache::new(ln) };
        let right = Piece { src: p.src, start: p.start + off, len: p.len - off, nl: NlCache::new(rn) };
        let b = Arc::make_mut(&mut self.blocks[bi]);
        b.pieces[pi] = left;
        b.pieces.insert(pi + 1, right);
        (bi, pi + 1)
    }

    fn remove_range(&mut self, pos: u64, end: u64) -> Vec<Piece> {
        let mut removed = Vec::new();
        if pos >= end {
            return removed;
        }
        self.split_at(end);
        self.split_at(pos);
        let (mut bi, mut pi, off) = self.locate(pos);
        debug_assert_eq!(off, 0);
        let mut remaining = end - pos;
        while remaining > 0 {
            if pi >= self.blocks[bi].pieces.len() {
                bi += 1;
                pi = 0;
                continue;
            }
            let b = Arc::make_mut(&mut self.blocks[bi]);
            let p = b.pieces.remove(pi);
            b.len -= p.len;
            b.nl.set(UNKNOWN);
            remaining -= p.len;
            removed.push(p);
        }
        self.blocks.retain(|b| !b.pieces.is_empty());
        if self.blocks.is_empty() {
            self.blocks.push(new_block());
        }
        removed
    }

    fn mergeable(a: &Piece, b: &Piece) -> bool {
        a.src == b.src && a.start + a.len == b.start
    }

    fn insert_pieces(&mut self, pos: u64, ps: Vec<Piece>) {
        if ps.is_empty() {
            return;
        }
        let (bi, pi) = self.split_at(pos);
        let total: u64 = ps.iter().map(|p| p.len).sum();
        let n = ps.len();
        let b = Arc::make_mut(&mut self.blocks[bi]);
        for (i, p) in ps.into_iter().enumerate() {
            b.pieces.insert(pi + i, p);
        }
        b.len += total;
        b.nl.set(UNKNOWN);
        // merge neighbours (typing runs, undo restoring a deleted range)
        let lo = pi.saturating_sub(1);
        let mut i = lo;
        let mut hi = pi + n;
        while i + 1 < b.pieces.len() && i < hi {
            if Self::mergeable(&b.pieces[i], &b.pieces[i + 1]) {
                let (x, y) = (b.pieces[i].nl.get(), b.pieces[i + 1].nl.get());
                let nl = if x == UNKNOWN || y == UNKNOWN { UNKNOWN } else { x + y };
                let add = b.pieces[i + 1].len;
                b.pieces[i].len += add;
                b.pieces[i].nl.set(nl);
                b.pieces.remove(i + 1);
                hi = hi.saturating_sub(1);
            } else {
                i += 1;
            }
        }
        if b.pieces.len() > self.cfg.max_pieces_per_block {
            let mid = b.pieces.len() / 2;
            let tail: Vec<Piece> = b.pieces.split_off(mid);
            let tlen: u64 = tail.iter().map(|p| p.len).sum();
            b.len -= tlen;
            b.nl.set(UNKNOWN);
            self.blocks.insert(bi + 1, Arc::new(Block { pieces: tail, len: tlen, nl: NlCache::new(UNKNOWN) }));
        }
    }

    // ---- reading --------------------------------------------------------------------------------

    pub fn visit_range(&self, r: Range<u64>, bulk: bool, f: &mut dyn FnMut(&[u8]) -> bool) {
        if r.start >= r.end {
            return;
        }
        let (mut bi, mut pi, mut off) = self.locate(r.start);
        let mut remaining = r.end.min(self.len) - r.start;
        while remaining > 0 && bi < self.blocks.len() {
            let b = &self.blocks[bi];
            if pi >= b.pieces.len() {
                bi += 1;
                pi = 0;
                continue;
            }
            let p = &b.pieces[pi];
            let n = (p.len - off).min(remaining);
            if !self.src_visit(p, off, off + n, bulk, f) {
                return;
            }
            remaining -= n;
            off = 0;
            pi += 1;
        }
    }
    fn byte_at(&self, pos: u64) -> Option<u8> {
        let mut out = None;
        self.visit_range(pos..pos + 1, false, &mut |c| {
            out = c.first().copied();
            false
        });
        out
    }
    pub fn byte_is_boundary(&self, pos: u64) -> bool {
        pos == 0 || pos >= self.len || self.byte_at(pos).map(|b| b & 0xC0 != 0x80).unwrap_or(true)
    }

    // ---- lines ----------------------------------------------------------------------------------

    /// Number of '\n' in [0,pos).
    pub fn newlines_before(&self, pos: u64) -> u64 {
        let mut acc = 0u64;
        let mut nl = 0u64;
        for b in &self.blocks {
            if acc + b.len <= pos {
                nl += self.block_nl(b);
                acc += b.len;
                continue;
            }
            for p in &b.pieces {
                if acc + p.len <= pos {
                    nl += self.piece_nl(p);
                    acc += p.len;
                } else {
                    return nl + self.count_nl_range(p, 0, pos - acc);
                }
            }
        }
        nl
    }
    fn nth_nl_in_piece(&self, p: &Piece, j: u64) -> Option<u64> {
        // offset (piece-relative) of the j-th '\n' inside a piece that is known to hold more than j
        if p.src == 0 {
            self.add.with(p.start, p.start + p.len, |s| memchr::memchr_iter(b'\n', s).nth(j as usize).map(|x| x as u64))
        } else {
            let bf = self.base(p.src);
            let before = bf.prefix(p.start);
            bf.nth_newline(before + j).filter(|&o| o < p.start + p.len).map(|o| o - p.start)
        }
    }
    /// Start byte of line `n`, or None if the document has fewer lines. Scans the line index only up to line n.
    pub fn line_start(&self, n: u64) -> Option<u64> {
        if n == 0 {
            return Some(0);
        }
        let mut remaining = n; // the n-th '\n' (1-based) ends line n-1
        let mut acc = 0u64;
        for b in &self.blocks {
            if let Some(k) = self.block_nl_if_known(b) {
                if k < remaining {
                    remaining -= k;
                    acc += b.len;
                    continue;
                }
            }
            for p in &b.pieces {
                let pn = p.nl.get();
                if pn != UNKNOWN {
                    if pn < remaining {
                        remaining -= pn;
                        acc += p.len;
                        continue;
                    }
                    return self.nth_nl_in_piece(p, remaining - 1).map(|o| acc + o + 1);
                }
                // unknown base piece: try to find the target inside it without resolving the whole piece
                if let Some(o) = self.nth_nl_in_piece(p, remaining - 1) {
                    return Some(acc + o + 1);
                }
                remaining -= self.piece_nl(p);
                acc += p.len;
            }
        }
        None
    }
    pub fn total_newlines(&self) -> u64 {
        self.blocks.iter().map(|b| self.block_nl(b)).sum()
    }
    /// Some(line count) if it is known without waiting for the background scan.
    pub fn line_count_ready(&self) -> Option<u64> {
        for b in &self.blocks {
            for p in &b.pieces {
                if p.nl.get() == UNKNOWN && p.src != 0 && !self.base(p.src).idx.is_done() {
                    return None;
                }
            }
        }
        Some(self.total_newlines() + 1)
    }

    // ---- editing --------------------------------------------------------------------------------

    fn record(&mut self, rec: EditRec) {
        match self.open_group.as_mut() {
            Some(g) => g.push(rec),
            None => self.undo.push(vec![rec]),
        }
        self.redo.clear();
    }

    /// Raw byte replace WITHOUT logging it (the caller owns the history); returns the piece record.
    fn replace_bytes_rec(&mut self, a: u64, b: u64, bytes: &[u8]) -> EditRec {
        let removed = self.remove_range(a, b);
        let ins = if bytes.is_empty() {
            vec![]
        } else {
            let start = self.add.push(bytes);
            vec![Piece { src: 0, start, len: bytes.len() as u64, nl: NlCache::new(memchr::memchr_iter(b'\n', bytes).count() as u64) }]
        };
        self.insert_pieces(a, ins.clone());
        self.len = self.len - (b - a) + bytes.len() as u64;
        self.version += 1;
        EditRec { pos: a, removed, inserted: ins }
    }

    /// Raw byte replace (hex mode, or after the UTF-8 checks of `replace_str`).
    pub fn replace_bytes(&mut self, a: u64, b: u64, bytes: &[u8]) {
        let rec = self.replace_bytes_rec(a, b, bytes);
        self.record(rec);
    }

    // ---- additive API for an external history (the engine's `Buffer`) ---------------------------------

    /// Checked text edit that returns the piece record instead of logging it in the store's own undo log: the
    /// caller keeps the record in its history and replays it with `apply_rec_inverse` / `apply_rec_forward`.
    pub fn edit(&mut self, a: u64, b: u64, text: &str) -> Result<EditRec, StoreError> {
        self.check_edit(a, b)?;
        Ok(self.replace_bytes_rec(a, b, text.as_bytes()))
    }
    /// Undo `rec` (it must be the most recent un-undone change touching its range, as in any linear history).
    pub fn apply_rec_inverse(&mut self, rec: &EditRec) {
        self.apply_inverse(rec);
        self.version += 1;
    }
    /// Redo `rec`.
    pub fn apply_rec_forward(&mut self, rec: &EditRec) {
        self.apply_forward(rec);
        self.version += 1;
    }
    /// True when invalid UTF-8 was found in a base file: the document is read-only (`edit` refuses).
    pub fn is_read_only(&self) -> bool {
        matches!(self.utf8_status(), Utf8::Invalid(_))
    }
    pub fn has_bom(&self) -> bool {
        self.bom
    }
    /// Bytes held by the add buffer (grows with every insertion, shrinks only through `compact_add`).
    pub fn add_len(&self) -> usize {
        self.add.len()
    }
    /// Path of the newest base generation (the file the document was opened/saved from).
    pub fn base_path(&self) -> Option<PathBuf> {
        self.main_base().map(|b| b.path.clone())
    }
    /// Length of the newest base generation file, in bytes.
    pub fn base_len(&self) -> u64 {
        self.main_base().map_or(0, |b| b.len)
    }
    /// Pieces of the document that live in the add buffer / count of pieces (diagnostics, compaction policy).
    pub fn piece_count(&self) -> usize {
        self.blocks.iter().map(|b| b.pieces.len()).sum()
    }
    /// Add-buffer bytes the DOCUMENT still refers to (the history adds its own).
    pub fn doc_add_bytes(&self) -> usize {
        self.blocks.iter().flat_map(|b| b.pieces.iter()).filter(|p| p.src == 0).map(|p| p.len as usize).sum()
    }

    /// Rebuild the add buffer so it holds only the bytes that the document and the history records (shown through
    /// `history`, which is called twice and must visit the same pieces both times) still refer to; every such piece
    /// is remapped. The new add buffer is a fresh allocation: clones/snapshots made before keep the old one.
    /// Returns the bytes reclaimed.
    pub fn compact_add(&mut self, history: &mut dyn FnMut(&mut dyn FnMut(&mut Piece))) -> usize {
        let mut ranges: Vec<(u64, u64)> = Vec::new();
        for b in &self.blocks {
            for p in b.pieces.iter().filter(|p| p.src == 0) {
                ranges.push((p.start, p.start + p.len));
            }
        }
        history(&mut |p: &mut Piece| {
            if p.src == 0 && p.len > 0 {
                ranges.push((p.start, p.start + p.len));
            }
        });
        ranges.sort_unstable();
        let mut merged: Vec<(u64, u64, u64)> = Vec::new(); // (old_start, old_end, new_start)
        let mut newlen = 0u64;
        for (s, e) in ranges {
            match merged.last_mut() {
                Some(last) if s <= last.1 => {
                    if e > last.1 {
                        newlen += e - last.1;
                        last.1 = e;
                    }
                }
                _ => {
                    merged.push((s, e, newlen));
                    newlen += e - s;
                }
            }
        }
        let old_len = self.add.len();
        let mut nb: Vec<u8> = Vec::with_capacity(newlen as usize);
        for &(s, e, _) in &merged {
            self.add.with(s, e, |bytes| nb.extend_from_slice(bytes));
        }
        let remap = |p: &mut Piece| {
            if p.src == 0 && p.len > 0 {
                let i = merged.partition_point(|m| m.0 <= p.start) - 1;
                p.start = merged[i].2 + (p.start - merged[i].0);
            }
        };
        for b in &mut self.blocks {
            let blk = Arc::make_mut(b);
            blk.pieces.iter_mut().for_each(remap);
        }
        history(&mut |p: &mut Piece| remap(p));
        self.add = Arc::new(AddBuf { v: RwLock::new(nb) });
        old_len.saturating_sub(newlen as usize)
    }

    /// Like `gc_bases` for an external history: `history` shows every piece it still refers to.
    pub fn gc_bases_with(&mut self, history: &mut dyn FnMut(&mut dyn FnMut(&Piece))) {
        let mut used = vec![false; self.bases.len() + 1];
        self.blocks.iter().flat_map(|b| b.pieces.iter()).for_each(|p| used[p.src as usize] = true);
        history(&mut |p: &Piece| used[p.src as usize] = true);
        for (i, b) in self.bases.iter_mut().enumerate() {
            if !used[i + 1] {
                b.take();
            }
        }
    }

    /// Make `path` (a file just written by someone else from this document's bytes, e.g. a backup-aware save)
    /// the new base generation: the document becomes one piece of it, its line index is rebuilt in the background.
    /// Older generations stay alive while pieces/history refer to them (see `gc_bases_with`). Clears the dirty flag.
    pub fn rebase_to_file(&mut self, path: &Path) -> io::Result<()> {
        let mut head = [0u8; 3];
        let n = read_full_at(&File::open(path)?, &mut head, 0)?;
        self.bom = head[..n].starts_with(&[0xEF, 0xBB, 0xBF]);
        let nb = BaseFile::open(path, &self.cfg, None)?;
        if self.cfg.background_index {
            Self::spawn_scan(&nb)?;
        }
        let bom_len = if self.bom { 3 } else { 0 };
        let src = self.push_base(nb.clone());
        self.blocks = vec![new_block()];
        if nb.len > bom_len {
            let p = Piece { src, start: bom_len, len: nb.len - bom_len, nl: NlCache::new(UNKNOWN) };
            self.blocks[0] = Arc::new(Block { len: p.len, nl: NlCache::new(UNKNOWN), pieces: vec![p] });
        }
        self.len = nb.len.saturating_sub(bom_len);
        self.saved_version = self.version;
        Ok(())
    }

    // ---- non-blocking line queries (far jumps while the index is still being built) -----------------

    /// True when the document is exactly one untouched piece of the newest base generation.
    fn is_pristine(&self) -> bool {
        if self.blocks.len() != 1 || self.blocks[0].pieces.len() != 1 {
            return false;
        }
        let p = &self.blocks[0].pieces[0];
        let bom_len = if self.bom { 3 } else { 0 };
        p.src != 0 && p.start == bom_len && self.bases[p.src as usize - 1].as_ref().is_some_and(|b| b.len == p.start + p.len)
    }
    /// The base generation the document mostly reads from: that of its first base piece, else the newest one.
    fn main_base(&self) -> Option<&Arc<BaseFile>> {
        let first = self.blocks.iter().flat_map(|b| b.pieces.iter()).find(|p| p.src != 0);
        match first {
            Some(p) => self.bases[p.src as usize - 1].as_ref(),
            None => self.bases.iter().rev().flatten().next(),
        }
    }
    /// Start byte of line `n` WITHOUT waiting for the background scan: exact when the index already covers it
    /// (or is complete), `NotReady` otherwise (the caller shows an estimate or tries again later).
    pub fn line_start_ready(&self, n: u64) -> LineLookup {
        if n == 0 {
            return LineLookup::Found(0);
        }
        if self.index_done() {
            return match self.line_start(n) {
                Some(x) => LineLookup::Found(x),
                None => LineLookup::PastEnd,
            };
        }
        if self.is_pristine() {
            let covered = self.main_base().is_some_and(|b| *b.idx.st.lock().unwrap().cum.last().unwrap() >= n);
            if covered {
                if let Some(x) = self.line_start(n) {
                    return LineLookup::Found(x);
                }
            }
        }
        LineLookup::NotReady
    }
    /// `(line_count, exact)`: the exact count when it is known without waiting, else an estimate from the
    /// newline density of the part of the file scanned so far.
    pub fn estimated_line_count(&self) -> (u64, bool) {
        if let Some(n) = self.line_count_ready() {
            return (n, true);
        }
        let (nl, scanned) = match self.main_base() {
            Some(b) => {
                let g = b.idx.st.lock().unwrap();
                (*g.cum.last().unwrap() + g.cur_nl, g.scanned)
            }
            None => (0, 0),
        };
        if scanned == 0 {
            return (self.len / 40 + 1, false);
        }
        let est = (nl as f64 / scanned as f64 * self.len as f64) as u64 + 1;
        (est.max(nl + 1), false)
    }
    /// Fraction (0.0..=1.0) of the newest base file scanned by the line-index/UTF-8 pass.
    pub fn index_fraction(&self) -> f64 {
        let (done, len) = match self.main_base() {
            Some(b) => (b.idx.progress(), b.len),
            None => return 1.0,
        };
        if len == 0 || self.index_done() {
            1.0
        } else {
            (done as f64 / len as f64).min(1.0)
        }
    }

    /// Cheap immutable view for worker threads (see [`StoreSnapshot`]).
    pub fn snapshot(&self) -> StoreSnapshot {
        StoreSnapshot(Arc::new(self.clone()))
    }

    fn check_edit(&self, a: u64, b: u64) -> Result<(), StoreError> {
        if a > b || b > self.len {
            return Err(StoreError::OutOfRange);
        }
        if self.is_stale() {
            return Err(StoreError::Io(io::Error::other("the file changed on disk: reload or resolve before editing")));
        }
        if self.cfg.validate_before_edit {
            self.wait_index();
        }
        if let Utf8::Invalid(at) = self.utf8_status() {
            return Err(StoreError::InvalidUtf8(at));
        }
        for p in [a, b] {
            if !self.byte_is_boundary(p) {
                return Err(StoreError::NotCharBoundary(p as usize));
            }
        }
        Ok(())
    }

    fn apply_inverse(&mut self, rec: &EditRec) {
        let il = rec.inserted_len();
        self.remove_range(rec.pos, rec.pos + il);
        self.insert_pieces(rec.pos, rec.removed.clone());
        self.len = self.len - il + rec.removed_len();
    }
    fn apply_forward(&mut self, rec: &EditRec) {
        let rl = rec.removed_len();
        self.remove_range(rec.pos, rec.pos + rl);
        self.insert_pieces(rec.pos, rec.inserted.clone());
        self.len = self.len - rl + rec.inserted_len();
    }

    /// Drop every base generation no piece or history record refers to (frees the old file handle/mapping).
    pub fn gc_bases(&mut self) {
        let mut used = vec![false; self.bases.len() + 1];
        let mut mark = |p: &Piece| used[p.src as usize] = true;
        self.blocks.iter().flat_map(|b| b.pieces.iter()).for_each(&mut mark);
        for g in self.undo.iter().chain(self.redo.iter()).chain(self.open_group.iter()) {
            for r in g {
                r.removed.iter().chain(r.inserted.iter()).for_each(&mut mark);
            }
        }
        for (i, b) in self.bases.iter_mut().enumerate() {
            if !used[i + 1] {
                // Dropping our reference is enough: the scan thread only holds a weak one, so it ends when the
                // last clone/snapshot that still uses the base lets go.
                b.take();
            }
        }
    }
    pub fn clear_history(&mut self) {
        self.undo.clear();
        self.redo.clear();
        self.gc_bases();
    }
}

impl TextStore for LazyStore {
    fn byte_len(&self) -> usize {
        self.len as usize
    }
    fn line_count(&self) -> usize {
        (self.total_newlines() + 1) as usize
    }
    fn byte_of_line(&self, line: usize) -> Option<usize> {
        self.line_start(line as u64).map(|x| x as usize)
    }
    fn line_of_byte(&self, byte: usize) -> usize {
        self.newlines_before(byte as u64) as usize
    }
    fn is_char_boundary(&self, byte: usize) -> bool {
        self.byte_is_boundary(byte as u64)
    }
    fn visit(&self, r: Range<usize>, f: &mut dyn FnMut(&[u8]) -> bool) {
        // bulk (LRU-bypassing) reads only make sense for big ranges
        let bulk = r.len() > (1 << 20);
        self.visit_range(r.start as u64..r.end as u64, bulk, f)
    }
    fn replace_range(&mut self, r: Range<usize>, text: &str) -> Result<(), StoreError> {
        self.check_edit(r.start as u64, r.end as u64)?;
        self.replace_bytes(r.start as u64, r.end as u64, text.as_bytes());
        Ok(())
    }
    fn begin_group(&mut self) {
        self.open_group.get_or_insert_with(Vec::new);
    }
    fn end_group(&mut self) {
        if let Some(g) = self.open_group.take() {
            if !g.is_empty() {
                self.undo.push(g);
            }
        }
    }
    fn undo(&mut self) -> bool {
        let Some(g) = self.undo.pop() else { return false };
        for rec in g.iter().rev() {
            self.apply_inverse(rec);
        }
        self.redo.push(g);
        self.version += 1;
        true
    }
    fn redo(&mut self) -> bool {
        let Some(g) = self.redo.pop() else { return false };
        for rec in g.iter() {
            self.apply_forward(rec);
        }
        self.undo.push(g);
        self.version += 1;
        true
    }
    fn can_undo(&self) -> bool {
        !self.undo.is_empty()
    }
    fn can_redo(&self) -> bool {
        !self.redo.is_empty()
    }
    fn version(&self) -> u64 {
        self.version
    }
    fn is_dirty(&self) -> bool {
        self.version != self.saved_version
    }

    /// Streaming atomic save (see `persist`): temp + fsync + rename, then the saved file becomes the new base
    /// generation. Refuses to overwrite a base that changed on disk (use `save_with` to overrule).
    fn save(&mut self, path: &Path) -> io::Result<()> {
        self.save_with(path, persist::SaveOptions::default()).map(|_| ()).map_err(Into::into)
    }
    fn heap_estimate(&self) -> usize {
        let st = self.stats();
        st.pieces * std::mem::size_of::<Piece>()
            + st.blocks * std::mem::size_of::<Block>()
            + st.add_bytes
            + st.undo_records * (std::mem::size_of::<EditRec>() + 2 * std::mem::size_of::<Piece>())
            + st.index_bytes
            + st.lru_bytes
    }
}

impl LazyStore {
    /// `TextStore::save` for a store whose history lives OUTSIDE (the engine's `Buffer`): same streaming atomic
    /// save, but old base generations are kept; the caller releases them with `gc_bases_with` once it knows
    /// which ones its history still refers to.
    pub fn save_keep_bases(&mut self, path: &Path) -> io::Result<()> {
        // The app's own gate (external.rs) already asked about a changed file; this layer only adds the safe temp
        // name, fsync, rename, rebase and the stale-base refusal.
        let opts = persist::SaveOptions { keep_bases: true, overwrite_changed: true, ..Default::default() };
        self.save_with(path, opts).map(|_| ()).map_err(Into::into)
    }

    /// Register a new base generation in a free slot (or a new one) and return its 1-based `src` id. Slots are
    /// reused so that the `u16` id space cannot run out over thousands of saves.
    fn push_base(&mut self, nb: Arc<BaseFile>) -> u16 {
        match self.bases.iter().position(|b| b.is_none()) {
            Some(i) => {
                self.bases[i] = Some(nb);
                (i + 1) as u16
            }
            None => {
                self.bases.push(Some(nb));
                self.bases.len() as u16
            }
        }
    }
}

/// Read-only `TextStore` view of a snapshot, so the streaming helpers (`count_matches`, `hash_all`, `viewport`)
/// run on it from any thread. Every mutating method refuses (`ReadOnly` / `false`).
impl TextStore for StoreSnapshot {
    fn byte_len(&self) -> usize {
        self.0.byte_len()
    }
    fn line_count(&self) -> usize {
        self.0.line_count()
    }
    fn byte_of_line(&self, line: usize) -> Option<usize> {
        self.0.byte_of_line(line)
    }
    fn line_of_byte(&self, byte: usize) -> usize {
        self.0.line_of_byte(byte)
    }
    fn is_char_boundary(&self, byte: usize) -> bool {
        self.0.is_char_boundary(byte)
    }
    fn visit(&self, r: Range<usize>, f: &mut dyn FnMut(&[u8]) -> bool) {
        self.0.visit(r, f)
    }
    fn replace_range(&mut self, _r: Range<usize>, _text: &str) -> Result<(), StoreError> {
        Err(StoreError::ReadOnly)
    }
    fn begin_group(&mut self) {}
    fn end_group(&mut self) {}
    fn undo(&mut self) -> bool {
        false
    }
    fn redo(&mut self) -> bool {
        false
    }
    fn can_undo(&self) -> bool {
        false
    }
    fn can_redo(&self) -> bool {
        false
    }
    fn version(&self) -> u64 {
        self.0.version
    }
    fn is_dirty(&self) -> bool {
        self.0.is_dirty()
    }
    fn save(&mut self, _path: &Path) -> io::Result<()> {
        Err(io::Error::new(io::ErrorKind::PermissionDenied, "a snapshot is read-only"))
    }
    fn heap_estimate(&self) -> usize {
        self.0.heap_estimate()
    }
}
