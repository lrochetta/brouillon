//! Hex viewer for binary file inspection.
//!
//! Provides hex + ASCII side-by-side display of raw file bytes. Each line
//! shows: offset (8 hex digits) | hex dump (16 bytes per row) | ASCII
//! representation (printable chars or dots).
//!
//! M2 scope: read-only hex view as a tab mode toggle (Ctrl-H). Editing
//! hex bytes is deferred to M3.

use std::sync::Arc;
use floem::reactive::{RwSignal, SignalGet};
use floem::views::{container, scroll, virtual_stack, label, stack, Decorators, VirtualDirection, VirtualItemSize, VirtualVector};
use floem::IntoView;

use brouillon_core::theme::current_theme;
use brouillon_editor_engine::buffer::Buffer;

const LINE_HEIGHT: f64 = 18.0;

/// Default number of bytes displayed per row in the hex view.
pub const BYTES_PER_ROW: usize = 16;

/// One formatted line of the hex viewer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HexLine {
    /// Offset string, e.g. "00000000"
    pub offset: String,
    /// Hex dump string, e.g. "48 65 6C 6C 6F 20 57 6F  72 6C 64 21 0A 00 00 00"
    pub hex: String,
    /// ASCII representation, e.g. "Hello World!...."
    pub ascii: String,
}

/// Format a single hex line from a slice of bytes at the given offset.
///
/// `bytes_per_row` controls how many bytes appear on each line (default 16).
/// The last row may have fewer bytes — hex cells are padded with spaces.
pub fn format_hex_line(data: &[u8], offset: usize, bytes_per_row: usize) -> HexLine {
    let offset_str = format!("{:08X}", offset);

    let mut hex_parts: Vec<String> = Vec::with_capacity(bytes_per_row);
    let mut ascii_parts: Vec<char> = Vec::with_capacity(bytes_per_row);

    for i in 0..bytes_per_row {
        if i < data.len() {
            let b = data[i];
            hex_parts.push(format!("{:02X}", b));
            ascii_parts.push(if b.is_ascii_graphic() || b == b' ' {
                b as char
            } else {
                '.'
            });
        } else {
            hex_parts.push("  ".to_string());
            ascii_parts.push(' ');
        }
    }

    // Insert extra space at the midpoint for visual grouping (8+8 layout)
    let mid = bytes_per_row / 2;
    let hex_str = if hex_parts.len() > mid {
        let left = hex_parts[..mid].join(" ");
        let right = hex_parts[mid..].join(" ");
        format!("{}  {}", left, right)
    } else {
        hex_parts.join(" ")
    };

    let ascii_str: String = ascii_parts.into_iter().collect();

    HexLine {
        offset: offset_str,
        hex: hex_str,
        ascii: ascii_str,
    }
}

/// Generate hex view lines for a viewport range of bytes.
///
/// `data` is the full file content as bytes.
/// `viewport_start_line` is the first line to display (0-based).
/// `viewport_lines` is how many lines to generate.
/// `bytes_per_row` controls bytes per line (default 16).
///
/// Returns the formatted lines for the visible viewport.
pub fn hex_view_lines(
    data: &[u8],
    viewport_start_line: usize,
    viewport_lines: usize,
    bytes_per_row: usize,
) -> Vec<HexLine> {
    let start_byte = viewport_start_line * bytes_per_row;
    if start_byte >= data.len() {
        return Vec::new();
    }

    let mut lines = Vec::with_capacity(viewport_lines);
    for i in 0..viewport_lines {
        let offset = start_byte + i * bytes_per_row;
        if offset >= data.len() {
            break;
        }
        let end = (offset + bytes_per_row).min(data.len());
        let chunk = &data[offset..end];
        lines.push(format_hex_line(chunk, offset, bytes_per_row));
    }
    lines
}

/// Calculate the total number of lines needed to display `data_len` bytes.
pub fn total_hex_lines(data_len: usize, bytes_per_row: usize) -> usize {
    if data_len == 0 {
        return 0;
    }
    (data_len + bytes_per_row - 1) / bytes_per_row
}

