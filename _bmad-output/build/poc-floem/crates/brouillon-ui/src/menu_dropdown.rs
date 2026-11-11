//! Moldu top menu bar dropdown popovers (Round 14).
//! Shows Tokyo-Night styled absolute overlays floating under the active menu items.

use std::sync::Arc;
use floem::reactive::{RwSignal, SignalGet, SignalUpdate};
use floem::peniko::Color;
use brouillon_core::theme::Theme;
use floem::views::{container, dyn_container, empty, label, stack, Decorators};
use floem::IntoView;
use floem::style::CursorStyle;
use floem::event::{Event, EventListener};
use floem::keyboard::{Key, NamedKey};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DropdownItem {
    pub label: &'static str,
    pub shortcut: &'static str,
    pub action_id: &'static str,
}

pub fn get_dropdown_items(menu_id: &str) -> Vec<DropdownItem> {
    match menu_id {
        "file" => vec![
            DropdownItem { label: "New Tab", shortcut: "Ctrl-N", action_id: "file.new-tab" },
            DropdownItem { label: "Open...", shortcut: "Ctrl-O", action_id: "file.open" },
            DropdownItem { label: "Save", shortcut: "Ctrl-S", action_id: "file.save" },
            DropdownItem { label: "Save As...", shortcut: "Ctrl-Shift-S", action_id: "file.save-as" },
            DropdownItem { label: "Print...", shortcut: "", action_id: "file_print" },
            DropdownItem { label: "Open Folder...", shortcut: "", action_id: "project_open_folder" },
            DropdownItem { label: "Open Project...", shortcut: "", action_id: "project_load" },
            DropdownItem { label: "Save Project", shortcut: "", action_id: "project_save" },
            DropdownItem { label: "Preferences...", shortcut: "Ctrl-,", action_id: "preferences_open_settings" },
            DropdownItem { label: "Keyboard Shortcuts...", shortcut: "", action_id: "preferences_open_keymap" },
            DropdownItem { label: "Quit", shortcut: "", action_id: "app.quit" },
        ],
        "edit" => vec![
            DropdownItem { label: "Undo", shortcut: "Ctrl-Z", action_id: "edit.undo" },
            DropdownItem { label: "Redo", shortcut: "Ctrl-Y", action_id: "edit.redo" },
            DropdownItem { label: "Cut", shortcut: "Ctrl-X", action_id: "edit.cut" },
            DropdownItem { label: "Copy", shortcut: "Ctrl-C", action_id: "edit.copy" },
            DropdownItem { label: "Paste", shortcut: "Ctrl-V", action_id: "edit.paste" },
            DropdownItem { label: "Expand Abbreviation (Emmet)", shortcut: "Ctrl-E", action_id: "emmet_expand" },
        ],
        "search" => vec![
            DropdownItem { label: "Find...", shortcut: "Ctrl-F", action_id: "edit.find" },
            DropdownItem { label: "Replace...", shortcut: "Ctrl-H", action_id: "edit.replace" },
            DropdownItem { label: "Go To Line...", shortcut: "Ctrl-G", action_id: "edit.go-to-line" },
            DropdownItem { label: "List Bookmarks", shortcut: "", action_id: "markers_list" },
        ],
        "view" => vec![
            DropdownItem { label: "Moldu Theme", shortcut: "", action_id: "preset.moldu" },
            DropdownItem { label: "Geek Theme", shortcut: "", action_id: "preset.geek" },
            DropdownItem { label: "Theme: Dark", shortcut: "", action_id: "theme.dark" },
            DropdownItem { label: "Theme: Light", shortcut: "", action_id: "theme.light" },
            DropdownItem { label: "Theme: Follow System", shortcut: "", action_id: "theme.system" },
            DropdownItem { label: "Cycle Theme", shortcut: "Ctrl-T", action_id: "view.theme.cycle" },
            DropdownItem { label: "Toggle Markdown Preview", shortcut: "Ctrl-Shift-V", action_id: "view.toggle-preview" },
            DropdownItem { label: "Toggle Folder Tree", shortcut: "Ctrl-Shift-E", action_id: "view_toggle_tree" },
            DropdownItem { label: "Toggle Sidebar", shortcut: "Ctrl-B", action_id: "view_toggle_sidebar" },
            DropdownItem { label: "Tabs at Top / Bottom", shortcut: "", action_id: "view_tabs_position_toggle" },
            DropdownItem { label: "Last Visited Tab", shortcut: "", action_id: "tab_last_visited" },
            DropdownItem { label: "Toggle Minimap", shortcut: "", action_id: "view_toggle_minimap" },
            DropdownItem { label: "Compare with File on Disk", shortcut: "", action_id: "diff_with_disk" },
            DropdownItem { label: "Compare with Last Visited Tab", shortcut: "", action_id: "diff_with_last_tab" },
            DropdownItem { label: "Toggle Indent Guides", shortcut: "", action_id: "view_toggle_indent_guides" },
            DropdownItem { label: "Toggle Column Ruler", shortcut: "", action_id: "view_toggle_ruler" },
            DropdownItem { label: "Distraction-Free Mode", shortcut: "Ctrl-Alt-Z", action_id: "view_distraction_free" },
        ],
        "tools" => vec![
            DropdownItem { label: "Toggle Crayon Mode", shortcut: "", action_id: "view.toggle-crayon" },
            DropdownItem { label: "Toggle Infinite Layout", shortcut: "", action_id: "tools.infinite" },
            DropdownItem { label: "Cliptext Library", shortcut: "", action_id: "cliptext_open_panel" },
            DropdownItem { label: "Record / Stop Macro", shortcut: "", action_id: "macro_record_toggle" },
            DropdownItem { label: "Play Macro", shortcut: "Ctrl-Shift-Q", action_id: "macro_play" },
            DropdownItem { label: "Import Settings from Another Editor...", shortcut: "", action_id: "tools_import_settings" },
        ],
        "help" => vec![
            DropdownItem { label: "Welcome", shortcut: "", action_id: "help_welcome" },
            DropdownItem { label: "Documentation", shortcut: "", action_id: "help_docs" },
            DropdownItem { label: "Release Notes", shortcut: "", action_id: "help_release_notes" },
            DropdownItem { label: "About Brouillon", shortcut: "", action_id: "help.about" },
            DropdownItem { label: "Show Shortcuts", shortcut: "", action_id: "help.shortcuts" },
            DropdownItem { label: "First-Run Setup...", shortcut: "", action_id: "help_first_run" },
            DropdownItem { label: "Try an Agent...", shortcut: "", action_id: "help_try_agent" },
        ],
        _ => Vec::new(),
    }
}

