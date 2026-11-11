//! Minimap: a narrow overview of the whole document at the right edge of the editor.
//!
//! Cheap by construction: it shows `brouillon_editor_engine::minimap::sample` rows (a fixed
//! number of sampled lines, never one row per line), rebuilt at most once per `DEBOUNCE` after the
//! last edit, and painted as plain rectangles. Click or drag moves the caret to that part of the
//! document (the editor scrolls to the caret).

use std::cell::Cell;
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

use brouillon_core::theme::current_theme;
use brouillon_editor_engine::buffer::Buffer;
use brouillon_editor_engine::minimap::{self, MiniRow};
use floem::context::PaintCx;
use floem::event::{Event, EventListener};
use floem::kurbo::Rect;
use floem::reactive::{create_effect, create_rw_signal, RwSignal, SignalGet, SignalUpdate};
use floem::views::{dyn_container, empty, Decorators};
use floem::{IntoView, View, ViewId};
use floem_renderer::Renderer;

const ROW_H: f64 = 2.0;
const MAX_ROWS: usize = 600;
const WIDTH: f64 = 90.0;
const DEBOUNCE: Duration = Duration::from_millis(150);
const COARSE_DEBOUNCE: Duration = Duration::from_millis(1000);

struct MinimapCanvas {
    id: ViewId,
    rows: RwSignal<Vec<MiniRow>>,
    line_count: RwSignal<usize>,
    viewport: RwSignal<Rect>,
    theme_idx: RwSignal<usize>,
}

impl View for MinimapCanvas {
    fn id(&self) -> ViewId {
        self.id
    }

    fn paint(&mut self, cx: &mut PaintCx) {
        let t = current_theme(self.theme_idx.get());
        let size = self.id.get_layout().map(|l| (l.size.width as f64, l.size.height as f64)).unwrap_or((WIDTH, 0.0));
        let rows = self.rows.get();
        if rows.is_empty() || size.1 <= 0.0 {
            return;
        }
        let row_h = row_height(rows.len(), size.1);
        let ink = t.fg.multiply_alpha(0.32);
        for (i, r) in rows.iter().enumerate() {
            if r.len == 0 {
                continue;
            }
            let x0 = 6.0 + r.indent as f64 * 1.2;
            let w = (r.len as f64 * 0.7).min(size.0 - x0 - 4.0).max(1.0);
            let y = i as f64 * row_h;
            cx.fill(&Rect::new(x0, y, x0 + w, y + (row_h * 0.75).max(1.0)), ink, 0.0);
        }
        // Visible part of the document.
        let vp = self.viewport.get();
        let total = self.line_count.get().max(1);
        let (top, h) = minimap::viewport_span(total, (vp.y0 / crate::editor_view::line_height()).max(0.0) as usize, (vp.height() / crate::editor_view::line_height()).ceil() as usize, 0.02);
        let map_h = rows.len() as f64 * row_h;
        cx.fill(&Rect::new(0.0, top * map_h, size.0, (top + h) * map_h), t.accent.multiply_alpha(0.16), 0.0);
    }
}

fn row_height(rows: usize, height: f64) -> f64 {
    (height / rows.max(1) as f64).min(ROW_H)
}

