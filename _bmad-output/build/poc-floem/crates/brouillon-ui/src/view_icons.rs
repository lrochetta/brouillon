//! Two small view-layout buttons at the start of the menu row, drawn here as vector shapes (no image,
//! no third-party artwork): "Show/hide sidebar" and "Tabs at top/bottom". Colours come from the active
//! theme; hover and pressed states are plain background tints.

use brouillon_core::theme::current_theme;
use floem::context::PaintCx;
use floem::kurbo::{Line, RoundedRect, RoundedRectRadii, Stroke};
use floem::reactive::{RwSignal, SignalGet};
use floem::style::CursorStyle;
use floem::views::{container, label, tooltip, Decorators};
use floem::{IntoView, View, ViewId};
use floem_renderer::Renderer;

/// Side of the (square) drawing area, in logical pixels.
/// Width of the two icon buttons in front of the menu titles (2 x (26 cell + 2 margin)); the menu popups
/// are offset by it so each one still opens under its own title.
pub const LEAD_WIDTH: f64 = 56.0;
const BOX: f64 = 16.0;
const FRAME: (f64, f64, f64, f64) = (1.5, 2.5, 14.5, 13.5);
const RADIUS: f64 = 2.0;
const STROKE: f64 = 1.25;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Glyph {
    /// Rounded rectangle, vertical divider, left pane filled while the sidebar is visible.
    Sidebar,
    /// Rounded rectangle with a filled strip along the top (tabs on top) or the bottom (tabs at the bottom).
    Tabs,
}

fn frame() -> RoundedRect {
    RoundedRect::new(FRAME.0, FRAME.1, FRAME.2, FRAME.3, RADIUS)
}

/// X of the sidebar divider.
pub fn sidebar_divider_x() -> f64 {
    6.5
}

/// The filled sidebar pane (left of the divider), corners rounded on the outer side only.
pub fn sidebar_pane() -> RoundedRect {
    RoundedRect::new(FRAME.0, FRAME.1, sidebar_divider_x(), FRAME.3, RoundedRectRadii::new(RADIUS, 0.0, 0.0, RADIUS))
}

/// The tab strip: `(y_top, y_bottom)` of the filled band.
pub fn tab_strip_span(bottom: bool) -> (f64, f64) {
    if bottom { (10.0, FRAME.3) } else { (FRAME.1, 6.0) }
}

struct GlyphView {
    id: ViewId,
    glyph: Glyph,
    /// Sidebar: visible. Tabs: at the bottom.
    on: RwSignal<bool>,
    theme_idx: RwSignal<usize>,
}

impl View for GlyphView {
    fn id(&self) -> ViewId {
        self.id
    }

    fn paint(&mut self, cx: &mut PaintCx) {
        let t = current_theme(self.theme_idx.get());
        let on = self.on.get();
        let ink = t.fg.multiply_alpha(0.9);
        let fill = t.fg.multiply_alpha(0.55);
        let stroke = Stroke::new(STROKE);
        cx.stroke(&frame(), ink, &stroke);
        match self.glyph {
            Glyph::Sidebar => {
                if on {
                    cx.fill(&sidebar_pane(), fill, 0.0);
                }
                cx.stroke(&Line::new((sidebar_divider_x(), FRAME.1), (sidebar_divider_x(), FRAME.3)), ink, &stroke);
            }
            Glyph::Tabs => {
                let (y0, y1) = tab_strip_span(on);
                let radii = if on { RoundedRectRadii::new(0.0, 0.0, RADIUS, RADIUS) } else { RoundedRectRadii::new(RADIUS, RADIUS, 0.0, 0.0) };
                cx.fill(&RoundedRect::new(FRAME.0, y0, FRAME.2, y1, radii), fill, 0.0);
                // A gap in the strip: two tabs.
                cx.stroke(&Line::new((8.0, y0), (8.0, y1)), t.bg_chrome, &Stroke::new(1.0));
            }
        }
    }
}

/// One icon button. `tip` is the tooltip and the accessible name (floem 0.2 has no accessibility tree, so
/// the same text is what the command palette and the View menu carry for screen-reader users).
pub fn icon_button(
    theme_idx: RwSignal<usize>,
    glyph: Glyph,
    on: RwSignal<bool>,
    tip: impl Fn() -> String + 'static,
    on_click: impl Fn() + 'static,
) -> impl IntoView {
    let g = GlyphView { id: ViewId::new(), glyph, on, theme_idx }.style(|s| s.size(BOX, BOX));
    let cell = container(g)
        .on_click_stop(move |_| on_click())
        .style(move |s| {
            let t = current_theme(theme_idx.get());
            let fg = t.fg;
            s.size(26.0, 22.0)
                .margin_horiz(1.0)
                .items_center()
                .justify_center()
                .border_radius(4.0)
                .cursor(CursorStyle::Pointer)
                .hover(move |h| h.background(fg.multiply_alpha(0.10)))
                .active(move |a| a.background(fg.multiply_alpha(0.22)))
        });
    tooltip(cell, move || { let t = tip(); label(move || t.clone()) })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shapes_stay_inside_the_box() {
        for v in [FRAME.0, FRAME.1, FRAME.2, FRAME.3] {
            assert!((0.0..=BOX).contains(&v));
        }
        assert!(sidebar_divider_x() > FRAME.0 && sidebar_divider_x() < FRAME.2);
        for bottom in [false, true] {
            let (a, b) = tab_strip_span(bottom);
            assert!(a >= FRAME.1 && b <= FRAME.3 && a < b);
        }
    }

    #[test]
    fn strip_is_on_the_side_it_names() {
        assert!(tab_strip_span(false).0 < tab_strip_span(true).0);
    }
}
