//! Lot 6 - selection commands (BK-038, BK-042 jumps, BK-044).
//!
//! Every command is a pure function of `CmdCtx` (text + selection) returning a
//! `CmdOutcome` with a new selection and NO edits. Selection vectors are in
//! "added order, LAST = primary" (see `cursor.rs`).
//!
//! Stateful commands (`bookmark_toggle`, `bookmark_clear`, `bookmark_invert`,
//! `nav_back`, `nav_forward`) are listed in `commands()` for the registry but
//! `run` returns `None` for them: the shell calls `Markers` / `NavHistory`
//! directly. `bookmark_next` / `bookmark_prev` read the marker lines (0-based)
//! from `ctx.args`.

use brouillon_core::contract::{CmdCtx, CmdOutcome, CommandSpec, Region};

use crate::cursor::{
    byte_to_line_col, collapse_to_primary, line_col_to_byte, line_range, line_starts, normalize, word_range,
};
use crate::markers::Markers;

/// Commands this module contributes to the registry.
pub fn commands() -> Vec<CommandSpec> {
    let c = |id: &str, cap: &str, chord: Option<&str>| CommandSpec {
        id: id.into(),
        caption: cap.into(),
        category: "Selection".into(),
        default_chord: chord.map(String::from),
        when: None, ..Default::default()
    };
    let b = |id: &str, cap: &str, chord: Option<&str>| CommandSpec { category: "Bookmarks".into(), ..c(id, cap, chord) };
    vec![
        c("add_next_occurrence", "Selection: Add Next Occurrence", Some("ctrl+d")),
        c("skip_occurrence", "Selection: Skip Occurrence", Some("ctrl+k ctrl+d")),
        c("undo_last_added", "Selection: Undo Last Added", Some("ctrl+u")),
        c("select_all_occurrences", "Selection: Select All Occurrences", Some("ctrl+alt+d")),
        c("split_selection_into_lines", "Selection: Split Into Lines", Some("ctrl+shift+l")),
        c("add_caret_above", "Selection: Add Caret Above", Some("ctrl+alt+up")),
        c("add_caret_below", "Selection: Add Caret Below", Some("ctrl+alt+down")),
        c("collapse_selection", "Selection: Collapse To Primary", Some("escape")),
        c("select_inside_brackets", "Selection: Select Inside Brackets", Some("ctrl+shift+m")),
        c("go_to", "Navigate: Go To (line, line:col, @0xOFFSET)", Some("ctrl+g")),
        b("bookmark_toggle", "Bookmarks: Toggle", Some("ctrl+f2")),
        b("bookmark_next", "Bookmarks: Next", Some("f2")),
        b("bookmark_prev", "Bookmarks: Previous", Some("shift+f2")),
        b("bookmark_clear", "Bookmarks: Clear All", None),
        b("bookmark_invert", "Bookmarks: Invert", None),
        c("nav_back", "Navigate: Back", Some("alt+left")),
        c("nav_forward", "Navigate: Forward", Some("alt+right")),
    ]
}

