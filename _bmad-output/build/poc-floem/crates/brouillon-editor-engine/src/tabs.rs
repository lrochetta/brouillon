#![allow(dead_code)]

//! Multi-tab document model. M1 milestone. The view layer wires into this
//! next round — here we just ship the data + open-dialog.
//!
//! Design notes (G4):
//! - `Arc<Buffer>` so future split views / preview panes can share a buffer
//!   without re-loading. `Buffer` is already `Clone` (rope is structurally
//!   shared), but `Arc` lets the view layer hand out references without
//!   triggering rope clones on every render.
//! - Cursor is `(line, col)` 1-based to match the status-line convention
//!   the view already uses ; the view-layer-wire-up agent reads/writes this
//!   when switching tabs so each tab remembers its caret.
//! - `dirty` is set by the view layer on edits — this module does not
//!   inspect buffer contents to detect mutations.

use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::buffer::Buffer;
use brouillon_core::eol::EolKind;

/// External-change state of the file on disk (Lot 7 writes, others read).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ExternalState {
    #[default]
    InSync,
    Modified,
    Deleted,
    ReadonlyFlipped,
}

/// Per-document metadata (Step 0, reconciled with Lot 2 by the orchestrator).
/// Lots touch only these fields (via accessors); `tabs.rs` stays orchestrator-owned.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DocMeta {
    /// Codepage id (`brouillon_core::codepages::TABLE`) the file was read with.
    pub codepage: &'static str,
    /// File started with a BOM (preserved on save).
    pub bom: bool,
    /// Dominant EOL kind (Lot 2).
    pub eol_kind: EolKind,
    /// More than one EOL kind was found on open.
    pub eol_mixed: bool,
    pub readonly: bool,
    /// Large-file profile active (Lot 2).
    pub large_file: bool,
    pub external_state: ExternalState,
    /// 0-based marker/bookmark lines, sorted (Lot 6).
    pub markers: Vec<usize>,
}

impl Default for DocMeta {
    fn default() -> Self {
        Self {
            codepage: "utf-8",
            bom: false,
            eol_kind: EolKind::Lf,
            eol_mixed: false,
            readonly: false,
            large_file: false,
            external_state: ExternalState::InSync,
            markers: Vec::new(),
        }
    }
}

impl DocMeta {
    /// Metadata of a freshly decoded file.
    pub fn from_loaded(doc: &crate::file_io::LoadedDoc) -> Self {
        Self {
            codepage: doc.codepage,
            bom: doc.bom,
            eol_kind: doc.eol_kind().unwrap_or(EolKind::Lf),
            eol_mixed: doc.eol_mixed(),
            ..Self::default()
        }
    }

    /// Legacy two-state EOL view (`Cr` maps to `Lf`) for callers that still use `EolMode`.
    pub fn eol_mode(&self) -> brouillon_core::encoding::EolMode {
        match self.eol_kind {
            EolKind::Crlf => brouillon_core::encoding::EolMode::Crlf,
            _ => brouillon_core::encoding::EolMode::Lf,
        }
    }

    /// Legacy four-variant encoding view for callers that still use `DocumentEncoding`.
    pub fn legacy_encoding(&self) -> brouillon_core::encoding::DocumentEncoding {
        use brouillon_core::encoding::DocumentEncoding as E;
        match self.codepage {
            "utf-16le" => E::Utf16Le,
            "utf-16be" => E::Utf16Be,
            "windows-1252" => E::Windows1252,
            _ => E::Utf8,
        }
    }
}

