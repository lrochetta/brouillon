//! Encoding picker (BK-008/009): filterable list grouped by script, backed by
//! `brouillon_core::codepages`. Pure model + a floem list view.

use brouillon_core::codepages;
use floem::reactive::{RwSignal, SignalGet};
use floem::style::CursorStyle;
use floem::views::{dyn_stack, label, scroll, Decorators};
use floem::IntoView;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PickerItem {
    pub id: &'static str,
    pub label: String,
    pub group: &'static str,
    pub supported: bool,
}

/// Items matching `query` (empty = all), in table order. Unsupported
/// encodings are listed (flagged) so the user sees why they cannot be used.
pub fn picker_items(query: &str) -> Vec<PickerItem> {
    codepages::search(query)
        .into_iter()
        .map(|c| PickerItem { id: c.id, label: c.label.to_string(), group: c.group, supported: c.is_supported() })
        .collect()
}

/// Row text: label, with a marker for unsupported entries.
pub fn row_label(item: &PickerItem) -> String {
    if item.supported {
        item.label.clone()
    } else {
        format!("{} (unsupported)", item.label)
    }
}

/// List view over `query`; `on_pick` receives the chosen codepage id.
pub fn encoding_picker_view(
    query: RwSignal<String>,
    theme_idx: RwSignal<usize>,
    on_pick: impl Fn(&'static str) + 'static + Clone,
) -> impl IntoView {
    let get_theme = move || {
        let idx = theme_idx.get();
        let all = brouillon_core::theme::builtin_themes();
        all[idx % all.len()].clone()
    };
    let bg_theme = get_theme.clone();
    let list = dyn_stack(
        move || picker_items(&query.get()),
        |i: &PickerItem| i.id,
        move |item: PickerItem| {
            let cb = on_pick.clone();
            let t = get_theme.clone();
            let (id, supported, text) = (item.id, item.supported, row_label(&item));
            label(move || text.clone())
                .on_click_stop(move |_| cb(id))
                .style(move |s| {
                    let t = t();
                    s.color(if supported { t.fg } else { t.fg_dim })
                        .font_size(12.0)
                        .padding_horiz(10.0)
                        .padding_vert(3.0)
                        .width_full()
                        .cursor(CursorStyle::Pointer)
                        .hover(move |h| h.background(t.fg.multiply_alpha(0.06)))
                })
        },
    )
    .style(|s| s.flex_col().width_full());
    scroll(list).style(move |s| {
        let t = bg_theme();
        s.width(280.0).max_height(360.0).background(t.bg_chrome).border(1.0).border_color(t.border)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lists_everything_and_flags_unsupported() {
        let all = picker_items("");
        assert!(all.iter().filter(|i| i.supported).count() >= 40);
        let ebcdic = picker_items("ebcdic");
        assert!(!ebcdic.is_empty() && ebcdic.iter().all(|i| !i.supported));
        assert!(row_label(&ebcdic[0]).ends_with("(unsupported)"));
    }

    #[test]
    fn filter_by_script_group_and_id() {
        assert!(picker_items("cyrillic").iter().any(|i| i.id == "koi8-r"));
        assert_eq!(picker_items("windows-1252").len(), 1);
        assert!(picker_items("zzzz").is_empty());
    }
}
