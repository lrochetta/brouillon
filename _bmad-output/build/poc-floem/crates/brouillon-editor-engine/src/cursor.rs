//! Pure-logic cursor navigation. Kept out of `main.rs` so it's testable
//! without spinning up a Floem reactive scope.
//!
//! `RwSignal<(usize, usize)>` is the cursor state — 1-based (line, col),
//! identical convention to the rest of the project. The handler reads
//! the buffer (`&Buffer`) to clamp against real line lengths.

use floem::keyboard::{Key, NamedKey};
use floem::reactive::{RwSignal, SignalGet, SignalUpdate};

use crate::buffer::Buffer;
use crate::folding::FoldState;

/// Number of lines a PageUp / PageDown jump moves. Real editors compute
/// this from the visible viewport height ; we use a fixed step until
/// the Scroll widget exposes its visible-rows count.
pub const PAGE_STEP: usize = 20;

/// Selection state of the editor.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Selection {
    /// The starting point of the selection, 1-based (line, col).
    pub anchor: (usize, usize),
    /// The ending point / current cursor location, 1-based (line, col).
    pub head: (usize, usize),
    /// True if this is a block / column / rectangular selection.
    pub is_block: bool,
}

impl Selection {
    /// Create a new selection.
    pub fn new(anchor: (usize, usize), head: (usize, usize), is_block: bool) -> Self {
        Self { anchor, head, is_block }
    }

    /// Returns true if the selection is empty (anchor == head).
    pub fn is_empty(&self) -> bool {
        self.anchor == self.head
    }

    /// Check if a given position (1-based line, col) is within the selection.
    /// Supports both standard linear selection and block/rectangular selection.
    pub fn contains(&self, line: usize, col: usize) -> bool {
        if self.is_empty() {
            return false;
        }

        if self.is_block {
            // Block selection logic: the column must fall between start_col and end_col
            // (inclusive) and the line must fall between start_line and end_line (inclusive).
            let min_line = self.anchor.0.min(self.head.0);
            let max_line = self.anchor.0.max(self.head.0);
            let min_col = self.anchor.1.min(self.head.1);
            let max_col = self.anchor.1.max(self.head.1);

            line >= min_line && line <= max_line && col >= min_col && col <= max_col
        } else {
            // Linear selection logic.
            let (start, end) = if self.anchor.0 < self.head.0 || (self.anchor.0 == self.head.0 && self.anchor.1 < self.head.1) {
                (self.anchor, self.head)
            } else {
                (self.head, self.anchor)
            };

            if line < start.0 || line > end.0 {
                return false;
            }

            if line > start.0 && line < end.0 {
                return true;
            }

            if start.0 == end.0 {
                // Same line selection
                col >= start.1 && col < end.1
            } else if line == start.0 {
                // First line of selection: from start.1 to end of line
                col >= start.1
            } else {
                // Last line of selection: from start of line to end.1
                col < end.1
            }
        }
    }
}

/// Apply a key event to the cursor, clamped against the buffer geometry.
/// Returns true if the cursor moved, false if no handler matched.
pub fn handle_cursor_key(
    logical: &Key,
    cursor: RwSignal<(usize, usize)>,
    buffer: &Buffer,
) -> bool {
    handle_cursor_key_folded(logical, cursor, buffer, &FoldState::empty())
}

/// Same as [`handle_cursor_key`] when some lines are hidden by folds: a caret sitting in a
/// hidden line first moves to the header that hides it, then Up/Down/PageUp/PageDown step over
/// the hidden lines and Left/Right at a line edge jump to the neighbouring VISIBLE line.
/// Home/End stay on the (visible) caret line.
pub fn handle_cursor_key_folded(
    logical: &Key,
    cursor: RwSignal<(usize, usize)>,
    buffer: &Buffer,
    fold: &FoldState,
) -> bool {
    // 1-based bounds. A column may be the position *after* the last char — i.e.
    // the cursor can sit one past EOL like every mainstream editor.
    let line_len = |line_1b: usize| -> usize {
        buffer
            .nth_line(line_1b.saturating_sub(1))
            .map(|s| s.chars().count() + 1)
            .unwrap_or(1)
    };

    let total = buffer.line_count().max(1);
    // Line (1-based) `delta` visible rows away from `l`.
    let step = |l: usize, delta: isize| fold.step_visible(total, l - 1, delta) + 1;
    if matches!(logical, Key::Named(NamedKey::ArrowUp | NamedKey::ArrowDown | NamedKey::ArrowLeft | NamedKey::ArrowRight | NamedKey::Home | NamedKey::End | NamedKey::PageUp | NamedKey::PageDown)) {
        let (l, c) = cursor.get_untracked();
        let shown = fold.nearest_visible(l.saturating_sub(1).min(total - 1)) + 1;
        if shown != l {
            // The caret was inside a folded block: it lives on the header line.
            cursor.set((shown, c.min(line_len(shown))));
        }
    }

    match logical {
        Key::Named(NamedKey::ArrowUp) => {
            cursor.update(|(l, c)| {
                let to = step(*l, -1);
                if to != *l {
                    *l = to;
                    *c = (*c).min(line_len(*l));
                }
            });
            true
        }
        Key::Named(NamedKey::ArrowDown) => {
            cursor.update(|(l, c)| {
                let to = step(*l, 1);
                if to != *l {
                    *l = to;
                    *c = (*c).min(line_len(*l));
                }
            });
            true
        }
        Key::Named(NamedKey::ArrowLeft) => {
            cursor.update(|(l, c)| {
                if *c > 1 {
                    *c -= 1;
                } else {
                    let to = step(*l, -1);
                    if to != *l {
                        // Wrap to end of previous visible line.
                        *l = to;
                        *c = line_len(*l);
                    }
                }
            });
            true
        }
        Key::Named(NamedKey::ArrowRight) => {
            cursor.update(|(l, c)| {
                let max = line_len(*l);
                if *c < max {
                    *c += 1;
                } else {
                    let to = step(*l, 1);
                    if to != *l {
                        // Wrap to start of next visible line.
                        *l = to;
                        *c = 1;
                    }
                }
            });
            true
        }
        Key::Named(NamedKey::Home) => {
            cursor.update(|(_, c)| *c = 1);
            true
        }
        Key::Named(NamedKey::End) => {
            cursor.update(|(l, c)| *c = line_len(*l));
            true
        }
        Key::Named(NamedKey::PageUp) => {
            cursor.update(|(l, c)| {
                *l = step(*l, -(PAGE_STEP as isize));
                *c = (*c).min(line_len(*l));
            });
            true
        }
        Key::Named(NamedKey::PageDown) => {
            cursor.update(|(l, c)| {
                *l = step(*l, PAGE_STEP as isize);
                *c = (*c).min(line_len(*l));
            });
            true
        }
        _ => false,
    }
}

