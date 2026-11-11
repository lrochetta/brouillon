//! Session persistence — restore open tabs across launches.
//!
//! M1 spec: on window close, serialize the file-backed tabs (path + cursor
//! position) to `~/.brouillon/session.toml`. On next launch, re-open each
//! file from disk and re-anchor the caret. Scratch (untitled-N) tabs are
//! NOT persisted in M1 — the buffer contents would have to live in the
//! session file, which is out of scope for this round.
//!
//! Design notes (G4):
//! - We deliberately do NOT serialize `Buffer` or `dirty` flags. The buffer
//!   is reloaded from disk on restore, which means external edits since the
//!   last close are picked up (correct behaviour — the editor isn't a VCS).
//!   `dirty` is always reset to `false` on restore for the same reason.
//! - Durable write via `durable::durable_write` (temp + fsync + atomic
//!   rename): never leaves a half-written file if the process is killed.
//! - An empty session (zero file-tabs) deletes the session file rather
//!   than writing an empty one. A stale file would mislead a future "did
//!   I leave anything open?" inspection.
//! - The core save/load logic accepts a base directory so tests can drive
//!   the module against a tempdir without touching the real `~/.brouillon`.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::tabs::TabManager;

/// Current session schema. v1 (no `version` key) had only path + caret.
pub const SCHEMA_VERSION: u32 = 2;

fn legacy_version() -> u32 {
    1
}

/// Top-level serialized session.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionSnapshot {
    /// Missing in v1 files; `load_from_dir` migrates it to `SCHEMA_VERSION`.
    #[serde(default = "legacy_version")]
    pub version: u32,
    /// Index into `tabs` of the tab that should be active on restore.
    /// Clamped on load — stored value may be stale if files have been
    /// deleted since the last save.
    pub active_idx: usize,
    /// Tabs in left-to-right order: file-backed tabs plus non-empty scratch
    /// tabs (their text lives in `TabSnapshot::scratch_text`).
    pub tabs: Vec<TabSnapshot>,
}

/// Per-tab serialized state. Every field after the caret is optional in the
/// file (`serde(default)`) so v1 files still parse.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct TabSnapshot {
    /// `None` for a persisted scratch tab (then `scratch_text` is set).
    pub path: Option<PathBuf>,
    pub cursor_line: usize,
    pub cursor_col: usize,
    /// First visible line. Applied by the view layer, not by `restore_into`.
    #[serde(default)]
    pub scroll_line: usize,
    /// Folded start lines. Applied by the view layer.
    #[serde(default)]
    pub folds: Vec<usize>,
    #[serde(default)]
    pub markers: Vec<usize>,
    #[serde(default)]
    pub wrap: bool,
    #[serde(default)]
    pub readonly: bool,
    /// Title of a scratch tab.
    #[serde(default)]
    pub title: Option<String>,
    /// Content of a scratch tab (hot exit).
    #[serde(default)]
    pub scratch_text: Option<String>,
    /// Unsaved drawing (`SketchDocument` JSON), kept so closing the app never
    /// loses strokes. Only written while the tab has strokes.
    #[serde(default)]
    pub sketch_json: Option<String>,
}

/// Name of the session file inside the session directory.
const SESSION_FILE: &str = "session.toml";

/// Resolve `~/.brouillon/session.toml`. Returns `None` if `dirs::home_dir`
/// can't find a home directory (rare — sandboxed services, broken env).
pub fn session_path() -> Option<PathBuf> {
    dirs::home_dir().map(|home| home.join(".brouillon").join(SESSION_FILE))
}

/// Save the file-backed tabs of `tabs` to `~/.brouillon/session.toml`.
///
/// Thin wrapper over `save_to_dir` that resolves the session directory.
/// Returns `Ok(())` and does nothing if the home directory is undiscoverable
/// — we don't want a sandboxed service to crash on shutdown.
pub fn save(tabs: &TabManager) -> io::Result<()> {
    let Some(dir) = session_dir() else {
        return Ok(());
    };
    save_to_dir(tabs, &dir)
}

