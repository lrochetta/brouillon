//! Search scopes (BK-018) and line modes / marking (BK-019). Pure: everything
//! returns data (`EditSet`, `LineSet`, `String`), nothing mutates a buffer.

use std::collections::BTreeSet;
use std::ops::Range;

use brouillon_core::contract::{EditSet, LineSet, TextEdit};
use crop::Rope;

use super::replace_tpl::Template;
use super::{normalize_ranges, slice_text, CancelToken, Cancelled, Matcher};

/// Where a search/replace applies inside ONE document.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Scope {
    Document,
    /// Non-caret selection ranges (several for multi-region / column mode).
    Selection(Vec<Range<usize>>),
    /// From this offset to the end of the document.
    ToEndOfFile(usize),
}

impl Scope {
    /// Concrete byte ranges for a document of `len` bytes.
    pub fn ranges(&self, len: usize) -> Vec<Range<usize>> {
        match self {
            Scope::Document => vec![0..len],
            Scope::Selection(rs) => normalize_ranges(&rs.iter().map(|r| r.start.min(len)..r.end.min(len)).collect::<Vec<_>>()),
            Scope::ToEndOfFile(from) => vec![(*from).min(len)..len],
        }
    }
}

/// One open document handed to the all-tabs operations (`id` is the caller's tab id).
pub struct TabDoc<'a> {
    pub id: usize,
    pub rope: &'a Rope,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TabReport {
    pub id: usize,
    pub count: usize,
    /// Edits in this tab's original offsets (apply as one undo group by Lot 1).
    pub edits: EditSet,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct MultiReplace {
    /// One entry per input tab, same order (empty edits when nothing matched).
    pub tabs: Vec<TabReport>,
    pub total: usize,
}

/// Replace all in every open tab: one `EditSet` per tab plus a per-tab count.
pub fn replace_in_tabs(m: &Matcher, tpl: &Template, tabs: &[TabDoc], cancel: &CancelToken) -> Result<MultiReplace, Cancelled> {
    let mut out = MultiReplace::default();
    for t in tabs {
        let r = m.replace(t.rope, &[0..t.rope.byte_len()], tpl, cancel)?;
        out.total += r.count;
        out.tabs.push(TabReport { id: t.id, count: r.count, edits: r.edits });
    }
    Ok(out)
}

/// Per-tab match counts `(id, count)`.
pub fn count_in_tabs(m: &Matcher, tabs: &[TabDoc], cancel: &CancelToken) -> Result<Vec<(usize, usize)>, Cancelled> {
    tabs.iter().map(|t| Ok((t.id, m.count(t.rope, &[0..t.rope.byte_len()], cancel)?))).collect()
}

// ------------------------------------------------------------ line modes

/// One entry of the Occurrences list: a line that contains a match.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Occurrence {
    /// 0-based line.
    pub line: usize,
    /// Line text without terminator.
    pub text: String,
    /// First match on the line, absolute byte span.
    pub span: Range<usize>,
}

/// Number of real lines (a trailing empty line after the last `\n` is not one).
fn real_line_count(rope: &Rope) -> usize {
    let len = rope.byte_len();
    if len == 0 {
        return 0;
    }
    let n = rope.line_len();
    if n > 1 && rope.byte_of_line(n - 1) == len {
        n - 1
    } else {
        n
    }
}

/// Lines touched by `r` (end-exclusive: a range ending at a line start does not touch that line).
fn lines_of(rope: &Rope, r: &Range<usize>) -> Range<usize> {
    let first = rope.line_of_byte(r.start);
    let mut last = rope.line_of_byte(r.end);
    if r.end > r.start && rope.byte_of_line(last) == r.end {
        last -= 1;
    }
    first..last.max(first) + 1
}

fn match_lines(m: &Matcher, rope: &Rope, ranges: &[Range<usize>], cancel: &CancelToken) -> Result<(BTreeSet<usize>, Vec<Range<usize>>), Cancelled> {
    let hits = m.find_all(rope, ranges, cancel)?;
    let mut lines = BTreeSet::new();
    for h in &hits {
        lines.extend(lines_of(rope, h));
    }
    Ok((lines, hits))
}

/// "Mark all hits": sorted unique 0-based lines containing a match.
pub fn mark_all(m: &Matcher, rope: &Rope, ranges: &[Range<usize>], cancel: &CancelToken) -> Result<LineSet, Cancelled> {
    Ok(LineSet::from_unsorted(match_lines(m, rope, ranges, cancel)?.0.into_iter().collect()))
}

/// Occurrences list: one entry per matching line.
pub fn occurrences(m: &Matcher, rope: &Rope, ranges: &[Range<usize>], cancel: &CancelToken) -> Result<Vec<Occurrence>, Cancelled> {
    let (lines, hits) = match_lines(m, rope, ranges, cancel)?;
    let mut out: Vec<Occurrence> = Vec::new();
    for line in lines {
        let span = hits
            .iter()
            .find(|h| lines_of(rope, h).contains(&line))
            .cloned()
            .unwrap_or(0..0);
        out.push(Occurrence { line, text: slice_text(rope.line(line)).into_owned(), span });
    }
    Ok(out)
}

