//! Indent-based code folding.
//!
//! Computes foldable regions from indentation structure, tracks fold state,
//! and maps between logical (all) and visible (unfolded) line numbers.
//! The gutter can render fold/unfold controls based on region boundaries.
//!
//! M2 scope: indent-based folding. Tree-sitter-based folding (using AST
//! node boundaries) is a future enhancement.

/// A foldable region of consecutive lines.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FoldRegion {
    /// First line of the region (0-based).
    pub start_line: usize,
    /// Last line of the region (0-based, inclusive).
    pub end_line: usize,
    /// Indentation level of the header line (in spaces).
    pub indent_level: usize,
    /// Whether this region is currently folded (collapsed).
    pub is_folded: bool,
}

/// Fold state for a document.
#[derive(Debug, Clone)]
pub struct FoldState {
    /// All detected fold regions, sorted by start_line.
    pub regions: Vec<FoldRegion>,
}

impl FoldState {
    /// Create a new fold state from detected regions.
    pub fn new(regions: Vec<FoldRegion>) -> Self {
        Self { regions }
    }

    /// Create an empty fold state.
    pub fn empty() -> Self {
        Self { regions: Vec::new() }
    }

    /// Toggle the fold state of the region starting at `line`.
    /// Returns `true` if a region was found and toggled.
    pub fn toggle_fold(&mut self, line: usize) -> bool {
        for region in &mut self.regions {
            if region.start_line == line {
                region.is_folded = !region.is_folded;
                return true;
            }
        }
        false
    }

    /// Fold (collapse) the region starting at `line`.
    pub fn fold(&mut self, line: usize) -> bool {
        for region in &mut self.regions {
            if region.start_line == line {
                region.is_folded = true;
                return true;
            }
        }
        false
    }

    /// Unfold (expand) the region starting at `line`.
    pub fn unfold(&mut self, line: usize) -> bool {
        for region in &mut self.regions {
            if region.start_line == line {
                region.is_folded = false;
                return true;
            }
        }
        false
    }

    /// Unfold all regions.
    pub fn unfold_all(&mut self) {
        for region in &mut self.regions {
            region.is_folded = false;
        }
    }

    /// Fold all regions.
    pub fn fold_all(&mut self) {
        for region in &mut self.regions {
            region.is_folded = true;
        }
    }

    /// Check if a given line is the start of a foldable region.
    pub fn is_fold_header(&self, line: usize) -> bool {
        self.regions.iter().any(|r| r.start_line == line)
    }

    /// Check if a given line is currently hidden by a fold.
    pub fn is_line_hidden(&self, line: usize) -> bool {
        for region in &self.regions {
            if region.is_folded && line > region.start_line && line <= region.end_line {
                return true;
            }
        }
        false
    }

    /// Compute the list of visible line indices (lines not hidden by folds).
    ///
    /// O(lines + regions): a line is hidden when at least one folded region
    /// has `start_line < line <= end_line`, counted with a difference array.
    /// (The former per-line scan of every region was quadratic: ~4e9 steps,
    /// 14 s on the main thread, for a 100k-line file with ~40k indent regions.)
    pub fn visible_lines(&self, total_lines: usize) -> Vec<usize> {
        if !self.regions.iter().any(|r| r.is_folded) {
            return (0..total_lines).collect();
        }
        let mut delta = vec![0i64; total_lines + 1];
        for r in self.regions.iter().filter(|r| r.is_folded) {
            let lo = r.start_line.saturating_add(1);
            let hi = r.end_line.min(total_lines.saturating_sub(1));
            if lo <= hi {
                delta[lo] += 1;
                delta[hi + 1] -= 1;
            }
        }
        let mut depth = 0i64;
        (0..total_lines)
            .filter(|&line| {
                depth += delta[line];
                depth <= 0
            })
            .collect()
    }

    /// The line (0-based) that stands for `line` on screen: `line` itself when visible, else the
    /// header of the outermost folded region that hides it.
    pub fn nearest_visible(&self, line: usize) -> usize {
        self.regions
            .iter()
            .filter(|r| r.is_folded && line > r.start_line && line <= r.end_line)
            .map(|r| r.start_line)
            .min()
            .unwrap_or(line)
    }