/// Run command `id`; `None` = not mine (or stateful, see module doc).
pub fn run(id: &str, ctx: &CmdCtx) -> Option<CmdOutcome> {
    let sel = ctx.selection;
    let out = |regions: Vec<Region>, message: Option<String>| CmdOutcome { selection: Some(regions), message, ..Default::default() };
    let none = |m: &str| Some(CmdOutcome { message: Some(m.into()), ..Default::default() });
    match id {
        "add_next_occurrence" => match add_next_occurrence(ctx.text, sel) {
            Some(r) => Some(out(r, None)),
            None => none("No more occurrences"),
        },
        "skip_occurrence" => match skip_occurrence(ctx.text, sel) {
            Some(r) => Some(out(r, None)),
            None => none("No more occurrences"),
        },
        "undo_last_added" => Some(out(undo_last_added(sel), None)),
        "select_all_occurrences" => Some(out(select_all_occurrences(ctx.text, sel), None)),
        "split_selection_into_lines" => Some(out(split_selection_into_lines(ctx.text, sel), None)),
        "add_caret_above" => Some(out(add_caret_vertical(ctx.text, sel, false), None)),
        "add_caret_below" => Some(out(add_caret_vertical(ctx.text, sel, true), None)),
        "collapse_selection" => Some(out(collapse_to_primary(sel), None)),
        "select_inside_brackets" => match select_inside_brackets(ctx.text, sel) {
            Some(r) => Some(out(vec![r], None)),
            None => none("No enclosing brackets"),
        },
        "go_to" => match ctx.args.first().and_then(|a| parse_goto(a)) {
            Some(t) => Some(out(vec![Region::caret(goto_byte(ctx.text, t))], None)),
            None => none("Invalid position"),
        },
        "bookmark_next" | "bookmark_prev" => {
            let m = Markers::from_lines(ctx.args.iter().filter_map(|a| a.parse::<usize>().ok()));
            let here = sel.last().map(|r| byte_to_line_col(ctx.text, r.head).0).unwrap_or(0);
            let line = if id == "bookmark_next" { m.next(here) } else { m.prev(here) };
            match line {
                Some(l) => Some(out(vec![Region::caret(line_col_to_byte(ctx.text, l, 0))], None)),
                None => none("No bookmarks"),
            }
        }
        _ => None,
    }
}

fn is_word_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

pub(crate) fn is_whole_word(text: &str, s: usize, e: usize) -> bool {
    s < e
        && text[s..e].chars().all(is_word_char)
        && !text[..s].chars().next_back().is_some_and(is_word_char)
        && !text[e..].chars().next().is_some_and(is_word_char)
}

/// Next non-overlapping match of `needle` at/after `from`, wrapping, that is
/// not already selected. Whole-word matching when the source selection is one.
fn next_match(text: &str, needle: &str, from: usize, taken: &[Region], whole: bool) -> Option<Region> {
    let hits = |range: std::ops::Range<usize>| {
        text[range.clone()].match_indices(needle).map(move |(i, _)| range.start + i).find(|&s| {
            let e = s + needle.len();
            (!whole || is_whole_word(text, s, e)) && !taken.iter().any(|r| s < r.end().max(r.start() + 1) && r.start() < e)
        })
    };
    let s = hits(from..text.len()).or_else(|| hits(0..text.len()))?;
    Some(Region { anchor: s, head: s + needle.len() })
}

/// Add the next occurrence of the primary selection (a caret first expands to
/// the word under it). `None` when nothing new can be added.
pub fn add_next_occurrence(text: &str, sel: &[Region]) -> Option<Vec<Region>> {
    let mut v = sel.to_vec();
    let p = *v.last()?;
    if p.is_caret() {
        let (s, e) = word_range(text, p.head, WORD_DELIMS)?;
        *v.last_mut()? = Region { anchor: s, head: e };
        return Some(v);
    }
    let needle = &text[p.start()..p.end()];
    let r = next_match(text, needle, p.end(), &v, is_whole_word(text, p.start(), p.end()))?;
    v.push(r);
    Some(v)
}

/// ASCII punctuation except `_`: makes `word_range` identifier-style.
pub(crate) const WORD_DELIMS: &str = "!\"#$%&'()*+,-./:;<=>?@[\\]^`{|}~";

/// Replace the primary by the next occurrence (the current one stays unselected).
pub fn skip_occurrence(text: &str, sel: &[Region]) -> Option<Vec<Region>> {
    let p = *sel.last()?;
    if p.is_caret() {
        return None;
    }
    let mut rest = sel[..sel.len() - 1].to_vec();
    let needle = &text[p.start()..p.end()];
    let mut taken = rest.clone();
    taken.push(p); // do not land on the skipped one
    let r = next_match(text, needle, p.end(), &taken, is_whole_word(text, p.start(), p.end()))?;
    rest.push(r);
    Some(rest)
}

/// Drop the most recently added region (keeps at least one).
pub fn undo_last_added(sel: &[Region]) -> Vec<Region> {
    if sel.len() > 1 { sel[..sel.len() - 1].to_vec() } else { sel.to_vec() }
}