/// Load a previously saved session from `~/.brouillon/session.toml`.
///
/// Returns `Ok(None)` if the file doesn't exist, the home dir is
/// undiscoverable, or the file is empty. Other I/O / parse errors are
/// surfaced so the caller can log them (the integrator decides whether
/// to show a UI toast or silently fall back to a fresh scratch).
pub fn load() -> io::Result<Option<SessionSnapshot>> {
    let Some(dir) = session_dir() else {
        return Ok(None);
    };
    load_from_dir(&dir)
}

/// Apply a loaded snapshot to `tm`. Returns the number of tabs that were
/// successfully restored — tabs whose files have disappeared since the
/// last session are skipped (and logged to stderr).
///
/// The caller is expected to invoke this on a fresh `TabManager::new()` —
/// the freshly-created scratch tab will be replaced in-place by the first
/// `open_file` call (per `open_file`'s pristine-scratch semantics).
pub fn restore_into(snap: SessionSnapshot, tm: &mut TabManager) -> usize {
    restore_with_report(snap, tm).restored
}

/// Outcome of a restore: how many tabs came back and which files were skipped.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct RestoreReport {
    pub restored: usize,
    /// (path, reason) for files that could not be reopened.
    pub skipped: Vec<(PathBuf, String)>,
}

/// Like `restore_into` but reports the skipped files. Also reapplies markers,
/// read-only and persisted scratch tabs.
pub fn restore_with_report(snap: SessionSnapshot, tm: &mut TabManager) -> RestoreReport {
    let mut report = RestoreReport::default();
    let mut restored: Vec<(usize, &TabSnapshot)> = Vec::with_capacity(snap.tabs.len());
    for tab in &snap.tabs {
        let idx = match &tab.path {
            Some(path) => match tm.open_file(path) {
                Ok(idx) => idx,
                Err(e) => {
                    report.skipped.push((path.clone(), e.to_string()));
                    continue;
                }
            },
            None => {
                // Reuse the pristine startup scratch instead of stacking a new one.
                let t = tm.current();
                let idx = if tm.count() == 1 && t.path.is_none() && !t.dirty && t.buffer.byte_len() == 0 {
                    tm.active_idx()
                } else {
                    tm.new_scratch()
                };
                let t = tm.current_mut();
                // Unsaved text: the BUFFER must be dirty, the tab flag alone is
                // recomputed from it by the mirror sync.
                let mut buf = crate::buffer::Buffer::from_str(tab.scratch_text.as_deref().unwrap_or(""));
                if buf.byte_len() > 0 {
                    buf.mark_dirty();
                }
                t.buffer = std::sync::Arc::new(buf);
                if let Some(title) = &tab.title {
                    t.title = title.clone();
                }
                t.dirty = true;
                idx
            }
        };
        restored.push((idx, tab));
    }
    // Re-anchor each tab's caret in a second pass so we operate on stable
    // indices after every open_file has completed.
    for (idx, tab) in &restored {
        tm.set_cursor(*idx, (tab.cursor_line, tab.cursor_col));
        tm.switch_to(*idx);
        let t = tm.current_mut();
        if let Some(doc) = tab.sketch_json.as_deref().and_then(|j| crate::sketch::SketchDocument::from_json(j).ok()) {
            if doc.stroke_count() > 0 && !doc.pages.is_empty() {
                t.sketch = doc;
                t.sketch_saved = 0; // never written to disk: still unsaved
                t.dirty = true;
            }
        }
        t.meta.markers = tab.markers.clone();
        t.meta.readonly |= tab.readonly;
    }
    // Clamp active index to a valid slot. If everything failed, we leave
    // `tm` in whatever state open_file left it (a single fresh scratch).
    if !restored.is_empty() {
        let last = tm.count().saturating_sub(1);
        tm.switch_to(snap.active_idx.min(last));
    }
    report.restored = restored.len();
    report
}

