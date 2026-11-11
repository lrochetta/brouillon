//! BK-047 — autosave journal and crash recovery.
//!
//! Dirty and non-empty untitled buffers are written as sidecar files
//! (`<id>.txt`) plus an `index.toml` inside the journal directory. After a
//! crash `Journal::recover` returns the latest text of each; recovered docs
//! are opened dirty (the BUFFER is marked dirty: the tab flag alone is
//! overwritten by the mirror sync) so they must be saved explicitly.
//!
//! A normal exit keeps the entries of tabs that still have unsaved text too
//! (`mark_clean_exit`): the next launch restores them like a hot exit, with a
//! different message than after a crash. Entries are removed only when the
//! tab is saved, closed or discarded (`snapshot_tabs` drops what is no longer
//! dirty), never by quitting.

use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use brouillon_core::contract::{CmdCtx, CmdOutcome, CommandSpec, PanelId};
use serde::{Deserialize, Serialize};

use crate::buffer::Buffer;
use crate::tabs::TabManager;

const INDEX_FILE: &str = "index.toml";
pub const DEFAULT_INTERVAL: Duration = Duration::from_secs(30);

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct IndexEntry {
    id: String,
    path: Option<PathBuf>,
    title: String,
    saved_at: u64,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct Index {
    #[serde(default)]
    entries: Vec<IndexEntry>,
    /// Set by `mark_clean_exit`, cleared by the next `write`: tells a normal
    /// shutdown from a crash when the journal is read at launch.
    #[serde(default)]
    clean_exit: bool,
}

/// A journaled document offered for recovery.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecoveredDoc {
    pub id: String,
    /// Original path; `None` for an untitled buffer.
    pub path: Option<PathBuf>,
    pub title: String,
    /// Full text for a normal entry; for a lazy-journal entry only the text typed since the base file (the
    /// salvage fallback when the base file changed).
    pub text: String,
    /// Unix seconds of the last journal write.
    pub saved_at: u64,
    /// Large-file document: the piece journal (`<id>.lzj`, a delta against the base file) instead of a full copy.
    pub lazy_journal: Option<PathBuf>,
}

/// `~/.brouillon/recovery`, next to `session.toml`.
pub fn default_dir() -> Option<PathBuf> {
    dirs::home_dir().map(|h| h.join(".brouillon").join("recovery"))
}

pub struct Journal {
    dir: PathBuf,
    /// The buffer last journaled per tab id by `snapshot_tabs` in this process: the
    /// same `Arc` (every edit makes a new one; holding it rules out address reuse)
    /// means the text did not change, so it is not written, nor copied, again.
    written: std::cell::RefCell<std::collections::HashMap<String, std::sync::Arc<Buffer>>>,
}