/// Search for a byte pattern in the data. Returns the offset of the first match.
pub fn find_bytes(data: &[u8], pattern: &[u8]) -> Option<usize> {
    if pattern.is_empty() || pattern.len() > data.len() {
        return None;
    }
    data.windows(pattern.len())
        .position(|w| w == pattern)
}

/// `len` bytes of the rope's UTF-8 text starting at byte `offset` (shorter at the end of the document).
/// Rope slices must start and end on character boundaries, so the slice is widened to them and trimmed.
pub fn rope_bytes(rope: &crop::Rope, offset: usize, len: usize) -> Vec<u8> {
    let total = rope.byte_len();
    if offset >= total {
        return Vec::new();
    }
    let end = (offset + len).min(total);
    let mut a = offset;
    while !rope.is_char_boundary(a) {
        a -= 1;
    }
    let mut b = end;
    while !rope.is_char_boundary(b) {
        b += 1;
    }
    let mut out = Vec::with_capacity(b - a);
    for c in rope.byte_slice(a..b).chunks() {
        out.extend_from_slice(c.as_bytes());
    }
    out[offset - a..offset - a + (end - offset)].to_vec()
}

/// `len` bytes of the buffer's UTF-8 text from byte `offset`, on either backing (rope or large-file-mode store).
pub fn buffer_bytes(buf: &Buffer, offset: usize, len: usize) -> Vec<u8> {
    let total = buf.byte_len();
    if offset >= total {
        return Vec::new();
    }
    let end = (offset + len).min(total);
    let mut out = Vec::with_capacity(end - offset);
    buf.visit_bytes(offset..end, &mut |c| {
        out.extend_from_slice(c);
        true
    });
    out
}

/// Rows of the hex view, produced lazily for the visible window only.
struct HexRows {
    rows: usize,
    version: u64,
}

impl VirtualVector<(usize, u64)> for HexRows {
    fn total_len(&self) -> usize {
        self.rows
    }
    fn slice(&mut self, range: std::ops::Range<usize>) -> impl Iterator<Item = (usize, u64)> {
        let end = range.end.min(self.rows);
        let version = self.version;
        (range.start.min(end)..end).map(move |i| (i, version))
    }
}

/// Paged variant for huge documents: formats `lines` rows starting at row `first_line` straight from a
/// byte source (a `HexDoc`, a `HexSnapshot`, a `FileSource`), reading only the page. The whole-file
/// `hex_view_lines(&[u8], ..)` above stays for small documents.
pub fn hex_page_lines(
    src: &dyn brouillon_editor_engine::byte_source::ByteSource,
    first_line: u64,
    lines: usize,
    bytes_per_row: usize,
) -> Vec<HexLine> {
    let page = brouillon_editor_engine::hex_doc::read_page(src, first_line, lines, bytes_per_row);
    page.chunks(bytes_per_row)
        .enumerate()
        .map(|(i, row)| format_hex_line(row, ((first_line + i as u64) * bytes_per_row as u64) as usize, bytes_per_row))
        .collect()
}

// ----------------------------------------------------------------------------- styled rows (editable hex)

use brouillon_editor_engine::hex_session::CellKind;

/// A stretch of consecutive characters of one row that share a paint kind. A row is a handful of
/// runs (usually one to three), so a screenful stays far below a hundred labels, instead of one
/// widget per byte.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Run {
    pub text: String,
    pub kind: CellKind,
}

fn push_run(runs: &mut Vec<Run>, text: &str, kind: CellKind) {
    match runs.last_mut() {
        Some(last) if last.kind == kind => last.text.push_str(text),
        _ => runs.push(Run { text: text.to_string(), kind }),
    }
}

