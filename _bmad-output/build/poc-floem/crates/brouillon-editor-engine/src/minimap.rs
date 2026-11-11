//! Minimap model: a fixed number of sampled rows, never the whole document.
//!
//! The cost of a rebuild is `O(rows)` line fetches whatever the file size (a 100 MB file maps to
//! the same ~300 rows as a 100-line one), and the view rebuilds it debounced, not per keystroke.

/// One minimap row = the line sampled for this slice of the document.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MiniRow {
    /// 0-based sampled line.
    pub line: usize,
    /// Cells of leading indentation (clamped).
    pub indent: u8,
    /// Visible characters (clamped to `MAX_LEN`); 0 = blank line.
    pub len: u8,
}

pub const MAX_LEN: usize = 120;
pub const MAX_INDENT: usize = 40;

/// Sample `rows` lines out of `line_count`, evenly spread. With fewer lines than rows every line
/// is shown (one row each). `line_at` returns the text of a 0-based line.
pub fn sample(line_count: usize, rows: usize, tab_size: usize, line_at: &dyn Fn(usize) -> Option<String>) -> Vec<MiniRow> {
    if line_count == 0 || rows == 0 {
        return Vec::new();
    }
    let n = rows.min(line_count);
    (0..n)
        .map(|i| {
            // Evenly spaced, first and last row pinned to first/last line.
            let line = if n == 1 { 0 } else { (i as u128 * (line_count as u128 - 1) / (n as u128 - 1)) as usize };
            let text = line_at(line).unwrap_or_default();
            let t = text.trim_end_matches(['\r', '\n']);
            let indent = crate::guides::indent_width(t, tab_size).min(MAX_INDENT);
            let len = t.trim().chars().count().min(MAX_LEN);
            MiniRow { line, indent: indent as u8, len: len as u8 }
        })
        .collect()
}

/// Map a click at `y_frac` (0..1 down the minimap) to the document line to scroll to.
pub fn line_at_fraction(line_count: usize, y_frac: f64) -> usize {
    if line_count == 0 {
        return 0;
    }
    ((y_frac.clamp(0.0, 1.0) * (line_count - 1) as f64).round() as usize).min(line_count - 1)
}

/// Fraction (0..1) of the minimap covered by the editor viewport starting at `first_line` and
/// showing `visible` lines: `(top, height)`, height at least `min_h`.
pub fn viewport_span(line_count: usize, first_line: usize, visible: usize, min_h: f64) -> (f64, f64) {
    if line_count == 0 {
        return (0.0, 1.0);
    }
    let top = first_line as f64 / line_count as f64;
    let h = (visible as f64 / line_count as f64).max(min_h).min(1.0);
    (top.min(1.0 - h), h)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn small_files_show_every_line() {
        let doc = ["a", "    b", "", "cc"];
        let rows = sample(4, 300, 4, &|i| doc.get(i).map(|s| s.to_string()));
        assert_eq!(rows.len(), 4);
        assert_eq!(rows[1], MiniRow { line: 1, indent: 4, len: 1 });
        assert_eq!(rows[2].len, 0);
        assert_eq!(rows[3].line, 3);
    }

    #[test]
    fn huge_files_cost_o_rows_fetches() {
        use std::cell::Cell;
        let fetched = Cell::new(0usize);
        let rows = sample(5_000_000, 300, 4, &|i| {
            fetched.set(fetched.get() + 1);
            Some(format!("{}line {i}", " ".repeat(i % 3 * 4)))
        });
        assert_eq!(rows.len(), 300);
        assert_eq!(fetched.get(), 300);
        assert_eq!(rows[0].line, 0);
        assert_eq!(rows[299].line, 4_999_999);
        assert!(rows.windows(2).all(|w| w[0].line < w[1].line));
    }

    #[test]
    fn click_and_viewport_mapping() {
        assert_eq!(line_at_fraction(1000, 0.0), 0);
        assert_eq!(line_at_fraction(1000, 1.0), 999);
        assert_eq!(line_at_fraction(1000, 0.5), 500);
        assert_eq!(line_at_fraction(0, 0.5), 0);
        let (top, h) = viewport_span(1000, 500, 50, 0.02);
        assert!((top - 0.5).abs() < 1e-9 && (h - 0.05).abs() < 1e-9);
        let (top, h) = viewport_span(10, 9, 50, 0.02);
        assert_eq!((top, h), (0.0, 1.0));
        let (_, h) = viewport_span(10_000_000, 0, 50, 0.02);
        assert_eq!(h, 0.02);
    }
}