/// All matched texts joined by `\n`.
pub fn copy_all_matches(m: &Matcher, rope: &Rope, ranges: &[Range<usize>], cancel: &CancelToken) -> Result<String, Cancelled> {
    let hits = m.find_all(rope, ranges, cancel)?;
    let parts: Vec<String> = hits.into_iter().map(|h| slice_text(rope.byte_slice(h)).into_owned()).collect();
    Ok(parts.join("\n"))
}

/// Lines selected by a line mode: those with (or, `invert`, without) a match,
/// restricted to lines touched by `ranges`.
fn selected_lines(m: &Matcher, rope: &Rope, ranges: &[Range<usize>], invert: bool, cancel: &CancelToken) -> Result<Vec<usize>, Cancelled> {
    let (hit_lines, _) = match_lines(m, rope, ranges, cancel)?;
    if !invert {
        return Ok(hit_lines.into_iter().collect());
    }
    let total = real_line_count(rope);
    let mut cand = BTreeSet::new();
    for r in normalize_ranges(ranges) {
        cand.extend(lines_of(rope, &r).filter(|l| *l < total));
    }
    Ok(cand.difference(&hit_lines).copied().collect())
}

/// Number of lines containing (or, `invert`, not containing) a match.
pub fn count_lines(m: &Matcher, rope: &Rope, ranges: &[Range<usize>], invert: bool, cancel: &CancelToken) -> Result<usize, Cancelled> {
    Ok(selected_lines(m, rope, ranges, invert, cancel)?.len())
}

/// Text of the selected lines joined by `\n`.
pub fn copy_lines(m: &Matcher, rope: &Rope, ranges: &[Range<usize>], invert: bool, cancel: &CancelToken) -> Result<String, Cancelled> {
    let lines = selected_lines(m, rope, ranges, invert, cancel)?;
    Ok(lines.iter().map(|&l| slice_text(rope.line(l)).into_owned()).collect::<Vec<_>>().join("\n"))
}

/// Edits deleting the selected lines (terminator included). An unterminated
/// last line takes the preceding terminator with it so no empty line is left.
pub fn delete_lines(m: &Matcher, rope: &Rope, ranges: &[Range<usize>], invert: bool, cancel: &CancelToken) -> Result<EditSet, Cancelled> {
    let lines = selected_lines(m, rope, ranges, invert, cancel)?;
    let (len, total) = (rope.byte_len(), real_line_count(rope));
    let mut spans: Vec<Range<usize>> = Vec::new();
    for &l in &lines {
        let start = rope.byte_of_line(l);
        let mut span = start..if l + 1 < rope.line_len() { rope.byte_of_line(l + 1) } else { len };
        let unterminated_last = l + 1 == total && span.end == len && !slice_text(rope.byte_slice(span.clone())).ends_with('\n');
        let prev_selected = l > 0 && lines.binary_search(&(l - 1)).is_ok();
        if unterminated_last && l > 0 && !prev_selected {
            let tl = if start >= 2 && slice_text(rope.byte_slice(start - 2..start)) == "\r\n" { 2 } else { 1 };
            span.start = start - tl;
        }
        match spans.last_mut() {
            Some(last) if span.start <= last.end => last.end = last.end.max(span.end),
            _ => spans.push(span),
        }
    }
    Ok(EditSet(spans.into_iter().map(|s| TextEdit::delete(s.start, s.end)).collect()))
}

/// Cut = copy text + delete edits.
pub fn cut_lines(m: &Matcher, rope: &Rope, ranges: &[Range<usize>], invert: bool, cancel: &CancelToken) -> Result<(String, EditSet), Cancelled> {
    Ok((copy_lines(m, rope, ranges, invert, cancel)?, delete_lines(m, rope, ranges, invert, cancel)?))
}

#[cfg(test)]
mod tests {
    use super::super::{SearchMode, SearchOptions};
    use super::*;

    fn m(p: &str) -> Matcher {
        Matcher::new(p, &SearchOptions { mode: SearchMode::Normal, ..SearchOptions::default() }).unwrap()
    }
    fn t() -> CancelToken {
        CancelToken::new()
    }
    const FIX: &str = "keep 1\nDROP a\nkeep 2\nDROP b\nDROP c\nkeep 3\n";

