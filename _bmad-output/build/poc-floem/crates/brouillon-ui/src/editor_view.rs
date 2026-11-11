use std::sync::{Arc, Mutex};
use floem::event::{Event, EventListener};
use floem::kurbo::{Point, Rect};
use floem::peniko::Color;
use floem::reactive::{create_effect, create_rw_signal, RwSignal, SignalGet, SignalUpdate, SignalWith};
use floem::style::CursorStyle;
use floem::views::{
    container, empty, label, scroll, stack, virtual_stack, Decorators, VirtualDirection,
    VirtualItemSize, VirtualVector,
};
use floem::IntoView;

use brouillon_editor_engine::buffer::Buffer;
use crate::bracket_paint::BracketPaint;
use brouillon_editor_engine::cursor::Selection;
use brouillon_editor_engine::highlight::{Highlighter, HighlightSpan, SyntaxKind};
use brouillon_core::theme::{Theme, current_theme};
use crate::link_detect;
use crate::viewport::{self, GotoPlan, RowState, ViewportSignals, ViewportSource};

/// Row height for the default 13 px font; other sizes scale it (see [`line_height_for`]).
const BASE_LINE_HEIGHT: f64 = 18.0;

/// Left edge of the text inside the editor body: row padding 6 + gutter 60 + caret bar 2 + text padding 4.
const TEXT_ORIGIN_X: f64 = 72.0;
const EDITOR_FONT_FAMILY: &str = "Consolas, Menlo, DejaVu Sans Mono, monospace";
const EDITOR_FONT_SIZE: f32 = 13.0;

/// The editor font in use (`editor.font_family`, `editor.font_size`), with the measured cell width.
struct FontState {
    family: String,
    size: f32,
    cell: Option<f64>,
}

static FONT: std::sync::Mutex<FontState> = std::sync::Mutex::new(FontState { family: String::new(), size: EDITOR_FONT_SIZE, cell: None });

/// Settings to a usable font: an empty family or a size of 0 means "the default"; the sizes are
/// kept within 6 to 72 and the chosen family is followed by the default monospace fallbacks.
/// The editor positions the caret on a grid, so a monospace family is expected.
pub fn resolve_editor_font(family: &str, size: i64) -> (String, f32) {
    let fam = family.trim().trim_matches('"').replace(['\n', '\r', ';'], " ");
    let fam = fam.trim();
    let family = if fam.is_empty() || fam.eq_ignore_ascii_case(EDITOR_FONT_FAMILY) { EDITOR_FONT_FAMILY.to_string() } else { format!("{fam}, {EDITOR_FONT_FAMILY}") };
    let size = if size <= 0 { EDITOR_FONT_SIZE } else { size.clamp(6, 72) as f32 };
    (family, size)
}

/// Row height for a font size (the default 13 px gives 18 px).
pub fn line_height_for(size: f32) -> f64 {
    (size as f64 * BASE_LINE_HEIGHT / EDITOR_FONT_SIZE as f64).round().max(10.0)
}

/// Make `family` / `size` the editor font. Callers then bump `EditorOpts::font_rev`.
pub fn set_editor_font(family: &str, size: i64) {
    let (family, size) = resolve_editor_font(family, size);
    if let Ok(mut f) = FONT.lock() {
        if f.family != family || f.size != size {
            *f = FontState { family, size, cell: None };
        }
    }
}

/// (family list, size) of the editor font now in use.
pub fn editor_font() -> (String, f32) {
    match FONT.lock() {
        Ok(f) if !f.family.is_empty() => (f.family.clone(), f.size),
        _ => (EDITOR_FONT_FAMILY.to_string(), EDITOR_FONT_SIZE),
    }
}

/// Height of one text row in logical px.
pub fn line_height() -> f64 {
    line_height_for(editor_font().1)
}

/// Advance of one monospace cell in logical px, measured once with the editor font
/// (7.15 for Consolas 13: the old hard-coded 8.0 put clicks 1+ columns off, more on long lines).
pub(crate) fn char_width() -> f64 {
    let (family, size) = editor_font();
    if let Ok(f) = FONT.lock() {
        if let Some(w) = f.cell {
            if f.family == family {
                return w;
            }
        }
    }
    use floem::text::{Attrs, AttrsList, FamilyOwned, TextLayout};
    let list: Vec<FamilyOwned> = FamilyOwned::parse_list(&family).collect();
    let attrs = Attrs::new().font_size(size).family(&list);
    let mut layout = TextLayout::new();
    layout.set_text(&"0".repeat(100), AttrsList::new(attrs));
    let w = layout.size().width / 100.0;
    let w = if w.is_finite() && (3.0..40.0).contains(&w) { w } else { 8.0 };
    if let Ok(mut f) = FONT.lock() {
        if f.family == family && f.size == size {
            f.cell = Some(w);
        }
    }
    w
}

/// 1-based column for a pointer x (relative to the editor body), clamped to the line length.
pub fn click_col(x: f64, cw: f64, line_len_chars: usize) -> usize {
    let col0 = ((x - TEXT_ORIGIN_X) / cw).round().max(0.0) as usize;
    (col0 + 1).min(line_len_chars + 1)
}

/// One text fragment with a syntax kind. Returned by `compute_line_fragments`
/// and consumed by `line_row` to build a colored flex_row of labels.
#[derive(Clone, Debug, PartialEq)]
pub struct LineFragment {
    pub text: String,
    pub kind: SyntaxKind,
    pub is_selected: bool,
}

/// Map a SyntaxKind to a Color via the active theme.
pub fn color_for_kind(theme: &Theme, kind: SyntaxKind) -> Color {
    match kind {
        SyntaxKind::Keyword => theme.syn_keyword,
        SyntaxKind::String => theme.syn_string,
        SyntaxKind::Comment => theme.syn_comment,
        SyntaxKind::Type => theme.syn_type,
        SyntaxKind::Number => theme.syn_number,
        SyntaxKind::Punct => theme.syn_punct,
        SyntaxKind::Identifier => theme.fg,
        SyntaxKind::Default => theme.fg,
        SyntaxKind::BracketMatch => theme.accent,
        SyntaxKind::BracketUnmatched => theme.error,
        SyntaxKind::Link => theme.accent,
        SyntaxKind::Keyword2 => theme.syn_keyword2,
        SyntaxKind::Keyword3 => theme.syn_keyword3,
        SyntaxKind::Keyword4 => theme.syn_keyword4,
        SyntaxKind::Keyword5 => theme.syn_keyword5,
        SyntaxKind::Preprocessor => theme.syn_preproc,
        SyntaxKind::Operator => theme.syn_operator,
        SyntaxKind::Function => theme.syn_function,
        SyntaxKind::Tag => theme.syn_tag,
        SyntaxKind::Attribute => theme.syn_attribute,
        SyntaxKind::Escape => theme.syn_escape,
        SyntaxKind::Variable => theme.syn_variable,
    }
}

