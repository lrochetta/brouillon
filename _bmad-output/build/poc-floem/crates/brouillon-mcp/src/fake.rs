//! In-memory [`EditorApi`] used by tests (and by the GUI lot as a behavioural reference).

use std::collections::HashMap;
use std::sync::Mutex;

use sha2::{Digest, Sha256};

use crate::api::*;

#[derive(Clone)]
struct Doc {
    id: String,
    path: Option<String>,
    text: String,
    version: u64,
    private: bool,
    dirty: bool,
    selection: Option<(usize, usize)>,
    /// (text before, version before) per undo group.
    undo: Vec<String>,
}

#[derive(Default)]
struct State {
    docs: Vec<Doc>,
    active: Option<String>,
    folders: Vec<String>,
    /// Labels of the undo groups created, in order.
    groups: Vec<String>,
    opened: Vec<(String, bool)>,
    annotations: Vec<(String, usize, String, String)>,
    owners: Vec<String>,
    consent_reqs: Vec<ConsentRequest>,
    notices: Vec<String>,
    save_conflict: bool,
    saved: Vec<String>,
    files: HashMap<String, String>,
    commands: Vec<CommandInfo>,
    ran: Vec<String>,
    events: Vec<String>,
    consent: Vec<String>,
    next_doc: u32,
}

#[derive(Default)]
pub struct FakeEditor {
    s: Mutex<State>,
}

pub fn hash_text(t: &str) -> String {
    let d = Sha256::digest(t.as_bytes());
    d[..8].iter().map(|b| format!("{b:02x}")).collect()
}

impl FakeEditor {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn add_folder(&self, f: &str) {
        self.s.lock().unwrap().folders.push(f.into());
    }

    /// Add an open document; returns its id.
    pub fn add_doc(&self, path: Option<&str>, text: &str) -> String {
        let mut s = self.s.lock().unwrap();
        s.next_doc += 1;
        let id = format!("d{}", s.next_doc);
        s.docs.push(Doc {
            id: id.clone(),
            path: path.map(Into::into),
            text: text.into(),
            version: 1,
            private: false,
            dirty: path.is_none(),
            selection: None,
            undo: vec![],
        });
        if s.active.is_none() {
            s.active = Some(id.clone());
        }
        id
    }

    pub fn add_file(&self, path: &str, text: &str) {
        self.s.lock().unwrap().files.insert(path.into(), text.into());
    }

    pub fn set_private(&self, doc: &str, private: bool) {
        if let Some(d) = self.s.lock().unwrap().docs.iter_mut().find(|d| d.id == doc) {
            d.private = private;
        }
    }

    pub fn set_selection(&self, doc: &str, sel: (usize, usize)) {
        if let Some(d) = self.s.lock().unwrap().docs.iter_mut().find(|d| d.id == doc) {
            d.selection = Some(sel);
        }
    }

    pub fn add_command(&self, c: CommandInfo) {
        self.s.lock().unwrap().commands.push(c);
    }

    /// Simulate the human typing: replaces the whole text and bumps the version.
    pub fn human_edit(&self, doc: &str, new_text: &str) {
        if let Some(d) = self.s.lock().unwrap().docs.iter_mut().find(|d| d.id == doc) {
            d.text = new_text.into();
            d.version += 1;
            d.dirty = true;
        }
    }

    pub fn text(&self, doc: &str) -> String {
        self.s.lock().unwrap().docs.iter().find(|d| d.id == doc).map(|d| d.text.clone()).unwrap_or_default()
    }

    pub fn version(&self, doc: &str) -> u64 {
        self.s.lock().unwrap().docs.iter().find(|d| d.id == doc).map(|d| d.version).unwrap_or(0)
    }

    /// Undo the last undo group (one gesture). Bumps the version like the real editor.
    pub fn undo(&self, doc: &str) -> bool {
        let mut s = self.s.lock().unwrap();
        let Some(d) = s.docs.iter_mut().find(|d| d.id == doc) else { return false };
        match d.undo.pop() {
            Some(prev) => {
                d.text = prev;
                d.version += 1;
                true
            }
            None => false,
        }
    }