/// Bring an older snapshot up to `SCHEMA_VERSION`. v1 → v2 only adds
/// defaulted fields, so migration is just the version bump.
pub fn migrate(mut snap: SessionSnapshot) -> SessionSnapshot {
    snap.version = SCHEMA_VERSION;
    snap
}

/// Reopen-last-closed-tab stack (caret and scroll are kept in the snapshot).
#[derive(Debug, Default)]
pub struct ClosedTabs(Vec<TabSnapshot>);

impl ClosedTabs {
    const CAP: usize = 20;

    pub fn push(&mut self, tab: TabSnapshot) {
        self.0.push(tab);
        if self.0.len() > Self::CAP {
            self.0.remove(0);
        }
    }

    /// Reopen the most recently closed tab; `None` when empty or the file is gone.
    pub fn reopen(&mut self, tm: &mut TabManager) -> Option<(usize, TabSnapshot)> {
        let tab = self.0.pop()?;
        let snap = SessionSnapshot { version: SCHEMA_VERSION, active_idx: 0, tabs: vec![tab.clone()] };
        if restore_with_report(snap, tm).restored == 0 {
            return None;
        }
        Some((tm.active_idx(), tab))
    }
}

/// Save the file-backed tabs of `tabs` to `<dir>/session.toml`. Creates
/// `<dir>` if missing. If the snapshot would be empty (zero file-tabs),
/// removes any existing session file instead of writing an empty one.
pub fn save_to_dir(tabs: &TabManager, dir: &Path) -> io::Result<()> {
    let snap = snapshot_from(tabs);
    let target = dir.join(SESSION_FILE);

    if snap.tabs.is_empty() {
        // Nothing worth persisting — clean up any stale file. NotFound is
        // expected on first run, so it's not an error.
        match fs::remove_file(&target) {
            Ok(()) => {}
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(e) => return Err(e),
        }
        return Ok(());
    }

    fs::create_dir_all(dir)?;
    let serialized = toml::to_string_pretty(&snap)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;

    // Durable write: temp file + fsync + atomic rename (see `durable`).
    crate::durable::durable_write(&target, serialized.as_bytes())
}

/// Load `<dir>/session.toml`. Returns `Ok(None)` if the file doesn't exist
/// (first launch, or session was empty last time).
pub fn load_from_dir(dir: &Path) -> io::Result<Option<SessionSnapshot>> {
    let target = dir.join(SESSION_FILE);
    let raw = match fs::read_to_string(&target) {
        Ok(s) => s,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e),
    };
    if raw.trim().is_empty() {
        return Ok(None);
    }
    let snap: SessionSnapshot = toml::from_str(&raw)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
    Ok(Some(migrate(snap)))
}

/// Resolve `~/.brouillon/` (the parent of `SESSION_FILE`).
fn session_dir() -> Option<PathBuf> {
    dirs::home_dir().map(|home| home.join(".brouillon"))
}

