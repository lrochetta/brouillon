//! Line diff between two versions of a document: which lines changed (for `STALE_VERSION`), and
//! rebasing a set of edits over typing the user did meanwhile (proposals that still fit).
//!
//! Safety rule of the rebase: an edit is moved only when every line it touches, and the line just
//! before and just after it, are unchanged and still adjacent in the new text. Anything else is
//! reported as stale; the user's typing is never overwritten and an edit is never applied to a
//! look-alike line.

use crate::api::TextEdit;

/// Largest middle region (old lines x new lines) aligned with the quadratic table.
const MAX_CELLS: usize = 4_000_000;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LineRange {
    /// 1-based, inclusive, in the NEW text.
    pub start_line: usize,
    pub end_line: usize,
    /// "changed", "added" or "deleted" (lines were removed just before `start_line`).
    pub kind: &'static str,
}

fn starts(text: &str) -> Vec<usize> {
    let mut v = vec![0];
    v.extend(text.match_indices('\n').map(|(i, _)| i + 1));
    v
}

fn lines<'a>(text: &'a str, st: &[usize]) -> Vec<&'a str> {
    (0..st.len()).map(|i| &text[st[i]..st.get(i + 1).copied().unwrap_or(text.len())]).collect()
}

/// Matched `(old line, new line)` pairs, in order.
fn align(old: &[&str], new: &[&str]) -> Vec<(usize, usize)> {
    let (n, m) = (old.len(), new.len());
    let mut p = 0;
    while p < n && p < m && old[p] == new[p] {
        p += 1;
    }
    let mut s = 0;
    while s < n - p && s < m - p && old[n - 1 - s] == new[m - 1 - s] {
        s += 1;
    }
    let mut pairs: Vec<(usize, usize)> = (0..p).map(|i| (i, i)).collect();
    let (on, nn) = (n - p - s, m - p - s);
    if on > 0 && nn > 0 && on.saturating_mul(nn) <= MAX_CELLS {
        let w = nn + 1;
        let mut t = vec![0u16; (on + 1) * w];
        for i in (0..on).rev() {
            for j in (0..nn).rev() {
                t[i * w + j] = if old[p + i] == new[p + j] { t[(i + 1) * w + j + 1] + 1 } else { t[(i + 1) * w + j].max(t[i * w + j + 1]) };
            }
        }
        let (mut i, mut j) = (0, 0);
        while i < on && j < nn {
            if old[p + i] == new[p + j] {
                pairs.push((p + i, p + j));
                i += 1;
                j += 1;
            } else if t[(i + 1) * w + j] >= t[i * w + j + 1] {
                i += 1;
            } else {
                j += 1;
            }
        }
    }
    pairs.extend((0..s).map(|k| (n - s + k, m - s + k)));
    pairs
}

/// Lines of `new` that differ from `old`, grouped. Empty when the texts are equal.
pub fn changed_ranges(old: &str, new: &str) -> Vec<LineRange> {
    let (so, sn) = (starts(old), starts(new));
    let (ol, nl) = (lines(old, &so), lines(new, &sn));
    let pairs = align(&ol, &nl);
    let mut out = Vec::new();
    let (mut po, mut pn) = (0usize, 0usize);
    let mut gap = |po: usize, oi: usize, pn: usize, ni: usize| {
        if ni > pn {
            out.push(LineRange { start_line: pn + 1, end_line: ni, kind: if oi > po { "changed" } else { "added" } });
        } else if oi > po {
            out.push(LineRange { start_line: (pn + 1).min(nl.len()), end_line: (pn + 1).min(nl.len()), kind: "deleted" });
        }
    };
    for &(oi, ni) in &pairs {
        gap(po, oi, pn, ni);
        po = oi + 1;
        pn = ni + 1;
    }
    gap(po, ol.len(), pn, nl.len());
    out
}

fn line_of(st: &[usize], off: usize) -> usize {
    match st.binary_search(&off) {
        Ok(i) => i,
        Err(i) => i - 1,
    }
}

