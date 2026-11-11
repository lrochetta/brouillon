//! BK-030 sort lines. Selection else whole buffer; always stable.

use super::*;
use rand::seq::SliceRandom;
use rand::SeedableRng;
use std::cmp::Ordering;
use std::collections::HashSet;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Plain,
    CaseInsensitive,
    Numeric,
    Natural,
    Length,
}

#[derive(Debug, Clone, Copy)]
pub struct SortOpts {
    pub kind: Kind,
    pub descending: bool,
    /// Remove later lines identical to an earlier one (after sorting).
    pub dedupe: bool,
    /// Sort key = chars `start..end` (0-based, end exclusive, `None` = to line end).
    pub key: Option<(usize, Option<usize>)>,
}

impl Default for SortOpts {
    fn default() -> Self {
        Self { kind: Kind::Plain, descending: false, dedupe: false, key: None }
    }
}

const ALL: &[(&str, &str, Kind, bool)] = &[
    ("sort_asc", "Sort: Ascending", Kind::Plain, false),
    ("sort_desc", "Sort: Descending", Kind::Plain, true),
    ("sort_ci", "Sort: Ascending (case-insensitive)", Kind::CaseInsensitive, false),
    ("sort_ci_desc", "Sort: Descending (case-insensitive)", Kind::CaseInsensitive, true),
    ("sort_numeric", "Sort: Numeric Ascending", Kind::Numeric, false),
    ("sort_numeric_desc", "Sort: Numeric Descending", Kind::Numeric, true),
    ("sort_natural", "Sort: Natural Ascending", Kind::Natural, false),
    ("sort_length", "Sort: By Length", Kind::Length, false),
];

pub fn commands() -> Vec<CommandSpec> {
    let mut v: Vec<_> = ALL.iter().map(|(id, cap, ..)| spec(id, cap, "Sort")).collect();
    v.push(spec("sort_unique", "Sort: Ascending and Remove Duplicates", "Sort"));
    v.push(spec("sort_by_column", "Sort: By Column Range", "Sort"));
    v.push(spec("sort_shuffle", "Sort: Shuffle Lines", "Sort"));
    v
}

pub fn run(id: &str, ctx: &CmdCtx) -> Option<CmdOutcome> {
    let t = ctx.text;
    let mut opts = SortOpts::default();
    let mut seed = None;
    match id {
        "sort_unique" => opts.dedupe = true,
        "sort_by_column" => {
            // args: start column, optional end column (1-based, inclusive)
            let s = arg_usize(ctx, 0, 1).max(1) - 1;
            opts.key = Some((s, ctx.args.get(1).and_then(|a| a.parse::<usize>().ok()).map(|e| e.max(s + 1))));
        }
        "sort_shuffle" => seed = Some(arg_usize(ctx, 0, 0) as u64),
        _ => {
            let &(_, _, kind, desc) = ALL.iter().find(|(i, ..)| *i == id)?;
            opts.kind = kind;
            opts.descending = desc;
        }
    }
    let ls = split_lines(t);
    let (a, b) = target_range(t, &ls, ctx.selection, true);
    let lines: Vec<String> = (a..=b).map(|i| t[ls[i].start..ls[i].end].to_string()).collect();
    let sorted = match seed {
        Some(s) => shuffle_strings(lines, s),
        None => sort_strings(lines, &opts),
    };
    Some(outcome(t, vec![replace_lines(t, &ls, a, b, &sorted)]))
}

pub fn sort_strings(mut lines: Vec<String>, o: &SortOpts) -> Vec<String> {
    let key = |s: &str| -> String {
        match o.key {
            Some((st, en)) => s.chars().skip(st).take(en.map_or(usize::MAX, |e| e - st)).collect(),
            None => s.to_string(),
        }
    };
    let keys: Vec<String> = lines.iter().map(|l| key(l)).collect();
    let mut idx: Vec<usize> = (0..lines.len()).collect();
    idx.sort_by(|&x, &y| {
        let ord = compare(&keys[x], &keys[y], o.kind);
        if o.descending { ord.reverse() } else { ord }
    });
    let mut out: Vec<String> = idx.into_iter().map(|i| std::mem::take(&mut lines[i])).collect();
    if o.dedupe {
        let mut seen = HashSet::new();
        out.retain(|l| seen.insert(l.clone()));
    }
    out
}

pub fn shuffle_strings(mut lines: Vec<String>, seed: u64) -> Vec<String> {
    lines.shuffle(&mut rand_chacha::ChaCha8Rng::seed_from_u64(seed));
    lines
}

fn compare(a: &str, b: &str, kind: Kind) -> Ordering {
    match kind {
        Kind::Plain => a.cmp(b),
        Kind::CaseInsensitive => a.to_lowercase().cmp(&b.to_lowercase()),
        Kind::Length => a.chars().count().cmp(&b.chars().count()),
        Kind::Numeric => match (first_number(a), first_number(b)) {
            (Some(x), Some(y)) => x.total_cmp(&y),
            (None, None) => Ordering::Equal,
            (None, _) => Ordering::Less,
            (_, None) => Ordering::Greater,
        },
        Kind::Natural => natural_cmp(a, b),
    }
}