/// Build a `SessionSnapshot` from a live `TabManager`, filtering scratch
/// tabs and re-mapping the active index to the filtered vector.
fn snapshot_from(tabs: &TabManager) -> SessionSnapshot {
    let original_active = tabs.active_idx();
    let mut new_active: usize = 0;
    let mut snap_tabs: Vec<TabSnapshot> = Vec::new();

    for (orig_idx, t) in tabs.iter().enumerate() {
        if t.help_doc.is_some() {
            continue; // built-in help: rebuilt from the program, never a file or a scratch tab
        }
        let scratch_text = (t.path.is_none() && t.remote_path.is_none() && t.buffer.byte_len() > 0)
            .then(|| t.buffer.text());
        // Unsaved strokes are worth persisting on their own (a drawing-only tab).
        let sketch_json = (t.remote_path.is_none() && t.sketch_unsaved() && t.sketch.stroke_count() > 0)
            .then(|| t.sketch.to_json().ok())
            .flatten();
        if t.path.is_none() && scratch_text.is_none() && sketch_json.is_none() {
            continue; // empty scratch or remote tab: nothing worth persisting
        }
        let drawing_only_scratch = t.path.is_none() && scratch_text.is_none();
        // The about-to-be-pushed slot is `snap_tabs.len()`. If the original
        // active tab is THIS one, that's our new active index. If the
        // original active was a scratch (and thus filtered out), we fall
        // back to the closest surviving file-tab — that ends up being
        // either `snap_tabs.len()` (the next one after the scratch) or
        // `snap_tabs.len() - 1` (the previous one) depending on order.
        if orig_idx == original_active {
            new_active = snap_tabs.len();
        } else if orig_idx < original_active {
            // Pre-active file tabs — if the active is a scratch we'll
            // never hit the `==` branch, so keep tracking the last
            // surviving pre-active index as a fallback.
            new_active = snap_tabs.len();
        }
        snap_tabs.push(TabSnapshot {
            path: t.path.clone(),
            cursor_line: t.cursor.0,
            cursor_col: t.cursor.1,
            markers: t.meta.markers.clone(),
            readonly: t.meta.readonly,
            title: (scratch_text.is_some() || drawing_only_scratch).then(|| t.title.clone()),
            scratch_text,
            sketch_json,
            ..Default::default()
        });
    }

    // If the active was a scratch and there are no pre-active file tabs,
    // `new_active` is 0 — which is the correct fallback (first surviving
    // tab). Clamp defensively in case snap_tabs ended up empty.
    if !snap_tabs.is_empty() {
        new_active = new_active.min(snap_tabs.len() - 1);
    }

    SessionSnapshot {
        version: SCHEMA_VERSION,
        active_idx: new_active,
        tabs: snap_tabs,
    }
}

// ─── Recent Files ────────────────────────────────────────────────────────────

const RECENT_FILE: &str = "recent.toml";
const MAX_RECENT: usize = 10;

/// Serialized recent-files list.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct RecentFilesList {
    paths: Vec<PathBuf>,
}

/// Load the recent-files list from `~/.brouillon/recent.toml`.
/// Returns an empty Vec if the file doesn't exist or can't be parsed.
pub fn load_recent() -> Vec<PathBuf> {
    let Some(dir) = session_dir() else {
        return Vec::new();
    };
    load_recent_from_dir(&dir)
}

/// Push a path to the recent-files list. Deduplicates and truncates to 10.
/// Saves immediately to disk.
pub fn push_recent(path: &Path) {
    let Some(dir) = session_dir() else {
        return;
    };
    let mut recent = load_recent_from_dir(&dir);
    // Remove existing entry if present (dedup) to bring it to the front
    recent.retain(|p| p != path);
    // Insert at front (most recent first)
    recent.insert(0, path.to_path_buf());
    // Truncate to MAX_RECENT
    recent.truncate(MAX_RECENT);
    let _ = save_recent_to_dir(&recent, &dir);
}

/// Load recent files from a specific directory (testable).
pub fn load_recent_from_dir(dir: &Path) -> Vec<PathBuf> {
    let target = dir.join(RECENT_FILE);
    let raw = match fs::read_to_string(&target) {
        Ok(s) => s,
        Err(_) => return Vec::new(),
    };
    if raw.trim().is_empty() {
        return Vec::new();
    }
    let list: RecentFilesList = match toml::from_str(&raw) {
        Ok(l) => l,
        Err(_) => return Vec::new(),
    };
    list.paths
}

/// Save recent files to a specific directory (testable).
pub fn save_recent_to_dir(paths: &[PathBuf], dir: &Path) -> io::Result<()> {
    let list = RecentFilesList {
        paths: paths.to_vec(),
    };
    let _ = fs::create_dir_all(dir);
    let serialized = toml::to_string_pretty(&list)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
    crate::durable::durable_write(&dir.join(RECENT_FILE), serialized.as_bytes())
}

#[cfg(test)]
mod tests {

