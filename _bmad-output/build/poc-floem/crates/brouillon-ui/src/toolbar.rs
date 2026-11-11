#![allow(dead_code)]

//! Moldu icon toolbar — Open / Save / Cut / Copy / Paste / Find / Undo / Redo.
//!
//! Round-13 sprint C deliverable. Sits below the menu bar in the Moldu preset
//! and is hidden in Geek (the integrator wraps the call in a `dyn_container`
//! reading `preset_sig`). We just build the chrome assuming it'll be shown.
//!
//! No icon-font dep — labels are plain text plus two BMP arrow glyphs
//! (U+21B6 / U+21B7 for undo / redo) that render reliably in Consolas /
//! Menlo. Emoji fallback was considered and rejected (vger glyph coverage
//! is uneven for emoji-presentation codepoints in Floem 0.2.0).
//!
//! Action ids are the same strings the palette dispatch already routes —
//! `"file.open"`, `"edit.find"`, etc. — so the integrator can hand the
//! callback straight to the existing dispatcher.

use floem::peniko::Color;
use floem::reactive::{RwSignal, SignalGet, SignalUpdate};
use floem::style::CursorStyle;
use floem::views::{empty, label, stack, Decorators};
use floem::IntoView;
use std::sync::Arc;


pub(crate) const TOOLBAR_HEIGHT: f64 = 24.0;
const DIVIDER_WIDTH: f64 = 1.0;
const DIVIDER_VERT_INSET: f64 = 6.0;

/// Toolbar entries — `(action_id, label, group_break_after)`. `group_break_after`
/// triggers a 1-px vertical divider between this cell and the next, giving
/// the Word-style "tool groups" feel the project owner asked for. Three breaks: after
/// Save, after Paste, after Find.
pub const TOOLBAR_ITEMS: &[(&str, &str, bool)] = &[
    ("edit.undo", "\u{21B6} Undo", false),
    ("edit.redo", "\u{21B7} Redo", true), // group break: history | mode switch
];

/// Resolve a configured toolbar layout (command ids from settings) against
/// the registry. Returns the commands to draw (`None` = group divider, from an
/// empty id) and the ids that are not registered (reported, never drawn).
pub fn toolbar_commands<'a>(
    reg: &'a brouillon_core::command_registry::Registry,
    wanted: &'a [String],
) -> (Vec<Option<&'a brouillon_core::contract::CommandSpec>>, Vec<&'a str>) {
    let mut unknown = Vec::new();
    let items = wanted
        .iter()
        .filter_map(|id| {
            if id.is_empty() {
                return Some(None);
            }
            let found = reg.get(id);
            if found.is_none() {
                unknown.push(id.as_str());
            }
            found.map(Some)
        })
        .collect();
    (items, unknown)
}

/// Build the icon toolbar. Eight clickable text cells in a horizontal row
/// with three vertical dividers.
///
/// Callback contract:
/// - `on_action(id)` fires on click with one of `file.open` / `file.save` /
///   `edit.cut` / `edit.copy` / `edit.paste` / `edit.find` / `edit.undo` /
///   `edit.redo`. These match the palette dispatch ids so the integrator
///   can route through the existing callback machinery.
pub fn toolbar_view(
    theme_idx: RwSignal<usize>,
    crayon_mode: RwSignal<bool>,
    on_action: Arc<dyn Fn(&str) + 'static>,
) -> impl IntoView {
    // Undo, Redo, divider, Clavier, Crayon: sits in the header row (no row of
    // its own). Open/Save/Cut/Copy/Paste/Find are menu-only.
    let cb1 = on_action.clone();
    let cb2 = on_action.clone();
    stack((
        tool_cell(theme_idx, TOOLBAR_ITEMS[0].0, TOOLBAR_ITEMS[0].1, cb1),
        tool_cell(theme_idx, TOOLBAR_ITEMS[1].0, TOOLBAR_ITEMS[1].1, cb2),
        divider(theme_idx),
        mode_cell(theme_idx, move || !crayon_mode.get(), "\u{2328} Clavier", move || {
            crayon_mode.set(false);
        }),
        mode_cell(theme_idx, move || crayon_mode.get(), "\u{270F} Crayon", move || {
            crayon_mode.set(true);
        }),
    ))
    .style(|s| s.flex_row().items_center().height(TOOLBAR_HEIGHT).margin_left(8.0))
}

/// One toolbar cell — clickable text label with hover background and the
/// shared `cursor: pointer`. Font size kept at 13 to match the menu bar
/// so the two strips read as a single visual block.
fn tool_cell(
    theme_idx: RwSignal<usize>,
    action_id: &'static str,
    text: &'static str,
    on_click: Arc<dyn Fn(&str) + 'static>,
) -> impl IntoView {
    label(move || text.to_string())
        .on_click_stop(move |_| on_click(action_id))
        .style(move |s| {
            let theme = read_theme(theme_idx);
            s.height(TOOLBAR_HEIGHT)
                .padding_horiz(10.0)
                .padding_vert(4.0)
                .font_size(13.0)
                .color(theme.fg)
                .cursor(CursorStyle::Pointer)
                .items_center()
                .hover(|h| h.background(theme.fg.multiply_alpha(0.06)))
        })
}

