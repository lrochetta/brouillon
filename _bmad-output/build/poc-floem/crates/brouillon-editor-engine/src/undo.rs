#![allow(dead_code)]

//! Grouped multi-level undo / redo (Lot 1, BK-001/002/003).
//!
//! The history is a bounded stack of [`UndoGroup`]s. A group is the unit the
//! user sees: one undo step reverts every [`UndoEntry`] it holds. Entries are
//! REVERSE patches, stored in the order the forward ops were recorded; undoing
//! a group applies them last-to-first, and the inverses collected that way are
//! the entries of the matching redo group (same convention, so redo and undo
//! are symmetric).
//!
//! Each group carries
//!   - an [`Origin`] tag (`User` / `Ai(name)`) so a future MCP server can undo
//!     or list "AI edits" separately;
//!   - the selection before / after, restored by undo / redo;
//!   - a monotonically increasing `id`, used by the save marker.
//!
//! Save marker: [`UndoHistory::mark_saved`] remembers the id of the top group.
//! History is NOT cleared on save, so "undo to last save" works afterwards.
//!
//! Limits: at most `capacity` groups and `byte_cap` bytes of stored text per
//! history (default 1000 groups / 64 MB); the oldest groups are dropped first.

use std::collections::VecDeque;
use std::sync::Arc;

use brouillon_core::contract::Region;

/// A reverse-op record. To UNDO an `insert(pos, text)` we apply
/// `remove(pos..pos+text.len())`. To UNDO a `remove(range)`, we apply
/// `insert(range.start, removed_text)`. To UNDO a `replace(range, new)`,
/// we apply `replace(range.start..range.start+new.len(), old_text)`.
#[derive(Debug, Clone)]
pub enum UndoEntry {
    /// Reverse-op : remove this byte range (to undo an insert).
    RemoveRange { byte_range: std::ops::Range<usize> },
    /// Reverse-op : insert this text at this byte position (to undo a remove).
    InsertAt { byte_pos: usize, text: String },
    /// Reverse-op : replace this byte range with this text (to undo a replace).
    ReplaceRange { byte_range: std::ops::Range<usize>, text: String },
    /// Reverse-op of a lazy-store buffer: the same change as piece operations (no text copied). Applying it
    /// removes `inserted` and puts `removed` back at `pos`; its inverse is the swapped record.
    Pieces(Box<brouillon_textstore_spike::lazy::EditRec>),
}

impl UndoEntry {
    fn bytes(&self) -> usize {
        std::mem::size_of::<Self>()
            + match self {
                Self::RemoveRange { .. } => 0,
                Self::InsertAt { text, .. } | Self::ReplaceRange { text, .. } => text.len(),
                // the record itself plus the add-buffer bytes it keeps alive
                Self::Pieces(rec) => rec.heap_bytes() + rec.add_bytes(),
            }
    }
}

/// Who produced an undo group.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum Origin {
    #[default]
    User,
    /// An AI client (name of the agent / MCP client).
    Ai(String),
}

/// One user-visible undo step.
#[derive(Debug, Clone)]
pub struct UndoGroup {
    pub id: u64,
    pub origin: Origin,
    /// Reverse ops in recording order (applied last-to-first).
    pub entries: Vec<UndoEntry>,
    /// Restored by undo.
    pub selection_before: Option<Vec<Region>>,
    /// Restored by redo.
    pub selection_after: Option<Vec<Region>>,
}

impl UndoGroup {
    fn bytes(&self) -> usize {
        self.entries.iter().map(UndoEntry::bytes).sum()
    }
}

/// Kind of typing run currently being coalesced (BK-002).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TypingKind {
    Insert,
    /// Backspace: each delete ends where the previous one started.
    DeleteBack,
    /// Delete key: each delete starts where the previous one started.
    DeleteFwd,
}

