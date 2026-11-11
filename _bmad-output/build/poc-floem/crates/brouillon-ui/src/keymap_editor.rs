//! Shortcut editor view-model (BK-069): searchable id -> chords table,
//! conflict-checked assignment, per-command reset, TOML import/export.
//! Pure data; the Floem view binds to it.

use brouillon_core::keymap::{
    chord_to_string, chords_for, compose, parse_bindings_toml, parse_chord, serialize_bindings, Binding,
};

use crate::palette::fuzzy_score;

/// One table row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Row {
    pub id: String,
    pub caption: String,
    pub chords: Vec<String>,
    /// True when the command's chords differ from the defaults (reset is meaningful):
    /// the user bound, removed or lost one of them.
    pub customized: bool,
}

#[derive(Clone)]
pub struct KeymapEditorModel {
    /// (id, caption) of every registered command.
    commands: Vec<(String, String)>,
    /// Effective bindings below the user layer (spec defaults + preset overlay).
    base: Vec<Binding>,
    user: Vec<Binding>,
}

impl KeymapEditorModel {
    pub fn new(commands: Vec<(String, String)>, base: Vec<Binding>, user: Vec<Binding>) -> Self {
        Self { commands, base, user }
    }

    pub fn effective(&self) -> Vec<Binding> {
        compose(&[&self.base, &self.user])
    }

    pub fn user_bindings(&self) -> &[Binding] {
        &self.user
    }

    /// Caption of a command (its id when unknown).
    pub fn caption(&self, id: &str) -> String {
        self.commands.iter().find(|(i, _)| i == id).map_or_else(|| id.to_string(), |(_, c)| c.clone())
    }

    /// The command's effective chords differ from the defaults (preset included).
    pub fn customized(&self, id: &str) -> bool {
        let mut want = chords_for(&self.base, id);
        let mut have = chords_for(&self.effective(), id);
        want.sort();
        have.sort();
        want != have
    }

    /// Rows matching `filter` (fuzzy on caption/id), best first; all rows when empty.
    pub fn rows(&self, filter: &str) -> Vec<Row> {
        let eff = self.effective();
        let mut scored: Vec<(i32, Row)> = self
            .commands
            .iter()
            .filter_map(|(id, caption)| {
                let score = fuzzy_score(filter, caption).or_else(|| fuzzy_score(filter, id))?;
                Some((
                    score,
                    Row {
                        id: id.clone(),
                        caption: caption.clone(),
                        chords: chords_for(&eff, id),
                        customized: self.customized(id),
                    },
                ))
            })
            .collect();
        if !filter.is_empty() {
            scored.sort_by(|a, b| b.0.cmp(&a.0));
        }
        scored.into_iter().map(|(_, r)| r).collect()
    }

    /// Commands (other than `id`) already holding `chord` in an overlapping context.
    pub fn conflicts_for(&self, id: &str, chord: &str) -> Vec<String> {
        let Some(parsed) = parse_chord(chord) else { return Vec::new() };
        let mut out: Vec<String> = Vec::new();
        let eff = self.effective();
        for (i, b) in eff.iter().enumerate() {
            let shadowed = eff[i + 1..].iter().any(|n| n.chord == b.chord && n.when == b.when);
            if b.chord == parsed && b.command != id && !b.command.is_empty() && !shadowed && !out.contains(&b.command) {
                out.push(b.command.clone());
            }
        }
        out
    }

    /// Assign `chord` to `id`. `Err(conflicting ids)` when taken; use
    /// `assign_force` to take it over. `Err(vec![])` means the chord is invalid.
    pub fn assign(&mut self, id: &str, chord: &str) -> Result<(), Vec<String>> {
        if parse_chord(chord).is_none() {
            return Err(Vec::new());
        }
        let conflicts = self.conflicts_for(id, chord);
        if !conflicts.is_empty() {
            return Err(conflicts);
        }
        self.assign_force(id, chord);
        Ok(())
    }

    /// Bind regardless of conflicts: the user layer wins, so the previous owner loses the chord.
    pub fn assign_force(&mut self, id: &str, chord: &str) {
        if let Some(b) = Binding::new(chord, id, None) {
            self.user.retain(|u| !(u.chord == b.chord && u.when.is_none()));
            self.user.push(b);
        }
    }

    /// Back to the preset/default chords: drops the user bindings that target `id` and the ones
    /// that took or removed one of its default chords.
    pub fn reset(&mut self, id: &str) {
        let defaults: Vec<_> = self.base.iter().filter(|b| b.command == id).map(|b| b.chord.clone()).collect();
        self.user.retain(|b| b.command != id && !defaults.contains(&b.chord));
    }

    /// Leave `id` without any chord (its defaults are unbound, the rest dropped).
    pub fn clear(&mut self, id: &str) {
        self.reset(id);
        for chord in chords_for(&self.effective(), id) {
            self.unbind_chord(&chord);
        }
    }