/// First number in `s`; `,` or `.` may be decimal or thousands separator ("1,234.5", "1.234,5", "3,5").
pub fn first_number(s: &str) -> Option<f64> {
    let cs: Vec<char> = s.chars().collect();
    let start = cs.iter().position(|c| c.is_ascii_digit())?;
    let neg = start > 0 && cs[start - 1] == '-';
    let mut end = start;
    while end < cs.len() && (cs[end].is_ascii_digit() || ((cs[end] == '.' || cs[end] == ',') && cs.get(end + 1).is_some_and(|c| c.is_ascii_digit()))) {
        end += 1;
    }
    let tok: String = cs[start..end].iter().collect();
    let last_sep = tok.rfind(['.', ',']);
    let mut norm = String::new();
    for (i, c) in tok.char_indices() {
        if c == '.' || c == ',' {
            let seps = tok.matches(['.', ',']).count();
            let same = tok.matches(c).count();
            // decimal only if it is the last separator and (single separator, or the other kind precedes it)
            if Some(i) == last_sep && (seps == 1 || same == 1) {
                norm.push('.');
            }
        } else {
            norm.push(c);
        }
    }
    norm.parse::<f64>().ok().map(|v| if neg { -v } else { v })
}

pub fn natural_cmp(a: &str, b: &str) -> Ordering {
    let (mut a, mut b) = (a.chars().peekable(), b.chars().peekable());
    loop {
        match (a.peek().copied(), b.peek().copied()) {
            (None, None) => return Ordering::Equal,
            (None, _) => return Ordering::Less,
            (_, None) => return Ordering::Greater,
            (Some(x), Some(y)) if x.is_ascii_digit() && y.is_ascii_digit() => {
                let take = |it: &mut std::iter::Peekable<std::str::Chars>| {
                    let mut s = String::new();
                    while let Some(&c) = it.peek().filter(|c| c.is_ascii_digit()) {
                        s.push(c);
                        it.next();
                    }
                    s
                };
                let (na, nb) = (take(&mut a), take(&mut b));
                let (ta, tb) = (na.trim_start_matches('0'), nb.trim_start_matches('0'));
                let ord = ta.len().cmp(&tb.len()).then_with(|| ta.cmp(tb));
                if ord != Ordering::Equal {
                    return ord;
                }
            }
            (Some(x), Some(y)) => {
                let ord = x.to_lowercase().cmp(y.to_lowercase());
                if ord != Ordering::Equal {
                    return ord;
                }
                a.next();
                b.next();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::textops::{apply, sel};

    fn s(v: &[&str]) -> Vec<String> {
        v.iter().map(|x| x.to_string()).collect()
    }

    #[test]
    fn numeric_10_after_9() {
        assert_eq!(apply("sort_numeric", "10\n9\n-1\nx\n2.5", &sel(0, 0), &[]), "x\n-1\n2.5\n9\n10");
        assert_eq!(apply("sort_asc", "10\n9", &sel(0, 0), &[]), "10\n9"); // plain is lexicographic
        assert_eq!(first_number("a 1,5 b"), Some(1.5));
        assert_eq!(first_number("1,234.5"), Some(1234.5));
        assert_eq!(first_number("1.234,5"), Some(1234.5));
        assert_eq!(first_number("1,234,567"), Some(1234567.0));
    }

    #[test]
    fn stable_with_equal_keys_and_desc() {
        let o = SortOpts { key: Some((0, Some(1))), ..Default::default() };
        assert_eq!(sort_strings(s(&["b2", "a1", "b1", "a2"]), &o), s(&["a1", "a2", "b2", "b1"]));
        let d = SortOpts { descending: true, key: Some((0, Some(1))), ..Default::default() };
        // equal keys keep original order even descending
        assert_eq!(sort_strings(s(&["b2", "a1", "b1", "a2"]), &d), s(&["b2", "b1", "a1", "a2"]));
    }

    #[test]
    fn dedupe_keeps_first_and_variants() {
        assert_eq!(apply("sort_unique", "b\na\nb\na\nc", &sel(0, 0), &[]), "a\nb\nc");
        assert_eq!(apply("sort_ci", "b\nA\na", &sel(0, 0), &[]), "A\na\nb");
        assert_eq!(apply("sort_natural", "f10\nf9\nf09a", &sel(0, 0), &[]), "f9\nf09a\nf10");
        assert_eq!(apply("sort_length", "ccc\na\nbb", &sel(0, 0), &[]), "a\nbb\nccc");
        assert_eq!(apply("sort_desc", "a\nc\nb", &sel(0, 0), &[]), "c\nb\na");
        assert_eq!(apply("sort_by_column", "x3\ny1\nz2", &sel(0, 0), &["2"]), "y1\nz2\nx3");
    }

    #[test]
    fn selection_only_crlf_and_trailing_eol() {
        assert_eq!(apply("sort_asc", "z\r\nb\r\na\r\nq", &sel(3, 10), &[]), "z\r\na\r\nb\r\nq");
        assert_eq!(apply("sort_asc", "b\na\n", &sel(0, 0), &[]), "a\nb\n");
    }

    #[test]
    fn shuffle_is_seeded_permutation() {
        let l = s(&["1", "2", "3", "4", "5", "6"]);
        let a = shuffle_strings(l.clone(), 7);
        assert_eq!(a, shuffle_strings(l.clone(), 7));
        let mut sorted = a.clone();
        sorted.sort();
        assert_eq!(sorted, l);
    }
}
