//! Soft wrap (`editor.word_wrap`, `editor.wrap_column`): which part of a line shows on which
//! screen row. Pure arithmetic on characters: the editor font is monospace, so a row holds
//! `cols` characters and the view needs no text measuring.
//!
//! A [`WrapMap`] lists the screen rows of the visible lines (folded lines are left out). Each row
//! is a slice `[start, end)` of one source line, in characters. The first row of a line starts at
//! 0, the last ends at `usize::MAX` (the end of the line, whatever its length).

use crate::folding::FoldState;

/// Wrapping is not offered above this many lines (the map is rebuilt on every edit).
pub const WRAP_MAX_LINES: usize = 50_000;
/// Narrowest wrap width accepted.
pub const MIN_COLS: usize = 10;

/// One screen row: a slice of a source line.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct WrapRow {
    /// Source line, 0-based.
    pub line: usize,
    /// First character of the slice.
    pub start: usize,
    /// One past the last character; `usize::MAX` = up to the end of the line.
    pub end: usize,
}

impl WrapRow {
    pub fn is_first(&self) -> bool {
        self.start == 0
    }
}

/// Start (in characters) of every screen row of `text` at `cols` characters per row. A break
/// goes after the last blank that fits; a word longer than a row is cut at `cols`.
pub fn break_starts(text: &str, cols: usize) -> Vec<usize> {
    let cols = cols.max(MIN_COLS);
    let chars: Vec<char> = text.trim_end_matches(['\n', '\r']).chars().collect();
    let mut starts = vec![0];
    let mut start = 0;
    while chars.len() - start > cols {
        let limit = start + cols; // first char that no longer fits
        let cut = chars[start + 1..limit].iter().rposition(|c| c.is_whitespace()).map(|i| start + 1 + i + 1);
        // `cut` = just after a blank inside the row (the row never exceeds `cols`).
        let next = match cut {
            Some(c) if c > start => c,
            _ => limit,
        };
        starts.push(next);
        start = next;
    }
    starts
}

/// The rows of every visible line.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct WrapMap {
    pub rows: Vec<WrapRow>,
}

impl WrapMap {
    /// `line_text(i)` gives the text of source line `i`; `total` is the line count.
    pub fn build(total: usize, fold: &FoldState, cols: usize, line_text: impl Fn(usize) -> String) -> Self {
        let mut rows = Vec::new();
        for line in fold.visible_lines(total) {
            let starts = break_starts(&line_text(line), cols);
            for (i, &s) in starts.iter().enumerate() {
                rows.push(WrapRow { line, start: s, end: starts.get(i + 1).copied().unwrap_or(usize::MAX) });
            }
        }
        Self { rows }
    }

    pub fn len(&self) -> usize {
        self.rows.len()
    }

    pub fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }

    /// Screen row (0-based) showing character `col` (0-based) of source line `line`, and the
    /// column inside that row. A hidden line maps to the row of the header that hides it.
    pub fn locate(&self, line: usize, col: usize) -> (usize, usize) {
        if self.rows.is_empty() {
            return (0, col);
        }
        let first = self.rows.partition_point(|r| r.line < line);
        let first = if self.rows.get(first).is_some_and(|r| r.line == line) { first } else { first.saturating_sub(1) };
        let l = self.rows[first].line;
        let mut row = first;
        while let Some(next) = self.rows.get(row + 1) {
            if next.line == l && next.start <= col {
                row += 1;
            } else {
                break;
            }
        }
        (row, col.saturating_sub(self.rows[row].start))
    }

    /// The row on screen row `row` (clamped to the last one).
    pub fn row(&self, row: usize) -> Option<WrapRow> {
        self.rows.get(row.min(self.rows.len().saturating_sub(1))).copied()
    }

    /// Row `delta` rows away from `row`, clamped.
    pub fn step(&self, row: usize, delta: isize) -> usize {
        (row as isize).saturating_add(delta).clamp(0, self.rows.len().saturating_sub(1) as isize) as usize
    }
}

/// Number of characters per row for the settings: a set `wrap_column` wins, else what fits in
/// `width_px` (text area width) at `cell_w` px per character. `None` = wrapping is off.
pub fn wrap_cols(word_wrap: bool, wrap_column: i64, width_px: f64, cell_w: f64) -> Option<usize> {
    if !word_wrap {
        return None;
    }
    if wrap_column >= MIN_COLS as i64 {
        return Some(wrap_column.min(1000) as usize);
    }
    if !(cell_w > 0.0) || !width_px.is_finite() || width_px <= 0.0 {
        return None;
    }
    Some(((width_px / cell_w).floor() as usize).max(MIN_COLS))
}