/// Compute syntax-highlighted fragments for a single line. Returns a Vec of
/// `LineFragment` covering the whole line, in left-to-right order. If the
/// Highlighter is None (bg-parse not done yet) or the line is out of range,
/// returns a single Default-kind fragment containing the whole line.
pub fn compute_line_fragments(
    buf: &Buffer,
    idx: usize,
    text: &str,
    highlighter_sig: RwSignal<Option<Arc<Mutex<Highlighter>>>>,
    bracket_paint: BracketPaint,
    selection: Selection,
) -> Vec<LineFragment> {
    let line_range = match buf.line_byte_range(idx) {
        Some(r) => r,
        None => return vec![LineFragment { text: text.to_string(), kind: SyntaxKind::Default, is_selected: false }],
    };

    let spans: Vec<HighlightSpan> = match highlighter_sig.get() {
        Some(hl_arc) => match hl_arc.lock() {
            Ok(hl) => hl.spans_in_range(line_range.clone()),
            Err(_) => Vec::new(),
        },
        None => Vec::new(),
    };

    let line_start = line_range.start;
    let line_len = text.len();
    let mut out: Vec<LineFragment> = Vec::new();
    let mut cursor_byte: usize = 0;

    if spans.is_empty() {
        out.push(LineFragment { text: text.to_string(), kind: SyntaxKind::Default, is_selected: false });
    } else {
        for span in &spans {
            let rel_start = span.byte_range.start.saturating_sub(line_start);
            let rel_end = span.byte_range.end.saturating_sub(line_start).min(line_len);
            if rel_end <= cursor_byte || rel_end <= rel_start {
                continue;
            }
            let s = rel_start.max(cursor_byte);

            // Emit Default-kind text BETWEEN spans.
            if cursor_byte < s {
                if let Some(seg) = text.get(cursor_byte..s) {
                    if !seg.is_empty() {
                        out.push(LineFragment { text: seg.to_string(), kind: SyntaxKind::Default, is_selected: false });
                    }
                }
            }
            // Emit the colored span (clamped to UTF-8 boundaries via `get`).
            if let Some(seg) = text.get(s..rel_end) {
                if !seg.is_empty() {
                    out.push(LineFragment { text: seg.to_string(), kind: span.kind, is_selected: false });
                }
            }
            cursor_byte = rel_end;
        }

        // Trailing Default-kind text after the last span.
        if cursor_byte < line_len {
            if let Some(seg) = text.get(cursor_byte..) {
                if !seg.is_empty() {
                    out.push(LineFragment { text: seg.to_string(), kind: SyntaxKind::Default, is_selected: false });
                }
            }
        }
    }

    if out.is_empty() {
        out.push(LineFragment { text: text.to_string(), kind: SyntaxKind::Default, is_selected: false });
    }

    // Now inject bracket match/unmatched spans by splitting the out fragments!
    let mut brackets_on_line = Vec::new();
    match bracket_paint {
        BracketPaint::None => {}
        BracketPaint::Unmatched { cursor_byte } => {
            if cursor_byte >= line_range.start && cursor_byte < line_range.end {
                brackets_on_line.push((cursor_byte, SyntaxKind::BracketUnmatched));
            }
        }
        BracketPaint::Matched { cursor_byte, pair_byte } => {
            if cursor_byte >= line_range.start && cursor_byte < line_range.end {
                brackets_on_line.push((cursor_byte, SyntaxKind::BracketMatch));
            }
            if pair_byte >= line_range.start && pair_byte < line_range.end {
                brackets_on_line.push((pair_byte, SyntaxKind::BracketMatch));
            }
        }
    }

    for (abs_pos, bracket_kind) in brackets_on_line {
        let rel_pos = abs_pos.saturating_sub(line_start);
        let mut new_out = Vec::with_capacity(out.len() + 2);
        let mut current_offset = 0;
        let mut split_done = false;

        for frag in out {
            let frag_len = frag.text.len();
            if !split_done && rel_pos >= current_offset && rel_pos < current_offset + frag_len {
                let split_idx = rel_pos - current_offset;
                let before = &frag.text[..split_idx];
                let bracket_char = &frag.text[split_idx..split_idx + 1];
                let after = &frag.text[split_idx + 1..];

                if !before.is_empty() {
                    new_out.push(LineFragment { text: before.to_string(), kind: frag.kind, is_selected: frag.is_selected });
                }
                new_out.push(LineFragment { text: bracket_char.to_string(), kind: bracket_kind, is_selected: frag.is_selected });
                if !after.is_empty() {
                    new_out.push(LineFragment { text: after.to_string(), kind: frag.kind, is_selected: frag.is_selected });
                }
                split_done = true;
            } else {
                new_out.push(frag);
            }
            current_offset += frag_len;
        }
        out = new_out;
    }

    // Inject URL/email link highlighting by splitting Default-kind fragments
    {
        let links = link_detect::detect_links(text);
        if !links.is_empty() {
            let mut link_out = Vec::with_capacity(out.len() + links.len() * 2);
            let mut byte_offset: usize = 0;

            for frag in out {
                let frag_start = byte_offset;
                let frag_end = frag_start + frag.text.len();
                byte_offset = frag_end;

                // Only split Default-kind fragments — we don't override syntax highlighting
                if frag.kind != SyntaxKind::Default {
                    link_out.push(frag);
                    continue;
                }

                let mut cursor = frag_start;
                let mut any_link = false;

                for link in &links {
                    if link.end <= frag_start || link.start >= frag_end {
                        continue; // link outside this fragment
                    }
                    any_link = true;
                    let link_start = link.start.max(frag_start);
                    let link_end = link.end.min(frag_end);

                    // Text before the link
                    if link_start > cursor {
                        if let Some(seg) = text.get(cursor..link_start) {
                            if !seg.is_empty() {
                                link_out.push(LineFragment {
                                    text: seg.to_string(),
                                    kind: SyntaxKind::Default,
                                    is_selected: frag.is_selected,
                                });
                            }
                        }
                    }
                    // The link itself
                    if let Some(seg) = text.get(link_start..link_end) {
                        if !seg.is_empty() {
                            link_out.push(LineFragment {
                                text: seg.to_string(),
                                kind: SyntaxKind::Link,
                                  is_selected: frag.is_selected,
                            });
                        }
                    }
                    cursor = link_end;
                }

                if any_link {
                    // Trailing text after the last link
                    if cursor < frag_end {
                        if let Some(seg) = text.get(cursor..frag_end) {
                            if !seg.is_empty() {
                                link_out.push(LineFragment {
                                    text: seg.to_string(),
                                    kind: SyntaxKind::Default,
                                    is_selected: frag.is_selected,
                                });
                            }
                        }
                    }
                } else {
                    link_out.push(frag);
                }
            }
            out = link_out;
        }
    }

    // Now inject selection highlighting by splitting the fragments!
    let mut selection_out = Vec::new();
    let line_1b = idx + 1;
    let mut col_1b = 1;

    for frag in out {
        let mut chars = frag.text.chars().peekable();
        if chars.peek().is_none() {
            selection_out.push(frag);
            continue;
        }

        let mut current_run = String::new();
        let mut current_selected = None;

        while let Some(c) = chars.next() {
            let selected = selection.contains(line_1b, col_1b);
            col_1b += 1;

            if current_selected.is_none() {
                current_selected = Some(selected);
                current_run.push(c);
            } else if current_selected == Some(selected) {
                current_run.push(c);
            } else {
                // Flush the run
                selection_out.push(LineFragment {
                    text: current_run,
                    kind: frag.kind,
                    is_selected: current_selected.unwrap(),
                });
                // Start a new run
                current_run = c.to_string();
                current_selected = Some(selected);
            }
        }
        if !current_run.is_empty() {
            selection_out.push(LineFragment {
                text: current_run,
                kind: frag.kind,
                is_selected: current_selected.unwrap(),
            });
        }
    }
    out = selection_out;

    out
}

/// View options shared by the editor body and its overlays (all settings-driven, see
/// `editor.line_numbers`, `editor.indent_guides`, `editor.ruler_column`, `editor.minimap`).
#[derive(Clone, Copy)]
pub struct EditorOpts {
    pub line_numbers: RwSignal<bool>,
    pub indent_guides: RwSignal<bool>,
    /// 1-based column of the ruler line; 0 = off.
    pub ruler_column: RwSignal<usize>,
    pub minimap: RwSignal<bool>,
    /// Visible part of the document (scroll offset and size), written by the scroll view.
    pub viewport: RwSignal<Rect>,
    pub tab_size: usize,
    /// Bumped when the editor font changes (the views re-read [`editor_font`]).
    pub font_rev: RwSignal<u64>,
    /// `editor.word_wrap` and `editor.wrap_column` (0 = wrap at the window edge).
    pub word_wrap: RwSignal<bool>,
    pub wrap_column: RwSignal<usize>,
    /// Width of the editor body in px, written when it is laid out.
    pub body_width: RwSignal<f64>,
    /// Index progress, pending go-to and the long-line window of a huge document (see `viewport`).
    pub vp: crate::viewport::ViewportSignals,
}

impl EditorOpts {
    pub fn new(line_numbers: RwSignal<bool>) -> Self {
        Self {
            line_numbers,
            indent_guides: create_rw_signal(true),
            ruler_column: create_rw_signal(0),
            minimap: create_rw_signal(false),
            viewport: create_rw_signal(Rect::new(0.0, 0.0, 0.0, 600.0)),
            tab_size: 4,
            font_rev: create_rw_signal(0),
            word_wrap: create_rw_signal(false),
            wrap_column: create_rw_signal(0),
            body_width: create_rw_signal(0.0),
            vp: crate::viewport::ViewportSignals::new(),
        }
    }

    /// Apply `editor.font_family` / `editor.font_size` / `editor.word_wrap` / `editor.wrap_column`.
    pub fn apply_settings(&self, st: &brouillon_core::settings::Settings) {
        set_editor_font(&st.get_str("editor.font_family", ""), st.get_i64("editor.font_size", 0));
        self.font_rev.update(|r| *r += 1);
        self.word_wrap.set(st.get_bool("editor.word_wrap", false));
        self.wrap_column.set(st.get_i64("editor.wrap_column", 0).clamp(0, 1000) as usize);
    }
}

/// Screen rows of the wrapped document; `None` = no wrapping (one row per visible line).
pub type WrapSig = RwSignal<Option<Arc<brouillon_editor_engine::wrap::WrapMap>>>;

/// One virtual-list item: (source line, first char, end char, revision). `end` is `usize::MAX`
/// for "to the end of the line"; an unwrapped line is `(line, 0, usize::MAX, rev)`.
pub type RowKey = (usize, usize, usize, u64);