impl Journal {
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        Self { dir: dir.into(), written: Default::default() }
    }

    fn read_index(&self) -> Index {
        fs::read_to_string(self.dir.join(INDEX_FILE))
            .ok()
            .and_then(|s| toml::from_str(&s).ok())
            .unwrap_or_default()
    }

    fn write_index(&self, idx: &Index) -> io::Result<()> {
        let s = toml::to_string_pretty(idx).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
        crate::backup::atomic_write(&self.dir.join(INDEX_FILE), s.as_bytes())
    }

    /// Journal `text` under a stable `id` (replaces the previous version).
    pub fn write(&self, id: &str, path: Option<&Path>, title: &str, text: &str) -> io::Result<()> {
        self.write_with(id, path, title, |out| out.write_all(text.as_bytes()))
    }

    fn lazy_file(&self, id: &str) -> PathBuf {
        self.dir.join(format!("{id}.{}", brouillon_textstore_spike::lazy::persist::JOURNAL_EXT))
    }

    fn write_with(&self, id: &str, path: Option<&Path>, title: &str, fill: impl FnMut(&mut fs::File) -> io::Result<()>) -> io::Result<()> {
        fs::create_dir_all(&self.dir)?;
        crate::backup::atomic_write_with(&self.dir.join(format!("{id}.txt")), fill)?;
        let _ = fs::remove_file(self.lazy_file(id));
        self.touch_index(id, path, title)
    }

    /// Journal a large-file document as a piece journal: only the typed text and the piece list are written (size
    /// proportional to the edits, never to the file). `Ok(false)` = could not (journal over the cap, store stale,
    /// not a lazy buffer): the caller falls back to the full-text journal or keeps the previous entry.
    fn write_lazy(&self, id: &str, path: Option<&Path>, title: &str, buffer: &Buffer) -> io::Result<bool> {
        use brouillon_textstore_spike::lazy::persist::{JournalError, JournalLimits};
        let Some(store) = buffer.lazy_store() else { return Ok(false) };
        fs::create_dir_all(&self.dir)?;
        match store.journal_write(&self.lazy_file(id), title, &JournalLimits::default()) {
            Ok(_) => {
                let _ = fs::remove_file(self.dir.join(format!("{id}.txt")));
                self.touch_index(id, path, title)?;
                Ok(true)
            }
            Err(JournalError::Io(e)) => Err(e),
            Err(_) => Ok(false),
        }
    }

    fn touch_index(&self, id: &str, path: Option<&Path>, title: &str) -> io::Result<()> {
        let mut idx = self.read_index();
        idx.clean_exit = false;
        idx.entries.retain(|e| e.id != id);
        idx.entries.push(IndexEntry {
            id: id.to_string(),
            path: path.map(Path::to_path_buf),
            title: title.to_string(),
            saved_at: SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0),
        });
        self.write_index(&idx)
    }

    /// Journal every dirty tab and every non-empty untitled tab; drop journal
    /// entries of tabs that are no longer dirty / no longer exist. Returns the
    /// number of documents journaled. Call on the autosave tick and at shutdown.
    pub fn snapshot_tabs(&self, tabs: &TabManager) -> io::Result<usize> {
        let mut live = Vec::new();
        for t in tabs.iter() {
            if t.help_doc.is_some() {
                continue; // built-in help documents are never journaled
            }
            let untitled_text = t.path.is_none() && t.remote_path.is_none() && t.buffer.byte_len() > 0;
            if !(t.dirty || untitled_text) {
                continue;
            }
            let id = tab_id(t.path.as_deref(), &t.title);
            let unchanged = self.written.borrow().get(&id).is_some_and(|b| std::sync::Arc::ptr_eq(b, &t.buffer)) && (self.dir.join(format!("{id}.txt")).is_file() || self.lazy_file(&id).is_file());
            if !unchanged {
                // Straight from the rope chunks / lazy pieces: no second copy of a large document.
                let buffer = &t.buffer;
                let stale = buffer.lazy_store().is_some_and(|s| s.is_stale());
                // Large file: piece journal (no copy of the document); the full-text path stays the fallback.
                if stale || !self.write_lazy(&id, t.path.as_deref(), &t.title, buffer)? {
                    if stale {
                        // the base is unreadable until the user resolves the external change: keep the last journal
                        live.push(id);
                        continue;
                    }
                    self.write_with(&id, t.path.as_deref(), &t.title, |out| buffer.write_to(out))?;
                }
                self.written.borrow_mut().insert(id.clone(), t.buffer.clone());
            }
            live.push(id);
        }
        self.written.borrow_mut().retain(|k, _| live.contains(k));
        let idx = self.read_index();
        for e in idx.entries.iter().filter(|e| !live.contains(&e.id)) {
            self.discard(&e.id)?;
        }
        Ok(live.len())
    }

    /// Record that the process is exiting normally (call after `snapshot_tabs`).
    pub fn mark_clean_exit(&self) -> io::Result<()> {
        let mut idx = self.read_index();
        if idx.entries.is_empty() {
            return Ok(());
        }
        idx.clean_exit = true;
        self.write_index(&idx)
    }

    /// True when the journal was last written by a normal shutdown (so its
    /// documents are unsaved work, not crash survivors).
    pub fn last_exit_clean(&self) -> bool {
        self.read_index().clean_exit
    }

    /// All journaled documents, oldest first.
    pub fn recover(&self) -> Vec<RecoveredDoc> {
        let mut docs: Vec<RecoveredDoc> = self
            .read_index()
            .entries
            .into_iter()
            .filter_map(|e| {
                let lz = self.lazy_file(&e.id);
                if lz.is_file() {
                    let mut typed = Vec::new();
                    brouillon_textstore_spike::lazy::persist::journal_salvage(&lz, &mut typed).ok()?;
                    let text = String::from_utf8_lossy(&typed).into_owned();
                    return Some(RecoveredDoc { id: e.id, path: e.path, title: e.title, text, saved_at: e.saved_at, lazy_journal: Some(lz) });
                }
                let text = fs::read_to_string(self.dir.join(format!("{}.txt", e.id))).ok()?;
                Some(RecoveredDoc { id: e.id, path: e.path, title: e.title, text, saved_at: e.saved_at, lazy_journal: None })
            })
            .collect();
        docs.sort_by_key(|d| d.saved_at);
        docs
    }

    /// Delete one journaled document.
    pub fn discard(&self, id: &str) -> io::Result<()> {
        let mut idx = self.read_index();
        idx.entries.retain(|e| e.id != id);
        match fs::remove_file(self.dir.join(format!("{id}.txt"))) {
            Ok(()) => {}
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(e) => return Err(e),
        }
        match fs::remove_file(self.lazy_file(id)) {
            Ok(()) => {}
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(e) => return Err(e),
        }
        if idx.entries.is_empty() {
            let _ = fs::remove_file(self.dir.join(INDEX_FILE));
            Ok(())
        } else {
            self.write_index(&idx)
        }
    }

    /// Delete the whole journal.
    pub fn discard_all(&self) -> io::Result<()> {
        for d in self.recover() {
            self.discard(&d.id)?;
        }
        let _ = fs::remove_file(self.dir.join(INDEX_FILE));
        Ok(())
    }

    /// True when every dirty / non-empty untitled tab has a journal entry
    /// holding its current text, i.e. closing needs no "save changes?" prompt.
    pub fn is_current(&self, tabs: &TabManager) -> bool {
        let docs = self.recover();
        tabs.iter().all(|t| {
            if t.help_doc.is_some() {
                return true;
            }
            let untitled_text = t.path.is_none() && t.remote_path.is_none() && t.buffer.byte_len() > 0;
            if !(t.dirty || untitled_text) {
                return true;
            }
            let id = tab_id(t.path.as_deref(), &t.title);
            docs.iter().any(|d| {
                d.id == id
                    && match &d.lazy_journal {
                        // piece journal: current when it describes a document of the same length
                        Some(f) => t.buffer.is_lazy() && brouillon_textstore_spike::lazy::persist::journal_meta(f).is_ok_and(|m| m.doc_len == t.buffer.byte_len() as u64),
                        None if t.buffer.is_lazy() => d.text.len() == t.buffer.byte_len(),
                        None => d.text == t.buffer.text(),
                    }
            })
        })
    }
}

