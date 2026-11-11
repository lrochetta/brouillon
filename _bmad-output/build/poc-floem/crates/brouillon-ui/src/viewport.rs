//! Viewport model for huge documents (docs: re/54, section 5/6/8).
//!
//! The editor never asks "give me the text"; it asks a [`ViewportSource`] for a few lines, for an
//! estimated line count and for how far the line index has got. Everything here is pure logic
//! (no floem) except [`ViewportSignals`], the three signals the views read.
//!
//! * `Buffer` implements the trait today (rope-backed, always exact, index progress 1.0). The
//!   lazy store plugs in by changing that one `impl` (see the end of this file's `impl` block).
//! * Rows beyond the indexed part are placeholders, not guesses: the user's line number is the
//!   anchor, the row text appears when the index passes it, nothing moves on screen. Only the
//!   scrollbar extent is an estimate, and it never shrinks below the anchor ([`stable_total_rows`]).
//! * A line longer than [`LONG_LINE_BYTES`] is drawn as a column window ([`LineWindow`]), so a
//!   100 MB single-line file costs a few KB per frame.

use std::ops::Range;

use floem::reactive::{create_rw_signal, RwSignal, SignalGet, SignalUpdate};

use brouillon_editor_engine::buffer::Buffer;

// ---------------------------------------------------------------------------------------------
// The source
// ---------------------------------------------------------------------------------------------

/// Columns `[first_col, first_col + text.chars())` of one line, without its line break.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct LineWindow {
    pub text: String,
    pub first_col: usize,
    /// The line goes on to the right of the window.
    pub more_right: bool,
    /// Total characters of the line, only when the window reached its end (else unknown).
    pub line_cols: Option<usize>,
}

/// What the editor view needs from a document. Cheap calls only: none may scan the file.
pub trait ViewportSource {
    fn byte_len(&self) -> usize;
    /// Line count: exact once [`is_exact`](Self::is_exact), else an estimate (bytes / average line
    /// length of the part already indexed).
    fn line_count_estimate(&self) -> usize;
    fn is_exact(&self) -> bool;
    /// Lines whose number is certain and whose text can be read now (all of them when exact).
    fn indexed_lines(&self) -> usize;
    /// 0.0..=1.0, 1.0 when the index is complete.
    fn index_progress(&self) -> f32;
    /// Full text of `range` (clamped to what is indexed; unreadable lines are simply missing).
    /// Only for ordinary lines: use [`read_line_window`](Self::read_line_window) when
    /// [`line_len_bytes`](Self::line_len_bytes) is above [`LONG_LINE_BYTES`].
    fn read_lines(&self, range: Range<usize>) -> Vec<String>;
    /// Byte length of a line including its break, `None` if out of range or not indexed.
    fn line_len_bytes(&self, line: usize) -> Option<usize>;
    /// At most `max_cols` characters of `line` starting at character column `first_col`.
    fn read_line_window(&self, line: usize, first_col: usize, max_cols: usize) -> LineWindow;
}

/// Window of an in-memory line (shared by implementations that already hold a `&str`).
pub fn window_of_str(line: &str, first_col: usize, max_cols: usize) -> LineWindow {
    window_of_chars(line.chars(), first_col, max_cols)
}

fn window_of_chars(chars: impl Iterator<Item = char>, first_col: usize, max_cols: usize) -> LineWindow {
    let mut text = String::new();
    let (mut n, mut seen) = (0usize, 0usize);
    let mut more_right = false;
    let mut ended = true;
    for ch in chars {
        if ch == '\n' || ch == '\r' {
            break;
        }
        if seen < first_col {
            seen += 1;
            continue;
        }
        if n == max_cols {
            more_right = true;
            ended = false;
            break;
        }
        text.push(ch);
        n += 1;
        seen += 1;
    }
    LineWindow { text, first_col, more_right, line_cols: if ended { Some(seen) } else { None } }
}