/// Per-tab state. Owned by `TabManager`. View layer reads `buffer` for
/// rendering and writes `cursor` / `dirty` on user input.
#[derive(Debug)]
pub struct TabState {
    /// `None` = unsaved scratch buffer (untitled-N). Becomes `Some` after
    /// the first successful save-as.
    pub path: Option<PathBuf>,
    pub remote_path: Option<String>,
    /// Set for a document that came from a server through the remote explorer. Drives the `~site`
    /// tab chip and routes Save to the remote save path. Never holds a secret.
    pub remote_origin: Option<crate::remote::RemoteOrigin>,
    pub buffer: Arc<Buffer>,
    /// 1-based (line, col). Default for a fresh buffer is (1, 1).
    pub cursor: (usize, usize),
    pub dirty: bool,
    /// Friendly title shown in the tab bar — file name or "untitled-N".
    pub title: String,
    /// Individual drawing canvas document for each tab.
    pub sketch: crate::sketch::SketchDocument,
    /// Stroke count of the drawing the last time it was written to disk (or
    /// loaded from it). Strokes beyond that are unsaved work: they make the tab
    /// modified, exactly like unsaved text.
    pub sketch_saved: usize,
    pub encoding: brouillon_core::encoding::DocumentEncoding,
    pub eol: brouillon_core::encoding::EolMode,
    pub meta: DocMeta,
    /// `Some(id)` for a built-in help document (Welcome, Documentation, Release
    /// notes). Such a tab has no file, is never marked modified, never asks to be
    /// saved on close, and is left out of the session and the recovery journal:
    /// it is rebuilt from the program every time it is opened.
    pub help_doc: Option<&'static str>,
    /// What the bytes are (detected at open from the file head; `Text` for scratch, remote and help tabs). A routed kind (PDF, Word...) opens on an empty read-only placeholder: the UI shows the real
    /// bytes in the hex view and, for a kind with a module, the offer bar.
    pub kind: brouillon_core::kind::FileKind,
}

/// Open `path` on the lazy text store when it is big enough (`[files] lazy_threshold_mb`) and looks like UTF-8:
/// nothing is read, memory does not depend on the file size ("large file mode"). `Ok(None)` = use the normal
/// path (small file, threshold 0). UTF-16/32 and legacy codepages are transcoded to a sidecar (`stream_codec`).
pub fn try_open_lazy(path: &Path) -> io::Result<Option<(Buffer, DocMeta)>> {
    use std::io::Read;
    let len = std::fs::metadata(path)?.len();
    if !crate::large_file::use_lazy(len) {
        return Ok(None);
    }
    if !crate::large_file::sniff_utf8_for_lazy(path)? {
        // UTF-16/32 or a legacy codepage: transcode once, streaming, to a UTF-8 sidecar the lazy store reads
        // (the document is saved back in its own encoding). `None` keeps the decode-into-memory path.
        return crate::stream_codec::open_transcoded_buffer(path);
    }
    let buffer = match Buffer::open_lazy(path) {
        Ok(b) => b,
        Err(e) if e.kind() == io::ErrorKind::Unsupported => return Ok(None),
        Err(e) => return Err(e),
    };
    // Line-ending style from the first MB (the whole file is never scanned at open).
    let mut head = Vec::new();
    std::fs::File::open(path)?.take(1 << 20).read_to_end(&mut head)?;
    let doc = crate::file_io::LoadedDoc { text: String::new(), codepage: "utf-8", bom: buffer.lazy_has_bom(), eol: brouillon_core::eol::scan(&String::from_utf8_lossy(&head)), lossy: false };
    Ok(Some((buffer, DocMeta::from_loaded(&doc))))
}

/// The kind a file opens as: detected from its head (one bounded read), or `Text` when the user chose
/// "always open as text" for that kind. Shared by tab open and the command-line first file.
pub fn kind_for_open(path: &Path) -> brouillon_core::kind::FileKind {
    let kind = brouillon_core::kind::detect_path(path).kind;
    if kind.module().is_some() && brouillon_core::kind::is_always_text(kind) {
        brouillon_core::kind::FileKind::Text
    } else {
        kind
    }
}

/// Files at least this big are read straight into the rope (see `file_io::read_utf8_rope`).
const STREAM_MIN_BYTES: u64 = 16 * 1024 * 1024;