/// A buffer holding unsaved text: dirty from the start, so the tab shows the
/// modified marker and the close prompt fires.
fn dirty_buffer(text: &str) -> std::sync::Arc<Buffer> {
    let mut b = Buffer::from_str(text);
    b.mark_dirty();
    std::sync::Arc::new(b)
}

/// The buffer a recovered document opens as: a piece-journal entry is rebuilt lazily on its base file; when the
/// base changed meanwhile (or the journal is damaged) it falls back to the text typed since the base, never to a
/// guess.
fn recovered_buffer(doc: &RecoveredDoc) -> std::sync::Arc<Buffer> {
    if let Some(f) = &doc.lazy_journal {
        if let Ok(store) = brouillon_textstore_spike::lazy::LazyStore::from_journal(f, brouillon_textstore_spike::lazy::Config::default()) {
            let mut b = Buffer::from_lazy(store);
            b.mark_dirty();
            return std::sync::Arc::new(b);
        }
    }
    dirty_buffer(&doc.text)
}

/// Open a recovered doc as a new tab; it is dirty until saved.
pub fn open_recovered(tabs: &mut TabManager, doc: &RecoveredDoc) -> usize {
    let idx = tabs.new_scratch();
    let t = tabs.current_mut();
    t.buffer = recovered_buffer(doc);
    t.path = doc.path.clone();
    t.title = doc.title.clone();
    t.dirty = true;
    idx
}

/// Put every journaled document into `tabs`, all marked modified. A tab the
/// session already restored for the same document (same path, or same untitled
/// id) gets the journal text instead of a duplicate tab. Returns how many docs
/// were applied. With `keep_active` the tab that was active stays active.
pub fn apply_recovered(tabs: &mut TabManager, docs: &[RecoveredDoc], keep_active: bool) -> usize {
    let active = tabs.active_idx();
    for doc in docs {
        let existing = tabs.iter().position(|t| tab_id(t.path.as_deref(), &t.title) == doc.id);
        match existing {
            Some(i) => {
                tabs.switch_to(i);
                let t = tabs.current_mut();
                t.buffer = recovered_buffer(doc);
                t.dirty = true;
            }
            None => {
                open_recovered(tabs, doc);
            }
        }
    }
    if keep_active && !docs.is_empty() {
        tabs.switch_to(active);
    }
    docs.len()
}