/// `Buffer` is the source for both backings. A rope is always exact and fully indexed; a lazy buffer
/// answers from its adapters without ever waiting for the index (`line_count_hint`,
/// `byte_of_line_ready`, `index_progress`). Only lines below `indexed_lines()` may be read.
impl ViewportSource for Buffer {
    fn byte_len(&self) -> usize {
        Buffer::byte_len(self)
    }
    fn line_count_estimate(&self) -> usize {
        self.line_count_hint().0
    }
    fn is_exact(&self) -> bool {
        self.line_count_hint().1
    }
    fn indexed_lines(&self) -> usize {
        let (n, exact) = self.line_count_hint();
        if exact {
            return n;
        }
        // Largest `k` whose start is ready (monotone); the last ready line's end may not be known yet, so it is
        // not offered. O(log n) cheap probes, none of them waits.
        let (mut lo, mut hi) = (0usize, n.saturating_add(1));
        while lo + 1 < hi {
            let mid = lo + (hi - lo) / 2;
            if self.byte_of_line_ready(mid).is_some() {
                lo = mid;
            } else {
                hi = mid;
            }
        }
        lo
    }
    fn index_progress(&self) -> f32 {
        let (done, total) = Buffer::index_progress(self);
        if total == 0 {
            1.0
        } else {
            (done as f64 / total as f64).clamp(0.0, 1.0) as f32
        }
    }
    fn read_lines(&self, range: Range<usize>) -> Vec<String> {
        self.visible_lines(range.start, range.end)
    }
    fn line_len_bytes(&self, line: usize) -> Option<usize> {
        self.line_byte_range(line).map(|r| r.len())
    }
    fn read_line_window(&self, line: usize, first_col: usize, max_cols: usize) -> LineWindow {
        let Some(r) = self.line_byte_range(line) else {
            return LineWindow { first_col, line_cols: Some(0), ..Default::default() };
        };
        // Stream the line's chunks and stop as soon as the window is full: a 100 MB line costs the
        // columns before the window plus the window, never the line.
        let mut text = String::new();
        let (mut n, mut seen) = (0usize, 0usize);
        let (mut more_right, mut ended) = (false, true);
        self.visit_str(r, &mut |chunk: &str| {
            for ch in chunk.chars() {
                if ch == '\n' || ch == '\r' {
                    return false;
                }
                if seen < first_col {
                    seen += 1;
                    continue;
                }
                if n == max_cols {
                    more_right = true;
                    ended = false;
                    return false;
                }
                text.push(ch);
                n += 1;
                seen += 1;
            }
            true
        });
        LineWindow { text, first_col, more_right, line_cols: if ended { Some(seen) } else { None } }
    }
}

// ---------------------------------------------------------------------------------------------
// Index state seen by the views
// ---------------------------------------------------------------------------------------------

/// What the views know about the index. Published at a coarse rate (whole percents).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct IndexSnapshot {
    pub exact: bool,
    pub estimate: usize,
    pub indexed_lines: usize,
    /// Whole percent, 0..=100.
    pub percent: u8,
}

impl IndexSnapshot {
    /// The state of a document that has no index at all (rope): the buffer is authoritative.
    pub const EXACT: IndexSnapshot = IndexSnapshot { exact: true, estimate: 0, indexed_lines: usize::MAX, percent: 100 };

    pub fn of(src: &dyn ViewportSource) -> Self {
        let exact = src.is_exact();
        let percent = if exact { 100 } else { ((src.index_progress().clamp(0.0, 1.0) * 100.0).floor() as u8).min(99) };
        IndexSnapshot { exact, estimate: src.line_count_estimate(), indexed_lines: src.indexed_lines(), percent }
    }

    /// Publish only when something a user can see changed: the whole percent or the exact flag.
    /// A worker that reports every 4 MB block therefore causes at most 100 UI updates per file.
    pub fn worth_publishing(&self, prev: &IndexSnapshot) -> bool {
        self.exact != prev.exact || self.percent != prev.percent
    }
}

/// Rows of the scrollable content. While the index is incomplete the estimate is used, but never
/// less than the anchor (the line at the top of the screen) plus one screenful, and never less
/// than what is already indexed: the scrollbar thumb may move, the content under the user may not.
pub fn stable_total_rows(snap: &IndexSnapshot, exact_count: impl FnOnce() -> usize, anchor_line: usize, visible_rows: usize) -> usize {
    if snap.exact {
        return exact_count();
    }
    snap.estimate.max(snap.indexed_lines).max(anchor_line + visible_rows + 1)
}

/// How a row has to be drawn right now.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RowState {
    /// Text can be read; the line number is certain.
    Ready,
    /// Beyond the indexed part: draw a placeholder, number shown as an estimate.
    Indexing,
}

pub fn row_state(snap: &IndexSnapshot, line: usize) -> RowState {
    if snap.exact || line < snap.indexed_lines {
        RowState::Ready
    } else {
        RowState::Indexing
    }
}