/// The hex part of one row as runs. Each byte is `XX` followed by a separator space (and one more
/// space after the eighth byte, as in [`format_hex_line`]); a separator is painted with its byte
/// only when the next byte has the same kind, so a selection does not bleed into the gap after it.
/// Missing bytes of a short last row are blank.
pub fn hex_runs(data: &[u8], first_off: u64, bytes_per_row: usize, kind_at: &dyn Fn(u64) -> CellKind) -> Vec<Run> {
    let mut runs = Vec::new();
    let mid = bytes_per_row / 2;
    for i in 0..bytes_per_row {
        let last = i + 1 == bytes_per_row;
        let sep = if last { "" } else if i + 1 == mid { "  " } else { " " };
        match data.get(i) {
            Some(b) => {
                let k = kind_at(first_off + i as u64);
                push_run(&mut runs, &format!("{b:02X}"), k);
                let same_next = !last && data.get(i + 1).is_some() && kind_at(first_off + i as u64 + 1) == k;
                push_run(&mut runs, sep, if same_next { k } else { CellKind::Plain });
            }
            None => push_run(&mut runs, &format!("  {sep}"), CellKind::Plain),
        }
    }
    runs
}

/// The ASCII part of one row as runs (one character per byte, `.` for non-printable).
pub fn ascii_runs(data: &[u8], first_off: u64, bytes_per_row: usize, kind_at: &dyn Fn(u64) -> CellKind) -> Vec<Run> {
    let mut runs = Vec::new();
    for i in 0..bytes_per_row {
        match data.get(i) {
            Some(b) => {
                let c = if b.is_ascii_graphic() || *b == b' ' { *b as char } else { '.' };
                push_run(&mut runs, &c.to_string(), kind_at(first_off + i as u64));
            }
            None => push_run(&mut runs, " ", CellKind::Plain),
        }
    }
    runs
}

/// Character columns of a hex row: `00000000  ` (10), the hex part (47 + 1 extra middle space = 48 for 16
/// bytes), two spaces, the ASCII part. These are the columns the runs above produce.
pub const OFFSET_COLS: usize = 10;

pub fn hex_cols(bytes_per_row: usize) -> usize {
    bytes_per_row * 3 - 1 + usize::from(bytes_per_row >= 2)
}

/// The byte index (0..bytes_per_row) under character column `col` of a row, hex or ASCII part; None on
/// the offset, a gap or outside the row.
pub fn byte_at_col(col: usize, bytes_per_row: usize) -> Option<usize> {
    let hex_start = OFFSET_COLS;
    let hex_end = hex_start + hex_cols(bytes_per_row);
    let ascii_start = hex_end + 2;
    if col >= ascii_start {
        let i = col - ascii_start;
        return (i < bytes_per_row).then_some(i);
    }
    if col < hex_start || col >= hex_end {
        return None;
    }
    let mid = bytes_per_row / 2;
    let c = col - hex_start;
    // byte i starts at column 3*i, plus one once past the middle (the extra space after byte mid-1)
    let c = if mid == 0 {
        c
    } else {
        match c.cmp(&(3 * mid)) {
            std::cmp::Ordering::Less => c,
            std::cmp::Ordering::Equal => return None, // the extra middle space
            std::cmp::Ordering::Greater => c - 1,
        }
    };
    let (byte, within) = (c / 3, c % 3);
    (within < 2 && byte < bytes_per_row).then_some(byte)
}

