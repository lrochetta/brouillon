//! Brouillon Link inside the real editor: [`UiState`] answers the MCP core on the UI thread (it is
//! the [`UiOps`] behind the [`brouillon_mcp::bridge::Bridge`]) and [`LinkRuntime`] starts and stops
//! the server. Nothing here runs unless the user enabled Agents (`[mcp] enabled`, or `--mcp`).

use std::cell::{Cell, RefCell};
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

use brouillon_mcp::api::{ApiError, ApiResult, CommandInfo, ConsentRequest, Diagnostic, DocInfo, DocSnapshot, ProposalView, TextEdit};
use brouillon_mcp::bridge::{Bridge, UiOps};
use brouillon_mcp::core::{McpConfig, McpCore};
use brouillon_mcp::link::{self, LinkDirs, LinkHost};
use brouillon_mcp::policy::{Grant, PolicyConfig, Scope};
use brouillon_mcp::settings::McpSettings;
use brouillon_ui::link_view::{live_notes_for, Consent, ConsentReq, HunkUi, LinkUi, NoteUi, ProposalUi};
use floem::reactive::{RwSignal, SignalGet, SignalUpdate, SignalWith};

use crate::buffer::{Buffer, EditMeta};
use crate::contract::{EditSet, TextEdit as CoreEdit};
use crate::cursor::Selection;
use crate::tabs::{TabManager, TabState};
use crate::undo::Origin;

/// Opaque, stable id of a tab: derived from its file path (or, for an unsaved buffer, its
/// never-reused "untitled-N" title). Stays valid when tabs are reordered or closed.
pub fn doc_id(path: Option<&std::path::Path>, title: &str) -> String {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    match path {
        Some(p) => ("path", p.to_string_lossy().to_lowercase()).hash(&mut h),
        None => ("scratch", title).hash(&mut h),
    }
    format!("d{:012x}", h.finish() & 0xffff_ffff_ffff)
}

/// The folders of the open files: what "workspace" means in a editor with no project concept, and
/// what a consent grant covers.
pub fn folders_of(tm: &TabManager) -> Vec<String> {
    let mut out: Vec<String> = vec![];
    for t in tm.iter() {
        if let Some(parent) = t.path.as_ref().and_then(|p| p.parent()) {
            let s = parent.display().to_string();
            if !s.is_empty() && !out.contains(&s) {
                out.push(s);
            }
        }
    }
    out
}

/// Longest text the server will hand out for one document; larger ones are "private" (hidden).
const MAX_DOC_BYTES: usize = 8 * 1024 * 1024;

/// Notes one client may keep on screen at once.
const MAX_NOTES_PER_CLIENT: usize = 20;

/// The only editor commands an agent may run through `run_command`: view toggles and read-only
/// analysis. No command that changes text or files, no tool runner, no shell. The answer to a
/// command is the status line it leaves behind.
pub const SAFE_COMMANDS: [(&str, &str); 5] = [
    ("view_toggle_line_numbers", "View: Toggle Line Numbers"),
    ("view_toggle_preview", "View: Toggle Markdown Preview"),
    ("view_toggle_hex", "View: Toggle Hex View"),
    ("count_text", "Transform: Count Text (characters, words, lines)"),
    ("sum_numbers", "Transform: Sum Numbers in Selection"),
];

pub fn is_safe_command(id: &str) -> bool {
    SAFE_COMMANDS.iter().any(|(i, _)| *i == id)
}

/// Why an agent-initiated save did not happen.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SaveFail {
    /// The disk changed since the document was opened or last saved.
    Conflict(String),
    /// The document's encoding cannot hold some characters: only the user may decide to lose them.
    Lossy,
    Io(String),
}

/// Save one tab (by index). Implemented in `main.rs`, which owns the tab state; `force` skips only
/// the external-change gate.
pub type SaveHook = Rc<dyn Fn(usize, bool) -> Result<(), SaveFail>>;
/// Run an editor command by id on the UI thread (the palette dispatcher).
pub type RunHook = Rc<dyn Fn(&str)>;

/// Late-bound callbacks: the dispatcher and the save routine are created after [`UiState`].
#[derive(Default)]
pub struct LinkHooks {
    pub save: RefCell<Option<SaveHook>>,
    pub run: RefCell<Option<RunHook>>,
}

/// Write `text` to `path` for an agent: no dialog ever opens. Refuses on an external change unless
/// `force`, and refuses a lossy encoding (that decision belongs to the user).
pub fn save_for_agent(path: &std::path::Path, text: &crop::Rope, meta: &crate::tabs::DocMeta, force: bool, opts: &crate::backup::BackupOptions) -> Result<(), SaveFail> {
    use crate::wiring::{save_document_rope as save_document, SaveResult};
    if !force {
        let spec = crate::file_io::SaveSpec { codepage: meta.codepage, bom: meta.bom, eol: None };
        if crate::external::save_gate_pieces(path, || text.chunks(), &spec) == crate::external::SaveGate::Conflict {
            return Err(SaveFail::Conflict("the file changed on disk since it was opened or last saved".into()));
        }
    }
    match save_document(path, text, meta, false, opts) {
        SaveResult::Saved { .. } => {
            crate::external::note(path);
            Ok(())
        }
        SaveResult::Failed(e) => Err(SaveFail::Io(e)),
        SaveResult::Lossy { .. } => Err(SaveFail::Lossy),
    }
}

/// The persistent half of the kill switch: write `[mcp] enabled = false` into the user settings
/// file (creating it when missing), keeping everything else in it.
pub fn persist_disabled(settings_file: &std::path::Path) -> std::io::Result<()> {
    let text = std::fs::read_to_string(settings_file).unwrap_or_default();
    let new = brouillon_core::settings::set_raw_key(&text, "mcp", "enabled", "false");
    if let Some(dir) = settings_file.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(settings_file, new)
}

