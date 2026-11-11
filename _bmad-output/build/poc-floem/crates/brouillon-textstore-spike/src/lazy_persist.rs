//! Persistence side of the lazy store: streaming atomic save (+ backup hook, changed-on-disk gate, read-only and
//! low-disk handling, crash-injection points), external-change detection with a "stale" state, a piece-table
//! journal for autosave / crash recovery that never copies the document, and session-restore helpers.
//!
//! This file is a child module of `lazy.rs` (`#[path]`), so it can use the private piece table directly.
//! Nothing here reads the whole document into memory: every pass streams through `visit_range`.
use super::*;
use std::hash::{BuildHasher, Hasher};
use std::time::UNIX_EPOCH;

// ------------------------------------------------------------------------------------------ hashing

const FNV_OFFSET: u64 = 0xcbf29ce484222325;
const FNV_PRIME: u64 = 0x100000001b3;

fn fnv(mut h: u64, bytes: &[u8]) -> u64 {
    for &b in bytes {
        h ^= b as u64;
        h = h.wrapping_mul(FNV_PRIME);
    }
    h
}

const FP_SPAN: u64 = 64 << 10;

/// Identity of a file extent `[0, len)`: length + FNV of the first and last 64 KB. Cheap (two preads) and enough
/// to tell "same file" from "rewritten/replaced"; it does NOT prove a middle-of-file same-length edit did not
/// happen (the app also compares length + mtime, and a changed handle stat always makes the base stale).
pub(crate) fn fingerprint(file: &File, len: u64) -> u64 {
    let mut h = fnv(FNV_OFFSET, &len.to_le_bytes());
    let head = len.min(FP_SPAN);
    let mut buf = vec![0u8; head as usize];
    let got = read_full_at(file, &mut buf, 0).unwrap_or(0);
    h = fnv(h, &buf[..got]);
    if len > FP_SPAN {
        let tail_start = (len - FP_SPAN).max(FP_SPAN);
        let mut tb = vec![0u8; (len - tail_start) as usize];
        let got = read_full_at(file, &mut tb, tail_start).unwrap_or(0);
        h = fnv(h, &tb[..got]);
    }
    h
}

pub(crate) fn next_gen() -> u64 {
    static G: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
    G.fetch_add(1, Relaxed)
}

fn nonce() -> String {
    format!("{:012x}", std::collections::hash_map::RandomState::new().build_hasher().finish() & 0xffff_ffff_ffff)
}

fn file_name_of(p: &Path) -> String {
    p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "file".into())
}

fn parent_of(p: &Path) -> PathBuf {
    match p.parent() {
        Some(d) if !d.as_os_str().is_empty() => d.to_path_buf(),
        _ => PathBuf::from("."),
    }
}

/// Temp names are `.<name>.brouillon-<12 hex>.tmp` in the target's directory: only these are ever swept.
fn is_our_temp(entry: &str, target_name: &str) -> bool {
    let pre = format!(".{target_name}.brouillon-");
    entry.starts_with(&pre) && entry.ends_with(".tmp") && entry.len() > pre.len() + 4
}

/// Remove the temp files an interrupted save of `target` left behind (call at launch / before a save).
pub fn sweep_orphan_temps(target: &Path) -> usize {
    let name = file_name_of(target);
    let Ok(rd) = std::fs::read_dir(parent_of(target)) else { return 0 };
    let mut n = 0;
    for e in rd.flatten() {
        if is_our_temp(&e.file_name().to_string_lossy(), &name) && std::fs::remove_file(e.path()).is_ok() {
            n += 1;
        }
    }
    n
}

fn resolve_link(p: &Path) -> PathBuf {
    match std::fs::symlink_metadata(p) {
        Ok(m) if m.file_type().is_symlink() => std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf()),
        _ => p.to_path_buf(),
    }
}

fn same_file(a: &Path, b: &Path) -> bool {
    match (std::fs::canonicalize(a), std::fs::canonicalize(b)) {
        (Ok(x), Ok(y)) => x == y,
        _ => a == b,
    }
}

fn mtime_parts(t: Option<SystemTime>) -> (i64, u32, bool) {
    match t.and_then(|t| t.duration_since(UNIX_EPOCH).ok()) {
        Some(d) => (d.as_secs() as i64, d.subsec_nanos(), true),
        None => (0, 0, false),
    }
}

fn unix_secs() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

// ------------------------------------------------------------------------------------------ external change

/// How the base file on disk compares with what we opened.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum External {
    Unchanged,
    /// Someone wrote into the file we are reading, and it only grew: the old extent looks intact (head/tail
    /// fingerprint). The store is stale until `follow_append()` (keep my edits + show the new tail) or `reload()`.
    Appended { old_len: u64, new_len: u64 },
    /// Written in place (rewrite, truncation, touch): the store is stale, reads are refused.
    Modified,
    /// The path now names a different file (editor-style atomic replace) but our handle still reads the old
    /// content consistently: reads continue, saving is gated.
    Replaced,
    /// Deleted or renamed away; our handle still reads. A save recreates the file.
    Gone,
}

impl External {
    pub fn is_change(&self) -> bool {
        !matches!(self, External::Unchanged)
    }
}

