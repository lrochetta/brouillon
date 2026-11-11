//! Cliptext panel view-model (BK-055): library selector, entry list, insert by
//! double-click or quick chord (Ctrl+Alt+1..0), add/modify/remove/sort, and the
//! per-file-type default library. The floem view binds to this model; the model
//! itself is pure so it can be tested without a window.

use brouillon_core::contract::{CmdCtx, CmdOutcome};
use brouillon_core::filetype::FileProfile;
use brouillon_editor_engine::cliptext::{expand, today, Clip, Library};

#[derive(Debug, Clone, Default)]
pub struct CliptextPanel {
    /// (library id such as the file stem, library). Order = combo order.
    libs: Vec<(String, Library)>,
    selected: usize,
}

impl CliptextPanel {
    pub fn new(libs: Vec<(String, Library)>) -> Self {
        Self { libs, selected: 0 }
    }

    /// Combo-box labels (library title, or its id when untitled).
    pub fn library_names(&self) -> Vec<String> {
        self.libs.iter().map(|(id, l)| if l.title.is_empty() { id.clone() } else { l.title.clone() }).collect()
    }

    pub fn selected(&self) -> usize {
        self.selected
    }

    pub fn select_library(&mut self, index: usize) -> bool {
        let ok = index < self.libs.len();
        if ok {
            self.selected = index;
        }
        ok
    }

    /// Select the library named by the profile's `cliptext_library` (id or title).
    pub fn select_for_profile(&mut self, profile: &FileProfile) -> bool {
        let Some(want) = profile.cliptext_library.as_deref() else { return false };
        match self.libs.iter().position(|(id, l)| id.eq_ignore_ascii_case(want) || l.title.eq_ignore_ascii_case(want)) {
            Some(i) => self.select_library(i),
            None => false,
        }
    }

    pub fn library(&self) -> Option<&Library> {
        self.libs.get(self.selected).map(|(_, l)| l)
    }

    /// List rows; the first 10 carry their quick-insert digit (`1`..`9`, `0`).
    pub fn rows(&self) -> Vec<String> {
        self.library()
            .map(|l| {
                l.entries
                    .iter()
                    .enumerate()
                    .map(|(i, c)| if i < 10 { format!("{} {}", (i + 1) % 10, c.title) } else { c.title.clone() })
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Double-click / Enter on row `index`.
    pub fn insert(&self, index: usize, ctx: &CmdCtx) -> Option<CmdOutcome> {
        let clip = self.library()?.entries.get(index)?;
        Some(expand(clip, ctx, &today()))
    }

    /// Chord such as `ctrl+alt+3` -> entry index (`0` = 10th entry).
    pub fn chord_index(chord: &str) -> Option<usize> {
        let key = chord.to_lowercase().strip_prefix("ctrl+alt+")?.to_string();
        let d = key.chars().next().filter(|_| key.len() == 1)?.to_digit(10)? as usize;
        Some(if d == 0 { 9 } else { d - 1 })
    }

    /// Quick insert by chord on the selected library.
    pub fn quick_insert(&self, chord: &str, ctx: &CmdCtx) -> Option<CmdOutcome> {
        self.insert(Self::chord_index(chord)?, ctx)
    }

    pub fn add(&mut self, clip: Clip) {
        if let Some((_, l)) = self.libs.get_mut(self.selected) {
            l.add(clip);
        }
    }
    pub fn modify(&mut self, index: usize, clip: Clip) -> bool {
        self.libs.get_mut(self.selected).is_some_and(|(_, l)| l.modify(index, clip))
    }
    pub fn remove(&mut self, index: usize) -> Option<Clip> {
        self.libs.get_mut(self.selected)?.1.remove(index)
    }
    pub fn sort(&mut self) {
        if let Some((_, l)) = self.libs.get_mut(self.selected) {
            l.sort();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use brouillon_core::contract::Region;

    fn lib(title: &str, titles: &[&str]) -> Library {
        Library {
            title: title.into(),
            entries: titles.iter().map(|t| Clip { title: t.to_string(), body: format!("<{t}>^!</{t}>") }).collect(),
            ..Default::default()
        }
    }

    fn panel() -> CliptextPanel {
        CliptextPanel::new(vec![
            ("html".into(), lib("HTML", &["a", "b", "c"])),
            ("css".into(), lib("", &["x"])),
        ])
    }

    #[test]
    fn select_library_and_rows() {
        let mut p = panel();
        assert_eq!(p.library_names(), vec!["HTML", "css"]);
        assert_eq!(p.rows(), vec!["1 a", "2 b", "3 c"]);
        assert!(p.select_library(1) && !p.select_library(5));
        assert_eq!(p.rows(), vec!["1 x"]);
    }

    #[test]
    fn ctrl_alt_3_inserts_third_entry() {
        let p = panel();
        let sel = [Region::caret(0)];
        let ctx = CmdCtx { text: "", selection: &sel, ..Default::default() };
        let o = p.quick_insert("Ctrl+Alt+3", &ctx).unwrap();
        assert_eq!(o.edits.apply_to_str("").unwrap(), "<c></c>");
        assert_eq!(CliptextPanel::chord_index("ctrl+alt+0"), Some(9));
        assert_eq!(CliptextPanel::chord_index("ctrl+3"), None);
        assert!(p.quick_insert("ctrl+alt+9", &ctx).is_none());
    }

    #[test]
    fn per_file_type_default_library() {
        let mut p = panel();
        let prof = FileProfile { cliptext_library: Some("CSS".into()), ..Default::default() };
        assert!(p.select_for_profile(&prof));
        assert_eq!(p.selected(), 1);
        assert!(!p.select_for_profile(&FileProfile::default()));
    }

    #[test]
    fn edit_operations() {
        let mut p = panel();
        p.add(Clip { title: "0first".into(), body: String::new() });
        p.sort();
        assert_eq!(p.library().unwrap().entries[0].title, "0first");
        assert!(p.modify(0, Clip { title: "zz".into(), body: String::new() }));
        assert_eq!(p.remove(0).unwrap().title, "zz");
        assert_eq!(p.rows().len(), 3);
    }
}