/// A menu row built from the command registry (BK-070).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MenuEntry {
    /// Caption without the "Category: " prefix.
    pub label: String,
    /// Display accelerator, empty when unbound.
    pub accel: String,
    /// Registered command id the row dispatches.
    pub command: String,
    /// Underlined mnemonic letter (first letter not used by an earlier row).
    pub mnemonic: Option<char>,
    /// `Some` for View toggles ("Toggle ..." captions); state from the callback.
    pub checked: Option<bool>,
}

/// Rows of `menu_id` ("file", "edit", ...): registry commands whose category
/// matches, in registration order. Geek hides menus (existing rule).
pub fn menu_entries(
    reg: &brouillon_core::command_registry::Registry,
    bindings: &[brouillon_core::keymap::Binding],
    menu_id: &str,
    preset: brouillon_core::preset::Preset,
    is_checked: &dyn Fn(&str) -> bool,
) -> Vec<MenuEntry> {
    use brouillon_core::keymap::{chord_display, chords_for};
    if !preset.config().show_menu_bar {
        return Vec::new();
    }
    let mut used: Vec<char> = Vec::new();
    reg.in_category_ci(menu_id)
        .map(|s| {
            let label = s.caption.split_once(": ").map_or(s.caption.as_str(), |(_, a)| a).to_string();
            let mnemonic = label.chars().find(|c| c.is_alphanumeric() && !used.contains(&c.to_ascii_lowercase()));
            if let Some(m) = mnemonic {
                used.push(m.to_ascii_lowercase());
            }
            MenuEntry {
                accel: chords_for(bindings, &s.id).first().map(|c| chord_display(c)).unwrap_or_default(),
                checked: label.starts_with("Toggle").then(|| is_checked(&s.id)),
                command: s.id.clone(),
                mnemonic,
                label,
            }
        })
        .collect()
}