// ---------------------------------------------------------------------------
// BK-037 / BK-039 / BK-040 - multi-region selection model (pure, byte offsets).
//
// Convention: a `&[Region]` handed around is in "added order, LAST = primary".
// Edit helpers sort/merge internally (`normalize`) and return carets in
// ascending order.
// ---------------------------------------------------------------------------

use brouillon_core::contract::{EditSet, Region, TextEdit};

/// Byte offset of the start of every line (`split('\n')` semantics: a trailing
/// newline yields a final empty line).
pub fn line_starts(text: &str) -> Vec<usize> {
    let mut v = vec![0];
    v.extend(text.bytes().enumerate().filter(|(_, b)| *b == b'\n').map(|(i, _)| i + 1));
    v
}

/// Content range of 0-based `line` (without `\n` / `\r\n`).
pub fn line_range(text: &str, starts: &[usize], line: usize) -> (usize, usize) {
    let s = starts[line];
    let mut e = starts.get(line + 1).map(|n| n - 1).unwrap_or(text.len());
    if e > s && text.as_bytes()[e - 1] == b'\r' {
        e -= 1;
    }
    (s, e)
}

/// (line0, char_col0) of a byte offset.
pub fn byte_to_line_col(text: &str, byte: usize) -> (usize, usize) {
    let byte = byte.min(text.len());
    let starts = line_starts(text);
    let line = starts.partition_point(|&s| s <= byte) - 1;
    (line, text[starts[line]..byte].chars().count())
}

/// Byte offset of (line0, char_col0), clamped to the line end.
pub fn line_col_to_byte(text: &str, line: usize, col: usize) -> usize {
    let starts = line_starts(text);
    let line = line.min(starts.len() - 1);
    let (s, e) = line_range(text, &starts, line);
    text[s..e].char_indices().nth(col).map(|(i, _)| s + i).unwrap_or(e)
}

/// Sort and merge overlapping regions (touching selections stay apart; a caret
/// touching or inside another region is absorbed).
pub fn normalize(regions: &[Region]) -> Vec<Region> {
    let mut v: Vec<Region> = regions.to_vec();
    v.sort_by_key(|r| (r.start(), r.end()));
    let mut out: Vec<Region> = Vec::with_capacity(v.len());
    for r in v {
        match out.last_mut() {
            Some(p) if r.start() < p.end() || (r.start() == p.end() && (r.is_caret() || p.is_caret())) => {
                let (s, e) = (p.start(), p.end().max(r.end()));
                *p = if p.head < p.anchor && r.head < r.anchor { Region { anchor: e, head: s } } else { Region { anchor: s, head: e } };
            }
            _ => out.push(r),
        }
    }
    out
}

/// Esc: keep only the primary (last) region.
pub fn collapse_to_primary(regions: &[Region]) -> Vec<Region> {
    regions.last().map(|r| vec![*r]).unwrap_or_default()
}

/// Edit set replacing each (normalized) region by the aligned text, plus the
/// resulting carets.
fn replace_each(regions: &[Region], texts: &[String]) -> (EditSet, Vec<Region>) {
    let mut edits = Vec::with_capacity(regions.len());
    let mut carets = Vec::with_capacity(regions.len());
    let mut delta: isize = 0;
    for (r, t) in regions.iter().zip(texts) {
        edits.push(TextEdit::new(r.start(), r.end(), t.clone()));
        carets.push(Region::caret((r.start() as isize + delta) as usize + t.len()));
        delta += t.len() as isize - (r.end() - r.start()) as isize;
    }
    (EditSet(edits), carets)
}

/// Type `s` at every region (replacing selected text). One `EditSet`.
pub fn insert_text(regions: &[Region], s: &str) -> (EditSet, Vec<Region>) {
    let n = normalize(regions);
    let texts = vec![s.to_string(); n.len()];
    replace_each(&n, &texts)
}

