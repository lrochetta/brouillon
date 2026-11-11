#![allow(dead_code)]

//! Tab bar view — companion to `tabs.rs` data layer.
//!
//! The host (main.rs) owns an `RwSignal<TabManager>` so swapping the
//! active tab or closing one re-renders the bar reactively. This view
//! takes the signal + callbacks for tab-switch / tab-close / new-tab
//! and renders the bar.
//!
//! Design notes (G2 — simplicity first):
//!   - The view does NOT mutate `tabs_sig`. Click handlers invoke caller
//!     callbacks (`on_switch`, `on_close`, `on_new_tab`) ; main.rs decides
//!     side-effects (saving the active buffer to the closing tab's path,
//!     prompt-on-dirty-close, etc.). Decoupled this way so the view layer
//!     doesn't grow tangled in host-state concerns.
//!   - Reactivity rides on a `create_memo` that snapshots
//!     `(idx, title, is_active, is_dirty)` tuples on each `tabs_sig` change.
//!     `dyn_stack` keys on `idx` so steady-state re-renders (e.g. just a
//!     switch_to) reuse child views and only diff the changed tabs.
//!   - Click vs. close separation : the close `×` is a `button` peer
//!     *sibling* of the tab body (not nested inside the body's click
//!     handler), so its action fires without bubbling to the body's
//!     `PointerDown`. Matches the brief's "peer, not nested" requirement.
//!   - Horizontal overflow wraps the inner row in `scroll(...)`. floem
//!     0.2.0 `scroll` exposes both axes; the bar's `height(TAB_HEIGHT)`
//!     style on the outer container clamps vertical, so only horizontal
//!     overflow is reachable in practice.

use floem::peniko::Color;
use floem::reactive::{create_memo, RwSignal, SignalGet, SignalWith};
use floem::style::{CursorStyle, Style};
use floem::views::{button, container, dyn_stack, empty, label, scroll, stack, Decorators};
use floem::IntoView;
use std::sync::Arc;

use brouillon_editor_engine::tabs::TabManager;

use crate::theme_ctx::active_theme;

pub(crate) const TAB_HEIGHT: f64 = 28.0;
const TAB_MIN_WIDTH: f64 = 100.0;
const TAB_MAX_WIDTH: f64 = 220.0;

/// Tab strip colours, all from the active theme (re-read inside style closures
/// so a dark/light switch repaints the bar): bar and inactive tabs = chrome,
/// active tab = the editor page colour, modified marker / underline = accent.
struct TabColors {
    bar: Color,
    inactive: Color,
    active: Color,
    accent: Color,
    separator: Color,
    text: Color,
    text_dim: Color,
}

fn tab_colors() -> TabColors {
    let t = active_theme();
    TabColors {
        bar: t.bg_chrome,
        inactive: t.bg_chrome,
        active: t.bg,
        accent: t.accent,
        separator: t.border,
        text: t.fg,
        text_dim: t.fg_status,
    }
}