/// Status-bar text announcing recovered documents.
pub fn recovery_notice(count: usize, after_clean_exit: bool) -> String {
    let docs = if count == 1 { "1 unsaved document".to_string() } else { format!("{count} unsaved documents") };
    if after_clean_exit {
        format!("Restored {docs} from your last session (marked modified; save to keep).")
    } else {
        format!("Recovered {docs} after an unexpected shutdown (marked modified; save to keep).")
    }
}

/// Remove the temp files an interrupted save of `path` left next to it (`.<name>.brouillon-*.tmp`).
pub fn sweep_save_temps(path: &Path) -> usize {
    brouillon_textstore_spike::lazy::persist::sweep_orphan_temps(path)
}

/// Stable journal id: hash of the path, or the untitled title.
fn tab_id(path: Option<&Path>, title: &str) -> String {
    match path {
        Some(p) => {
            // FNV-1a: stable across runs (unlike DefaultHasher's guarantees).
            let mut h: u64 = 0xcbf29ce484222325;
            for b in p.to_string_lossy().bytes() {
                h = (h ^ u64::from(b)).wrapping_mul(0x100000001b3);
            }
            format!("f-{h:016x}")
        }
        None => format!("s-{}", title.chars().filter(|c| c.is_ascii_alphanumeric() || *c == '-').collect::<String>()),
    }
}

/// Interval gate for the autosave timer.
pub struct AutoSaver {
    pub interval: Duration,
    last: Instant,
}

impl AutoSaver {
    pub fn new(interval: Duration) -> Self {
        Self { interval, last: Instant::now() }
    }

    /// True (and re-arms) when `interval` has elapsed since the last tick.
    pub fn due(&mut self, now: Instant) -> bool {
        if now.duration_since(self.last) >= self.interval {
            self.last = now;
            true
        } else {
            false
        }
    }
}

/// Commands this module contributes to the registry.
pub fn commands() -> Vec<CommandSpec> {
    vec![CommandSpec {
        id: "file.recover".into(),
        caption: "Recover Unsaved Documents...".into(),
        category: "File".into(),
        ..Default::default()
    }]
}

/// Run command `id`; `None` = not mine.
pub fn run(id: &str, ctx: &CmdCtx) -> Option<CmdOutcome> {
    let _ = ctx;
    match id {
        "file.recover" => Some(CmdOutcome { open_panel: Some(PanelId("recovery".into())), ..Default::default() }),
        _ => None,
    }
}

#[cfg(test)]
mod tests {

    #[test]
    fn help_tabs_are_never_journaled() {
        let d = tempfile::tempdir().unwrap();
        let mut tm = TabManager::new();
        tm.open_help("welcome", "Welcome", "# Welcome\ntext that is not empty");
        let j = Journal::new(d.path());
        assert_eq!(j.snapshot_tabs(&tm).unwrap(), 0);
        assert!(j.recover().is_empty());
        assert!(j.is_current(&tm));
    }
    use super::*;

    #[test]
    fn crash_recovery_returns_two_dirty_and_one_untitled() {
        let d = tempfile::tempdir().unwrap();
        let files = tempfile::tempdir().unwrap();
        let (a, b) = (files.path().join("a.txt"), files.path().join("b.txt"));
        fs::write(&a, "a0").unwrap();
        fs::write(&b, "b0").unwrap();

        let mut tm = TabManager::new();
        tm.open_file(&a).unwrap();
        tm.open_file(&b).unwrap();
        tm.new_scratch();
        for (i, text) in [(0, "a-edited"), (1, "b-edited"), (2, "scratch text")] {
            tm.switch_to(i);
            let t = tm.current_mut();
            t.buffer = std::sync::Arc::new(Buffer::from_str(text));
            t.dirty = true;
        }
        let j = Journal::new(d.path());
        assert_eq!(j.snapshot_tabs(&tm).unwrap(), 3);
        assert!(j.is_current(&tm));
        // "Crash": drop everything, new process reads the journal.
        let docs = Journal::new(d.path()).recover();
        assert_eq!(docs.len(), 3);
        let texts: Vec<&str> = docs.iter().map(|d| d.text.as_str()).collect();
        assert!(texts.contains(&"a-edited") && texts.contains(&"b-edited") && texts.contains(&"scratch text"));
        assert_eq!(docs.iter().filter(|d| d.path.is_none()).count(), 1);
        assert_eq!(fs::read_to_string(&a).unwrap(), "a0", "disk untouched");

        let mut fresh = TabManager::new();
        let doc = docs.iter().find(|d| d.path.as_deref() == Some(a.as_path())).unwrap();
        open_recovered(&mut fresh, doc);
        assert!(fresh.current().dirty);
        assert!(fresh.current().buffer.is_dirty(), "the mirror sync derives the tab flag from the buffer");
        assert_eq!(fresh.current().buffer.rope().to_string(), "a-edited");
    }