/// Read a file into a buffer plus its metadata (encoding, BOM, EOL). A huge BOM-less UTF-8 file is read into
/// the rope in chunks: no second, full-size copy as a `String`. Shared by tab open and the external-change reload.
pub fn load_buffer(path: &Path) -> io::Result<(Buffer, DocMeta)> {
    // Large file mode first: the lazy store when the size and the encoding allow it.
    if let Some(pair) = try_open_lazy(path)? {
        return Ok(pair);
    }
    let streamed = if std::fs::metadata(path).map(|m| m.len()).unwrap_or(0) >= STREAM_MIN_BYTES { crate::file_io::read_utf8_rope(path)? } else { None };
    Ok(match streamed {
        Some(s) => {
            let doc = crate::file_io::LoadedDoc { text: String::new(), codepage: "utf-8", bom: false, eol: s.eol, lossy: false };
            (Buffer::from_rope(s.rope), DocMeta::from_loaded(&doc))
        }
        None => {
            let loaded = crate::file_io::read_file(path, "windows-1252")?;
            let meta = DocMeta::from_loaded(&loaded);
            (Buffer::from_str(&loaded.text), meta)
        }
    })
}

impl TabState {
    /// Load a file from disk into a new tab. Title is the file name
    /// (no parent path) — view layer can show the full path in a tooltip.
    pub fn from_path<P: AsRef<Path>>(path: P) -> io::Result<Self> {
        let path = path.as_ref().to_path_buf();
        crate::external::note(&path); // stamp taken before the read: a change during the read is seen later
        // One bounded head read (16 KiB). Only a routed document kind leaves the text editor; "always as text" wins.
        let kind = kind_for_open(&path);
        let routed = kind.routed_to_viewer();
        let (buffer, meta) = if routed {
            // Never decode a PDF/Word file as text: nothing is read, memory does not depend on its size.
            (Buffer::from_str(""), DocMeta { readonly: true, ..DocMeta::default() })
        } else {
            load_buffer(&path)?
        };
        let encoding = meta.legacy_encoding();
        let eol = meta.eol_mode();
        let title = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| path.to_string_lossy().into_owned());
        let sketch = crate::sketch::SketchDocument::load_sidecar(&path).unwrap_or_default();
        let sketch = if sketch.pages.is_empty() { crate::sketch::SketchDocument::new() } else { sketch };
        let sketch_saved = sketch.stroke_count();
        Ok(Self {
            path: Some(path),
            remote_path: None,
            remote_origin: None,
            buffer: Arc::new(buffer),
            cursor: (1, 1),
            dirty: false,
            title,
            sketch,
            sketch_saved,
            encoding,
            eol,
            meta,
            help_doc: None,
            kind,
        })
    }

    /// Load content from a remote SFTP path.
    pub fn from_remote(remote_path: String, content: String) -> Self {
        let buffer = Buffer::from_str(&content);
        let title = remote_path
            .split('/')
            .last()
            .unwrap_or(&remote_path)
            .to_string();
        let title = format!("[SIM] {}", title); // local simulation, not a real server
        Self {
            path: None,
            remote_path: Some(remote_path),
            remote_origin: None,
            buffer: Arc::new(buffer),
            cursor: (1, 1),
            dirty: false,
            title,
            sketch: crate::sketch::SketchDocument::new(),
            sketch_saved: 0,
            encoding: brouillon_core::encoding::DocumentEncoding::Utf8,
            eol: brouillon_core::encoding::EolMode::Lf,
            meta: DocMeta::default(),
            help_doc: None,
            kind: brouillon_core::kind::FileKind::Text,
        }
    }

    /// A document read from a server by the remote explorer (`content` decoded by the caller).
    pub fn from_remote_origin(origin: crate::remote::RemoteOrigin, content: &str) -> Self {
        let mut t = Self::from_remote(origin.path.clone(), content.to_string());
        t.title = origin.path.rsplit('/').next().filter(|n| !n.is_empty()).unwrap_or(&origin.path).to_string();
        t.remote_origin = Some(origin);
        t
    }

    /// Empty scratch buffer named "untitled-N".
    pub fn scratch(n: usize) -> Self {
        Self {
            path: None,
            remote_path: None,
            remote_origin: None,
            buffer: Arc::new(Buffer::from_str("")),
            cursor: (1, 1),
            dirty: false,
            title: format!("untitled-{}", n),
            sketch: crate::sketch::SketchDocument::new(),
            sketch_saved: 0,
            encoding: brouillon_core::encoding::DocumentEncoding::Utf8,
            eol: brouillon_core::encoding::EolMode::Lf,
            meta: DocMeta::default(),
            help_doc: None,
            kind: brouillon_core::kind::FileKind::Text,
        }
    }

    /// Drawn strokes that were never written to disk.
    pub fn sketch_unsaved(&self) -> bool {
        self.sketch.stroke_count() != self.sketch_saved
    }

    /// Modified = unsaved text OR unsaved strokes.
    pub fn is_modified(&self) -> bool {
        self.buffer.is_dirty() || self.sketch_unsaved()
    }

    /// True when this tab is an unmodified, never-saved scratch buffer.
    /// Used by `open_file` to decide whether to replace-in-place vs. append.
    fn is_pristine_scratch(&self) -> bool {
        self.path.is_none()
            && self.remote_path.is_none()
            && !self.dirty
            && self.buffer.byte_len() == 0
            && self.sketch.stroke_count() == 0
    }
}

