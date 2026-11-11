//! The editor-side contract. The GUI implements [`EditorApi`] later; tests use [`crate::fake::FakeEditor`].
//!
//! Offsets are UTF-8 byte offsets on char boundaries. Every mutation goes through
//! [`EditorApi::apply_edits`], which must be ONE undo group carrying `label`.

use serde::Serialize;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DocInfo {
    pub id: String,
    pub path: Option<String>,
    pub dirty: bool,
    pub lang: String,
    /// Monotonic, incremented on every edit including undo.
    pub version: u64,
    /// Content hash (hex) of the current text.
    pub hash: String,
    pub encoding: String,
    pub eol: String,
    /// "Private buffer": excluded from every tool.
    pub private: bool,
    pub window: u32,
}

/// Atomic view of one buffer: info and text taken together.
#[derive(Debug, Clone)]
pub struct DocSnapshot {
    pub info: DocInfo,
    pub text: String,
    /// Byte range of the user's selection, if any.
    pub selection: Option<(usize, usize)>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TextEdit {
    pub start: usize,
    pub end: usize,
    pub new_text: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Diagnostic {
    pub line: usize,
    pub col: usize,
    pub severity: String,
    pub message: String,
    pub source: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CommandInfo {
    pub id: String,
    pub caption: String,
    pub mutates: bool,
    pub exec: bool,
    /// Only `ai_exposed` commands are ever visible to agents (default false).
    pub ai_exposed: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ApiError {
    UnknownDoc,
    /// The buffer moved on; carries the current version.
    Stale { current: u64 },
    Invalid(String),
    Io(String),
    /// The file on disk changed since it was opened/saved (retry with `force`).
    Conflict(String),
    Unsupported,
}

pub type ApiResult<T> = Result<T, ApiError>;

/// What the editor exposes to the MCP core. Must be callable from any thread (the real
/// implementation marshals to the UI thread with a timeout).
pub trait EditorApi: Send + Sync {
    fn docs(&self) -> Vec<DocInfo>;
    fn active_doc(&self) -> Option<String>;
    fn workspace_folders(&self) -> Vec<String>;
    fn snapshot(&self, doc: &str) -> Option<DocSnapshot>;
    /// Read a file that is not open (caller already confined the path).
    fn read_file(&self, path: &str) -> ApiResult<String>;
    /// Apply `edits` as ONE undo group labelled `label`, only if `expected_version` still matches.
    /// Returns the new version.
    fn apply_edits(&self, doc: &str, expected_version: u64, edits: &[TextEdit], label: &str) -> ApiResult<u64>;
    fn open(&self, path: &str, line: Option<usize>, col: Option<usize>, focus: bool) -> ApiResult<String>;
    fn diagnostics(&self, doc: &str) -> ApiResult<Vec<Diagnostic>>;
    fn commands(&self) -> Vec<CommandInfo>;
    fn run_command(&self, id: &str, args: &serde_json::Value) -> ApiResult<serde_json::Value>;
    fn save(&self, doc: &str, force: bool) -> ApiResult<()>;
    /// A visible, removable note owned by `owner` (the client name). Never changes the text.
    fn annotate(&self, doc: &str, line: usize, kind: &str, text: &str, ttl_s: u64, owner: &str) -> ApiResult<()>;
    /// Remove the notes `owner` placed on `doc` (one line, or all when `line` is `None`); returns how many.
    fn unannotate(&self, doc: &str, owner: &str, line: Option<usize>) -> ApiResult<usize>;

    // UI notifications (default no-ops so a minimal implementation compiles).
    /// A proposal is pending: render the overlay.
    fn proposal_pending(&self, _proposal: &ProposalView) {}
    /// A proposal left the pending state (accepted/rejected/cancelled/expired/stale).
    fn proposal_closed(&self, _id: &str, _status: &str) {}
    /// A client needs the user's answer: unknown client, changed program, or a folder opened later.
    fn consent_needed(&self, _req: &ConsentRequest) {}
    /// Something the user should see in the status line (rate limit, circuit breaker).
    fn notice(&self, _msg: &str) {}
}

/// Why and for whom the consent modal is shown.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ConsentRequest {
    pub client: String,
    /// Path of the client program (as reported by the relay), informational.
    pub exe_path: Option<String>,
    /// Hash of that program: the grant is bound to it.
    pub exe_hash: Option<String>,
    /// `Some(folder)`: the client is already allowed elsewhere and now touches this folder.
    pub folder: Option<String>,
    /// The program changed since the user allowed it.
    pub exe_changed: bool,
}

/// What the UI needs to draw a proposal (never sent to the audit log).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProposalView {
    pub id: String,
    pub client: String,
    pub doc: String,
    pub summary: String,
    pub hunks: Vec<HunkView>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HunkView {
    pub start_line: usize,
    pub old_text: String,
    pub new_text: String,
}