/// Recompute what the rows show: drop expired notes, keep those of the active document.
pub fn refresh_notes(ui: LinkUi, tabs: RwSignal<TabManager>) {
    let now = std::time::Instant::now();
    ui.notes.update(|v| v.retain(|n| n.expires > now));
    let active = tabs.with_untracked(|tm| if tm.is_empty { None } else { Some(doc_id(tm.current().path.as_deref(), &tm.current().title)) });
    let list = ui.notes.with_untracked(|v| live_notes_for(v, active.as_deref(), now));
    if ui.line_notes.with_untracked(|cur| *cur != list) {
        ui.line_notes.set(list);
    }
}

pub struct UiState {
    pub hooks: Rc<LinkHooks>,
    pub next_note: Cell<u64>,
    pub tabs: RwSignal<TabManager>,
    pub buffer: RwSignal<Arc<Buffer>>,
    pub cursor: RwSignal<(usize, usize)>,
    pub selection: RwSignal<Selection>,
    pub status: RwSignal<String>,
    pub ui: LinkUi,
    /// Open a path as a tab (and make it active), like the open dialog.
    pub open: Rc<dyn Fn(PathBuf, Option<usize>, Option<usize>)>,
    /// Make tab `idx` the active one.
    pub activate: Rc<dyn Fn(usize)>,
}

impl UiState {
    fn live_buffer(&self, tm: &TabManager, idx: usize) -> Arc<Buffer> {
        if idx == tm.active_idx() {
            self.buffer.get_untracked()
        } else {
            tm.iter().nth(idx).map(|t| t.buffer.clone()).unwrap_or_else(|| self.buffer.get_untracked())
        }
    }

    fn info(&self, tm: &TabManager, idx: usize, t: &TabState) -> DocInfo {
        let buf = self.live_buffer(tm, idx);
        let lang = t.path.as_ref().and_then(|p| p.extension()).map(|e| e.to_string_lossy().to_lowercase()).unwrap_or_else(|| "text".into());
        DocInfo {
            id: doc_id(t.path.as_deref(), &t.title),
            path: t.path.as_ref().map(|p| p.display().to_string()),
            dirty: buf.is_dirty(),
            lang,
            version: buf.version(),
            hash: format!("{:016x}", buf.hash()),
            encoding: t.meta.codepage.to_string(),
            eol: t.meta.eol_kind.name().to_string(),
            // Built-in help pages and very large files are never exposed.
            private: t.help_doc.is_some() || t.meta.large_file || buf.byte_len() > MAX_DOC_BYTES,
            window: 1,
        }
    }

    fn find(&self, tm: &TabManager, doc: &str) -> Option<usize> {
        if tm.is_empty {
            return None;
        }
        tm.iter().position(|t| doc_id(t.path.as_deref(), &t.title) == doc)
    }
}

impl UiOps for UiState {
    fn docs(&self) -> Vec<DocInfo> {
        self.tabs.with_untracked(|tm| if tm.is_empty { vec![] } else { tm.iter().enumerate().map(|(i, t)| self.info(tm, i, t)).collect() })
    }

    fn active_doc(&self) -> Option<String> {
        self.tabs.with_untracked(|tm| if tm.is_empty { None } else { Some(tm.current()).map(|t| doc_id(t.path.as_deref(), &t.title)) })
    }

    fn workspace_folders(&self) -> Vec<String> {
        self.tabs.with_untracked(folders_of)
    }

    fn snapshot(&self, doc: &str) -> Option<DocSnapshot> {
        self.tabs.with_untracked(|tm| {
            let idx = self.find(tm, doc)?;
            let t = tm.iter().nth(idx)?;
            let info = self.info(tm, idx, t);
            if info.private {
                return Some(DocSnapshot { info, text: String::new(), selection: None });
            }
            let buf = self.live_buffer(tm, idx);
            if buf.is_lazy() {
                // Large file mode: agents never get a copy of a multi-megabyte document.
                return Some(DocSnapshot { info, text: String::new(), selection: None });
            }
            let text = buf.text();
            let selection = if idx == tm.active_idx() {
                let sel = self.selection.get_untracked();
                if sel.is_empty() {
                    None
                } else {
                    let r = crate::wiring::regions_from(&text, self.cursor.get_untracked(), Some((sel.anchor, sel.head)))[0];
                    Some((r.start(), r.end()))
                }
            } else {
                None
            };
            Some(DocSnapshot { info, text, selection })
        })
    }

    fn apply_edits(&self, doc: &str, expected_version: u64, edits: &[TextEdit], label: &str) -> ApiResult<u64> {
        let (idx, active, buf) = self.tabs.with_untracked(|tm| {
            let idx = self.find(tm, doc).ok_or(ApiError::UnknownDoc)?;
            Ok::<_, ApiError>((idx, idx == tm.active_idx(), self.live_buffer(tm, idx)))
        })?;
        if buf.is_lazy() {
            return Err(ApiError::Invalid("this document is open in large file mode: agents cannot edit it".into()));
        }
        if buf.version() != expected_version {
            return Err(ApiError::Stale { current: buf.version() });
        }
        let set = EditSet::from_unsorted(edits.iter().map(|e| CoreEdit::new(e.start, e.end, e.new_text.clone())).collect()).map_err(|e| ApiError::Invalid(e.to_string()))?;
        let mut nb = (*buf).clone();
        nb.break_typing();
        // Origin::Ai makes this ONE undo step that the history knows came from an agent.
        nb.apply_edits_with(&set, EditMeta { origin: Origin::Ai(label.to_string()), ..Default::default() }).map_err(|e| ApiError::Invalid(e.to_string()))?;
        let version = nb.version();
        let old_text = buf.text();
        self.tabs.update(|tm| {
            if let Some(t) = tm.tab_mut(idx) {
                if !t.meta.markers.is_empty() {
                    let mut m = crate::markers::Markers::from_lines(t.meta.markers.iter().copied());
                    m.apply_edits(&old_text, &set);
                    t.meta.markers = m.to_vec();
                }
                if !active {
                    t.buffer = Arc::new(nb.clone());
                    t.dirty = true;
                }
            }
        });
        if active {
            let last = nb.line_count().max(1);
            self.buffer.set(Arc::new(nb));
            self.cursor.update(|c| c.0 = c.0.min(last));
            self.selection.set(Selection::new((1, 1), (1, 1), false));
        }
        self.status.set(label.to_string());
        Ok(version)
    }