/// Multi-document model.
pub struct TabManager {
    tabs: Vec<TabState>,
    active: usize,
    /// Monotonic counter for next untitled-N. Never resets — re-using
    /// numbers after close would confuse users with a stale tab title.
    untitled_counter: usize,
    /// Flag indicating if the workspace is in an empty visual state (all tabs closed).
    pub is_empty: bool,
}

impl Default for TabManager {
    fn default() -> Self {
        Self::new()
    }
}

impl TabManager {
    /// Fresh manager with one scratch tab ("untitled-1"), active = 0.
    pub fn new() -> Self {
        Self {
            tabs: vec![TabState::scratch(1)],
            active: 0,
            untitled_counter: 1,
            is_empty: false,
        }
    }

    /// Open a file. Replaces the active tab in-place if it's an unmodified
    /// scratch (common editor behaviour: opening a file from
    /// a blank window doesn't leave the blank window behind). Otherwise
    /// appends a new tab. Returns the index of the new active tab.
    pub fn open_file<P: AsRef<Path>>(&mut self, path: P) -> io::Result<usize> {
        let tab = TabState::from_path(path)?;
        if self.is_empty {
            self.tabs[0] = tab;
            self.active = 0;
            self.is_empty = false;
        } else if self.tabs[self.active].is_pristine_scratch() {
            self.tabs[self.active] = tab;
        } else {
            self.tabs.push(tab);
            self.active = self.tabs.len() - 1;
        }
        Ok(self.active)
    }

    /// Open (or activate) a document read from a server. The same server file is never opened twice.
    pub fn open_remote_doc(&mut self, origin: crate::remote::RemoteOrigin, content: &str) -> usize {
        if !self.is_empty {
            if let Some(i) = self.tabs.iter().position(|t| t.remote_origin.as_ref() == Some(&origin)) {
                self.active = i;
                return i;
            }
        }
        let tab = TabState::from_remote_origin(origin, content);
        if self.is_empty {
            self.tabs[0] = tab;
            self.active = 0;
            self.is_empty = false;
        } else if self.tabs[self.active].is_pristine_scratch() {
            self.tabs[self.active] = tab;
        } else {
            self.tabs.push(tab);
            self.active = self.tabs.len() - 1;
        }
        self.active
    }

    /// Put `content` in the tab of a server file (opening or activating it). `modified` = the tab counts
    /// as changed (recovered unsaved text); `false` = it equals the server copy (after a reload).
    pub fn replace_remote_doc(&mut self, origin: crate::remote::RemoteOrigin, content: &str, modified: bool) -> usize {
        let i = self.open_remote_doc(origin, content);
        let tab = &mut self.tabs[i];
        tab.buffer = Arc::new(Buffer::from_str(content));
        tab.dirty = modified;
        i
    }

