//! Lot 4 — pure model behind the Find in Files results panel (BK-024).
//!
//! No UI types here: the panel (`results_panel.rs`) renders it, the shell
//! feeds it `FileResult`s and routes F4 / Shift+F4 / Enter to it. Several
//! searches are kept (`sets`); the active one is shown.

use std::path::PathBuf;

use brouillon_editor_engine::search_files::FileResult;

/// "Open this location" request. `line` and `col` are 0-based
/// (`col` = byte column in the line).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpenRequest {
    pub path: PathBuf,
    pub line: usize,
    pub col: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResultEntry {
    pub line: usize,
    pub col: usize,
    /// Match length in bytes, starting at `col` (clamped to the line text).
    pub len: usize,
    pub text: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileGroup {
    pub path: PathBuf,
    pub entries: Vec<ResultEntry>,
    pub collapsed: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchSet {
    pub title: String,
    pub groups: Vec<FileGroup>,
    /// Collapse several hits on the same line into one entry.
    pub one_per_line: bool,
}

type Pos = (usize, usize); // (group, entry)

impl SearchSet {
    fn new(title: &str) -> Self {
        Self { title: title.to_string(), groups: Vec::new(), one_per_line: false }
    }

    /// Entries currently shown (collapsed groups hidden, per-line dedup).
    pub fn visible(&self) -> Vec<Pos> {
        let mut out = Vec::new();
        for (g, grp) in self.groups.iter().enumerate() {
            if grp.collapsed {
                continue;
            }
            let mut last_line = None;
            for (e, ent) in grp.entries.iter().enumerate() {
                if self.one_per_line && last_line == Some(ent.line) {
                    continue;
                }
                last_line = Some(ent.line);
                out.push((g, e));
            }
        }
        out
    }

    pub fn hit_count(&self) -> usize {
        self.groups.iter().map(|g| g.entries.len()).sum()
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ResultsModel {
    sets: Vec<SearchSet>,
    active: usize,
    cursor: Option<Pos>,
}

/// Oldest searches are dropped beyond this many.
const MAX_SETS: usize = 20;

impl ResultsModel {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn sets(&self) -> &[SearchSet] {
        &self.sets
    }

    pub fn active_index(&self) -> usize {
        self.active
    }

    pub fn active(&self) -> Option<&SearchSet> {
        self.sets.get(self.active)
    }

    fn active_mut(&mut self) -> Option<&mut SearchSet> {
        self.sets.get_mut(self.active)
    }

    /// Start a new search; previous ones are kept and stay selectable.
    pub fn begin_search(&mut self, title: &str) {
        if self.sets.len() >= MAX_SETS {
            self.sets.remove(0);
        }
        self.sets.push(SearchSet::new(title));
        self.active = self.sets.len() - 1;
        self.cursor = None;
    }

    /// Append one file's hits to the active search (streaming).
    pub fn add_file(&mut self, r: &FileResult) {
        if self.sets.is_empty() {
            self.begin_search("");
        }
        let entries = r
            .hits
            .iter()
            .map(|h| ResultEntry {
                line: h.line,
                col: h.col,
                len: h.matched.len().min(h.line_text.len().saturating_sub(h.col)),
                text: h.line_text.clone(),
            })
            .collect();
        if let Some(s) = self.active_mut() {
            s.groups.push(FileGroup { path: r.path.clone(), entries, collapsed: false });
        }
    }

    pub fn select_set(&mut self, index: usize) {
        if index < self.sets.len() {
            self.active = index;
            self.cursor = None;
        }
    }

    /// Clear the active search's results (history keeps the others).
    pub fn clear(&mut self) {
        if self.active < self.sets.len() {
            self.sets.remove(self.active);
            self.active = self.active.saturating_sub(1);
        }
        self.cursor = None;
    }

    pub fn clear_all(&mut self) {
        *self = Self::default();
    }

    pub fn set_one_per_line(&mut self, on: bool) {
        if let Some(s) = self.active_mut() {
            s.one_per_line = on;
        }
    }

    pub fn toggle_fold(&mut self, group: usize) {
        if let Some(g) = self.active_mut().and_then(|s| s.groups.get_mut(group)) {
            g.collapsed = !g.collapsed;
        }
    }

    pub fn set_all_folded(&mut self, collapsed: bool) {
        if let Some(s) = self.active_mut() {
            s.groups.iter_mut().for_each(|g| g.collapsed = collapsed);
        }
    }

    pub fn cursor(&self) -> Option<(usize, usize)> {
        self.cursor
    }

    fn request(&self, (g, e): Pos) -> Option<OpenRequest> {
        let grp = self.active()?.groups.get(g)?;
        let ent = grp.entries.get(e)?;
        Some(OpenRequest { path: grp.path.clone(), line: ent.line, col: ent.col })
    }

    /// Select a specific entry (mouse click / double-click / Enter).
    pub fn select(&mut self, group: usize, entry: usize) -> Option<OpenRequest> {
        let req = self.request((group, entry))?;
        self.cursor = Some((group, entry));
        Some(req)
    }

    /// Request for the currently selected entry (Enter).
    pub fn current(&self) -> Option<OpenRequest> {
        self.request(self.cursor?)
    }

    /// F4: next visible hit after the cursor, wrapping.
    pub fn next(&mut self) -> Option<OpenRequest> {
        let vis = self.active()?.visible();
        let pos = match self.cursor {
            None => *vis.first()?,
            Some(c) => vis.iter().copied().find(|&p| p > c).or_else(|| vis.first().copied())?,
        };
        self.select(pos.0, pos.1)
    }

    /// Shift+F4: previous visible hit before the cursor, wrapping.
    pub fn prev(&mut self) -> Option<OpenRequest> {
        let vis = self.active()?.visible();
        let pos = match self.cursor {
            None => *vis.last()?,
            Some(c) => vis.iter().copied().rev().find(|&p| p < c).or_else(|| vis.last().copied())?,
        };
        self.select(pos.0, pos.1)
    }

    /// `path:line:text` (1-based line) for one entry.
    pub fn copy_entry(&self, group: usize, entry: usize) -> Option<String> {
        let grp = self.active()?.groups.get(group)?;
        let ent = grp.entries.get(entry)?;
        Some(format!("{}:{}:{}", grp.path.display(), ent.line + 1, ent.text))
    }

    /// All visible entries of the active search, one `path:line:text` per line.
    pub fn copy_text(&self) -> String {
        let Some(s) = self.active() else { return String::new() };
        s.visible().into_iter().filter_map(|(g, e)| self.copy_entry(g, e)).collect::<Vec<_>>().join("\n")
    }

    /// Display title, e.g. `"needle" - 12 hits in 3 files`.
    pub fn status(&self) -> String {
        match self.active() {
            Some(s) => format!("{} - {} hit(s) in {} file(s)", s.title, s.hit_count(), s.groups.len()),
            None => "No results".into(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use brouillon_editor_engine::search_files::Hit;

    fn hit(line: usize, col: usize, text: &str) -> Hit {
        Hit { line, col, start_byte: 0, end_byte: 0, line_text: text.into(), matched: "x".into() }
    }

    fn file(name: &str, hits: Vec<Hit>) -> FileResult {
        FileResult { path: PathBuf::from(name), hits, from_buffer: false }
    }

    fn model() -> ResultsModel {
        let mut m = ResultsModel::new();
        m.begin_search("x");
        m.add_file(&file("a.rs", vec![hit(0, 1, "ax"), hit(0, 5, "ax x"), hit(4, 0, "x")]));
        m.add_file(&file("b.rs", vec![hit(2, 3, "abcx")]));
        m
    }

    #[test]
    fn next_prev_wrap_and_carry_location() {
        let mut m = model();
        let r = m.next().unwrap();
        assert_eq!((r.path, r.line, r.col), (PathBuf::from("a.rs"), 0, 1));
        m.next();
        m.next();
        let r = m.next().unwrap();
        assert_eq!((r.path.to_str().unwrap(), r.line, r.col), ("b.rs", 2, 3));
        assert_eq!(m.next().unwrap().line, 0, "wraps to first");
        assert_eq!(m.prev().unwrap().path, PathBuf::from("b.rs"), "prev wraps to last");
        assert_eq!(m.prev().unwrap().line, 4);
    }

    #[test]
    fn fold_skips_hidden_and_one_per_line_collapses() {
        let mut m = model();
        m.toggle_fold(0);
        assert_eq!(m.next().unwrap().path, PathBuf::from("b.rs"));
        assert_eq!(m.next().unwrap().path, PathBuf::from("b.rs"));
        m.toggle_fold(0);
        m.set_one_per_line(true);
        assert_eq!(m.active().unwrap().visible(), vec![(0, 0), (0, 2), (1, 0)]);
        m.set_all_folded(true);
        assert!(m.next().is_none());
    }

    #[test]
    fn copy_is_path_line_text_with_one_based_line() {
        let m = model();
        assert_eq!(m.copy_entry(1, 0).unwrap(), "b.rs:3:abcx");
        assert_eq!(m.copy_text().lines().count(), 4);
        assert!(m.copy_text().starts_with("a.rs:1:ax\n"));
    }

    #[test]
    fn history_kept_and_clear() {
        let mut m = model();
        m.begin_search("y");
        m.add_file(&file("c.rs", vec![hit(1, 0, "y")]));
        assert_eq!(m.sets().len(), 2);
        assert_eq!(m.status(), "y - 1 hit(s) in 1 file(s)");
        m.select_set(0);
        assert_eq!(m.active().unwrap().title, "x");
        m.clear();
        assert_eq!(m.sets().len(), 1);
        assert_eq!(m.active().unwrap().title, "y");
        m.clear_all();
        assert!(m.active().is_none() && m.copy_text().is_empty() && m.next().is_none());
    }

    #[test]
    fn select_and_current() {
        let mut m = model();
        assert!(m.current().is_none());
        let r = m.select(1, 0).unwrap();
        assert_eq!(m.current().unwrap(), r);
        assert!(m.select(9, 9).is_none());
    }
}