/// Paste: with exactly one clipboard line per region the lines are distributed
/// in order, otherwise the whole text goes to every region.
pub fn paste(regions: &[Region], clip: &str) -> (EditSet, Vec<Region>) {
    let n = normalize(regions);
    let lines: Vec<&str> = clip.split('\n').collect();
    if n.len() > 1 && lines.len() == n.len() {
        let texts: Vec<String> = lines.iter().map(|l| l.trim_end_matches('\r').to_string()).collect();
        replace_each(&n, &texts)
    } else {
        insert_text(regions, clip)
    }
}

fn prev_char(text: &str, at: usize) -> usize {
    text[..at].char_indices().next_back().map(|(i, _)| i).unwrap_or(0)
}
fn next_char(text: &str, at: usize) -> usize {
    text[at..].chars().next().map(|c| at + c.len_utf8()).unwrap_or(at)
}

/// Backspace (`forward == false`) / Delete (`forward == true`) on all regions.
/// Selections are deleted as-is; carets delete one char (`\r\n` counts as one).
pub fn delete_chars(text: &str, regions: &[Region], forward: bool) -> (EditSet, Vec<Region>) {
    let n: Vec<Region> = normalize(regions)
        .into_iter()
        .map(|r| {
            if !r.is_caret() {
                return r;
            }
            let p = r.head;
            let (mut a, mut b) = if forward { (p, next_char(text, p)) } else { (prev_char(text, p), p) };
            if &text[a..b] == "\n" && a > 0 && text.as_bytes()[a - 1] == b'\r' {
                a -= 1;
            } else if &text[a..b] == "\r" && text[b..].starts_with('\n') {
                b += 1;
            }
            Region { anchor: a, head: b }
        })
        .collect();
    let n = normalize(&n);
    let texts = vec![String::new(); n.len()];
    replace_each(&n, &texts)
}

/// Caret motions applied to every region.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Motion {
    Left,
    Right,
    Up,
    Down,
    Home,
    End,
}

/// Move every region's head (`extend` keeps the anchor), then merge overlaps.
/// Without `extend`, a non-empty region collapses to its edge for Left/Right.
pub fn move_regions(text: &str, regions: &[Region], m: Motion, extend: bool) -> Vec<Region> {
    let starts = line_starts(text);
    let out: Vec<Region> = regions
        .iter()
        .map(|r| {
            let (line, col) = byte_to_line_col(text, r.head);
            let (ls, le) = line_range(text, &starts, line);
            let head = match m {
                Motion::Left if !extend && !r.is_caret() => r.start(),
                Motion::Right if !extend && !r.is_caret() => r.end(),
                Motion::Left => prev_char(text, r.head),
                Motion::Right => next_char(text, r.head),
                Motion::Home => ls,
                Motion::End => le,
                Motion::Up if line == 0 => 0,
                Motion::Up => line_col_to_byte(text, line - 1, col),
                Motion::Down if line + 1 >= starts.len() => text.len(),
                Motion::Down => line_col_to_byte(text, line + 1, col),
            };
            Region { anchor: if extend { r.anchor } else { head }, head }
        })
        .collect();
    normalize(&out)
}

/// Indent: insert `unit` at the start of every distinct line touched by a region.
pub fn indent(text: &str, regions: &[Region], unit: &str) -> (EditSet, Vec<Region>) {
    let starts = line_starts(text);
    let mut lines: Vec<usize> = Vec::new();
    for r in normalize(regions) {
        let a = byte_to_line_col(text, r.start()).0;
        let mut b = byte_to_line_col(text, r.end()).0;
        if !r.is_caret() && b > a && starts[b] == r.end() {
            b -= 1; // a selection ending at a line start does not touch that line
        }
        lines.extend(a..=b);
    }
    lines.sort_unstable();
    lines.dedup();
    let edits = EditSet(lines.iter().map(|&l| TextEdit::insert(starts[l], unit)).collect());
    let shift = |p: usize| p + unit.len() * lines.iter().filter(|&&l| starts[l] <= p).count();
    let regs = regions.iter().map(|r| Region { anchor: shift(r.anchor), head: shift(r.head) }).collect();
    (edits, regs)
}

// ---- BK-039: column / block editing ----------------------------------------

/// Rectangle in (0-based line, char column) space; columns are half-open caret
/// positions (`left == right` = zero-width, i.e. a column of carets).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ColumnRect {
    pub top: usize,
    pub bottom: usize,
    pub left: usize,
    pub right: usize,
}

impl ColumnRect {
    /// Rect spanned by two (line0, col0) corners in any order.
    pub fn from_corners(a: (usize, usize), b: (usize, usize)) -> Self {
        Self { top: a.0.min(b.0), bottom: a.0.max(b.0), left: a.1.min(b.1), right: a.1.max(b.1) }
    }
}

/// One region per row of the rectangle (short rows clamp to their end).
pub fn column_regions(text: &str, rect: ColumnRect) -> Vec<Region> {
    let last = line_starts(text).len() - 1;
    (rect.top..=rect.bottom.min(last))
        .map(|l| Region { anchor: line_col_to_byte(text, l, rect.left), head: line_col_to_byte(text, l, rect.right) })
        .collect()
}