    fn open(&self, path: &str, line: Option<usize>, col: Option<usize>, focus: bool) -> ApiResult<String> {
        let p = PathBuf::from(path);
        if !p.is_file() {
            return Err(ApiError::Io("not a file".into()));
        }
        let before = self.tabs.with_untracked(|tm| tm.active_idx());
        (self.open)(p.clone(), line, col);
        if !focus {
            (self.activate)(before);
        }
        let want = std::fs::canonicalize(&p).unwrap_or(p);
        self.tabs
            .with_untracked(|tm| tm.iter().find(|t| t.path.as_ref().map(|q| std::fs::canonicalize(q).unwrap_or_else(|_| q.clone())) == Some(want.clone())).map(|t| doc_id(t.path.as_deref(), &t.title)))
            .ok_or_else(|| ApiError::Io("could not open the file".into()))
    }

    /// "Lite" diagnostics (brackets, JSON syntax, encoding damage, mixed line endings): there is no
    /// language server yet, so an empty list means only that none of these checks found anything.
    fn diagnostics(&self, doc: &str) -> ApiResult<Vec<Diagnostic>> {
        let snap = self.snapshot(doc).ok_or(ApiError::UnknownDoc)?;
        if snap.info.private {
            return Err(ApiError::UnknownDoc);
        }
        Ok(brouillon_mcp::diag::lite(&snap.text, &snap.info.lang))
    }

    fn commands(&self) -> Vec<CommandInfo> {
        SAFE_COMMANDS.iter().map(|(id, caption)| CommandInfo { id: id.to_string(), caption: caption.to_string(), mutates: false, exec: false, ai_exposed: true }).collect()
    }

    fn run_command(&self, id: &str, _args: &serde_json::Value) -> ApiResult<serde_json::Value> {
        if !is_safe_command(id) {
            return Err(ApiError::Invalid("that command is not available to agents".into()));
        }
        let run = self.hooks.run.borrow().clone().ok_or(ApiError::Unsupported)?;
        self.status.set(String::new());
        run(id);
        Ok(serde_json::json!({"ran": id, "status": self.status.get_untracked()}))
    }

    fn save(&self, doc: &str, force: bool) -> ApiResult<()> {
        let (idx, private) = self.tabs.with_untracked(|tm| self.find(tm, doc).and_then(|i| tm.iter().nth(i).map(|t| (i, self.info(tm, i, t).private)))).ok_or(ApiError::UnknownDoc)?;
        if private {
            return Err(ApiError::UnknownDoc);
        }
        let hook = self.hooks.save.borrow().clone().ok_or(ApiError::Unsupported)?;
        hook(idx, force).map_err(|e| match e {
            SaveFail::Conflict(m) => ApiError::Conflict(m),
            SaveFail::Lossy => ApiError::Invalid("some characters cannot be written in this document's encoding; only the user can save it".into()),
            SaveFail::Io(m) => ApiError::Io(m),
        })
    }

    fn annotate(&self, doc: &str, line: usize, kind: &str, text: &str, ttl_s: u64, owner: &str) -> ApiResult<()> {
        let (total, private) = self
            .tabs
            .with_untracked(|tm| self.find(tm, doc).and_then(|idx| tm.iter().nth(idx).map(|t| (self.live_buffer(tm, idx).line_count(), self.info(tm, idx, t).private))))
            .ok_or(ApiError::UnknownDoc)?;
        if private {
            return Err(ApiError::UnknownDoc);
        }
        if line == 0 || line > total.max(1) {
            return Err(ApiError::Invalid(format!("line {line} is outside the document ({total} lines)")));
        }
        let replacing = |n: &NoteUi| n.doc == doc && n.owner == owner && n.line == line;
        let owned = self.ui.notes.with_untracked(|v| v.iter().filter(|n| n.owner == owner && !replacing(n)).count());
        if owned >= MAX_NOTES_PER_CLIENT {
            return Err(ApiError::Invalid(format!("at most {MAX_NOTES_PER_CLIENT} notes at a time: remove some with remove:true")));
        }
        let id = self.next_note.get() + 1;
        self.next_note.set(id);
        let note = NoteUi { id, doc: doc.into(), owner: owner.into(), line, kind: kind.into(), text: text.into(), expires: std::time::Instant::now() + Duration::from_secs(ttl_s) };
        self.ui.notes.update(|v| {
            v.retain(|n| !replacing(n));
            v.push(note);
        });
        let (ui, tabs) = (self.ui, self.tabs);
        refresh_notes(ui, tabs);
        // The note disappears by itself when its time is up.
        floem::action::exec_after(Duration::from_secs(ttl_s) + Duration::from_millis(50), move |_| refresh_notes(ui, tabs));
        Ok(())
    }

    fn unannotate(&self, doc: &str, owner: &str, line: Option<usize>) -> ApiResult<usize> {
        let before = self.ui.notes.with_untracked(|v| v.len());
        self.ui.notes.update(|v| v.retain(|n| !(n.doc == doc && n.owner == owner && line.map_or(true, |l| l == n.line))));
        refresh_notes(self.ui, self.tabs);
        Ok(before - self.ui.notes.with_untracked(|v| v.len()))
    }