    #[test]
    fn autosave_skips_unchanged_buffers_and_rewrites_changed_ones() {
        let d = tempfile::tempdir().unwrap();
        let mut tm = TabManager::new();
        tm.new_scratch();
        let t = tm.current_mut();
        t.buffer = std::sync::Arc::new(Buffer::from_str("first"));
        t.dirty = true;
        let j = Journal::new(d.path());
        assert_eq!(j.snapshot_tabs(&tm).unwrap(), 1);
        let file = fs::read_dir(d.path()).unwrap().map(|e| e.unwrap().path()).find(|p| p.extension().is_some_and(|x| x == "txt")).unwrap();
        // Tamper with the journal file: an unchanged buffer must not be rewritten.
        fs::write(&file, "marker").unwrap();
        assert_eq!(j.snapshot_tabs(&tm).unwrap(), 1);
        assert_eq!(fs::read_to_string(&file).unwrap(), "marker", "unchanged buffer is not written again");
        // An edit bumps the version: the file is rewritten from the rope.
        let mut nb = (*tm.current().buffer).clone();
        nb.insert_at(5, "!");
        tm.current_mut().buffer = std::sync::Arc::new(nb);
        assert_eq!(j.snapshot_tabs(&tm).unwrap(), 1);
        assert_eq!(fs::read_to_string(&file).unwrap(), "first!");
    }

    /// R01: after a crash the session restores the file tab (from disk) and the
    /// journal puts the unsaved text back: modified, no duplicate, and still
    /// journaled at the next autosave tick (it used to be dropped there).
    #[test]
    fn recovered_docs_are_modified_deduplicated_and_stay_journaled() {
        let d = tempfile::tempdir().unwrap();
        let files = tempfile::tempdir().unwrap();
        let a = files.path().join("a.txt");
        fs::write(&a, "a0").unwrap();
        let j = Journal::new(d.path());
        let mut tm = TabManager::new();
        tm.open_file(&a).unwrap();
        tm.new_scratch();
        for (i, text) in [(0, "a-edited"), (1, "scratch text")] {
            tm.switch_to(i);
            let t = tm.current_mut();
            t.buffer = dirty_buffer(text);
            t.dirty = true;
        }
        j.snapshot_tabs(&tm).unwrap();

        // New process: the session restored both tabs from disk / its own copy.
        let mut fresh = TabManager::new();
        fresh.open_file(&a).unwrap();
        fresh.new_scratch();
        fresh.current_mut().title = tm.current().title.clone();
        fresh.switch_to(0);
        let docs = j.recover();
        assert_eq!(apply_recovered(&mut fresh, &docs, true), 2);
        assert_eq!(fresh.count(), 2, "no duplicate tab");
        assert_eq!(fresh.active_idx(), 0);
        for t in fresh.iter() {
            assert!(t.dirty && t.buffer.is_dirty(), "{} must open modified", t.title);
        }
        assert_eq!(fresh.iter().next().unwrap().buffer.rope().to_string(), "a-edited");
        assert_eq!(fs::read_to_string(&a).unwrap(), "a0", "disk untouched until the user saves");
        // next autosave tick keeps both entries
        assert_eq!(j.snapshot_tabs(&fresh).unwrap(), 2);
        assert_eq!(j.recover().len(), 2);
    }

    #[test]
    fn clean_exit_flag_distinguishes_quit_from_crash() {
        let d = tempfile::tempdir().unwrap();
        let j = Journal::new(d.path());
        j.mark_clean_exit().unwrap(); // nothing journaled: no-op
        j.write("s-x", None, "x", "text").unwrap();
        assert!(!j.last_exit_clean(), "a fresh write is a crash survivor until a clean exit says otherwise");
        j.mark_clean_exit().unwrap();
        assert!(j.last_exit_clean());
        assert_eq!(j.recover().len(), 1);
        j.write("s-y", None, "y", "more").unwrap();
        assert!(!j.last_exit_clean());
        assert!(recovery_notice(1, false).contains("unexpected shutdown"));
        assert!(recovery_notice(2, true).contains("last session"));
    }

