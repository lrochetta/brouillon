//! BK-027 line operations and BK-031 line filters.

use super::*;
use brouillon_core::contract::LineSet;
use std::collections::HashSet;

pub fn commands() -> Vec<CommandSpec> {
    [
        ("delete_line", "Edit: Delete Line"),
        ("delete_word", "Edit: Delete Word"),
        ("delete_word_back", "Edit: Delete Word Backward"),
        ("delete_to_line_end", "Edit: Delete to End of Line"),
        ("delete_to_line_start", "Edit: Delete to Start of Line"),
        ("duplicate_line", "Edit: Duplicate Line or Selection"),
        ("move_line_up", "Edit: Move Line Up"),
        ("move_line_down", "Edit: Move Line Down"),
        ("join_lines", "Edit: Join Lines"),
        ("join_lines_space", "Edit: Join Lines With Space"),
        ("split_lines", "Edit: Split Line at Caret or Column"),
        ("transpose_chars", "Edit: Transpose Characters"),
        ("transpose_lines", "Edit: Transpose Lines"),
        ("insert_line_above", "Edit: Insert Blank Line Above"),
        ("insert_line_below", "Edit: Insert Blank Line Below"),
        ("reverse_lines", "Edit: Reverse Lines"),
        ("delete_duplicate_lines", "Lines: Delete Duplicate Lines"),
        ("delete_consecutive_duplicate_lines", "Lines: Delete Consecutive Duplicate Lines"),
        ("delete_blank_lines", "Lines: Delete Blank Lines"),
        ("delete_whitespace_only_lines", "Lines: Delete Blank and Whitespace-only Lines"),
    ]
    .iter()
    .map(|(id, cap)| spec(id, cap, "Edit"))
    .collect()
}

pub fn run(id: &str, ctx: &CmdCtx) -> Option<CmdOutcome> {
    let t = ctx.text;
    let ls = split_lines(t);
    let sel = ctx.selection;
    Some(match id {
        "delete_line" => outcome(t, delete_lines(t, &ls, &selected_lines(&ls, sel))),
        "delete_word" => delete_ranges(t, sel, |t, p| {
            let c = t[p..].chars().next()?;
            let class = |c: char| if is_word(c) { 0 } else if c.is_whitespace() && c != '\n' && c != '\r' { 1 } else { 2 };
            let k = class(c);
            let n: usize = t[p..].chars().take_while(|&d| class(d) == k && (k != 2 || d == c) && d != '\n' && d != '\r').map(char::len_utf8).sum();
            Some((p, p + if n == 0 { c.len_utf8() } else { n }))
        }),
        "delete_word_back" => delete_ranges(t, sel, |t, p| {
            let c = t[..p].chars().next_back()?;
            let class = |c: char| if is_word(c) { 0 } else if c.is_whitespace() && c != '\n' && c != '\r' { 1 } else { 2 };
            let k = class(c);
            let n: usize = t[..p].chars().rev().take_while(|&d| class(d) == k && (k != 2 || d == c) && d != '\n' && d != '\r').map(char::len_utf8).sum();
            Some((p - if n == 0 { c.len_utf8() } else { n }, p))
        }),
        "delete_to_line_end" => delete_ranges(t, sel, |t, p| {
            let l = &ls[line_of(&ls, p)];
            // at the end of a line, join with the next one
            Some((p, if p >= l.end { l.next.min(t.len()) } else { l.end }))
        }),
        "delete_to_line_start" => delete_ranges(t, sel, |_, p| Some((ls[line_of(&ls, p)].start, p))),
        "duplicate_line" => outcome(t, duplicate(t, &ls, sel)),
        "move_line_up" => move_lines(t, &ls, sel, true),
        "move_line_down" => move_lines(t, &ls, sel, false),
        "transpose_lines" => move_lines(t, &ls, sel, true),
        "join_lines" => outcome(t, join(t, &ls, sel, false)),
        "join_lines_space" => outcome(t, join(t, &ls, sel, true)),
        "split_lines" => outcome(t, split(t, &ls, sel, ctx.args.first().and_then(|a| a.parse().ok()))),
        "transpose_chars" => transpose_chars(t, &ls, sel),
        "insert_line_above" => outcome(t, selected_lines(&ls, sel).iter().map(|&i| TextEdit::insert(ls[i].start, eol_of(t))).collect()),
        "insert_line_below" => outcome(t, selected_lines(&ls, sel).iter().map(|&i| TextEdit::insert(ls[i].end, eol_of(t))).collect()),
        "reverse_lines" => {
            let (a, b) = target_range(t, &ls, sel, true);
            let mut v: Vec<String> = (a..=b).map(|i| t[ls[i].start..ls[i].end].to_string()).collect();
            v.reverse();
            outcome(t, vec![replace_lines(t, &ls, a, b, &v)])
        }
        "delete_duplicate_lines" => outcome(t, dedupe_lines(t, &ls, sel, false)),
        "delete_consecutive_duplicate_lines" => outcome(t, dedupe_lines(t, &ls, sel, true)),
        "delete_blank_lines" | "delete_whitespace_only_lines" => {
            let (a, b) = target_range(t, &ls, sel, true);
            let strict = id == "delete_blank_lines";
            let hit: Vec<usize> = (a..=b).filter(|&i| {
                let s = &t[ls[i].start..ls[i].end];
                if strict { s.is_empty() } else { s.trim().is_empty() }
            }).collect();
            outcome(t, delete_lines(t, &ls, &hit))
        }
        _ => return None,
    })
}

