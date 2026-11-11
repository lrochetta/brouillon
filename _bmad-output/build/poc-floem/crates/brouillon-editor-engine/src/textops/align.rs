//! BK-033 align / reformat and BK-034 fill / number sequence.
//! Widths are measured in chars (no wide-glyph or tab-stop awareness).

use super::*;

pub fn commands() -> Vec<CommandSpec> {
    [
        ("align_left", "Format: Align Left"),
        ("align_center", "Format: Align Center"),
        ("align_right", "Format: Align Right"),
        ("align_on", "Format: Align on Separator (default =)"),
        ("rewrap", "Format: Rewrap Paragraph at Column"),
        ("split_long_lines", "Format: Split Long Lines at Column"),
        ("fill_insert", "Fill: Insert Text in Rectangle"),
        ("fill_replace", "Fill: Replace Rectangle With Text"),
        ("fill_repeat", "Fill: Repeat Text Across Rectangle"),
        ("fill_remove", "Fill: Remove Rectangle"),
        ("fill_append", "Fill: Append Text to Lines"),
        ("fill_number", "Fill: Number Sequence"),
    ]
    .iter()
    .map(|(id, cap)| spec(id, cap, if id.starts_with("fill") { "Fill" } else { "Format" }))
    .collect()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Radix {
    Dec,
    Hex,
    Oct,
    Bin,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FillOp {
    Insert(String),
    Replace(String),
    Repeat(String),
    Remove,
    Append(String),
    Number { start: i64, step: i64, pad: usize, radix: Radix },
}

pub fn run(id: &str, ctx: &CmdCtx) -> Option<CmdOutcome> {
    let t = ctx.text;
    let text_arg = || ctx.args.first().cloned().unwrap_or_default();
    let ls = split_lines(t);
    Some(match id {
        "align_left" | "align_center" | "align_right" => {
            let (a, b) = target_range(t, &ls, ctx.selection, false);
            let trimmed: Vec<&str> = (a..=b).map(|i| t[ls[i].start..ls[i].end].trim()).collect();
            let width = ctx.args.first().and_then(|w| w.parse().ok()).unwrap_or_else(|| trimmed.iter().map(|s| s.chars().count()).max().unwrap_or(0));
            let new: Vec<String> = trimmed
                .iter()
                .map(|s| {
                    let gap = width.saturating_sub(s.chars().count());
                    match id {
                        "align_left" => s.to_string(),
                        "align_right" => format!("{}{s}", " ".repeat(gap)),
                        _ => format!("{}{s}", " ".repeat(gap / 2)),
                    }
                })
                .collect();
            outcome(t, vec![replace_lines(t, &ls, a, b, &new)])
        }
        "align_on" => {
            let sep = ctx.args.first().filter(|s| !s.is_empty()).cloned().unwrap_or_else(|| "=".into());
            let (a, b) = target_range(t, &ls, ctx.selection, false);
            let lines: Vec<&str> = (a..=b).map(|i| &t[ls[i].start..ls[i].end]).collect();
            let width = lines.iter().filter_map(|l| l.find(&sep).map(|p| l[..p].trim_end().chars().count())).max().unwrap_or(0);
            let new: Vec<String> = lines
                .iter()
                .map(|l| match l.find(&sep) {
                    Some(p) => {
                        let left = l[..p].trim_end();
                        format!("{left}{} {}", " ".repeat(width - left.chars().count()), &l[p..])
                    }
                    None => l.to_string(),
                })
                .collect();
            outcome(t, vec![replace_lines(t, &ls, a, b, &new)])
        }
        "rewrap" => {
            let col = arg_usize(ctx, 0, 80).max(1);
            let (a, b) = paragraph_range(t, &ls, ctx.selection);
            let lines: Vec<&str> = (a..=b).map(|i| &t[ls[i].start..ls[i].end]).collect();
            outcome(t, vec![replace_lines(t, &ls, a, b, &rewrap(&lines, col))])
        }
        "split_long_lines" => {
            let col = arg_usize(ctx, 0, 80).max(1);
            let (a, b) = target_range(t, &ls, ctx.selection, true);
            let new: Vec<String> = (a..=b)
                .flat_map(|i| {
                    let l = &t[ls[i].start..ls[i].end];
                    if l.chars().count() <= col { vec![l.to_string()] } else { wrap_words(l, col) }
                })
                .collect();
            outcome(t, vec![replace_lines(t, &ls, a, b, &new)])
        }
        "fill_insert" => fill_rect(t, ctx.selection, &FillOp::Insert(text_arg())),
        "fill_replace" => fill_rect(t, ctx.selection, &FillOp::Replace(text_arg())),
        "fill_repeat" => fill_rect(t, ctx.selection, &FillOp::Repeat(text_arg())),
        "fill_remove" => fill_rect(t, ctx.selection, &FillOp::Remove),
        "fill_append" => fill_rect(t, ctx.selection, &FillOp::Append(text_arg())),
        "fill_number" => {
            let num = |i: usize, d: i64| ctx.args.get(i).and_then(|a| a.parse().ok()).unwrap_or(d);
            let radix = match ctx.args.get(3).map(String::as_str) {
                Some("hex") => Radix::Hex,
                Some("oct") => Radix::Oct,
                Some("bin") => Radix::Bin,
                _ => Radix::Dec,
            };
            fill_rect(t, ctx.selection, &FillOp::Number { start: num(0, 1), step: num(1, 1), pad: num(2, 0).max(0) as usize, radix })
        }
        _ => return None,
    })
}

/// Selected lines, or the blank-line-delimited paragraph around the caret.
fn paragraph_range(t: &str, ls: &[Line], sel: &[Region]) -> (usize, usize) {
    let (a, b) = target_range(t, ls, sel, false);
    if a != b || sel.iter().any(|r| !r.is_caret()) {
        return (a, b);
    }
    let blank = |i: usize| t[ls[i].start..ls[i].end].trim().is_empty();
    if blank(a) {
        return (a, a);
    }
    let (mut x, mut y) = (a, a);
    while x > 0 && !blank(x - 1) {
        x -= 1;
    }
    while y + 1 < ls.len() && !blank(y + 1) {
        y += 1;
    }
    (x, y)
}

/// Greedy wrap of one logical line (keeps its indent); a word longer than `col` stays whole.
pub fn wrap_words(line: &str, col: usize) -> Vec<String> {
    let indent: String = line.chars().take_while(|c| c.is_whitespace()).collect();
    wrap(&indent, line.split_whitespace(), col)
}

fn wrap<'a>(indent: &str, words: impl Iterator<Item = &'a str>, col: usize) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = indent.to_string();
    let mut has_word = false;
    for w in words {
        if has_word && cur.chars().count() + 1 + w.chars().count() > col {
            out.push(std::mem::replace(&mut cur, indent.to_string()));
            has_word = false;
        }
        if has_word {
            cur.push(' ');
        }
        cur.push_str(w);
        has_word = true;
    }
    if has_word {
        out.push(cur);
    }
    out
}