/// Type `s` into every row: the rect's text is replaced; with `virtual_space`
/// short rows are first padded with spaces up to the left column.
pub fn column_insert(text: &str, rect: ColumnRect, s: &str, virtual_space: bool) -> (EditSet, Vec<Region>) {
    let starts = line_starts(text);
    let mut edits = Vec::new();
    let mut carets = Vec::new();
    let mut delta: isize = 0;
    for l in rect.top..=rect.bottom.min(starts.len() - 1) {
        let (ls, le) = line_range(text, &starts, l);
        let len = text[ls..le].chars().count();
        let a = line_col_to_byte(text, l, rect.left);
        let b = line_col_to_byte(text, l, rect.right);
        let pad = if virtual_space && len < rect.left { " ".repeat(rect.left - len) } else { String::new() };
        let ins = format!("{pad}{s}");
        carets.push(Region::caret((a as isize + delta) as usize + ins.len()));
        delta += ins.len() as isize - (b - a) as isize;
        edits.push(TextEdit::new(a, b, ins));
    }
    (EditSet(edits), carets)
}

/// Delete the rectangle's content on every row.
pub fn column_delete(text: &str, rect: ColumnRect) -> (EditSet, Vec<Region>) {
    column_insert(text, rect, "", false)
}

/// Paste `clip` column-wise with its top-left at (line0, col0): clipboard line
/// `i` goes to row `line + i` (short rows padded with spaces; rows past the end
/// of the text are appended). A single-line clip is repeated down to
/// `repeat_to` (inclusive) when given. Rows beyond the clip are untouched.
pub fn column_paste(text: &str, line: usize, col: usize, clip: &str, repeat_to: Option<usize>) -> (EditSet, Vec<Region>) {
    let starts = line_starts(text);
    let mut rows: Vec<&str> = clip.split('\n').map(|l| l.trim_end_matches('\r')).collect();
    if rows.len() > 1 && rows.last() == Some(&"") {
        rows.pop();
    }
    if rows.len() == 1 {
        if let Some(to) = repeat_to {
            rows = vec![rows[0]; to.saturating_sub(line) + 1];
        }
    }
    let mut edits = Vec::new();
    let mut carets = Vec::new();
    let mut delta: isize = 0;
    let mut appended = String::new();
    for (i, row) in rows.iter().enumerate() {
        let l = line + i;
        if l < starts.len() {
            let (ls, le) = line_range(text, &starts, l);
            let len = text[ls..le].chars().count();
            let at = line_col_to_byte(text, l, col);
            let ins = format!("{}{row}", " ".repeat(col.saturating_sub(len)));
            carets.push(Region::caret((at as isize + delta) as usize + ins.len()));
            delta += ins.len() as isize;
            edits.push(TextEdit::insert(at, ins));
        } else {
            appended.push_str(&format!("\n{}{row}", " ".repeat(col)));
        }
    }
    if !appended.is_empty() {
        carets.push(Region::caret(text.len() + delta as usize + appended.len()));
        edits.push(TextEdit::insert(text.len(), appended));
    }
    (EditSet(edits), carets)
}

impl Selection {
    /// Convert to byte regions. A block selection becomes one region per row
    /// (1-based columns are caret positions, so equal columns give a column of
    /// carets): the existing Alt-drag column is just N regions.
    pub fn to_regions(&self, text: &str) -> Vec<Region> {
        let to_b = |(l, c): (usize, usize)| line_col_to_byte(text, l.saturating_sub(1), c.saturating_sub(1));
        if !self.is_block {
            return vec![Region { anchor: to_b(self.anchor), head: to_b(self.head) }];
        }
        column_regions(
            text,
            ColumnRect::from_corners(
                (self.anchor.0.saturating_sub(1), self.anchor.1.saturating_sub(1)),
                (self.head.0.saturating_sub(1), self.head.1.saturating_sub(1)),
            ),
        )
    }
}

// ---- BK-040: caret behaviours ----------------------------------------------

/// Smart Home: first press goes to the first non-blank of the line; pressed
/// again (already there) goes to column 0, and back.
pub fn smart_home(text: &str, caret: usize) -> usize {
    let starts = line_starts(text);
    let (line, _) = byte_to_line_col(text, caret);
    let (ls, le) = line_range(text, &starts, line);
    let first = text[ls..le].find(|c: char| c != ' ' && c != '\t').map(|i| ls + i).unwrap_or(le);
    if caret == first { ls } else { first }
}

/// Word range around `at` with a configurable delimiter set (a char is a word
/// char unless whitespace or listed in `delims`).
pub fn word_range(text: &str, at: usize, delims: &str) -> Option<(usize, usize)> {
    let is_w = |c: char| !c.is_whitespace() && !delims.contains(c);
    let mut s = at;
    while let Some(c) = text[..s].chars().next_back().filter(|c| is_w(*c)) {
        s -= c.len_utf8();
    }
    let mut e = at;
    while let Some(c) = text[e..].chars().next().filter(|c| is_w(*c)) {
        e += c.len_utf8();
    }
    (s < e).then_some((s, e))
}