    #[test]
    fn discard_deletes_journal() {
        let d = tempfile::tempdir().unwrap();
        let j = Journal::new(d.path());
        j.write("s-untitled1", None, "untitled-1", "x").unwrap();
        j.write("s-untitled2", None, "untitled-2", "y").unwrap();
        j.discard("s-untitled1").unwrap();
        assert_eq!(j.recover().len(), 1);
        j.discard_all().unwrap();
        assert!(j.recover().is_empty());
        assert_eq!(fs::read_dir(d.path()).unwrap().count(), 0);
    }

    #[test]
    fn clean_tab_entries_are_dropped_and_latest_text_wins() {
        let d = tempfile::tempdir().unwrap();
        let j = Journal::new(d.path());
        let mut tm = TabManager::new();
        tm.current_mut().buffer = std::sync::Arc::new(Buffer::from_str("v1"));
        j.snapshot_tabs(&tm).unwrap();
        tm.current_mut().buffer = std::sync::Arc::new(Buffer::from_str("v2"));
        assert!(!j.is_current(&tm));
        j.snapshot_tabs(&tm).unwrap();
        assert_eq!(j.recover()[0].text, "v2");
        tm.current_mut().buffer = std::sync::Arc::new(Buffer::from_str(""));
        j.snapshot_tabs(&tm).unwrap();
        assert!(j.recover().is_empty());
    }

    #[test]
    fn lazy_document_is_journaled_as_pieces_not_as_a_copy() {
        let d = tempfile::tempdir().unwrap();
        let files = tempfile::tempdir().unwrap();
        let p = files.path().join("big.log");
        let base: String = (0..20_000).map(|i| format!("line {i} of the log\n")).collect();
        fs::write(&p, &base).unwrap();
        let mut tm = TabManager::new();
        tm.open_file(&p).unwrap();
        {
            let t = tm.current_mut();
            let mut b = Buffer::open_lazy(&p).unwrap();
            b.insert_at(10, "TYPED ");
            t.buffer = std::sync::Arc::new(b);
            t.dirty = true;
        }
        let j = Journal::new(d.path());
        assert_eq!(j.snapshot_tabs(&tm).unwrap(), 1);
        assert!(j.is_current(&tm));
        let files_in_journal: Vec<_> = fs::read_dir(d.path()).unwrap().flatten().map(|e| e.file_name().to_string_lossy().into_owned()).collect();
        assert!(files_in_journal.iter().any(|n| n.ends_with(".lzj")) && !files_in_journal.iter().any(|n| n.ends_with(".txt")), "{files_in_journal:?}");
        let jf = d.path().read_dir().unwrap().flatten().find(|e| e.file_name().to_string_lossy().ends_with(".lzj")).unwrap();
        assert!(jf.metadata().unwrap().len() < 1024, "journal must not copy the {}-byte document", base.len());
        // "crash": a new process rebuilds the document from the journal on the untouched base file
        let docs = Journal::new(d.path()).recover();
        assert_eq!(docs.len(), 1);
        assert!(docs[0].lazy_journal.is_some());
        let mut tm2 = TabManager::new();
        assert_eq!(apply_recovered(&mut tm2, &docs, false), 1);
        let t = tm2.current_mut();
        assert!(t.dirty && t.buffer.is_lazy());
        let mut want = base.clone();
        want.insert_str(10, "TYPED ");
        assert_eq!(t.buffer.byte_len(), want.len());
        assert_eq!(t.buffer.slice(0..40), want[..40]);
        // base changed meanwhile: refuse the rebuild, fall back to the typed text
        fs::write(&p, "something else entirely\n").unwrap();
        let docs = Journal::new(d.path()).recover();
        let mut tm3 = TabManager::new();
        apply_recovered(&mut tm3, &docs, false);
        assert!(!tm3.current_mut().buffer.is_lazy());
        assert_eq!(tm3.current_mut().buffer.text(), "TYPED \n");
    }

    #[test]
    fn autosaver_interval() {
        let mut a = AutoSaver::new(Duration::from_secs(30));
        let t0 = a.last;
        assert!(!a.due(t0 + Duration::from_secs(29)));
        assert!(a.due(t0 + Duration::from_secs(30)));
        assert!(!a.due(t0 + Duration::from_secs(31)));
    }
}