    /// Open a remote file from the local SFTP simulation.
    pub fn open_remote(&mut self, remote_path: String, content: String) -> usize {
        let tab = TabState::from_remote(remote_path, content);
        if self.is_empty {
            self.tabs[0] = tab;
            self.active = 0;
            self.is_empty = false;
        } else if self.tabs[self.active].is_pristine_scratch() {
            self.tabs[self.active] = tab;
        } else {
            self.tabs.push(tab);
            self.active = self.tabs.len() - 1;
        }
        self.active
    }

    /// Open (or activate) the built-in help document `id`. The tab is read-only,
    /// has no path and is not persisted (see [`TabState::help_doc`]). An already
    /// open tab for the same id is reused; an unmodified blank tab is replaced.
    pub fn open_help(&mut self, id: &'static str, title: &str, text: &str) -> usize {
        if !self.is_empty {
            if let Some(i) = self.tabs.iter().position(|t| t.help_doc == Some(id)) {
                self.active = i;
                return i;
            }
        }
        let mut tab = TabState::scratch(0);
        tab.buffer = Arc::new(Buffer::from_str(text));
        tab.title = title.to_string();
        tab.meta.readonly = true;
        tab.help_doc = Some(id);
        if self.is_empty {
            self.tabs[0] = tab;
            self.active = 0;
            self.is_empty = false;
        } else if self.tabs[self.active].is_pristine_scratch() {
            self.tabs[self.active] = tab;
        } else {
            self.tabs.push(tab);
            self.active = self.tabs.len() - 1;
        }
        self.active
    }

    /// Add an empty untitled tab and switch to it.
    pub fn new_scratch(&mut self) -> usize {
        self.untitled_counter += 1;
        let new_tab = TabState::scratch(self.untitled_counter);
        if self.is_empty {
            self.tabs[0] = new_tab;
            self.active = 0;
            self.is_empty = false;
        } else {
            self.tabs.push(new_tab);
            self.active = self.tabs.len() - 1;
        }
        self.active
    }

    /// Close the tab at `idx`.
    ///
    /// Rules:
    /// - If it's the last tab AND dirty → `Err(DirtyAndOnlyTab)`. The view
    ///   layer is expected to prompt the user before retrying.
    /// - If it's the last tab and clean → switch to visual empty state.
    /// - Otherwise remove ; active shifts left if we removed the active
    ///   tab or anything before it.
    ///
    /// Out-of-range `idx` is a no-op — defensive against view-layer bugs.
    pub fn close(&mut self, idx: usize) -> Result<(), CloseError> {
        if self.is_empty || idx >= self.tabs.len() {
            return Ok(());
        }
        if self.tabs.len() == 1 {
            if self.tabs[0].dirty {
                return Err(CloseError::DirtyAndOnlyTab);
            }
            // Transition to visual empty state keeping a single pristine tab in memory
            self.untitled_counter += 1;
            self.tabs[0] = TabState::scratch(self.untitled_counter);
            self.active = 0;
            self.is_empty = true;
            return Ok(());
        }
        self.tabs.remove(idx);
        // Re-anchor `active`: shift left if needed, clamp to valid range.
        if self.active > idx {
            self.active -= 1;
        } else if self.active == idx {
            // Closed the active one — stay at the same slot if possible,
            // else move one step left.
            if self.active >= self.tabs.len() {
                self.active = self.tabs.len() - 1;
            }
        }
        Ok(())
    }

    /// Switch active tab. Out-of-range clamps to the nearest valid index
    /// rather than panicking — view layer may race with close events.
    pub fn switch_to(&mut self, idx: usize) {
        if self.is_empty || self.tabs.is_empty() {
            return;
        }
        self.active = idx.min(self.tabs.len() - 1);
    }