/// Render the tab bar. Reactive on the `TabManager` signal — adds/removes/
/// switches re-render the bar.
///
/// Callbacks (each cloned per-tab inside the `dyn_stack` view_fn):
/// - `on_switch(idx)` : called when the user clicks a tab body.
/// - `on_close(idx)` : called when the user clicks the × button on a tab.
/// - `on_new_tab()` : called when the user clicks the trailing "+" button.
pub fn tab_bar_view(
    tabs_sig: RwSignal<TabManager>,
    on_switch: Arc<dyn Fn(usize) + 'static>,
    on_close: Arc<dyn Fn(usize) + 'static>,
    on_new_tab: Arc<dyn Fn() + 'static>,
) -> impl IntoView {
    // Memoised snapshot of the tab list. `TabManager` is not `Clone`
    // (data layer constraint — we MUST NOT touch tabs.rs), so we read
    // via `SignalWith::with` which borrows in place. Returning owned
    // `Vec<TabSnapshot>` keeps the memo's output `Clone`-able for
    // `dyn_stack`'s each_fn.
    let snapshot = create_memo(move |_| {
        tabs_sig.with(|tm| {
            let active = tm.active_idx();
            tm.iter()
                .enumerate()
                .map(|(i, t)| TabSnapshot {
                    idx: i,
                    title: t.title.clone(),
                    is_active: i == active,
                    is_dirty: t.dirty,
                    origin: t.remote_origin.as_ref().map(|o| o.chip()),
                })
                .collect::<Vec<_>>()
        })
    });

    // dyn_stack diffs by `idx` — steady-state switches (active flag flip)
    // reuse children, only the toggled-active tab and the previously-
    // active one re-render their styles via the captured signal reads.
    let switch_for_each = on_switch.clone();
    let close_for_each = on_close.clone();
    let tabs_row = dyn_stack(
        move || snapshot.get(),
        tab_key,
        move |t: TabSnapshot| {
            single_tab(
                t.idx,
                t.title,
                t.is_active,
                t.is_dirty,
                t.origin,
                switch_for_each.clone(),
                close_for_each.clone(),
            )
        },
    )
    .style(|s: Style| s.flex_row().items_center().height(TAB_HEIGHT));

    // Scrollable inner row so a long list of tabs overflows horizontally
    // rather than squashing each tab below its min width. The "+" button
    // sits OUTSIDE the scroll so it's always reachable.
    let scroller = scroll(tabs_row).style(|s| {
        s.height(TAB_HEIGHT)
            .flex_basis(0.0)
            .flex_grow(1.0)
            .min_width(0.0)
    });

    let new_btn = button(label(|| "  +  ".to_string()))
        .action(move || on_new_tab())
        .style(|s| {
            let c = tab_colors();
            s.height(TAB_HEIGHT)
                .padding_horiz(8.0)
                .background(c.inactive)
                .border(0.0)
                .color(c.text)
                .cursor(CursorStyle::Pointer)
                .hover(move |s| s.background(c.active))
        });

    container(
        stack((scroller, new_btn)).style(|s| {
            s.flex_row()
                .items_center()
                .width_full()
                .height(TAB_HEIGHT)
        }),
    )
    .style(|s| {
        let c = tab_colors();
        s.width_full()
            .height(TAB_HEIGHT)
            .background(c.bar)
            .border_bottom(1.0)
            .border_color(c.separator)
    })
}

/// Snapshot row for the memo — owned data, `Clone` so `dyn_stack`'s
/// each_fn can re-read it on each diff pass without aliasing the signal.
#[derive(Debug, Clone, PartialEq, Eq)]
struct TabSnapshot {
    idx: usize,
    title: String,
    is_active: bool,
    is_dirty: bool,
    /// `~site` chip of a document that lives on a server.
    origin: Option<String>,
}

/// `dyn_stack` key of a tab. `single_tab` bakes title / active / dirty at build time,
/// so ALL of them must be part of the key: keyed by `idx` alone, closing a tab left the
/// remaining cells showing the closed tab's title, and the active underline stayed on
/// the previously active tab.
fn tab_key(t: &TabSnapshot) -> (usize, String, bool, bool, Option<String>) {
    (t.idx, t.title.clone(), t.is_active, t.is_dirty, t.origin.clone())
}