/// Gutter text for a line: `12`, or `~12` when the number is still an estimate.
pub fn gutter_label(snap: &IndexSnapshot, line: usize) -> String {
    match row_state(snap, line) {
        RowState::Ready => format!("{:>5}", line + 1),
        RowState::Indexing => format!("~{}", line + 1),
    }
}

/// The signals the views read. `sync_from` is the only writer of `snapshot`; the worker that
/// builds the index calls it (through the existing `create_ext_action` pattern, never a timer).
#[derive(Clone, Copy)]
pub struct ViewportSignals {
    pub snapshot: RwSignal<IndexSnapshot>,
    /// 1-based line the user asked for while the index was too short.
    pub pending_goto: RwSignal<Option<usize>>,
    /// First column of the window drawn for very long lines (multiple of [`WINDOW_CHUNK_COLS`]).
    pub hcol: RwSignal<usize>,
    /// Feature gate of the document on screen (from its byte size).
    pub gate: RwSignal<HugeGate>,
}

impl ViewportSignals {
    pub fn new() -> Self {
        Self { snapshot: create_rw_signal(IndexSnapshot::EXACT), pending_goto: create_rw_signal(None), hcol: create_rw_signal(0), gate: create_rw_signal(HugeGate::for_bytes(0)) }
    }

    /// Re-read the source; returns whether the views were told. Call on the UI thread from the
    /// ext action the index worker triggers (at most once per percent is published).
    pub fn sync_from(&self, src: &dyn ViewportSource) -> bool {
        let next = IndexSnapshot::of(src);
        if next.worth_publishing(&self.snapshot.get_untracked()) {
            self.snapshot.set(next);
            true
        } else {
            false
        }
    }

    /// Start of a document that is still being indexed (publishes unconditionally).
    pub fn reset_from(&self, src: &dyn ViewportSource) {
        self.snapshot.set(IndexSnapshot::of(src));
        self.pending_goto.set(None);
        self.hcol.set(0);
    }
}

impl Default for ViewportSignals {
    fn default() -> Self {
        Self::new()
    }
}

/// Rows for `snap`: for the rope (`EXACT`, estimate 0) the buffer's own count.
pub fn total_rows_for(snap: &IndexSnapshot, buffer_lines: impl FnOnce() -> usize, anchor_line: usize, visible_rows: usize) -> usize {
    stable_total_rows(snap, buffer_lines, anchor_line, visible_rows)
}

// ---------------------------------------------------------------------------------------------
// Status bar chip
// ---------------------------------------------------------------------------------------------

/// "Indexing 63%" (plus the pending go-to, if any). `None` once the index is complete.
pub fn index_chip(snap: &IndexSnapshot, pending_goto: Option<usize>) -> Option<String> {
    if snap.exact {
        return None;
    }
    Some(match pending_goto {
        Some(l) => format!("Indexing {}% - going to line {} when ready", snap.percent, l),
        None => format!("Indexing {}%", snap.percent),
    })
}

/// Apply the index state to the status panes: the line count becomes `~N lines` while it is an
/// estimate and an "indexing" chip follows it. Plain words, not a modal, no timer.
pub fn decorate_panes(panes: &mut Vec<crate::status_bar::Pane>, snap: &IndexSnapshot, pending_goto: Option<usize>) {
    let Some(chip) = index_chip(snap, pending_goto) else { return };
    if let Some(p) = panes.iter_mut().find(|p| p.id == "lines") {
        p.label = format!("~{} lines", snap.estimate.max(snap.indexed_lines));
    }
    let at = panes.iter().position(|p| p.id == "lines").map_or(panes.len(), |i| i + 1);
    panes.insert(at, crate::status_bar::Pane { id: "indexing", label: chip, command: None, tone: Some(brouillon_editor_engine::remote::ChipTone::Busy) });
}

// ---------------------------------------------------------------------------------------------
// Go to line while the index is incomplete
// ---------------------------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GotoPlan {
    /// Move the caret to this 1-based line now.
    Go(usize),
    /// The line is past the indexed part: remember it, show the chip, retry on the next update.
    Wait(usize),
}

pub fn plan_goto(snap: &IndexSnapshot, line_1b: usize, exact_count: impl FnOnce() -> usize) -> GotoPlan {
    let want = line_1b.max(1);
    if snap.exact {
        return GotoPlan::Go(want.min(exact_count().max(1)));
    }
    if want <= snap.indexed_lines {
        GotoPlan::Go(want)
    } else {
        GotoPlan::Wait(want)
    }
}