#[derive(Debug, Clone, Copy)]
struct Typing {
    kind: TypingKind,
    last_ms: u64,
    /// Where the next merged op must happen (end of the inserted text, or the
    /// start of the last deleted range).
    pos: usize,
    /// True when the last inserted char was a word char.
    last_word: bool,
}

/// A pause longer than this closes the typing group.
pub const TYPING_PAUSE_MS: u64 = 1000;
/// Default per-file memory bound.
pub const DEFAULT_BYTE_CAP: usize = 64 * 1024 * 1024;

fn is_word(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

/// Linear history with redo stack. Bounded.
#[derive(Debug, Clone)]
pub struct UndoHistory {
    // Groups are shared (`Arc`): `Buffer` is cloned on every edit, and a deep copy of up to 64 MB of undo text
    // per keystroke (and per old buffer a journal or a pending save still holds) was the largest amplification.
    undo_stack: VecDeque<Arc<UndoGroup>>,
    redo_stack: VecDeque<Arc<UndoGroup>>,
    capacity: usize,
    byte_cap: usize,
    undo_bytes: usize,
    next_id: u64,
    /// Id of the top undo group when the document was last saved (0 = empty
    /// history). `None` = the saved state is no longer reachable.
    saved: Option<u64>,
    typing: Option<Typing>,
}

impl UndoHistory {
    /// Default 1000-group / 64 MB capacity. A fresh history is "saved" (the
    /// loaded text is the on-disk text).
    pub fn new() -> Self {
        Self::with_capacity(1000)
    }

    pub fn with_capacity(capacity: usize) -> Self {
        Self {
            undo_stack: VecDeque::new(),
            redo_stack: VecDeque::new(),
            capacity,
            byte_cap: DEFAULT_BYTE_CAP,
            undo_bytes: 0,
            next_id: 1,
            saved: Some(0),
            typing: None,
        }
    }

    /// Change the limits (oldest groups are dropped immediately if exceeded).
    pub fn set_limits(&mut self, capacity: usize, byte_cap: usize) {
        self.capacity = capacity;
        self.byte_cap = byte_cap;
        self.enforce_limits();
    }

    /// Bytes of undo text currently held (for a global budget across files).
    pub fn bytes(&self) -> usize {
        self.undo_bytes
    }

    /// Show every piece that a `Pieces` entry of the undo AND redo stacks refers to (mutably: add-buffer
    /// compaction remaps them). Cheap when the history holds no piece entries.
    pub fn visit_pieces_mut(&mut self, f: &mut dyn FnMut(&mut brouillon_textstore_spike::lazy::Piece)) {
        for g in self.undo_stack.iter_mut().chain(self.redo_stack.iter_mut()) {
            if !g.entries.iter().any(|e| matches!(e, UndoEntry::Pieces(_))) {
                continue;
            }
            // groups are shared (Arc) between clones of the buffer: copy-on-write before remapping
            for e in &mut Arc::make_mut(g).entries {
                if let UndoEntry::Pieces(rec) = e {
                    rec.pieces_mut().for_each(&mut *f);
                }
            }
        }
    }

    /// Read-only variant of [`Self::visit_pieces_mut`] (base-generation bookkeeping).
    pub fn visit_pieces(&self, f: &mut dyn FnMut(&brouillon_textstore_spike::lazy::Piece)) {
        for g in self.undo_stack.iter().chain(self.redo_stack.iter()) {
            for e in &g.entries {
                if let UndoEntry::Pieces(rec) = e {
                    rec.pieces().for_each(&mut *f);
                }
            }
        }
    }

    /// Drop the oldest undo group; returns false when there is none.
    pub fn drop_oldest(&mut self) -> bool {
        let Some(g) = self.undo_stack.pop_front() else {
            return false;
        };
        self.undo_bytes -= g.bytes();
        // The saved state is the state AFTER group `saved`; once that group
        // (or the empty base, id 0) falls off, it can't be reached any more.
        if self.saved == Some(g.id) || self.saved == Some(0) {
            self.saved = None;
        }
        true
    }

    fn enforce_limits(&mut self) {
        // Always keep the newest group, even if it alone exceeds the byte cap.
        while self.undo_stack.len() > self.capacity
            || (self.undo_bytes > self.byte_cap && self.undo_stack.len() > 1)
        {
            if !self.drop_oldest() {
                break;
            }
        }
        while self.redo_stack.len() > self.capacity {
            self.redo_stack.pop_front();
        }
    }

    fn alloc_id(&mut self) -> u64 {
        let id = self.next_id;
        self.next_id += 1;
        id
    }

    /// Push a new group. Truncates redo (a new edit invalidates it) and
    /// closes any typing run. Returns the group id.
    pub fn push_group(
        &mut self,
        origin: Origin,
        entries: Vec<UndoEntry>,
        selection_before: Option<Vec<Region>>,
        selection_after: Option<Vec<Region>>,
    ) -> u64 {
        self.redo_stack.clear();
        self.typing = None;
        let id = self.alloc_id();
        let g = UndoGroup { id, origin, entries, selection_before, selection_after };
        self.undo_bytes += g.bytes();
        self.undo_stack.push_back(Arc::new(g));
        self.enforce_limits();
        id
    }

    /// Convenience: push a single-entry user group.
    pub fn push_undo(&mut self, entry: UndoEntry) -> u64 {
        self.push_group(Origin::User, vec![entry], None, None)
    }

    /// Append an entry to the top group (typing coalescing). Falls back to a
    /// new group when the history is empty. Does not touch redo (it is empty
    /// anyway while a typing run is live) nor the typing state.
    pub fn extend_top(&mut self, entry: UndoEntry, selection_after: Option<Vec<Region>>) {
        let Some(top) = self.undo_stack.back_mut() else {
            let keep = self.typing;
            self.push_group(Origin::User, vec![entry], None, selection_after);
            self.typing = keep;
            return;
        };
        self.undo_bytes += entry.bytes();
        let top = Arc::make_mut(top); // copies only this one group, and only when an older buffer still shares it
        top.entries.push(entry);
        top.selection_after = selection_after;
        self.enforce_limits();
    }

    /// Push onto undo WITHOUT clearing the redo stack (used by redo).
    pub fn push_undo_no_redo_clear(&mut self, group: UndoGroup) {
        self.undo_bytes += group.bytes();
        self.undo_stack.push_back(Arc::new(group));
        self.enforce_limits();
    }

    /// Pop the top undo group. The caller applies it and pushes the inverse
    /// onto redo via `push_redo`.
    pub fn pop_undo(&mut self) -> Option<UndoGroup> {
        let g = self.undo_stack.pop_back()?;
        self.undo_bytes -= g.bytes();
        self.typing = None;
        Some(Arc::try_unwrap(g).unwrap_or_else(|shared| (*shared).clone()))
    }

    pub fn pop_redo(&mut self) -> Option<UndoGroup> {
        self.typing = None;
        self.redo_stack.pop_back().map(|g| Arc::try_unwrap(g).unwrap_or_else(|shared| (*shared).clone()))
    }

    pub fn push_redo(&mut self, group: UndoGroup) {
        self.redo_stack.push_back(Arc::new(group));
        self.enforce_limits();
    }

    pub fn can_undo(&self) -> bool {
        !self.undo_stack.is_empty()
    }
    pub fn can_redo(&self) -> bool {
        !self.redo_stack.is_empty()
    }
    /// Number of undo groups.
    pub fn undo_depth(&self) -> usize {
        self.undo_stack.len()
    }
    pub fn redo_depth(&self) -> usize {
        self.redo_stack.len()
    }

    /// Origin of the group the next undo would revert.
    pub fn top_origin(&self) -> Option<&Origin> {
        self.undo_stack.back().map(|g| &g.origin)
    }

    /// Id of the top undo group (0 when empty).
    pub fn top_id(&self) -> u64 {
        self.undo_stack.back().map_or(0, |g| g.id)
    }

    /// Clear both stacks; the save marker is lost (caller re-marks if clean).
    pub fn clear(&mut self) {
        self.undo_stack.clear();
        self.redo_stack.clear();
        self.undo_bytes = 0;
        self.typing = None;
        self.saved = None;
    }

    // ---------- save marker (BK-003) ----------

    /// Remember the current state as the saved one. Keeps the history.
    pub fn mark_saved(&mut self) {
        self.typing = None; // a later keystroke must not merge into the saved group
        self.saved = Some(self.top_id());
    }

    pub fn is_at_save(&self) -> bool {
        self.saved == Some(self.top_id())
    }

    /// Direction to travel to reach the saved state: `Some(true)` = undo,
    /// `Some(false)` = redo, `None` = already there or unreachable.
    pub fn save_direction(&self) -> Option<bool> {
        let id = self.saved?;
        if id == self.top_id() {
            return None;
        }
        if id == 0 || self.undo_stack.iter().any(|g| g.id == id) {
            Some(true)
        } else if self.redo_stack.iter().any(|g| g.id == id) {
            Some(false)
        } else {
            None
        }
    }

    // ---------- typing coalescing (BK-002) ----------

    /// Close the current typing run (caret jump, non-typing command, ...).
    pub fn break_typing(&mut self) {
        self.typing = None;
    }

    /// Can an op of this shape join the live typing run?
    pub(crate) fn typing_mergeable(
        &self,
        kind: TypingKind,
        pos: usize,
        first_char: Option<char>,
        now_ms: u64,
    ) -> bool {
        let Some(t) = self.typing else { return false };
        if !self.can_undo() || t.kind != kind || t.pos != pos {
            return false;
        }
        if now_ms.saturating_sub(t.last_ms) > TYPING_PAUSE_MS {
            return false;
        }
        // Word boundary: a word char after punctuation / space starts a new run.
        if kind == TypingKind::Insert {
            if let Some(c) = first_char {
                if !t.last_word && is_word(c) {
                    return false;
                }
            }
        }
        true
    }

    pub(crate) fn set_typing(&mut self, kind: TypingKind, pos: usize, last: Option<char>, now_ms: u64) {
        self.typing = Some(Typing { kind, last_ms: now_ms, pos, last_word: last.map_or(false, is_word) });
    }
}

impl Default for UndoHistory {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ins(pos: usize, s: &str) -> UndoEntry {
        UndoEntry::InsertAt { byte_pos: pos, text: s.to_string() }
    }
    fn rm(start: usize, end: usize) -> UndoEntry {
        UndoEntry::RemoveRange { byte_range: start..end }
    }
    fn group(h: &mut UndoHistory, e: UndoEntry) -> u64 {
        h.push_undo(e)
    }

    #[test]
    fn new_history_is_empty_and_saved() {
        let h = UndoHistory::new();
        assert!(!h.can_undo() && !h.can_redo());
        assert_eq!((h.undo_depth(), h.redo_depth()), (0, 0));
        assert!(h.is_at_save());
    }

    #[test]
    fn push_pop_roundtrip() {
        let mut h = UndoHistory::new();
        group(&mut h, rm(0, 5));
        let g = h.pop_undo().unwrap();
        assert!(matches!(&g.entries[0], UndoEntry::RemoveRange { byte_range } if *byte_range == (0..5)));
        assert!(!h.can_undo());
        assert!(h.pop_undo().is_none() && h.pop_redo().is_none());
    }

    #[test]
    fn push_group_truncates_redo() {
        let mut h = UndoHistory::new();
        let id = group(&mut h, rm(0, 1));
        let mut g = h.pop_undo().unwrap();
        g.entries = vec![ins(0, "x")];
        h.push_redo(g);
        assert!(h.can_redo());
        assert_ne!(id, 0);
        group(&mut h, rm(0, 1));
        assert!(!h.can_redo());
    }

    #[test]
    fn capacity_ring_drops_oldest_group() {
        let mut h = UndoHistory::with_capacity(3);
        for (i, s) in ["a", "b", "c", "d"].iter().enumerate() {
            group(&mut h, ins(i, s));
        }
        assert_eq!(h.undo_depth(), 3);
        let labels: Vec<String> = (0..3)
            .map(|_| match h.pop_undo().unwrap().entries.remove(0) {
                UndoEntry::InsertAt { text, .. } => text,
                _ => panic!(),
            })
            .collect();
        assert_eq!(labels, ["d", "c", "b"]);
    }

    #[test]
    fn capacity_zero_drops_everything() {
        let mut h = UndoHistory::with_capacity(0);
        group(&mut h, ins(0, "a"));
        assert!(!h.can_undo());
    }

    #[test]
    fn byte_cap_drops_oldest_but_keeps_newest() {
        let mut h = UndoHistory::new();
        h.set_limits(1000, 500);
        for i in 0..10 {
            group(&mut h, ins(i, &"x".repeat(200)));
        }
        assert!(h.bytes() <= 500 + 300, "bytes {}", h.bytes());
        assert!(h.undo_depth() >= 1 && h.undo_depth() < 10);
        assert!(h.can_undo());
        let mut n = 0;
        while h.pop_undo().is_some() {
            n += 1;
        }
        assert!(n >= 1);
        assert_eq!(h.bytes(), 0, "accounting returns to zero");
    }

    #[test]
    fn save_marker_survives_and_directs() {
        let mut h = UndoHistory::new();
        group(&mut h, rm(0, 1));
        h.mark_saved();
        assert!(h.is_at_save());
        group(&mut h, rm(0, 1));
        group(&mut h, rm(0, 1));
        assert!(!h.is_at_save());
        assert_eq!(h.save_direction(), Some(true));
        h.pop_undo();
        h.pop_undo();
        assert!(h.is_at_save());
        h.pop_undo(); // past the save point
        assert_eq!(h.save_direction(), None); // groups in redo not pushed in this unit test
    }

    #[test]
    fn dropping_saved_group_makes_marker_unreachable() {
        let mut h = UndoHistory::with_capacity(2);
        group(&mut h, rm(0, 1));
        h.mark_saved();
        group(&mut h, rm(0, 1));
        group(&mut h, rm(0, 1)); // drops the saved group
        assert_eq!(h.save_direction(), None);
        assert!(!h.is_at_save());
    }
    #[test]
    fn a_cloned_history_shares_its_groups_and_stays_independent() {
        let mut a = UndoHistory::new();
        group(&mut a, ins(0, "first group"));
        group(&mut a, ins(3, "second"));
        let mut b = a.clone();
        // Both histories point at the same group allocations (no deep copy of the undo text).
        assert!(a.undo_stack.iter().zip(b.undo_stack.iter()).all(|(x, y)| Arc::ptr_eq(x, y)));
        // Extending the top group of the clone copies that group only; the original is untouched.
        b.extend_top(ins(9, "more"), None);
        assert!(Arc::ptr_eq(&a.undo_stack[0], &b.undo_stack[0]), "older groups stay shared");
        assert!(!Arc::ptr_eq(&a.undo_stack[1], &b.undo_stack[1]));
        assert_eq!(a.undo_stack[1].entries.len(), 1);
        assert_eq!(b.undo_stack[1].entries.len(), 2);
        // Popping from the clone leaves the original's stack alone.
        let g = b.pop_undo().unwrap();
        assert_eq!(g.entries.len(), 2);
        assert_eq!(a.undo_depth(), 2);
        assert_eq!(b.undo_depth(), 1);
        // Popping the only owner moves the group out without a copy; redo keeps working.
        b.push_redo(g);
        assert_eq!(b.pop_redo().unwrap().entries.len(), 2);
    }
}