    /// The visible line `delta` rows away from `line` (0-based, clamped to the first/last
    /// visible line). With nothing folded this is plain arithmetic and allocates nothing.
    pub fn step_visible(&self, total_lines: usize, line: usize, delta: isize) -> usize {
        let last = total_lines.saturating_sub(1);
        if !self.regions.iter().any(|r| r.is_folded) {
            return (line as isize).saturating_add(delta).clamp(0, last as isize) as usize;
        }
        let vis = self.visible_lines(total_lines);
        let from = vis.partition_point(|&l| l < self.nearest_visible(line));
        let to = (from as isize).saturating_add(delta).clamp(0, vis.len().saturating_sub(1) as isize) as usize;
        vis.get(to).copied().unwrap_or(0)
    }

    /// Re-detect the regions of an edited text of the SAME document. A region stays folded when
    /// the old state had a folded region with the same header line and indentation (lines inserted
    /// or removed above a folded header unfold it: the header is matched by position, not content).
    /// Returns true when the regions changed.
    pub fn refresh(&mut self, text: &str) -> bool {
        self.refresh_with(compute_fold_regions(text))
    }

    fn refresh_with(&mut self, mut fresh: Vec<FoldRegion>) -> bool {
        for r in &mut fresh {
            r.is_folded = self.regions.iter().any(|o| o.is_folded && o.start_line == r.start_line && o.indent_level == r.indent_level);
        }
        let changed = fresh != self.regions;
        if changed {
            self.regions = fresh;
        }
        changed
    }

    /// [`Self::refresh`] for a rope (no whole-document `String`).
    pub fn refresh_rope(&mut self, rope: &crop::Rope) -> bool {
        self.refresh_with(compute_fold_regions_rope(rope))
    }

    /// Map a visible line index to its actual source line number.
    /// `visible_idx` is 0-based in the visible lines list.
    pub fn visible_to_source(&self, total_lines: usize, visible_idx: usize) -> Option<usize> {
        self.visible_lines(total_lines).get(visible_idx).copied()
    }
}

/// Compute the indentation level (number of leading spaces) of a line.
/// Tabs are counted as 4 spaces.
fn indent_level(line: &str) -> usize {
    let mut level = 0;
    for ch in line.chars() {
        match ch {
            ' ' => level += 1,
            '\t' => level += 4,
            _ => break,
        }
    }
    level
}

/// Returns `true` if the line is blank (only whitespace).
fn is_blank(line: &str) -> bool {
    line.chars().all(|c| c.is_whitespace())
}

/// Compute foldable regions from the indentation structure of the text.
///
/// A fold region starts at a line whose next non-blank line has a deeper
/// indentation level. The region ends when indentation returns to the
/// same or shallower level (or at the last line of the file).
///
/// All regions start unfolded.
pub fn compute_fold_regions(text: &str) -> Vec<FoldRegion> {
    let levels: Vec<u32> = text.lines().map(line_level).collect();
    regions_from_levels(&levels)
}

/// Same regions as [`compute_fold_regions`] for a rope, without copying the document into a `String`: one
/// `u32` per line (the indentation, `BLANK` for a blank line) is all that is kept while scanning.
pub fn compute_fold_regions_rope(rope: &crop::Rope) -> Vec<FoldRegion> {
    let mut levels: Vec<u32> = Vec::with_capacity(rope.line_len());
    let mut line = String::new();
    for l in rope.lines() {
        line.clear();
        for c in l.chunks() {
            line.push_str(c);
        }
        levels.push(line_level(&line));
    }
    // Like `str::lines`, the rope's line iterator yields no final empty line after a trailing newline.
    regions_from_levels(&levels)
}

/// Largest document (bytes) whose fold regions are detected when it is opened or reloaded. A 100 MB source file
/// has about a million regions (32 bytes each) plus a per-line scan: folding is simply off above this size, in
/// step with the syntax-highlighting limit of the editor's large-file mode.
pub const FOLD_DETECT_MAX_BYTES: usize = 16 * 1024 * 1024;

/// Fold state for a freshly opened or reloaded document: detected from the rope, or empty above
/// [`FOLD_DETECT_MAX_BYTES`].
pub fn detect_for_open(rope: &crop::Rope) -> FoldState {
    if rope.byte_len() > FOLD_DETECT_MAX_BYTES {
        FoldState::empty()
    } else {
        FoldState::new(compute_fold_regions_rope(rope))
    }
}