    /// Remove a chord from whichever command owns it (user-layer unbind).
    pub fn unbind_chord(&mut self, chord: &str) {
        if let Some(b) = Binding::new(chord, "", None) {
            self.user.retain(|u| !(u.chord == b.chord && u.when.is_none()));
            self.user.push(b);
        }
    }

    pub fn export_toml(&self) -> String {
        serialize_bindings(&self.user)
    }

    /// Replace the user layer; returns parse warnings.
    pub fn import_toml(&mut self, text: &str) -> Vec<String> {
        let (b, warnings) = parse_bindings_toml(text);
        self.user = b;
        warnings
    }

    /// Canonical display of the user layer's chords (for tests/inspection).
    pub fn user_chords(&self) -> Vec<String> {
        self.user.iter().map(|b| chord_to_string(&b.chord)).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn model() -> KeymapEditorModel {
        let cmds = vec![
            ("file_save".to_string(), "File: Save".to_string()),
            ("file_open".to_string(), "File: Open".to_string()),
            ("find_open".to_string(), "Search: Find".to_string()),
        ];
        let base = vec![
            Binding::new("ctrl+s", "file_save", None).unwrap(),
            Binding::new("ctrl+o", "file_open", None).unwrap(),
        ];
        KeymapEditorModel::new(cmds, base, Vec::new())
    }

    #[test]
    fn rows_are_searchable() {
        let m = model();
        assert_eq!(m.rows("").len(), 3);
        let r = m.rows("fnd");
        assert_eq!(r[0].id, "find_open");
        assert_eq!(m.rows("save")[0].chords, vec!["ctrl+s"]);
    }

    #[test]
    fn assigning_a_taken_chord_returns_the_conflict_list() {
        let mut m = model();
        assert_eq!(m.assign("find_open", "ctrl+s"), Err(vec!["file_save".to_string()]));
        assert!(m.user_bindings().is_empty());
        assert_eq!(m.assign("find_open", "ctrl+f"), Ok(()));
        assert_eq!(m.rows("find")[0].chords, vec!["ctrl+f"]);
        assert!(m.assign("find_open", "").unwrap_err().is_empty());
    }

    #[test]
    fn force_takes_over_and_reset_restores() {
        let mut m = model();
        m.assign_force("find_open", "ctrl+s");
        assert!(m.rows("save")[0].chords.is_empty(), "previous owner lost the chord");
        assert!(m.rows("find")[0].customized);
        m.reset("find_open");
        assert_eq!(m.rows("save")[0].chords, vec!["ctrl+s"]);
        assert!(!m.rows("find")[0].customized);
    }

    #[test]
    fn clear_removes_every_chord_and_reset_brings_them_back() {
        let mut m = model();
        m.assign("file_save", "ctrl+alt+s").unwrap();
        assert_eq!(m.rows("save")[0].chords, vec!["ctrl+s", "ctrl+alt+s"]);
        m.clear("file_save");
        let r = &m.rows("save")[0];
        assert!(r.chords.is_empty() && r.customized, "{r:?}");
        // The freed chord can be given to another command without a conflict.
        assert_eq!(m.assign("find_open", "ctrl+s"), Ok(()));
        m.reset("file_save");
        let r = &m.rows("save")[0];
        assert_eq!(r.chords, vec!["ctrl+s"], "the default chord is back, taken from the other command");
        assert!(!r.customized);
        assert!(m.rows("find")[0].chords.is_empty());
        assert!(m.user_bindings().is_empty(), "{:?}", m.user_bindings());
    }

    #[test]
    fn replacing_a_conflict_takes_the_chord_and_the_victim_can_get_it_back() {
        let mut m = model();
        assert_eq!(m.conflicts_for("find_open", "ctrl+s"), vec!["file_save"]);
        assert_eq!(m.caption("file_save"), "File: Save");
        m.assign_force("find_open", "ctrl+s");
        assert!(m.rows("save")[0].customized && m.rows("save")[0].chords.is_empty());
        // The effective table has exactly one owner for the chord.
        assert!(m.conflicts_for("file_save", "ctrl+s") == vec!["find_open"]);
        m.reset("file_save");
        assert_eq!(m.rows("save")[0].chords, vec!["ctrl+s"]);
        assert!(m.rows("find")[0].chords.is_empty());
    }

    #[test]
    fn export_import_round_trip() {
        let mut m = model();
        m.assign("find_open", "ctrl+k ctrl+f").unwrap();
        m.unbind_chord("ctrl+o");
        let text = m.export_toml();
        let mut other = model();
        assert!(other.import_toml(&text).is_empty());
        assert_eq!(other.user_bindings(), m.user_bindings());
        assert!(other.rows("open").iter().find(|r| r.id == "file_open").unwrap().chords.is_empty());
    }
}