/// The minimap overlay (absolutely positioned at the right of the editor body, left of the
/// scrollbar). `visible` false builds nothing: no effect, no timer, no sampling.
pub fn minimap_overlay(
    buffer_sig: RwSignal<Arc<Buffer>>,
    viewport: RwSignal<Rect>,
    cursor: RwSignal<(usize, usize)>,
    theme_idx: RwSignal<usize>,
    visible: RwSignal<bool>,
    tab_size: usize,
    vp: crate::viewport::ViewportSignals,
) -> impl IntoView {
    use crate::viewport::{GotoPlan, MinimapMode};
    // Off for a document too big to sample meaningfully (see `HugeGate`), whatever the setting says.
    let shown = move || visible.get() && vp.gate.get().minimap != MinimapMode::Off;
    dyn_container(
        shown,
        move |on| {
            if !on {
                return empty().into_any();
            }
            let rows: RwSignal<Vec<MiniRow>> = create_rw_signal(Vec::new());
            let line_count: RwSignal<usize> = create_rw_signal(1);
            let resample = move || {
                let b = buffer_sig.get_untracked();
                let snap = vp.snapshot.get_untracked();
                let n = crate::viewport::total_rows_for(&snap, || b.line_count(), 0, 0);
                let gate = vp.gate.get_untracked();
                // At most `minimap_rows` window reads, whatever the line count; rows not indexed yet stay blank.
                rows.set(crate::viewport::sample_minimap(&*b, gate.minimap_rows().min(MAX_ROWS), tab_size));
                line_count.set(n);
            };
            resample();
            // Debounced rebuild: one sampling per quiet period, however many keystrokes came.
            let pending = Rc::new(Cell::new(false));
            create_effect(move |first: Option<()>| {
                let _ = buffer_sig.get();
                let _ = vp.snapshot.get();
                if first.is_none() || pending.replace(true) {
                    return;
                }
                let pending = pending.clone();
                let wait = if vp.gate.get_untracked().minimap == MinimapMode::Coarse { COARSE_DEBOUNCE } else { DEBOUNCE };
                floem::action::exec_after(wait, move |_| {
                    pending.set(false);
                    resample();
                });
            });
            let id = ViewId::new();
            create_effect(move |_| {
                let _ = (rows.get(), viewport.get(), theme_idx.get());
                id.request_paint();
            });
            let canvas = MinimapCanvas { id, rows, line_count, viewport, theme_idx };
            let goto = move |y: f64| {
                let n = rows.with_len();
                let map_h = (n as f64 * ROW_H).min(viewport.get_untracked().height()).max(1.0);
                let line = minimap::line_at_fraction(line_count.get_untracked(), y / map_h);
                // A line past the indexed part waits for the index instead of landing somewhere else.
                let b = buffer_sig.get_untracked();
                match crate::viewport::plan_goto(&vp.snapshot.get_untracked(), line + 1, || b.line_count()) {
                    GotoPlan::Go(l) => cursor.set((l, 1)),
                    GotoPlan::Wait(l) => vp.pending_goto.set(Some(l)),
                }
            };
            let dragging = create_rw_signal(false);
            canvas
                .style(move |s| {
                    let t = current_theme(theme_idx.get());
                    s.size_full().background(t.bg.multiply_alpha(0.85)).border_left(1.0).border_color(t.border)
                })
                .on_event_stop(EventListener::PointerDown, move |e| {
                    crate::kbd_focus::editor_pointer();
                    if let Event::PointerDown(pe) = e {
                        dragging.set(true);
                        goto(pe.pos.y);
                    }
                })
                .on_event_stop(EventListener::PointerMove, move |e| {
                    if let (Event::PointerMove(pe), true) = (e, dragging.get_untracked()) {
                        goto(pe.pos.y);
                    }
                })
                .on_event_stop(EventListener::PointerUp, move |_| dragging.set(false))
                .into_any()
        },
    )
    // The container itself is the minimap box: pointer events only reach children inside
    // their ancestors' rectangles, so it must not be a 0 x 0 flex item.
    .style(move |s| {
        if shown() {
            s.absolute().inset_top(0.0).inset_bottom(0.0).inset_right(14.0).width(WIDTH).z_index(40)
        } else {
            s.hide()
        }
    })
}

trait WithLen {
    fn with_len(&self) -> usize;
}

impl WithLen for RwSignal<Vec<MiniRow>> {
    fn with_len(&self) -> usize {
        floem::reactive::SignalWith::with_untracked(self, |v| v.len())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rows_squeeze_to_the_available_height() {
        assert_eq!(row_height(300, 1000.0), ROW_H);
        assert!((row_height(600, 600.0) - 1.0).abs() < 1e-9);
        assert_eq!(row_height(0, 100.0), ROW_H.min(100.0));
    }
}
