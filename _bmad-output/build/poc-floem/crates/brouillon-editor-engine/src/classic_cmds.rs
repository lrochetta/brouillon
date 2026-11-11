//! Classic-preset commands that are pure functions of text + selection:
//! line/word selection, selection duplicate, word-under-caret search, go to
//! column, bracket matching, column paste and the date/time inserts.
//!
//! Same contract as `selection_cmds`: every command is a function of `CmdCtx`
//! returning a `CmdOutcome`. Time-based inserts are UTC (same as `insert_datetime`).

use brouillon_core::contract::{CmdCtx, CmdOutcome, CommandSpec, EditSet, Region, TextEdit};

use crate::cursor::{byte_to_line_col, line_col_to_byte, line_starts, normalize, word_range};
use crate::selection_cmds::{is_whole_word, select_all_occurrences, WORD_DELIMS};
use crate::textops::transform::format_datetime;

pub fn commands() -> Vec<CommandSpec> {
    let c = |id: &str, cap: &str, cat: &str| CommandSpec { id: id.into(), caption: cap.into(), category: cat.into(), ..Default::default() };
    let mut v = vec![
        c("edit_paste_column", "Edit: Paste Column", "Edit"),
        c("select_line", "Edit: Select Line", "Edit"),
        c("select_word", "Edit: Select Word", "Edit"),
        c("select_word_all", "Edit: Select Word (all occurrences)", "Edit"),
        c("selection_duplicate", "Edit: Duplicate Selection", "Edit"),
        c("search_find_next_word", "Search: Find Next Word Under Caret", "Search"),
        c("search_find_prev_word", "Search: Find Previous Word Under Caret", "Search"),
        c("goto_column", "Navigate: Go to Column", "Navigate"),
        c("bracket_match", "Navigate: Go to Matching Bracket", "Navigate"),
        c("bracket_select", "Navigate: Select to Matching Bracket", "Navigate"),
        c("insert_time_short", "Insert: Time (short)", "Insert"),
        c("insert_time_long", "Insert: Time (long)", "Insert"),
        c("insert_date_long", "Insert: Date (long)", "Insert"),
    ];
    for s in &mut v {
        s.mutates = matches!(s.id.as_str(), "edit_paste_column" | "selection_duplicate" | "insert_time_short" | "insert_time_long" | "insert_date_long");
        if s.id == "goto_column" {
            s.args_schema = Some(r#"{"type":"array","items":{"type":"string"},"minItems":1,"description":"[column, 1-based]"}"#.into());
        }
        if s.id == "edit_paste_column" {
            s.args_schema = Some(r#"{"type":"array","items":{"type":"string"},"minItems":1,"description":"[clipboard text]"}"#.into());
        }
    }
    v
}

fn sel_out(r: Vec<Region>) -> Option<CmdOutcome> {
    Some(CmdOutcome { selection: Some(r), ..Default::default() })
}

fn msg(m: &str) -> Option<CmdOutcome> {
    Some(CmdOutcome { message: Some(m.into()), ..Default::default() })
}

pub fn run(id: &str, ctx: &CmdCtx) -> Option<CmdOutcome> {
    let (text, sel) = (ctx.text, ctx.selection);
    match id {
        "select_line" => sel_out(select_line(text, sel)),
        "select_word" => match select_word(text, sel) {
            Some(r) => sel_out(r),
            None => msg("No word at the caret"),
        },
        "select_word_all" => match select_word(text, sel) {
            Some(r) => sel_out(select_all_occurrences(text, &r)),
            None => msg("No word at the caret"),
        },
        "selection_duplicate" => match selection_duplicate(text, sel) {
            Some((edits, after)) => Some(CmdOutcome { edits, selection: Some(after), ..Default::default() }),
            None => msg("Select some text first"),
        },
        "search_find_next_word" | "search_find_prev_word" => match find_word(text, sel, id == "search_find_next_word") {
            Some(r) => sel_out(vec![r]),
            None => msg("No word at the caret"),
        },
        "goto_column" => match ctx.args.first().and_then(|a| a.trim().parse::<usize>().ok()).filter(|c| *c >= 1) {
            Some(col) => {
                let head = sel.last().map_or(0, |r| r.head);
                let (line, _) = byte_to_line_col(text, head);
                sel_out(vec![Region::caret(line_col_to_byte(text, line, col - 1))])
            }
            None => msg("Go to column: give a column number (1-based)"),
        },
        "bracket_match" => match match_bracket(text, sel) {
            Some((_, m)) => sel_out(vec![Region::caret(m)]),
            None => msg("No bracket at the caret"),
        },
        "bracket_select" => match match_bracket(text, sel) {
            Some((a, b)) => {
                let (s, e) = (a.min(b), a.max(b));
                // Inclusive of both bracket characters (all ASCII, 1 byte).
                sel_out(vec![Region { anchor: s, head: e + 1 }])
            }
            None => msg("No bracket at the caret"),
        },
        "edit_paste_column" => match ctx.args.first() {
            Some(clip) if !clip.is_empty() => paste_column(text, sel, clip).map(|edits| CmdOutcome { edits, ..Default::default() }),
            _ => msg("Clipboard is empty"),
        },
        "insert_time_short" => Some(insert_stamp(text, sel, "%H:%M")),
        "insert_time_long" => Some(insert_stamp(text, sel, "%H:%M:%S")),
        "insert_date_long" => Some(insert_stamp(text, sel, "%A, %B %d, %Y")),
        _ => None,
    }
}

/// Whole lines (EOL included) covered by each region.
pub fn select_line(text: &str, sel: &[Region]) -> Vec<Region> {
    let starts = line_starts(text);
    let line_of = |b: usize| starts.partition_point(|&s| s <= b) - 1;
    let out: Vec<Region> = sel
        .iter()
        .map(|r| {
            let (s, mut e) = (r.start(), r.end());
            // A selection ending exactly at a line start does not include that line.
            if e > s && starts.binary_search(&e).is_ok() {
                e -= 1;
            }
            let (l1, l2) = (line_of(s), line_of(e));
            Region { anchor: starts[l1], head: starts.get(l2 + 1).copied().unwrap_or(text.len()) }
        })
        .collect();
    normalize(&out)
}

/// The word under each caret (selections are kept). `None` when no caret is on a word.
pub fn select_word(text: &str, sel: &[Region]) -> Option<Vec<Region>> {
    let mut changed = false;
    let out: Vec<Region> = sel
        .iter()
        .map(|r| {
            if !r.is_caret() {
                return *r;
            }
            match word_range(text, r.head, WORD_DELIMS) {
                Some((s, e)) => {
                    changed = true;
                    Region { anchor: s, head: e }
                }
                None => *r,
            }
        })
        .collect();
    (changed || sel.iter().any(|r| !r.is_caret())).then(|| normalize(&out))
}

/// Insert a copy of every selected text right after it; the copies end up selected.
pub fn selection_duplicate(text: &str, sel: &[Region]) -> Option<(EditSet, Vec<Region>)> {
    let regions: Vec<Region> = normalize(sel).into_iter().filter(|r| !r.is_caret()).collect();
    if regions.is_empty() {
        return None;
    }
    let mut edits = Vec::new();
    let mut after = Vec::new();
    let mut shift = 0usize;
    for r in &regions {
        let copy = &text[r.start()..r.end()];
        edits.push(TextEdit::insert(r.end(), copy));
        after.push(Region { anchor: r.end() + shift, head: r.end() + shift + copy.len() });
        shift += copy.len();
    }
    EditSet::from_unsorted(edits).ok().map(|e| (e, after))
}

/// Next/previous occurrence (wrapping) of the primary selection or the word under the caret.
pub fn find_word(text: &str, sel: &[Region], forward: bool) -> Option<Region> {
    let p = *sel.last()?;
    let (s, e) = if p.is_caret() { word_range(text, p.head, WORD_DELIMS)? } else { (p.start(), p.end()) };
    let needle = &text[s..e];
    let whole = is_whole_word(text, s, e);
    let hits: Vec<usize> =
        text.match_indices(needle).map(|(i, _)| i).filter(|&i| !whole || is_whole_word(text, i, i + needle.len())).collect();
    let pick = if forward {
        hits.iter().copied().find(|&i| i >= e).or_else(|| hits.first().copied())
    } else {
        hits.iter().copied().rev().find(|&i| i < s).or_else(|| hits.last().copied())
    }?;
    Some(Region { anchor: pick, head: pick + needle.len() })
}

/// Byte offsets (bracket at the caret, its partner). The bracket right after the
/// caret wins over the one right before it. Pure bracket counting: brackets
/// inside strings and comments are not skipped.
pub fn match_bracket(text: &str, sel: &[Region]) -> Option<(usize, usize)> {
    let head = sel.last()?.head;
    let b = text.as_bytes();
    let at = |i: usize| b.get(i).copied();
    let origin = [Some(head), head.checked_sub(1)].into_iter().flatten().find(|&i| matches!(at(i), Some(b'(' | b')' | b'[' | b']' | b'{' | b'}')))?;
    let c = b[origin];
    let (open, close, fwd) = match c {
        b'(' => (b'(', b')', true),
        b'[' => (b'[', b']', true),
        b'{' => (b'{', b'}', true),
        b')' => (b'(', b')', false),
        b']' => (b'[', b']', false),
        _ => (b'{', b'}', false),
    };
    let mut depth = 0usize;
    if fwd {
        for (i, &x) in b.iter().enumerate().skip(origin) {
            if x == open {
                depth += 1;
            } else if x == close {
                depth -= 1;
                if depth == 0 {
                    return Some((origin, i));
                }
            }
        }
    } else {
        for i in (0..=origin).rev() {
            if b[i] == close {
                depth += 1;
            } else if b[i] == open {
                depth -= 1;
                if depth == 0 {
                    return Some((origin, i));
                }
            }
        }
    }
    None
}

/// Paste `clip` as a column block: line i of the clip goes at the primary
/// caret's column on line (caret line + i). Short lines are padded with spaces,
/// missing lines are appended.
pub fn paste_column(text: &str, sel: &[Region], clip: &str) -> Option<EditSet> {
    let head = sel.last().map_or(0, |r| r.start());
    let (line0, col) = byte_to_line_col(text, head);
    let rows: Vec<&str> = clip.strip_suffix('\n').unwrap_or(clip).split('\n').map(|l| l.strip_suffix('\r').unwrap_or(l)).collect();
    let starts = line_starts(text);
    let eol = if text.contains("\r\n") { "\r\n" } else { "\n" };
    let mut edits = Vec::new();
    let mut tail = String::new();
    for (i, row) in rows.iter().enumerate() {
        let line = line0 + i;
        if line < starts.len() {
            let end = starts.get(line + 1).map_or(text.len(), |n| n - 1);
            let end = if end > starts[line] && text.as_bytes()[end - 1] == b'\r' { end - 1 } else { end };
            let len = text[starts[line]..end].chars().count();
            if len >= col {
                edits.push(TextEdit::insert(line_col_to_byte(text, line, col), *row));
            } else {
                edits.push(TextEdit::insert(end, format!("{}{row}", " ".repeat(col - len))));
            }
        } else {
            tail.push_str(&format!("{eol}{}{row}", " ".repeat(col)));
        }
    }
    if !tail.is_empty() {
        edits.push(TextEdit::insert(text.len(), tail));
    }
    EditSet::from_unsorted(edits).ok()
}

fn insert_stamp(text: &str, sel: &[Region], pattern: &str) -> CmdOutcome {
    let secs = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs() as i64);
    stamp_at(text, sel, pattern, secs)
}

fn stamp_at(text: &str, sel: &[Region], pattern: &str, secs: i64) -> CmdOutcome {
    let s = format_datetime(pattern, secs);
    let regions = normalize(sel);
    let edits: Vec<TextEdit> = regions.iter().map(|r| TextEdit::new(r.start(), r.end(), s.as_str())).collect();
    debug_assert!(edits.iter().all(|e| e.end_byte <= text.len()));
    CmdOutcome { edits: EditSet::from_unsorted(edits).unwrap_or_default(), ..Default::default() }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn c(at: usize) -> Region {
        Region::caret(at)
    }
    fn r(a: usize, h: usize) -> Region {
        Region { anchor: a, head: h }
    }
    fn ctx<'a>(text: &'a str, sel: &'a [Region], args: &'a [String]) -> CmdCtx<'a> {
        CmdCtx { text, selection: sel, path: None, args }
    }

    #[test]
    fn select_line_takes_eol_and_handles_last_line() {
        assert_eq!(select_line("ab\ncd\nef", &[c(4)]), vec![r(3, 6)]);
        assert_eq!(select_line("ab\ncd\nef", &[c(7)]), vec![r(6, 8)]);
        // multi-line selection ending at a line start stops before that line
        assert_eq!(select_line("ab\ncd\nef", &[r(1, 6)]), vec![r(0, 6)]);
        // two carets on the same line merge
        assert_eq!(select_line("ab\ncd", &[c(0), c(1)]), vec![r(0, 3)]);
    }

    #[test]
    fn select_word_and_all() {
        let t = "foo bar foo_x foo";
        assert_eq!(select_word(t, &[c(1)]), Some(vec![r(0, 3)]));
        assert_eq!(select_word(t, &[c(3)]), Some(vec![r(0, 3)]), "caret at the word end");
        assert_eq!(select_word("a  b", &[c(2)]), None);
        let out = run("select_word_all", &ctx(t, &[c(1)], &[])).unwrap().selection.unwrap();
        assert_eq!(out, vec![r(0, 3), r(14, 17)], "whole words only");
    }

    #[test]
    fn duplicate_selection_copies_after_and_selects_copy() {
        let t = "ab cd";
        let (e, after) = selection_duplicate(t, &[r(0, 2)]).unwrap();
        assert_eq!(e.apply_to_str(t).unwrap(), "abab cd");
        assert_eq!(after, vec![r(2, 4)]);
        let (e, after) = selection_duplicate(t, &[r(0, 2), r(3, 5)]).unwrap();
        let new = e.apply_to_str(t).unwrap();
        assert_eq!(new, "abab cdcd");
        assert_eq!(&new[after[1].start()..after[1].end()], "cd");
        assert_eq!(after[1].start(), 7);
        assert!(selection_duplicate(t, &[c(1)]).is_none());
    }

    #[test]
    fn find_word_wraps_both_ways() {
        let t = "foo x foo y foo";
        assert_eq!(find_word(t, &[c(1)], true), Some(r(6, 9)));
        assert_eq!(find_word(t, &[r(12, 15)], true), Some(r(0, 3)), "wrap forward");
        assert_eq!(find_word(t, &[c(1)], false), Some(r(12, 15)), "wrap backward");
        assert_eq!(find_word(t, &[r(6, 9)], false), Some(r(0, 3)));
        assert_eq!(find_word("a  b", &[c(2)], true), None);
        // a word that only occurs inside longer words is not matched as whole word
        assert_eq!(find_word("ab abc ab", &[c(0)], true), Some(r(7, 9)));
    }

    #[test]
    fn goto_column_is_one_based_and_clamped() {
        let t = "abc\ndefgh";
        let a = |s: &str| vec![s.to_string()];
        let o = run("goto_column", &ctx(t, &[c(5)], &a("3"))).unwrap();
        assert_eq!(o.selection, Some(vec![c(6)]));
        let o = run("goto_column", &ctx(t, &[c(5)], &a("99"))).unwrap();
        assert_eq!(o.selection, Some(vec![c(9)]));
        assert!(run("goto_column", &ctx(t, &[c(0)], &a("0"))).unwrap().message.is_some());
        assert!(run("goto_column", &ctx(t, &[c(0)], &[])).unwrap().message.is_some());
    }

    #[test]
    fn bracket_matching_nested_and_select() {
        let t = "f(a[1], (b)) { x }";
        // caret before '(' at 1 -> its partner ')' at 11
        assert_eq!(match_bracket(t, &[c(1)]), Some((1, 11)));
        // caret right after ')' (offset 12) -> back to '(' at 1
        assert_eq!(match_bracket(t, &[c(12)]), Some((11, 1)));
        // inner pair
        assert_eq!(match_bracket(t, &[c(8)]), Some((8, 10)));
        assert_eq!(match_bracket(t, &[c(0)]), None);
        assert_eq!(match_bracket("(()", &[c(0)]), None, "unbalanced");
        let o = run("bracket_select", &ctx(t, &[c(1)], &[])).unwrap();
        assert_eq!(o.selection, Some(vec![r(1, 12)]));
        let o = run("bracket_select", &ctx(t, &[c(12)], &[])).unwrap();
        assert_eq!(o.selection, Some(vec![r(1, 12)]));
        let o = run("bracket_match", &ctx(t, &[c(1)], &[])).unwrap();
        assert_eq!(o.selection, Some(vec![c(11)]));
    }

    #[test]
    fn paste_column_pads_short_lines_and_appends() {
        let t = "abcd\nx\nwxyz";
        let e = paste_column(t, &[c(2)], "1\n2\n3\n4").unwrap();
        assert_eq!(e.apply_to_str(t).unwrap(), "ab1cd\nx 2\nwx3yz\n  4");
        // CRLF clipboard and trailing newline
        let e = paste_column("ab\r\ncd", &[c(1)], "X\r\nY\r\n").unwrap();
        assert_eq!(e.apply_to_str("ab\r\ncd").unwrap(), "aXb\r\ncYd");
        assert!(run("edit_paste_column", &ctx(t, &[c(0)], &[String::new()])).unwrap().message.is_some());
        let o = run("edit_paste_column", &ctx(t, &[c(0)], &["Q".to_string()])).unwrap();
        assert_eq!(o.edits.apply_to_str(t).unwrap(), "Qabcd\nx\nwxyz");
    }

    #[test]
    fn date_and_time_inserts() {
        // 2026-11-05 13:45:09 UTC is a Thursday
        let secs = 1_793_886_309;
        let o = stamp_at("", &[c(0)], "%H:%M", secs);
        assert_eq!(o.edits.apply_to_str("").unwrap(), "13:45");
        let o = stamp_at("x", &[c(1)], "%H:%M:%S", secs);
        assert_eq!(o.edits.apply_to_str("x").unwrap(), "x13:45:09");
        let o = stamp_at("", &[c(0)], "%A, %B %d, %Y", secs);
        assert_eq!(o.edits.apply_to_str("").unwrap(), "Thursday, November 05, 2026");
        // live commands produce one edit per caret
        for id in ["insert_time_short", "insert_time_long", "insert_date_long"] {
            let o = run(id, &ctx("ab", &[c(0), c(2)], &[])).unwrap();
            assert_eq!(o.edits.0.len(), 2, "{id}");
        }
    }

    #[test]
    fn registry_ids_match_run() {
        for s in commands() {
            assert!(run(&s.id, &ctx("a b", &[c(0)], &[])).is_some(), "{} has no implementation", s.id);
        }
        assert!(run("nope", &ctx("", &[], &[])).is_none());
    }
}