/// One toolbar mode cell — clickable text label that is highlighted when active.
fn mode_cell(
    theme_idx: RwSignal<usize>,
    active: impl Fn() -> bool + 'static,
    text: &'static str,
    on_click: impl Fn() + 'static,
) -> impl IntoView {
    let click_cb = Arc::new(on_click);
    label(move || text.to_string())
        .on_click_stop(move |_| click_cb())
        .style(move |s| {
            let theme = read_theme(theme_idx);
            let is_active = active();
            let mut s = s.height(TOOLBAR_HEIGHT)
                .padding_horiz(10.0)
                .padding_vert(4.0)
                .font_size(13.0)
                .cursor(CursorStyle::Pointer)
                .items_center();
            if is_active {
                s = s.background(theme.accent.multiply_alpha(0.2))
                     .color(theme.accent);
            } else {
                s = s.color(theme.fg)
                     .hover(|h| h.background(theme.fg.multiply_alpha(0.06)));
            }
            s
        })
}

/// 1-px vertical divider with a small vertical inset so it doesn't kiss the
/// top / bottom edges of the strip. `empty()` with a background and fixed
/// width is the floem 0.2.0 idiom for "draw a rectangle" — no peniko geometry
/// needed at this scale.
fn divider(theme_idx: RwSignal<usize>) -> impl IntoView {
    empty().style(move |s| {
        let theme = read_theme(theme_idx);
        s.width(DIVIDER_WIDTH)
            .height(TOOLBAR_HEIGHT - 2.0 * DIVIDER_VERT_INSET)
            .margin_horiz(4.0)
            .background(theme.fg.multiply_alpha(0.15))
    })
}

/// Snapshot the active theme. Subscribes the surrounding reactive scope to
/// `theme_idx` so style closures re-run on Ctrl-T. Mirrors the helper in
/// menu_bar.rs ; copy-not-extract is intentional — sibling modules diverge
/// in M1 (toolbar gets icon glyphs, menu bar gets popovers).
fn read_theme(theme_idx: RwSignal<usize>) -> brouillon_core::theme::Theme {
    let idx = theme_idx.get();
    let all = brouillon_core::theme::builtin_themes();
    all[idx % all.len()].clone()
}

/// Unused but kept for symmetry — M1 will surface keyboard shortcuts in a
/// muted color beside / under each cell.
#[allow(dead_code)]
fn dim_fg(theme: &brouillon_core::theme::Theme) -> Color {
    theme.fg_dim
}

// --------------------------------------------------------------------------
// Tests
// --------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn toolbar_layout_resolves_against_registry() {
        use brouillon_core::{command_registry::Registry, contract::CommandSpec};
        let mut reg = Registry::new();
        reg.register(CommandSpec { id: "file_save".into(), ..Default::default() }).unwrap();
        let wanted = vec!["file_save".to_string(), String::new(), "ghost".to_string()];
        let (items, unknown) = toolbar_commands(&reg, &wanted);
        assert_eq!(items.len(), 2);
        assert!(items[0].is_some() && items[1].is_none(), "divider kept as None");
        assert_eq!(unknown, vec!["ghost"]);
    }

    /// The action ids must match the palette dispatcher's expected strings.
    #[test]
    fn toolbar_action_ids_are_canonical() {
        let ids: Vec<&str> = TOOLBAR_ITEMS.iter().map(|(id, _, _)| *id).collect();
        assert_eq!(ids, vec!["edit.undo", "edit.redo"]);
    }

    /// Open/Save/Cut/Copy/Paste/Find are menu-only now (owner request).
    #[test]
    fn toolbar_keeps_only_undo_redo() {
        assert_eq!(TOOLBAR_ITEMS.len(), 2);
        for gone in ["file.open", "file.save", "edit.cut", "edit.copy", "edit.paste", "edit.find"] {
            assert!(TOOLBAR_ITEMS.iter().all(|(id, _, _)| *id != gone), "{gone} must not be on the toolbar");
        }
    }

    /// Undo and Redo labels carry their arrow glyphs (BMP U+21B6 / U+21B7).
    #[test]
    fn undo_redo_labels_have_arrow_glyphs() {
        let undo = TOOLBAR_ITEMS[0].1;
        let redo = TOOLBAR_ITEMS[1].1;
        assert!(undo.contains('\u{21B6}'), "undo label missing arrow: {undo}");
        assert!(redo.contains('\u{21B7}'), "redo label missing arrow: {redo}");
    }

    #[test]
    fn all_labels_non_empty() {
        for (_, lbl, _) in TOOLBAR_ITEMS {
            assert!(!lbl.is_empty(), "Toolbar label must not be empty");
        }
    }

    #[test]
    fn no_duplicate_action_ids() {
        let mut ids: Vec<&str> = TOOLBAR_ITEMS.iter().map(|(id, _, _)| *id).collect();
        ids.sort();
        ids.dedup();
        assert_eq!(ids.len(), TOOLBAR_ITEMS.len(), "Duplicate action id detected");
    }

    #[test]
    fn toolbar_height_is_positive() {
        assert!(TOOLBAR_HEIGHT > 0.0);
        assert!(DIVIDER_WIDTH > 0.0);
    }

    #[test]
    fn divider_inset_less_than_half_height() {
        assert!(DIVIDER_VERT_INSET < TOOLBAR_HEIGHT / 2.0,
            "Divider inset would collapse the divider to zero height");
    }
}
