//! Shared pieces of the preference dialogs: which dialog is open, the modal
//! frame (backdrop + card + Escape) and the small buttons they all use.
//! Colours come from `active_theme()` only, so dark / light repaint everything.

use floem::event::{Event, EventListener};
use floem::keyboard::{Key, NamedKey};
use floem::reactive::{create_rw_signal, RwSignal, SignalGet, SignalUpdate};
use floem::style::{CursorStyle, Style};
use floem::views::{container, dyn_container, empty, label, stack, Decorators};
use floem::{AnyView, IntoView, View, ViewId};
use std::cell::RefCell;
use std::rc::Rc;

use crate::theme_ctx::active_theme;

/// Visibility of the preference surfaces. `settings` and `shortcuts` are modal
/// (the host must not type into the document while one is open); `cliptext`
/// is a side panel that lets the document keep the keyboard.
#[derive(Clone, Copy)]
pub struct Dialogs {
    pub settings: RwSignal<bool>,
    pub shortcuts: RwSignal<bool>,
    pub cliptext: RwSignal<bool>,
    /// "Import EditPlus settings..." dialog.
    pub import: RwSignal<bool>,
    /// About page.
    pub about: RwSignal<bool>,
    /// First-run screens (modal; Esc skips them, see the host's key handler).
    pub firstrun: RwSignal<bool>,
    /// Remote: connect (three fields) and site manager.
    pub remote_connect: RwSignal<bool>,
    pub remote_sites: RwSignal<bool>,
    /// One of the remote question / conflict / recovery dialogs is up (mirrored by `RemoteUi`).
    pub remote_dialog: RwSignal<bool>,
}

impl Dialogs {
    pub fn new() -> Self {
        Self {
            settings: create_rw_signal(false),
            shortcuts: create_rw_signal(false),
            cliptext: create_rw_signal(false),
            import: create_rw_signal(false),
            about: create_rw_signal(false),
            firstrun: create_rw_signal(false),
            remote_connect: create_rw_signal(false),
            remote_sites: create_rw_signal(false),
            remote_dialog: create_rw_signal(false),
        }
    }

    /// A modal dialog is on screen: the host's key handler must leave the document alone.
    pub fn modal_open(&self) -> bool {
        self.settings.get_untracked() || self.shortcuts.get_untracked() || self.import.get_untracked() || self.about.get_untracked() || self.firstrun.get_untracked() || self.remote_connect.get_untracked() || self.remote_sites.get_untracked() || self.remote_dialog.get_untracked()
    }

    pub fn close_modals(&self) {
        self.settings.set(false);
        self.shortcuts.set(false);
        // Escape on top of the first-run screens closes only the dialog above them.
        self.import.set(false);
        self.about.set(false);
        self.remote_connect.set(false);
        self.remote_sites.set(false);
    }

    /// Open the surface a command asked for (`open_panel` name). `false` = not one of ours.
    pub fn open_panel(&self, name: &str) -> bool {
        match name {
            "settings_editor" => self.settings.set(true),
            "keymap_editor" => self.shortcuts.set(true),
            "cliptext" => self.cliptext.update(|v| *v = !*v),
            "import_editplus" => self.import.set(true),
            "about" => self.about.set(true),
            "remote_connect" => self.remote_connect.set(true),
            "remote_sites" => self.remote_sites.set(true),
            _ => return false,
        }
        true
    }
}

impl Default for Dialogs {
    fn default() -> Self {
        Self::new()
    }
}