/// WCAG 2.x contrast ratio between two opaque colours.
pub fn contrast_ratio(a: Color, b: Color) -> f64 {
    fn lin(c: u8) -> f64 {
        let c = c as f64 / 255.0;
        if c <= 0.03928 { c / 12.92 } else { ((c + 0.055) / 1.055).powf(2.4) }
    }
    let lum = |c: Color| 0.2126 * lin(c.r) + 0.7152 * lin(c.g) + 0.0722 * lin(c.b);
    let (la, lb) = (lum(a), lum(b));
    let (hi, lo) = if la > lb { (la, lb) } else { (lb, la) };
    (hi + 0.05) / (lo + 0.05)
}

/// `a` moved `t` (0..1) of the way towards `b`, opaque.
fn mix(a: Color, b: Color, t: f64) -> Color {
    let m = |x: u8, y: u8| (x as f64 + (y as f64 - x as f64) * t).round().clamp(0.0, 255.0) as u8;
    Color::rgb8(m(a.r, b.r), m(a.g, b.g), m(a.b, b.b))
}

/// Colours of a popup menu, readable in every theme.
#[derive(Debug, Clone, Copy)]
pub struct PopupPalette {
    pub bg: Color,
    /// First mix of text colour over the chrome reaching 3:1 (WCAG non-text contrast)
    /// against BOTH surfaces the popup can overlap (editor body and toolbar/chrome).
    pub border: Color,
    /// Item text: the theme text colour, pushed towards black/white until 4.5:1 (AA) on `bg`
    /// (Solarized's own text colour only reaches ~3.8:1).
    pub text: Color,
    /// Accelerator hints: the theme's dim text pulled towards `text` until 4.5:1 on `bg`.
    pub hint: Color,
}

pub fn popup_palette(t: &Theme) -> PopupPalette {
    let bg = mix(t.bg_chrome, t.fg, 0.04);
    let mut border = t.fg;
    for step in 6..=20 {
        let cand = mix(t.bg_chrome, t.fg, step as f64 * 0.05);
        if contrast_ratio(cand, t.bg) >= 3.0 && contrast_ratio(cand, t.bg_chrome) >= 3.0 {
            border = cand;
            break;
        }
    }
    let extreme = if t.is_dark { Color::rgb8(255, 255, 255) } else { Color::rgb8(0, 0, 0) };
    let mut text = t.fg;
    for step in 1..=20 {
        if contrast_ratio(text, bg) >= 4.5 {
            break;
        }
        text = mix(t.fg, extreme, step as f64 * 0.05);
    }
    let mut hint = t.fg_dim;
    for step in 1..=20 {
        if contrast_ratio(hint, bg) >= 4.5 {
            break;
        }
        hint = mix(t.fg_dim, text, step as f64 * 0.05);
    }
    PopupPalette { bg, border, text, hint }
}

/// Accelerator text of a row: `accel(action_id, built-in hint)`. The shell answers from the live keymap so a rebinding shows in the menus.
pub type AccelFn = Arc<dyn Fn(&str, &'static str) -> String + 'static>;

/// Display form of a registry chord in the menus: "ctrl+shift+s" -> "Ctrl-Shift-S".
pub fn menu_accel_text(chord: &str) -> String {
    brouillon_core::keymap::chord_display(chord).replace('+', "-")
}

/// The accelerator a menu row shows: the first chord now bound to `command` (user keymap and Classic overlay
/// included), else the built-in `hint` (rows whose key the shell catches itself, like Go To Line).
pub fn live_accel(bindings: &[brouillon_core::keymap::Binding], command: &str, hint: &str) -> String {
    brouillon_core::keymap::chords_for(bindings, command).first().map(|c| menu_accel_text(c)).unwrap_or_else(|| hint.to_string())
}

/// Menu open after a click on a title: the same title toggles it closed, another title switches to it.
pub fn toggle_menu(current: Option<&'static str>, clicked: Option<&'static str>) -> Option<&'static str> {
    if current == clicked { None } else { clicked }
}