impl LazyStore {
    /// The newest base generation (the file the document was last opened from or saved to).
    fn current_base(&self) -> Option<&Arc<BaseFile>> {
        self.bases.iter().flatten().max_by_key(|b| b.gen)
    }
    /// 1-based `src` id of the base returned by `current_base`.
    fn current_src(&self) -> u16 {
        match self.current_base() {
            Some(cb) => self.bases.iter().position(|b| b.as_ref().is_some_and(|b| Arc::ptr_eq(b, cb))).map_or(0, |i| i as u16 + 1),
            None => 0,
        }
    }

    /// True once a base was found changed in place: reads are refused, edits fail, saves refuse, until the user
    /// picks `reload()` / `follow_append()` / saves elsewhere with `save_with(.., overwrite_changed)`.
    pub fn is_stale(&self) -> bool {
        self.current_base().is_some_and(|b| b.is_stale())
    }

    /// Bytes whose read was refused because of a stale base (the view shows a "file changed" placeholder).
    pub fn refused_reads(&self) -> u64 {
        self.bases.iter().flatten().map(|b| b.refused_reads.load(Relaxed)).sum()
    }

    /// `(len, mtime)` of the base as opened: feed it to the app's `external::note` so the shared poll stays in step.
    pub fn base_stamp(&self) -> Option<(u64, Option<SystemTime>)> {
        self.current_base().map(|b| b.stamp)
    }

    /// Compare the disk with the base. Cheap (two stats; reads only when the file grew). Marks the base stale
    /// when it was modified in place, so no later read can return mixed old/new bytes.
    pub fn check_external(&self) -> io::Result<External> {
        let Some(b) = self.current_base() else { return Ok(External::Unchanged) };
        let hm = b.file.metadata()?;
        let handle_changed = hm.len() != b.stamp.0 || hm.modified().ok() != b.stamp.1;
        if handle_changed {
            b.stale.store(true, Relaxed);
            if hm.len() > b.stamp.0 && fingerprint(&b.file, b.stamp.0) == b.fp {
                return Ok(External::Appended { old_len: b.stamp.0, new_len: hm.len() });
            }
            return Ok(External::Modified);
        }
        match std::fs::metadata(&b.path) {
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(External::Gone),
            Err(e) => Err(e),
            Ok(pm) if pm.len() != b.stamp.0 || pm.modified().ok() != b.stamp.1 => Ok(External::Replaced),
            Ok(_) => Ok(External::Unchanged),
        }
    }

    /// Text typed (or pasted) that exists only in the add buffer, in document order. This is what a user would
    /// lose on "Reload": `reload` writes it to a salvage file first.
    pub fn unsaved_insertions(&self, out: &mut dyn Write) -> io::Result<u64> {
        let mut total = 0;
        for p in self.blocks.iter().flat_map(|b| b.pieces.iter()).filter(|p| p.src == 0) {
            self.add.with(p.start, p.start + p.len, |s| out.write_all(s))?;
            out.write_all(b"\n")?;
            total += p.len;
        }
        Ok(total)
    }

    /// "Reload": drop the edits and reopen the file lazily. When the document is dirty its inserted text is first
    /// written to `<name>.unsaved-<unix>.txt` in `salvage_dir` (returned), so Reload never destroys typing.
    pub fn reload(&mut self, salvage_dir: Option<&Path>) -> io::Result<Option<PathBuf>> {
        let path = self.base_path().ok_or_else(|| io::Error::other("no base file"))?;
        let mut salvage = None;
        if let (true, Some(dir)) = (self.is_dirty(), salvage_dir) {
            std::fs::create_dir_all(dir)?;
            let p = dir.join(format!("{}.unsaved-{}.txt", file_name_of(&path), unix_secs()));
            let mut f = io::BufWriter::new(File::create(&p)?);
            self.unsaved_insertions(&mut f)?;
            f.flush()?;
            f.get_ref().sync_all()?;
            salvage = Some(p);
        }
        let fresh = LazyStore::open(&path, self.cfg.clone())?;
        *self = fresh;
        Ok(salvage)
    }

    /// "Keep mine" for a log-style append: keep every edit (and the undo history), adopt the file's new length and
    /// show the appended bytes at the end of the document. Only valid right after `check_external` said `Appended`.
    /// `history` visits the pieces of the caller's own history records (the engine's `Buffer` keeps them outside the
    /// store) so they are remapped to the new generation too; afterwards call `gc_bases_with` to release the old one.
    pub fn follow_append(&mut self, history: &mut dyn FnMut(&mut dyn FnMut(&mut Piece))) -> io::Result<()> {
        let old_src = self.current_src();
        let (path, old_len, old_fp) = match self.current_base() {
            Some(b) => (b.path.clone(), b.stamp.0, b.fp),
            None => return Err(io::Error::other("no base file")),
        };
        let nb = BaseFile::open(&path, &self.cfg, None)?;
        if nb.len < old_len || fingerprint(&nb.file, old_len) != old_fp {
            return Err(io::Error::other("the file did not just grow: reload instead"));
        }
        if self.cfg.background_index {
            Self::spawn_scan(&nb)?;
        }
        let was_dirty = self.is_dirty();
        let new_src = self.push_base(nb.clone());
        let remap = |p: &mut Piece| {
            if p.src == old_src {
                p.src = new_src;
            }
        };
        for b in self.blocks.iter_mut() {
            Arc::make_mut(b).pieces.iter_mut().for_each(remap);
        }
        for g in self.undo.iter_mut().chain(self.redo.iter_mut()).chain(self.open_group.iter_mut()) {
            for r in g.iter_mut() {
                r.pieces_mut().for_each(remap);
            }
        }
        history(&mut |p| remap(p));
        if nb.len > old_len {
            let tail = Piece { src: new_src, start: old_len, len: nb.len - old_len, nl: NlCache::new(UNKNOWN) };
            let at = self.len;
            self.insert_pieces(at, vec![tail]);
            self.len += nb.len - old_len;
        }
        self.version += 1;
        if !was_dirty {
            self.saved_version = self.version;
        }
        Ok(())
    }

