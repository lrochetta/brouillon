//! Render-mode badge for the header: icon + "CPU"/"GPU" label, explained on hover.
//!
//! The explanation is a floem overlay (window coordinates, so none of the
//! "absolute child in a non-absolute container" layout traps), shown after a
//! short hover delay, on keyboard focus, or pinned by a click / Enter / Space.
//! Clicking never switches the renderer: that needs a restart.

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::Duration;

use brouillon_core::render_mode::{icon_svg, RenderMode};
use brouillon_core::theme::{current_theme, Theme};
use floem::action::{add_overlay, exec_after, remove_overlay};
use floem::event::{Event, EventListener};
use floem::keyboard::{Key, NamedKey};
use floem::peniko::kurbo::Point;
use floem::peniko::Color;
use floem::reactive::{RwSignal, SignalGet};
use floem::style::CursorStyle;
use floem::views::{label, stack, svg, Decorators};
use floem::{IntoView, View, ViewId};

const POPUP_W: f64 = 360.0;
const HOVER_DELAY: Duration = Duration::from_millis(450);

fn hex(c: Color) -> String {
    format!("#{:02x}{:02x}{:02x}", c.r, c.g, c.b)
}

struct Popup {
    host: Cell<Option<ViewId>>,
    overlay: RefCell<Option<ViewId>>,
    pinned: Cell<bool>,
    hovering: Cell<bool>,
    generation: Cell<u32>,
}

impl Popup {
    fn show(&self, text: &str, theme_idx: RwSignal<usize>) {
        if self.overlay.borrow().is_some() {
            return;
        }
        let Some(host) = self.host.get() else { return };
        let r = host.layout_rect();
        let pos = Point::new((r.x1 - POPUP_W).max(4.0), r.y1 + 4.0);
        let text = text.to_string();
        let id = add_overlay(pos, move |_| {
            label(move || text.clone()).style(move |s| {
                let t: Theme = current_theme(theme_idx.get());
                s.width(POPUP_W)
                    .padding(10.0)
                    .font_size(12.0)
                    .color(t.fg)
                    .background(t.bg_chrome)
                    .border(1.0)
                    .border_color(t.accent)
                    .border_radius(4.0)
            })
        });
        *self.overlay.borrow_mut() = Some(id);
    }

    fn hide(&self) {
        self.generation.set(self.generation.get() + 1);
        if let Some(id) = self.overlay.borrow_mut().take() {
            remove_overlay(id);
        }
    }
}

/// Pill for the header, right-aligned by the caller.
pub fn render_badge(mode: RenderMode, settings_file: String, theme_idx: RwSignal<usize>) -> impl IntoView {
    let text = Rc::new(mode.explanation(&settings_file));
    let st = Rc::new(Popup {
        host: Cell::new(None),
        overlay: RefCell::new(None),
        pinned: Cell::new(false),
        hovering: Cell::new(false),
        generation: Cell::new(0),
    });

    let icon = svg(icon_svg(mode, &hex(current_theme(theme_idx.get_untracked()).fg)))
        .update_value(move || icon_svg(mode, &hex(current_theme(theme_idx.get()).fg)))
        .style(move |s| s.size(16.0, 16.0).color(current_theme(theme_idx.get()).fg));
    let caption = label(move || mode.short().to_string())
        .style(move |s| s.color(current_theme(theme_idx.get()).fg).font_size(11.0).margin_left(4.0));

    let view = stack((icon, caption))
        .keyboard_navigable()
        .style(move |s| {
            let t = current_theme(theme_idx.get());
            let accent = t.accent;
            s.flex_row()
                .items_center()
                .height(18.0)
                .padding_horiz(6.0)
                .border(1.0)
                .border_color(t.border)
                .border_radius(4.0)
                .cursor(CursorStyle::Pointer)
                .focus_visible(move |s| s.border_color(accent))
        });
    st.host.set(Some(view.id()));

    let (s1, t1) = (st.clone(), text.clone());
    let (s2, s3, s4, s5, s6) = (st.clone(), st.clone(), st.clone(), st.clone(), st.clone());
    let (t3, t4, t5) = (text.clone(), text.clone(), text.clone());

    view.on_event_cont(EventListener::PointerEnter, move |_| {
        s1.hovering.set(true);
        let g = s1.generation.get() + 1;
        s1.generation.set(g);
        let (s, t) = (s1.clone(), t1.clone());
        exec_after(HOVER_DELAY, move |_| {
            if s.hovering.get() && s.generation.get() == g {
                s.show(&t, theme_idx);
            }
        });
    })
    .on_event_cont(EventListener::PointerLeave, move |_| {
        s2.hovering.set(false);
        if !s2.pinned.get() {
            s2.hide();
        }
    })
    .on_event_cont(EventListener::FocusGained, move |_| s3.show(&t3, theme_idx))
    .on_event_cont(EventListener::FocusLost, move |_| {
        s4.pinned.set(false);
        s4.hide();
    })
    .on_click_stop(move |_| {
        // Pin the explanation (or unpin). Never changes the renderer.
        if s5.pinned.get() {
            s5.pinned.set(false);
            s5.hide();
        } else {
            s5.pinned.set(true);
            s5.show(&t4, theme_idx);
        }
    })
    .on_event_stop(EventListener::KeyDown, move |e| {
        let Event::KeyDown(k) = e else { return };
        match &k.key.logical_key {
            Key::Named(NamedKey::Escape) => {
                s6.pinned.set(false);
                s6.hide();
            }
            Key::Named(NamedKey::Enter | NamedKey::Space) => {
                let on = !s6.pinned.get();
                s6.pinned.set(on);
                if on {
                    s6.show(&t5, theme_idx);
                } else {
                    s6.hide();
                }
            }
            _ => {}
        }
    })
}