// ---------------------------------------------------------------------------------------------
// Very long lines
// ---------------------------------------------------------------------------------------------

/// Above this a line is drawn as a column window instead of being read whole.
pub const LONG_LINE_BYTES: usize = 16 * 1024;
/// The window start moves in steps of this many columns (rows are rebuilt once per step).
pub const WINDOW_CHUNK_COLS: usize = 256;
/// Columns kept on each side of the visible columns.
pub const WINDOW_MARGIN_COLS: usize = 256;
/// Hard cap of columns ever drawn for one line.
pub const WINDOW_MAX_COLS: usize = 4096;

pub fn is_long_line(len_bytes: Option<usize>) -> bool {
    len_bytes.is_some_and(|n| n > LONG_LINE_BYTES)
}

/// Window start column for a text area scrolled `scroll_x` px to the right of its origin.
pub fn window_start(scroll_x: f64, cell_w: f64) -> usize {
    let first_visible = (scroll_x / cell_w.max(1.0)).floor().max(0.0) as usize;
    first_visible.saturating_sub(WINDOW_MARGIN_COLS) / WINDOW_CHUNK_COLS * WINDOW_CHUNK_COLS
}

/// Columns to draw from `start`: the visible width plus margins, rounded up to whole chunks, capped.
pub fn window_cols(view_w: f64, cell_w: f64) -> usize {
    let visible = (view_w / cell_w.max(1.0)).ceil().max(1.0) as usize;
    let want = visible + 2 * WINDOW_MARGIN_COLS + WINDOW_CHUNK_COLS;
    (want.div_ceil(WINDOW_CHUNK_COLS) * WINDOW_CHUNK_COLS).min(WINDOW_MAX_COLS)
}

/// Character length of a line for click clamping without reading a long line: exact for ordinary
/// lines, the byte length (an upper bound) for long ones.
pub fn line_chars_fast(src: &dyn ViewportSource, line: usize) -> usize {
    match src.line_len_bytes(line) {
        Some(n) if n > LONG_LINE_BYTES => n,
        Some(_) => src.read_lines(line..line + 1).first().map_or(0, |s| s.trim_end_matches(['\r', '\n']).chars().count()),
        None => 0,
    }
}

// ---------------------------------------------------------------------------------------------
// Gating of expensive features on big documents
// ---------------------------------------------------------------------------------------------