    /// Session restore: byte offset for a saved `(line, col)` (both 0-based, `col` in chars), clamped to the
    /// document and to a char boundary. Reads at most one line (capped), never the document.
    pub fn restore_cursor(&self, line: usize, col: usize) -> usize {
        let Some(start) = self.line_start(line as u64) else { return self.len as usize };
        let end = (start + (1 << 16)).min(self.len);
        let mut chunk = Vec::new();
        self.visit_range(start..end, false, &mut |c| {
            chunk.extend_from_slice(c);
            true
        });
        let text = String::from_utf8_lossy(&chunk);
        let mut off = 0usize;
        for (i, ch) in text.chars().enumerate() {
            if i >= col || ch == '\n' || (ch == '\r' && text[off..].starts_with("\r\n")) {
                break;
            }
            off += ch.len_utf8();
        }
        start as usize + off
    }

    /// Session restore: a saved first-visible line clamped to the document (needs the line index only up to it).
    pub fn restore_scroll_line(&self, line: usize) -> usize {
        let mut l = line;
        while l > 0 && self.line_start(l as u64).is_none() {
            l /= 2;
        }
        l
    }
}

// ------------------------------------------------------------------------------------------ save

/// Where the injected failure strikes (tests; production passes `None`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SaveStep {
    CreateTemp,
    /// Before the n-th (0-based) chunk is written to the temp file.
    Write(usize),
    Flush,
    Fsync,
    Rename,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FaultKind {
    /// A real error (disk full, I/O): the save cleans its temp file and reports.
    Error(io::ErrorKind),
    /// The process dies right here: nothing is cleaned up (temp left behind), as after a kill.
    Crash,
}

#[derive(Default)]
pub struct SaveOptions<'a> {
    /// The user chose "Overwrite" after the changed-on-disk question.
    pub overwrite_changed: bool,
    /// The caller (the engine's `Buffer`) owns the undo history: keep every base generation, release them later
    /// with `gc_bases_with`.
    pub keep_bases: bool,
    /// Backup-before-save hook (the app passes its `.bak` logic): receives the target, returns the backup path.
    pub backup: Option<&'a mut dyn FnMut(&Path) -> io::Result<Option<PathBuf>>>,
    pub fault: Option<(SaveStep, FaultKind)>,
}

#[derive(Debug)]
pub enum SaveError {
    /// The base changed on disk since open/last save: ask Reload / Keep mine / Compare, or save with `overwrite_changed`.
    ChangedOnDisk(External),
    /// The store is stale (base modified in place) and cannot be read: resolve first.
    Stale,
    ReadOnly(PathBuf),
    Backup(io::Error),
    /// Writing failed (disk full, locked target...): the old file is intact and the temp file is gone.
    Write(io::Error),
    /// Test hook: simulated process death (temp file deliberately left).
    Crashed(SaveStep),
}

impl std::fmt::Display for SaveError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SaveError::ChangedOnDisk(c) => write!(f, "the file changed on disk ({c:?}); nothing was written"),
            SaveError::Stale => write!(f, "the file changed on disk and can no longer be read safely; reload or save elsewhere"),
            SaveError::ReadOnly(p) => write!(f, "{} is read-only; nothing was written.", p.display()),
            SaveError::Backup(e) => write!(f, "could not create the backup ({e}); the original is unchanged."),
            SaveError::Write(e) => write!(f, "could not save ({e}); the original is unchanged."),
            SaveError::Crashed(s) => write!(f, "simulated crash at {s:?}"),
        }
    }
}
impl std::error::Error for SaveError {}
impl From<SaveError> for io::Error {
    fn from(e: SaveError) -> Self {
        match e {
            SaveError::Write(e) | SaveError::Backup(e) => e,
            SaveError::ReadOnly(_) => io::Error::new(io::ErrorKind::PermissionDenied, e.to_string()),
            other => io::Error::other(other.to_string()),
        }
    }
}

#[derive(Debug)]
pub struct SaveReport {
    pub backup: Option<PathBuf>,
    pub bytes: u64,
}