    pub fn current(&self) -> &TabState {
        &self.tabs[self.active]
    }
    pub fn current_mut(&mut self) -> &mut TabState {
        &mut self.tabs[self.active]
    }
    /// Any tab, mutably (Brouillon Link edits a background tab without activating it).
    pub fn tab_mut(&mut self, idx: usize) -> Option<&mut TabState> {
        self.tabs.get_mut(idx)
    }

    pub fn count(&self) -> usize {
        if self.is_empty {
            0
        } else {
            self.tabs.len()
        }
    }
    pub fn active_idx(&self) -> usize {
        self.active
    }
    pub fn iter(&self) -> impl Iterator<Item = &TabState> {
        let limit = if self.is_empty { 0 } else { self.tabs.len() };
        self.tabs.iter().take(limit)
    }

    /// Set the cursor position for the tab at `idx`. Out-of-range is a
    /// no-op (matches the defensive style of `switch_to` / `close` — the
    /// view layer may race with close events). Used by session restore
    /// to re-anchor the caret after loading each tab from disk.
    pub fn set_cursor(&mut self, idx: usize, cur: (usize, usize)) {
        if idx < self.tabs.len() {
            self.tabs[idx].cursor = cur;
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum CloseError {
    /// The tab is the only one open and has unsaved changes. The view
    /// layer should prompt the user (save / discard / cancel) and retry.
    DirtyAndOnlyTab,
}

impl std::fmt::Display for CloseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CloseError::DirtyAndOnlyTab => {
                write!(f, "cannot close the last tab with unsaved changes")
            }
        }
    }
}

impl std::error::Error for CloseError {}

/// Show a native file-open dialog and return the chosen path. Blocking on
/// the calling thread — UI callers should `std::thread::spawn` this so
/// the Floem event loop keeps pumping.
///
/// Implemented via the `rfd` crate (Rust File Dialog): Win32 IFileDialog
/// on Windows, AppKit on macOS, XDG Desktop Portal (with Zenity fallback)
/// on Linux.
pub fn show_open_dialog() -> Option<PathBuf> {
    rfd::FileDialog::new()
        .add_filter("Text files", &["txt", "md", "rs", "toml", "yaml", "json"])
        .add_filter("All files", &["*"])
        .pick_file()
}

/// Show a native save-as dialog. `suggested_name` pre-fills the filename
/// field (e.g. the current tab title). Returns `None` on user cancel.
pub fn show_save_dialog(suggested_name: Option<&str>) -> Option<PathBuf> {
    let mut dialog = rfd::FileDialog::new()
        .add_filter("Text files", &["txt", "md", "rs", "toml", "yaml", "json"])
        .add_filter("All files", &["*"]);
    if let Some(name) = suggested_name {
        dialog = dialog.set_file_name(name);
    }
    dialog.save_file()
}

#[cfg(test)]
mod tests {

    #[test]
    fn help_tab_replaces_blank_scratch_is_readonly_and_reused() {
        let mut m = TabManager::new();
        let i = m.open_help("welcome", "Welcome", "# Hi\nbody");
        assert_eq!((i, m.count()), (0, 1), "the blank scratch tab is replaced in place");
        let t = m.current();
        assert_eq!((t.title.as_str(), t.path.is_none(), t.meta.readonly, t.help_doc), ("Welcome", true, true, Some("welcome")));
        assert!(!t.dirty && !t.is_modified());
        // A second request for the same document activates the existing tab.
        m.new_scratch();
        assert_eq!(m.count(), 2);
        let j = m.open_help("welcome", "Welcome", "# Hi\nbody");
        assert_eq!((j, m.count(), m.active_idx()), (0, 2, 0));
        // A different document is appended and becomes active.
        let k = m.open_help("docs", "Documentation", "docs");
        assert_eq!((k, m.count(), m.active_idx()), (2, 3, 2));
    }

    use super::*;
    use std::io::Write;

    /// Helper: write content to a unique temp file and return its path.
    /// We avoid the `tempfile` dep (not in workspace) — std::env::temp_dir
    /// plus a per-test name is fine for our single-process test runner.
    fn write_temp(name: &str, content: &str) -> PathBuf {
        let mut path = std::env::temp_dir();
        path.push(format!("brouillon-tabs-test-{}-{}", std::process::id(), name));
        let mut f = std::fs::File::create(&path).expect("create temp file");
        f.write_all(content.as_bytes()).expect("write temp file");
        path
    }