/// Overwrite mode: typing `s` replaces the next char of each caret, but never a
/// line break (at EOL it inserts).
pub fn overwrite_text(text: &str, regions: &[Region], s: &str) -> (EditSet, Vec<Region>) {
    let adj: Vec<Region> = normalize(regions)
        .into_iter()
        .map(|r| match text[r.end()..].chars().next() {
            Some(c) if r.is_caret() && c != '\n' && c != '\r' => Region { anchor: r.start(), head: r.end() + c.len_utf8() },
            _ => r,
        })
        .collect();
    insert_text(&adj, s)
}

#[cfg(test)]
mod multi_tests {
    use super::*;

    fn apply(t: &str, e: &EditSet) -> String {
        e.apply_to_str(t).unwrap()
    }

    #[test]
    fn three_carets_typing_x_is_three_insertions() {
        let t = "a\nb\nc";
        let (e, c) = insert_text(&[Region::caret(0), Region::caret(2), Region::caret(4)], "x");
        assert_eq!(e.len(), 3);
        assert_eq!(apply(t, &e), "xa\nxb\nxc");
        assert_eq!(c, vec![Region::caret(1), Region::caret(4), Region::caret(7)]);
    }

    #[test]
    fn overlapping_regions_merge() {
        let n = normalize(&[Region { anchor: 0, head: 5 }, Region { anchor: 3, head: 8 }, Region::caret(8)]);
        assert_eq!(n, vec![Region { anchor: 0, head: 8 }]);
        // touching selections stay apart
        assert_eq!(normalize(&[Region { anchor: 0, head: 2 }, Region { anchor: 2, head: 4 }]).len(), 2);
    }

    #[test]
    fn column_drag_then_typing_equals_n_carets() {
        let t = "abc\ndef\nghi";
        let sel = Selection::new((1, 2), (3, 2), true);
        let regs = sel.to_regions(t);
        assert_eq!(regs, vec![Region::caret(1), Region::caret(5), Region::caret(9)]);
        let (e1, _) = insert_text(&regs, "X");
        let (e2, _) = insert_text(&[Region::caret(1), Region::caret(5), Region::caret(9)], "X");
        assert_eq!(e1, e2);
        assert_eq!(apply(t, &e1), "aXbc\ndXef\ngXhi");
    }

    #[test]
    fn backspace_forward_delete_and_selection() {
        let t = "ab\ncd";
        let (e, c) = delete_chars(t, &[Region::caret(1), Region::caret(4)], false);
        assert_eq!(apply(t, &e), "b\nd");
        assert_eq!(c, vec![Region::caret(0), Region::caret(2)]);
        let (e, _) = delete_chars("a\r\nb", &[Region::caret(1)], true);
        assert_eq!(apply("a\r\nb", &e), "ab");
        let (e, _) = delete_chars(t, &[Region { anchor: 0, head: 2 }, Region::caret(3)], true);
        assert_eq!(apply(t, &e), "\nd");
    }

    #[test]
    fn paste_distributes_lines_or_repeats() {
        let t = "a b";
        let (e, _) = paste(&[Region::caret(0), Region::caret(2)], "1\n2");
        assert_eq!(apply(t, &e), "1a 2b");
        let (e, _) = paste(&[Region::caret(0), Region::caret(2)], "z");
        assert_eq!(apply(t, &e), "za zb");
    }

    #[test]
    fn motion_and_esc() {
        let t = "ab\ncd";
        let m = move_regions(t, &[Region::caret(1), Region::caret(4)], Motion::End, false);
        assert_eq!(m, vec![Region::caret(2), Region::caret(5)]);
        // moving into each other merges
        let m = move_regions(t, &[Region::caret(0), Region::caret(1)], Motion::Left, false);
        assert_eq!(m, vec![Region::caret(0)]);
        assert_eq!(collapse_to_primary(&[Region::caret(0), Region::caret(4)]), vec![Region::caret(4)]);
    }

    #[test]
    fn indent_touches_each_line_once() {
        let t = "a\nb\nc";
        let (e, _) = indent(t, &[Region::caret(0), Region::caret(1), Region { anchor: 2, head: 5 }], "  ");
        assert_eq!(apply(t, &e), "  a\n  b\n  c");
    }

    #[test]
    fn column_paste_three_lines_into_five_row_block() {
        let t = "11111\n22222\n33333\n44444\n55555";
        let (e, _) = column_paste(t, 0, 2, "A\nB\nC", None);
        assert_eq!(apply(t, &e), "11A111\n22B222\n33C333\n44444\n55555");
    }

    #[test]
    fn column_paste_pads_short_rows_and_appends() {
        let (e, _) = column_paste("ab\n", 0, 4, "X\nY\nZ", None);
        assert_eq!(apply("ab\n", &e), "ab  X\n    Y\n    Z");
        let (e, _) = column_paste("a\nb\nc", 0, 1, "-", Some(2));
        assert_eq!(apply("a\nb\nc", &e), "a-\nb-\nc-");
    }

    #[test]
    fn column_insert_delete_and_virtual_space() {
        let t = "abcd\nx\nabcd";
        let rect = ColumnRect::from_corners((0, 3), (2, 3));
        let (e, _) = column_insert(t, rect, "|", false);
        assert_eq!(apply(t, &e), "abc|d\nx|\nabc|d");
        let (e, _) = column_insert(t, rect, "|", true);
        assert_eq!(apply(t, &e), "abc|d\nx  |\nabc|d");
        let (e, c) = column_delete(t, ColumnRect::from_corners((0, 1), (2, 3)));
        assert_eq!(apply(t, &e), "ad\nx\nad");
        assert_eq!(c.len(), 3);
    }