/// Move `edits` (byte offsets in `old`) to the same places in `new`, or `None` when that cannot be
/// done safely (see the module rule).
pub fn rebase(old: &str, new: &str, edits: &[TextEdit]) -> Option<Vec<TextEdit>> {
    let (so, sn) = (starts(old), starts(new));
    let (ol, nl) = (lines(old, &so), lines(new, &sn));
    let pairs = align(&ol, &nl);
    let mut map: Vec<Option<usize>> = vec![None; ol.len()];
    for (o, n) in pairs {
        map[o] = Some(n);
    }
    let mut out = Vec::with_capacity(edits.len());
    for e in edits {
        if e.start > e.end || e.end > old.len() {
            return None;
        }
        let l1 = line_of(&so, e.start);
        let l2 = line_of(&so, if e.end > e.start { e.end - 1 } else { e.start });
        let mut prev: Option<usize> = None;
        for l in l1..=l2 {
            let n = map[l]?;
            if prev.map_or(false, |p| p + 1 != n) {
                return None;
            }
            prev = Some(n);
        }
        // Unchanged, adjacent neighbours on both sides (unless the edit touches a document edge).
        if l1 > 0 {
            let (a, b) = (map[l1 - 1]?, map[l1]?);
            if a + 1 != b {
                return None;
            }
        }
        if l2 + 1 < ol.len() {
            let (a, b) = (map[l2]?, map[l2 + 1]?);
            if a + 1 != b {
                return None;
            }
        }
        let ns = sn[map[l1]?] + (e.start - so[l1]);
        let ne = sn[map[l2]?] + (e.end - so[l2]);
        if ne < ns || ne > new.len() || !new.is_char_boundary(ns) || !new.is_char_boundary(ne) || new[ns..ne] != old[e.start..e.end] {
            return None;
        }
        out.push(TextEdit { start: ns, end: ne, new_text: e.new_text.clone() });
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn r(s: usize, e: usize, k: &'static str) -> LineRange {
        LineRange { start_line: s, end_line: e, kind: k }
    }

    #[test]
    fn equal_texts_have_no_changes() {
        assert!(changed_ranges("a\nb\n", "a\nb\n").is_empty());
    }

    #[test]
    fn changed_added_and_deleted_lines() {
        assert_eq!(changed_ranges("a\nb\nc\n", "a\nB\nc\n"), [r(2, 2, "changed")]);
        assert_eq!(changed_ranges("a\nc\n", "a\nb\nc\n"), [r(2, 2, "added")]);
        assert_eq!(changed_ranges("a\nb\nc\n", "a\nc\n"), [r(2, 2, "deleted")]);
        assert_eq!(changed_ranges("a\nb\nc\nd\ne\n", "A\nb\nc\nd\nE\n"), [r(1, 1, "changed"), r(5, 5, "changed")]);
    }

    #[test]
    fn typing_at_the_end_of_a_line_is_one_changed_line() {
        assert_eq!(changed_ranges("hello\n", "hello, typed\n"), [r(1, 1, "changed")]);
    }

    #[test]
    fn rebase_moves_edits_over_typing_elsewhere() {
        let old = "one\ntwo\nthree\nfour\nfive\n";
        // The user inserted a line at the top and changed "five".
        let new = "zero\none\ntwo\nthree\nfour\nFIVE!\n";
        let off = old.find("three").unwrap();
        let moved = rebase(old, new, &[TextEdit { start: off, end: off + 5, new_text: "3".into() }]).unwrap();
        assert_eq!(&new[moved[0].start..moved[0].end], "three");
        assert_eq!(moved[0].new_text, "3");
    }

    #[test]
    fn rebase_refuses_when_the_touched_or_neighbouring_line_changed() {
        let old = "one\ntwo\nthree\n";
        let e = |s: &str| {
            let o = old.find(s).unwrap();
            TextEdit { start: o, end: o + s.len(), new_text: "X".into() }
        };
        assert!(rebase(old, "one\ntwo!\nthree\n", &[e("two")]).is_none(), "same line");
        assert!(rebase(old, "one!\ntwo\nthree\n", &[e("two")]).is_none(), "previous line");
        assert!(rebase(old, "one\ntwo\nthree!\n", &[e("two")]).is_none(), "next line");
        assert!(rebase(old, "one\ntwo\nx\nthree\n", &[e("two")]).is_none(), "a line was inserted right after");
    }

    #[test]
    fn rebase_does_not_land_on_a_look_alike_line() {
        let old = "a\n}\nb\n}\nc\n";
        // The user added "}" lines at the top: the second "}" of the old text must stay the target.
        let new = "}\n}\na\n}\nb\n}\nc\n";
        let off = old.rfind('}').unwrap();
        let moved = rebase(old, new, &[TextEdit { start: off, end: off + 1, new_text: "]".into() }]).unwrap();
        assert_eq!(moved[0].start, new.rfind('}').unwrap());
    }

    #[test]
    fn rebase_handles_multibyte_and_insertions() {
        let old = "é1\nmid\nz\n";
        let new = "top\né1\nmid\nz\n";
        let at = old.find("mid").unwrap();
        let moved = rebase(old, new, &[TextEdit { start: at, end: at, new_text: "-".into() }]).unwrap();
        assert_eq!(moved[0].start, new.find("mid").unwrap());
        assert!(new.is_char_boundary(moved[0].start));
    }
}