/// Screen row (0-based) of a 1-based (line, col) caret and its 1-based column inside that row.
pub fn caret_row(
    wrap: &Option<Arc<brouillon_editor_engine::wrap::WrapMap>>,
    fs: &brouillon_editor_engine::folding::FoldState,
    total: usize,
    line_1b: usize,
    col_1b: usize,
) -> (usize, usize) {
    match wrap {
        Some(w) => {
            let (r, c) = w.locate(line_1b.saturating_sub(1), col_1b.saturating_sub(1));
            (r, c + 1)
        }
        None => (fold_row(fs, total, line_1b.saturating_sub(1)), col_1b),
    }
}

/// Caret (line, col), both 1-based, for a click on screen row `row` at horizontal pixel `x`.
/// `line_chars(line0)` is the length in characters of a source line without its line break.
pub fn click_target(
    wrap: &Option<Arc<brouillon_editor_engine::wrap::WrapMap>>,
    fs: &brouillon_editor_engine::folding::FoldState,
    total: usize,
    row: usize,
    x: f64,
    cw: f64,
    line_chars: impl Fn(usize) -> usize,
) -> (usize, usize) {
    match wrap {
        None => {
            let line_1b = click_line(fold_line(fs, total, row), total);
            (line_1b, click_col(x, cw, line_chars(line_1b - 1)))
        }
        Some(w) => {
            let Some(r) = w.row(row) else { return (1, 1) };
            let line_1b = click_line(r.line, total);
            let len = line_chars(line_1b - 1);
            let seg_len = r.end.min(len).saturating_sub(r.start);
            let mut c0 = click_col(x, cw, seg_len) - 1;
            if r.end < len {
                // not the last row of the line: the column after its last char is the next row's first
                c0 = c0.min(seg_len.saturating_sub(1));
            }
            (line_1b, r.start + c0 + 1)
        }
    }
}

/// Column marker: one full-height line right after column `ruler_column` (0 = hidden).
pub fn ruler_view(ruler_column: RwSignal<usize>, theme_idx: RwSignal<usize>, font_rev: RwSignal<u64>) -> impl IntoView {
    empty()
        .style(move |s| {
            let _ = font_rev.get();
            let t = current_theme(theme_idx.get());
            match brouillon_editor_engine::guides::ruler_column(ruler_column.get() as i64) {
                Some(c) => s
                    .absolute()
                    .inset_top(0.0)
                    .inset_bottom(0.0)
                    .inset_left(TEXT_ORIGIN_X + c as f64 * char_width())
                    .width(1.0)
                    .background(t.accent.multiply_alpha(0.35)),
                None => s.hide(),
            }
        })
        .pointer_events(|| false)
}

/// Thin vertical lines at each indentation level of a row (cells from the text origin).
fn guides_overlay(cols: Vec<usize>, theme_idx: RwSignal<usize>) -> impl IntoView {
    let cw = char_width();
    floem::views::stack_from_iter(cols.into_iter().map(move |c| {
        empty()
            .style(move |s| {
                let t = current_theme(theme_idx.get());
                s.absolute().inset_left(TEXT_ORIGIN_X + c as f64 * cw).inset_top(0.0).width(1.0).height(line_height()).background(t.fg.multiply_alpha(0.13))
            })
            .pointer_events(|| false)
    }))
    .style(|s| s.absolute().inset_left(0.0).inset_top(0.0).width_full().height(line_height()).z_index(2))
    .pointer_events(|| false)
}

/// One rendered line: 6-digit gutter + monospace text, with current-line
/// background and caret overlay when this line matches the cursor.
#[allow(clippy::too_many_arguments)]
pub fn line_row(
    idx: usize,
    _full_text: String,
    fragments: Vec<LineFragment>,
    cursor: RwSignal<(usize, usize)>,
    theme_idx: RwSignal<usize>,
    fold_state_sig: RwSignal<brouillon_editor_engine::folding::FoldState>,
    line_numbers: RwSignal<bool>,
    notes: crate::link_view::NotesView,
    guide_cols: Vec<usize>,
    font_rev: RwSignal<u64>,
    first_row: bool,
    // Gutter text for this line (`12` or `~12` while the number is an estimate) and the column the
    // drawn text starts at (non-zero only for the window of a very long line).
    gutter: String,
    first_col: usize,
) -> impl IntoView {
    let line_1b = idx + 1;
    let guides = guides_overlay(guide_cols, theme_idx);

    let fold_indicator = label(move || {
        let fs = fold_state_sig.get();
        if first_row && fs.is_fold_header(idx) {
            let is_folded = fs.regions.iter().find(|r| r.start_line == idx).map(|r| r.is_folded).unwrap_or(false);
            if is_folded { "+" } else { "-" }
        } else {
            " "
        }.to_string()
    })
    .on_event_stop(EventListener::PointerDown, move |_| {
        crate::kbd_focus::editor_pointer();
        fold_state_sig.update(|fs| {
            fs.toggle_fold(idx);
        });
        // Folding a block that holds the caret moves the caret to the header.
        let (l, c) = cursor.get_untracked();
        let shown = fold_state_sig.get_untracked().nearest_visible(l.saturating_sub(1)) + 1;
        if shown != l {
            cursor.set((shown, c));
        }
    })
    .style(move |s| {
        let t = current_theme(theme_idx.get());
        s.width(14.0)
            .color(t.accent)
            .cursor(CursorStyle::Pointer)
            .items_center()
            .justify_center()
    });

    // Notes an agent left on this line (owned by that agent, one click removes one).
    let notes_sig = notes.notes;
    let remove_note = notes.remove.clone();
    let note_views = floem::views::dyn_container(
        move || notes_sig.with(|v| v.iter().filter(|n| n.line == line_1b).cloned().collect::<Vec<crate::link_view::LineNote>>()),
        move |list| {
            let remove = remove_note.clone();
            let views: Vec<floem::AnyView> = list
                .into_iter()
                .filter(|n| n.kind != "highlight")
                .map(|n| {
                    let id = n.id;
                    let remove = remove.clone();
                    let status = n.kind == "status";
                    label(move || format!("  < {}: {}  [x]", n.owner, n.text))
                        .on_click_stop(move |_| remove(id))
                        .style(move |s| {
                            let t = current_theme(theme_idx.get());
                            s.margin_left(10.0).padding_horiz(6.0).color(if status { t.fg_dim } else { t.accent }).background(t.accent.multiply_alpha(0.14)).border_radius(3.0).cursor(CursorStyle::Pointer)
                        })
                        .into_any()
                })
                .collect();
            floem::views::stack_from_iter(views).style(|s| s.flex_row()).into_any()
        },
    );
    stack((
        stack((
            label(move || if line_numbers.get() && first_row { gutter.clone() } else { String::new() }).style(move |s| {
                let _ = font_rev.get();
                let (fam, size) = editor_font();
                let s = s.font_family(fam).font_size(size);
                let t = current_theme(theme_idx.get());
                let marked = notes_sig.with(|v| v.iter().any(|n| n.line == line_1b));
                let s = s.color(t.fg_dim).width(40.0);
                if marked {
                    s.background(t.accent).color(t.bg)
                } else {
                    s
                }
            }),
            fold_indicator,
        ))
        .style(|s| s.flex_row().items_center().padding_right(8.0).width(60.0)),
        empty().style(move |s| {
            let t = current_theme(theme_idx.get());
            let is_current = cursor.get().0 == line_1b;
            let base = s.width(2.0).height(line_height());
            if is_current {
                base.background(t.accent)
            } else {
                base.background(Color::TRANSPARENT)
            }
        }),
        floem::views::dyn_stack(
            move || fragments.clone(),
            move |frag: &LineFragment| (frag.text.len(), frag.kind as u8, frag.is_selected),
            move |frag: LineFragment| {
                let kind = frag.kind;
                let text = frag.text.clone();
                let is_sel = frag.is_selected;
                label(move || text.clone()).style(move |s| {
                    let _ = font_rev.get();
                    let (fam, size) = editor_font();
                    let t = current_theme(theme_idx.get());
                    let mut s = s.font_family(fam).font_size(size).color(color_for_kind(&t, kind));
                    if kind == SyntaxKind::BracketMatch {
                        s = s.border_bottom(1.0).border_color(t.accent);
                    } else if kind == SyntaxKind::BracketUnmatched {
                        s = s.border_bottom(1.0).border_color(t.error);
                    } else if kind == SyntaxKind::Link {
                        s = s.border_bottom(1.0).border_color(t.accent.multiply_alpha(0.6));
                    }
                    if is_sel {
                        s = s.background(t.accent.multiply_alpha(brouillon_core::theme::SELECTION_ALPHA));
                    }
                    s
                })
            },
        )
        .style(move |s| {
            let _ = font_rev.get();
            s.flex_row().padding_left(4.0 + first_col as f64 * char_width())
        }),
        note_views,
        guides,
    ))
    .style(move |s| {
        let _ = font_rev.get();
        let t = current_theme(theme_idx.get());
        let base = s
            .flex_row()
            .height(line_height())
            .padding_left(6.0)
            .cursor(CursorStyle::Text);
        if notes_sig.with(|v| v.iter().any(|n| n.line == line_1b && n.kind == "highlight")) {
            return base.background(t.accent.multiply_alpha(0.2));
        }
        if cursor.get().0 == line_1b {
            base.background(t.bg_current_line)
        } else {
            base
        }
    })
}