/// The fragments (`text` pieces, in order) cut to the characters `[start, end)` of their line.
/// `len_of`/`cut` are generic so the editor's fragment type stays out of this crate: `cut(frag, a, b)`
/// returns the piece of `frag` between its own character offsets `a..b`.
pub fn slice_pieces<T>(frags: &[T], len_of: impl Fn(&T) -> usize, cut: impl Fn(&T, usize, usize) -> T, start: usize, end: usize) -> Vec<T> {
    let mut out = Vec::new();
    let mut at = 0usize;
    for f in frags {
        let n = len_of(f);
        let (a, b) = (at.max(start), (at + n).min(end));
        if a < b {
            out.push(cut(f, a - at, b - at));
        }
        at += n;
        if at >= end {
            break;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::folding::FoldRegion;

    #[test]
    fn short_lines_have_one_row() {
        assert_eq!(break_starts("hello", 20), vec![0]);
        assert_eq!(break_starts("", 20), vec![0]);
        assert_eq!(break_starts("a line of exactly 20", 20), vec![0]);
        assert_eq!(break_starts("text\r\n", 20), vec![0]);
    }

    #[test]
    fn breaks_after_the_last_blank_that_fits() {
        // "aaaa bbbb cccc dddd" (19 chars) in 10 columns: breaks after "aaaa bbbb " (10 chars).
        let t = "aaaa bbbb cccc dddd";
        assert_eq!(break_starts(t, 10), vec![0, 10]);
        // 12 columns: "aaaa bbbb " fits, "cccc" does not
        assert_eq!(break_starts("aaaa bbbb cccc dddd eeee", 12), vec![0, 10, 20]);
    }

    #[test]
    fn long_words_are_cut_at_the_width() {
        let t = "x".repeat(25);
        assert_eq!(break_starts(&t, 10), vec![0, 10, 20]);
        // never narrower than MIN_COLS
        assert_eq!(break_starts(&t, 2), vec![0, 10, 20]);
    }

    #[test]
    fn every_char_lands_in_exactly_one_row() {
        let t = "the quick brown fox jumps over the lazy dog and keeps running far away";
        for cols in [10, 11, 17, 25, 40] {
            let s = break_starts(t, cols);
            assert_eq!(s[0], 0);
            assert!(s.windows(2).all(|w| w[0] < w[1] && w[1] - w[0] <= cols), "{cols}: {s:?}");
            assert!(t.chars().count() - s.last().unwrap() <= cols);
        }
    }

    fn map(total_text: &str, cols: usize, fold: &FoldState) -> WrapMap {
        let lines: Vec<String> = total_text.split('\n').map(String::from).collect();
        WrapMap::build(lines.len(), fold, cols, |i| lines[i].clone())
    }

    #[test]
    fn map_lists_rows_and_locates_carets() {
        let m = map("aaaa bbbb cccc dddd\nshort\n", 10, &FoldState::empty());
        // line 0 -> 2 rows, line 1 -> 1 row, line 2 (empty) -> 1 row
        assert_eq!(m.len(), 4);
        assert_eq!(m.rows[0], WrapRow { line: 0, start: 0, end: 10 });
        assert_eq!(m.rows[1], WrapRow { line: 0, start: 10, end: usize::MAX });
        assert_eq!(m.locate(0, 3), (0, 3));
        assert_eq!(m.locate(0, 10), (1, 0), "the break column belongs to the next row");
        assert_eq!(m.locate(0, 14), (1, 4));
        assert_eq!(m.locate(1, 2), (2, 2));
        assert_eq!(m.locate(2, 0), (3, 0));
        assert_eq!(m.step(0, 1), 1);
        assert_eq!(m.step(3, 5), 3);
        assert_eq!(m.step(0, -1), 0);
        assert_eq!(m.row(99).unwrap().line, 2);
    }

    #[test]
    fn folded_lines_have_no_rows_and_map_to_their_header() {
        let fold = FoldState::new(vec![FoldRegion { start_line: 0, end_line: 2, indent_level: 0, is_folded: true }]);
        let m = map("head\n  one\n  two\ntail", 10, &fold);
        assert_eq!(m.rows.iter().map(|r| r.line).collect::<Vec<_>>(), vec![0, 3]);
        assert_eq!(m.locate(1, 0).0, 0, "hidden line -> header row");
        assert_eq!(m.locate(3, 1), (1, 1));
    }

    #[test]
    fn cols_follow_the_settings() {
        assert_eq!(wrap_cols(false, 80, 800.0, 8.0), None);
        assert_eq!(wrap_cols(true, 80, 800.0, 8.0), Some(80), "an explicit column wins");
        assert_eq!(wrap_cols(true, 0, 800.0, 8.0), Some(100), "else what fits");
        assert_eq!(wrap_cols(true, 0, 0.0, 8.0), None, "no width yet");
        assert_eq!(wrap_cols(true, 3, 400.0, 8.0), Some(50), "below the minimum counts as unset");
        assert_eq!(wrap_cols(true, 5000, 400.0, 8.0), Some(1000));
        assert_eq!(wrap_cols(true, 0, 40.0, 8.0), Some(MIN_COLS));
    }

    #[test]
    fn pieces_are_cut_to_the_row() {
        let frags = vec!["hello ".to_string(), "world".to_string(), "!!".to_string()];
        let cut = |f: &String, a: usize, b: usize| f.chars().skip(a).take(b - a).collect::<String>();
        let len = |f: &String| f.chars().count();
        assert_eq!(slice_pieces(&frags, len, cut, 0, 8), vec!["hello ", "wo"]);
        assert_eq!(slice_pieces(&frags, len, cut, 8, usize::MAX), vec!["rld", "!!"]);
        assert_eq!(slice_pieces(&frags, len, cut, 4, 7), vec!["o ", "w"]);
        assert!(slice_pieces(&frags, len, cut, 50, usize::MAX).is_empty());
    }
}
