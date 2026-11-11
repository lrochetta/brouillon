#![allow(dead_code)]

//! Pure-data helper for bracket-pair highlighting (alpha.13).
//!
//! Given the cursor's byte position in a rope's text and an optional
//! Highlighter (tree-sitter-parsed), return the pair of byte positions
//! that the paint layer should decorate, plus whether the pair matched.
//!
//! Stays decoupled from Floem — testable as a pure fn. The painter in
//! `main.rs` consumes the returned `BracketPaint` value and computes
//! line/column from the byte positions using the rope.

use brouillon_editor_engine::highlight::{BracketMatch, Highlighter, matching_bracket_text};

/// Result of a bracket-pair lookup at a cursor position.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BracketPaint {
    /// Cursor is not on a bracket character. Painter should render nothing.
    None,
    /// Cursor is on a bracket but no pair was found (open without close,
    /// or close without open). Painter should decorate only `cursor_byte`
    /// with the unmatched style (typically red).
    Unmatched { cursor_byte: usize },
    /// Cursor is on a bracket and its pair was found. Painter should
    /// decorate BOTH positions with the matched style (typically accent).
    Matched { cursor_byte: usize, pair_byte: usize },
}

impl BracketPaint {
    /// True when the painter should render any bracket decoration at all.
    pub fn is_active(&self) -> bool {
        !matches!(self, BracketPaint::None)
    }

    /// Iterate the byte positions the painter should decorate. Returns
    /// 0, 1, or 2 positions depending on the variant.
    pub fn positions(&self) -> impl Iterator<Item = usize> {
        let v: Vec<usize> = match *self {
            BracketPaint::None => vec![],
            BracketPaint::Unmatched { cursor_byte } => vec![cursor_byte],
            BracketPaint::Matched { cursor_byte, pair_byte } => vec![cursor_byte, pair_byte],
        };
        v.into_iter()
    }
}

/// Compute the bracket-paint state. If `hl` is Some, prefer the
/// tree-sitter-aware variant; else fall back to the text scanner.
pub fn compute(text: &str, cursor_byte: usize, hl: Option<&Highlighter>) -> BracketPaint {
    // First sanity-check the cursor: if past end-of-text, no bracket.
    if cursor_byte >= text.len() {
        return BracketPaint::None;
    }
    // Look up the byte char — must be a bracket char to be interesting.
    let bracket_chars = b"()[]{}";
    let b = text.as_bytes()[cursor_byte];
    if !bracket_chars.contains(&b) {
        return BracketPaint::None;
    }
    // Resolve the pair via tree-sitter if available, else text fallback.
    let result = match hl {
        Some(h) => h.matching_bracket(text, cursor_byte),
        None => matching_bracket_text(text, cursor_byte),
    };
    match result {
        BracketMatch::NotOnBracket => BracketPaint::None,
        BracketMatch::Unmatched => BracketPaint::Unmatched { cursor_byte },
        BracketMatch::At(pair_byte) => BracketPaint::Matched { cursor_byte, pair_byte },
    }
}