/// Thresholds (bytes) at which each feature stops. They match the limits already used in
/// `main.rs` (bracket 256 KB, fold refresh 2 MB, highlight 16 MB) and add the new ones.
pub const BRACKET_MAX_BYTES: usize = 256 * 1024;
pub const FOLD_MAX_BYTES: usize = 2 * 1024 * 1024;
pub const WRAP_MAX_BYTES: usize = 4 * 1024 * 1024;
pub const HUGE_BYTES: usize = 16 * 1024 * 1024;
/// Above this the minimap is not offered at all.
pub const MINIMAP_OFF_BYTES: usize = 512 * 1024 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MinimapMode {
    /// Up to 600 sampled rows, rebuilt 150 ms after the last edit.
    Full,
    /// 96 sampled rows, rebuilt at index milestones and after 1 s of quiet.
    Coarse,
    Off,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HugeGate {
    pub huge: bool,
    pub syntax_highlight: bool,
    pub outline: bool,
    pub folding: bool,
    pub bracket_match: bool,
    pub indent_guides: bool,
    pub word_wrap: bool,
    pub word_completion: bool,
    pub minimap: MinimapMode,
}

impl HugeGate {
    pub fn for_bytes(byte_len: usize) -> Self {
        let huge = byte_len >= HUGE_BYTES;
        HugeGate {
            huge,
            syntax_highlight: !huge,
            outline: !huge,
            folding: byte_len <= FOLD_MAX_BYTES,
            bracket_match: byte_len <= BRACKET_MAX_BYTES,
            indent_guides: !huge,
            word_wrap: byte_len <= WRAP_MAX_BYTES,
            word_completion: !huge,
            minimap: if byte_len >= MINIMAP_OFF_BYTES {
                MinimapMode::Off
            } else if huge {
                MinimapMode::Coarse
            } else {
                MinimapMode::Full
            },
        }
    }

    /// Plain-language status note, only for huge documents. Names what is off, not why in jargon.
    pub fn note(&self, byte_len: usize) -> Option<String> {
        if !self.huge {
            return None;
        }
        let mb = byte_len / (1024 * 1024);
        let minimap = match self.minimap {
            MinimapMode::Full => "",
            MinimapMode::Coarse => " The minimap is simplified.",
            MinimapMode::Off => " The minimap is off.",
        };
        Some(format!(
            "Large file ({mb} MB): colours, folding, outline, word wrap, bracket matching, indent guides and word completion are off so it stays fast.{minimap}"
        ))
    }

    pub fn minimap_rows(&self) -> usize {
        match self.minimap {
            MinimapMode::Full => 600,
            MinimapMode::Coarse => 96,
            MinimapMode::Off => 0,
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Minimap sampling over a source
// ---------------------------------------------------------------------------------------------

/// Sampled minimap rows over `[0, estimate)`: `rows` reads at most, each a 160-column window
/// (never a whole line), unreadable (not yet indexed) rows come out blank. No per-line allocation
/// whatever the line count.
pub fn sample_minimap(src: &dyn ViewportSource, rows: usize, tab_size: usize) -> Vec<brouillon_editor_engine::minimap::MiniRow> {
    let snap = IndexSnapshot::of(src);
    let n = if snap.exact { src.line_count_estimate() } else { snap.estimate.max(snap.indexed_lines) };
    brouillon_editor_engine::minimap::sample(n, rows, tab_size, &|i| {
        if row_state(&snap, i) == RowState::Indexing {
            return None;
        }
        Some(src.read_line_window(i, 0, 160).text)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    /// A document of `n` lines of varying length whose index grows on demand (`advance`).
    struct Fake {
        lens: Vec<usize>,
        indexed: Cell<usize>,
        long_line: Option<usize>,
        reads: Cell<usize>,
    }

    impl Fake {
        fn new(n: usize) -> Self {
            Fake { lens: (0..n).map(|i| 10 + (i * 7) % 23).collect(), indexed: Cell::new(0), long_line: None, reads: Cell::new(0) }
        }
        fn text(&self, i: usize) -> String {
            let body = format!("line {i} ");
            let pad = self.lens[i].saturating_sub(body.len());
            format!("{body}{}", "x".repeat(pad))
        }
        fn indexed_bytes(&self) -> usize {
            (0..self.indexed.get()).map(|i| self.lens[i] + 1).sum()
        }
        fn advance(&self, to: usize) {
            self.indexed.set(to.min(self.lens.len()));
        }
    }

    impl ViewportSource for Fake {
        fn byte_len(&self) -> usize {
            self.lens.iter().map(|l| l + 1).sum()
        }
        fn line_count_estimate(&self) -> usize {
            let n = self.lens.len();
            let idx = self.indexed.get();
            if idx >= n {
                return n;
            }
            if idx == 0 {
                return (self.byte_len() / 20).max(1);
            }
            let avg = self.indexed_bytes() as f64 / idx as f64;
            idx + ((self.byte_len() - self.indexed_bytes()) as f64 / avg).round() as usize
        }
        fn is_exact(&self) -> bool {
            self.indexed.get() >= self.lens.len()
        }
        fn indexed_lines(&self) -> usize {
            self.indexed.get()
        }
        fn index_progress(&self) -> f32 {
            self.indexed_bytes() as f32 / self.byte_len() as f32
        }
        fn read_lines(&self, range: Range<usize>) -> Vec<String> {
            self.reads.set(self.reads.get() + range.len());
            range.filter(|i| *i < self.indexed.get()).map(|i| self.text(i)).collect()
        }
        fn line_len_bytes(&self, line: usize) -> Option<usize> {
            if let Some(l) = self.long_line {
                if l == line {
                    return Some(100_000_000);
                }
            }
            (line < self.indexed.get()).then(|| self.lens[line] + 1)
        }
        fn read_line_window(&self, line: usize, first_col: usize, max_cols: usize) -> LineWindow {
            self.reads.set(self.reads.get() + 1);
            if self.long_line == Some(line) {
                // 100 MB of "0123456789" repeated, produced on demand, never held.
                let text: String = (first_col..first_col + max_cols).map(|c| char::from(b'0' + (c % 10) as u8)).collect();
                return LineWindow { text, first_col, more_right: true, line_cols: None };
            }
            window_of_str(&self.text(line), first_col, max_cols)
        }
    }

    #[test]
    fn rope_source_is_exact_and_reads_windows() {
        let b = Buffer::from_str("alpha\r\nbeta gamma\nlast");
        assert!(b.is_exact());
        assert_eq!(ViewportSource::index_progress(&b), 1.0);
        assert_eq!(b.indexed_lines(), b.line_count_estimate());
        assert_eq!(b.read_lines(0..2), vec!["alpha".to_string(), "beta gamma".to_string()], "lines come without their break");
        let w = b.read_line_window(1, 5, 3);
        assert_eq!((w.text.as_str(), w.first_col, w.more_right, w.line_cols), ("gam", 5, true, None));
        let w = b.read_line_window(1, 8, 10);
        assert_eq!((w.text.as_str(), w.more_right, w.line_cols), ("ma", false, Some(10)));
        assert_eq!(b.read_line_window(0, 0, 99).text, "alpha", "the line break never enters a window");
        assert_eq!(b.read_line_window(99, 0, 5).text, "");
        assert_eq!(b.line_len_bytes(0), Some(7));
        assert_eq!(b.line_len_bytes(9), None);
    }

    #[test]
    fn index_grows_estimate_converges_and_snapshot_publishes_per_percent() {
        let f = Fake::new(10_000);
        let mut last = IndexSnapshot::of(&f);
        assert!(!last.exact && last.percent == 0);
        let mut published = 0;
        let mut prev_est_err = usize::MAX;
        for step in 1..=100 {
            f.advance(step * 100);
            let s = IndexSnapshot::of(&f);
            if s.worth_publishing(&last) {
                published += 1;
                last = s;
            }
            if step >= 20 {
                let err = s.estimate.abs_diff(10_000);
                assert!(err <= 300, "estimate {} off by {err} at step {step}", s.estimate);
                let _ = prev_est_err;
                prev_est_err = err;
            }
        }
        assert!(last.exact && last.percent == 100 && last.estimate == 10_000);
        assert!(published <= 101, "at most one publication per whole percent, got {published}");
        // Intermediate callbacks inside one percent are not published.
        let a = IndexSnapshot { exact: false, estimate: 100, indexed_lines: 50, percent: 50 };
        let b = IndexSnapshot { estimate: 101, indexed_lines: 51, ..a };
        assert!(!b.worth_publishing(&a));
        assert!(IndexSnapshot { percent: 51, ..a }.worth_publishing(&a));
        assert!(IndexSnapshot { exact: true, percent: 100, ..a }.worth_publishing(&a));
    }

    #[test]
    fn far_target_is_a_placeholder_then_text_without_moving_the_anchor() {
        let f = Fake::new(10_000);
        f.advance(1_000);
        let anchor = 7_000usize;
        let visible = 40usize;
        let lh = 18.0;
        let y = anchor as f64 * lh; // scroll offset is line * height whatever the total
        let mut totals = Vec::new();
        for idx in [1_000usize, 2_000, 4_000, 7_000, 9_000, 10_000] {
            f.advance(idx);
            let snap = IndexSnapshot::of(&f);
            totals.push(stable_total_rows(&snap, || f.line_count_estimate(), anchor, visible));
            // The row under the anchor: placeholder until its line is indexed, then its text.
            let st = row_state(&snap, anchor);
            if idx <= anchor {
                assert_eq!(st, RowState::Indexing, "idx {idx}");
                assert_eq!(gutter_label(&snap, anchor), "~7001");
            } else {
                assert_eq!(st, RowState::Ready);
                assert_eq!(f.read_lines(anchor..anchor + 1), vec![f.text(7_000)]);
                assert_eq!(gutter_label(&snap, anchor), " 7001");
            }
            // The scroll offset that keeps the anchor is the same at every step.
            assert_eq!(y / lh, anchor as f64);
            // The content never ends above the anchor's screenful, even if the estimate is low.
            assert!(*totals.last().unwrap() >= anchor + visible + 1 || snap.exact);
        }
        assert_eq!(*totals.last().unwrap(), 10_000, "exact count at the end");
    }

    #[test]
    fn stable_total_never_drops_below_what_is_indexed_or_shown() {
        let snap = IndexSnapshot { exact: false, estimate: 100, indexed_lines: 500, percent: 40 };
        assert_eq!(stable_total_rows(&snap, || unreachable!(), 0, 30), 500);
        assert_eq!(stable_total_rows(&snap, || unreachable!(), 900, 30), 931);
        let ex = IndexSnapshot { exact: true, ..snap };
        assert_eq!(stable_total_rows(&ex, || 321, 900, 30), 321);
        // The exact path never touches the estimate and the estimate path never counts lines.
    }

    #[test]
    fn chip_and_panes_say_it_in_words() {
        let snap = IndexSnapshot { exact: false, estimate: 4_000_000, indexed_lines: 2_500_000, percent: 63 };
        assert_eq!(index_chip(&snap, None).unwrap(), "Indexing 63%");
        assert_eq!(index_chip(&snap, Some(3_000_000)).unwrap(), "Indexing 63% - going to line 3000000 when ready");
        assert!(index_chip(&IndexSnapshot::EXACT, None).is_none());
        let mut panes = crate::status_bar::status_model(&crate::status_bar::DocStatus::default());
        decorate_panes(&mut panes, &snap, None);
        let ids: Vec<&str> = panes.iter().map(|p| p.id).collect();
        assert_eq!(&ids[..3], ["pos", "lines", "indexing"]);
        assert_eq!(panes[1].label, "~4000000 lines");
        let mut ok = crate::status_bar::status_model(&crate::status_bar::DocStatus::default());
        let before = ok.clone();
        decorate_panes(&mut ok, &IndexSnapshot::EXACT, None);
        assert_eq!(ok, before, "nothing changes once the index is complete or absent");
    }

    #[test]
    fn goto_inside_the_index_is_immediate_beyond_it_waits_then_clamps() {
        let f = Fake::new(1_000);
        f.advance(300);
        let snap = IndexSnapshot::of(&f);
        assert_eq!(plan_goto(&snap, 200, || unreachable!()), GotoPlan::Go(200));
        assert_eq!(plan_goto(&snap, 301, || unreachable!()), GotoPlan::Wait(301));
        assert_eq!(plan_goto(&snap, 0, || unreachable!()), GotoPlan::Go(1));
        f.advance(600);
        assert_eq!(plan_goto(&IndexSnapshot::of(&f), 301, || unreachable!()), GotoPlan::Go(301));
        f.advance(1_000);
        let done = IndexSnapshot::of(&f);
        assert_eq!(plan_goto(&done, 301, || 1_000), GotoPlan::Go(301));
        assert_eq!(plan_goto(&done, 5_000, || 1_000), GotoPlan::Go(1_000), "past the end after indexing: last line");
    }

    #[test]
    fn long_line_is_a_window_not_a_line() {
        let mut f = Fake::new(100);
        f.advance(100);
        f.long_line = Some(3);
        assert!(is_long_line(f.line_len_bytes(3)));
        assert!(!is_long_line(f.line_len_bytes(4)));
        let start = window_start(8.0 * 1_000_000.0, 8.0);
        assert_eq!(start % WINDOW_CHUNK_COLS, 0);
        assert_eq!(start, 999_680, "1M columns right: margin 256 below the first visible column, snapped to a chunk");
        let cols = window_cols(1_600.0, 8.0);
        assert!(cols >= 200 + 2 * WINDOW_MARGIN_COLS && cols <= WINDOW_MAX_COLS && cols % WINDOW_CHUNK_COLS == 0);
        let w = f.read_line_window(3, start, cols);
        assert_eq!(w.text.chars().count(), cols, "only the window is produced");
        assert_eq!(w.first_col, start);
        assert!(w.more_right);
        // Clicking on it must not read the line.
        let before = f.reads.get();
        assert_eq!(line_chars_fast(&f, 3), 100_000_000);
        assert_eq!(f.reads.get(), before);
        assert_eq!(line_chars_fast(&f, 4), f.text(4).chars().count());
        // Window start is stable inside a chunk (rows are not rebuilt per pixel).
        assert_eq!(window_start(8.0 * 5_000.0, 8.0), window_start(8.0 * 5_100.0, 8.0));
        assert_eq!(window_start(0.0, 8.0), 0);
    }

    #[test]
    fn lazy_buffer_is_a_source_with_the_same_answers_as_the_rope() {
        let mut text = String::new();
        for i in 0..2_000 {
            text.push_str(&format!("line {i}\n"));
        }
        text.push_str(&"abcdefghij".repeat(30_000)); // a 300 KB last line, no break
        let dir = std::env::temp_dir().join(format!("brouillon-vp-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("lazy.txt");
        std::fs::write(&path, &text).unwrap();
        let lazy = Buffer::open_lazy(&path).unwrap();
        let rope = Buffer::from_str(&text);
        assert!(lazy.is_lazy());
        lazy.lazy_wait_index();
        let (ls, rs): (&dyn ViewportSource, &dyn ViewportSource) = (&lazy, &rope);
        assert!(ls.is_exact() && ls.index_progress() == 1.0);
        assert_eq!(ls.line_count_estimate(), rs.line_count_estimate());
        assert_eq!(ls.indexed_lines(), ls.line_count_estimate());
        assert_eq!(ls.read_lines(5..8), rs.read_lines(5..8));
        assert_eq!(ls.line_len_bytes(2_000), rs.line_len_bytes(2_000));
        assert!(is_long_line(ls.line_len_bytes(2_000)));
        assert_eq!(ls.read_line_window(2_000, 123_456, 40), rs.read_line_window(2_000, 123_456, 40));
        assert_eq!(ls.read_line_window(3, 2, 3).text, "ne ");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn rope_long_line_window_matches_a_slice_of_the_text() {
        let long = "abcdefghij".repeat(30_000); // 300 KB on one line
        let b = Buffer::from_str(&format!("{long}\nshort"));
        assert!(is_long_line(b.line_len_bytes(0)));
        let w = b.read_line_window(0, 123_456, 40);
        assert_eq!(w.text, long[123_456..123_496]);
        assert!(w.more_right);
        assert_eq!(b.read_line_window(0, 299_990, 100).line_cols, Some(300_000));
        assert_eq!(line_chars_fast(&b, 1), 5);
    }

    #[test]
    fn minimap_sampling_is_o_rows_and_blank_where_not_indexed() {
        let f = Fake::new(2_000_000 / 100);
        f.advance(10_000 / 100 * 100 / 2);
        let before = f.reads.get();
        let rows = sample_minimap(&f, 96, 4);
        assert!(rows.len() <= 96);
        assert!(f.reads.get() - before <= 96, "at most one window read per row, got {}", f.reads.get() - before);
        let snap = IndexSnapshot::of(&f);
        assert!(rows.iter().any(|r| r.len > 0), "indexed part is drawn");
        assert!(rows.iter().filter(|r| r.line >= snap.indexed_lines).all(|r| r.len == 0), "unindexed part stays blank");
        f.advance(usize::MAX);
        assert!(sample_minimap(&f, 96, 4).iter().all(|r| r.len > 0));
        // A one-line document of 100 MB samples one 160-column window.
        let mut g = Fake::new(1);
        g.advance(1);
        g.long_line = Some(0);
        let r = sample_minimap(&g, 600, 4);
        assert_eq!(r.len(), 1);
        assert_eq!(r[0].len, 120, "length is clamped, the line itself was never read");
    }

    #[test]
    fn gating_tiers_and_notes() {
        let small = HugeGate::for_bytes(10_000);
        assert!(small.syntax_highlight && small.folding && small.bracket_match && small.word_wrap && small.word_completion && small.indent_guides && small.outline);
        assert_eq!(small.minimap, MinimapMode::Full);
        assert!(small.note(10_000).is_none());

        let mid = HugeGate::for_bytes(3 * 1024 * 1024);
        assert!(!mid.bracket_match && !mid.folding && mid.word_wrap, "bracket 256 KB, folding 2 MB");
        assert!(mid.syntax_highlight && mid.note(3 << 20).is_none(), "below 16 MB nothing is announced");
        assert!(!HugeGate::for_bytes(5 << 20).word_wrap);

        let big = HugeGate::for_bytes(100 * 1024 * 1024);
        assert!(big.huge);
        assert!(!big.syntax_highlight && !big.outline && !big.folding && !big.bracket_match && !big.indent_guides && !big.word_wrap && !big.word_completion);
        assert_eq!(big.minimap, MinimapMode::Coarse);
        assert_eq!(big.minimap_rows(), 96);
        let note = big.note(100 * 1024 * 1024).unwrap();
        assert!(note.starts_with("Large file (100 MB)") && note.contains("word wrap") && note.contains("simplified"));

        let giant = HugeGate::for_bytes(1_000_000_000);
        assert_eq!(giant.minimap, MinimapMode::Off);
        assert_eq!(giant.minimap_rows(), 0);
        assert!(giant.note(1_000_000_000).unwrap().contains("minimap is off"));

        // Boundaries are inclusive/exclusive exactly where the constants say.
        assert!(HugeGate::for_bytes(HUGE_BYTES - 1).syntax_highlight);
        assert!(!HugeGate::for_bytes(HUGE_BYTES).syntax_highlight);
        assert!(HugeGate::for_bytes(BRACKET_MAX_BYTES).bracket_match);
        assert!(!HugeGate::for_bytes(BRACKET_MAX_BYTES + 1).bracket_match);
    }
}
