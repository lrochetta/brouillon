//! Line diff of two texts (buffer vs buffer, buffer vs disk) for the diff view.
//!
//! Inline (unified) rows and side-by-side rows come from one `similar` line diff. Side-by-side
//! pairs a run of deletions with the run of insertions that follows it, so a modified line shows
//! the old text on the left and the new one on the right.

use similar::{ChangeTag, TextDiff};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Equal,
    Delete,
    Insert,
    /// Side-by-side only: a deletion paired with an insertion.
    Change,
}

/// Unified row. Line numbers are 1-based.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InlineRow {
    pub kind: Kind,
    pub old_no: Option<usize>,
    pub new_no: Option<usize>,
    pub text: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Side {
    pub no: usize,
    pub text: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SideRow {
    pub kind: Kind,
    pub left: Option<Side>,
    pub right: Option<Side>,
}

fn trim_eol(s: &str) -> String {
    s.trim_end_matches(['\n', '\r']).to_string()
}

pub fn inline_rows(old: &str, new: &str) -> Vec<InlineRow> {
    let diff = TextDiff::from_lines(old, new);
    diff.iter_all_changes()
        .map(|c| InlineRow {
            kind: match c.tag() {
                ChangeTag::Equal => Kind::Equal,
                ChangeTag::Delete => Kind::Delete,
                ChangeTag::Insert => Kind::Insert,
            },
            old_no: c.old_index().map(|i| i + 1),
            new_no: c.new_index().map(|i| i + 1),
            text: trim_eol(c.value()),
        })
        .collect()
}

pub fn side_by_side(old: &str, new: &str) -> Vec<SideRow> {
    side_by_side_from(&inline_rows(old, new))
}

/// [`side_by_side`] from rows that were already computed (the diff itself runs once).
pub fn side_by_side_from(rows: &[InlineRow]) -> Vec<SideRow> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < rows.len() {
        if rows[i].kind == Kind::Equal {
            let r = &rows[i];
            out.push(SideRow {
                kind: Kind::Equal,
                left: Some(Side { no: r.old_no.unwrap_or(0), text: r.text.clone() }),
                right: Some(Side { no: r.new_no.unwrap_or(0), text: r.text.clone() }),
            });
            i += 1;
            continue;
        }
        let dels: Vec<&InlineRow> = rows[i..].iter().take_while(|r| r.kind == Kind::Delete).collect();
        let j = i + dels.len();
        let ins: Vec<&InlineRow> = rows[j..].iter().take_while(|r| r.kind == Kind::Insert).collect();
        for k in 0..dels.len().max(ins.len()) {
            let l = dels.get(k).map(|r| Side { no: r.old_no.unwrap_or(0), text: r.text.clone() });
            let r = ins.get(k).map(|r| Side { no: r.new_no.unwrap_or(0), text: r.text.clone() });
            let kind = match (&l, &r) {
                (Some(_), Some(_)) => Kind::Change,
                (Some(_), None) => Kind::Delete,
                _ => Kind::Insert,
            };
            out.push(SideRow { kind, left: l, right: r });
        }
        i = j + ins.len();
    }
    out
}

/// Indexes (into the rows) of the first row of every changed hunk, for next/previous change.
pub fn hunk_starts(kinds: &[Kind]) -> Vec<usize> {
    let mut v = Vec::new();
    for (i, k) in kinds.iter().enumerate() {
        if *k != Kind::Equal && (i == 0 || kinds[i - 1] == Kind::Equal) {
            v.push(i);
        }
    }
    v
}

/// `(added, removed)` line counts, for the diff header.
pub fn stats(old: &str, new: &str) -> (usize, usize) {
    stats_from(&inline_rows(old, new))
}