/// Select every occurrence of the primary selection / word under the caret.
pub fn select_all_occurrences(text: &str, sel: &[Region]) -> Vec<Region> {
    let Some(&p) = sel.last() else { return Vec::new() };
    let (s, e) = if p.is_caret() {
        match word_range(text, p.head, WORD_DELIMS) {
            Some(w) => w,
            None => return sel.to_vec(),
        }
    } else {
        (p.start(), p.end())
    };
    let needle = &text[s..e];
    let whole = is_whole_word(text, s, e);
    let all: Vec<Region> = text
        .match_indices(needle)
        .filter(|(i, _)| !whole || is_whole_word(text, *i, i + needle.len()))
        .map(|(i, _)| Region { anchor: i, head: i + needle.len() })
        .collect();
    normalize(&all)
}

/// Split every multi-line selection into one region per line; carets and
/// single-line regions are kept.
pub fn split_selection_into_lines(text: &str, sel: &[Region]) -> Vec<Region> {
    let starts = line_starts(text);
    let mut out = Vec::new();
    for r in sel {
        let (a, b) = (byte_to_line_col(text, r.start()).0, byte_to_line_col(text, r.end()).0);
        if r.is_caret() || a == b {
            out.push(*r);
            continue;
        }
        for l in a..=b {
            let (ls, le) = line_range(text, &starts, l);
            let (s, e) = (ls.max(r.start()), le.min(r.end()));
            if l == b && e == ls && s == ls && r.end() == ls {
                continue; // selection ending exactly at a line start: no empty last piece
            }
            out.push(Region { anchor: s, head: e });
        }
    }
    out
}

/// Add a caret one line above/below the primary head, same character column.
pub fn add_caret_vertical(text: &str, sel: &[Region], below: bool) -> Vec<Region> {
    let mut v = sel.to_vec();
    let Some(p) = sel.last() else { return v };
    let (line, col) = byte_to_line_col(text, p.head);
    let last = line_starts(text).len() - 1;
    let target = if below { (line < last).then_some(line + 1) } else { line.checked_sub(1) };
    if let Some(t) = target {
        let c = Region::caret(line_col_to_byte(text, t, col));
        if !v.contains(&c) {
            v.push(c);
        }
    }
    v
}

/// Select the contents of the innermost bracket pair `() [] {}` strictly
/// enclosing the selection; repeating expands to the next enclosing pair.
pub fn select_inside_brackets(text: &str, sel: &[Region]) -> Option<Region> {
    let p = sel.last()?;
    let (s, e) = (p.start(), p.end());
    let mut stack: Vec<(u8, usize)> = Vec::new();
    let mut best: Option<(usize, usize)> = None;
    for (i, b) in text.bytes().enumerate() {
        match b {
            b'(' | b'[' | b'{' => stack.push((b, i)),
            b')' | b']' | b'}' => {
                let want = match b {
                    b')' => b'(',
                    b']' => b'[',
                    _ => b'{',
                };
                if let Some(pos) = stack.iter().rposition(|(o, _)| *o == want) {
                    let (_, o) = stack[pos];
                    stack.truncate(pos);
                    let (is, ie) = (o + 1, i);
                    let encloses = is <= s && e <= ie && (is, ie) != (s, e);
                    if encloses && best.is_none_or(|(bs, be)| ie - is < be - bs) {
                        best = Some((is, ie));
                    }
                }
            }
            _ => {}
        }
    }
    best.map(|(a, b)| Region { anchor: a, head: b })
}

// ---- BK-044: Go to ----------------------------------------------------------

/// Parsed "Go to" target. Lines and columns are 1-based as typed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GotoTarget {
    Line(usize),
    LineCol(usize, usize),
    Offset(usize),
}

/// Parse `12`, `12:5` or `@0x40` / `@64` (byte offset).
pub fn parse_goto(input: &str) -> Option<GotoTarget> {
    let t = input.trim();
    if let Some(off) = t.strip_prefix('@') {
        let n = match off.strip_prefix("0x").or_else(|| off.strip_prefix("0X")) {
            Some(h) => usize::from_str_radix(h, 16).ok()?,
            None => off.parse().ok()?,
        };
        return Some(GotoTarget::Offset(n));
    }
    match t.split_once(':') {
        Some((l, c)) => Some(GotoTarget::LineCol(l.trim().parse().ok()?, c.trim().parse().ok()?)),
        None => t.parse().ok().map(GotoTarget::Line),
    }
}