    pub fn groups(&self) -> Vec<String> {
        self.s.lock().unwrap().groups.clone()
    }
    pub fn opened(&self) -> Vec<(String, bool)> {
        self.s.lock().unwrap().opened.clone()
    }
    pub fn annotations(&self) -> Vec<(String, usize, String, String)> {
        self.s.lock().unwrap().annotations.clone()
    }
    pub fn saved(&self) -> Vec<String> {
        self.s.lock().unwrap().saved.clone()
    }
    pub fn ran(&self) -> Vec<String> {
        self.s.lock().unwrap().ran.clone()
    }
    pub fn events(&self) -> Vec<String> {
        self.s.lock().unwrap().events.clone()
    }
    pub fn consent_requests(&self) -> Vec<String> {
        self.s.lock().unwrap().consent.clone()
    }
    pub fn consent_reqs(&self) -> Vec<ConsentRequest> {
        self.s.lock().unwrap().consent_reqs.clone()
    }
    pub fn notices(&self) -> Vec<String> {
        self.s.lock().unwrap().notices.clone()
    }
    pub fn annotation_owners(&self) -> Vec<String> {
        self.s.lock().unwrap().owners.clone()
    }
    /// Simulate "the file changed on disk": `save` without `force` is refused.
    pub fn set_save_conflict(&self, on: bool) {
        self.s.lock().unwrap().save_conflict = on;
    }
}

fn info(d: &Doc) -> DocInfo {
    DocInfo {
        id: d.id.clone(),
        path: d.path.clone(),
        dirty: d.dirty,
        lang: "text".into(),
        version: d.version,
        hash: hash_text(&d.text),
        encoding: "utf-8".into(),
        eol: if d.text.contains("\r\n") { "crlf" } else { "lf" }.into(),
        private: d.private,
        window: 1,
    }
}