impl LazyStore {
    /// Streaming atomic save to `path` (the base file itself, or a new path = Save As):
    /// gate (changed on disk) -> read-only check -> backup hook -> temp in the same directory -> stream base ranges and
    /// add-buffer pieces -> fsync -> rename over the target -> the saved file becomes the new base generation.
    /// Any failure before the rename leaves the old file untouched and removes the temp file. Undo history survives:
    /// older base generations stay open while undo/redo records refer to them.
    ///
    /// Bytes are written verbatim (UTF-8, the BOM flag, EOLs exactly as in the document); EOL conversion is an
    /// edit made in the buffer, as in the rope path (`SaveSpec { eol: None }`).
    pub fn save_with(&mut self, path: &Path, mut opts: SaveOptions<'_>) -> Result<SaveReport, SaveError> {
        let target = resolve_link(path);
        let fail = |opts: &SaveOptions<'_>, step: SaveStep| -> Result<(), SaveError> {
            match opts.fault {
                Some((s, FaultKind::Error(k))) if s == step => Err(SaveError::Write(io::Error::new(k, "injected failure"))),
                Some((s, FaultKind::Crash)) if s == step => Err(SaveError::Crashed(step)),
                _ => Ok(()),
            }
        };
        if self.is_stale() {
            return Err(SaveError::Stale);
        }
        // changed-on-disk gate: only when overwriting the file we are reading from
        if !opts.overwrite_changed {
            if let Some(bp) = self.base_path() {
                if same_file(&bp, &target) {
                    match self.check_external().map_err(SaveError::Write)? {
                        External::Unchanged | External::Gone => {}
                        c => return Err(SaveError::ChangedOnDisk(c)),
                    }
                }
            }
        }
        if self.is_stale() {
            return Err(SaveError::Stale);
        }
        let existing = std::fs::metadata(&target).ok();
        if existing.as_ref().is_some_and(|m| m.permissions().readonly()) {
            return Err(SaveError::ReadOnly(target));
        }
        sweep_orphan_temps(&target);
        let mut backup = None;
        if let (Some(hook), true) = (opts.backup.as_mut(), target.is_file()) {
            backup = hook(&target).map_err(SaveError::Backup)?;
        }
        let dir = parent_of(&target);
        let tmp = dir.join(format!(".{}.brouillon-{}.tmp", file_name_of(&target), nonce()));
        let refused_before = self.refused_reads();
        let outcome = self.write_temp(&tmp, existing.as_ref(), &opts, &fail);
        let (idx, bytes) = match outcome {
            Ok(v) => v,
            Err(e) => {
                if !matches!(e, SaveError::Crashed(_)) {
                    let _ = std::fs::remove_file(&tmp);
                }
                return Err(e);
            }
        };
        if self.refused_reads() != refused_before {
            let _ = std::fs::remove_file(&tmp);
            return Err(SaveError::Stale);
        }
        if let Err(e) = fail(&opts, SaveStep::Rename) {
            if !matches!(e, SaveError::Crashed(_)) {
                let _ = std::fs::remove_file(&tmp);
            }
            return Err(e);
        }
        if let Err(e) = std::fs::rename(&tmp, &target) {
            let _ = std::fs::remove_file(&tmp);
            return Err(SaveError::Write(e));
        }
        sync_dir(&dir);
        self.rebase(&target, idx, !opts.keep_bases);
        Ok(SaveReport { backup, bytes })
    }

    fn write_temp(
        &self,
        tmp: &Path,
        existing: Option<&std::fs::Metadata>,
        opts: &SaveOptions<'_>,
        fail: &dyn Fn(&SaveOptions<'_>, SaveStep) -> Result<(), SaveError>,
    ) -> Result<(IdxSt, u64), SaveError> {
        fail(opts, SaveStep::CreateTemp)?;
        let file = create_like(tmp, existing).map_err(SaveError::Write)?;
        let mut idx = IdxSt::new();
        let block = self.cfg.block as u64;
        let mut w = io::BufWriter::with_capacity(1 << 20, file);
        let mut bytes = 0u64;
        if self.bom {
            w.write_all(&[0xEF, 0xBB, 0xBF]).map_err(SaveError::Write)?;
            idx.feed(block, &[0xEF, 0xBB, 0xBF]);
            bytes += 3;
        }
        let mut err: Option<SaveError> = None;
        let mut n = 0usize;
        self.visit_range(0..self.len, true, &mut |c| {
            if let Err(e) = fail(opts, SaveStep::Write(n)) {
                err = Some(e);
                return false;
            }
            n += 1;
            if let Err(e) = w.write_all(c) {
                err = Some(SaveError::Write(e));
                return false;
            }
            idx.feed(block, c);
            bytes += c.len() as u64;
            true
        });
        if let Some(e) = err {
            return Err(e);
        }
        fail(opts, SaveStep::Flush)?;
        w.flush().map_err(SaveError::Write)?;
        fail(opts, SaveStep::Fsync)?;
        w.get_ref().sync_all().map_err(SaveError::Write)?;
        drop(w);
        Ok((idx, bytes))
    }

    /// Make the freshly written file the new base generation (its line index was built while streaming).
    fn rebase(&mut self, path: &Path, idx: IdxSt, gc: bool) {
        let li = LineIndex::from_state(self.cfg.block, idx);
        let total_nl = *li.st.lock().unwrap().cum.last().unwrap();
        let Ok(nb) = BaseFile::open(path, &self.cfg, Some(li)) else {
            // saved but cannot be reopened (removed again, or locked): the old generations still read the same
            // content through their handles, so the document stays correct; just mark it clean.
            self.saved_version = self.version;
            return;
        };
        let bom_len = if self.bom { 3 } else { 0 };
        let src = self.push_base(nb.clone());
        self.blocks = vec![new_block()];
        if nb.len > bom_len {
            let p = Piece { src, start: bom_len, len: nb.len - bom_len, nl: NlCache::new(total_nl) };
            self.blocks[0] = Arc::new(Block { len: p.len, nl: NlCache::new(total_nl), pieces: vec![p] });
        }
        self.len = nb.len.saturating_sub(bom_len);
        self.saved_version = self.version;
        if gc {
            self.gc_bases();
        }
    }
}