/// Edits deleting whole lines (eol included). Deleting the final line without eol also
/// removes the eol before it so no dangling blank line remains.
pub fn delete_lines(text: &str, ls: &[Line], lines: &[usize]) -> Vec<TextEdit> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < lines.len() {
        let mut j = i;
        while j + 1 < lines.len() && lines[j + 1] == lines[j] + 1 {
            j += 1;
        }
        let (first, last) = (lines[i], lines[j]);
        let mut start = ls[first].start;
        if ls[last].next == text.len() && ls[last].end == text.len() && first > 0 {
            start = ls[first - 1].end;
        }
        out.push(TextEdit::delete(start, ls[last].next));
        i = j + 1;
    }
    out
}

/// BK-031: remove exactly the lines of `set` (`invert` = remove all the others).
pub fn delete_lineset(text: &str, set: &LineSet, invert: bool) -> CmdOutcome {
    let ls = split_lines(text);
    let hit: Vec<usize> = (0..ls.len()).filter(|&i| set.contains(i) != invert).collect();
    outcome(text, delete_lines(text, &ls, &hit))
}

/// BK-031: text of the `set` lines joined in order (cut = also returns the deletion edits).
pub fn copy_lineset(text: &str, set: &LineSet) -> String {
    let ls = split_lines(text);
    let v: Vec<&str> = set.0.iter().filter(|&&i| i < ls.len()).map(|&i| &text[ls[i].start..ls[i].end]).collect();
    v.join(eol_of(text))
}

pub fn cut_lineset(text: &str, set: &LineSet) -> (String, CmdOutcome) {
    (copy_lineset(text, set), delete_lineset(text, set, false))
}

fn dedupe_lines(t: &str, ls: &[Line], sel: &[Region], consecutive: bool) -> Vec<TextEdit> {
    let (a, b) = target_range(t, ls, sel, true);
    let mut seen = HashSet::new();
    let mut hit = Vec::new();
    for i in a..=b {
        let s = &t[ls[i].start..ls[i].end];
        let dup = if consecutive { i > a && s == &t[ls[i - 1].start..ls[i - 1].end] } else { !seen.insert(s) };
        if dup {
            hit.push(i);
        }
    }
    delete_lines(t, ls, &hit)
}

fn delete_ranges(t: &str, sel: &[Region], f: impl Fn(&str, usize) -> Option<(usize, usize)>) -> CmdOutcome {
    outcome(t, sel.iter().filter_map(|r| if r.is_caret() { f(t, r.head) } else { Some((r.start(), r.end())) }).map(|(a, b)| TextEdit::delete(a, b)).collect())
}

fn duplicate(t: &str, ls: &[Line], sel: &[Region]) -> Vec<TextEdit> {
    let eol = eol_of(t);
    let mut edits = Vec::new();
    let caret_lines: Vec<usize> = sel.iter().filter(|r| r.is_caret()).flat_map(|r| selected_lines(ls, &[*r])).collect();
    for r in sel.iter().filter(|r| !r.is_caret()) {
        edits.push(TextEdit::insert(r.end(), &t[r.start()..r.end()]));
    }
    let mut lines = if sel.is_empty() { vec![0] } else { caret_lines };
    lines.sort_unstable();
    lines.dedup();
    let mut i = 0;
    while i < lines.len() {
        let mut j = i;
        while j + 1 < lines.len() && lines[j + 1] == lines[j] + 1 {
            j += 1;
        }
        let (a, b) = (lines[i], lines[j]);
        if ls[b].next > ls[b].end {
            edits.push(TextEdit::insert(ls[b].next, &t[ls[a].start..ls[b].next]));
        } else {
            edits.push(TextEdit::insert(ls[b].end, format!("{eol}{}", &t[ls[a].start..ls[b].end])));
        }
        i = j + 1;
    }
    edits
}