    #[test]
    fn replace_all_across_three_tabs() {
        let (a, b, c) = (Rope::from("x x"), Rope::from("y"), Rope::from("x\nx\nx"));
        let tabs = [TabDoc { id: 7, rope: &a }, TabDoc { id: 8, rope: &b }, TabDoc { id: 9, rope: &c }];
        let rep = replace_in_tabs(&m("x"), &Template::parse("Z"), &tabs, &t()).unwrap();
        assert_eq!(rep.tabs.len(), 3);
        assert_eq!(rep.total, 5);
        assert_eq!(rep.tabs.iter().map(|r| (r.id, r.count)).collect::<Vec<_>>(), vec![(7, 2), (8, 0), (9, 3)]);
        assert!(rep.tabs[1].edits.is_empty());
        assert_eq!(rep.tabs[2].edits.apply_to_str("x\nx\nx").unwrap(), "Z\nZ\nZ");
        assert_eq!(count_in_tabs(&m("x"), &tabs, &t()).unwrap(), vec![(7, 2), (8, 0), (9, 3)]);
    }

    #[test]
    fn scopes_resolve_to_ranges() {
        assert_eq!(Scope::Document.ranges(10), vec![0..10]);
        assert_eq!(Scope::ToEndOfFile(4).ranges(10), vec![4..10]);
        assert_eq!(Scope::ToEndOfFile(40).ranges(10), vec![10..10]);
        assert_eq!(Scope::Selection(vec![5..8, 1..3, 2..4]).ranges(6), vec![1..4, 5..6]);
    }

    #[test]
    fn mark_and_occurrences() {
        let r = Rope::from(FIX);
        let all = [0..r.byte_len()];
        let marks = mark_all(&m("DROP"), &r, &all, &t()).unwrap();
        assert_eq!(marks.0, vec![1, 3, 4]);
        let occ = occurrences(&m("DROP"), &r, &all, &t()).unwrap();
        assert_eq!(occ.len(), 3);
        assert_eq!((occ[0].line, occ[0].text.as_str(), occ[0].span.clone()), (1, "DROP a", 7..11));
        // two matches on one line still make a single entry; a multi-line match marks both lines
        let r2 = Rope::from("aa aa\nb\nc");
        assert_eq!(mark_all(&m("aa"), &r2, &[0..9], &t()).unwrap().0, vec![0]);
        let ml = Matcher::new(r"b\Rc", &SearchOptions { mode: SearchMode::Regex, ..SearchOptions::default() }).unwrap();
        assert_eq!(mark_all(&ml, &r2, &[0..9], &t()).unwrap().0, vec![1, 2]);
    }

    #[test]
    fn delete_lines_containing_and_not_containing() {
        let r = Rope::from(FIX);
        let all = [0..r.byte_len()];
        let del = delete_lines(&m("DROP"), &r, &all, false, &t()).unwrap();
        assert_eq!(del.apply_to_str(FIX).unwrap(), "keep 1\nkeep 2\nkeep 3\n");
        let del = delete_lines(&m("DROP"), &r, &all, true, &t()).unwrap();
        assert_eq!(del.apply_to_str(FIX).unwrap(), "DROP a\nDROP b\nDROP c\n");
        assert_eq!(count_lines(&m("DROP"), &r, &all, false, &t()).unwrap(), 3);
        assert_eq!(count_lines(&m("DROP"), &r, &all, true, &t()).unwrap(), 3);
        assert_eq!(copy_lines(&m("DROP"), &r, &all, false, &t()).unwrap(), "DROP a\nDROP b\nDROP c");
        let (txt, edits) = cut_lines(&m("keep"), &r, &all, false, &t()).unwrap();
        assert_eq!(txt, "keep 1\nkeep 2\nkeep 3");
        assert_eq!(edits.apply_to_str(FIX).unwrap(), "DROP a\nDROP b\nDROP c\n");
    }

    #[test]
    fn delete_last_unterminated_line() {
        for (text, want) in [("a\nDROP", "a"), ("a\r\nDROP", "a"), ("DROP\nDROP", ""), ("DROP", "")] {
            let r = Rope::from(text);
            let del = delete_lines(&m("DROP"), &r, &[0..r.byte_len()], false, &t()).unwrap();
            assert_eq!(del.apply_to_str(text).unwrap(), want, "{text:?}");
        }
        // CRLF document
        let text = "a\r\nDROP\r\nb\r\n";
        let r = Rope::from(text);
        let del = delete_lines(&m("DROP"), &r, &[0..r.byte_len()], false, &t()).unwrap();
        assert_eq!(del.apply_to_str(text).unwrap(), "a\r\nb\r\n");
    }

    #[test]
    fn line_ops_respect_selection_scope() {
        let r = Rope::from(FIX);
        // selection covers lines 1..=2 only
        let sel = [7..20];
        assert_eq!(mark_all(&m("DROP"), &r, &sel, &t()).unwrap().0, vec![1]);
        assert_eq!(count_lines(&m("DROP"), &r, &sel, true, &t()).unwrap(), 1);
    }

    #[test]
    fn copy_matches_joined() {
        let r = Rope::from("a1 b22\nc333");
        let re = Matcher::new(r"\d+", &SearchOptions { mode: SearchMode::Regex, ..SearchOptions::default() }).unwrap();
        assert_eq!(copy_all_matches(&re, &r, &[0..r.byte_len()], &t()).unwrap(), "1\n22\n333");
    }
}