    fn proposal_pending(&self, p: &ProposalView) {
        self.ui.add_proposal(ProposalUi {
            id: p.id.clone(),
            client: p.client.clone(),
            summary: p.summary.clone(),
            hunks: p.hunks.iter().map(|h| HunkUi { start_line: h.start_line, old_text: h.old_text.clone(), new_text: h.new_text.clone() }).collect(),
        });
        self.status.set(format!("{} proposes a change: review it (bottom right)", if p.client.is_empty() { "An agent" } else { &p.client }));
    }

    fn proposal_closed(&self, id: &str, status: &str) {
        self.ui.close_proposal(id);
        if status != "accepted" && status != "partially_accepted" {
            self.status.set(format!("Agent proposal {status}"));
        }
    }

    fn consent_needed(&self, req: &ConsentRequest) {
        self.ui.add_consent_req(ConsentReq {
            client: req.client.clone(),
            exe_path: req.exe_path.clone(),
            exe_hash: req.exe_hash.clone(),
            folder: req.folder.clone(),
            exe_changed: req.exe_changed,
        });
    }

    fn notice(&self, msg: &str) {
        self.status.set(msg.to_string());
    }
}

// ---------------------------------------------------------------- runtime

struct Running {
    core: Arc<McpCore<Bridge>>,
    host: LinkHost,
    token: String,
}

/// Owns the server's lifecycle. Lives on the UI thread; `start` is the only place a socket is opened.
pub struct LinkRuntime {
    bridge: Arc<Bridge>,
    dirs: LinkDirs,
    ui: LinkUi,
    running: RefCell<Option<Running>>,
}

impl LinkRuntime {
    pub fn new(bridge: Arc<Bridge>, dirs: LinkDirs, ui: LinkUi) -> Self {
        Self { bridge, dirs, ui, running: RefCell::new(None) }
    }

    pub fn is_running(&self) -> bool {
        self.running.borrow().is_some()
    }

    pub fn link_dir(&self) -> &std::path::Path {
        &self.dirs.dir
    }

    /// Start listening if `settings.enabled` and not already running. `Ok(false)` = disabled.
    pub fn start(&self, settings: &McpSettings, workspace_roots: Vec<String>) -> Result<bool, String> {
        if !settings.enabled {
            return Ok(false);
        }
        if self.is_running() {
            return Ok(true);
        }
        // The ceiling is S2 (propose) unless the user explicitly set `allow_apply = true` (S3).
        let cfg = McpConfig { audit_path: Some(self.dirs.dir.join("audit.jsonl")), policy: PolicyConfig { max_scope: settings.ceiling() }, never_share: settings.never_share_config(), ..McpConfig::default() };
        let core = McpCore::new(self.bridge.clone(), cfg).map_err(|e| format!("audit log: {e}"))?;
        let host = LinkHost::start(settings, core.clone(), &self.dirs, workspace_roots).map_err(|e| format!("cannot listen: {e}"))?.ok_or("disabled")?;
        let token = link::load_or_create_token(&self.dirs.dir).map_err(|e| e.to_string())?;
        self.ui.http_url.set(host.http_url());
        *self.running.borrow_mut() = Some(Running { core, host, token });
        self.ui.on.set(true);
        Ok(true)
    }

    /// Stop everything: pending proposals are cancelled, grants dropped, the socket closed.
    pub fn stop(&self) {
        if let Some(r) = self.running.borrow_mut().take() {
            r.core.kill();
            r.host.stop();
        }
        self.ui.on.set(false);
        self.ui.http_url.set(None);
        self.ui.clear_pending();
    }

    /// (connected clients, idle time) of the running server, `None` when it is not running.
    pub fn activity(&self) -> Option<(usize, Duration)> {
        self.running.borrow().as_ref().map(|r| r.core.activity())
    }

    /// The consent modal's answer.
    ///
    /// * a folder question (`req.folder`): only that folder is granted (or denied: deny wins);
    ///   the client's other grants stay;
    /// * a new client or a changed program: its old grants are dropped first, so a stale grant bound
    ///   to the previous program hash cannot linger; the new grants are bound to the current hash.
    pub fn answer_consent(&self, req: &ConsentReq, answer: Consent, folders: &[String], mode: Scope) {
        let Some(core) = self.running.borrow().as_ref().map(|r| r.core.clone()) else { return };
        let client = req.client.as_str();
        let grant = |folder: String, scope: Scope| core.grant(Grant { client: client.to_string(), folder, scope, expires_at: None, exe_hash: req.exe_hash.clone() });
        if let Some(folder) = &req.folder {
            match answer {
                Consent::Deny => grant(folder.clone(), Scope::S0),
                Consent::ReadOnly => grant(folder.clone(), Scope::S1),
                Consent::Write => grant(folder.clone(), mode),
            }
            return;
        }
        match answer {
            Consent::Deny => core.block_client(client),
            Consent::Write | Consent::ReadOnly => {
                core.revoke_client(client);
                let scope = if answer == Consent::Write { mode } else { Scope::S1 };
                // With no file open there is no folder to grant: the link directory itself is the
                // harmless fallback (always hidden from agents), which still allows scratch buffers.
                let fallback = self.dirs.dir.display().to_string();
                let list: Vec<String> = if folders.is_empty() { vec![fallback] } else { folders.to_vec() };
                for folder in list {
                    grant(folder, scope);
                }
            }
        }
    }

    /// Accept (`None` = all hunks / `Some(list)`) or reject (`Some(empty)`). The core applies the edits
    /// through the bridge, which needs the UI thread: so this must not block it.
    pub fn resolve(&self, id: &str, accepted: Option<Vec<usize>>, done: impl FnOnce(String) + Send + 'static) {
        let Some(core) = self.running.borrow().as_ref().map(|r| r.core.clone()) else { return };
        let id = id.to_string();
        std::thread::spawn(move || {
            let msg = match core.resolve_proposal(&id, accepted.as_deref()) {
                Ok(o) => format!("Agent proposal {}", o.status.replace('_', " ")),
                Err(e) => format!("Agent proposal not applied: {}", e.message),
            };
            done(msg);
        });
    }