pub fn menu_dropdown_overlay(
    active_menu: RwSignal<Option<&'static str>>,
    theme_idx: RwSignal<usize>,
    on_dispatch: Arc<dyn Fn(&str) + 'static>,
) -> impl IntoView {
    menu_dropdown_overlay_with(active_menu, theme_idx, on_dispatch, Arc::new(|_, hint| hint.to_string()))
}

pub fn menu_dropdown_overlay_with(
    active_menu: RwSignal<Option<&'static str>>,
    theme_idx: RwSignal<usize>,
    on_dispatch: Arc<dyn Fn(&str) + 'static>,
    accel: AccelFn,
) -> impl IntoView {
    let get_theme = move || {
        let idx = theme_idx.get();
        let all = brouillon_core::theme::builtin_themes();
        all[idx % all.len()].clone()
    };

    dyn_container(
        move || active_menu.get(),
        move |active_opt| {
            if let Some(menu_id) = active_opt {
                let items = get_dropdown_items(menu_id);
                let dispatch = on_dispatch.clone();
                let accel = accel.clone();
                let active = active_menu;
                
                // Horizontal offset based on active menu column index
                let left_offset = match menu_id {
                    "file" => 8.0,
                    "edit" => 61.0,
                    "search" => 114.0,
                    "view" => 181.0,
                    "tools" => 234.0,
                    "help" => 293.0,
                    _ => 8.0,
                } + crate::view_icons::LEAD_WIDTH;

                let t = get_theme();
                let pal = popup_palette(&t);

                let row_views = floem::views::dyn_stack(
                    move || items.clone(),
                    move |item: &DropdownItem| item.action_id,
                    move |item: DropdownItem| {
                        let disp = dispatch.clone();
                        let a_id = item.action_id;
                        let item_theme = get_theme();

                        let label_lbl = label(move || item.label.to_string())
                            .style(move |s| s.color(pal.text).flex_grow(1.0).font_size(12.0));
                        
                        let (hint, a_for_label) = (item.shortcut, accel.clone());
                        let shortcut_lbl = label(move || a_for_label(a_id, hint))
                            .style(move |s| s.color(pal.hint).font_size(11.0).padding_left(20.0));

                        stack((label_lbl, shortcut_lbl))
                            .on_click_stop(move |_| {
                                active.set(None);
                                (disp)(a_id);
                            })
                            .style(move |s| {
                                s.flex_row()
                                    .items_center()
                                    .height(24.0)
                                    .padding_horiz(10.0)
                                    .cursor(CursorStyle::Pointer)
                                    .hover(move |h| h.background(item_theme.fg.multiply_alpha(0.08)))
                            })
                    }
                ).style(|s| s.flex_col().width_full());

                let popup_width = if menu_id == "tools" { 340.0 } else { 260.0 };
                let popup = container(row_views)
                    .on_event_stop(EventListener::KeyDown, move |event| {
                        if let Event::KeyDown(ke) = event {
                            if ke.key.logical_key == Key::Named(NamedKey::Escape) {
                                active.set(None);
                            }
                        }
                    })
                    .style(move |s| {
                        s.absolute()
                            .inset_top(MENU_POPUP_TOP - HOST_TOP) // floating directly under the Moldu top menu (host starts at HOST_TOP)
                            .inset_left(left_offset)
                            .width(popup_width) // widest rows: "Toggle Markdown Preview" + "Ctrl-Shift-V", Tools: "Import Settings from Another Editor..."
                            .background(pal.bg)
                            .border(1.0)
                            .border_color(pal.border)
                            .border_radius(4.0)
                            .padding_vert(4.0)
                            .box_shadow_color(Color::rgba8(0, 0, 0, 140))
                            .box_shadow_blur(14.0)
                            .box_shadow_v_offset(4.0)
                            .z_index(9999) // Stacked above standard panels
                    });
                // Click-away layer under the popup, below the menu bar (so another menu
                // title still works): a press anywhere else closes the menu.
                let backdrop = empty()
                    .on_event_stop(EventListener::PointerDown, move |_| active.set(None))
                    .style(|s| {
                        s.absolute()
                            .inset_top(0.0)
                            .inset_left(0.0)
                            .inset_right(0.0)
                            .inset_bottom(0.0)
                    });
                // size_full: the absolute backdrop is positioned inside this stack, which must span the host.
                stack((backdrop, popup)).style(|s| s.size_full()).into_any()
            } else {
                empty().into_any()
            }
        }
    )
    // Anchor the host to the window origin (like the palette overlay): as a
    // plain flex child it sat after the status bar, so the popup's `inset_top`
    // was measured from there and landed off-screen.
    .style(move |s| {
        // The host starts below the menu bar: a host spanning the whole window would sit above the
        // menu titles and swallow their clicks (another menu could only be opened after closing this one).
        let s = s.absolute().inset_top(HOST_TOP).inset_left(0.0);
        // While a menu is open the host spans the window below the menu bar so the click-away layer can catch presses.
        if active_menu.get().is_some() { s.inset_right(0.0).inset_bottom(0.0) } else { s }
    })
}