/// (top, left) of the completion popup relative to the editor body: just under the
/// caret's line, at the caret's column (text origin + columns x measured cell width).
pub fn popup_anchor(line_1b: usize, col_1b: usize, scroll_y: f64, cell_w: f64) -> (f64, f64) {
    let top = line_1b.saturating_sub(1) as f64 * line_height() + line_height() - scroll_y;
    let left = TEXT_ORIGIN_X + col_1b.saturating_sub(1) as f64 * cell_w;
    (top, left)
}

/// Visible row (0-based) of source line `line0` when folds hide lines (a hidden line maps to the
/// row of the header that hides it). Without a folded region the row IS the line: no allocation.
pub fn fold_row(fs: &brouillon_editor_engine::folding::FoldState, total: usize, line0: usize) -> usize {
    if !fs.regions.iter().any(|r| r.is_folded) {
        return line0;
    }
    let v = fs.visible_lines(total);
    let i = v.partition_point(|&l| l < line0);
    if v.get(i) == Some(&line0) { i } else { i.saturating_sub(1) }
}

/// Source line (0-based) shown on visible row `row`.
pub fn fold_line(fs: &brouillon_editor_engine::folding::FoldState, total: usize, row: usize) -> usize {
    if !fs.regions.iter().any(|r| r.is_folded) {
        return row;
    }
    fs.visible_lines(total).get(row).copied().unwrap_or(total.saturating_sub(1))
}

/// Position (top, left) and size of the text caret relative to the editor body.
pub fn caret_rect(line_1b: usize, col_1b: usize, scroll_y: f64, cell_w: f64) -> (f64, f64) {
    let top = line_1b.saturating_sub(1) as f64 * line_height() - scroll_y;
    let left = TEXT_ORIGIN_X + col_1b.saturating_sub(1) as f64 * cell_w;
    (top, left)
}

/// Visible text caret: a 2 px bar at the caret's line and column. It does not blink (a blink
/// needs a repaint timer; see vendor/floem/PATCH.md) so that an idle editor stays at 0 % CPU.
pub fn caret_view(
    cursor: RwSignal<(usize, usize)>,
    theme_idx: RwSignal<usize>,
    viewport_sig: RwSignal<Rect>,
    fold_state_sig: RwSignal<brouillon_editor_engine::folding::FoldState>,
    buffer_sig: RwSignal<Arc<Buffer>>,
    wrap_sig: WrapSig,
    font_rev: RwSignal<u64>,
    vp: ViewportSignals,
) -> impl IntoView {
    empty()
        .style(move |s| {
            let _ = font_rev.get();
            let t = current_theme(theme_idx.get());
            let (line_1b, col_1b) = cursor.get();
            let (row, col_in_row) = caret_row(&wrap_sig.get(), &fold_state_sig.get(), rows_total(&vp, &buffer_sig.get()), line_1b, col_1b);
            let (top, left) = caret_rect(row + 1, col_in_row, viewport_sig.get().y0, char_width());
            s.absolute()
                .inset_top(top)
                .inset_left(left)
                .width(2.0)
                .height(line_height())
                .background(t.fg)
                .z_index(50)
        })
        .pointer_events(|| false)
}

/// Floating auto-complete suggestion list overlay.
pub fn auto_complete_popup(
    visible: RwSignal<bool>,
    completions: RwSignal<Vec<brouillon_editor_engine::auto_complete::Completion>>,
    active_idx: RwSignal<usize>,
    cursor: RwSignal<(usize, usize)>,
    theme_idx: RwSignal<usize>,
    viewport_sig: RwSignal<Rect>,
    wrap_sig: WrapSig,
) -> impl IntoView {
    floem::views::dyn_container(
        move || (visible.get(), completions.get(), active_idx.get(), cursor.get(), viewport_sig.get()),
        move |(is_visible, comps, active, (line_1b, col_1b), vp)| {
            if is_visible && !comps.is_empty() {
                // Wrapped: anchor on the screen row and column of the caret.
                let (line_1b, col_1b) = match wrap_sig.get_untracked() {
                    Some(w) => {
                        let (r, c) = w.locate(line_1b.saturating_sub(1), col_1b.saturating_sub(1));
                        (r + 1, c + 1)
                    }
                    None => (line_1b, col_1b),
                };
                let (top_offset, left_offset) = popup_anchor(line_1b, col_1b, vp.y0, char_width());

                let comps_clone = comps.clone();
                let stack_items = floem::views::dyn_stack(
                    move || comps_clone.clone(),
                    move |c: &brouillon_editor_engine::auto_complete::Completion| (c.text.clone(), c.score),
                    move |c: brouillon_editor_engine::auto_complete::Completion| {
                        let text = c.text.clone();
                        let is_selected = comps.iter().position(|x| x.text == text) == Some(active);
                        
                        label(move || text.clone()).style(move |s| {
                            let t = current_theme(theme_idx.get());
                            let mut base = s.padding_horiz(6.0).padding_vert(2.0).width(200.0);
                            if is_selected {
                                base = base.background(t.accent).color(t.bg);
                            } else {
                                base = base.color(t.fg);
                            }
                            base
                        })
                    }
                )
                .style(move |s| {
                    let t = current_theme(theme_idx.get());
                    s.absolute()
                        .inset_top(top_offset)
                        .inset_left(left_offset)
                        .z_index(100)
                        .background(t.bg_chrome)
                        .border(1.0)
                        .border_color(t.border)
                        .border_radius(4.0)
                        .flex_col()
                        .padding(4.0)
                });

                stack_items.into_any()
            } else {
                empty().into_any()
            }
        }
    )
    // Anchor the host to the editor body's top-left (same fix as the menu dropdown,
    // 44b596f): as a plain flex child of the stack it sat after the scroll area, so
    // the popup's inset_left/inset_top were measured from there, ~180 px too far right.
    .style(|s| s.absolute().inset_top(0.0).inset_left(0.0))
}

/// Virtualized line view, wrapped in a Scroll.
/// Virtual-list items: (line index, revision). The revision is part of the key
/// so that a content change rebuilds the visible rows instead of keeping stale ones.
pub fn keyed_rows(visible: Vec<usize>, rev: u64) -> im::Vector<(usize, u64)> {
    visible.into_iter().map(|i| (i, rev)).collect()
}

/// Lazy row source for the virtual list: nothing is materialised per line. The
/// list asks for the visible window only; building a 1M-entry vector on every
/// edit or caret move cost ~30 ms per signal change on a 100 MB file.
pub struct LineRows {
    total: usize,
    rev: u64,
    /// Visible source lines when something is folded; `None` = every line.
    visible: Option<Vec<usize>>,
    /// Screen rows when the document is wrapped; they already leave the folded lines out.
    wrap: Option<Arc<brouillon_editor_engine::wrap::WrapMap>>,
}

impl LineRows {
    pub fn new(total: usize, rev: u64, fs: &brouillon_editor_engine::folding::FoldState) -> Self {
        Self::new_wrapped(total, rev, fs, None)
    }

    pub fn new_wrapped(total: usize, rev: u64, fs: &brouillon_editor_engine::folding::FoldState, wrap: Option<Arc<brouillon_editor_engine::wrap::WrapMap>>) -> Self {
        let visible = if wrap.is_none() && fs.regions.iter().any(|r| r.is_folded) { Some(fs.visible_lines(total)) } else { None };
        Self { total, rev, visible, wrap }
    }
}

impl VirtualVector<RowKey> for LineRows {
    fn total_len(&self) -> usize {
        match (&self.wrap, &self.visible) {
            (Some(w), _) => w.len(),
            (None, Some(v)) => v.len(),
            (None, None) => self.total,
        }
    }
    fn slice(&mut self, range: std::ops::Range<usize>) -> impl Iterator<Item = RowKey> {
        let rev = self.rev;
        let end = range.end.min(self.total_len());
        let start = range.start.min(end);
        let (visible, wrap) = (&self.visible, &self.wrap);
        (start..end).map(move |i| match wrap {
            Some(w) => {
                let r = w.rows[i];
                (r.line, r.start, r.end, rev)
            }
            None => (visible.as_ref().map_or(i, |v| v[i]), 0, usize::MAX, rev),
        })
    }
}