    #[test]
    fn new_yields_one_scratch_tab() {
        let m = TabManager::new();
        assert_eq!(m.count(), 1);
        assert_eq!(m.active_idx(), 0);
        assert_eq!(m.current().title, "untitled-1");
        assert!(m.current().path.is_none());
        assert!(!m.current().dirty);
        assert_eq!(m.current().cursor, (1, 1));
    }

    #[test]
    fn new_scratch_bumps_counter_and_switches() {
        let mut m = TabManager::new();
        let idx = m.new_scratch();
        assert_eq!(idx, 1);
        assert_eq!(m.count(), 2);
        assert_eq!(m.active_idx(), 1);
        assert_eq!(m.current().title, "untitled-2");

        // Another one — counter keeps going.
        m.new_scratch();
        assert_eq!(m.current().title, "untitled-3");
    }

    #[test]
    fn close_last_unmodified_tab_transitions_to_empty() {
        let mut m = TabManager::new();
        assert!(m.close(0).is_ok());
        // Bounded to 0 count because we closed the last tab!
        assert_eq!(m.count(), 0);
        assert!(m.is_empty);
    }

    #[test]
    fn close_last_dirty_tab_returns_err() {
        let mut m = TabManager::new();
        m.current_mut().dirty = true;
        let err = m.close(0).expect_err("dirty + only tab should fail");
        assert_eq!(err, CloseError::DirtyAndOnlyTab);
        // Tab is unchanged.
        assert_eq!(m.count(), 1);
        assert!(m.current().dirty);
    }

    #[test]
    fn close_middle_tab_shifts_active_left_if_needed() {
        let mut m = TabManager::new();
        m.new_scratch(); // idx 1
        m.new_scratch(); // idx 2, active
        assert_eq!(m.active_idx(), 2);

        // Close idx 0 — active should slide from 2 to 1.
        m.close(0).unwrap();
        assert_eq!(m.count(), 2);
        assert_eq!(m.active_idx(), 1);
    }

    #[test]
    fn close_active_tab_at_end_steps_back() {
        let mut m = TabManager::new();
        m.new_scratch(); // idx 1, active
        m.close(1).unwrap();
        assert_eq!(m.count(), 1);
        assert_eq!(m.active_idx(), 0);
    }