    /// The Test button: connect like a client would and report. Runs off the UI thread.
    pub fn test(&self, done: impl FnOnce(String) + Send + 'static) {
        let target = self.running.borrow().as_ref().map(|r| (r.host.pipe.clone(), r.token.clone()));
        std::thread::spawn(move || {
            let msg = match target {
                None => "Not running: turn Agents on first.".to_string(),
                Some((pipe, token)) => match link::self_test(&pipe, &token, Duration::from_secs(5)) {
                    Ok(n) => format!("OK: connected, {n} tools offered. A client still needs your permission the first time it uses one."),
                    Err(e) => format!("Failed: {e}"),
                },
            };
            done(msg);
        });
    }
}

impl Drop for LinkRuntime {
    fn drop(&mut self) {
        self.stop();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use floem::reactive::{with_scope, create_rw_signal, Scope};

    fn run<F: FnOnce() + 'static>(f: F) {
        let scope = Scope::new();
        with_scope(scope, f);
        scope.dispose();
    }

    fn state(texts: &[(&str, &str)]) -> UiState {
        let mut tm = TabManager::new();
        for (i, (title, text)) in texts.iter().enumerate() {
            if i > 0 {
                tm.new_scratch();
            }
            let t = tm.current_mut();
            t.title = title.to_string();
            t.buffer = Arc::new(Buffer::from_str(text));
        }
        tm.switch_to(0);
        let buffer = create_rw_signal(tm.current().buffer.clone());
        UiState {
            hooks: Rc::new(LinkHooks::default()),
            next_note: Cell::new(0),
            tabs: create_rw_signal(tm),
            buffer,
            cursor: create_rw_signal((1, 1)),
            selection: create_rw_signal(Selection::new((1, 1), (1, 1), false)),
            status: create_rw_signal(String::new()),
            ui: LinkUi::new(),
            open: Rc::new(|_, _, _| {}),
            activate: Rc::new(|_| {}),
        }
    }

    #[test]
    fn ids_are_stable_distinct_and_opaque() {
        let a = doc_id(Some(std::path::Path::new("C:\\w\\A.txt")), "A.txt");
        assert_eq!(a, doc_id(Some(std::path::Path::new("c:\\w\\a.txt")), "other title"), "path decides, case-insensitive");
        assert_ne!(a, doc_id(None, "A.txt"));
        assert_ne!(doc_id(None, "untitled-1"), doc_id(None, "untitled-2"));
        assert!(!a.contains('\\') && a.starts_with('d'));
    }

    #[test]
    fn docs_snapshot_and_selection_reflect_the_live_buffer() {
        run(|| {
            let s = state(&[("untitled-1", "hello\nworld\n"), ("untitled-2", "second")]);
            let docs = s.docs();
            assert_eq!(docs.len(), 2);
            assert_eq!(s.active_doc().as_deref(), Some(docs[0].id.as_str()));
            assert!(docs.iter().all(|d| !d.private && d.path.is_none() && d.hash.len() == 16));
            // The mirror buffer (what the user types into) is newer than the tab's copy.
            let mut nb = (*s.buffer.get_untracked()).clone();
            nb.insert_at(0, "X");
            s.buffer.set(Arc::new(nb));
            let snap = s.snapshot(&docs[0].id).unwrap();
            assert_eq!(snap.text, "Xhello\nworld\n");
            assert_ne!(snap.info.version, docs[0].version, "an edit bumps the version");
            assert!(snap.selection.is_none());
            s.selection.set(Selection::new((2, 1), (2, 4), false));
            assert_eq!(s.snapshot(&docs[0].id).unwrap().selection, Some((7, 10)));
            assert_eq!(s.snapshot(&docs[1].id).unwrap().text, "second");
            assert!(s.snapshot("nope").is_none());
        });
    }

    #[test]
    fn an_edit_is_one_undo_step_labelled_as_ai_and_stale_versions_are_refused() {
        run(|| {
            let s = state(&[("untitled-1", "alpha beta gamma")]);
            let d = s.docs().remove(0);
            let edits = [TextEdit { start: 0, end: 5, new_text: "ALPHA".into() }, TextEdit { start: 11, end: 16, new_text: "GAMMA".into() }];
            let v = s.apply_edits(&d.id, d.version, &edits, "AI edit: claude: caps").unwrap();
            let b = s.buffer.get_untracked();
            assert_eq!(b.rope().to_string(), "ALPHA beta GAMMA");
            assert_eq!(b.version(), v);
            assert!(b.is_dirty());
            assert!(matches!(b.top_undo_origin(), Some(Origin::Ai(l)) if l == "AI edit: claude: caps"));
            assert_eq!(s.status.get_untracked(), "AI edit: claude: caps");
            // The same base version again: the text moved on.
            assert_eq!(s.apply_edits(&d.id, d.version, &edits, "x"), Err(ApiError::Stale { current: v }));
            // ONE undo restores both hunks.
            let mut nb = (*s.buffer.get_untracked()).clone();
            assert!(nb.undo());
            assert_eq!(nb.rope().to_string(), "alpha beta gamma");
            assert!(!nb.can_undo(), "exactly one group was recorded");
            // Overlapping or out-of-range edits are invalid and change nothing.
            let bad = [TextEdit { start: 0, end: 99, new_text: "x".into() }];
            let v2 = s.buffer.get_untracked().version();
            assert!(matches!(s.apply_edits(&d.id, v2, &bad, "x"), Err(ApiError::Invalid(_))));
            assert_eq!(s.buffer.get_untracked().rope().to_string(), "ALPHA beta GAMMA");
            assert_eq!(s.apply_edits("nope", 0, &edits, "x"), Err(ApiError::UnknownDoc));
        });
    }

