//! BK-029 whitespace and indentation. `args[0]` = tab size (default 4).
//! Trim and tab conversion act on the whole buffer when nothing is selected;
//! indent / outdent act on the caret line.

use super::*;

pub fn commands() -> Vec<CommandSpec> {
    [
        ("trim_trailing", "Whitespace: Trim Trailing"),
        ("trim_leading", "Whitespace: Trim Leading"),
        ("trim_both", "Whitespace: Trim Leading and Trailing"),
        ("tabs_to_spaces", "Whitespace: Tabs to Spaces"),
        ("tabs_to_spaces_leading", "Whitespace: Leading Tabs to Spaces"),
        ("spaces_to_tabs", "Whitespace: Spaces to Tabs"),
        ("spaces_to_tabs_leading", "Whitespace: Leading Spaces to Tabs"),
        ("indent_increase", "Whitespace: Increase Indent (spaces)"),
        ("indent_increase_tab", "Whitespace: Increase Indent (tab)"),
        ("indent_decrease", "Whitespace: Decrease Indent"),
    ]
    .iter()
    .map(|(id, cap)| spec(id, cap, "Whitespace"))
    .collect()
}

pub fn run(id: &str, ctx: &CmdCtx) -> Option<CmdOutcome> {
    let t = ctx.text;
    let tab = arg_usize(ctx, 0, 4).max(1);
    let ls = split_lines(t);
    let whole = !matches!(id, "indent_increase" | "indent_increase_tab" | "indent_decrease");
    if !matches!(id, "trim_trailing" | "trim_leading" | "trim_both" | "tabs_to_spaces" | "tabs_to_spaces_leading" | "spaces_to_tabs" | "spaces_to_tabs_leading" | "indent_increase" | "indent_increase_tab" | "indent_decrease") {
        return None;
    }
    let (a, b) = target_range(t, &ls, ctx.selection, whole);
    let mut edits = Vec::new();
    for l in &ls[a..=b] {
        let line = &t[l.start..l.end];
        let new = match id {
            "trim_trailing" => line.trim_end().to_string(),
            "trim_leading" => line.trim_start().to_string(),
            "trim_both" => line.trim().to_string(),
            "tabs_to_spaces" => tabs_to_spaces(line, tab, false),
            "tabs_to_spaces_leading" => tabs_to_spaces(line, tab, true),
            "spaces_to_tabs" => spaces_to_tabs(line, tab, false),
            "spaces_to_tabs_leading" => spaces_to_tabs(line, tab, true),
            "indent_increase" if !line.is_empty() => format!("{}{line}", " ".repeat(tab)),
            "indent_increase_tab" if !line.is_empty() => format!("\t{line}"),
            "indent_decrease" => decrease_indent(line, tab),
            _ => line.to_string(),
        };
        edits.push(TextEdit::new(l.start, l.end, new));
    }
    Some(outcome(t, edits))
}

/// Column-aware: a tab advances to the next multiple of `tab`.
pub fn tabs_to_spaces(line: &str, tab: usize, leading_only: bool) -> String {
    let mut out = String::with_capacity(line.len());
    let mut col = 0;
    let mut leading = true;
    for c in line.chars() {
        if c == '\t' && (!leading_only || leading) {
            let n = tab - col % tab;
            out.extend(std::iter::repeat_n(' ', n));
            col += n;
        } else {
            if c != ' ' && c != '\t' {
                leading = false;
            }
            out.push(c);
            col += 1;
        }
    }
    out
}

/// Replace runs of spaces that reach a tab stop by tabs (+ remaining spaces).
pub fn spaces_to_tabs(line: &str, tab: usize, leading_only: bool) -> String {
    let chars: Vec<char> = line.chars().collect();
    let mut out = String::with_capacity(line.len());
    let (mut i, mut col) = (0, 0);
    while i < chars.len() {
        if chars[i] == ' ' && (!leading_only || chars[..i].iter().all(|&c| c == ' ' || c == '\t')) {
            let mut j = i;
            while j < chars.len() && chars[j] == ' ' {
                j += 1;
            }
            let (mut c, end) = (col, col + (j - i));
            while (c / tab + 1) * tab <= end && tab - c % tab > 1 {
                out.push('\t');
                c = (c / tab + 1) * tab;
            }
            out.extend(std::iter::repeat_n(' ', end - c));
            col = end;
            i = j;
        } else {
            out.push(chars[i]);
            col = if chars[i] == '\t' { (col / tab + 1) * tab } else { col + 1 };
            i += 1;
        }
    }
    out
}

/// Remove at most one indent level of leading whitespace; never touches other characters.
pub fn decrease_indent(line: &str, tab: usize) -> String {
    let mut width = 0;
    let mut cut = 0;
    for (i, c) in line.char_indices() {
        if width >= tab || (c != ' ' && c != '\t') {
            break;
        }
        let w = if c == '\t' { tab - width % tab } else { 1 };
        if width + w > tab {
            break;
        }
        width += w;
        cut = i + 1;
    }
    line[cut..].to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::textops::{apply, sel};

    #[test]
    fn tab_stop_math() {
        assert_eq!(tabs_to_spaces("a\tb", 4, false), "a   b");
        assert_eq!(tabs_to_spaces("\tx\ty", 4, true), "    x\ty");
        assert_eq!(spaces_to_tabs("    x", 4, false), "\tx");
        assert_eq!(spaces_to_tabs("a   b", 4, false), "a\tb");
        assert_eq!(spaces_to_tabs("a  b", 4, false), "a  b"); // does not reach a stop
        assert_eq!(spaces_to_tabs("x    y", 4, true), "x    y"); // leading only
        assert_eq!(spaces_to_tabs("      x", 4, true), "\t  x");
    }

    #[test]
    fn trim_whole_buffer_by_default() {
        assert_eq!(apply("trim_trailing", "a  \nb\t\n", &sel(0, 0), &[]), "a\nb\n");
        assert_eq!(apply("trim_trailing", "a  \nb  ", &sel(0, 2), &[]), "a\nb  ");
        assert_eq!(apply("trim_both", " a \r\n b ", &sel(0, 0), &[]), "a\r\nb");
        assert_eq!(apply("trim_leading", "  a\n\tb", &sel(0, 0), &[]), "a\nb");
    }

    #[test]
    fn indent_levels() {
        assert_eq!(apply("indent_increase", "a\n\nb", &sel(0, 4), &["2"]), "  a\n\n  b");
        assert_eq!(apply("indent_increase_tab", "a", &sel(0, 0), &[]), "\ta");
        assert_eq!(apply("indent_decrease", "    a", &sel(0, 0), &[]), "a");
        assert_eq!(apply("indent_decrease", "      a", &sel(0, 0), &[]), "  a");
        assert_eq!(apply("indent_decrease", "\ta", &sel(0, 0), &[]), "a");
        // mixed, and never removes non-whitespace
        assert_eq!(apply("indent_decrease", "  \tb", &sel(0, 0), &[]), "b");
        assert_eq!(decrease_indent("ab", 4), "ab");
        assert_eq!(decrease_indent(" x y", 4), "x y");
    }
}