/// The secondary carets of a multi-caret session, drawn like the primary one (2 px bar, no blink).
pub fn extra_carets_view(
    extras: RwSignal<Vec<(usize, usize)>>,
    theme_idx: RwSignal<usize>,
    viewport_sig: RwSignal<Rect>,
    fold_state_sig: RwSignal<brouillon_editor_engine::folding::FoldState>,
    buffer_sig: RwSignal<Arc<Buffer>>,
    wrap_sig: WrapSig,
    font_rev: RwSignal<u64>,
    vp: ViewportSignals,
) -> impl IntoView {
    floem::views::dyn_stack(
        move || extras.get().into_iter().enumerate().collect::<Vec<_>>(),
        |(i, p)| (*i, *p),
        move |(_, (line_1b, col_1b))| {
            empty()
                .style(move |s| {
                    let _ = font_rev.get();
                    let t = current_theme(theme_idx.get());
                    let (row, col_in_row) = caret_row(&wrap_sig.get(), &fold_state_sig.get(), rows_total(&vp, &buffer_sig.get()), line_1b, col_1b);
                    let (top, left) = caret_rect(row + 1, col_in_row, viewport_sig.get().y0, char_width());
                    s.absolute().inset_top(top).inset_left(left).width(2.0).height(line_height()).background(t.fg).z_index(50)
                })
                .pointer_events(|| false)
        },
    )
    .style(|s| s.absolute().inset_top(0.0).inset_left(0.0).size(0.0, 0.0))
}

/// Rows in the document right now: the buffer's own count, or the stable estimate while the
/// line index of a huge document is still being built (never asks a lazy store to count).
fn rows_total(vp: &ViewportSignals, buf: &Buffer) -> usize {
    viewport::total_rows_for(&vp.snapshot.get(), || buf.line_count(), 0, 0)
}

fn rows_total_untracked(vp: &ViewportSignals, buf: &Buffer) -> usize {
    viewport::total_rows_for(&vp.snapshot.get_untracked(), || buf.line_count(), 0, 0)
}

/// 1-based line for a 0-based clicked row, clamped to the CURRENT document
/// (the line count at view creation is stale as soon as lines are added).
pub fn click_line(line0: usize, line_count: usize) -> usize {
    (line0 + 1).clamp(1, line_count.max(1))
}

pub fn editor_body(
    buffer_sig: RwSignal<Arc<Buffer>>,
    cursor: RwSignal<(usize, usize)>,
    theme_idx: RwSignal<usize>,
    highlighter_sig: RwSignal<Option<Arc<Mutex<Highlighter>>>>,
    bracket_paint_sig: RwSignal<BracketPaint>,
    selection_sig: RwSignal<Selection>,
    is_dragging_selection: RwSignal<bool>,
    fold_state_sig: RwSignal<brouillon_editor_engine::folding::FoldState>,
    ac_visible: RwSignal<bool>,
    ac_completions: RwSignal<Vec<brouillon_editor_engine::auto_complete::Completion>>,
    ac_index: RwSignal<usize>,
    extra_carets: RwSignal<Vec<(usize, usize)>>,
    opts: EditorOpts,
    notes: crate::link_view::NotesView,
) -> impl IntoView {
    let line_numbers = opts.line_numbers;
    let vp = opts.vp;
    // Which features a document this size may use (re-read when the text changes size class).
    create_effect(move |_| {
        let g = viewport::HugeGate::for_bytes(buffer_sig.get().byte_len());
        if vp.gate.get_untracked() != g {
            vp.gate.set(g);
        }
    });
    // A long line is drawn as a column window that follows the horizontal scroll in chunks.
    let long_seen = std::rc::Rc::new(std::cell::Cell::new(false));
    {
        let long_seen = long_seen.clone();
        create_effect(move |_| {
            let r = opts.viewport.get();
            if !long_seen.get() {
                return;
            }
            let start = viewport::window_start((r.x0 - TEXT_ORIGIN_X).max(0.0), char_width());
            if vp.hcol.get_untracked() != start {
                vp.hcol.set(start);
            }
        });
    }
    // "Go to line N" asked while the index was too short completes here once it has caught up.
    create_effect(move |_| {
        let snap = vp.snapshot.get();
        let Some(want) = vp.pending_goto.get() else { return };
        let buf = buffer_sig.get_untracked();
        if let GotoPlan::Go(l) = viewport::plan_goto(&snap, want, || buf.line_count()) {
            vp.pending_goto.set(None);
            cursor.set((l, 1));
            selection_sig.set(Selection::new((l, 1), (l, 1), false));
        }
    });

    // Row views are keyed by line index and bake the line text, syntax colours,
    // selection and bracket highlights at creation time (`compute_line_fragments`
    // is not reactive). Without a revision in the key an edited line kept its
    // stale row: typed characters changed the buffer but never reached the
    // screen. `rev` bumps whenever one of those inputs changes, so the (few)
    // visible rows are rebuilt; off-screen rows are never instantiated.
    let rev = create_rw_signal(0u64);
    // Soft wrap: characters per screen row (None = off), then the rows of the visible lines.
    let wrap_sig: WrapSig = create_rw_signal(None);
    let cols_sig: RwSignal<Option<usize>> = create_rw_signal(None);
    create_effect(move |_| {
        let _ = opts.font_rev.get();
        let width = opts.body_width.get() - TEXT_ORIGIN_X - 14.0;
        let cols = brouillon_editor_engine::wrap::wrap_cols(opts.word_wrap.get(), opts.wrap_column.get() as i64, width, char_width());
        if cols_sig.get_untracked() != cols {
            cols_sig.set(cols);
        }
    });
    create_effect(move |_| {
        let cols = cols_sig.get();
        let buf = buffer_sig.get();
        let fs = fold_state_sig.get();
        // Wrap walks every line: not on a big document (decided from the byte size, no counting).
        let allowed = viewport::HugeGate::for_bytes(buf.byte_len()).word_wrap;
        let total = if allowed { buf.line_count() } else { usize::MAX };
        let map = match cols {
            Some(c) if allowed && total <= brouillon_editor_engine::wrap::WRAP_MAX_LINES => {
                Some(Arc::new(brouillon_editor_engine::wrap::WrapMap::build(total, &fs, c, |i| buf.nth_line(i).unwrap_or_default())))
            }
            _ => None,
        };
        wrap_sig.set(map);
    });
    // One keystroke sets the buffer, the caret/selection and sometimes the brackets in a row;
    // rebuilding the visible rows after each of those set them 3-5 times per key. The bump is
    // coalesced to once per event-loop turn.
    let rev_pending = std::rc::Rc::new(std::cell::Cell::new(false));
    create_effect(move |_| {
        let _ = buffer_sig.get();
        let _ = selection_sig.get();
        let _ = bracket_paint_sig.get();
        let _ = highlighter_sig.get();
        let _ = opts.indent_guides.get();
        let _ = wrap_sig.get();
        let _ = opts.font_rev.get();
        let _ = vp.snapshot.get();
        let _ = vp.hcol.get();
        if rev_pending.replace(true) {
            return;
        }
        let rev_pending = rev_pending.clone();
        floem::action::exec_after(std::time::Duration::ZERO, move |_| {
            rev_pending.set(false);
            rev.update(|r| *r += 1);
        });
    });

    let viewport_sig: RwSignal<Rect> = opts.viewport;
    let indices = move || {
        let buf = buffer_sig.get();
        let snap = vp.snapshot.get();
        // The scrollable extent is the estimate while the index is built, never below the top line.
        let (lh, vr) = (line_height(), viewport_sig.get_untracked());
        let total = viewport::total_rows_for(&snap, || buf.line_count(), (vr.y0 / lh).max(0.0) as usize, (vr.height() / lh).ceil() as usize);
        let fs = fold_state_sig.get();
        let r = rev.get();
        LineRows::new_wrapped(total, r, &fs, wrap_sig.get())
    };

    // Caret that was primary when an Alt+click started (promoted to an extra caret on release).
    let alt_prev: RwSignal<Option<(usize, usize)>> = create_rw_signal(None);

    container(
        stack((
            scroll(
                // virtual_stack, not virtual_list: the list also builds an N-entry offsets
                // vector on every change (8 MB and ~5 ms per signal on a 1M-line file) that
                // only serves its own row selection, which the editor does not use.
                virtual_stack(
                    VirtualDirection::Vertical,
                    VirtualItemSize::Fixed(Box::new(move || {
                        let _ = opts.font_rev.get();
                        line_height()
                    })),
                    indices,
                    move |k: &RowKey| *k,
                    move |(idx, start, end, _rev): RowKey| {
                        let buf = buffer_sig.get();
                        let snap = vp.snapshot.get_untracked();
                        let gutter = viewport::gutter_label(&snap, idx);
                        if viewport::row_state(&snap, idx) == RowState::Indexing {
                            // Not indexed yet: a quiet placeholder; the text appears when the index passes this line.
                            let dots = vec![LineFragment { text: "\u{2026}".into(), kind: SyntaxKind::Comment, is_selected: false }];
                            return line_row(idx, String::new(), dots, cursor, theme_idx, fold_state_sig, line_numbers, notes.clone(), Vec::new(), opts.font_rev, true, gutter, 0).into_any();
                        }
                        if start == 0 && end == usize::MAX && viewport::is_long_line(buf.line_len_bytes(idx)) {
                            // Very long line: only a window of columns is read and drawn.
                            long_seen.set(true);
                            let w = buf.read_line_window(idx, vp.hcol.get_untracked(), viewport::window_cols(opts.body_width.get_untracked(), char_width()));
                            let frags = vec![LineFragment { text: w.text.clone(), kind: SyntaxKind::Default, is_selected: false }];
                            return line_row(idx, w.text, frags, cursor, theme_idx, fold_state_sig, line_numbers, notes.clone(), Vec::new(), opts.font_rev, true, gutter, w.first_col).into_any();
                        }
                        let text = buf.nth_line(idx).unwrap_or_default();
                        let mut fragments = compute_line_fragments(&buf, idx, &text, highlighter_sig, bracket_paint_sig.get(), selection_sig.get());
                        let first_row = start == 0;
                        if !(first_row && end == usize::MAX) {
                            // One screen row of a wrapped line: only its characters.
                            fragments = brouillon_editor_engine::wrap::slice_pieces(
                                &fragments,
                                |f| f.text.chars().count(),
                                |f, a, b| LineFragment { text: f.text.chars().skip(a).take(b - a).collect(), kind: f.kind, is_selected: f.is_selected },
                                start,
                                end,
                            );
                        }
                        let guide_cols = if first_row && opts.indent_guides.get_untracked() && vp.gate.get_untracked().indent_guides {
                            let at = |i: usize| buf.nth_line(i);
                            brouillon_editor_engine::guides::guide_columns(brouillon_editor_engine::guides::effective_indent(&at, idx, opts.tab_size), opts.tab_size)
                        } else {
                            Vec::new()
                        };
                        line_row(idx, text, fragments, cursor, theme_idx, fold_state_sig, line_numbers, notes.clone(), guide_cols, opts.font_rev, first_row, gutter, 0).into_any()
                    },
                )
                .style(|s| s.flex_col().width_full()),
            )
            .on_scroll(move |rect| {
                viewport_sig.set(rect);
            })
            .scroll_to(move || {
                let (line_1b, _col) = cursor.get();
                // Untracked: tracking the viewport re-ran this on every wheel/thumb scroll and
                // snapped the view straight back to the caret (the document could not be scrolled).
                let vp = viewport_sig.get_untracked();
                if vp.height() <= 0.0 {
                    return None;
                }
                let (row, _) = caret_row(&wrap_sig.get_untracked(), &fold_state_sig.get_untracked(), rows_total_untracked(&opts.vp, &buffer_sig.get_untracked()), line_1b, _col);
                let line_y = row as f64 * line_height();
                if line_y < vp.y0 {
                    Some(Point::new(vp.x0, line_y))
                } else if line_y + line_height() > vp.y1 {
                    Some(Point::new(vp.x0, line_y + line_height() - vp.height()))
                } else {
                    None
                }
            })
            .style(move |s| {
                let t = current_theme(theme_idx.get());
                s.size_full().background(t.bg)
            }),
            ruler_view(opts.ruler_column, theme_idx, opts.font_rev),
            crate::minimap_view::minimap_overlay(buffer_sig, viewport_sig, cursor, theme_idx, opts.minimap, opts.tab_size, vp),
            caret_view(cursor, theme_idx, viewport_sig, fold_state_sig, buffer_sig, wrap_sig, opts.font_rev, vp),
            extra_carets_view(extra_carets, theme_idx, viewport_sig, fold_state_sig, buffer_sig, wrap_sig, opts.font_rev, vp),
            auto_complete_popup(ac_visible, ac_completions, ac_index, cursor, theme_idx, viewport_sig, wrap_sig),
        ))
        .style(|s| s.size_full()),
    )
    .style(|s| s.size_full())
    .on_resize(move |r| {
        if (opts.body_width.get_untracked() - r.width()).abs() > 0.5 {
            opts.body_width.set(r.width());
        }
    })
    .on_event_stop(EventListener::PointerDown, move |event| {
        crate::kbd_focus::editor_pointer();
        if let Event::PointerDown(pe) = event {
            let scroll_y = viewport_sig.get().y0;
            let row = ((pe.pos.y + scroll_y) / line_height()).floor().max(0.0) as usize;
            let buf = buffer_sig.get_untracked();
            let (line_1b, col_1b) = click_target(&wrap_sig.get_untracked(), &fold_state_sig.get_untracked(), rows_total_untracked(&vp, &buf), row, pe.pos.x, char_width(), |l| {
                viewport::line_chars_fast(&*buf, l)
            });

            // Alt+click adds a caret (kept on release when the pointer did not drag into a
            // column selection); a plain click collapses to a single caret.
            if pe.modifiers.alt() {
                alt_prev.set(Some(cursor.get_untracked()));
            } else {
                alt_prev.set(None);
                if !extra_carets.get_untracked().is_empty() {
                    extra_carets.set(Vec::new());
                }
            }
            cursor.set((line_1b, col_1b));
            is_dragging_selection.set(true);
            selection_sig.set(Selection::new((line_1b, col_1b), (line_1b, col_1b), pe.modifiers.alt()));
        }
    })
    .on_event_stop(EventListener::PointerMove, move |event| {
        if let Event::PointerMove(pe) = event {
            if is_dragging_selection.get_untracked() {
                let scroll_y = viewport_sig.get().y0;
                let row = ((pe.pos.y + scroll_y) / line_height()).floor().max(0.0) as usize;
                let buf = buffer_sig.get_untracked();
                let (line_1b, col_1b) = click_target(&wrap_sig.get_untracked(), &fold_state_sig.get_untracked(), rows_total_untracked(&vp, &buf), row, pe.pos.x, char_width(), |l| {
                    viewport::line_chars_fast(&*buf, l)
                });

                cursor.set((line_1b, col_1b));
                selection_sig.update(|sel| {
                    sel.head = (line_1b, col_1b);
                    sel.is_block = pe.modifiers.alt();
                });
            }
        }
    })
    .on_event_stop(EventListener::PointerUp, move |event| {
        if let Event::PointerUp(_pe) = event {
            is_dragging_selection.set(false);
            if let Some(prev) = alt_prev.get_untracked() {
                alt_prev.set(None);
                if selection_sig.get_untracked().is_empty() && prev != cursor.get_untracked() {
                    extra_carets.update(|v| {
                        if !v.contains(&prev) {
                            v.push(prev);
                        }
                    });
                }
            }
        }
    })
}

