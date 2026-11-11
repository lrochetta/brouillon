#![allow(dead_code)]

//! Command Palette overlay view (Cmd-K homepage per Rachel UX vision §3).
//!
//! Companion to `palette.rs` (alpha.3 data layer). Renders the palette
//! when `state.visible == true` ; vanishes otherwise. Action dispatch
//! is via a caller-provided callback so this module doesn't know about
//! buffer / theme / preset / find-replace state.
//!
//! Design (G2 — simplicity first):
//!   - `dyn_container` keyed on `visible` so the overlay is structurally
//!     absent from the layout tree when closed (mirrors find_replace.rs).
//!   - Filtered list is materialised once per render into an `im::Vector`
//!     of `(usize_index, PaletteEntry)` pairs. The index lets `row_view`
//!     know whether it's the selected row without re-deriving the
//!     filtered list per row. `im::Vector` is the only floem 0.2.0
//!     `VirtualVector` impl ; cloning a Vec into one is cheap at the
//!     ≤200-entry budget the data layer guarantees.
//!   - All interaction (typing, arrows, Enter, Escape) is funnelled
//!     through one `on_event_stop(KeyDown)` on the text_input. The
//!     text_input gets focus naturally when the overlay appears (the
//!     merger may add a `request_focus` ; we don't from here so we
//!     don't reach into ViewId plumbing the host owns).
//!   - Mouse click on a row dispatches by id via `on_click_stop`.
//!   - The overlay does NOT call `state.close()` itself after a
//!     dispatch — the host callback is responsible for closing so it
//!     can decide e.g. to keep the palette open on a non-terminal
//!     action. Escape always closes (canonical UX).

use std::sync::Arc;

use floem::event::{Event, EventListener};
use floem::keyboard::{Key, NamedKey};
use floem::reactive::{SignalGet, SignalUpdate};
use floem::views::{
    container, dyn_container, empty, label, scroll, stack, text_input, virtual_list, Decorators,
    VirtualDirection, VirtualItemSize,
};
use floem::IntoView;

use crate::palette::{PaletteEntry, PaletteState};
use crate::theme_ctx::active_theme;

const ROW_HEIGHT: f64 = 28.0;
const OVERLAY_WIDTH: f64 = 600.0;
const OVERLAY_MAX_HEIGHT: f64 = 400.0;
const QUERY_FIELD_HEIGHT: f64 = 32.0;

/// Build the overlay. `on_dispatch` is called with the accepted entry id
/// (the stable string from `PaletteEntry::id`) ; the host (main.rs)
/// dispatches it to the right action (Ctrl-T, Ctrl-O, etc.) and is
/// also responsible for closing the palette via `state.close()`.
pub fn palette_overlay(
    state: Arc<PaletteState>,
    on_dispatch: Arc<dyn Fn(&str) + 'static>,
) -> impl IntoView {
    let visible = state.visible;
    dyn_container(
        move || visible.get(),
        move |is_visible| {
            if is_visible {
                overlay_body(state.clone(), on_dispatch.clone()).into_any()
            } else {
                empty().into_any()
            }
        },
    )
    .style(|s| {
        s.absolute()
            // Anchor below the tab strip (top 80px), horizontally
            // centred in the viewport via the classic `left 50% +
            // negative-half-width margin` trick. Floem 0.2 has both
            // `inset_left_pct` and `margin_left` (PxPctAuto).
            .inset_top(80.0)
            .inset_left_pct(50.0)
            .margin_left((-OVERLAY_WIDTH / 2.0) as f32)
    })
}

