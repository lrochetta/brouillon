//! Indentation guides and the column ruler (both settings-driven, see `editor.indent_guides`
//! and `editor.ruler_column`). Pure arithmetic: the view asks, per visible line, where to draw.

/// Visual width of the leading whitespace of `line` (tab = next multiple of `tab_size`).
pub fn indent_width(line: &str, tab_size: usize) -> usize {
    let ts = tab_size.max(1);
    let mut w = 0;
    for c in line.chars() {
        match c {
            ' ' => w += 1,
            '\t' => w += ts - (w % ts),
            _ => break,
        }
    }
    w
}

pub fn is_blank(line: &str) -> bool {
    line.trim().is_empty()
}

/// Indent width that a line's guides follow: its own when it has text, otherwise the smaller
/// of the nearest non-blank neighbours (so a guide runs unbroken through an empty line, like
/// every editor with guides). Looks at most `LOOKAROUND` lines each way.
pub fn effective_indent(line_at: &dyn Fn(usize) -> Option<String>, idx: usize, tab_size: usize) -> usize {
    const LOOKAROUND: usize = 64;
    let Some(l) = line_at(idx) else { return 0 };
    if !is_blank(&l) {
        return indent_width(&l, tab_size);
    }
    let up = (1..=LOOKAROUND).map_while(|d| idx.checked_sub(d)).find_map(|i| line_at(i).filter(|s| !is_blank(s)));
    let down = (1..=LOOKAROUND).find_map(|d| line_at(idx + d).filter(|s| !is_blank(s)));
    match (up, down) {
        (Some(u), Some(d)) => indent_width(&u, tab_size).min(indent_width(&d, tab_size)),
        _ => 0,
    }
}

/// Columns (0-based, in cells from the text origin) at which a guide is drawn for a line whose
/// indent is `indent` cells: one per full indent level strictly inside the indent.
pub fn guide_columns(indent: usize, tab_size: usize) -> Vec<usize> {
    let ts = tab_size.max(1);
    (0..indent).step_by(ts).collect()
}

/// `editor.ruler_column` normalised: 0 / negative / absurd = off.
pub fn ruler_column(setting: i64) -> Option<usize> {
    (1..=1000).contains(&setting).then_some(setting as usize)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn indent_width_counts_tabs_to_the_next_stop() {
        assert_eq!(indent_width("    x", 4), 4);
        assert_eq!(indent_width("\tx", 4), 4);
        assert_eq!(indent_width("  \tx", 4), 4);
        assert_eq!(indent_width("\t\t  x", 4), 10);
        assert_eq!(indent_width("x", 4), 0);
        assert_eq!(indent_width("   ", 4), 3);
    }

    #[test]
    fn guides_sit_on_each_indent_level_inside_the_indent() {
        assert_eq!(guide_columns(0, 4), Vec::<usize>::new());
        assert_eq!(guide_columns(4, 4), vec![0]);
        assert_eq!(guide_columns(8, 4), vec![0, 4]);
        assert_eq!(guide_columns(9, 4), vec![0, 4, 8]);
        assert_eq!(guide_columns(6, 2), vec![0, 2, 4]);
    }

    #[test]
    fn blank_lines_inherit_the_smaller_neighbour_indent() {
        let doc = ["fn a() {", "    if x {", "        y();", "", "    }", "}"];
        let at = |i: usize| doc.get(i).map(|s| s.to_string());
        assert_eq!(effective_indent(&at, 2, 4), 8);
        assert_eq!(effective_indent(&at, 3, 4), 4); // min(8, 4)
        let edge = ["", "x"];
        let at2 = |i: usize| edge.get(i).map(|s| s.to_string());
        assert_eq!(effective_indent(&at2, 0, 4), 0);
        assert_eq!(effective_indent(&at2, 9, 4), 0);
    }

    #[test]
    fn ruler_setting_is_normalised() {
        assert_eq!(ruler_column(0), None);
        assert_eq!(ruler_column(-3), None);
        assert_eq!(ruler_column(80), Some(80));
        assert_eq!(ruler_column(5000), None);
    }
}