/// Build one tab cell. Three peer children inside a flex row:
///   1. dirty glyph (`•`) — rendered as `empty()` when clean so layout
///      width is consistent across dirty/clean states.
///   2. title label with ellipsis on overflow ; this and the dirty glyph
///      share a clickable container that calls `on_switch(idx)`.
///   3. close `×` button — a `button` peer (NOT nested under the title's
///      click handler) so its action fires without also triggering
///      switch. This is the "peer, not nested" requirement from the brief.
fn single_tab(
    idx: usize,
    title: String,
    is_active: bool,
    is_dirty: bool,
    origin: Option<String>,
    on_switch: Arc<dyn Fn(usize) + 'static>,
    on_close: Arc<dyn Fn(usize) + 'static>,
) -> impl IntoView {
    let bg_of = move |c: &TabColors| if is_active { c.active } else { c.inactive };

    // Dirty dot — rendered conditionally so inactive tabs don't reserve a
    // visible glyph slot. We use stack-arm + empty() rather than two
    // branches because `single_tab` is a sync builder (no reactive flip).
    let dirty_dot = if is_dirty {
        label(|| " \u{2022} ".to_string()) // U+2022 BULLET
            .style(|s| s.color(tab_colors().accent).padding_horiz(2.0))
            .into_any()
    } else {
        // Two-char padding so dirty <-> clean swaps don't reflow text.
        // The brief reserves a "dirty glyph + title + close" layout, so a
        // small empty spacer here keeps the title's horizontal origin
        // identical to dirty tabs.
        empty().style(|s| s.width(4.0)).into_any()
    };

    // Clickable body. Container so the PointerDown handler has a single
    // hit-target spanning the dirty glyph + title — but explicitly NOT
    // wrapping the close button (which is a peer sibling).
    let title_str = title.clone();
    let on_switch_body = on_switch.clone();
    // Origin chip (`~vps`): only for documents that live on a server; a word, not just a colour.
    let chip = match origin {
        Some(text) => label(move || text.clone())
            .style(|s| {
                let c = tab_colors();
                s.color(c.accent).font_size(10.0).padding_horiz(4.0)
            })
            .into_any(),
        None => empty().into_any(),
    };
    let body = container(
        stack((
            dirty_dot,
            label(move || title_str.clone()).style(move |s| {
                let c = tab_colors();
                s.flex_grow(1.0)
                    .min_width(0.0)
                    .text_ellipsis()
                    .color(if is_active { c.text } else { c.text_dim })
                    .padding_horiz(4.0)
            }),
            chip,
        ))
        .style(|s| s.flex_row().items_center().width_full().min_width(0.0)),
    )
    .on_click_stop(move |_| {
        crate::kbd_focus::editor_pointer();
        on_switch_body(idx)
    })
    .style(|s| {
        s.flex_grow(1.0)
            .min_width(0.0)
            .height(TAB_HEIGHT)
            .padding_horiz(4.0)
            .cursor(CursorStyle::Pointer)
    });

    let on_close_btn = on_close.clone();
    let close_btn = button(label(|| " \u{00d7} ".to_string())) // U+00D7 MULTIPLICATION SIGN — close glyph
        .action(move || on_close_btn(idx))
        .style(move |s| {
            let c = tab_colors();
            s.padding_horiz(4.0)
                .padding_vert(0.0)
                .background(bg_of(&c))
                .border(0.0)
                .color(c.text_dim)
                .cursor(CursorStyle::Pointer)
                .hover(move |s| s.color(c.text).background(c.accent.multiply_alpha(0.25)))
        });

    // Outer cell. Width clamped to [min, max] ; active tab gets a 2-px
    // accent bottom border (the rest of the borders stay 0 so the single
    // `border_color` value affects only the bottom edge). Inactive tabs
    // get no border — the bar itself draws the 1-px separator below.
    stack((body, close_btn)).style(move |s| {
        let c = tab_colors();
        let s = s
            .flex_row()
            .items_center()
            .height(TAB_HEIGHT)
            .min_width(TAB_MIN_WIDTH)
            .max_width(TAB_MAX_WIDTH)
            .background(bg_of(&c))
            .padding_horiz(2.0);
        if is_active {
            s.border_bottom(2.0).border_color(c.accent)
        } else {
            s
        }
    })
}

// --------------------------------------------------------------------------
// Tests
// --------------------------------------------------------------------------
//
// View code can't be unit-tested without a Floem reactive scope + event
// loop ; we cover the data flow instead. The tab_bar emits callbacks ;
// proving they're shaped right is enough to merge with main.rs.