    #[test]
    fn smart_home_cycles_and_overwrite() {
        let t = "    foo";
        assert_eq!(smart_home(t, 7), 4);
        assert_eq!(smart_home(t, 4), 0);
        assert_eq!(smart_home(t, 0), 4);
        let (e, _) = overwrite_text("abc", &[Region::caret(1)], "X");
        assert_eq!(apply("abc", &e), "aXc");
        let (e, _) = overwrite_text("ab\ncd", &[Region::caret(2)], "X");
        assert_eq!(apply("ab\ncd", &e), "abX\ncd");
    }

    #[test]
    fn word_range_custom_delims() {
        assert_eq!(word_range("foo-bar baz", 1, ""), Some((0, 7)));
        assert_eq!(word_range("foo-bar baz", 1, "-"), Some((0, 3)));
        assert_eq!(word_range("a  b", 2, ""), None);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use floem::reactive::{create_rw_signal, with_scope, Scope, SignalGet};

    /// Run a closure under a fresh reactive Scope so RwSignals have somewhere
    /// to live. `with_scope` (free fn) is the floem_reactive 0.2 spelling —
    /// `Scope::with` doesn't exist there. The closure's return type must be
    /// `'static` (the trait bound on `with_scope`), so we return `()`.
    fn run<F: FnOnce() + 'static>(f: F) {
        let scope = Scope::new();
        with_scope(scope, f);
        scope.dispose();
    }

    /// Small fixture buffer — 3 lines of 5 chars each, with a trailing
    /// newline so crop reports 4 lines (the trailing "" line).
    fn fixture() -> Buffer {
        Buffer::from_str("alpha\nbeta\ngamma\n")
    }

    #[test]
    fn arrow_up_at_first_line_does_nothing() {
        run(|| {
            let buf = fixture();
            let cursor = create_rw_signal((1usize, 3usize));
            let moved = handle_cursor_key(&Key::Named(NamedKey::ArrowUp), cursor, &buf);
            assert!(moved, "ArrowUp should be a handled key");
            assert_eq!(cursor.get(), (1, 3), "row stays at 1, col untouched");
        });
    }

    #[test]
    fn arrow_down_at_last_line_does_nothing() {
        run(|| {
            let buf = fixture();
            let last = buf.line_count().max(1);
            let cursor = create_rw_signal((last, 1usize));
            let moved = handle_cursor_key(&Key::Named(NamedKey::ArrowDown), cursor, &buf);
            assert!(moved);
            assert_eq!(cursor.get(), (last, 1));
        });
    }

    #[test]
    fn arrow_up_clamps_col_to_line_len() {
        run(|| {
            // "alpha" (5 chars) and "beta" (4 chars). From (line=2, col=5)
            // — last legal col on "beta" is 5 (one past EOL) — ArrowUp lands
            // on "alpha" at col 5 too (also valid). Try col 6 to force clamp.
            // But col=6 is *not* a legal state on line 2 (max=5). Use line 3
            // "gamma" (5 chars, max col=6), col=6, move up to "beta" (max=5).
            let buf = fixture();
            let cursor = create_rw_signal((3usize, 6usize));
            handle_cursor_key(&Key::Named(NamedKey::ArrowUp), cursor, &buf);
            // "beta" has 4 chars → line_len returns 5 (one past EOL).
            assert_eq!(cursor.get(), (2, 5));
        });
    }

    #[test]
    fn arrow_down_clamps_col_to_line_len() {
        run(|| {
            // From "alpha" (5 chars, max col=6) col=6 → move down to "beta"
            // (4 chars, max col=5). Col must clamp to 5.
            let buf = fixture();
            let cursor = create_rw_signal((1usize, 6usize));
            handle_cursor_key(&Key::Named(NamedKey::ArrowDown), cursor, &buf);
            assert_eq!(cursor.get(), (2, 5));
        });
    }

    #[test]
    fn arrow_left_at_col_1_wraps_to_previous_line_end() {
        run(|| {
            let buf = fixture();
            // From "beta", col=1 — wrap to end of "alpha" (col=6).
            let cursor = create_rw_signal((2usize, 1usize));
            handle_cursor_key(&Key::Named(NamedKey::ArrowLeft), cursor, &buf);
            assert_eq!(cursor.get(), (1, 6));
        });
    }

    #[test]
    fn arrow_left_at_first_col_first_line_does_nothing() {
        run(|| {
            let buf = fixture();
            let cursor = create_rw_signal((1usize, 1usize));
            handle_cursor_key(&Key::Named(NamedKey::ArrowLeft), cursor, &buf);
            // No underflow, no movement.
            assert_eq!(cursor.get(), (1, 1));
        });
    }

    #[test]
    fn arrow_right_at_eol_wraps_to_next_line_start() {
        run(|| {
            let buf = fixture();
            // "alpha" max col=6 ; from (1, 6) → wrap to (2, 1).
            let cursor = create_rw_signal((1usize, 6usize));
            handle_cursor_key(&Key::Named(NamedKey::ArrowRight), cursor, &buf);
            assert_eq!(cursor.get(), (2, 1));
        });
    }