#[cfg(test)]
mod tests {
    #[test]
    fn click_col_uses_cell_width_and_text_origin() {
        // boundary after 9 chars at 7.15 px/cell: the old 8.0 / 74.0 mapping returned col 9
        assert_eq!(click_col(72.0 + 9.0 * 7.15, 7.15, 40), 10);
        assert_eq!(click_col(10.0, 7.15, 40), 1, "gutter click -> column 1");
        assert_eq!(click_col(5000.0, 7.15, 12), 13, "past the end -> after last char");
    }

    #[test]
    fn caret_rect_sits_on_caret_cell_and_follows_scroll() {
        // caret before the 3rd char of line 4, scrolled 10 px
        let (top, left) = caret_rect(4, 3, 10.0, 8.0);
        assert_eq!(top, 3.0 * line_height() - 10.0);
        assert_eq!(left, TEXT_ORIGIN_X + 16.0);
    }

    #[test]
    fn popup_anchor_follows_caret_cell_and_scroll() {
        // caret after "alp" on line 3 (col 4), scrolled 10 px: under line 3, 3 cells right of the text origin
        let (top, left) = popup_anchor(3, 4, 10.0, 7.15);
        assert_eq!(top, 2.0 * 18.0 + 18.0 - 10.0);
        assert!((left - (72.0 + 3.0 * 7.15)).abs() < 1e-9);
        assert_eq!(popup_anchor(1, 1, 0.0, 7.15), (18.0, 72.0));
    }

    #[test]
    fn measured_char_width_is_sane() {
        let w = char_width();
        assert!((4.0..20.0).contains(&w), "{w}");
    }

    #[test]
    fn fold_row_and_line_are_inverse_over_visible_lines() {
        use brouillon_editor_engine::folding::{compute_fold_regions, FoldState};
        let text = "a
  b
  c
d
  e
f
";
        let mut fs = FoldState::new(compute_fold_regions(text));
        // nothing folded: identity
        assert_eq!((fold_row(&fs, 6, 4), fold_line(&fs, 6, 4)), (4, 4));
        fs.fold(0); // hides lines 1 and 2
        assert_eq!(fold_row(&fs, 6, 0), 0);
        assert_eq!(fold_row(&fs, 6, 3), 1, "line d sits on row 1");
        assert_eq!(fold_row(&fs, 6, 2), 0, "a hidden line maps to its header row");
        assert_eq!(fold_line(&fs, 6, 1), 3);
        assert_eq!(fold_line(&fs, 6, 2), 4);
        for row in 0..4 {
            assert_eq!(fold_row(&fs, 6, fold_line(&fs, 6, row)), row);
        }
    }