/// Resolve a target to a byte offset (clamped to the text; offsets snap back to a char boundary).
pub fn goto_byte(text: &str, t: GotoTarget) -> usize {
    match t {
        GotoTarget::Line(l) => line_col_to_byte(text, l.saturating_sub(1), 0),
        GotoTarget::LineCol(l, c) => line_col_to_byte(text, l.saturating_sub(1), c.saturating_sub(1)),
        GotoTarget::Offset(mut o) => {
            o = o.min(text.len());
            while !text.is_char_boundary(o) {
                o -= 1;
            }
            o
        }
    }
}

/// [`goto_line_col`] straight on a buffer (no whole-text copy; never waits for a lazy line index: past the
/// part already indexed the line count is an estimate and the caret is placed by line number alone).
pub fn goto_line_col_buf(buf: &crate::buffer::Buffer, t: GotoTarget) -> (usize, usize) {
    let last = buf.line_count().max(1);
    match t {
        GotoTarget::Line(l) => (l.clamp(1, last), 1),
        GotoTarget::LineCol(l, c) => {
            let l = l.clamp(1, last);
            let width = buf.nth_line(l - 1).map_or(0, |s| s.chars().count());
            (l, c.clamp(1, width + 1))
        }
        GotoTarget::Offset(o) => {
            let mut b = o.min(buf.byte_len());
            while !buf.is_char_boundary(b) {
                b -= 1;
            }
            let line = buf.line_of_byte(b);
            let start = buf.byte_of_line(line);
            let mut cols = 0;
            buf.visit_str(start..b, &mut |p| {
                cols += p.chars().count();
                true
            });
            (line + 1, cols + 1)
        }
    }
}