    #[test]
    fn a_background_tab_is_edited_in_place_without_becoming_active() {
        run(|| {
            let s = state(&[("untitled-1", "one"), ("untitled-2", "two")]);
            let docs = s.docs();
            s.apply_edits(&docs[1].id, docs[1].version, &[TextEdit { start: 0, end: 3, new_text: "TWO".into() }], "AI edit: c: x").unwrap();
            assert_eq!(s.active_doc().as_deref(), Some(docs[0].id.as_str()));
            assert_eq!(s.snapshot(&docs[1].id).unwrap().text, "TWO");
            assert!(s.snapshot(&docs[1].id).unwrap().info.dirty);
            assert_eq!(s.buffer.get_untracked().rope().to_string(), "one", "the active buffer is untouched");
        });
    }

    #[test]
    fn help_pages_and_unsupported_operations_are_not_exposed() {
        run(|| {
            let s = state(&[("untitled-1", "text")]);
            s.tabs.update(|tm| tm.current_mut().help_doc = Some("welcome"));
            let d = s.docs().remove(0);
            assert!(d.private);
            assert_eq!(s.snapshot(&d.id).unwrap().text, "", "a private document never leaks its text");
            // A private document is invisible to every operation, even if a hook exists.
            *s.hooks.save.borrow_mut() = Some(Rc::new(|_, _| panic!("must not be called")));
            assert_eq!(s.save(&d.id, false), Err(ApiError::UnknownDoc));
            assert_eq!(s.annotate(&d.id, 1, "note", "x", 5, "c"), Err(ApiError::UnknownDoc));
            assert_eq!(s.diagnostics(&d.id), Err(ApiError::UnknownDoc));
        });
    }

    #[test]
    fn run_command_offers_only_the_safe_allowlist_and_nothing_that_edits_or_executes() {
        run(|| {
            let s = state(&[("untitled-1", "text")]);
            let cmds = s.commands();
            assert_eq!(cmds.len(), SAFE_COMMANDS.len());
            assert!(cmds.iter().all(|c| c.ai_exposed && !c.mutates && !c.exec));
            // Every allowlisted id must be a command the editor really has, and none may mutate.
            let (reg, _) = crate::wiring::build_registry();
            for (id, _) in SAFE_COMMANDS {
                let spec = reg.get(id).unwrap_or_else(|| panic!("{id} is not a registered command"));
                assert!(!spec.mutates, "{id} mutates");
                assert!(!id.starts_with("tools_run") && !id.contains("shell") && !id.contains("save"), "{id}");
            }
            // Without a dispatcher nothing runs; with one, only allowlisted ids reach it.
            assert_eq!(s.run_command("view_toggle_hex", &serde_json::json!({})), Err(ApiError::Unsupported));
            let ran: Rc<RefCell<Vec<String>>> = Default::default();
            let (r2, st) = (ran.clone(), s.status);
            *s.hooks.run.borrow_mut() = Some(Rc::new(move |id| {
                r2.borrow_mut().push(id.to_string());
                st.set("12 characters".into());
            }));
            let out = s.run_command("count_text", &serde_json::json!({})).unwrap();
            assert_eq!(out["ran"], "count_text");
            assert_eq!(out["status"], "12 characters", "the answer is the status line the command left");
            for bad in ["file_save", "tools_run_slot_1", "case_upper", "edit_undo", "link_kill", "../x", ""] {
                assert!(matches!(s.run_command(bad, &serde_json::json!({})), Err(ApiError::Invalid(_))), "{bad}");
            }
            assert_eq!(*ran.borrow(), ["count_text"]);
        });
    }

    #[test]
    fn diagnostics_report_what_the_editor_really_checks() {
        run(|| {
            let s = state(&[("untitled-1", "a\r\nb\nc\r\n"), ("untitled-2", "fine\n")]);
            let docs = s.docs();
            let d = s.diagnostics(&docs[0].id).unwrap();
            assert_eq!(d.len(), 1);
            assert_eq!((d[0].source.as_str(), d[0].severity.as_str()), ("eol", "warning"));
            assert!(s.diagnostics(&docs[1].id).unwrap().is_empty(), "nothing found is an honest empty list");
            assert_eq!(s.diagnostics("nope"), Err(ApiError::UnknownDoc));
        });
    }

