//! BK-043 - navigation history (Go Back / Go Forward) and last-edit ring.
//!
//! Pure state; the shell calls `record_jump` right before a caret jump
//! (go-to, search hit, bookmark jump, tab switch) and applies the position
//! returned by `back` / `forward`.

use std::collections::VecDeque;

/// Ring capacity (BK-043: capped at 100).
pub const CAP: usize = 100;

/// A navigable place: document key (path or tab id) + byte offset.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NavPos {
    pub doc: String,
    pub byte: usize,
}

impl NavPos {
    pub fn new(doc: impl Into<String>, byte: usize) -> Self {
        Self { doc: doc.into(), byte }
    }
}

/// The GUI keeps carets as 1-based (line, column); `NavPos::at` packs that pair into `byte`.
const COL_BITS: u32 = 24;

impl NavPos {
    /// A place given as 1-based line and column (what the shell's caret signal holds).
    pub fn at(doc: impl Into<String>, line: usize, col: usize) -> Self {
        Self { doc: doc.into(), byte: (line.max(1) << COL_BITS) | col.clamp(1, (1 << COL_BITS) - 1) }
    }
    /// Inverse of [`NavPos::at`].
    pub fn line_col(&self) -> (usize, usize) {
        (self.byte >> COL_BITS, self.byte & ((1 << COL_BITS) - 1))
    }
}

#[derive(Debug, Clone, Default)]
pub struct NavHistory {
    ring: VecDeque<NavPos>,
    /// Index of the entry we are "at" while walking back/forward; `ring.len()` = live position.
    at: usize,
    edits: VecDeque<NavPos>,
    edit_at: usize,
}

fn push_capped(q: &mut VecDeque<NavPos>, p: NavPos) {
    if q.back() != Some(&p) {
        q.push_back(p);
        if q.len() > CAP {
            q.pop_front();
        }
    }
}

impl NavHistory {
    pub fn len(&self) -> usize {
        self.ring.len()
    }
    pub fn is_empty(&self) -> bool {
        self.ring.is_empty()
    }

    /// Remember the position we are leaving. Drops the forward branch.
    pub fn record_jump(&mut self, from: NavPos) {
        self.ring.truncate(self.at.min(self.ring.len()));
        push_capped(&mut self.ring, from);
        self.at = self.ring.len();
    }

    /// Go Back from `current` (saved so Forward can return to it).
    pub fn back(&mut self, current: NavPos) -> Option<NavPos> {
        if self.at == 0 {
            return None;
        }
        if self.at == self.ring.len() {
            // leaving the live position: remember it as the forward target
            if self.ring.back() != Some(&current) {
                self.ring.push_back(current);
                if self.ring.len() > CAP {
                    self.ring.pop_front();
                    self.at -= 1;
                }
            }
            self.at = self.ring.len() - 1;
        }
        if self.at == 0 {
            return None;
        }
        self.at -= 1;
        self.ring.get(self.at).cloned()
    }

    /// Go Forward; `None` when already at the newest position.
    pub fn forward(&mut self) -> Option<NavPos> {
        if self.at + 1 >= self.ring.len() {
            return None;
        }
        self.at += 1;
        self.ring.get(self.at).cloned()
    }

    /// Record where an edit happened (consecutive same-doc edits keep only the latest).
    pub fn record_edit(&mut self, p: NavPos) {
        if self.edits.back().is_some_and(|l| l.doc == p.doc) {
            self.edits.pop_back();
        }
        push_capped(&mut self.edits, p);
        self.edit_at = self.edits.len();
    }

    /// Cycle backwards through the last-edit ring (wraps).
    pub fn last_edit(&mut self) -> Option<NavPos> {
        if self.edits.is_empty() {
            return None;
        }
        self.edit_at = if self.edit_at == 0 { self.edits.len() - 1 } else { self.edit_at - 1 };
        self.edits.get(self.edit_at).cloned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(b: usize) -> NavPos {
        NavPos::new("a.txt", b)
    }

    #[test]
    fn back_after_goto_returns_prior_caret_then_forward_returns() {
        let mut h = NavHistory::default();
        h.record_jump(p(5)); // jump from 5 ...
        assert_eq!(h.back(p(90)), Some(p(5))); // ... now at 90
        assert_eq!(h.back(p(5)), None);
        assert_eq!(h.forward(), Some(p(90)));
        assert_eq!(h.forward(), None);
    }

    #[test]
    fn line_col_round_trips() {
        assert_eq!(NavPos::at("a", 40, 7).line_col(), (40, 7));
        assert_ne!(NavPos::at("a", 40, 7), NavPos::at("a", 40, 8));
        assert_eq!(NavPos::at("a", 0, 0).line_col(), (1, 1));
    }

    #[test]
    fn walks_across_documents_and_multiple_steps() {
        let mut h = NavHistory::default();
        h.record_jump(NavPos::new("a", 1));
        h.record_jump(NavPos::new("b", 2));
        assert_eq!(h.back(NavPos::new("c", 3)), Some(NavPos::new("b", 2)));
        assert_eq!(h.back(NavPos::new("b", 2)), Some(NavPos::new("a", 1)));
        assert_eq!(h.forward(), Some(NavPos::new("b", 2)));
        assert_eq!(h.forward(), Some(NavPos::new("c", 3)));
    }

    #[test]
    fn new_jump_drops_forward_branch_and_ring_is_capped() {
        let mut h = NavHistory::default();
        h.record_jump(p(1));
        h.record_jump(p(2));
        h.back(p(3));
        h.record_jump(p(10));
        assert_eq!(h.forward(), None);
        let mut h = NavHistory::default();
        for i in 0..250 {
            h.record_jump(p(i));
        }
        assert_eq!(h.len(), CAP);
        assert_eq!(h.back(p(999)), Some(p(249)));
    }

    #[test]
    fn last_edit_ring_cycles() {
        let mut h = NavHistory::default();
        assert_eq!(h.last_edit(), None);
        h.record_edit(NavPos::new("a", 1));
        h.record_edit(NavPos::new("a", 2)); // same doc: replaces
        h.record_edit(NavPos::new("b", 7));
        assert_eq!(h.last_edit(), Some(NavPos::new("b", 7)));
        assert_eq!(h.last_edit(), Some(NavPos::new("a", 2)));
        assert_eq!(h.last_edit(), Some(NavPos::new("b", 7)), "wraps");
    }
}