fn overlay_body(
    state: Arc<PaletteState>,
    on_dispatch: Arc<dyn Fn(&str) + 'static>,
) -> impl IntoView {
    let query = state.query;
    let selected = state.selected;
    let visible = state.visible;

    // --- Query input -------------------------------------------------------
    //
    // The text_input owns the focus and is where all key events land
    // first. We intercept Arrow{Up,Down}, Enter, Escape on KeyDown and
    // let every other key (typing) flow through to the input — which
    // also updates `state.query` reactively, causing `filtered_signal`
    // below to recompute and the virtual_list to re-key.
    let s_key = state.clone();
    let dispatch_key = on_dispatch.clone();
    let query_field = text_input(query)
        .placeholder("Type a command...")
        // The overlay is built when it opens: take the keyboard so typing lands here, not in the document.
        .request_focus(|| {})
        .style(|s| {
            let t = active_theme();
            s.width(OVERLAY_WIDTH - 16.0)
                .height(QUERY_FIELD_HEIGHT)
                .padding_horiz(8.0)
                .padding_vert(4.0)
                .background(t.bg)
                .border(1.0)
                .border_color(t.fg_dim)
                .border_radius(2.0)
                .color(t.fg)
        })
        .on_event_stop(EventListener::KeyDown, move |event| {
            if let Event::KeyDown(ke) = event {
                match &ke.key.logical_key {
                    Key::Named(NamedKey::ArrowDown) => {
                        s_key.move_selection(1);
                    }
                    Key::Named(NamedKey::ArrowUp) => {
                        s_key.move_selection(-1);
                    }
                    Key::Named(NamedKey::Enter) => {
                        if let Some(entry) = s_key.accept() {
                            (dispatch_key)(entry.id);
                        }
                    }
                    Key::Named(NamedKey::Escape) => {
                        s_key.close();
                    }
                    _ => {}
                }
            }
        });

    // --- Filtered list -----------------------------------------------------
    //
    // Materialise (idx, entry) pairs on every reactive tick. `idx` is
    // the 0-based position in the filtered list — comparing it to
    // `selected.get()` in the row tells us which row should glow.
    //
    // The `move ||` closure subscribes to `query` (via `filtered()`)
    // and `selected` (read in the view fn). When either changes, the
    // virtual_list diffs by key and reuses rows where possible.
    let s_rows = state.clone();
    let dispatch_row = on_dispatch.clone();
    let list = virtual_list(
        VirtualDirection::Vertical,
        VirtualItemSize::Fixed(Box::new(|| ROW_HEIGHT)),
        move || {
            let items: im::Vector<(usize, PaletteEntry)> = s_rows
                .filtered()
                .into_iter()
                .cloned()
                .enumerate()
                .collect();
            items
        },
        // Key = (index, id). Including the index means a re-ordered
        // list (different query) rebuilds rows cleanly ; the id alone
        // would let floem reuse the same row at a different position
        // and the "selected glow" would lag.
        move |(i, e): &(usize, PaletteEntry)| (*i, e.id),
        move |(i, e): (usize, PaletteEntry)| {
            let is_selected = selected.get() == i;
            let id_static: &'static str = e.id;
            let dispatch_one = dispatch_row.clone();
            row_view(&e, is_selected).on_click_stop(move |_| {
                (dispatch_one)(id_static);
            })
        },
    )
    .style(|s| s.flex_col().width_full());

    let list_scroll = scroll(list)
        .style(|s| s.max_height(OVERLAY_MAX_HEIGHT).width_full());

    // --- Card --------------------------------------------------------------
    container(
        stack((query_field, list_scroll))
            .style(|s| s.flex_col().gap(6.0).width_full()),
    )
    .style(|s| {
        let t = active_theme();
        s.padding(8.0)
            .width(OVERLAY_WIDTH)
            .background(t.bg_chrome)
            .color(t.fg)
            .border(1.0)
            .border_color(t.accent)
            .border_radius(4.0)
    })
    // Defensive : if a child has focus and doesn't intercept Escape
    // (e.g. focus drifts to a row), still close the overlay.
    .on_event_stop(EventListener::KeyDown, move |event| {
        if let Event::KeyDown(ke) = event {
            if let Key::Named(NamedKey::Escape) = &ke.key.logical_key {
                visible.set(false);
            }
        }
    })
}