fn move_lines(t: &str, ls: &[Line], sel: &[Region], up: bool) -> CmdOutcome {
    let (a, b) = target_range(t, ls, sel, false);
    if (up && a == 0) || (!up && (b + 1 >= ls.len() || (b + 2 == ls.len() && ls[b + 1].start == t.len()))) {
        return CmdOutcome::default();
    }
    let text_of = |i: usize| t[ls[i].start..ls[i].end].to_string();
    let (lo, hi) = if up { (a - 1, b) } else { (a, b + 1) };
    let mut v: Vec<String> = Vec::new();
    let new_run_start;
    if up {
        v.extend((a..=b).map(text_of));
        v.push(text_of(a - 1));
        new_run_start = ls[a - 1].start;
    } else {
        v.push(text_of(b + 1));
        v.extend((a..=b).map(text_of));
        new_run_start = ls[a].start + t[ls[b + 1].start..ls[b + 1].end].len() + (ls[a].next - ls[a].end);
    }
    let shift = new_run_start as isize - ls[a].start as isize;
    let sel_out = sel.iter().map(|r| Region { anchor: (r.anchor as isize + shift) as usize, head: (r.head as isize + shift) as usize }).collect();
    let mut o = outcome(t, vec![replace_lines(t, ls, lo, hi, &v)]);
    if !o.edits.is_empty() {
        o.selection = Some(sel_out);
    }
    o
}

fn join(t: &str, ls: &[Line], sel: &[Region], space: bool) -> Vec<TextEdit> {
    let (a, mut b) = target_range(t, ls, sel, false);
    if a == b {
        b = (a + 1).min(ls.len() - 1);
    }
    (a..b)
        .map(|i| {
            let next_content = &t[ls[i + 1].start..ls[i + 1].end];
            let lead = next_content.len() - next_content.trim_start().len();
            if space {
                let join = if t[ls[i].start..ls[i].end].is_empty() || next_content.trim().is_empty() { "" } else { " " };
                TextEdit::new(ls[i].end, ls[i + 1].start + lead, join)
            } else {
                TextEdit::delete(ls[i].end, ls[i].next)
            }
        })
        .collect()
}

fn split(t: &str, ls: &[Line], sel: &[Region], col: Option<usize>) -> Vec<TextEdit> {
    let eol = eol_of(t);
    match col {
        Some(c) if c > 0 => {
            let (a, b) = target_range(t, ls, sel, false);
            (a..=b)
                .flat_map(|i| {
                    let n = t[ls[i].start..ls[i].end].chars().count();
                    (1..).map(move |k| k * c).take_while(move |&p| p < n).map(move |p| (i, p))
                })
                .map(|(i, p)| TextEdit::insert(byte_at_column(t, ls[i], p), eol))
                .collect()
        }
        _ => sel.iter().map(|r| TextEdit::new(r.start(), r.end(), eol)).collect(),
    }
}