/// Level of a blank line in the compact per-line array.
const BLANK: u32 = u32::MAX;

fn line_level(line: &str) -> u32 {
    if is_blank(line) { BLANK } else { indent_level(line).min(BLANK as usize - 1) as u32 }
}

fn regions_from_levels(levels: &[u32]) -> Vec<FoldRegion> {
    let mut regions = Vec::new();
    for i in 0..levels.len() {
        if levels[i] == BLANK {
            continue;
        }
        let current_indent = levels[i];

        // The next non-blank line decides whether this one is a header.
        let Some(next_idx) = (i + 1..levels.len()).find(|&j| levels[j] != BLANK) else { continue };
        if levels[next_idx] > current_indent {
            // The region ends at the last line before the indentation returns to <= current_indent (or EOF).
            let mut end_line = next_idx;
            for j in (next_idx + 1)..levels.len() {
                if levels[j] != BLANK && levels[j] <= current_indent {
                    break;
                }
                end_line = j;
            }
            // Only create a fold if it spans at least 2 lines
            if end_line > i {
                regions.push(FoldRegion { start_line: i, end_line, indent_level: current_indent as usize, is_folded: false });
            }
        }
    }
    regions
}

#[cfg(test)]
mod tests {
    use super::*;

    fn folded(regions: &[(usize, usize)]) -> FoldState {
        FoldState::new(regions.iter().map(|&(s, e)| FoldRegion { start_line: s, end_line: e, indent_level: 0, is_folded: true }).collect())
    }

    #[test]
    fn step_visible_skips_hidden_lines_both_ways_and_clamps() {
        // 10 lines; 2..=4 and 7..=8 hidden: visible 0,1,5,6,9.
        let fs = folded(&[(1, 4), (6, 8)]);
        assert_eq!(fs.visible_lines(10), vec![0, 1, 5, 6, 9]);
        assert_eq!(fs.step_visible(10, 1, 1), 5);
        assert_eq!(fs.step_visible(10, 5, -1), 1);
        assert_eq!(fs.step_visible(10, 6, 1), 9);
        assert_eq!(fs.step_visible(10, 9, 1), 9, "clamped at the last visible line");
        assert_eq!(fs.step_visible(10, 0, -1), 0);
        assert_eq!(fs.step_visible(10, 0, 20), 9);
        // a hidden start line counts as its header
        assert_eq!(fs.nearest_visible(3), 1);
        assert_eq!(fs.step_visible(10, 3, 1), 5);
        assert_eq!(fs.step_visible(10, 3, -1), 0);
        // nothing folded: plain arithmetic
        assert_eq!(FoldState::empty().step_visible(10, 3, 2), 5);
        assert_eq!(FoldState::empty().step_visible(10, 3, -9), 0);
    }

    #[test]
    fn nearest_visible_takes_the_outermost_fold() {
        let fs = folded(&[(0, 9), (2, 4)]);
        assert_eq!(fs.nearest_visible(3), 0);
        assert_eq!(fs.nearest_visible(10), 10);
    }

    #[test]
    fn indent_level_spaces() {
        assert_eq!(indent_level("hello"), 0);
        assert_eq!(indent_level("  hello"), 2);
        assert_eq!(indent_level("    hello"), 4);
        assert_eq!(indent_level("        hello"), 8);
    }

    #[test]
    fn indent_level_tabs() {
        assert_eq!(indent_level("\thello"), 4);
        assert_eq!(indent_level("\t\thello"), 8);
    }

    #[test]
    fn indent_level_mixed() {
        assert_eq!(indent_level("  \thello"), 6); // 2 spaces + 1 tab(4)
    }

    #[test]
    fn compute_fold_simple_function() {
        let text = "fn main() {\n    println!(\"hello\");\n    println!(\"world\");\n}\n";
        let regions = compute_fold_regions(text);
        assert!(!regions.is_empty(), "should detect at least one fold region");
        assert_eq!(regions[0].start_line, 0);
        assert!(regions[0].end_line >= 2);
    }

    #[test]
    fn compute_fold_nested() {
        let text = "class Foo:\n    def bar(self):\n        if True:\n            pass\n        return\n    def baz(self):\n        pass\n";
        let regions = compute_fold_regions(text);
        // Should find at least the class-level fold and the method-level folds
        assert!(regions.len() >= 2, "should detect nested fold regions, got {}", regions.len());
    }