/// Create the temp file with the target's Windows HIDDEN/SYSTEM attributes (Unix: mode copied afterwards).
fn create_like(tmp: &Path, existing: Option<&std::fs::Metadata>) -> io::Result<File> {
    let mut oo = std::fs::OpenOptions::new();
    oo.write(true).create_new(true);
    #[cfg(windows)]
    if let Some(m) = existing {
        use std::os::windows::fs::{MetadataExt, OpenOptionsExt};
        const HIDDEN_SYSTEM: u32 = 0x2 | 0x4;
        oo.attributes(m.file_attributes() & HIDDEN_SYSTEM);
    }
    let f = oo.open(tmp)?;
    #[cfg(unix)]
    if let Some(m) = existing {
        let _ = std::fs::set_permissions(tmp, m.permissions());
    }
    #[cfg(not(any(windows, unix)))]
    let _ = existing;
    Ok(f)
}

fn sync_dir(dir: &Path) {
    #[cfg(unix)]
    if let Ok(d) = File::open(dir) {
        let _ = d.sync_all();
    }
    #[cfg(not(unix))]
    let _ = dir; // Windows refuses to flush a directory handle; the rename is already journaled by NTFS
}

// ------------------------------------------------------------------------------------------ journal

const MAGIC: &[u8; 8] = b"BRLZJ001";
const END: &[u8; 4] = b"END!";
pub const JOURNAL_EXT: &str = "lzj";
const ENTRY_BYTES: u64 = 17;

#[derive(Debug, Clone)]
pub struct JournalLimits {
    /// Refuse to write a journal bigger than this (the previous one stays; the caller shows a warning).
    pub max_bytes: u64,
}
impl Default for JournalLimits {
    fn default() -> Self {
        JournalLimits { max_bytes: 256 << 20 }
    }
}

#[derive(Debug)]
pub enum JournalError {
    Io(io::Error),
    /// The journal would be `need` bytes, over the cap.
    TooLarge { need: u64, cap: u64 },
    /// Base unreadable (stale): resolve first.
    Stale,
    /// The base file the journal was taken against is gone.
    BaseGone(PathBuf),
    /// The base file is not what the journal was taken against: refuse (rebuilding would risk garbage). Use
    /// `journal_salvage` to recover the typed text.
    BaseChanged { path: PathBuf, reason: &'static str },
    Corrupt(&'static str),
}
impl std::fmt::Display for JournalError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            JournalError::Io(e) => write!(f, "{e}"),
            JournalError::TooLarge { need, cap } => write!(f, "recovery journal would be {need} bytes (cap {cap})"),
            JournalError::Stale => write!(f, "the file changed on disk; resolve it before journaling"),
            JournalError::BaseGone(p) => write!(f, "{} no longer exists; unsaved text can be salvaged", p.display()),
            JournalError::BaseChanged { path, reason } => write!(f, "{} changed on disk ({reason}); unsaved text can be salvaged", path.display()),
            JournalError::Corrupt(w) => write!(f, "recovery journal is damaged ({w})"),
        }
    }
}
impl std::error::Error for JournalError {}
impl From<io::Error> for JournalError {
    fn from(e: io::Error) -> Self {
        JournalError::Io(e)
    }
}

/// Header of a journal file (readable without loading the body).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JournalMeta {
    pub base_path: PathBuf,
    pub title: String,
    pub saved_at: u64,
    pub base_len: u64,
    pub doc_len: u64,
    pub pieces: u64,
    /// Bytes of text that exist only in the journal (typed/pasted text + restored foreign-generation ranges).
    pub add_bytes: u64,
    pub file_bytes: u64,
}

struct Hw<W: Write> {
    inner: W,
    h: u64,
    n: u64,
}
impl<W: Write> Hw<W> {
    fn put(&mut self, b: &[u8]) -> io::Result<()> {
        self.h = fnv(self.h, b);
        self.n += b.len() as u64;
        self.inner.write_all(b)
    }
    fn u64(&mut self, v: u64) -> io::Result<()> {
        self.put(&v.to_le_bytes())
    }
    fn str(&mut self, s: &str) -> io::Result<()> {
        self.put(&(s.len() as u32).to_le_bytes())?;
        self.put(s.as_bytes())
    }
}