fn transpose_chars(t: &str, ls: &[Line], sel: &[Region]) -> CmdOutcome {
    let mut edits = Vec::new();
    let mut carets = Vec::new();
    for r in sel.iter().filter(|r| r.is_caret()) {
        let l = ls[line_of(ls, r.head)];
        // at end of line, swap the two chars before the caret
        let p = if r.head >= l.end { t[l.start..l.end].char_indices().next_back().map_or(r.head, |(i, _)| l.start + i) } else { r.head };
        let (Some(before), Some(after)) = (t[l.start..p].chars().next_back(), t[p..l.end].chars().next()) else { continue };
        let (s, e) = (p - before.len_utf8(), p + after.len_utf8());
        edits.push(TextEdit::new(s, e, format!("{after}{before}")));
        carets.push(Region::caret(e));
    }
    let mut o = outcome(t, edits);
    if !o.edits.is_empty() {
        o.selection = Some(carets);
    }
    o
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::textops::{apply, sel};

    fn c(at: usize) -> Vec<Region> {
        vec![Region::caret(at)]
    }

    #[test]
    fn delete_line_cases() {
        assert_eq!(apply("delete_line", "a\nb\nc\n", &c(2), &[]), "a\nc\n");
        // last line without eol takes the previous eol with it
        assert_eq!(apply("delete_line", "a\nb", &c(3), &[]), "a");
        assert_eq!(apply("delete_line", "a\r\nb\r\nc", &c(0), &[]), "b\r\nc");
        // selection ending at column 0 does not delete the next line
        assert_eq!(apply("delete_line", "a\nb\nc", &sel(0, 2), &[]), "b\nc");
        assert_eq!(apply("delete_line", "only", &c(1), &[]), "");
    }

    #[test]
    fn delete_word_and_ends() {
        assert_eq!(apply("delete_word", "foo bar", &c(0), &[]), " bar");
        assert_eq!(apply("delete_word_back", "foo bar", &c(7), &[]), "foo ");
        assert_eq!(apply("delete_to_line_end", "abc\ndef", &c(1), &[]), "a\ndef");
        assert_eq!(apply("delete_to_line_end", "abc\ndef", &c(3), &[]), "abcdef");
        assert_eq!(apply("delete_to_line_start", "abc\ndef", &c(5), &[]), "abc\nef");
    }

    #[test]
    fn duplicate_cases() {
        assert_eq!(apply("duplicate_line", "a\nb\n", &c(0), &[]), "a\na\nb\n");
        assert_eq!(apply("duplicate_line", "a\nb", &c(2), &[]), "a\nb\nb");
        assert_eq!(apply("duplicate_line", "a\r\nb", &c(0), &[]), "a\r\na\r\nb");
        assert_eq!(apply("duplicate_line", "abcd", &sel(1, 3), &[]), "abcbcd");
        assert_eq!(apply("duplicate_line", "a\nb\nc", &sel(0, 3), &[]), "a\nba\nb\nc");
    }

    #[test]
    fn move_lines_edges() {
        assert_eq!(apply("move_line_up", "a\nb\nc", &c(2), &[]), "b\na\nc");
        assert_eq!(apply("move_line_up", "a\nb", &c(0), &[]), "a\nb"); // no-op at line 0
        assert_eq!(apply("move_line_down", "a\nb\nc", &c(0), &[]), "b\na\nc");
        assert_eq!(apply("move_line_down", "a\nb", &c(2), &[]), "a\nb"); // no-op on last
        assert_eq!(apply("move_line_down", "a\r\nb\r\nc\r\n", &c(0), &[]), "b\r\na\r\nc\r\n");
        assert_eq!(apply("move_line_up", "a\nb\nc\nd", &sel(4, 7), &[]), "a\nc\nd\nb");
        let ctx = CmdCtx { text: "a\nb", selection: &c(2), path: None, args: &[] };
        let o = run("move_line_up", &ctx).unwrap();
        assert_eq!(o.selection, Some(c(0)));
    }

    #[test]
    fn join_split_transpose() {
        assert_eq!(apply("join_lines", "a\nb\nc", &c(0), &[]), "ab\nc");
        assert_eq!(apply("join_lines_space", "a\n   b\nc", &sel(0, 8), &[]), "a b c");
        assert_eq!(apply("join_lines", "a\r\nb", &c(0), &[]), "ab");
        assert_eq!(apply("split_lines", "abcd", &c(2), &[]), "ab\ncd");
        assert_eq!(apply("split_lines", "abcdefg", &c(0), &["3"]), "abc\ndef\ng");
        assert_eq!(apply("transpose_chars", "abc", &c(1), &[]), "bac");
        assert_eq!(apply("transpose_chars", "abc", &c(3), &[]), "acb");
        assert_eq!(apply("transpose_lines", "a\nb", &c(2), &[]), "b\na");
    }

    #[test]
    fn insert_and_reverse() {
        assert_eq!(apply("insert_line_above", "a\nb", &c(2), &[]), "a\n\nb");
        assert_eq!(apply("insert_line_below", "a\nb", &c(2), &[]), "a\nb\n");
        assert_eq!(apply("insert_line_below", "a\nb", &c(0), &[]), "a\n\nb");
        assert_eq!(apply("reverse_lines", "a\nb\nc", &c(0), &[]), "c\nb\na");
        assert_eq!(apply("reverse_lines", "a\r\nb\r\n", &c(0), &[]), "b\r\na\r\n");
    }

    #[test]
    fn filters() {
        assert_eq!(apply("delete_duplicate_lines", "a\nb\na\nc\nb", &c(0), &[]), "a\nb\nc");
        assert_eq!(apply("delete_consecutive_duplicate_lines", "a\na\nb\na", &c(0), &[]), "a\nb\na");
        assert_eq!(apply("delete_blank_lines", "a\n\n  \nb\n", &c(0), &[]), "a\n  \nb\n");
        assert_eq!(apply("delete_whitespace_only_lines", "a\n\n  \nb", &c(0), &[]), "a\nb");
        let t = "a\nb\nc\nd";
        let set = LineSet::from_unsorted(vec![3, 1]);
        assert_eq!(delete_lineset(t, &set, false).edits.apply_to_str(t).unwrap(), "a\nc");
        assert_eq!(delete_lineset(t, &set, true).edits.apply_to_str(t).unwrap(), "b\nd");
        let (copied, o) = cut_lineset(t, &set);
        assert_eq!(copied, "b\nd");
        assert_eq!(o.edits.apply_to_str(t).unwrap(), "a\nc");
    }
}
