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

/// How many bytes of the file the picker decodes for its preview.
pub const PREVIEW_BYTES: usize = 600;

/// Decoded start of `bytes` with codepage `id` (what "Reopen with encoding" would show), at most
/// `max_chars` characters, line breaks shown as a pilcrow so the preview stays a few rows high.
/// `None` when the encoding is unsupported.
pub fn preview_text(bytes: &[u8], id: &str, max_chars: usize) -> Option<String> {
    let head = &bytes[..bytes.len().min(PREVIEW_BYTES)];
    let doc = brouillon_editor_engine::file_io::decode_with(head, id).ok()?;
    Some(doc.text.chars().take(max_chars).map(|c| if c == '\n' { '\u{b6}' } else if c == '\r' { ' ' } else { c }).collect())
}

/// What the picker does with the choice: set the codepage of the next save, or re-decode the file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PickerMode {
    SaveWith,
    Reopen,
}

impl PickerMode {
    pub fn title(self) -> &'static str {
        match self {
            PickerMode::SaveWith => "Save with encoding (the text on screen is not changed)",
            PickerMode::Reopen => "Reopen with encoding (re-reads the file from disk)",
        }
    }
}

/// The codepage Enter would choose for `query`: the first supported match.
pub fn first_supported(query: &str) -> Option<&'static str> {
    picker_items(query).into_iter().find(|i| i.supported).map(|i| i.id)
}

/// List view over `query`; `on_pick` receives the chosen codepage id, `on_hover` the one under the pointer.
pub fn encoding_picker_view(
    query: RwSignal<String>,
    theme_idx: RwSignal<usize>,
    on_pick: impl Fn(&'static str) + 'static + Clone,
    on_hover: impl Fn(&'static str) + 'static + Clone,
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
            let hv = on_hover.clone();
            let t = get_theme.clone();
            let (id, supported, text) = (item.id, item.supported, row_label(&item));
            label(move || text.clone())
                .on_click_stop(move |_| cb(id))
                .on_event_cont(floem::event::EventListener::PointerEnter, move |_| hv(id))
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
    fn preview_decodes_with_the_candidate_codepage() {
        let bytes = [b'c', b'a', b'f', 0xE9, b'\n', b'x'];
        assert_eq!(preview_text(&bytes, "windows-1252", 20).as_deref(), Some("caf\u{e9}\u{b6}x"));
        assert_eq!(preview_text(&bytes, "windows-1251", 20).as_deref(), Some("caf\u{439}\u{b6}x"));
        assert_eq!(preview_text(&bytes, "windows-1252", 3).as_deref(), Some("caf"));
        assert_eq!(preview_text(&bytes, "ibm500", 20), None);
    }
    #[test]
    fn first_supported_skips_unsupported() {
        assert_eq!(first_supported("windows-1252"), Some("windows-1252"));
        assert_eq!(first_supported("ebcdic"), None);
    }

    #[test]
    fn filter_by_script_group_and_id() {
        assert!(picker_items("cyrillic").iter().any(|i| i.id == "koi8-r"));
        assert_eq!(picker_items("windows-1252").len(), 1);
        assert!(picker_items("zzzz").is_empty());
    }
}