/// Y of the popup, measured under the menu bar (header ~19 + menu 28 = 47).
const MENU_POPUP_TOP: f64 = 52.0;
/// Top of the overlay host and of the click-away layer: exactly the bottom edge of the menu titles
/// (header 24 + menu bar). Any higher and the click-away layer covers the lower strip of every title,
/// so a click there while a menu is open only closes it and the next click is needed to open the other one.
const HOST_TOP: f64 = MENU_POPUP_TOP;
/// Height of the header row above the menu bar (`header_bar` in main.rs).
#[cfg(test)]
const HEADER_HEIGHT: f64 = 24.0;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn one_click_on_a_title_opens_or_switches_and_the_same_title_closes() {
        assert_eq!(toggle_menu(None, Some("view")), Some("view"));
        assert_eq!(toggle_menu(Some("file"), Some("view")), Some("view"), "switching needs ONE click");
        assert_eq!(toggle_menu(Some("view"), Some("view")), None);
        assert_eq!(toggle_menu(Some("view"), None), None);
    }

    /// The click-away layer must not cover any part of the menu titles (one click must switch menus).
    #[test]
    fn click_away_layer_starts_below_the_menu_titles() {
        assert!(HOST_TOP >= HEADER_HEIGHT + crate::menu_bar::MENU_HEIGHT, "host top {HOST_TOP} overlaps the menu titles");
        assert!(MENU_POPUP_TOP >= HOST_TOP, "popup must sit at or below the host top");
    }

    #[test]
    fn registry_menu_rows_resolve_and_show_accelerators_and_toggles() {
        use brouillon_core::{command_registry::Registry, contract::CommandSpec, keymap::Binding, preset::Preset};
        let spec = |id: &str, cap: &str, cat: &str| CommandSpec {
            id: id.into(),
            caption: cap.into(),
            category: cat.into(),
            ..Default::default()
        };
        let mut reg = Registry::new();
        reg.register_all(vec![
            spec("file_save", "File: Save", "File"),
            spec("file_save_as", "File: Save As...", "File"),
            spec("view_toggle_minimap", "View: Toggle Minimap", "View"),
        ]);
        let b = vec![Binding::new("ctrl+s", "file_save", None).unwrap()];
        let rows = menu_entries(&reg, &b, "file", Preset::Moldu, &|_| false);
        assert_eq!(rows.len(), 2);
        assert!(rows.iter().all(|r| reg.contains(&r.command)));
        assert_eq!((rows[0].label.as_str(), rows[0].accel.as_str()), ("Save", "Ctrl+S"));
        assert_eq!(rows[0].mnemonic, Some('S'));
        assert_eq!(rows[1].mnemonic, Some('a'), "mnemonics are unique within a menu");
        assert_eq!(rows[1].accel, "");
        let view = menu_entries(&reg, &b, "view", Preset::Moldu, &|id| id == "view_toggle_minimap");
        assert_eq!(view[0].checked, Some(true));
        assert!(menu_entries(&reg, &b, "file", Preset::Geek, &|_| false).is_empty());
    }
    #[test]
    fn live_accelerators_follow_the_keymap_and_fall_back_to_the_built_in_hint() {
        use brouillon_core::keymap::Binding;
        let b = vec![Binding::new("ctrl+shift+s", "file_save_as", None).unwrap(), Binding::new("ctrl+j", "duplicate_line", None).unwrap()];
        assert_eq!(live_accel(&b, "file_save_as", "Ctrl-Shift-S"), "Ctrl-Shift-S");
        assert_eq!(live_accel(&b, "duplicate_line", ""), "Ctrl-J");
        let rebound = vec![Binding::new("ctrl+alt+s", "file_save_as", None).unwrap()];
        assert_eq!(live_accel(&rebound, "file_save_as", "Ctrl-Shift-S"), "Ctrl-Alt-S", "a rebinding shows in the menu");
        assert_eq!(live_accel(&[], "go_to", "Ctrl-G"), "Ctrl-G", "the hint stays for keys the shell catches itself");
    }

    #[test]
    fn popup_is_readable_and_outlined_in_every_theme() {
        let reg = brouillon_core::theme::ThemeRegistry::new();
        assert!(reg.themes.len() >= 2);
        for t in &reg.themes {
            let PopupPalette { bg, border, text, hint } = popup_palette(t);
            let text = contrast_ratio(text, bg);
            assert!(text >= 4.5, "{}: text on popup {text:.2}:1 < 4.5:1 (AA)", t.name);
            let hint_ratio = contrast_ratio(hint, bg);
            assert!(hint_ratio >= 4.5, "{}: accelerator hint on popup {hint_ratio:.2}:1 < 4.5:1 (AA)", t.name);
            for (what, surface) in [("editor", t.bg), ("chrome", t.bg_chrome)] {
                let edge = contrast_ratio(border, surface);
                assert!(edge >= 3.0, "{}: popup border on {what} {edge:.2}:1 < 3:1", t.name);
            }
        }
    }

    #[test]
    fn contrast_ratio_matches_wcag_reference_values() {
        let (black, white) = (Color::rgb8(0, 0, 0), Color::rgb8(255, 255, 255));
        assert!((contrast_ratio(black, white) - 21.0).abs() < 1e-9);
        assert!((contrast_ratio(white, white) - 1.0).abs() < 1e-9);
    }

    use floem::reactive::{with_scope, Scope, create_rw_signal};

    fn run<F: FnOnce() + 'static>(f: F) {
        let scope = Scope::new();
        with_scope(scope, f);
        scope.dispose();
    }

    #[test]
    fn get_dropdown_items_file() {
        let items = get_dropdown_items("file");
        assert!(!items.is_empty());
        assert_eq!(items[0].label, "New Tab");
    }

    #[test]
    fn get_dropdown_items_edit() {
        let items = get_dropdown_items("edit");
        assert!(!items.is_empty());
        assert_eq!(items[0].label, "Undo");
    }

    #[test]
    fn get_dropdown_items_search() {
        let items = get_dropdown_items("search");
        assert!(!items.is_empty());
        assert_eq!(items[0].label, "Find...");
    }

    #[test]
    fn get_dropdown_items_view() {
        let items = get_dropdown_items("view");
        assert!(!items.is_empty());
    }

    #[test]
    fn get_dropdown_items_tools_help() {
        let tools = get_dropdown_items("tools");
        let help = get_dropdown_items("help");
        assert!(!tools.is_empty());
        assert!(!help.is_empty());
    }

    #[test]
    fn get_dropdown_items_unknown() {
        let items = get_dropdown_items("unknown");
        assert!(items.is_empty());
    }

    #[test]
    fn menu_dropdown_overlay_none() {
        run(|| {
            let active = create_rw_signal(None);
            let theme = create_rw_signal(0);
            let dispatch: Arc<dyn Fn(&str) + 'static> = Arc::new(|_| {});
            let _view = menu_dropdown_overlay(active, theme, dispatch);
        });
    }

    #[test]
    fn menu_dropdown_overlay_some() {
        run(|| {
            let active = create_rw_signal(Some("file"));
            let theme = create_rw_signal(0);
            let dispatch: Arc<dyn Fn(&str) + 'static> = Arc::new(|_| {});
            let _view = menu_dropdown_overlay(active, theme, dispatch);
        });
    }
}