/// [`stats`] from rows that were already computed.
pub fn stats_from(rows: &[InlineRow]) -> (usize, usize) {
    (rows.iter().filter(|r| r.kind == Kind::Insert).count(), rows.iter().filter(|r| r.kind == Kind::Delete).count())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identical_texts_have_no_hunks() {
        let r = inline_rows("a\nb\n", "a\nb\n");
        assert!(r.iter().all(|r| r.kind == Kind::Equal));
        assert_eq!(stats("a\nb\n", "a\nb\n"), (0, 0));
        assert!(hunk_starts(&r.iter().map(|r| r.kind).collect::<Vec<_>>()).is_empty());
    }

    #[test]
    fn inline_numbers_lines_on_both_sides() {
        let r = inline_rows("a\nb\nc\n", "a\nB\nc\nd\n");
        let k: Vec<Kind> = r.iter().map(|r| r.kind).collect();
        assert_eq!(k, [Kind::Equal, Kind::Delete, Kind::Insert, Kind::Equal, Kind::Insert]);
        assert_eq!((r[1].old_no, r[1].new_no), (Some(2), None));
        assert_eq!((r[2].old_no, r[2].new_no), (None, Some(2)));
        assert_eq!(r[4].text, "d");
        assert_eq!(stats("a\nb\nc\n", "a\nB\nc\nd\n"), (2, 1));
    }

    #[test]
    fn side_by_side_pairs_a_modified_line() {
        let s = side_by_side("one\ntwo\nthree\n", "one\n2\nthree\n");
        assert_eq!(s.len(), 3);
        assert_eq!(s[1].kind, Kind::Change);
        assert_eq!(s[1].left, Some(Side { no: 2, text: "two".into() }));
        assert_eq!(s[1].right, Some(Side { no: 2, text: "2".into() }));
    }

    #[test]
    fn side_by_side_handles_pure_insert_and_delete_and_uneven_runs() {
        let s = side_by_side("a\nb\nc\n", "a\nc\n");
        assert_eq!(s.iter().map(|r| r.kind).collect::<Vec<_>>(), [Kind::Equal, Kind::Delete, Kind::Equal]);
        assert!(s[1].right.is_none());
        let s = side_by_side("a\nx\n", "a\ny\nz\n");
        assert_eq!(s.iter().map(|r| r.kind).collect::<Vec<_>>(), [Kind::Equal, Kind::Change, Kind::Insert]);
        assert!(s[2].left.is_none());
        assert_eq!(hunk_starts(&s.iter().map(|r| r.kind).collect::<Vec<_>>()), vec![1]);
    }

    #[test]
    fn crlf_does_not_make_every_line_different_in_the_text() {
        let r = inline_rows("a\r\nb\r\n", "a\r\nb\r\n");
        assert!(r.iter().all(|r| r.kind == Kind::Equal && !r.text.contains('\r')));
    }

    #[test]
    fn empty_sides() {
        assert_eq!(inline_rows("", "").len(), 0);
        assert_eq!(stats("", "a\nb\n"), (2, 0));
        assert_eq!(stats("a\n", ""), (0, 1));
    }

    #[test]
    fn views_built_from_one_diff_equal_the_separate_ones() {
        let cases = [("", ""), ("a\nb\nc\n", "a\nB\nc\nd\n"), ("a\nx\n", "a\ny\nz\n"), ("a\nb\nc\n", "a\nc\n"), ("1\n2\n3\n4\n5\n", "1\nX\n3\nY\nZ\n5\n")];
        for (o, n) in cases {
            let rows = inline_rows(o, n);
            assert_eq!(side_by_side_from(&rows), side_by_side(o, n));
            assert_eq!(stats_from(&rows), stats(o, n));
            // hunks counted on the inline rows are the same as on the side rows
            let hi = hunk_starts(&rows.iter().map(|r| r.kind).collect::<Vec<_>>()).len();
            let hs = hunk_starts(&side_by_side(o, n).iter().map(|r| r.kind).collect::<Vec<_>>()).len();
            assert_eq!(hi, hs);
        }
    }
}
