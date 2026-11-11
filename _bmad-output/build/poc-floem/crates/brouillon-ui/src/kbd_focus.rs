//! Who owns the keyboard: a small state machine, free of any GUI so it can be tested.
//!
//! The window has one real floem focus (a text field) plus a "virtual" one: the browser rows, which
//! have no text field and take keys through the root key handler. Both must agree, or typed text
//! lands in the wrong place (the bug this module fixes: a click on the editor text did not reach
//! the browser, because the editor stops the pointer event, so the rows kept the keyboard and the
//! typed letters went into the browser filter instead of the document).
//!
//! Rule: every pointer press on the editor side (text, gutter, tabs, minimap) is the event
//! [`Ev::EditorPointer`] and gives the keyboard to the editor, whatever stopped the event.

use std::cell::RefCell;
use std::rc::Rc;

/// The part of the focus that the browser pane and the editor share.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Base {
    /// Keys go to the document (default).
    #[default]
    Editor,
    /// The browser rows have the keyboard (arrows, Enter, type-to-filter go to the pane).
    Rows,
    /// The path field of the browser (a real text field).
    Path,
    /// The filter field of the browser (a real text field).
    Filter,
}

/// What a debug dump or a test reports: the base plus the windows that sit on top of it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Owner {
    Editor,
    Browser,
    Path,
    Filter,
    Modal,
    Palette,
}

impl Owner {
    pub fn as_str(self) -> &'static str {
        match self {
            Owner::Editor => "editor",
            Owner::Browser => "browser",
            Owner::Path => "path",
            Owner::Filter => "filter",
            Owner::Modal => "modal",
            Owner::Palette => "palette",
        }
    }
}

/// Everything that can move the keyboard.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Ev {
    /// A press on the editor text, gutter, empty area, tab bar or minimap.
    EditorPointer,
    /// A click on a row (or a navigation done with the pane).
    RowPointer,
    /// The filter field got the real focus.
    FilterFocused,
    /// The filter field lost the real focus (to somewhere else).
    FilterBlurred,
    /// The path field opened and got the real focus.
    PathFocused,
    /// The path field lost the focus or was closed.
    PathBlurred,
    /// Escape while the pane (rows, filter or path) has the keyboard and has nothing left to clear.
    Escape,
    /// A file was opened from the browser (Enter, double-click).
    FileOpened,
    /// "Browser: Focus File List" / Ctrl+Shift+E / reveal.
    FocusRows,
    /// The pane was hidden or a server was (dis)connected: the editor gets the keyboard.
    ToEditor,
    /// A palette or a modal opened: it takes the keyboard, the pane lets go.
    OverlayOpened,
}

/// The transition table.
pub fn step(base: Base, ev: Ev) -> Base {
    use Base::*;
    match (base, ev) {
        (_, Ev::EditorPointer | Ev::Escape | Ev::FileOpened | Ev::ToEditor | Ev::OverlayOpened) => Editor,
        (_, Ev::RowPointer | Ev::FocusRows) => Rows,
        (_, Ev::FilterFocused) => Filter,
        (_, Ev::PathFocused) => Path,
        // A blur only matters if that field was the owner: a click on a row can fire the blur AFTER the row took over.
        (Filter, Ev::FilterBlurred) | (Path, Ev::PathBlurred) => Editor,
        (b, Ev::FilterBlurred | Ev::PathBlurred) => b,
    }
}

/// Does this event also have to drop the real floem focus (a text field that would keep eating keys)?
/// Not for an overlay opening: the overlay asks for its own focus.
pub fn drops_real_focus(ev: Ev) -> bool {
    matches!(ev, Ev::Escape | Ev::FileOpened | Ev::ToEditor | Ev::FocusRows)
}

/// Overlays win over the base.
pub fn owner(base: Base, modal: bool, palette: bool) -> Owner {
    if modal {
        Owner::Modal
    } else if palette {
        Owner::Palette
    } else {
        match base {
            Base::Editor => Owner::Editor,
            Base::Rows => Owner::Browser,
            Base::Path => Owner::Path,
            Base::Filter => Owner::Filter,
        }
    }
}

/// Should a plain (no Ctrl/Alt) key be swallowed by the pane instead of reaching the document?
/// True for everything that would type or edit text; function keys stay free for shortcuts.
pub fn pane_swallows_plain_key(base: Base, is_text_or_edit_key: bool) -> bool {
    base == Base::Rows && is_text_or_edit_key
}

thread_local! {
    static EDITOR_POINTER_HOOK: RefCell<Option<Rc<dyn Fn()>>> = const { RefCell::new(None) };
}