    #[test]
    fn open_file_replaces_pristine_scratch() {
        let path = write_temp("replace.txt", "hello world\n");
        let mut m = TabManager::new();
        assert_eq!(m.count(), 1);

        let idx = m.open_file(&path).expect("open should succeed");
        assert_eq!(idx, 0);
        assert_eq!(m.count(), 1, "pristine scratch was replaced, not appended");
        assert_eq!(m.current().title, path.file_name().unwrap().to_string_lossy());
        assert_eq!(m.current().path.as_ref(), Some(&path));
        assert!(!m.current().dirty);

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn a_pdf_opens_on_an_empty_read_only_placeholder_and_text_is_unchanged() {
        use brouillon_core::kind::FileKind;
        let pdf = write_temp("route.pdf", "%PDF-1.7\n%binary stuff\n");
        let txt = write_temp("route-plain.txt", "just text\n");
        let rtf = write_temp("route.rtf", "{\\rtf1 hello}");
        let mut m = TabManager::new();
        m.open_file(&pdf).unwrap();
        let t = m.current();
        assert_eq!(t.kind, FileKind::Pdf);
        assert!(t.meta.readonly && !t.dirty && t.buffer.byte_len() == 0, "nothing is decoded as text");
        m.open_file(&txt).unwrap();
        assert_eq!(m.current().kind, FileKind::Text);
        assert_eq!(m.current().buffer.byte_len(), 10);
        // RTF keeps its text and is only labelled (the offer bar uses the kind)
        m.open_file(&rtf).unwrap();
        assert_eq!(m.current().kind, FileKind::Rtf);
        assert!(m.current().buffer.byte_len() > 0 && !m.current().meta.readonly);
        for p in [pdf, txt, rtf] {
            let _ = std::fs::remove_file(p);
        }
    }

    #[test]
    fn open_file_appends_when_scratch_is_dirty() {
        let path = write_temp("append-dirty.txt", "from disk\n");
        let mut m = TabManager::new();
        m.current_mut().dirty = true;

        let idx = m.open_file(&path).expect("open should succeed");
        assert_eq!(m.count(), 2);
        assert_eq!(idx, 1);
        assert_eq!(m.active_idx(), 1);

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn open_file_appends_when_other_file_already_loaded() {
        let a = write_temp("a.txt", "alpha\n");
        let b = write_temp("b.txt", "beta\n");
        let mut m = TabManager::new();

        m.open_file(&a).unwrap(); // replaces the pristine scratch
        assert_eq!(m.count(), 1);

        m.open_file(&b).unwrap(); // appends — current tab is a real file
        assert_eq!(m.count(), 2);
        assert_eq!(m.active_idx(), 1);

        let _ = std::fs::remove_file(&a);
        let _ = std::fs::remove_file(&b);
    }

    #[test]
    fn open_file_propagates_io_error() {
        let mut m = TabManager::new();
        let err = m.open_file("Z:\\definitely-does-not-exist\\nope.txt");
        assert!(err.is_err());
        // Manager unchanged.
        assert_eq!(m.count(), 1);
    }

    #[test]
    fn switch_to_clamps_out_of_range() {
        let mut m = TabManager::new();
        m.new_scratch(); // count = 2

        m.switch_to(999);
        assert_eq!(m.active_idx(), 1, "clamped to last valid index");

        m.switch_to(0);
        assert_eq!(m.active_idx(), 0);
    }

    #[test]
    fn iter_yields_all_tabs_in_order() {
        let mut m = TabManager::new();
        m.new_scratch();
        m.new_scratch();
        let titles: Vec<_> = m.iter().map(|t| t.title.clone()).collect();
        assert_eq!(titles, vec!["untitled-1", "untitled-2", "untitled-3"]);
    }

    #[test]
    fn test_tab_manager_switch_to_same() {
        let mut m = TabManager::new();
        m.switch_to(0);
        assert_eq!(m.active_idx(), 0);
    }

    #[test]
    fn test_tab_manager_close_only_tab() {
        let mut m = TabManager::new();
        assert_eq!(m.count(), 1);
        let _ = m.close(0);
        assert_eq!(m.count(), 0);
    }

    #[test]
    fn test_tab_manager_tab_is_dirty() {
        let mut m = TabManager::new();
        assert!(!m.current().dirty);
        let t = m.current_mut();
        let mut b = (*t.buffer).clone();
        b.insert_at(0, "x");
        t.buffer = std::sync::Arc::new(b);
        t.dirty = true;
        assert!(m.current().dirty);
    }

    #[test]
    fn test_tab_manager_tab_state_debug() {
        let m = TabManager::new();
        let dbg = format!("{:?}", m.current());
        assert!(dbg.contains("TabState"));
    }

    #[test]
    fn a_server_document_keeps_its_origin_and_is_never_opened_twice() {
        use crate::remote::RemoteOrigin;
        let o = RemoteOrigin { site: "me-box".into(), site_label: "box".into(), path: "/var/www/index.php".into() };
        let mut m = TabManager::new();
        let a = m.open_remote_doc(o.clone(), "<?php
");
        assert_eq!(m.current().title, "index.php");
        assert_eq!(m.current().remote_origin.as_ref().unwrap().chip(), "~box");
        assert!(m.current().path.is_none(), "a server file has no local path");
        m.new_scratch();
        assert_eq!(m.open_remote_doc(o, "other"), a, "same server file: the tab is reused");
        assert_eq!(m.count(), 2);
    }
}