    #[test]
    fn editor_font_settings_resolve_with_fallbacks_and_limits() {
        let (fam, size) = resolve_editor_font("", 0);
        assert_eq!((fam.as_str(), size), (EDITOR_FONT_FAMILY, 13.0));
        let (fam, size) = resolve_editor_font("  \"Cascadia Mono\" ", 16);
        assert_eq!(fam, format!("Cascadia Mono, {EDITOR_FONT_FAMILY}"));
        assert_eq!(size, 16.0);
        assert_eq!(resolve_editor_font("x", 3).1, 6.0, "too small -> 6");
        assert_eq!(resolve_editor_font("x", 500).1, 72.0, "too large -> 72");
        assert_eq!(resolve_editor_font("x", -4).1, 13.0, "negative -> default");
        assert!(!resolve_editor_font("a;b
c", 0).0.contains([';', '\n']));
        // row height scales with the size; the default stays 18 px
        assert_eq!(line_height_for(13.0), 18.0);
        assert_eq!(line_height_for(26.0), 36.0);
        assert!(line_height_for(6.0) >= 10.0);
    }

    #[test]
    fn settings_reach_the_editor_font_and_wrap_signals() {
        use brouillon_core::settings::{Layer, Settings};
        run(|| {
            let o = EditorOpts::new(create_rw_signal(true));
            let before = o.font_rev.get_untracked();
            let st = Settings::build(&[(Layer::User, "[editor]
word_wrap = true
wrap_column = 72
font_size = 0
")]);
            o.apply_settings(&st);
            assert!(o.word_wrap.get_untracked());
            assert_eq!(o.wrap_column.get_untracked(), 72);
            assert_eq!(o.font_rev.get_untracked(), before + 1);
            assert!(st.unknown_keys.is_empty());
        });
    }

    fn wrapped(text: &str, cols: usize) -> (Option<Arc<brouillon_editor_engine::wrap::WrapMap>>, brouillon_editor_engine::folding::FoldState, usize) {
        let lines: Vec<String> = text.split('\n').map(String::from).collect();
        let fs = brouillon_editor_engine::folding::FoldState::empty();
        let m = brouillon_editor_engine::wrap::WrapMap::build(lines.len(), &fs, cols, |i| lines[i].clone());
        (Some(Arc::new(m)), fs, lines.len())
    }

    #[test]
    fn caret_row_follows_wrapping_and_folds() {
        let (w, fs, total) = wrapped("aaaa bbbb cccc dddd
short", 10);
        // line 1 col 12 sits on the second row of line 1 (chars 10..), 2 chars in
        assert_eq!(caret_row(&w, &fs, total, 1, 13), (1, 3));
        assert_eq!(caret_row(&w, &fs, total, 2, 3), (2, 3));
        // no wrap: row = fold row, column untouched
        assert_eq!(caret_row(&None, &fs, total, 2, 3), (1, 3));
    }

    #[test]
    fn click_target_maps_rows_back_to_line_and_column() {
        let text = "aaaa bbbb cccc dddd
short";
        let (w, fs, total) = wrapped(text, 10);
        let lens = |l: usize| text.split('\n').nth(l).map_or(0, |t| t.chars().count());
        let cw = 8.0;
        let x = |col0: usize| TEXT_ORIGIN_X + col0 as f64 * cw;
        // second screen row of line 1, 3rd char of the row -> column 10 + 3 + 1
        assert_eq!(click_target(&w, &fs, total, 1, x(3), cw, lens), (1, 14));
        // far right on the first row stops before the break (the next row owns column 11)
        assert_eq!(click_target(&w, &fs, total, 0, x(60), cw, lens), (1, 10));
        // far right on the last row of the line goes after its last char (19 chars)
        assert_eq!(click_target(&w, &fs, total, 1, x(60), cw, lens), (1, 20));
        assert_eq!(click_target(&w, &fs, total, 2, x(2), cw, lens), (2, 3));
        // without wrapping this is the classic mapping
        assert_eq!(click_target(&None, &fs, total, 1, x(2), cw, lens), (2, 3));
    }

    #[test]
    fn wrapped_line_rows_list_every_screen_row() {
        let (w, fs, _) = wrapped("aaaa bbbb cccc dddd
short", 10);
        let mut rows = LineRows::new_wrapped(2, 5, &fs, w);
        assert_eq!(rows.total_len(), 3);
        let all: Vec<RowKey> = rows.slice(0..3).collect();
        assert_eq!(all, vec![(0, 0, 10, 5), (0, 10, usize::MAX, 5), (1, 0, usize::MAX, 5)]);
    }

    #[test]
    fn click_line_follows_current_line_count() {
        assert_eq!(click_line(0, 0), 1);
        assert_eq!(click_line(500, 3), 3);
        assert_eq!(click_line(500, 1000), 501);
    }

    use super::*;
    use floem::reactive::{with_scope, Scope, create_rw_signal};

    fn run<F: FnOnce() + 'static>(f: F) {
        let scope = Scope::new();
        with_scope(scope, f);
        scope.dispose();
    }

    #[test]
    fn row_keys_change_with_revision() {
        // Regression: rows were keyed by line index only, so an edited line kept
        // its stale view (typing never reached the screen).
        let a = keyed_rows(vec![0, 1, 2], 1);
        let b = keyed_rows(vec![0, 1, 2], 2);
        assert_eq!(a.len(), 3);
        assert!(a.iter().zip(b.iter()).all(|(x, y)| x.0 == y.0 && x != y));
        assert_eq!(a, keyed_rows(vec![0, 1, 2], 1));
    }

    #[test]
    fn line_rows_is_lazy_and_matches_keyed_rows() {
        use brouillon_editor_engine::folding::FoldState;
        let fs = FoldState::empty();
        let mut rows = LineRows::new(1_050_001, 7, &fs);
        assert_eq!(rows.total_len(), 1_050_001);
        let win: Vec<_> = rows.slice(500_000..500_003).collect();
        assert_eq!(win, vec![(500_000, 0, usize::MAX, 7), (500_001, 0, usize::MAX, 7), (500_002, 0, usize::MAX, 7)]);
        // Out-of-range windows are clamped, never panic.
        assert_eq!(rows.slice(1_050_000..1_060_000).count(), 1);
        assert_eq!(rows.slice(2_000_000..2_000_010).count(), 0);
    }

    #[test]
    fn line_rows_skips_folded_lines() {
        use brouillon_editor_engine::folding::{FoldRegion, FoldState};
        let fs = FoldState::new(vec![FoldRegion { start_line: 1, end_line: 3, indent_level: 0, is_folded: true }]);
        let mut rows = LineRows::new(6, 1, &fs);
        let all: Vec<usize> = rows.slice(0..rows.total_len()).map(|(i, ..)| i).collect();
        assert_eq!(all, fs.visible_lines(6));
        assert!(all.len() < 6);
    }

    #[test]
    fn test_line_fragment_equality() {
        let f1 = LineFragment {
            text: "hello".to_string(),
            kind: SyntaxKind::Keyword,
            is_selected: false,
        };
        let f2 = LineFragment {
            text: "hello".to_string(),
            kind: SyntaxKind::Keyword,
            is_selected: false,
        };
        let f3 = LineFragment {
            text: "world".to_string(),
            kind: SyntaxKind::String,
            is_selected: true,
        };
        assert_eq!(f1, f2);
        assert_ne!(f1, f3);
        assert!(format!("{:?}", f1).contains("Keyword"));
    }

    #[test]
    fn test_color_for_kind() {
        let t = Theme::tokyo_night();
        let c = color_for_kind(&t, SyntaxKind::Keyword);
        assert_eq!(c, t.syn_keyword);
        let c_comment = color_for_kind(&t, SyntaxKind::Comment);
        assert_eq!(c_comment, t.syn_comment);
        let c_link = color_for_kind(&t, SyntaxKind::Link);
        assert_eq!(c_link, t.accent);
    }

    #[test]
    fn test_new_kinds_reach_theme_slots() {
        for t in brouillon_core::theme::ThemeRegistry::new().themes {
            assert_eq!(color_for_kind(&t, SyntaxKind::Keyword2), t.syn_keyword2);
            assert_eq!(color_for_kind(&t, SyntaxKind::Keyword3), t.syn_keyword3);
            assert_eq!(color_for_kind(&t, SyntaxKind::Keyword4), t.syn_keyword4);
            assert_eq!(color_for_kind(&t, SyntaxKind::Keyword5), t.syn_keyword5);
            assert_eq!(color_for_kind(&t, SyntaxKind::Preprocessor), t.syn_preproc);
            assert_eq!(color_for_kind(&t, SyntaxKind::Operator), t.syn_operator);
            assert_eq!(color_for_kind(&t, SyntaxKind::Function), t.syn_function);
            assert_eq!(color_for_kind(&t, SyntaxKind::Tag), t.syn_tag);
            assert_eq!(color_for_kind(&t, SyntaxKind::Attribute), t.syn_attribute);
            assert_eq!(color_for_kind(&t, SyntaxKind::Escape), t.syn_escape);
            assert_eq!(color_for_kind(&t, SyntaxKind::Variable), t.syn_variable);
        }
    }

    /// End to end: file name -> language -> Highlighter -> per-line fragments
    /// (what `line_row` turns into coloured labels) -> theme colours.
    #[test]
    fn test_table_driven_spans_reach_the_painter() {
        use brouillon_editor_engine::grammars::detect_language;
        run(|| {
            let src = "#include <stdio.h>\nint main(void) { return 0; } /* c\nmore */\n";
            let buf = Buffer::from_str(src);
            let mut hl = Highlighter::new(detect_language(std::path::Path::new("main.c")));
            hl.parse(src);
            let hl_sig = create_rw_signal(Some(Arc::new(Mutex::new(hl))));
            let sel = Selection::new((1, 1), (1, 1), false);
            let t = Theme::tokyo_night();

            let l0 = compute_line_fragments(&buf, 0, "#include <stdio.h>", hl_sig, BracketPaint::None, sel);
            let inc = l0.iter().find(|f| f.text == "#include").unwrap();
            assert_eq!(inc.kind, SyntaxKind::Preprocessor);
            assert_eq!(color_for_kind(&t, inc.kind), t.syn_preproc);

            let l1 = compute_line_fragments(&buf, 1, "int main(void) { return 0; } /* c", hl_sig, BracketPaint::None, sel);
            assert_eq!(l1.iter().find(|f| f.text == "int").unwrap().kind, SyntaxKind::Type);
            assert_eq!(l1.iter().find(|f| f.text == "main").unwrap().kind, SyntaxKind::Function);
            assert_eq!(l1.iter().find(|f| f.text == "return").unwrap().kind, SyntaxKind::Keyword);
            assert_eq!(l1.iter().find(|f| f.text == "0").unwrap().kind, SyntaxKind::Number);

            // multi-line state: line 3 is still inside the block comment
            let l2 = compute_line_fragments(&buf, 2, "more */", hl_sig, BracketPaint::None, sel);
            assert_eq!(l2[0].kind, SyntaxKind::Comment);
            assert_eq!(color_for_kind(&t, l2[0].kind), t.syn_comment);
        });
    }

    #[test]
    fn test_compute_line_fragments_empty() {
        run(|| {
            let buf = Buffer::from_str("hello world");
            let hl_sig = create_rw_signal(None);
            let frags = compute_line_fragments(&buf, 0, "hello world", hl_sig, BracketPaint::None, Selection::new((1,1), (1,1), false));
            assert_eq!(frags.len(), 1);
            assert_eq!(frags[0].text, "hello world");
            assert_eq!(frags[0].kind, SyntaxKind::Default);
            assert!(!frags[0].is_selected);
        });
    }

    #[test]
    fn test_compute_line_fragments_out_of_range() {
        run(|| {
            let buf = Buffer::from_str("hello");
            let hl_sig = create_rw_signal(None);
            let frags = compute_line_fragments(&buf, 10, "hello", hl_sig, BracketPaint::None, Selection::new((1,1), (1,1), false));
            assert_eq!(frags.len(), 1);
            assert_eq!(frags[0].text, "hello");
        });
    }

    #[test]
    fn test_compute_line_fragments_with_bracket_match() {
        run(|| {
            let buf = Buffer::from_str("fn main() {}");
            let hl_sig = create_rw_signal(None);
            let bp = BracketPaint::Matched { cursor_byte: 7, pair_byte: 8 };
            let frags = compute_line_fragments(&buf, 0, "fn main() {}", hl_sig, bp, Selection::new((1,1), (1,1), false));
            
            assert!(frags.len() >= 3);
            let p_open = frags.iter().find(|f| f.text == "(").unwrap();
            let p_close = frags.iter().find(|f| f.text == ")").unwrap();
            assert_eq!(p_open.kind, SyntaxKind::BracketMatch);
            assert_eq!(p_close.kind, SyntaxKind::BracketMatch);
        });
    }

    #[test]
    fn test_compute_line_fragments_with_bracket_unmatched() {
        run(|| {
            let buf = Buffer::from_str("fn main( {}");
            let hl_sig = create_rw_signal(None);
            let bp = BracketPaint::Unmatched { cursor_byte: 7 };
            let frags = compute_line_fragments(&buf, 0, "fn main( {}", hl_sig, bp, Selection::new((1,1), (1,1), false));
            
            let p_unmatched = frags.iter().find(|f| f.text == "(").unwrap();
            assert_eq!(p_unmatched.kind, SyntaxKind::BracketUnmatched);
        });
    }

    #[test]
    fn test_compute_line_fragments_with_selection() {
        run(|| {
            let buf = Buffer::from_str("hello world");
            let hl_sig = create_rw_signal(None);
            let selection = Selection::new((1, 7), (1, 12), false);
            let frags = compute_line_fragments(&buf, 0, "hello world", hl_sig, BracketPaint::None, selection);
            
            let unsel = frags.iter().find(|f| f.text == "hello ").unwrap();
            let sel = frags.iter().find(|f| f.text == "world").unwrap();
            assert!(!unsel.is_selected);
            assert!(sel.is_selected);
        });
    }

    #[test]
    fn test_compute_line_fragments_with_link() {
        run(|| {
            let buf = Buffer::from_str("check https://google.com for info");
            let hl_sig = create_rw_signal(None);
            let frags = compute_line_fragments(&buf, 0, "check https://google.com for info", hl_sig, BracketPaint::None, Selection::new((1,1), (1,1), false));
            
            let link_frag = frags.iter().find(|f| f.text == "https://google.com").unwrap();
            assert_eq!(link_frag.kind, SyntaxKind::Link);
        });
    }

    #[test]
    fn test_editor_body_with_folding() {
        run(|| {
            let buf_sig = create_rw_signal(Arc::new(Buffer::from_str("line 1\n  line 2\n  line 3\nline 4")));
            let cursor_sig = create_rw_signal((1, 1));
            let theme_sig = create_rw_signal(0);
            let hl_sig = create_rw_signal(None);
            let bp_sig = create_rw_signal(BracketPaint::None);
            let selection_sig = create_rw_signal(Selection::new((1,1), (1,1), false));
            let is_dragging = create_rw_signal(false);
            
            let regions = brouillon_editor_engine::folding::compute_fold_regions("line 1\n  line 2\n  line 3\nline 4");
            let fold_sig = create_rw_signal(brouillon_editor_engine::folding::FoldState::new(regions));
            
            let ac_vis = create_rw_signal(false);
            let ac_comps = create_rw_signal(Vec::new());
            let ac_idx = create_rw_signal(0);
            let _v = editor_body(
                buf_sig,
                cursor_sig,
                theme_sig,
                hl_sig,
                bp_sig,
                selection_sig,
                is_dragging,
                fold_sig,
                ac_vis,
                ac_comps,
                ac_idx,
                create_rw_signal(Vec::new()),
                EditorOpts::new(create_rw_signal(true)),
                crate::link_view::NotesView::none(),
            );
            
            // Toggle folding on line 0
            fold_sig.update(|fs| {
                let _ = fs.toggle_fold(0);
            });
            
            // Should be folded now
            let fs = fold_sig.get();
            assert!(fs.is_line_hidden(1));
            assert!(fs.is_line_hidden(2));
            assert!(!fs.is_line_hidden(3));
        });
    }

    #[test]
    fn test_line_row_rendering() {
        run(|| {
            let cursor = create_rw_signal((1, 1));
            let theme_idx = create_rw_signal(0);
            let fold_state_sig = create_rw_signal(brouillon_editor_engine::folding::FoldState::empty());
            let frags = vec![LineFragment { text: "hello".to_string(), kind: SyntaxKind::Default, is_selected: false }];
            let _v = line_row(0, "hello".to_string(), frags, cursor, theme_idx, fold_state_sig, create_rw_signal(true), crate::link_view::NotesView::none(), vec![0, 4], create_rw_signal(0), true, "    1".to_string(), 0);
        });
    }

    #[test]
    fn test_auto_complete_popup_creation() {
        run(|| {
            let visible = create_rw_signal(false);
            let completions = create_rw_signal(Vec::new());
            let active_idx = create_rw_signal(0);
            let cursor = create_rw_signal((1, 1));
            let theme_idx = create_rw_signal(0);
            let viewport_sig = create_rw_signal(Rect::ZERO);
            let _v = auto_complete_popup(visible, completions, active_idx, cursor, theme_idx, viewport_sig, create_rw_signal(None));
        });
    }
}