/// Renders the hex view panel with columns for offset, hex data, and printable ASCII.
pub fn hex_view_panel(
    buffer_sig: RwSignal<Arc<Buffer>>,
    theme_idx: RwSignal<usize>,
) -> impl IntoView {
    // The bytes are read from the rope one visible row at a time: no copy of the document and no
    // per-row index (that was 1.8-2.6x the file size while the hex view was open).
    let indices = move || HexRows { rows: total_hex_lines(buffer_sig.get().byte_len(), BYTES_PER_ROW), version: buffer_sig.get().version() };

    container(
        scroll(
            virtual_stack(
                VirtualDirection::Vertical,
                VirtualItemSize::Fixed(Box::new(|| LINE_HEIGHT)),
                indices,
                move |k: &(usize, u64)| *k,
                move |(idx, _version): (usize, u64)| {
                    let buf = buffer_sig.get_untracked();
                    let offset = idx * BYTES_PER_ROW;
                    let chunk = buffer_bytes(&buf, offset, BYTES_PER_ROW);
                    let line = format_hex_line(&chunk, offset, BYTES_PER_ROW);

                    stack((
                        label(move || format!("{}  ", line.offset)).style(move |s| {
                            let t = current_theme(theme_idx.get());
                            s.color(t.fg_dim).padding_right(12.0)
                        }),
                        label(move || format!("{}  ", line.hex)).style(move |s| {
                            let t = current_theme(theme_idx.get());
                            s.color(t.fg).padding_right(20.0)
                        }),
                        label(move || line.ascii.clone()).style(move |s| {
                            let t = current_theme(theme_idx.get());
                            s.color(t.accent)
                        }),
                    ))
                    .style(|s| s.flex_row().height(LINE_HEIGHT).padding_horiz(10.0))
                },
            )
            .style(|s| s.flex_col().width_full()),
        )
        .style(move |s| {
            let t = current_theme(theme_idx.get());
            s.size_full().background(t.bg)
        }),
    )
    .style(|s| s.size_full())
}

#[cfg(test)]
mod tests {
    use super::*;
    use floem::reactive::{with_scope, Scope, create_rw_signal, SignalUpdate};

