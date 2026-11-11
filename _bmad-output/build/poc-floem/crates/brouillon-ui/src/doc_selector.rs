//! Document selector pane — vertical list of open files (Moldu sidebar).
//!
//! Displays all open tabs as a scrollable vertical list in the left sidebar.
//! Each entry shows the file name (or "untitled-N"), a dirty indicator (●),
//! and the active tab is highlighted with the theme accent color.
//!
//! Visible only in Moldu mode (gated on `preset.show_sidebar` in `main.rs`).

use floem::reactive::{RwSignal, SignalGet};
use floem::style::CursorStyle;
use floem::views::{label, scroll, Decorators};
use floem::IntoView;

/// A snapshot entry for the document selector view.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DocSelectorEntry {
    pub title: String,
    pub is_dirty: bool,
    pub is_active: bool,
    pub index: usize,
}

/// Build a list of `DocSelectorEntry` from a `TabManager` snapshot.
///
/// This function is designed to be called reactively from a signal
/// so the view updates whenever the tab state changes.
pub fn snapshot_entries(
    titles: &[(String, bool)], // (title, is_dirty) for each tab
    active_idx: usize,
) -> Vec<DocSelectorEntry> {
    titles
        .iter()
        .enumerate()
        .map(|(i, (title, dirty))| DocSelectorEntry {
            title: title.clone(),
            is_dirty: *dirty,
            is_active: i == active_idx,
            index: i,
        })
        .collect()
}

/// Build the document selector view.
///
/// - `entries_sig`: reactive signal containing the current list of entries
/// - `theme_idx`: reactive signal for the active theme index
/// - `on_switch`: callback invoked with the tab index when the user clicks an entry
pub fn doc_selector_view(
    entries_sig: RwSignal<Vec<DocSelectorEntry>>,
    theme_idx: RwSignal<usize>,
    on_switch: impl Fn(usize) + 'static + Clone,
) -> impl IntoView {
    let get_theme = move || {
        let idx = theme_idx.get();
        let all = brouillon_core::theme::builtin_themes();
        all[idx % all.len()].clone()
    };

    let header_theme = get_theme.clone();
    let header = label(move || "OPEN FILES".to_string()).style(move |s| {
        let t = header_theme();
        s.color(t.fg_dim)
            .font_size(10.0)
            .padding_horiz(10.0)
            .padding_vert(6.0)
    });

    let items = doc_selector_items(entries_sig, theme_idx, on_switch);

    let t_border = get_theme.clone();

    scroll(
        floem::views::stack((header, items)).style(|s| s.flex_col().width_full()),
    )
    .style(move |s| {
        let t = t_border();
        s.flex_col()
            .width(180.0)
            .height_full()
            .background(t.bg_chrome)
            .border_right(1.0)
            .border_color(t.border)
    })
}

/// The rows alone (no header, no scroll, no fixed width): what the sidebar section shows under its
/// own "OPEN FILES" header.
pub fn doc_selector_items(
    entries_sig: RwSignal<Vec<DocSelectorEntry>>,
    theme_idx: RwSignal<usize>,
    on_switch: impl Fn(usize) + 'static + Clone,
) -> impl IntoView {
    let get_theme = move || {
        let idx = theme_idx.get();
        let reg = brouillon_core::theme::ThemeRegistry::new();
        reg.themes[idx % reg.themes.len()].clone()
    };
    floem::views::dyn_stack(
        move || entries_sig.get(),
        // Title / dirty / active are baked into the row: they must be part of the key.
        move |entry: &DocSelectorEntry| (entry.index, entry.title.clone(), entry.is_dirty, entry.is_active),
        move |entry: DocSelectorEntry| {
            let cb = on_switch.clone();
            let idx = entry.index;
            let is_active = entry.is_active;
            let is_dirty = entry.is_dirty;
            let title = entry.title.clone();
            let item_theme = get_theme.clone();

            let display_text = if is_dirty {
                format!("● {}", title)
            } else {
                format!("  {}", title)
            };

            label(move || display_text.clone())
                .on_click_stop(move |_| {
                    (cb)(idx);
                })
                .style(move |s| {
                    let t = item_theme();
                    let base = s
                        .color(if is_active { t.fg } else { t.fg_dim })
                        .font_size(12.0)
                        .padding_horiz(10.0)
                        .padding_vert(3.0)
                        .width_full()
                        .cursor(CursorStyle::Pointer)
                        .hover(move |h| h.background(t.fg.multiply_alpha(0.06)));

                    if is_active {
                        base.background(t.accent.multiply_alpha(0.12))
                            .border_left(2.0)
                            .border_color(t.accent)
                    } else {
                        base
                    }
                })
        },
    )
    .style(|s| s.flex_col().width_full())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snapshot_entries_basic() {
        let titles = vec![
            ("main.rs".to_string(), false),
            ("lib.rs".to_string(), true),
            ("untitled-1".to_string(), false),
        ];
        let entries = snapshot_entries(&titles, 1);
        assert_eq!(entries.len(), 3);
        assert_eq!(entries[0].title, "main.rs");
        assert!(!entries[0].is_active);
        assert!(!entries[0].is_dirty);
        assert_eq!(entries[1].title, "lib.rs");
        assert!(entries[1].is_active);
        assert!(entries[1].is_dirty);
        assert_eq!(entries[2].index, 2);
    }

    #[test]
    fn snapshot_entries_empty() {
        let entries = snapshot_entries(&[], 0);
        assert!(entries.is_empty());
    }

    #[test]
    fn snapshot_entries_clamps_active_idx() {
        let titles = vec![("test.rs".to_string(), false)];
        // active_idx out of range — no entry should be is_active
        let entries = snapshot_entries(&titles, 5);
        assert!(!entries[0].is_active);
    }

    #[test]
    fn dirty_indicator_format() {
        let entries = snapshot_entries(
            &[("dirty.rs".to_string(), true), ("clean.rs".to_string(), false)],
            0,
        );
        assert!(entries[0].is_dirty);
        assert!(!entries[1].is_dirty);
    }

    #[test]
    fn entry_indices_are_sequential() {
        let titles: Vec<_> = (0..10).map(|i| (format!("file_{}.rs", i), false)).collect();
        let entries = snapshot_entries(&titles, 0);
        for (i, entry) in entries.iter().enumerate() {
            assert_eq!(entry.index, i);
        }
    }

    #[test]
    fn entry_equality() {
        let a = DocSelectorEntry { title: "test.rs".to_string(), is_dirty: false, is_active: true, index: 0 };
        let b = a.clone();
        assert_eq!(a, b);
    }

    #[test]
    fn single_tab_is_active() {
        let entries = snapshot_entries(&[("only.rs".to_string(), false)], 0);
        assert_eq!(entries.len(), 1);
        assert!(entries[0].is_active);
    }

    #[test]
    fn entry_debug_format() {
        let e = DocSelectorEntry { title: "x.rs".to_string(), is_dirty: true, is_active: false, index: 3 };
        let dbg = format!("{:?}", e);
        assert!(dbg.contains("x.rs"));
        assert!(dbg.contains("true")); // is_dirty
    }
}