/// The host registers what an editor-side press does (it feeds [`Ev::EditorPointer`] to the pane).
pub fn set_editor_pointer_hook(f: Rc<dyn Fn()>) {
    EDITOR_POINTER_HOOK.with(|h| *h.borrow_mut() = Some(f));
}

/// Called by every editor-side view that stops the pointer event (so the root never sees it).
pub fn editor_pointer() {
    let f = EDITOR_POINTER_HOOK.with(|h| h.borrow().clone());
    if let Some(f) = f {
        f();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(start: Base, evs: &[Ev]) -> Base {
        evs.iter().fold(start, |b, e| step(b, *e))
    }

    #[test]
    fn click_in_editor_takes_the_keyboard_from_every_pane_state() {
        for b in [Base::Rows, Base::Path, Base::Filter, Base::Editor] {
            assert_eq!(step(b, Ev::EditorPointer), Base::Editor, "from {b:?}");
        }
    }

    #[test]
    fn the_reported_bug_sequence() {
        // type in the filter, click the editor text: the editor owns the keys, typed text must not reach the filter.
        let b = run(Base::Editor, &[Ev::FilterFocused, Ev::EditorPointer, Ev::FilterBlurred]);
        assert_eq!(b, Base::Editor);
        // click a row, then the editor.
        assert_eq!(run(Base::Editor, &[Ev::RowPointer, Ev::EditorPointer]), Base::Editor);
        assert_eq!(owner(b, false, false), Owner::Editor);
    }

    #[test]
    fn row_click_wins_over_a_late_filter_blur() {
        // floem fires FocusLost after the row's click handler ran.
        assert_eq!(run(Base::Filter, &[Ev::RowPointer, Ev::FilterBlurred]), Base::Rows);
        assert_eq!(run(Base::Path, &[Ev::RowPointer, Ev::PathBlurred]), Base::Rows);
    }

    #[test]
    fn blur_of_the_owner_gives_the_editor_the_keyboard() {
        assert_eq!(step(Base::Filter, Ev::FilterBlurred), Base::Editor);
        assert_eq!(step(Base::Path, Ev::PathBlurred), Base::Editor);
        // A stale blur of the other field changes nothing.
        assert_eq!(step(Base::Rows, Ev::FilterBlurred), Base::Rows);
        assert_eq!(step(Base::Filter, Ev::PathBlurred), Base::Filter);
    }

    #[test]
    fn escape_returns_to_the_editor() {
        for b in [Base::Rows, Base::Path, Base::Filter] {
            assert_eq!(step(b, Ev::Escape), Base::Editor);
        }
    }

    #[test]
    fn opening_a_file_gives_the_editor_the_keyboard() {
        assert_eq!(step(Base::Rows, Ev::FileOpened), Base::Editor);
        assert_eq!(step(Base::Filter, Ev::FileOpened), Base::Editor);
    }

    #[test]
    fn focus_file_list_command_works_from_anywhere() {
        for b in [Base::Editor, Base::Filter, Base::Path, Base::Rows] {
            assert_eq!(step(b, Ev::FocusRows), Base::Rows);
        }
        assert!(drops_real_focus(Ev::FocusRows), "a filter field with the real focus would keep eating the keys");
    }

    #[test]
    fn text_fields_get_their_ownership_when_focused() {
        assert_eq!(step(Base::Editor, Ev::FilterFocused), Base::Filter);
        assert_eq!(step(Base::Rows, Ev::PathFocused), Base::Path);
    }

    #[test]
    fn overlays_win_and_release_the_pane() {
        assert_eq!(owner(Base::Rows, true, false), Owner::Modal);
        assert_eq!(owner(Base::Filter, false, true), Owner::Palette);
        assert_eq!(owner(Base::Rows, true, true), Owner::Modal);
        assert_eq!(step(Base::Rows, Ev::OverlayOpened), Base::Editor);
    }

    #[test]
    fn owner_names_are_the_smoke_vocabulary() {
        let names: Vec<_> = [Owner::Editor, Owner::Browser, Owner::Path, Owner::Filter, Owner::Modal, Owner::Palette].iter().map(|o| o.as_str()).collect();
        assert_eq!(names, ["editor", "browser", "path", "filter", "modal", "palette"]);
    }

    #[test]
    fn only_the_rows_swallow_typing() {
        assert!(pane_swallows_plain_key(Base::Rows, true));
        assert!(!pane_swallows_plain_key(Base::Editor, true));
        assert!(!pane_swallows_plain_key(Base::Rows, false));
    }

    #[test]
    fn hook_is_called() {
        use std::cell::Cell;
        let n = Rc::new(Cell::new(0));
        let n2 = n.clone();
        set_editor_pointer_hook(Rc::new(move || n2.set(n2.get() + 1)));
        editor_pointer();
        editor_pointer();
        assert_eq!(n.get(), 2);
    }
}