    #[test]
    fn annotations_are_owned_capped_expire_and_follow_the_active_tab() {
        run(|| {
            let s = state(&[("untitled-1", "one\ntwo\nthree\n"), ("untitled-2", "other\n")]);
            let docs = s.docs();
            s.annotate(&docs[0].id, 2, "note", "look here", 60, "claude").unwrap();
            s.annotate(&docs[0].id, 2, "note", "look again", 60, "claude").unwrap();
            assert_eq!(s.ui.notes.get_untracked().len(), 1, "same owner and line replaces");
            s.annotate(&docs[0].id, 2, "highlight", "", 60, "codex").unwrap();
            assert_eq!(s.ui.notes.get_untracked().len(), 2);
            // The rows show the notes of the active document only.
            assert_eq!(s.ui.line_notes.get_untracked().len(), 2);
            assert!(s.ui.line_notes.get_untracked().iter().any(|n| n.owner == "claude" && n.text == "look again"));
            s.tabs.update(|tm| tm.switch_to(1));
            refresh_notes(s.ui, s.tabs);
            assert!(s.ui.line_notes.get_untracked().is_empty());
            s.tabs.update(|tm| tm.switch_to(0));
            refresh_notes(s.ui, s.tabs);
            // The buffer text never changes.
            assert_eq!(s.snapshot(&docs[0].id).unwrap().text, "one\ntwo\nthree\n");
            // Lines outside the document are refused.
            assert!(matches!(s.annotate(&docs[0].id, 99, "note", "x", 5, "claude"), Err(ApiError::Invalid(_))));
            assert!(matches!(s.annotate(&docs[0].id, 0, "note", "x", 5, "claude"), Err(ApiError::Invalid(_))));
            assert_eq!(s.annotate("nope", 1, "note", "x", 5, "claude"), Err(ApiError::UnknownDoc));
            // Removal is by owner: codex cannot remove claude's note.
            assert_eq!(s.unannotate(&docs[0].id, "codex", Some(2)), Ok(1));
            assert_eq!(s.unannotate(&docs[0].id, "codex", None), Ok(0));
            assert_eq!(s.ui.notes.get_untracked()[0].owner, "claude");
            assert_eq!(s.unannotate(&docs[0].id, "claude", None), Ok(1));
            assert!(s.ui.line_notes.get_untracked().is_empty());
            // At most 20 notes per client.
            for l in 1..=3 {
                s.annotate(&docs[0].id, l, "note", "n", 60, "spam").unwrap();
            }
            s.buffer.set(Arc::new(Buffer::from_str(&"x\n".repeat(40))));
            for l in 4..=20 {
                s.annotate(&docs[0].id, l, "note", "n", 60, "spam").unwrap();
            }
            assert!(matches!(s.annotate(&docs[0].id, 21, "note", "n", 60, "spam"), Err(ApiError::Invalid(_))));
            s.annotate(&docs[0].id, 21, "note", "n", 60, "other-client").unwrap();
            // An expired note disappears on the next refresh.
            s.ui.notes.update(|v| v.iter_mut().for_each(|n| n.expires = std::time::Instant::now() - Duration::from_secs(1)));
            refresh_notes(s.ui, s.tabs);
            assert!(s.ui.notes.get_untracked().is_empty() && s.ui.line_notes.get_untracked().is_empty());
        });
    }

    #[test]
    fn save_goes_through_the_hook_and_maps_every_failure() {
        run(|| {
            let s = state(&[("untitled-1", "x")]);
            let d = s.docs().remove(0);
            assert_eq!(s.save(&d.id, false), Err(ApiError::Unsupported), "no hook yet");
            let calls: Rc<RefCell<Vec<(usize, bool)>>> = Default::default();
            let mode: Rc<Cell<u8>> = Default::default();
            let (c2, m2) = (calls.clone(), mode.clone());
            *s.hooks.save.borrow_mut() = Some(Rc::new(move |i, f| {
                c2.borrow_mut().push((i, f));
                match m2.get() {
                    0 => Ok(()),
                    1 => Err(SaveFail::Conflict("disk changed".into())),
                    2 => Err(SaveFail::Lossy),
                    _ => Err(SaveFail::Io("read-only".into())),
                }
            }));
            assert_eq!(s.save(&d.id, true), Ok(()));
            mode.set(1);
            assert_eq!(s.save(&d.id, false), Err(ApiError::Conflict("disk changed".into())));
            mode.set(2);
            assert!(matches!(s.save(&d.id, false), Err(ApiError::Invalid(m)) if m.contains("only the user")));
            mode.set(3);
            assert_eq!(s.save(&d.id, false), Err(ApiError::Io("read-only".into())));
            assert_eq!(s.save("nope", false), Err(ApiError::UnknownDoc));
            assert_eq!(*calls.borrow(), [(0, true), (0, false), (0, false), (0, false)]);
        });
    }

