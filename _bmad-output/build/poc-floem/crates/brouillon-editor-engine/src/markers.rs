//! BK-042 - bookmarks / markers (0-based line numbers) that survive edits.
//!
//! Pure state: `DocMeta::markers` (`Vec<usize>`) is the persisted form
//! (`Markers::from_lines` / `to_vec`). Consumers get a `LineSet` via `lines()`.

use std::collections::BTreeSet;

use brouillon_core::contract::{EditSet, LineSet};

use crate::cursor::line_starts;

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Markers(BTreeSet<usize>);

impl Markers {
    pub fn from_lines(lines: impl IntoIterator<Item = usize>) -> Self {
        Self(lines.into_iter().collect())
    }
    pub fn to_vec(&self) -> Vec<usize> {
        self.0.iter().copied().collect()
    }
    /// Sorted unique lines for Lot 3 `mark_all` / Lot 5 line filters.
    pub fn lines(&self) -> LineSet {
        LineSet(self.to_vec())
    }
    pub fn contains(&self, line: usize) -> bool {
        self.0.contains(&line)
    }
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// Toggle; returns true when the marker is now set.
    pub fn toggle(&mut self, line: usize) -> bool {
        if !self.0.remove(&line) {
            self.0.insert(line);
            return true;
        }
        false
    }
    pub fn clear(&mut self) {
        self.0.clear();
    }
    /// Invert over `0..line_count`.
    pub fn invert(&mut self, line_count: usize) {
        self.0 = (0..line_count).filter(|l| !self.0.contains(l)).collect();
    }

    /// Next marker strictly after `from`, wrapping to the first.
    pub fn next(&self, from: usize) -> Option<usize> {
        self.0.range(from + 1..).next().or_else(|| self.0.iter().next()).copied()
    }
    /// Previous marker strictly before `from`, wrapping to the last.
    pub fn prev(&self, from: usize) -> Option<usize> {
        self.0.range(..from).next_back().or_else(|| self.0.iter().next_back()).copied()
    }

    /// Re-anchor through an `EditSet` expressed against `old_text`: markers
    /// below an edit shift by its net newline count; a marker whose line break
    /// is deleted (its line merges into the previous one) is removed.
    pub fn apply_edits(&mut self, old_text: &str, edits: &EditSet) {
        if edits.is_empty() || self.0.is_empty() {
            return;
        }
        let starts = line_starts(old_text);
        let nl = |s: &str| s.bytes().filter(|b| *b == b'\n').count() as isize;
        let deltas: Vec<(usize, usize, isize)> = edits
            .0
            .iter()
            .map(|e| (e.start_byte, e.end_byte, nl(&e.text) - nl(&old_text[e.start_byte..e.end_byte])))
            .collect();
        self.0 = self
            .0
            .iter()
            .filter_map(|&l| {
                let s = *starts.get(l)?;
                if l > 0 && deltas.iter().any(|&(a, b, _)| a < s && b >= s) {
                    return None; // line break before this line was deleted
                }
                let shift: isize = deltas.iter().filter(|&&(_, b, _)| b <= s).map(|d| d.2).sum();
                Some((l as isize + shift).max(0) as usize)
            })
            .collect();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use brouillon_core::contract::TextEdit;

    #[test]
    fn toggle_next_prev_wrap() {
        let mut m = Markers::default();
        assert!(m.toggle(5));
        m.toggle(2);
        m.toggle(9);
        assert_eq!(m.next(5), Some(9));
        assert_eq!(m.next(9), Some(2), "wraps");
        assert_eq!(m.prev(5), Some(2));
        assert_eq!(m.prev(2), Some(9), "wraps");
        assert!(!m.toggle(5));
        assert_eq!(m.lines(), LineSet(vec![2, 9]));
        assert_eq!(Markers::default().next(0), None);
    }

    #[test]
    fn clear_and_invert() {
        let mut m = Markers::from_lines([1, 3]);
        m.invert(5);
        assert_eq!(m.to_vec(), vec![0, 2, 4]);
        m.clear();
        assert!(m.is_empty());
    }

    #[test]
    fn marker_moves_when_lines_inserted_above() {
        let text = "a\nb\nc\nd";
        let mut m = Markers::from_lines([2]);
        let e = EditSet(vec![TextEdit::insert(0, "x\ny\n")]); // k = 2 lines above
        m.apply_edits(text, &e);
        assert_eq!(m.to_vec(), vec![4]);
        // edit inside the marker line or below does not move it
        let mut m = Markers::from_lines([1]);
        m.apply_edits(text, &EditSet(vec![TextEdit::insert(3, "zz\n"), TextEdit::insert(7, "!")]));
        assert_eq!(m.to_vec(), vec![1]);
    }

    #[test]
    fn marker_removed_when_its_line_is_deleted_and_moves_up_after_deletion() {
        let text = "a\nb\nc\nd";
        let mut m = Markers::from_lines([1, 3]);
        m.apply_edits(text, &EditSet(vec![TextEdit::delete(1, 3)])); // removes "\nb" -> line 1 merged away
        assert_eq!(m.to_vec(), vec![2], "line 3 becomes 2, deleted line 1 is dropped");
    }
}