/// 1-based (line, col) of a byte offset, as used by the UI cursor signal.
pub fn goto_line_col(text: &str, t: GotoTarget) -> (usize, usize) {
    let (l, c) = byte_to_line_col(text, goto_byte(text, t));
    (l + 1, c + 1)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx<'a>(text: &'a str, selection: &'a [Region], args: &'a [String]) -> CmdCtx<'a> {
        CmdCtx { text, selection, path: None, args }
    }
    fn sel_of(o: Option<CmdOutcome>) -> Vec<Region> {
        o.unwrap().selection.unwrap()
    }
    fn txt<'a>(t: &'a str, r: &Region) -> &'a str {
        &t[r.start()..r.end()]
    }

    #[test]
    fn repeated_add_next_selects_word_then_each_whole_word_match_and_wraps() {
        let t = "foo foobar foo x foo";
        let s1 = add_next_occurrence(t, &[Region::caret(1)]).unwrap();
        assert_eq!(txt(t, &s1[0]), "foo");
        let s2 = add_next_occurrence(t, &s1).unwrap();
        assert_eq!((s2.len(), s2[1].start()), (2, 11), "skips 'foobar' (whole word)");
        let s3 = add_next_occurrence(t, &s2).unwrap();
        assert_eq!(s3[2].start(), 17);
        assert!(add_next_occurrence(t, &s3).is_none(), "all taken");
        // wrap-around: start from the last occurrence
        let w = add_next_occurrence(t, &[Region { anchor: 17, head: 20 }]).unwrap();
        assert_eq!(w[1].start(), 0);
    }

    #[test]
    fn non_word_selection_matches_substrings() {
        let t = "ab_ab ab";
        let r = add_next_occurrence(t, &[Region { anchor: 0, head: 2 }]).unwrap();
        assert_eq!(r[1], Region { anchor: 3, head: 5 }, "not a whole word (bounded by _): substring match");
    }

    #[test]
    fn skip_undo_and_select_all() {
        let t = "a b a b a";
        let s = [Region { anchor: 0, head: 1 }];
        let skipped = skip_occurrence(t, &s).unwrap();
        assert_eq!(skipped, vec![Region { anchor: 4, head: 5 }]);
        let two = add_next_occurrence(t, &s).unwrap();
        assert_eq!(undo_last_added(&two), s.to_vec());
        assert_eq!(undo_last_added(&s), s.to_vec());
        assert_eq!(select_all_occurrences(t, &s).len(), 3);
        assert_eq!(select_all_occurrences(t, &[Region::caret(2)]).len(), 2);
    }

    #[test]
    fn split_into_lines_and_add_carets() {
        let t = "ab\ncd\nef";
        let r = split_selection_into_lines(t, &[Region { anchor: 1, head: 7 }]);
        assert_eq!(r, vec![Region { anchor: 1, head: 2 }, Region { anchor: 3, head: 5 }, Region { anchor: 6, head: 7 }]);
        let below = add_caret_vertical(t, &[Region::caret(1)], true);
        assert_eq!(below, vec![Region::caret(1), Region::caret(4)]);
        assert_eq!(add_caret_vertical(t, &[Region::caret(1)], false).len(), 1, "no line above");
        assert_eq!(add_caret_vertical(t, &[Region::caret(7)], true).len(), 1, "no line below");
    }

    #[test]
    fn select_inside_brackets_expands_on_repeat() {
        let t = "f(a, [b, (c)])";
        let c = t.find('c').unwrap();
        let r1 = select_inside_brackets(t, &[Region::caret(c)]).unwrap();
        assert_eq!(txt(t, &r1), "c");
        let r2 = select_inside_brackets(t, &[r1]).unwrap();
        assert_eq!(txt(t, &r2), "b, (c)");
        let r3 = select_inside_brackets(t, &[r2]).unwrap();
        assert_eq!(txt(t, &r3), "a, [b, (c)]");
        assert!(select_inside_brackets(t, &[r3]).is_none());
    }

    #[test]
    fn goto_parsing() {
        assert_eq!(parse_goto("12"), Some(GotoTarget::Line(12)));
        assert_eq!(parse_goto(" 12:5 "), Some(GotoTarget::LineCol(12, 5)));
        assert_eq!(parse_goto("@0x40"), Some(GotoTarget::Offset(64)));
        assert_eq!(parse_goto("@64"), Some(GotoTarget::Offset(64)));
        assert_eq!(parse_goto("nope"), None);
        assert_eq!(parse_goto("1:x"), None);
        let t = "ab\ncdef\n";
        assert_eq!(goto_line_col(t, GotoTarget::LineCol(2, 3)), (2, 3));
        assert_eq!(goto_line_col(t, GotoTarget::Line(99)), (3, 1));
        assert_eq!(goto_byte(t, GotoTarget::Offset(4)), 4);
        assert_eq!(goto_byte("é", GotoTarget::Offset(1)), 0);
    }

    #[test]
    fn run_dispatch_and_bookmarks() {
        let t = "a\nb\nc\nd";
        let args: Vec<String> = vec!["1".into(), "3".into()];
        let s = [Region::caret(0)];
        assert_eq!(sel_of(run("bookmark_next", &ctx(t, &s, &args))), vec![Region::caret(2)]);
        let s = [Region::caret(2)];
        assert_eq!(sel_of(run("bookmark_next", &ctx(t, &s, &args))), vec![Region::caret(6)]);
        let s = [Region::caret(6)];
        assert_eq!(sel_of(run("bookmark_next", &ctx(t, &s, &args))), vec![Region::caret(2)], "wraps");
        assert_eq!(sel_of(run("bookmark_prev", &ctx(t, &s, &args))), vec![Region::caret(2)]);
        let g = vec!["3:1".to_string()];
        assert_eq!(sel_of(run("go_to", &ctx(t, &[], &g))), vec![Region::caret(4)]);
        assert!(run("bookmark_toggle", &ctx(t, &s, &args)).is_none());
        assert!(run("nope", &ctx(t, &s, &args)).is_none());
        let two = [Region::caret(0), Region::caret(4)];
        assert_eq!(sel_of(run("collapse_selection", &ctx(t, &two, &[]))), vec![Region::caret(4)]);
        assert!(commands().iter().all(|c| !c.id.is_empty() && !c.caption.is_empty()));
    }
}