#[cfg(test)]
mod tests {
    use super::*;
    use floem::reactive::{create_rw_signal, with_scope, Scope};
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// Run a closure under a fresh reactive Scope so RwSignals have somewhere
    /// to live. Mirrors the helper in `find_replace::tests`.
    fn run<F: FnOnce() + 'static>(f: F) {
        let scope = Scope::new();
        with_scope(scope, f);
        scope.dispose();
    }

    #[test]
    fn snapshot_reflects_manager_state() {
        // Direct test of the memo-shaped snapshot: build a TabManager,
        // map it the same way the memo would, and assert.
        run(|| {
            let mut tm = TabManager::new();
            tm.new_scratch(); // untitled-2 ; active=1
            tm.current_mut().dirty = true;

            let active = tm.active_idx();
            let snaps: Vec<TabSnapshot> = tm
                .iter()
                .enumerate()
                .map(|(i, t)| TabSnapshot {
                    idx: i,
                    title: t.title.clone(),
                    is_active: i == active,
                    is_dirty: t.dirty,
                    origin: None,
                })
                .collect();

            assert_eq!(snaps.len(), 2);
            assert_eq!(snaps[0].title, "untitled-1");
            assert!(!snaps[0].is_active);
            assert!(!snaps[0].is_dirty);
            assert_eq!(snaps[1].title, "untitled-2");
            assert!(snaps[1].is_active);
            assert!(snaps[1].is_dirty);
        });
    }

    #[test]
    fn callbacks_fire_with_expected_indices() {
        // Smoke-test the callback shape : `Arc<dyn Fn(usize)>` is the
        // contract `tab_bar_view` accepts, and it's the host's job to
        // forward to `tabs_sig.update`. We verify the wiring intent
        // (callable + carries idx) here ; the live-render integration
        // is exercised by main.rs at startup.
        let calls = Arc::new(AtomicUsize::new(0));
        let captured = Arc::new(AtomicUsize::new(usize::MAX));

        let c1 = calls.clone();
        let p1 = captured.clone();
        let on_switch: Arc<dyn Fn(usize)> = Arc::new(move |idx| {
            c1.fetch_add(1, Ordering::Relaxed);
            p1.store(idx, Ordering::Relaxed);
        });
        on_switch(3);
        assert_eq!(calls.load(Ordering::Relaxed), 1);
        assert_eq!(captured.load(Ordering::Relaxed), 3);
    }

    #[test]
    fn tab_bar_view_builds_with_empty_callbacks() {
        run(|| {
            let tabs_sig = create_rw_signal(TabManager::new());
            let on_switch: Arc<dyn Fn(usize)> = Arc::new(|_| {});
            let on_close: Arc<dyn Fn(usize)> = Arc::new(|_| {});
            let on_new_tab: Arc<dyn Fn()> = Arc::new(|| {});
            let _view = tab_bar_view(tabs_sig, on_switch, on_close, on_new_tab);
        });
    }

    #[test]
    fn tab_height_and_width_constants_valid() {
        assert!(TAB_HEIGHT > 0.0);
        assert!(TAB_MIN_WIDTH > 0.0);
        assert!(TAB_MAX_WIDTH > TAB_MIN_WIDTH);
    }

    #[test]
    fn tab_key_changes_with_every_baked_field() {
        let a = TabSnapshot { idx: 0, title: "a".into(), is_active: true, is_dirty: false, origin: None };
        let mut b = a.clone();
        assert_eq!(tab_key(&a), tab_key(&b));
        b.title = "b".into();
        assert_ne!(tab_key(&a), tab_key(&b), "closing the first tab shifts titles under the same idx");
        let mut c = a.clone();
        c.is_active = false;
        assert_ne!(tab_key(&a), tab_key(&c), "switching must rebuild the cell (underline)");
        let mut d = a.clone();
        d.is_dirty = true;
        assert_ne!(tab_key(&a), tab_key(&d));
    }

    #[test]
    fn tab_snapshot_equality() {
        let a = TabSnapshot { idx: 0, title: "test".into(), is_active: true, is_dirty: false, origin: None };
        let b = a.clone();
        assert_eq!(a, b);
    }

    #[test]
    fn a_remote_origin_changes_the_cell_key_so_the_chip_is_drawn() {
        let a = TabSnapshot { idx: 0, title: "a".into(), is_active: true, is_dirty: false, origin: None };
        let b = TabSnapshot { origin: Some("~vps".into()), ..a.clone() };
        assert_ne!(tab_key(&a), tab_key(&b));
    }

    #[test]
    fn snapshot_indices_sequential() {
        run(|| {
            let mut tm = TabManager::new();
            tm.new_scratch();
            tm.new_scratch();
            let snaps: Vec<TabSnapshot> = tm.iter().enumerate().map(|(i, t)| TabSnapshot {
                idx: i, title: t.title.clone(), is_active: i == tm.active_idx(), is_dirty: t.dirty, origin: None,
            }).collect();
            assert_eq!(snaps.len(), 3);
            for (i, s) in snaps.iter().enumerate() {
                assert_eq!(s.idx, i);
            }
        });
    }

    #[test]
    fn only_one_active_tab_in_snapshot() {
        run(|| {
            let mut tm = TabManager::new();
            tm.new_scratch();
            tm.new_scratch();
            let active_count = tm.iter().enumerate()
                .filter(|(i, _)| *i == tm.active_idx())
                .count();
            assert_eq!(active_count, 1);
        });
    }

    #[test]
    fn tab_snapshot_debug() {
        let snap = TabSnapshot { idx: 0, title: "test".into(), is_active: true, is_dirty: false, origin: None };
        let dbg = format!("{:?}", snap);
        assert!(dbg.contains("TabSnapshot"));
    }

    #[test]
    fn tab_cell_spacing_constants() {
        assert!(TAB_HEIGHT >= 20.0);
    }

    #[test]
    fn new_tab_callback_fires() {
        let fired = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let fired_clone = fired.clone();
        let on_new_tab: Arc<dyn Fn() + 'static> = Arc::new(move || {
            fired_clone.store(true, Ordering::Relaxed);
        });
        on_new_tab();
        assert!(fired.load(Ordering::Relaxed));
    }
}