struct Rd<'a> {
    b: &'a [u8],
    p: usize,
}
impl<'a> Rd<'a> {
    fn take(&mut self, n: usize) -> Result<&'a [u8], JournalError> {
        if self.p + n > self.b.len() {
            return Err(JournalError::Corrupt("truncated"));
        }
        let s = &self.b[self.p..self.p + n];
        self.p += n;
        Ok(s)
    }
    fn u64(&mut self) -> Result<u64, JournalError> {
        Ok(u64::from_le_bytes(self.take(8)?.try_into().unwrap()))
    }
    fn u32(&mut self) -> Result<u32, JournalError> {
        Ok(u32::from_le_bytes(self.take(4)?.try_into().unwrap()))
    }
    fn u8(&mut self) -> Result<u8, JournalError> {
        Ok(self.take(1)?[0])
    }
    fn str(&mut self) -> Result<String, JournalError> {
        let n = self.u32()? as usize;
        String::from_utf8(self.take(n)?.to_vec()).map_err(|_| JournalError::Corrupt("text"))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct JournalStats {
    pub bytes: u64,
    pub pieces: u64,
    pub add_bytes: u64,
}

struct Header {
    meta: JournalMeta,
    base_mtime: (i64, u32, bool),
    base_fp: u64,
    bom: bool,
    body_at: usize,
}

fn parse_header(buf: &[u8]) -> Result<Header, JournalError> {
    let mut r = Rd { b: buf, p: 0 };
    if r.take(8)? != MAGIC {
        return Err(JournalError::Corrupt("magic"));
    }
    let base_path = PathBuf::from(r.str()?);
    let title = r.str()?;
    let saved_at = r.u64()?;
    let base_len = r.u64()?;
    let secs = r.u64()? as i64;
    let nanos = r.u32()?;
    let has = r.u8()? != 0;
    let base_fp = r.u64()?;
    let bom = r.u8()? != 0;
    let doc_len = r.u64()?;
    let pieces = r.u64()?;
    let add_bytes = r.u64()?;
    Ok(Header {
        meta: JournalMeta { base_path, title, saved_at, base_len, doc_len, pieces, add_bytes, file_bytes: 0 },
        base_mtime: (secs, nanos, has),
        base_fp,
        bom,
        body_at: r.p,
    })
}

impl LazyStore {
    /// Snapshot the unsaved state as a delta against the base file: the piece list (base ranges by offset, typed
    /// text by value) plus the text that exists only in memory. Size = O(edits), never O(document); streamed, so
    /// the heap stays flat. Written atomically (temp + fsync + rename). Undo history is not journaled.
    pub fn journal_write(&self, file: &Path, title: &str, limits: &JournalLimits) -> Result<JournalStats, JournalError> {
        if self.is_stale() {
            return Err(JournalError::Stale);
        }
        let base = self.current_base().ok_or(JournalError::Corrupt("no base"))?;
        let base_idx = self.current_src();
        // pass 1: sizes
        let (mut n_pieces, mut add_len) = (0u64, 0u64);
        for p in self.blocks.iter().flat_map(|b| b.pieces.iter()) {
            n_pieces += 1;
            if p.src != base_idx {
                add_len += p.len;
            }
        }
        let header_bytes = (8 + 4 + base.path.to_string_lossy().len() + 4 + title.len() + 8 * 3 + 4 + 1 + 8 + 1 + 8 * 3) as u64;
        let need = header_bytes + n_pieces * ENTRY_BYTES + add_len + 8 + 4;
        if need > limits.max_bytes {
            return Err(JournalError::TooLarge { need, cap: limits.max_bytes });
        }
        let dir = parent_of(file);
        std::fs::create_dir_all(&dir)?;
        let tmp = dir.join(format!(".{}.{}.tmp", file_name_of(file), nonce()));
        let refused_before = self.refused_reads();
        let res = (|| -> Result<JournalStats, JournalError> {
            let f = File::create(&tmp)?;
            let mut w = Hw { inner: io::BufWriter::with_capacity(256 << 10, f), h: FNV_OFFSET, n: 0 };
            w.put(MAGIC)?;
            w.str(&base.path.to_string_lossy())?;
            w.str(title)?;
            w.u64(unix_secs())?;
            w.u64(base.stamp.0)?;
            let (secs, nanos, has) = mtime_parts(base.stamp.1);
            w.u64(secs as u64)?;
            w.put(&nanos.to_le_bytes())?;
            w.put(&[has as u8])?;
            w.u64(base.fp)?;
            w.put(&[self.bom as u8])?;
            w.u64(self.len)?;
            w.u64(n_pieces)?;
            w.u64(add_len)?;
            let mut add_off = 0u64;
            for p in self.blocks.iter().flat_map(|b| b.pieces.iter()) {
                if p.src == base_idx {
                    w.put(&[1])?;
                    w.u64(p.start)?;
                } else {
                    w.put(&[0])?;
                    w.u64(add_off)?;
                    add_off += p.len;
                }
                w.u64(p.len)?;
            }
            let mut werr: Option<io::Error> = None;
            for p in self.blocks.iter().flat_map(|b| b.pieces.iter()).filter(|p| p.src != base_idx) {
                self.src_visit(p, 0, p.len, true, &mut |c| match w.put(c) {
                    Ok(()) => true,
                    Err(e) => {
                        werr = Some(e);
                        false
                    }
                });
                if let Some(e) = werr.take() {
                    return Err(e.into());
                }
            }
            let h = w.h;
            let n = w.n;
            w.inner.write_all(&h.to_le_bytes())?;
            w.inner.write_all(END)?;
            w.inner.flush()?;
            w.inner.get_ref().sync_all()?;
            Ok(JournalStats { bytes: n + 12, pieces: n_pieces, add_bytes: add_len })
        })();
        match res {
            Ok(s) => {
                if self.refused_reads() != refused_before {
                    let _ = std::fs::remove_file(&tmp);
                    return Err(JournalError::Stale);
                }
                if let Err(e) = std::fs::rename(&tmp, file) {
                    let _ = std::fs::remove_file(&tmp);
                    return Err(e.into());
                }
                sync_dir(&dir);
                Ok(s)
            }
            Err(e) => {
                let _ = std::fs::remove_file(&tmp);
                Err(e)
            }
        }
    }

    /// Rebuild the unsaved document from a journal against its base file. The result opens dirty (must be saved
    /// explicitly). Refuses (never guesses) when the base is gone, or its length / mtime / fingerprint differ.
    pub fn from_journal(file: &Path, cfg: Config) -> Result<LazyStore, JournalError> {
        let buf = std::fs::read(file)?;
        let (hdr, body) = verify_journal(&buf)?;
        let m = &hdr.meta;
        let md = match std::fs::metadata(&m.base_path) {
            Ok(md) => md,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Err(JournalError::BaseGone(m.base_path.clone())),
            Err(e) => return Err(e.into()),
        };
        let changed = |reason| JournalError::BaseChanged { path: m.base_path.clone(), reason };
        if md.len() != m.base_len {
            return Err(changed("length differs"));
        }
        if mtime_parts(md.modified().ok()) != hdr.base_mtime {
            return Err(changed("modification time differs"));
        }
        let probe = File::open(&m.base_path)?;
        if fingerprint(&probe, m.base_len) != hdr.base_fp {
            return Err(changed("content differs"));
        }
        drop(probe);
        let mut s = LazyStore::open(&m.base_path, cfg).map_err(JournalError::Io)?;
        if s.bom != hdr.bom {
            return Err(changed("byte order mark differs"));
        }
        let bom_len = if s.bom { 3 } else { 0 };
        let n = m.pieces as usize;
        let mut r = Rd { b: body, p: 0 };
        let add_start = n * ENTRY_BYTES as usize;
        if body.len() < add_start || (body.len() - add_start) as u64 != m.add_bytes {
            return Err(JournalError::Corrupt("sizes"));
        }
        let mut pieces = Vec::with_capacity(n);
        let mut total = 0u64;
        for _ in 0..n {
            let kind = r.u8()?;
            let start = r.u64()?;
            let len = r.u64()?;
            let src = match kind {
                1 => {
                    if start < bom_len || start.checked_add(len).is_none_or(|e| e > m.base_len) {
                        return Err(JournalError::Corrupt("base range"));
                    }
                    1u16
                }
                0 => {
                    if start.checked_add(len).is_none_or(|e| e > m.add_bytes) {
                        return Err(JournalError::Corrupt("add range"));
                    }
                    0u16
                }
                _ => return Err(JournalError::Corrupt("kind")),
            };
            total += len;
            pieces.push(Piece { src, start, len, nl: NlCache::new(UNKNOWN) });
        }
        if total != m.doc_len {
            return Err(JournalError::Corrupt("document length"));
        }
        s.add = Arc::new(AddBuf { v: std::sync::RwLock::new(body[add_start..].to_vec()) });
        for p in pieces.iter().filter(|p| p.src == 0) {
            p.nl.set(s.add.with(p.start, p.start + p.len, |b| memchr::memchr_iter(b'\n', b).count() as u64));
        }
        // pieces -> blocks of half capacity
        let per = (s.cfg.max_pieces_per_block / 2).max(1);
        let mut blocks: Vec<Arc<Block>> = pieces
            .chunks(per)
            .map(|c| Arc::new(Block { len: c.iter().map(|p| p.len).sum(), nl: NlCache::new(UNKNOWN), pieces: c.to_vec() }))
            .collect();
        if blocks.is_empty() {
            blocks.push(new_block());
        }
        s.blocks = blocks;
        s.len = total;
        s.version = 1;
        s.saved_version = 0;
        Ok(s)
    }
}

fn verify_journal(buf: &[u8]) -> Result<(Header, &[u8]), JournalError> {
    if buf.len() < 8 + 12 || &buf[buf.len() - 4..] != END {
        return Err(JournalError::Corrupt("end marker"));
    }
    let body_end = buf.len() - 12;
    let want = u64::from_le_bytes(buf[body_end..body_end + 8].try_into().unwrap());
    if fnv(FNV_OFFSET, &buf[..body_end]) != want {
        return Err(JournalError::Corrupt("checksum"));
    }
    let mut hdr = parse_header(&buf[..body_end])?;
    hdr.meta.file_bytes = buf.len() as u64;
    let at = hdr.body_at;
    Ok((hdr, &buf[at..body_end]))
}

/// Read only the header of a journal (no checksum pass): for listing in the recovery panel.
pub fn journal_meta(file: &Path) -> Result<JournalMeta, JournalError> {
    let mut f = File::open(file)?;
    let len = f.metadata()?.len();
    let mut head = vec![0u8; len.min(16 << 10) as usize];
    io::Read::read_exact(&mut f, &mut head)?;
    let mut h = parse_header(&head)?;
    h.meta.file_bytes = len;
    Ok(h.meta)
}

/// The text a journal holds that is not in the base file (what the user typed), in document order, newline
/// separated. Works even when the base changed or vanished: the way out when `from_journal` refuses.
pub fn journal_salvage(file: &Path, out: &mut dyn Write) -> Result<u64, JournalError> {
    let buf = std::fs::read(file)?;
    let (hdr, body) = verify_journal(&buf)?;
    let n = hdr.meta.pieces as usize;
    let mut r = Rd { b: body, p: 0 };
    let add = body.get(n * ENTRY_BYTES as usize..).ok_or(JournalError::Corrupt("sizes"))?;
    let mut total = 0;
    for _ in 0..n {
        let kind = r.u8()?;
        let start = r.u64()? as usize;
        let len = r.u64()? as usize;
        if kind == 0 {
            out.write_all(add.get(start..start + len).ok_or(JournalError::Corrupt("add range"))?)?;
            out.write_all(b"\n")?;
            total += len as u64;
        }
    }
    Ok(total)
}

/// A directory of journals, one `<id>.lzj` per lazy document (self-describing, no index file to corrupt).
pub struct LazyJournals {
    dir: PathBuf,
}

fn safe_id(id: &str) -> String {
    id.chars().map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '_' }).collect()
}