    #[test]
    fn arrow_right_at_last_line_eof_does_nothing() {
        run(|| {
            let buf = fixture();
            let last = buf.line_count().max(1);
            // Compute the real max col of the last line via the same path
            // the handler uses : nth_line(last-1).chars().count() + 1. For
            // the trailing empty line crop produces, that's 1.
            let last_line_max = buf
                .nth_line(last - 1)
                .map(|s| s.chars().count() + 1)
                .unwrap_or(1);
            let cursor = create_rw_signal((last, last_line_max));
            handle_cursor_key(&Key::Named(NamedKey::ArrowRight), cursor, &buf);
            assert_eq!(cursor.get(), (last, last_line_max), "no wrap past EOF");
        });
    }

    #[test]
    fn home_jumps_to_col_1() {
        run(|| {
            let buf = fixture();
            let cursor = create_rw_signal((2usize, 4usize));
            handle_cursor_key(&Key::Named(NamedKey::Home), cursor, &buf);
            assert_eq!(cursor.get(), (2, 1));
        });
    }

    #[test]
    fn end_jumps_to_line_end_position() {
        run(|| {
            let buf = fixture();
            // "beta" has 4 chars — End lands at col=5 (one past EOL).
            let cursor = create_rw_signal((2usize, 1usize));
            handle_cursor_key(&Key::Named(NamedKey::End), cursor, &buf);
            assert_eq!(cursor.get(), (2, 5));
        });
    }

    #[test]
    fn page_up_subtracts_page_step_clamped_to_1() {
        run(|| {
            // PAGE_STEP = 20 ; from line 5 we underflow → clamp to line 1.
            let buf = fixture();
            let cursor = create_rw_signal((1usize, 1usize));
            handle_cursor_key(&Key::Named(NamedKey::PageUp), cursor, &buf);
            assert_eq!(cursor.get(), (1, 1));
        });
    }

    #[test]
    fn page_down_adds_page_step_clamped_to_total() {
        run(|| {
            // PAGE_STEP = 20 ; fixture has 4 lines — clamp to last line.
            let buf = fixture();
            let last = buf.line_count().max(1);
            let cursor = create_rw_signal((1usize, 1usize));
            handle_cursor_key(&Key::Named(NamedKey::PageDown), cursor, &buf);
            assert_eq!(cursor.get().0, last);
        });
    }

    #[test]
    fn unknown_key_returns_false() {
        run(|| {
            let buf = fixture();
            let cursor = create_rw_signal((1usize, 1usize));
            let moved = handle_cursor_key(&Key::Character("x".into()), cursor, &buf);
            assert!(!moved, "non-navigation key must return false");
            assert_eq!(cursor.get(), (1, 1), "cursor untouched");
        });
    }

    #[test]
    fn known_key_returns_true() {
        run(|| {
            let buf = fixture();
            let cursor = create_rw_signal((2usize, 2usize));
            // Even a no-op known key (ArrowUp at row 1 would no-op ; here
            // ArrowUp at row 2 actually moves) returns true.
            let moved = handle_cursor_key(&Key::Named(NamedKey::ArrowUp), cursor, &buf);
            assert!(moved);
        });
    }

    #[test]
    fn home_on_empty_buffer_does_not_panic() {
        // Edge case spotted while wiring tests: a buffer constructed from
        // "" has line_count() == 0 ; line_max.max(1) keeps cursor logic
        // safe, but line_len(1) goes through nth_line(0) which returns
        // None → unwrap_or(1). End/Home/PageUp/PageDown must not panic.
        run(|| {
            let buf = Buffer::from_str("");
            let cursor = create_rw_signal((1usize, 1usize));
            handle_cursor_key(&Key::Named(NamedKey::End), cursor, &buf);
            assert_eq!(cursor.get(), (1, 1));
            handle_cursor_key(&Key::Named(NamedKey::Home), cursor, &buf);
            assert_eq!(cursor.get(), (1, 1));
            handle_cursor_key(&Key::Named(NamedKey::PageDown), cursor, &buf);
            assert_eq!(cursor.get().0, 1);
        });
    }

    #[test]
    fn selection_linear_contains() {
        let sel = Selection::new((1, 3), (3, 4), false);
        // Start line
        assert!(!sel.contains(1, 2));
        assert!(sel.contains(1, 3));
        assert!(sel.contains(1, 10));

        // Intervening line
        assert!(sel.contains(2, 1));
        assert!(sel.contains(2, 99));

        // End line
        assert!(sel.contains(3, 1));
        assert!(sel.contains(3, 3));
        assert!(!sel.contains(3, 4));
        assert!(!sel.contains(3, 5));
    }

    #[test]
    fn selection_block_contains() {
        let sel = Selection::new((1, 3), (3, 5), true);
        // On lines 1..3, columns 3..5 should be selected
        assert!(!sel.contains(1, 2));
        assert!(sel.contains(1, 3));
        assert!(sel.contains(1, 4));
        assert!(sel.contains(1, 5));
        assert!(!sel.contains(1, 6));

        assert!(!sel.contains(2, 2));
        assert!(sel.contains(2, 4));

        assert!(!sel.contains(4, 4));
    }

    #[test]
    fn selection_empty_contains() {
        let sel = Selection::new((1, 3), (1, 3), false);
        assert!(!sel.contains(1, 3));
        assert!(!sel.contains(1, 2));
    }