/// Re-flow paragraphs (separated by blank lines) at `col`; blank lines are kept.
pub fn rewrap(lines: &[&str], col: usize) -> Vec<String> {
    let mut out = Vec::new();
    let mut para: Vec<&str> = Vec::new();
    let flush = |para: &mut Vec<&str>, out: &mut Vec<String>| {
        if let Some(first) = para.first() {
            let indent: String = first.chars().take_while(|c| c.is_whitespace()).collect();
            out.extend(wrap(&indent, para.iter().flat_map(|l| l.split_whitespace()), col));
            para.clear();
        }
    };
    for l in lines {
        if l.trim().is_empty() {
            flush(&mut para, &mut out);
            out.push(String::new());
        } else {
            para.push(l);
        }
    }
    flush(&mut para, &mut out);
    out
}

fn format_number(n: i64, pad: usize, radix: Radix) -> String {
    let (sign, m) = (if n < 0 { "-" } else { "" }, n.unsigned_abs());
    let body = match radix {
        Radix::Dec => m.to_string(),
        Radix::Hex => format!("{m:x}"),
        Radix::Oct => format!("{m:o}"),
        Radix::Bin => format!("{m:b}"),
    };
    format!("{sign}{body:0>pad$}")
}

/// Apply `op` on the rectangle spanned by the first region (lines x char columns).
/// Short lines are padded with spaces up to the rectangle's left column.
pub fn fill_rect(t: &str, sel: &[Region], op: &FillOp) -> CmdOutcome {
    let Some(r) = sel.first() else { return CmdOutcome::default() };
    let ls = split_lines(t);
    let (la, lb) = (line_of(&ls, r.anchor), line_of(&ls, r.head));
    let (ca, cb) = (column_of(t, &ls, r.anchor), column_of(t, &ls, r.head));
    let (first, last, (s, e)) = (la.min(lb), la.max(lb), (ca.min(cb), ca.max(cb)));
    let mut edits = Vec::new();
    for (k, i) in (first..=last).enumerate() {
        let l = ls[i];
        let len = t[l.start..l.end].chars().count();
        let (cs, ce) = (s.min(len), e.min(len));
        let pad = " ".repeat(s.saturating_sub(len));
        let (bs, be) = (byte_at_column(t, l, cs), byte_at_column(t, l, ce));
        edits.push(match op {
            FillOp::Insert(x) => TextEdit::insert(bs, format!("{pad}{x}")),
            FillOp::Replace(x) => TextEdit::new(bs, be, format!("{pad}{x}")),
            FillOp::Repeat(x) => {
                if x.is_empty() || e == s {
                    continue;
                }
                TextEdit::new(bs, be, format!("{pad}{}", x.chars().cycle().take(e - s).collect::<String>()))
            }
            FillOp::Remove => TextEdit::delete(bs, be),
            FillOp::Append(x) => TextEdit::insert(l.end, x.as_str()),
            FillOp::Number { start, step, pad: w, radix } => {
                TextEdit::new(bs, be, format!("{pad}{}", format_number(start + step * k as i64, *w, *radix)))
            }
        });
    }
    outcome(t, edits)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::textops::{apply, sel};

    #[test]
    fn align_basic() {
        assert_eq!(apply("align_right", " a\nbbb", &sel(0, 6), &[]), "  a\nbbb");
        assert_eq!(apply("align_center", "a\nbbb", &sel(0, 5), &[]), " a\nbbb");
        assert_eq!(apply("align_left", "  a\n b", &sel(0, 6), &[]), "a\nb");
    }

    #[test]
    fn align_on_equals_golden() {
        let t = "a = 1\nlonger=2\nnoeq\nxy   =   3";
        assert_eq!(apply("align_on", t, &sel(0, t.len()), &[]), "a      = 1\nlonger =2\nnoeq\nxy     =   3");
    }

    #[test]
    fn rewrap_never_splits_long_words() {
        let t = "aaa bbb ccc ddd\nsupercalifragilistic x\n\n  indented words here";
        let out = apply("rewrap", t, &sel(0, t.len()), &["10"]);
        assert_eq!(out, "aaa bbb\nccc ddd\nsupercalifragilistic\nx\n\n  indented\n  words\n  here");
        // caret: only its paragraph
        assert_eq!(apply("rewrap", "a b\nc\n\nd e", &sel(0, 0), &["80"]), "a b c\n\nd e");
    }

    #[test]
    fn split_long_lines_keeps_indent() {
        assert_eq!(apply("split_long_lines", "  aa bb cc", &sel(0, 0), &["6"]), "  aa\n  bb\n  cc");
        assert_eq!(apply("split_long_lines", "short", &sel(0, 0), &["80"]), "short");
    }

    #[test]
    fn number_fill_step_two_zero_pad_12_lines() {
        let t = (0..12).map(|_| "x").collect::<Vec<_>>().join("
");
        // zero-width rectangle at column 0 from line 0 to line 11 (line 11 starts at byte 22)
        let out = apply("fill_number", &t, &sel(0, 22), &["1", "2", "3"]);
        let want: Vec<String> = (0..12).map(|i| format!("{:03}x", 1 + 2 * i)).collect();
        assert_eq!(out, want.join("
"));
    }

    #[test]
    fn rectangle_ops_on_ragged_lines() {
        let t = "a
abcdef
abcd"; // line starts: 0, 2, 9
        let cols_1_3 = sel(1, 12); // line0 col1 .. line2 col3
        assert_eq!(apply("fill_remove", t, &cols_1_3, &[]), "a
adef
ad");
        assert_eq!(apply("fill_replace", t, &cols_1_3, &["Z"]), "aZ
aZdef
aZd");
        assert_eq!(apply("fill_repeat", t, &cols_1_3, &["xy"]), "axy
axydef
axyd");
        assert_eq!(apply("fill_append", t, &sel(0, 2), &["!"]), "a!
abcdef!
abcd");
        // short middle line is padded with spaces up to the rectangle column
        let r = "abcd
a
abcd"; // line starts: 0, 5, 7
        assert_eq!(apply("fill_insert", r, &sel(3, 10), &["-"]), "abc-d
a  -
abc-d");
    }

    #[test]
    fn number_radixes() {
        assert_eq!(format_number(255, 4, Radix::Hex), "00ff");
        assert_eq!(format_number(5, 0, Radix::Bin), "101");
        assert_eq!(format_number(8, 0, Radix::Oct), "10");
        assert_eq!(format_number(-3, 3, Radix::Dec), "-003");
    }
}