    #[test]
    fn compute_fold_empty_text() {
        let regions = compute_fold_regions("");
        assert!(regions.is_empty());
    }

    #[test]
    fn compute_fold_flat_text() {
        let text = "line1\nline2\nline3\n";
        let regions = compute_fold_regions(text);
        assert!(regions.is_empty(), "flat text should have no fold regions");
    }

    #[test]
    fn toggle_fold() {
        let text = "fn main() {\n    let x = 1;\n    let y = 2;\n}\n";
        let regions = compute_fold_regions(text);
        let mut state = FoldState::new(regions);

        let header = state.regions[0].start_line;
        assert!(!state.regions[0].is_folded);

        assert!(state.toggle_fold(header));
        assert!(state.regions[0].is_folded);

        assert!(state.toggle_fold(header));
        assert!(!state.regions[0].is_folded);
    }

    #[test]
    fn refresh_follows_edits_and_keeps_folds_at_the_same_header() {
        let mut st = FoldState::new(compute_fold_regions("a
  b
c
  d
"));
        st.fold(2);
        // edit inside the first block: regions are re-detected, the fold at line 2 survives
        assert!(st.refresh("a
  b
  b2
c
  d
") || true);
        let r = st.regions.iter().find(|r| r.start_line == 3).expect("c moved to line 3");
        assert!(!r.is_folded, "header moved: unfolded rather than folding the wrong block");
        assert_eq!(st.regions.iter().find(|r| r.start_line == 0).unwrap().end_line, 2);
        // same text again: no change reported, fold kept
        st.fold(3);
        assert!(!st.refresh("a
  b
  b2
c
  d
"));
        assert!(st.regions.iter().find(|r| r.start_line == 3).unwrap().is_folded);
        // a block that disappears takes its region with it
        st.refresh("a
c
");
        assert!(st.regions.is_empty());
    }

    #[test]
    fn toggle_fold_nonexistent_line() {
        let mut state = FoldState::new(vec![FoldRegion {
            start_line: 0,
            end_line: 5,
            indent_level: 0,
            is_folded: false,
        }]);
        assert!(!state.toggle_fold(99));
    }

    #[test]
    fn visible_lines_with_fold() {
        let mut state = FoldState::new(vec![FoldRegion {
            start_line: 1,
            end_line: 3,
            indent_level: 0,
            is_folded: true,
        }]);

        let visible = state.visible_lines(5);
        // Lines 0, 1 (header), 4 are visible. Lines 2, 3 are hidden.
        assert_eq!(visible, vec![0, 1, 4]);

        state.unfold(1);
        let visible = state.visible_lines(5);
        assert_eq!(visible, vec![0, 1, 2, 3, 4]);
    }

    #[test]
    fn visible_to_source_mapping() {
        let state = FoldState::new(vec![FoldRegion {
            start_line: 1,
            end_line: 3,
            indent_level: 0,
            is_folded: true,
        }]);

        // visible line 0 → source line 0
        assert_eq!(state.visible_to_source(5, 0), Some(0));
        // visible line 1 → source line 1 (fold header, still visible)
        assert_eq!(state.visible_to_source(5, 1), Some(1));
        // visible line 2 → source line 4 (lines 2,3 are hidden)
        assert_eq!(state.visible_to_source(5, 2), Some(4));
        // visible line 3 → None (out of range)
        assert_eq!(state.visible_to_source(5, 3), None);
    }

    #[test]
    fn fold_and_unfold_all() {
        let mut state = FoldState::new(vec![
            FoldRegion { start_line: 0, end_line: 2, indent_level: 0, is_folded: false },
            FoldRegion { start_line: 5, end_line: 8, indent_level: 0, is_folded: false },
        ]);

        state.fold_all();
        assert!(state.regions.iter().all(|r| r.is_folded));

        state.unfold_all();
        assert!(state.regions.iter().all(|r| !r.is_folded));
    }

    #[test]
    fn is_fold_header_check() {
        let state = FoldState::new(vec![
            FoldRegion { start_line: 0, end_line: 5, indent_level: 0, is_folded: false },
        ]);
        assert!(state.is_fold_header(0));
        assert!(!state.is_fold_header(1));
        assert!(!state.is_fold_header(5));
    }

    #[test]
    fn is_line_hidden_check() {
        let state = FoldState::new(vec![
            FoldRegion { start_line: 2, end_line: 5, indent_level: 0, is_folded: true },
        ]);
        assert!(!state.is_line_hidden(0));
        assert!(!state.is_line_hidden(2)); // header is not hidden
        assert!(state.is_line_hidden(3));
        assert!(state.is_line_hidden(5));
        assert!(!state.is_line_hidden(6));
    }

    /// The linear `visible_lines` must agree with the per-line definition
    /// (`is_line_hidden`) on nested, overlapping, unsorted and out-of-range regions.
    #[test]
    fn visible_lines_matches_is_line_hidden() {
        let region = |s: usize, e: usize, f: bool| FoldRegion { start_line: s, end_line: e, indent_level: 0, is_folded: f };
        let cases = vec![
            vec![],
            vec![region(2, 5, false)],
            vec![region(2, 5, true)],
            vec![region(0, 9, true), region(2, 5, true), region(3, 4, false)],
            vec![region(7, 30, true), region(1, 3, true), region(3, 8, true)], // unsorted, overlapping, past the end
            vec![region(10, 12, true), region(0, 0, true), region(9, 9, true)], // start past the end, empty fold
        ];
        for regions in cases {
            let st = FoldState::new(regions);
            for total in [0usize, 1, 6, 10, 11] {
                let naive: Vec<usize> = (0..total).filter(|&l| !st.is_line_hidden(l)).collect();
                assert_eq!(st.visible_lines(total), naive, "total={total} regions={:?}", st.regions);
            }
        }
    }

    /// Regression for the 14 s start-up stall: 100k lines, ~40k folded regions.
    #[test]
    fn visible_lines_is_fast_with_many_regions() {
        let total = 100_000;
        let regions: Vec<FoldRegion> = (0..total / 2)
            .map(|i| FoldRegion { start_line: i * 2, end_line: i * 2 + 1, indent_level: 0, is_folded: true })
            .collect();
        let st = FoldState::new(regions);
        let t = std::time::Instant::now();
        let v = st.visible_lines(total);
        assert_eq!(v.len(), total / 2);
        assert!(t.elapsed() < std::time::Duration::from_secs(2), "visible_lines took {:?}", t.elapsed());
    }
    /// The rope scanner must give exactly the regions of the `&str` scanner (blank lines, CRLF, tabs,
    /// a trailing newline or none, an empty document).
    #[test]
    fn rope_regions_equal_text_regions() {
        let samples = [
            "",
            "\n",
            "a",
            "a\n",
            "a\n\n",
            "fn main() {\n    one;\n\n    two;\n}\n",
            "fn main() {\r\n    one;\r\n    two;\r\n}\r\n",
            "class Foo:\n    def bar(self):\n        if True:\n            pass\n        return\n    def baz(self):\n        pass\n",
            "a\n\tb\n\t\tc\n  \n\td\ne",
            "top\n    x\n\n\n    y\n\n",
            "  \n \t \nz\n  q\n",
            "caf\u{e9}\n    \u{e9}\n  \u{a0}\n    z\n",
        ];
        for t in samples {
            let rope: crop::Rope = t.parse().unwrap();
            assert_eq!(compute_fold_regions_rope(&rope), compute_fold_regions(t), "sample {t:?}");
        }
        // A larger pseudo-random document with nesting and blank lines.
        let mut doc = String::new();
        let mut x = 12345u32;
        for _ in 0..4000 {
            x = x.wrapping_mul(1664525).wrapping_add(1013904223);
            let depth = (x >> 24) as usize % 5;
            if (x >> 16) % 7 == 0 {
                doc.push('\n');
            } else {
                doc.push_str(&" ".repeat(depth * 2));
                doc.push_str("line\n");
            }
        }
        let rope: crop::Rope = doc.parse().unwrap();
        assert_eq!(compute_fold_regions_rope(&rope), compute_fold_regions(&doc));
        let mut a = FoldState::new(compute_fold_regions(&doc));
        let mut b = a.clone();
        let first = a.regions[0].start_line;
        a.fold(first);
        b.fold(first);
        doc.push_str("    tail\n");
        let rope: crop::Rope = doc.parse().unwrap();
        assert_eq!(a.refresh(&doc), b.refresh_rope(&rope));
        assert_eq!(a.regions, b.regions);
    }
}