/// Modal frame: dimmed backdrop, centred card with a title and a close button.
/// `body` is built each time the dialog opens, so it always starts from fresh state.
pub fn modal(visible: RwSignal<bool>, title: &'static str, width: f64, height: f64, body: impl Fn() -> AnyView + 'static) -> impl IntoView {
    dyn_container(
        move || visible.get(),
        move |show| {
            if !show {
                return empty().into_any();
            }
            let close = move || visible.set(false);
            let header = stack((
                label(move || title.to_string()).style(|s| s.font_bold().font_size(14.0).color(active_theme().accent).flex_grow(1.0)),
                label(|| " x ".to_string()).on_click_stop(move |_| close()).style(|s| {
                    let t = active_theme();
                    s.color(t.fg_dim).cursor(CursorStyle::Pointer).padding_horiz(6.0).hover(move |h| h.color(t.accent))
                }),
            ))
            .style(|s| s.flex_row().items_center().width_full());
            let card = stack((header, body()))
                .style(move |s| {
                    let t = active_theme();
                    s.flex_col()
                        .gap(8.0)
                        .padding(12.0)
                        .width(width)
                        .height(height)
                        .max_height_pct(92.0)
                        .background(t.bg_chrome)
                        .color(t.fg)
                        .border(1.0)
                        .border_color(t.accent)
                        .border_radius(4.0)
                })
                // A click inside the card must not reach the backdrop (which closes).
                .on_click_stop(|_| {})
                .on_event_stop(EventListener::KeyDown, move |e| {
                    if let Event::KeyDown(ke) = e {
                        if ke.key.logical_key == Key::Named(NamedKey::Escape) {
                            close();
                        }
                    }
                });
            container(card)
                .style(|s| {
                    let t = active_theme();
                    s.size_full().flex_col().items_center().padding_top(44.0).background(t.bg.multiply_alpha(0.55))
                })
                .on_click_stop(move |_| close())
                .into_any()
        },
    )
    .style(move |s| {
        if visible.get() {
            s.absolute().inset_left(0.0).inset_top(0.0).size_full()
        } else {
            s.display(floem::style::Display::None)
        }
    })
}

/// Look of a small clickable label ("button"): `selected` = the current choice.
pub fn button_style(s: Style, selected: bool) -> Style {
    let t = active_theme();
    let base = s
        .padding_horiz(8.0)
        .padding_vert(3.0)
        .font_size(12.0)
        .border(1.0)
        .border_radius(3.0)
        .cursor(CursorStyle::Pointer);
    if selected {
        base.background(t.accent).color(t.bg).border_color(t.accent)
    } else {
        base.background(t.bg).color(t.fg).border_color(t.border).hover(move |h| h.border_color(t.accent))
    }
}

/// Look of a single-line text field.
pub fn field_style(s: Style) -> Style {
    let t = active_theme();
    s.padding_horiz(8.0)
        .padding_vert(4.0)
        .background(t.bg)
        .color(t.fg)
        .border(1.0)
        .border_color(t.border)
        .border_radius(3.0)
}


// ---------------------------------------------------------------- keyboard order inside a dialog

/// What a key means inside a dialog (pure, so the order logic is testable without a window).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DialogKey {
    Next,
    Prev,
    Enter,
    Esc,
    Other,
}

/// Tab = next, Shift+Tab = previous, Enter, Escape. Ctrl/Alt chords are never ours.
pub fn classify_dialog_key(key: &Key, shift: bool, ctrl_or_alt: bool) -> DialogKey {
    if ctrl_or_alt {
        return DialogKey::Other;
    }
    match key {
        Key::Named(NamedKey::Tab) if shift => DialogKey::Prev,
        Key::Named(NamedKey::Tab) => DialogKey::Next,
        Key::Named(NamedKey::Enter) => DialogKey::Enter,
        Key::Named(NamedKey::Escape) => DialogKey::Esc,
        _ => DialogKey::Other,
    }
}

/// The index that gets focus after `cur` (wraps around both ways). `len == 0` stays at 0.
pub fn cycle_focus(cur: usize, len: usize, backwards: bool) -> usize {
    if len == 0 {
        return 0;
    }
    if backwards {
        (cur + len - 1) % len
    } else {
        (cur + 1) % len
    }
}

/// The focusable controls of one dialog, in visual order. Floem's own Tab handling never reaches a modal
/// (the window-level key handler consumes it), so each control asks the ring to move focus.
#[derive(Clone, Default)]
pub struct FocusRing {
    ids: Rc<RefCell<Vec<ViewId>>>,
}

impl FocusRing {
    pub fn new() -> Self {
        Self::default()
    }