    #[test]
    fn help_tabs_are_never_written_to_the_session() {
        let dir = make_tempdir("help-tabs");
        let a = write_file(&dir, "a.txt", "alpha\n");
        let mut tm = TabManager::new();
        tm.open_help("welcome", "Welcome", "# Welcome\ntext");
        tm.open_file(&a).unwrap();
        tm.open_help("release-notes", "Release notes", "notes");
        assert_eq!(tm.count(), 3);
        save_to_dir(&tm, &dir).unwrap();
        let snap = load_from_dir(&dir).unwrap().expect("the file tab is persisted");
        assert_eq!(snap.tabs.len(), 1, "only the real file: {snap:?}");
        assert_eq!(snap.tabs[0].path.as_deref(), Some(a.as_path()));
        assert!(snap.tabs.iter().all(|t| t.scratch_text.is_none() && t.title.is_none()));
        // Help tabs alone: no session file at all (nothing to duplicate on restore).
        let mut only = TabManager::new();
        only.open_help("docs", "Documentation", "docs");
        save_to_dir(&only, &dir).unwrap();
        assert!(load_from_dir(&dir).unwrap().is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }
    use super::*;
    use std::io::Write;
    use std::sync::atomic::{AtomicU64, Ordering};

    /// Monotonic suffix to avoid collisions when several tests run in the
    /// same process (cargo test does this by default).
    static SUFFIX: AtomicU64 = AtomicU64::new(0);

    fn unique_suffix() -> u64 {
        SUFFIX.fetch_add(1, Ordering::Relaxed)
    }

    /// Create a unique tempdir for a test. Caller is responsible for
    /// cleanup via `remove_dir_all`.
    fn make_tempdir(tag: &str) -> PathBuf {
        let mut dir = std::env::temp_dir();
        dir.push(format!(
            "brouillon-session-test-{}-{}-{}",
            std::process::id(),
            tag,
            unique_suffix()
        ));
        std::fs::create_dir_all(&dir).expect("create tempdir");
        dir
    }

    /// Write a small text file inside `dir` and return its absolute path.
    fn write_file(dir: &Path, name: &str, content: &str) -> PathBuf {
        let path = dir.join(name);
        let mut f = std::fs::File::create(&path).expect("create temp file");
        f.write_all(content.as_bytes()).expect("write temp file");
        path
    }

    #[test]
    fn session_path_under_home_dir_when_available() {
        // We can't assume a home dir exists in every test environment, but
        // if it does, the path must end with `.brouillon/session.toml`.
        if let Some(p) = session_path() {
            let suffix: PathBuf = [".brouillon", "session.toml"].iter().collect();
            assert!(
                p.ends_with(&suffix),
                "session_path() = {:?} should end with {:?}",
                p,
                suffix
            );
        }
    }

    #[test]
    fn snapshot_roundtrip_via_toml() {
        let snap = SessionSnapshot {
            active_idx: 1,
            version: SCHEMA_VERSION,
            tabs: vec![
                TabSnapshot {
                    path: Some(PathBuf::from("/tmp/a.rs")),
                    cursor_line: 3,
                    cursor_col: 14,
                    ..Default::default()
                },
                TabSnapshot {
                    path: Some(PathBuf::from("/tmp/b.rs")),
                    cursor_line: 99,
                    cursor_col: 1,
                    ..Default::default()
                },
            ],
        };
        let s = toml::to_string_pretty(&snap).expect("serialize");
        let parsed: SessionSnapshot = toml::from_str(&s).expect("deserialize");
        assert_eq!(parsed, snap);
    }

    #[test]
    fn snapshot_skips_scratch_tabs() {
        let dir = make_tempdir("skip-scratch");
        let a = write_file(&dir, "a.txt", "alpha\n");
        let b = write_file(&dir, "b.txt", "beta\n");

        let mut tm = TabManager::new(); // 1 scratch
        tm.open_file(&a).unwrap(); // replaces the scratch (count=1)
        tm.new_scratch(); // appends untitled-2 (count=2, active=1)
        tm.open_file(&b).unwrap(); // active scratch is pristine -> replaced (count=2)
        // tabs are now: [a.txt, b.txt] — both file-backed. Add a scratch
        // explicitly to exercise the filter.
        tm.new_scratch(); // count=3, active=2, tab[2] is scratch

        save_to_dir(&tm, &dir).expect("save");
        let snap = load_from_dir(&dir).expect("load").expect("some snapshot");
        assert_eq!(snap.tabs.len(), 2, "scratch tab should be filtered out");
        assert!(snap.tabs.iter().all(|t| t.path.is_some()));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn save_then_load_roundtrip() {
        let dir = make_tempdir("roundtrip");
        let a = write_file(&dir, "a.txt", "alpha\n");
        let b = write_file(&dir, "b.txt", "beta\n");

        let mut tm = TabManager::new();
        tm.open_file(&a).unwrap();
        tm.open_file(&b).unwrap();
        tm.set_cursor(0, (7, 3));
        tm.set_cursor(1, (42, 11));
        tm.switch_to(0);

        save_to_dir(&tm, &dir).expect("save");
        let snap = load_from_dir(&dir).expect("load").expect("some snapshot");

        assert_eq!(snap.active_idx, 0);
        assert_eq!(snap.tabs.len(), 2);
        assert_eq!(snap.tabs[0].path.as_ref().unwrap(), &a);
        assert_eq!(snap.tabs[0].cursor_line, 7);
        assert_eq!(snap.tabs[0].cursor_col, 3);
        assert_eq!(snap.tabs[1].path.as_ref().unwrap(), &b);
        assert_eq!(snap.tabs[1].cursor_line, 42);
        assert_eq!(snap.tabs[1].cursor_col, 11);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn restore_into_skips_deleted_files() {
        let dir = make_tempdir("restore-missing");
        let real = write_file(&dir, "real.txt", "hello\n");
        let missing = dir.join("ghost.txt"); // never created

        let snap = SessionSnapshot {
            active_idx: 0,
            version: SCHEMA_VERSION,
            tabs: vec![
                TabSnapshot {
                    path: Some(real.clone()),
                    cursor_line: 2,
                    cursor_col: 4,
                    ..Default::default()
                },
                TabSnapshot {
                    path: Some(missing.clone()),
                    cursor_line: 1,
                    cursor_col: 1,
                    ..Default::default()
                },
            ],
        };

        let mut tm = TabManager::new();
        let restored = restore_into(snap, &mut tm);
        assert_eq!(restored, 1, "only the real file should be restored");
        assert_eq!(tm.count(), 1);
        assert_eq!(tm.current().path.as_ref(), Some(&real));
        assert_eq!(tm.current().cursor, (2, 4));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn empty_snapshot_removes_session_file() {
        let dir = make_tempdir("empty");

        // First, write a stale session by hand so we can prove `save_to_dir`
        // removes it when there's nothing to save.
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join(SESSION_FILE), "stale = true\n").unwrap();
        assert!(dir.join(SESSION_FILE).exists());

        // A fresh TabManager has 1 scratch -> zero file-tabs.
        let tm = TabManager::new();
        save_to_dir(&tm, &dir).expect("save");
        assert!(
            !dir.join(SESSION_FILE).exists(),
            "stale session file should have been removed"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn load_returns_none_when_file_missing() {
        let dir = make_tempdir("missing");
        let out = load_from_dir(&dir).expect("load should not error on missing file");
        assert!(out.is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn set_cursor_updates_tab_state() {
        let mut tm = TabManager::new();
        tm.new_scratch(); // count=2
        tm.set_cursor(0, (10, 5));
        // Reach into tab 0 via iter to assert without adding a new accessor.
        let cursors: Vec<_> = tm.iter().map(|t| t.cursor).collect();
        assert_eq!(cursors[0], (10, 5));
        assert_eq!(cursors[1], (1, 1));

        // Out-of-range is a no-op (defensive).
        tm.set_cursor(999, (1, 1));
        assert_eq!(tm.count(), 2);
    }

    // ─── Recent Files tests ──────────────────────────────────────────────

    #[test]
    fn recent_files_push_and_load() {
        let dir = make_tempdir("recent-push");
        let p1 = PathBuf::from("/tmp/file1.rs");
        let p2 = PathBuf::from("/tmp/file2.rs");

        // Push two files
        let mut recent = Vec::new();
        recent.insert(0, p1.clone());
        recent.insert(0, p2.clone());
        recent.truncate(MAX_RECENT);
        save_recent_to_dir(&recent, &dir).unwrap();

        let loaded = load_recent_from_dir(&dir);
        assert_eq!(loaded.len(), 2);
        assert_eq!(loaded[0], p2); // most recent first
        assert_eq!(loaded[1], p1);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn recent_files_dedup_on_push() {
        let dir = make_tempdir("recent-dedup");
        let p1 = PathBuf::from("/tmp/dup.rs");
        let p2 = PathBuf::from("/tmp/other.rs");

        let mut recent = vec![p2.clone(), p1.clone()];
        // Re-push p1 — should move to front
        recent.retain(|p| p != &p1);
        recent.insert(0, p1.clone());
        recent.truncate(MAX_RECENT);

        save_recent_to_dir(&recent, &dir).unwrap();
        let loaded = load_recent_from_dir(&dir);
        assert_eq!(loaded.len(), 2);
        assert_eq!(loaded[0], p1); // deduped, moved to front

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn recent_files_truncate_at_max() {
        let dir = make_tempdir("recent-truncate");
        let paths: Vec<PathBuf> = (0..15)
            .map(|i| PathBuf::from(format!("/tmp/file{}.rs", i)))
            .collect();

        save_recent_to_dir(&paths[..MAX_RECENT + 3], &dir).unwrap();
        let loaded = load_recent_from_dir(&dir);
        // We saved 13, but that's fine — MAX_RECENT is enforced at push time
        assert_eq!(loaded.len(), 13);

        // Now push with truncation
        let mut capped = paths;
        capped.truncate(MAX_RECENT);
        save_recent_to_dir(&capped, &dir).unwrap();
        let loaded = load_recent_from_dir(&dir);
        assert_eq!(loaded.len(), MAX_RECENT);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn recent_files_empty_when_missing() {
        let dir = make_tempdir("recent-empty");
        let loaded = load_recent_from_dir(&dir);
        assert!(loaded.is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn snapshot_serde_roundtrip() {
        let snap = SessionSnapshot {
            active_idx: 1,
            version: SCHEMA_VERSION,
            tabs: vec![
                TabSnapshot { path: Some("/test/file.rs".into()), cursor_line: 10, cursor_col: 5, ..Default::default() },
            ],
        };
        let toml_str = toml::to_string(&snap).unwrap();
        let parsed: SessionSnapshot = toml::from_str(&toml_str).unwrap();
        assert_eq!(snap, parsed);
    }

    #[test]
    fn empty_snapshot_serializes() {
        let snap = SessionSnapshot { active_idx: 0, tabs: vec![], version: SCHEMA_VERSION };
        let toml_str = toml::to_string(&snap).unwrap();
        assert!(toml_str.contains("active_idx"));
    }

    #[test]
    fn tab_snapshot_clone_eq() {
        let e = TabSnapshot { path: Some("/tmp/x.rs".into()), cursor_line: 1, cursor_col: 1, ..Default::default() };
        let e2 = e.clone();
        assert_eq!(e, e2);
    }

    #[test]
    fn max_recent_is_positive() {
        assert!(MAX_RECENT > 0);
        assert!(MAX_RECENT <= 100, "Unreasonably large recent files limit");
    }

    #[test]
    fn session_snapshot_debug() {
        let snap = SessionSnapshot { active_idx: 0, tabs: vec![], version: SCHEMA_VERSION };
        let dbg = format!("{:?}", snap);
        assert!(dbg.contains("SessionSnapshot"));
    }

    #[test]
    fn v1_file_migrates_to_v2() {
        let dir = make_tempdir("v1");
        std::fs::write(
            dir.join(SESSION_FILE),
            "active_idx = 0\n[[tabs]]\npath = \"/x/a.rs\"\ncursor_line = 3\ncursor_col = 4\n",
        )
        .unwrap();
        let snap = load_from_dir(&dir).unwrap().unwrap();
        assert_eq!(snap.version, SCHEMA_VERSION);
        assert_eq!(snap.tabs[0].cursor_line, 3);
        assert!(snap.tabs[0].folds.is_empty() && !snap.tabs[0].wrap);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn scratch_tab_text_survives_hot_exit_and_missing_files_are_reported() {
        let dir = make_tempdir("hot");
        let a = write_file(&dir, "a.txt", "hello");
        let mut tm = TabManager::new();
        tm.open_file(&a).unwrap();
        tm.new_scratch();
        tm.current_mut().buffer = std::sync::Arc::new(crate::buffer::Buffer::from_str("draft"));
        tm.current_mut().meta.markers = vec![2];
        save_to_dir(&tm, &dir).unwrap();

        std::fs::remove_file(&a).unwrap();
        let snap = load_from_dir(&dir).unwrap().unwrap();
        assert_eq!(snap.tabs.len(), 2);
        let mut fresh = TabManager::new();
        let rep = restore_with_report(snap, &mut fresh);
        assert_eq!(rep.restored, 1);
        assert_eq!(rep.skipped.len(), 1);
        assert_eq!(fresh.count(), 1, "pristine startup scratch is reused");
        assert_eq!(fresh.current().buffer.rope().to_string(), "draft");
        assert_eq!(fresh.current().meta.markers, vec![2]);
        std::fs::remove_dir_all(&dir).ok();
    }

    /// Crayon mode: strokes on a text-less scratch tab (and on a file tab) are
    /// written to the session and come back, still unsaved/modified.
    #[test]
    fn unsaved_strokes_survive_the_session() {
        use crate::sketch::{SketchColor, SketchTool, Stroke, StrokePoint};
        let stroke = Stroke {
            points: vec![StrokePoint { x: 1.0, y: 2.0, pressure: 1.0, tilt_x: 0.0, tilt_y: 0.0 }],
            tool: SketchTool::Ballpoint,
            color: SketchColor::Black,
            width: 2.0,
            layer_idx: 0,
            is_eraser: false,
        };
        let dir = make_tempdir("strokes");
        let a = write_file(&dir, "a.txt", "hello");
        let mut tm = TabManager::new();
        tm.open_file(&a).unwrap();
        tm.current_mut().sketch.active_page_mut().strokes.push(stroke.clone());
        tm.new_scratch();
        tm.current_mut().sketch.active_page_mut().strokes.push(stroke.clone());
        tm.current_mut().sketch.active_page_mut().strokes.push(stroke);
        let title = tm.current().title.clone();
        save_to_dir(&tm, &dir).unwrap();

        let snap = load_from_dir(&dir).unwrap().unwrap();
        assert_eq!(snap.tabs.len(), 2, "a drawing-only scratch tab is persisted");
        let mut fresh = TabManager::new();
        assert_eq!(restore_with_report(snap, &mut fresh).restored, 2);
        let counts: Vec<_> = fresh.iter().map(|t| (t.sketch.stroke_count(), t.dirty, t.sketch_unsaved())).collect();
        assert_eq!(counts, vec![(1, true, true), (2, true, true)]);
        assert_eq!(fresh.current().title, title);
        assert_eq!(fresh.current().buffer.byte_len(), 0);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn reopen_closed_tab_restores_caret() {
        let dir = make_tempdir("reopen");
        let a = write_file(&dir, "a.txt", "l1\nl2\n");
        let mut tm = TabManager::new();
        let mut closed = ClosedTabs::default();
        closed.push(TabSnapshot { path: Some(a), cursor_line: 2, cursor_col: 1, ..Default::default() });
        let (idx, _) = closed.reopen(&mut tm).unwrap();
        assert_eq!(tm.current().cursor, (2, 1));
        assert_eq!(idx, tm.active_idx());
        assert!(closed.reopen(&mut tm).is_none());
        std::fs::remove_dir_all(&dir).ok();
    }
}