    #[test]
    fn page_up_clamps_at_line_1() {
        run(|| {
            let buf = fixture();
            let cursor = create_rw_signal((3usize, 2usize));
            handle_cursor_key(&Key::Named(NamedKey::PageUp), cursor, &buf);
            assert_eq!(cursor.get().0, 1);
        });
    }

    #[test]
    fn handle_cursor_key_unknown() {
        run(|| {
            let buf = fixture();
            let cursor = create_rw_signal((1usize, 1usize));
            let moved = handle_cursor_key(&Key::Character("a".into()), cursor, &buf);
            assert!(!moved);
        });
    }

    // ---- fold-aware navigation ----

    use crate::folding::{FoldRegion, FoldState};

    /// 8 lines "l0".."l7" (+ trailing empty line); lines 2..=4 are hidden under header 1.
    fn folded_fixture() -> (Buffer, FoldState) {
        let buf = Buffer::from_str("l0
l1
l2
l3
l4
l5
l6
l7
");
        let fs = FoldState::new(vec![FoldRegion { start_line: 1, end_line: 4, indent_level: 0, is_folded: true }]);
        (buf, fs)
    }

    fn key(k: NamedKey) -> Key {
        Key::Named(k)
    }

    #[test]
    fn down_from_a_folded_header_lands_after_the_hidden_block() {
        run(|| {
            let (buf, fs) = folded_fixture();
            let cursor = create_rw_signal((2usize, 2usize)); // header = line 2 (1-based)
            assert!(handle_cursor_key_folded(&key(NamedKey::ArrowDown), cursor, &buf, &fs));
            assert_eq!(cursor.get(), (6, 2), "line 6 is the first visible line after the fold");
        });
    }

    #[test]
    fn up_from_below_a_fold_lands_on_the_header() {
        run(|| {
            let (buf, fs) = folded_fixture();
            let cursor = create_rw_signal((6usize, 1usize));
            handle_cursor_key_folded(&key(NamedKey::ArrowUp), cursor, &buf, &fs);
            assert_eq!(cursor.get(), (2, 1));
            handle_cursor_key_folded(&key(NamedKey::ArrowUp), cursor, &buf, &fs);
            assert_eq!(cursor.get(), (1, 1));
        });
    }

    #[test]
    fn a_caret_inside_a_fold_moves_to_the_header_first() {
        run(|| {
            let (buf, fs) = folded_fixture();
            // caret on hidden line 4: Down goes header(2) -> 6, Up goes header(2) -> 1
            let cursor = create_rw_signal((4usize, 2usize));
            handle_cursor_key_folded(&key(NamedKey::ArrowDown), cursor, &buf, &fs);
            assert_eq!(cursor.get().0, 6);
            let cursor = create_rw_signal((4usize, 2usize));
            handle_cursor_key_folded(&key(NamedKey::ArrowUp), cursor, &buf, &fs);
            assert_eq!(cursor.get().0, 1);
            // Home / End act on the visible line, never on the hidden one
            let cursor = create_rw_signal((3usize, 2usize));
            handle_cursor_key_folded(&key(NamedKey::Home), cursor, &buf, &fs);
            assert_eq!(cursor.get(), (2, 1));
            let cursor = create_rw_signal((3usize, 1usize));
            handle_cursor_key_folded(&key(NamedKey::End), cursor, &buf, &fs);
            assert_eq!(cursor.get().0, 2);
        });
    }

    #[test]
    fn left_and_right_at_line_edges_skip_the_hidden_lines() {
        run(|| {
            let (buf, fs) = folded_fixture();
            let end_of_header = buf.nth_line(1).map(|s| s.chars().count() + 1).unwrap();
            let cursor = create_rw_signal((2usize, end_of_header));
            handle_cursor_key_folded(&key(NamedKey::ArrowRight), cursor, &buf, &fs);
            assert_eq!(cursor.get(), (6, 1));
            handle_cursor_key_folded(&key(NamedKey::ArrowLeft), cursor, &buf, &fs);
            assert_eq!(cursor.get(), (2, end_of_header));
        });
    }

    #[test]
    fn page_keys_count_visible_rows_only() {
        run(|| {
            let (buf, fs) = folded_fixture();
            let cursor = create_rw_signal((1usize, 1usize));
            handle_cursor_key_folded(&key(NamedKey::PageDown), cursor, &buf, &fs);
            let last = buf.line_count();
            assert_eq!(cursor.get().0, last, "clamped to the last line");
            handle_cursor_key_folded(&key(NamedKey::PageUp), cursor, &buf, &fs);
            assert_eq!(cursor.get().0, 1);
        });
    }

    #[test]
    fn without_folds_the_folded_handler_equals_the_plain_one() {
        run(|| {
            let buf = fixture();
            for k in [NamedKey::ArrowUp, NamedKey::ArrowDown, NamedKey::ArrowLeft, NamedKey::ArrowRight, NamedKey::Home, NamedKey::End, NamedKey::PageUp, NamedKey::PageDown] {
                for start in [(1usize, 1usize), (2, 3), (3, 6), (4, 1)] {
                    let a = create_rw_signal(start);
                    let b = create_rw_signal(start);
                    handle_cursor_key(&key(k), a, &buf);
                    handle_cursor_key_folded(&key(k), b, &buf, &FoldState::empty());
                    assert_eq!(a.get(), b.get());
                }
            }
        });
    }
}