    fn run<F: FnOnce() + 'static>(f: F) {
        let scope = Scope::new();
        with_scope(scope, f);
        scope.dispose();
    }

    #[test]
    fn paged_lines_equal_the_whole_buffer_lines() {
        let data: Vec<u8> = (0..=255u8).cycle().take(1000).collect();
        let whole = hex_view_lines(&data, 0, 100, BYTES_PER_ROW);
        let paged = hex_page_lines(&data, 0, 100, BYTES_PER_ROW);
        assert_eq!(whole, paged);
        let tail = hex_page_lines(&data, 60, 10, BYTES_PER_ROW);
        assert_eq!(tail, whole[60..]);
        assert!(hex_page_lines(&data, 80, 5, BYTES_PER_ROW).is_empty());
    }

    #[test]
    fn test_hex_view_panel_creation() {
        run(|| {
            let buf_sig = create_rw_signal(Arc::new(Buffer::from_str("hello world")));
            let theme_sig = create_rw_signal(0);
            let _v = hex_view_panel(buf_sig, theme_sig);
        });
    }

    #[test]
    fn test_hex_view_panel_updates() {
        run(|| {
            let buf_sig = create_rw_signal(Arc::new(Buffer::from_str("")));
            let theme_sig = create_rw_signal(0);
            let _v = hex_view_panel(buf_sig, theme_sig);
            
            buf_sig.set(Arc::new(Buffer::from_str("non-empty binary buffer data")));
        });
    }

    #[test]
    fn format_hex_line_full_row() {
        let data: Vec<u8> = (0x40..0x50).collect(); // @ABCDEFGHIJKLMNO
        let line = format_hex_line(&data, 0, 16);
        assert_eq!(line.offset, "00000000");
        assert!(line.hex.contains("40 41 42"));
        assert!(line.ascii.starts_with("@ABCDEFG"));
    }

    #[test]
    fn format_hex_line_partial_row() {
        let data = b"Hi!";
        let line = format_hex_line(data, 0x100, 16);
        assert_eq!(line.offset, "00000100");
        assert!(line.hex.starts_with("48 69 21"));
        assert!(line.ascii.starts_with("Hi!"));
    }

    #[test]
    fn format_hex_line_non_printable() {
        let data = &[0x00, 0x01, 0x7F, 0xFF, 0x41];
        let line = format_hex_line(data, 0, 16);
        assert!(line.ascii.starts_with("....A"));
    }

    #[test]
    fn hex_view_lines_basic() {
        let data: Vec<u8> = (0..48).collect(); // 3 full rows
        let lines = hex_view_lines(&data, 0, 10, 16);
        assert_eq!(lines.len(), 3);
        assert_eq!(lines[0].offset, "00000000");
        assert_eq!(lines[1].offset, "00000010");
        assert_eq!(lines[2].offset, "00000020");
    }

    #[test]
    fn hex_view_lines_viewport_offset() {
        let data: Vec<u8> = (0..64).collect(); // 4 rows
        let lines = hex_view_lines(&data, 2, 10, 16);
        assert_eq!(lines.len(), 2); // only rows 2 and 3
        assert_eq!(lines[0].offset, "00000020");
        assert_eq!(lines[1].offset, "00000030");
    }

    #[test]
    fn hex_view_lines_past_eof() {
        let data = b"short";
        let lines = hex_view_lines(data, 100, 10, 16);
        assert!(lines.is_empty());
    }

    #[test]
    fn hex_view_lines_empty_data() {
        let lines = hex_view_lines(&[], 0, 10, 16);
        assert!(lines.is_empty());
    }

    #[test]
    fn total_hex_lines_calculation() {
        assert_eq!(total_hex_lines(0, 16), 0);
        assert_eq!(total_hex_lines(1, 16), 1);
        assert_eq!(total_hex_lines(16, 16), 1);
        assert_eq!(total_hex_lines(17, 16), 2);
        assert_eq!(total_hex_lines(48, 16), 3);
        assert_eq!(total_hex_lines(100, 16), 7); // ceil(100/16) = 7
    }

    #[test]
    fn find_bytes_found() {
        let data = b"Hello World!";
        assert_eq!(find_bytes(data, b"World"), Some(6));
    }

    #[test]
    fn find_bytes_not_found() {
        let data = b"Hello World!";
        assert_eq!(find_bytes(data, b"xyz"), None);
    }

    #[test]
    fn find_bytes_empty_pattern() {
        let data = b"Hello";
        assert_eq!(find_bytes(data, b""), None);
    }

    #[test]
    fn find_bytes_at_start() {
        let data = b"Hello";
        assert_eq!(find_bytes(data, b"He"), Some(0));
    }

    #[test]
    fn hex_grouping_has_double_space() {
        let data: Vec<u8> = (0..16).collect();
        let line = format_hex_line(&data, 0, 16);
        // Should have double-space between groups of 8
        assert!(line.hex.contains("  "), "hex line should have double-space grouping: {}", line.hex);
    }

    /// Row bytes read from the rope equal the same slice of the document's UTF-8 bytes, at every offset
    /// (multi-byte characters straddling row boundaries included).
    #[test]
    fn rope_bytes_equal_the_text_bytes_at_every_offset() {
        let text = "caf\u{e9} \u{20ac}uro \u{1f600} smile\nline two \u{e9}\u{e9}\u{e9}\n".repeat(60);
        let rope: crop::Rope = text.parse().unwrap();
        let bytes = text.as_bytes();
        for offset in 0..bytes.len() + 3 {
            let want: Vec<u8> = bytes.iter().copied().skip(offset).take(BYTES_PER_ROW).collect();
            assert_eq!(rope_bytes(&rope, offset, BYTES_PER_ROW), want, "offset {offset}");
        }
        // Whole rows, as the panel asks for them.
        let rows = total_hex_lines(bytes.len(), BYTES_PER_ROW);
        let joined: Vec<u8> = (0..rows).flat_map(|r| rope_bytes(&rope, r * BYTES_PER_ROW, BYTES_PER_ROW)).collect();
        assert_eq!(joined, bytes);
        assert!(rope_bytes(&crop::Rope::new(), 0, 16).is_empty());
    }

    #[test]
    fn hex_rows_are_produced_lazily_for_the_window_only() {
        let mut rows = HexRows { rows: 10_000_000, version: 7 };
        assert_eq!(rows.total_len(), 10_000_000);
        let w: Vec<_> = rows.slice(5..8).collect();
        assert_eq!(w, vec![(5, 7), (6, 7), (7, 7)]);
        assert_eq!(rows.slice(9_999_998..10_000_100).count(), 2, "clamped to the row count");
    }
}

#[cfg(test)]
mod run_tests {
    use super::*;
    use brouillon_editor_engine::hex_regions::RegionColor;