/// Helper for the painter: given a byte position in `text` and the byte
/// offset of a particular row's first character, return the column
/// (1-based) where the bracket sits, OR None if the bracket is not on
/// that row. Used by `line_row` to scope per-row decoration.
pub fn col_within_row(text: &str, byte_pos: usize, row_start_byte: usize, row_end_byte: usize) -> Option<usize> {
    if byte_pos < row_start_byte || byte_pos >= row_end_byte {
        return None;
    }
    // Count characters from row_start to byte_pos. Assumes UTF-8 valid.
    // 1-based column for status-bar parity.
    let slice = &text[row_start_byte..byte_pos];
    Some(slice.chars().count() + 1)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cursor_off_bracket_returns_none() {
        // "let x = 1;" — cursor on 'x' (byte 4) is not on a bracket.
        let bp = compute("let x = 1;", 4, None);
        assert_eq!(bp, BracketPaint::None);
    }

    #[test]
    fn matched_pair_via_text_fallback() {
        // "(hi)" — cursor on '(' at byte 0 should pair with ')' at byte 3.
        let bp = compute("(hi)", 0, None);
        assert_eq!(bp, BracketPaint::Matched { cursor_byte: 0, pair_byte: 3 });
    }

    #[test]
    fn matched_pair_from_close_to_open() {
        // Cursor on ')' at byte 3 should pair back to '(' at byte 0.
        let bp = compute("(hi)", 3, None);
        assert_eq!(bp, BracketPaint::Matched { cursor_byte: 3, pair_byte: 0 });
    }

    #[test]
    fn unmatched_open_returns_unmatched() {
        // "(no close" — cursor on '(' has no pair.
        let bp = compute("(no close", 0, None);
        assert_eq!(bp, BracketPaint::Unmatched { cursor_byte: 0 });
    }

    #[test]
    fn unmatched_close_returns_unmatched() {
        // ")" alone — cursor on ')' has no opener.
        let bp = compute(")", 0, None);
        assert_eq!(bp, BracketPaint::Unmatched { cursor_byte: 0 });
    }

    #[test]
    fn cursor_past_end_returns_none() {
        let bp = compute("()", 99, None);
        assert_eq!(bp, BracketPaint::None);
    }

    #[test]
    fn empty_text_returns_none() {
        let bp = compute("", 0, None);
        assert_eq!(bp, BracketPaint::None);
    }

    #[test]
    fn nested_brackets_resolve_to_correct_pair() {
        // "(a(b)c)" — cursor on outer '(' at byte 0 should pair to outer ')' at byte 6.
        let bp = compute("(a(b)c)", 0, None);
        assert_eq!(bp, BracketPaint::Matched { cursor_byte: 0, pair_byte: 6 });
        // Cursor on inner '(' at byte 2 should pair to inner ')' at byte 4.
        let bp = compute("(a(b)c)", 2, None);
        assert_eq!(bp, BracketPaint::Matched { cursor_byte: 2, pair_byte: 4 });
    }

    #[test]
    fn square_and_curly_brackets_match() {
        let bp = compute("[x]", 0, None);
        assert_eq!(bp, BracketPaint::Matched { cursor_byte: 0, pair_byte: 2 });
        let bp = compute("{y}", 0, None);
        assert_eq!(bp, BracketPaint::Matched { cursor_byte: 0, pair_byte: 2 });
    }

    #[test]
    fn positions_iterates_correct_count() {
        assert_eq!(BracketPaint::None.positions().count(), 0);
        assert_eq!(BracketPaint::Unmatched { cursor_byte: 0 }.positions().count(), 1);
        assert_eq!(
            BracketPaint::Matched { cursor_byte: 0, pair_byte: 5 }.positions().count(),
            2
        );
    }

    #[test]
    fn is_active_true_when_painting() {
        assert!(!BracketPaint::None.is_active());
        assert!(BracketPaint::Unmatched { cursor_byte: 0 }.is_active());
        assert!(BracketPaint::Matched { cursor_byte: 0, pair_byte: 5 }.is_active());
    }

    #[test]
    fn col_within_row_returns_none_outside_range() {
        // Row covers bytes 10..20, bracket at byte 5 is outside.
        assert_eq!(col_within_row("abcdef", 5, 10, 20), None);
    }

    #[test]
    fn col_within_row_returns_1_based_col() {
        // Row "let x = (1);\n" — let's say row_start=0, row_end=13. The '(' is at byte 8 → col 9 (1-based).
        let text = "let x = (1);\n";
        assert_eq!(col_within_row(text, 8, 0, 13), Some(9));
    }

    #[test]
    fn col_within_row_handles_multibyte_correctly() {
        // "héllo()" — é is 2 bytes. The '(' is at byte 6 ('h'=1+'é'=2+'l'=1+'l'=1+'o'=1=6). Col = 6 (1-based) because chars().count() of "héllo" is 5, +1 = 6.
        let text = "héllo()";
        // byte_pos of '(' is 6
        assert_eq!(col_within_row(text, 6, 0, text.len()), Some(6));
    }

    #[test]
    fn test_compute_nested() {
        let text = "((x))";
        let bp = compute(text, 1, None);
        assert_eq!(bp, BracketPaint::Matched { cursor_byte: 1, pair_byte: 3 });
    }

    #[test]
    fn test_compute_mismatched() {
        let text = "(x]";
        let bp = compute(text, 0, None);
        assert_eq!(bp, BracketPaint::Unmatched { cursor_byte: 0 });
    }

    #[test]
    fn test_bracket_paint_positions_values() {
        let bp = BracketPaint::Matched { cursor_byte: 2, pair_byte: 8 };
        let pos: Vec<usize> = bp.positions().collect();
        assert_eq!(pos, vec![2, 8]);
    }

    #[test]
    fn test_col_within_row_exactly_on_boundary() {
        let text = "abc";
        assert_eq!(col_within_row(text, 0, 0, 3), Some(1));
        assert_eq!(col_within_row(text, 3, 0, 3), None);
    }
}