impl LazyJournals {
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        LazyJournals { dir: dir.into() }
    }
    pub fn file(&self, id: &str) -> PathBuf {
        self.dir.join(format!("{}.{JOURNAL_EXT}", safe_id(id)))
    }
    pub fn write(&self, id: &str, store: &LazyStore, title: &str, limits: &JournalLimits) -> Result<JournalStats, JournalError> {
        store.journal_write(&self.file(id), title, limits)
    }
    /// `(id, header)` of every readable journal, oldest first. Damaged ones are skipped, never deleted.
    pub fn list(&self) -> Vec<(String, JournalMeta)> {
        let Ok(rd) = std::fs::read_dir(&self.dir) else { return Vec::new() };
        let mut v: Vec<(String, JournalMeta)> = rd
            .flatten()
            .filter(|e| e.path().extension().is_some_and(|x| x == JOURNAL_EXT))
            .filter_map(|e| {
                let id = e.path().file_stem()?.to_string_lossy().into_owned();
                journal_meta(&e.path()).ok().map(|m| (id, m))
            })
            .collect();
        v.sort_by_key(|(_, m)| m.saved_at);
        v
    }
    pub fn restore(&self, id: &str, cfg: Config) -> Result<LazyStore, JournalError> {
        LazyStore::from_journal(&self.file(id), cfg)
    }
    pub fn salvage(&self, id: &str, out: &mut dyn Write) -> Result<u64, JournalError> {
        journal_salvage(&self.file(id), out)
    }
    pub fn discard(&self, id: &str) -> io::Result<()> {
        match std::fs::remove_file(self.file(id)) {
            Err(e) if e.kind() != io::ErrorKind::NotFound => Err(e),
            _ => Ok(()),
        }
    }
    /// Remove temp files left by an interrupted journal write (launch time).
    pub fn sweep_temps(&self) -> usize {
        let Ok(rd) = std::fs::read_dir(&self.dir) else { return 0 };
        let mut n = 0;
        for e in rd.flatten() {
            let name = e.file_name().to_string_lossy().into_owned();
            if name.starts_with('.') && name.contains(&format!(".{JOURNAL_EXT}.")) && name.ends_with(".tmp") && std::fs::remove_file(e.path()).is_ok() {
                n += 1;
            }
        }
        n
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum AutosaveOutcome {
    /// Nothing changed since the last tick.
    Unchanged,
    Written(JournalStats),
    /// The document is clean again: its journal was removed.
    Cleared,
}

/// Per-document autosave state: write only when the version moved, drop the journal when the doc is clean.
pub struct Autosaver {
    pub journals: LazyJournals,
    pub id: String,
    pub limits: JournalLimits,
    last: Option<u64>,
}

impl Autosaver {
    pub fn new(journals: LazyJournals, id: &str) -> Self {
        Autosaver { journals, id: id.to_string(), limits: JournalLimits::default(), last: None }
    }
    pub fn tick(&mut self, store: &LazyStore, title: &str) -> Result<AutosaveOutcome, JournalError> {
        if !store.is_dirty() {
            let had = self.last.take().is_some() || self.journals.file(&self.id).exists();
            self.journals.discard(&self.id)?;
            return Ok(if had { AutosaveOutcome::Cleared } else { AutosaveOutcome::Unchanged });
        }
        if self.last == Some(store.version) {
            return Ok(AutosaveOutcome::Unchanged);
        }
        let s = self.journals.write(&self.id, store, title, &self.limits)?;
        self.last = Some(store.version);
        Ok(AutosaveOutcome::Written(s))
    }
    /// Save the document, then (only then) drop its journal. A crash in between leaves a journal whose base no
    /// longer matches: recovery refuses it (`BaseChanged`) and offers the salvage, never a wrong rebuild.
    pub fn save(&mut self, store: &mut LazyStore, path: &Path, opts: SaveOptions<'_>) -> Result<SaveReport, SaveError> {
        let r = store.save_with(path, opts)?;
        self.last = None;
        let _ = self.journals.discard(&self.id);
        Ok(r)
    }
}
