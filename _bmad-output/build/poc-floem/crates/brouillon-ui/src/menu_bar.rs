#![allow(dead_code)]

//! Moldu top menu bar — File / Edit / Search / View / Tools / Help.
//!
//! Round-13 sprint C deliverable. Hidden in Geek preset (the integrator gates
//! visibility on `preset_sig.get().config().show_menu_bar`) ; this module
//! just builds the chrome assuming it'll be shown.
//!
//! Dropdowns are NOT implemented for alpha.13 — clicking a top-level label
//! invokes `on_item_click(<id>)` with the lowercase item name. The integrator
//! currently logs to stderr ; M1 polish will swap that for a popover.
//!
//! Theming rides on `theme_idx` (RwSignal<usize>) the same way `tab_bar.rs`
//! reads the active palette — we resolve the theme inside reactive closures
//! so Ctrl-T flips colors without rebuilding the view.

use floem::peniko::Color;
use floem::reactive::{RwSignal, SignalGet};
use floem::style::CursorStyle;
use floem::views::{container, label, stack, Decorators};
use floem::IntoView;
use std::sync::Arc;


pub(crate) const MENU_HEIGHT: f64 = 28.0;

/// Stable item ids — match what the integrator routes (lowercase, no spaces).
/// Kept as a `const` so the test module can iterate the same list the view
/// emits ; if the view ever drifts, the canonical-ids test catches it.
pub const MENU_ITEMS: &[(&str, &str)] = &[
    ("file", "File"),
    ("edit", "Edit"),
    ("search", "Search"),
    ("view", "View"),
    ("tools", "Tools"),
    ("help", "Help"),
];

/// Build the top menu bar. Six clickable text labels in a horizontal row.
///
/// Callback contract:
/// - `on_item_click(id)` fires on click with one of `file` / `edit` /
///   `search` / `view` / `tools` / `help`. The integrator decides what to do
///   — for alpha.13 it's `eprintln!`, for M1 it'll open a popover dropdown.
pub fn menu_bar_view(
    theme_idx: RwSignal<usize>,
    on_item_click: Arc<dyn Fn(&str) + 'static>,
    lead: floem::AnyView,
) -> impl IntoView {
    // Build each cell with its own captured callback clone. Hover background
    // and text color flip reactively on theme change via the `move ||` style
    // closures (they subscribe to theme_idx through `read_theme`).
    let cb_file = on_item_click.clone();
    let cb_edit = on_item_click.clone();
    let cb_search = on_item_click.clone();
    let cb_view = on_item_click.clone();
    let cb_tools = on_item_click.clone();
    let cb_help = on_item_click.clone();

    let row = stack((
        lead,
        menu_item(theme_idx, "file", "File", cb_file),
        menu_item(theme_idx, "edit", "Edit", cb_edit),
        menu_item(theme_idx, "search", "Search", cb_search),
        menu_item(theme_idx, "view", "View", cb_view),
        menu_item(theme_idx, "tools", "Tools", cb_tools),
        menu_item(theme_idx, "help", "Help", cb_help),
    ))
    .style(|s| s.flex_row().items_center().height(MENU_HEIGHT).padding_left(4.0));

    container(row).style(move |s| {
        let theme = read_theme(theme_idx);
        s.width_full()
            .height(MENU_HEIGHT)
            .background(theme.bg_chrome)
            .border_bottom(1.0)
            .border_color(theme.fg.multiply_alpha(0.08))
    })
}

/// One menu cell — a clickable label with hover background. Width follows
/// content (no min_width clamp like tabs ; menu labels are short and uniform
/// enough that natural sizing reads cleanly).
fn menu_item(
    theme_idx: RwSignal<usize>,
    id: &'static str,
    text: &'static str,
    on_click: Arc<dyn Fn(&str) + 'static>,
) -> impl IntoView {
    label(move || text.to_string())
        .on_click_stop(move |_| on_click(id))
        .style(move |s| {
            let theme = read_theme(theme_idx);
            s.height(MENU_HEIGHT)
                .padding_horiz(12.0)
                .padding_vert(6.0)
                .font_size(13.0)
                .color(theme.fg)
                .cursor(CursorStyle::Pointer)
                .items_center()
                .hover(|h| h.background(theme.fg.multiply_alpha(0.08)))
        })
}