    #[test]
    fn save_for_agent_refuses_after_an_external_change_unless_forced_and_never_opens_a_dialog() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("a.txt");
        std::fs::write(&p, "disk v1\n").unwrap();
        crate::external::note(&p);
        let meta = crate::tabs::DocMeta::default();
        let opts = crate::backup::BackupOptions { enabled: false, ..Default::default() };
        // In sync: it writes.
        assert_eq!(save_for_agent(&p, &crop::Rope::from("mine v1\n"), &meta, false, &opts), Ok(()));
        assert_eq!(std::fs::read_to_string(&p).unwrap(), "mine v1\n");
        // Someone else changes the file behind the editor's back.
        std::thread::sleep(Duration::from_millis(30));
        std::fs::write(&p, "someone else wrote this, longer\n").unwrap();
        let r = save_for_agent(&p, &crop::Rope::from("mine v2\n"), &meta, false, &opts);
        assert!(matches!(r, Err(SaveFail::Conflict(_))), "{r:?}");
        assert_eq!(std::fs::read_to_string(&p).unwrap(), "someone else wrote this, longer\n", "nothing was written");
        // Forcing overwrites (the agent had to say so explicitly).
        assert_eq!(save_for_agent(&p, &crop::Rope::from("mine v2\n"), &meta, true, &opts), Ok(()));
        assert_eq!(std::fs::read_to_string(&p).unwrap(), "mine v2\n");
        // A lossy encoding is refused outright (no "save anyway?" dialog for an agent).
        let mut latin = crate::tabs::DocMeta::default();
        latin.codepage = "windows-1252";
        let r = save_for_agent(&p, &crop::Rope::from("snowman \u{2603}\n"), &latin, true, &opts);
        assert_eq!(r, Err(SaveFail::Lossy));
        assert_eq!(std::fs::read_to_string(&p).unwrap(), "mine v2\n");
    }

    #[test]
    fn the_persistent_kill_writes_enabled_false_and_keeps_the_rest_of_the_settings() {
        let dir = tempfile::tempdir().unwrap();
        let f = dir.path().join("sub").join("settings.toml");
        persist_disabled(&f).unwrap();
        assert!(!McpSettings::from_toml(&std::fs::read_to_string(&f).unwrap()).enabled);
        std::fs::write(&f, "[editor]\nfont = 12\n\n[mcp]\nenabled = true\nmode = \"read\"\nauto_off_minutes = 15\n").unwrap();
        assert!(McpSettings::from_toml(&std::fs::read_to_string(&f).unwrap()).enabled);
        persist_disabled(&f).unwrap();
        let t = std::fs::read_to_string(&f).unwrap();
        let s = McpSettings::from_toml(&t);
        assert!(!s.enabled, "{t}");
        assert_eq!((s.mode.as_str(), s.auto_off_minutes), ("read", Some(15)));
        assert!(t.contains("font = 12"), "{t}");
    }

    #[test]
    fn consent_answers_bind_the_program_hash_and_folder_answers_only_touch_their_folder() {
        run(|| {
            let (bridge, _pump) = brouillon_mcp::bridge::channel_pair(|| {}, Duration::from_millis(200));
            let dir = tempfile::tempdir().unwrap();
            let dirs = LinkDirs { dir: dir.path().join("link"), pipe_tag: format!("rc{}", std::process::id()) };
            let rt = LinkRuntime::new(Arc::new(bridge), dirs, LinkUi::new());
            assert_eq!(rt.start(&McpSettings::default().enable_for_session(), vec![]), Ok(true));
            let core = rt.running.borrow().as_ref().unwrap().core.clone();
            let req = ConsentReq { exe_hash: Some("h1".into()), ..ConsentReq::new("claude") };
            rt.answer_consent(&req, Consent::Write, &["C:\\a".into(), "C:\\b".into()], brouillon_mcp::policy::Scope::S2);
            let hashes: Vec<_> = core.grants_for("claude").iter().map(|g| (g.folder.clone(), g.scope, g.exe_hash.clone())).collect();
            assert_eq!(hashes, [("C:\\a".into(), brouillon_mcp::policy::Scope::S2, Some("h1".into())), ("C:\\b".into(), brouillon_mcp::policy::Scope::S2, Some("h1".into()))]);
            // A changed program: the old grants are replaced, not stacked.
            let changed = ConsentReq { exe_hash: Some("h2".into()), exe_changed: true, ..ConsentReq::new("claude") };
            rt.answer_consent(&changed, Consent::ReadOnly, &["C:\\a".into()], brouillon_mcp::policy::Scope::S2);
            let g = core.grants_for("claude");
            assert_eq!(g.len(), 1);
            assert_eq!((g[0].scope, g[0].exe_hash.as_deref()), (brouillon_mcp::policy::Scope::S1, Some("h2")));
            // A later folder: only that folder is granted, the old grant stays; deny wins on its folder.
            let later = ConsentReq { folder: Some("C:\\later".into()), exe_hash: Some("h2".into()), ..ConsentReq::new("claude") };
            rt.answer_consent(&later, Consent::Write, &[], brouillon_mcp::policy::Scope::S2);
            assert_eq!(core.grants_for("claude").len(), 2);
            let denied = ConsentReq { folder: Some("C:\\secret".into()), exe_hash: Some("h2".into()), ..ConsentReq::new("claude") };
            rt.answer_consent(&denied, Consent::Deny, &[], brouillon_mcp::policy::Scope::S2);
            assert!(core.grants_for("claude").iter().any(|g| g.folder == "C:\\secret" && g.scope == brouillon_mcp::policy::Scope::S0));
            rt.stop();
        });
    }

    #[test]
    fn notifications_feed_the_overlay_state() {
        run(|| {
            let s = state(&[("untitled-1", "x")]);
            s.consent_needed(&ConsentRequest { client: "claude-code".into(), ..Default::default() });
            s.consent_needed(&ConsentRequest { client: "claude-code".into(), ..Default::default() });
            assert_eq!(s.ui.consent.get_untracked(), vec![ConsentReq::new("claude-code")]);
            let view = ProposalView { id: "p1".into(), client: "claude-code".into(), doc: "d".into(), summary: "rename".into(), hunks: vec![brouillon_mcp::api::HunkView { start_line: 1, old_text: "a".into(), new_text: "b".into() }] };
            s.proposal_pending(&view);
            assert_eq!(s.ui.proposals.get_untracked().len(), 1);
            assert!(s.status.get_untracked().contains("review"));
            s.proposal_closed("p1", "rejected");
            assert!(s.ui.proposals.get_untracked().is_empty());
            assert!(s.status.get_untracked().contains("rejected"));
        });
    }

    #[test]
    fn the_runtime_opens_nothing_until_enabled_and_stop_closes_it() {
        run(|| {
            let (bridge, _pump) = brouillon_mcp::bridge::channel_pair(|| {}, Duration::from_millis(200));
            let dir = tempfile::tempdir().unwrap();
            let dirs = LinkDirs { dir: dir.path().join("link"), pipe_tag: format!("rt{}", std::process::id()) };
            let ui = LinkUi::new();
            let rt = LinkRuntime::new(Arc::new(bridge), dirs, ui);
            assert_eq!(rt.start(&McpSettings::default(), vec![]), Ok(false));
            assert!(!rt.is_running() && !ui.on.get_untracked());
            assert!(!dir.path().join("link").exists(), "disabled: not even the folder is created");
            assert_eq!(rt.start(&McpSettings::default().enable_for_session(), vec![]), Ok(true));
            assert!(rt.is_running() && ui.on.get_untracked());
            assert!(dir.path().join("link").join("link.token").is_file());
            ui.add_consent("x");
            rt.stop();
            assert!(!rt.is_running() && !ui.on.get_untracked());
            assert!(ui.consent.get_untracked().is_empty(), "nothing is left to answer after a stop");
            let (tx, rx) = std::sync::mpsc::channel();
            rt.test(move |m| tx.send(m).unwrap());
            assert!(rx.recv_timeout(Duration::from_secs(3)).unwrap().contains("Not running"));
        });
    }
}