impl EditorApi for FakeEditor {
    fn docs(&self) -> Vec<DocInfo> {
        self.s.lock().unwrap().docs.iter().map(info).collect()
    }
    fn active_doc(&self) -> Option<String> {
        self.s.lock().unwrap().active.clone()
    }
    fn workspace_folders(&self) -> Vec<String> {
        self.s.lock().unwrap().folders.clone()
    }
    fn snapshot(&self, doc: &str) -> Option<DocSnapshot> {
        let s = self.s.lock().unwrap();
        s.docs.iter().find(|d| d.id == doc).map(|d| DocSnapshot { info: info(d), text: d.text.clone(), selection: d.selection })
    }
    fn read_file(&self, path: &str) -> ApiResult<String> {
        self.s.lock().unwrap().files.get(path).cloned().ok_or_else(|| ApiError::Io("not found".into()))
    }
    fn apply_edits(&self, doc: &str, expected_version: u64, edits: &[TextEdit], label: &str) -> ApiResult<u64> {
        let mut s = self.s.lock().unwrap();
        let d = s.docs.iter_mut().find(|d| d.id == doc).ok_or(ApiError::UnknownDoc)?;
        if d.version != expected_version {
            return Err(ApiError::Stale { current: d.version });
        }
        let before = d.text.clone();
        let mut sorted = edits.to_vec();
        sorted.sort_by_key(|e| std::cmp::Reverse(e.start));
        let mut t = d.text.clone();
        for e in &sorted {
            if e.start > e.end || e.end > t.len() || !t.is_char_boundary(e.start) || !t.is_char_boundary(e.end) {
                return Err(ApiError::Invalid("bad range".into()));
            }
            t.replace_range(e.start..e.end, &e.new_text);
        }
        d.undo.push(before);
        d.text = t;
        d.version += 1;
        d.dirty = true;
        let v = d.version;
        s.groups.push(label.to_string());
        Ok(v)
    }
    fn open(&self, path: &str, _line: Option<usize>, _col: Option<usize>, focus: bool) -> ApiResult<String> {
        let mut s = self.s.lock().unwrap();
        s.opened.push((path.to_string(), focus));
        if let Some(d) = s.docs.iter().find(|d| d.path.as_deref() == Some(path)) {
            return Ok(d.id.clone());
        }
        let text = s.files.get(path).cloned().ok_or_else(|| ApiError::Io("not found".into()))?;
        s.next_doc += 1;
        let id = format!("d{}", s.next_doc);
        s.docs.push(Doc { id: id.clone(), path: Some(path.into()), text, version: 1, private: false, dirty: false, selection: None, undo: vec![] });
        Ok(id)
    }
    fn diagnostics(&self, doc: &str) -> ApiResult<Vec<Diagnostic>> {
        let s = self.s.lock().unwrap();
        let d = s.docs.iter().find(|d| d.id == doc).ok_or(ApiError::UnknownDoc)?;
        // Parser-lite: an unbalanced '(' on a line is reported.
        let mut out = vec![];
        for (i, l) in d.text.lines().enumerate() {
            if l.matches('(').count() > l.matches(')').count() {
                out.push(Diagnostic { line: i + 1, col: 1, severity: "error".into(), message: "unclosed bracket".into(), source: "fake".into() });
            }
        }
        Ok(out)
    }
    fn commands(&self) -> Vec<CommandInfo> {
        self.s.lock().unwrap().commands.clone()
    }
    fn run_command(&self, id: &str, _args: &serde_json::Value) -> ApiResult<serde_json::Value> {
        let mut s = self.s.lock().unwrap();
        if !s.commands.iter().any(|c| c.id == id) {
            return Err(ApiError::Invalid("unknown command".into()));
        }
        s.ran.push(id.into());
        Ok(serde_json::json!({"ran": id}))
    }
    fn save(&self, doc: &str, force: bool) -> ApiResult<()> {
        let mut s = self.s.lock().unwrap();
        if s.save_conflict && !force {
            return Err(ApiError::Conflict("the file changed on disk".into()));
        }
        let d = s.docs.iter_mut().find(|d| d.id == doc).ok_or(ApiError::UnknownDoc)?;
        d.dirty = false;
        s.saved.push(doc.into());
        Ok(())
    }
    fn annotate(&self, doc: &str, line: usize, kind: &str, text: &str, _ttl_s: u64, owner: &str) -> ApiResult<()> {
        let mut s = self.s.lock().unwrap();
        if !s.docs.iter().any(|d| d.id == doc) {
            return Err(ApiError::UnknownDoc);
        }
        s.annotations.push((doc.into(), line, kind.into(), text.into()));
        s.owners.push(owner.into());
        Ok(())
    }
    fn unannotate(&self, doc: &str, owner: &str, line: Option<usize>) -> ApiResult<usize> {
        let mut s = self.s.lock().unwrap();
        let mut removed = 0;
        let mut i = 0;
        while i < s.annotations.len() {
            let a = &s.annotations[i];
            if a.0 == doc && s.owners[i] == owner && line.map_or(true, |l| l == a.1) {
                s.annotations.remove(i);
                s.owners.remove(i);
                removed += 1;
            } else {
                i += 1;
            }
        }
        Ok(removed)
    }
    fn proposal_pending(&self, p: &ProposalView) {
        self.s.lock().unwrap().events.push(format!("pending:{}", p.id));
    }
    fn proposal_closed(&self, id: &str, status: &str) {
        self.s.lock().unwrap().events.push(format!("closed:{id}:{status}"));
    }
    fn consent_needed(&self, req: &ConsentRequest) {
        let mut s = self.s.lock().unwrap();
        s.consent.push(req.client.clone());
        s.consent_reqs.push(req.clone());
    }
    fn notice(&self, msg: &str) {
        self.s.lock().unwrap().notices.push(msg.into());
    }
}