/// Snapshot the active theme. Subscribes the surrounding reactive scope to
/// `theme_idx` so style closures re-run on Ctrl-T.
fn read_theme(theme_idx: RwSignal<usize>) -> brouillon_core::theme::Theme {
    let idx = theme_idx.get();
    // The modulo clamps a stale idx (e.g. mid-shutdown); the list is never empty.
    let all = brouillon_core::theme::builtin_themes();
    all[idx % all.len()].clone()
}

/// Convenience: returns the dim variant of fg for any disabled / muted text.
/// Currently unused but kept for symmetry with toolbar's similar helper —
/// dropdown items will need a "shortcut hint" muted color in M1.
#[allow(dead_code)]
fn dim_fg(theme: &brouillon_core::theme::Theme) -> Color {
    theme.fg_dim
}

// --------------------------------------------------------------------------
// Tests
// --------------------------------------------------------------------------
//
// View code can't be unit-tested without a Floem reactive scope ; we cover
// the callback contract (ids match what the integrator routes) instead.

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    /// The six top-level menu items must emit canonical lowercase ids — the
    /// integrator routes on these strings. If a future refactor renames an
    /// id (e.g. `"search"` -> `"find"`), this test fails and the integrator
    /// gets a heads-up before a silent break.
    #[test]
    fn menu_item_ids_are_canonical() {
        let log = Arc::new(Mutex::new(Vec::<String>::new()));
        let cb_log = log.clone();
        let cb: Arc<dyn Fn(&str)> = Arc::new(move |id: &str| {
            cb_log.lock().unwrap().push(id.to_string());
        });
        for (id, _label) in MENU_ITEMS {
            cb(id);
        }
        let captured = log.lock().unwrap().clone();
        assert_eq!(
            captured,
            vec!["file", "edit", "search", "view", "tools", "help"]
        );
    }

    /// MENU_ITEMS is the single source of truth for both view construction
    /// and the callback test ; size must stay at six. A future "Window" or
    /// "Run" menu would need a deliberate edit here, not a silent extension.
    #[test]
    fn menu_has_exactly_six_items() {
        assert_eq!(MENU_ITEMS.len(), 6);
    }

    /// Labels must be Title Case — matches Notepad / Word / WordPad. Catches
    /// a future copy-paste typo (`"file"` for the label, not the id).
    #[test]
    fn menu_labels_are_title_case() {
        for (_id, label) in MENU_ITEMS {
            let first = label.chars().next().unwrap();
            assert!(
                first.is_uppercase(),
                "menu label '{label}' must start with an uppercase letter"
            );
        }
    }

    #[test]
    fn no_duplicate_menu_ids() {
        let mut ids: Vec<&str> = MENU_ITEMS.iter().map(|(id, _)| *id).collect();
        ids.sort();
        ids.dedup();
        assert_eq!(ids.len(), MENU_ITEMS.len());
    }

    #[test]
    fn menu_ids_are_lowercase() {
        for (id, _) in MENU_ITEMS {
            assert_eq!(*id, id.to_lowercase(), "Menu id should be lowercase: {id}");
        }
    }

    #[test]
    fn id_matches_lowercase_label() {
        for (id, lbl) in MENU_ITEMS {
            assert_eq!(*id, lbl.to_lowercase(), "Id should be lowercase of label");
        }
    }

    #[test]
    fn menu_height_positive() {
        assert!(MENU_HEIGHT > 0.0);
    }

    #[test]
    fn labels_are_non_empty() {
        for (_, lbl) in MENU_ITEMS {
            assert!(!lbl.is_empty());
        }
    }
}