    fn join(runs: &[Run]) -> String {
        runs.iter().map(|r| r.text.as_str()).collect()
    }

    #[test]
    fn hex_runs_text_equals_format_hex_line() {
        let data: Vec<u8> = (0x40..0x50).collect();
        let runs = hex_runs(&data, 0, 16, &|_| CellKind::Plain);
        assert_eq!(join(&runs), format_hex_line(&data, 0, 16).hex);
        assert_eq!(runs.len(), 1);
        let short = b"Hi!";
        assert_eq!(join(&hex_runs(short, 0, 16, &|_| CellKind::Plain)), format_hex_line(short, 0, 16).hex);
        assert_eq!(join(&ascii_runs(short, 0, 16, &|_| CellKind::Plain)), format_hex_line(short, 0, 16).ascii);
        assert_eq!(hex_runs(&[], 0, 16, &|_| CellKind::Plain).iter().map(|r| r.text.len()).sum::<usize>(), hex_cols(16));
    }

    #[test]
    fn runs_split_at_kind_changes_and_gaps_stay_plain() {
        let data = [0u8; 16];
        let kind = |o: u64| match o {
            2..=4 => CellKind::Selected,
            9 => CellKind::Cursor,
            12..=13 => CellKind::Region(RegionColor::Green),
            _ => CellKind::Plain,
        };
        let runs = hex_runs(&data, 0, 16, &kind);
        assert_eq!(join(&runs), format_hex_line(&data, 0, 16).hex);
        let sel: Vec<&Run> = runs.iter().filter(|r| r.kind == CellKind::Selected).collect();
        assert_eq!(sel.len(), 1);
        assert_eq!(sel[0].text, "00 00 00", "no trailing separator in the selected run");
        let cur: Vec<&Run> = runs.iter().filter(|r| r.kind == CellKind::Cursor).collect();
        assert_eq!(cur[0].text, "00");
        let ascii = ascii_runs(&data, 0, 16, &kind);
        assert_eq!(ascii.iter().filter(|r| r.kind == CellKind::Selected).map(|r| r.text.len()).sum::<usize>(), 3);
        assert_eq!(join(&ascii).chars().count(), 16);
    }

    #[test]
    fn click_columns_map_to_bytes() {
        assert_eq!(byte_at_col(0, 16), None, "offset");
        assert_eq!(byte_at_col(10, 16), Some(0));
        assert_eq!(byte_at_col(11, 16), Some(0));
        assert_eq!(byte_at_col(12, 16), None, "separator");
        assert_eq!(byte_at_col(13, 16), Some(1));
        assert_eq!(byte_at_col(10 + 21, 16), Some(7));
        assert_eq!(byte_at_col(10 + 23, 16), None);
        assert_eq!(byte_at_col(10 + 24, 16), None, "extra middle space");
        assert_eq!(byte_at_col(10 + 25, 16), Some(8));
        assert_eq!(byte_at_col(10 + 26, 16), Some(8));
        assert_eq!(byte_at_col(10 + 27, 16), None);
        assert_eq!(byte_at_col(10 + 47, 16), Some(15));
        assert_eq!(byte_at_col(10 + 48, 16), None);
        let a0 = 10 + hex_cols(16) + 2;
        assert_eq!(byte_at_col(a0 - 1, 16), None);
        assert_eq!(byte_at_col(a0, 16), Some(0));
        assert_eq!(byte_at_col(a0 + 15, 16), Some(15));
        assert_eq!(byte_at_col(a0 + 16, 16), None);
        let data: Vec<u8> = (0..16).collect();
        let line = format_hex_line(&data, 0, 16).hex;
        for (c, ch) in line.chars().enumerate() {
            let b = byte_at_col(OFFSET_COLS + c, 16);
            if ch == ' ' {
                assert_eq!(b, None, "col {c}");
            } else {
                let want = if c < 24 { c / 3 } else { (c - 1) / 3 };
                assert_eq!(b, Some(want), "col {c}");
            }
        }
    }
}