/// One row : title on the left, dimmed shortcut hint on the right,
/// optional accent background when selected.
fn row_view(entry: &PaletteEntry, is_selected: bool) -> impl IntoView {
    let title = entry.title.clone();
    let hint = entry.shortcut_hint.clone().unwrap_or_default();

    let title_label = label(move || title.clone())
        .style(|s| s.color(active_theme().fg).flex_grow(1.0));

    let hint_label = label(move || hint.clone())
        .style(|s| s.color(active_theme().fg_dim).padding_horiz(6.0));

    container(
        stack((title_label, hint_label))
            .style(|s| s.flex_row().items_center().width_full().gap(8.0)),
    )
    .style(move |s| {
        let base = s
            .height(ROW_HEIGHT)
            .width_full()
            .padding_horiz(8.0)
            .padding_vert(2.0)
            .items_center();
        if is_selected {
            // Same highlight as the current line: text stays readable (tested).
            base.background(active_theme().bg_current_line)
        } else {
            base
        }
    })
}

// --------------------------------------------------------------------------
// Tests
// --------------------------------------------------------------------------
//
// The data layer is fully tested in `palette.rs`. This module is pure view
// glue (no logic to test without a render loop) ; we add one smoke test
// that builds the overlay under a Scope just to prove the type wiring
// compiles end-to-end. Concrete UI behavior is covered by the integration
// run from the binary.

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
    fn overlay_builds_with_dispatch_callback() {
        run(|| {
            let state = Arc::new(PaletteState::new());
            let dispatch: Arc<dyn Fn(&str) + 'static> = Arc::new(|_id: &str| {});
            // Just constructing the view tree exercises every generic
            // bound on virtual_list / dyn_container / IntoView. If this
            // compiles and doesn't panic in Scope, the wiring is sane.
            let _ = palette_overlay(state, dispatch);
        });
    }

    #[test]
    fn row_view_builds_selected() {
        run(|| {
            let entry = PaletteEntry {
                id: "test.cmd",
                title: "Test Command".into(),
                shortcut_hint: Some("Ctrl-Alt-T".into()),
                kind: crate::palette::PaletteKind::Action,
            };
            let _view = row_view(&entry, true);
        });
    }

    #[test]
    fn row_view_builds_unselected() {
        run(|| {
            let entry = PaletteEntry {
                id: "test.cmd2",
                title: "Test Command 2".into(),
                shortcut_hint: None,
                kind: crate::palette::PaletteKind::Action,
            };
            let _view = row_view(&entry, false);
        });
    }

    #[test]
    fn overlay_body_builds_directly() {
        run(|| {
            let state = Arc::new(PaletteState::new());
            let dispatch: Arc<dyn Fn(&str) + 'static> = Arc::new(|_| {});
            let _view = overlay_body(state, dispatch);
        });
    }

    #[test]
    fn palette_dimensions_are_sane() {
        assert!(ROW_HEIGHT > 0.0);
        assert!(OVERLAY_WIDTH > 0.0);
        assert!(OVERLAY_MAX_HEIGHT > ROW_HEIGHT);
        assert!(QUERY_FIELD_HEIGHT > 0.0);
    }

    #[test]
    fn overlay_builds_visible_state() {
        run(|| {
            let state = Arc::new(PaletteState::new());
            state.visible.set(true);
            let dispatch: Arc<dyn Fn(&str) + 'static> = Arc::new(|_| {});
            let _view = palette_overlay(state, dispatch);
        });
    }

    #[test]
    fn overlay_builds_with_query_text() {
        run(|| {
            let state = Arc::new(PaletteState::new());
            state.query.set("save".to_string());
            let dispatch: Arc<dyn Fn(&str) + 'static> = Arc::new(|_| {});
            let _view = palette_overlay(state, dispatch);
        });
    }

    #[test]
    fn overlay_builds_with_selection() {
        run(|| {
            let state = Arc::new(PaletteState::new());
            state.selected.set(2);
            let dispatch: Arc<dyn Fn(&str) + 'static> = Arc::new(|_| {});
            let _view = palette_overlay(state, dispatch);
        });
    }
}
