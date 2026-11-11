//! Pure-edit contract shared by all lots (Step 0).
//!
//! Lots 3/5/6 compute edits and NEVER mutate a `Buffer`; Lot 1's
//! `Buffer::apply_edits` applies an `EditSet` as one undo group.
//! Lives in `brouillon-core` (not the engine) so `command_registry` (Lot 10)
//! can depend on `CommandSpec`; `brouillon_editor_engine::contract` re-exports it.

use std::fmt;
use std::path::Path;

/// Replace `start_byte..end_byte` (UTF-8 byte offsets) with `text`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TextEdit {
    pub start_byte: usize,
    pub end_byte: usize,
    pub text: String,
}

impl TextEdit {
    pub fn new(start_byte: usize, end_byte: usize, text: impl Into<String>) -> Self {
        Self { start_byte, end_byte, text: text.into() }
    }
    pub fn insert(at: usize, text: impl Into<String>) -> Self {
        Self::new(at, at, text)
    }
    pub fn delete(start_byte: usize, end_byte: usize) -> Self {
        Self::new(start_byte, end_byte, "")
    }
}

/// Why an `EditSet` could not be built or applied.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EditError {
    /// `start_byte > end_byte`.
    Inverted { index: usize },
    /// Two edits overlap (touching ranges are allowed).
    Overlap { index: usize },
    /// An offset is past the end of the text or not on a char boundary.
    OutOfRange { index: usize },
}

impl fmt::Display for EditError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Inverted { index } => write!(f, "edit {index}: start after end"),
            Self::Overlap { index } => write!(f, "edit {index} overlaps the previous edit"),
            Self::OutOfRange { index } => write!(f, "edit {index}: offset out of range or not on a char boundary"),
        }
    }
}
impl std::error::Error for EditError {}

/// Non-overlapping edits in ascending order, all expressed against the SAME
/// original text (offsets are not shifted by earlier edits).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct EditSet(pub Vec<TextEdit>);

impl EditSet {
    pub fn new() -> Self {
        Self(Vec::new())
    }

    /// Sort (stable) by start and validate: no inverted or overlapping edits.
    pub fn from_unsorted(mut edits: Vec<TextEdit>) -> Result<Self, EditError> {
        edits.sort_by_key(|e| (e.start_byte, e.end_byte));
        let set = Self(edits);
        set.validate()?;
        Ok(set)
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
    pub fn len(&self) -> usize {
        self.0.len()
    }

    /// Check the ascending / non-overlapping invariant.
    pub fn validate(&self) -> Result<(), EditError> {
        let mut prev_end = 0usize;
        for (i, e) in self.0.iter().enumerate() {
            if e.start_byte > e.end_byte {
                return Err(EditError::Inverted { index: i });
            }
            if e.start_byte < prev_end {
                return Err(EditError::Overlap { index: i });
            }
            prev_end = e.end_byte;
        }
        Ok(())
    }

    /// Pure application to a string (reference semantics for tests; Lot 1's
    /// `Buffer::apply_edits` must agree with this).
    pub fn apply_to_str(&self, src: &str) -> Result<String, EditError> {
        self.validate()?;
        let mut out = String::with_capacity(src.len());
        let mut pos = 0usize;
        for (i, e) in self.0.iter().enumerate() {
            if e.end_byte > src.len() || !src.is_char_boundary(e.start_byte) || !src.is_char_boundary(e.end_byte) {
                return Err(EditError::OutOfRange { index: i });
            }
            out.push_str(&src[pos..e.start_byte]);
            out.push_str(&e.text);
            pos = e.end_byte;
        }
        out.push_str(&src[pos..]);
        Ok(out)
    }
}

/// One selection region (byte offsets). `anchor == head` is a bare caret.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Region {
    pub anchor: usize,
    pub head: usize,
}

impl Region {
    pub fn caret(at: usize) -> Self {
        Self { anchor: at, head: at }
    }
    pub fn is_caret(&self) -> bool {
        self.anchor == self.head
    }
    pub fn start(&self) -> usize {
        self.anchor.min(self.head)
    }
    pub fn end(&self) -> usize {
        self.anchor.max(self.head)
    }
}

/// Sorted, unique 0-based line numbers (markers, "mark all hits", filters).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct LineSet(pub Vec<usize>);

impl LineSet {
    pub fn from_unsorted(mut lines: Vec<usize>) -> Self {
        lines.sort_unstable();
        lines.dedup();
        Self(lines)
    }
    pub fn contains(&self, line: usize) -> bool {
        self.0.binary_search(&line).is_ok()
    }
}

/// Identifier of a UI panel a command asks the shell to open.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct PanelId(pub String);

/// Result of a command: pure data, applied by the shell.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct CmdOutcome {
    pub edits: EditSet,
    /// New selection after applying `edits` (None = let the shell map it).
    pub selection: Option<Vec<Region>>,
    /// Status-bar / toast message.
    pub message: Option<String>,
    pub open_panel: Option<PanelId>,
}

/// Declarative description of a command (registry, palette, keymap, menus).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct CommandSpec {
    /// Stable snake_case id.
    pub id: String,
    /// "Category: Action" caption.
    pub caption: String,
    pub category: String,
    /// e.g. "ctrl+k ctrl+b".
    pub default_chord: Option<String>,
    /// Context expression, e.g. "selection_empty".
    pub when: Option<String>,
    /// JSON Schema (as JSON text) of the command arguments, for the future
    /// MCP server. `None` = takes no structured arguments.
    pub args_schema: Option<String>,
    /// `true` when the command can modify document text or files (MCP
    /// clients must ask before calling it). Defaults to `false`.
    pub mutates: bool,
}

/// Read-only input handed to `run`. Lots read this, never the live editor.
#[derive(Debug, Clone, Default)]
pub struct CmdCtx<'a> {
    pub text: &'a str,
    pub selection: &'a [Region],
    pub path: Option<&'a Path>,
    pub args: &'a [String],
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn apply_in_original_coordinates() {
        let set = EditSet::from_unsorted(vec![TextEdit::new(6, 11, "there"), TextEdit::insert(0, ">> ")]).unwrap();
        assert_eq!(set.apply_to_str("hello world").unwrap(), ">> hello there");
    }

    #[test]
    fn overlap_and_inverted_rejected() {
        assert_eq!(
            EditSet::from_unsorted(vec![TextEdit::new(0, 5, "a"), TextEdit::new(3, 6, "b")]),
            Err(EditError::Overlap { index: 1 })
        );
        assert!(EditSet(vec![TextEdit::new(4, 2, "")]).validate().is_err());
        // touching ranges are fine
        assert!(EditSet::from_unsorted(vec![TextEdit::new(0, 2, "x"), TextEdit::new(2, 4, "y")]).is_ok());
    }

    #[test]
    fn out_of_range_and_char_boundary() {
        assert!(EditSet(vec![TextEdit::delete(0, 99)]).apply_to_str("abc").is_err());
        assert!(EditSet(vec![TextEdit::delete(0, 1)]).apply_to_str("é").is_err());
    }

    #[test]
    fn lineset_sorted_unique_and_region_helpers() {
        let l = LineSet::from_unsorted(vec![5, 1, 5, 3]);
        assert_eq!(l.0, vec![1, 3, 5]);
        assert!(l.contains(3) && !l.contains(2));
        let r = Region { anchor: 9, head: 4 };
        assert_eq!((r.start(), r.end(), r.is_caret()), (4, 9, false));
    }
}