    /// Make `view` part of the ring (next slot) and wire Tab / Shift+Tab / Enter / Escape on it.
    /// `on_enter`: Enter on a text field. For a button, pass its action as `activate` instead (Enter and Space press it).
    pub fn control<V: View + 'static>(&self, view: V, on_enter: Option<Rc<dyn Fn()>>, activate: Option<Rc<dyn Fn()>>, on_esc: Rc<dyn Fn()>) -> V {
        let idx = self.register(view.id());
        let ring = self.clone();
        view.keyboard_navigable().on_event_stop(EventListener::KeyDown, move |e| {
            let Event::KeyDown(ke) = e else { return };
            let chord = ke.modifiers.control() || ke.modifiers.alt() || ke.modifiers.meta();
            match classify_dialog_key(&ke.key.logical_key, ke.modifiers.shift(), chord) {
                DialogKey::Next => ring.go(idx, false),
                DialogKey::Prev => ring.go(idx, true),
                DialogKey::Esc => on_esc(),
                DialogKey::Enter => {
                    if let Some(f) = on_enter.as_ref().or(activate.as_ref()) {
                        f();
                    }
                }
                DialogKey::Other => {
                    if ke.key.logical_key == Key::Named(NamedKey::Space) {
                        if let Some(f) = activate.as_ref() {
                            f();
                        }
                    }
                }
            }
        })
    }

    /// Add a control to the ring; its index is what `go` starts from.
    pub fn register(&self, id: ViewId) -> usize {
        let mut ids = self.ids.borrow_mut();
        ids.push(id);
        ids.len() - 1
    }

    pub fn go(&self, from: usize, backwards: bool) {
        let ids = self.ids.borrow();
        if let Some(id) = ids.get(cycle_focus(from, ids.len(), backwards)) {
            id.request_focus();
        }
    }

    pub fn len(&self) -> usize {
        self.ids.borrow().len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// A button in a [`FocusRing`]: Tab reaches it, Enter and Space press it.
pub fn ring_button(ring: &FocusRing, text: &'static str, selected: bool, on: impl Fn() + 'static, on_esc: Rc<dyn Fn()>) -> impl View {
    let on: Rc<dyn Fn()> = Rc::new(on);
    let click = on.clone();
    let v = label(move || text.to_string()).on_click_stop(move |_| click()).style(move |s| button_style(s, selected));
    ring.control(v, None, Some(on), on_esc)
}

#[cfg(test)]
mod tests {
    use super::*;
    use floem::reactive::{with_scope, Scope};

    fn run<F: FnOnce() + 'static>(f: F) {
        let scope = Scope::new();
        with_scope(scope, f);
        scope.dispose();
    }

    #[test]
    fn tab_cycles_forward_and_shift_tab_backward_with_wraparound() {
        assert_eq!(cycle_focus(0, 4, false), 1);
        assert_eq!(cycle_focus(3, 4, false), 0);
        assert_eq!(cycle_focus(0, 4, true), 3);
        assert_eq!(cycle_focus(2, 4, true), 1);
        assert_eq!(cycle_focus(0, 1, false), 0);
        assert_eq!(cycle_focus(0, 0, true), 0);
        // Address -> Folder -> Cancel -> Connect -> Address
        let mut at = 0;
        let seen: Vec<usize> = (0..5).map(|_| { at = cycle_focus(at, 4, false); at }).collect();
        assert_eq!(seen, vec![1, 2, 3, 0, 1]);
    }

    #[test]
    fn dialog_keys_are_classified_and_chords_are_left_alone() {
        let tab = Key::Named(NamedKey::Tab);
        assert_eq!(classify_dialog_key(&tab, false, false), DialogKey::Next);
        assert_eq!(classify_dialog_key(&tab, true, false), DialogKey::Prev);
        assert_eq!(classify_dialog_key(&tab, false, true), DialogKey::Other);
        assert_eq!(classify_dialog_key(&Key::Named(NamedKey::Enter), false, false), DialogKey::Enter);
        assert_eq!(classify_dialog_key(&Key::Named(NamedKey::Escape), false, false), DialogKey::Esc);
        assert_eq!(classify_dialog_key(&Key::Character("a".into()), false, false), DialogKey::Other);
    }

    #[test]
    fn dialogs_open_by_panel_name_and_report_modality() {
        run(|| {
            let d = Dialogs::new();
            assert!(!d.modal_open());
            assert!(d.open_panel("settings_editor") && d.modal_open());
            d.close_modals();
            assert!(!d.modal_open());
            assert!(d.open_panel("keymap_editor") && d.shortcuts.get_untracked());
            d.close_modals();
            // The cliptext panel toggles and never counts as modal.
            assert!(d.open_panel("cliptext") && d.cliptext.get_untracked() && !d.modal_open());
            assert!(d.open_panel("cliptext") && !d.cliptext.get_untracked());
            assert!(!d.open_panel("quick_open"));
        });
    }

    #[test]
    fn modal_builds_open_and_closed() {
        run(|| {
            let v = create_rw_signal(true);
            let _ = modal(v, "T", 300.0, 200.0, || label(|| "x".to_string()).into_any());
            v.set(false);
            let _ = modal(v, "T", 300.0, 200.0, || label(|| "x".to_string()).into_any());
        });
    }
}
