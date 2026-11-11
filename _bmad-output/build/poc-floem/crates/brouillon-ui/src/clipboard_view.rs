use std::sync::Arc;
use floem::event::{Event, EventListener};
use floem::keyboard::{Key, NamedKey};
use floem::reactive::{RwSignal, SignalGet, SignalUpdate};
use floem::views::{container, dyn_container, empty, label, scroll, stack, virtual_list, Decorators, VirtualDirection, VirtualItemSize};
use floem::IntoView;

use crate::clipboard;

const ROW_HEIGHT: f64 = 32.0;
const MODAL_WIDTH: f64 = 600.0;
const MODAL_MAX_HEIGHT: f64 = 300.0;

pub fn clipboard_overlay(
    visible_sig: RwSignal<bool>,
    selected_sig: RwSignal<usize>,
    theme_idx: RwSignal<usize>,
    on_paste: Arc<dyn Fn(String) + 'static>,
) -> impl IntoView {
    let visible = visible_sig;
    dyn_container(
        move || visible.get(),
        move |is_visible| {
            if is_visible {
                overlay_body(visible_sig, selected_sig, theme_idx, on_paste.clone()).into_any()
            } else {
                empty().into_any()
            }
        },
    )
    .style(|s| {
        s.absolute()
            .inset_top(80.0)
            .inset_left_pct(50.0)
            .margin_left((-MODAL_WIDTH / 2.0) as f32)
            .z_index(999)
    })
}

fn overlay_body(
    visible_sig: RwSignal<bool>,
    selected_sig: RwSignal<usize>,
    theme_idx: RwSignal<usize>,
    on_paste: Arc<dyn Fn(String) + 'static>,
) -> impl IntoView {
    let items = clipboard::get_history();
    let count = items.len();

    selected_sig.update(|s| {
        if count > 0 && *s >= count {
            *s = 0;
        }
    });

    let s_key = selected_sig;
    let v_sig = visible_sig;
    let paste_cb = on_paste.clone();
    let items_for_index = items.clone();
    let items_clone = items.iter().cloned().collect::<im::Vector<clipboard::ClipboardItem>>();
    let list_view = virtual_list(
        VirtualDirection::Vertical,
        VirtualItemSize::Fixed(Box::new(|| ROW_HEIGHT)),
        move || items_clone.clone(),
        move |item: &clipboard::ClipboardItem| item.timestamp.clone() + &item.preview + &item.text,
        move |item: clipboard::ClipboardItem| {
            let index = items_for_index.iter().position(|x| x.text == item.text).unwrap_or(0);
            let is_selected = selected_sig.get() == index;
            let paste_cb_one = paste_cb.clone();
            let item_text = item.text.clone();
            
            row_view(item, is_selected, theme_idx)
                .on_click_stop(move |_| {
                    (paste_cb_one)(item_text.clone());
                    v_sig.set(false);
                })
        }
    )
    .style(|s| s.flex_col().width_full());

    let scroll_view = scroll(list_view)
        .style(|s| s.max_height(MODAL_MAX_HEIGHT).width_full());

    container(
        stack((
            label(|| " Historique du Presse-papiers ").style(move |s| {
                let t = brouillon_core::theme::current_theme(theme_idx.get());
                s.font_size(14.0)
                    .color(t.accent)
                    .padding_bottom(6.0)
                    .margin_bottom(6.0)
            }),
            scroll_view,
        ))
        .style(|s| s.flex_col().width_full())
    )
    .style(move |s| {
        let t = brouillon_core::theme::current_theme(theme_idx.get());
        s.padding(10.0)
            .width(MODAL_WIDTH)
            .background(t.bg_chrome)
            .border(1.0)
            .border_color(t.accent)
            .border_radius(4.0)
    })
    .on_event_stop(EventListener::KeyDown, move |event| {
        if let Event::KeyDown(ke) = event {
            match &ke.key.logical_key {
                Key::Named(NamedKey::ArrowDown) => {
                    s_key.update(|s| {
                        if count > 0 {
                            *s = (*s + 1) % count;
                        }
                    });
                }
                Key::Named(NamedKey::ArrowUp) => {
                    s_key.update(|s| {
                        if count > 0 {
                            *s = if *s == 0 { count - 1 } else { *s - 1 };
                        }
                    });
                }
                Key::Named(NamedKey::Enter) => {
                    let sel_idx = s_key.get_untracked();
                    if sel_idx < count {
                        let text = items[sel_idx].text.clone();
                        (on_paste)(text);
                    }
                    v_sig.set(false);
                }
                Key::Named(NamedKey::Escape) => {
                    v_sig.set(false);
                }
                _ => {}
            }
        }
    })
}

fn row_view(item: clipboard::ClipboardItem, is_selected: bool, theme_idx: RwSignal<usize>) -> impl IntoView {
    container(
        stack((
            label(move || format!(" [{}] ", item.timestamp)).style(move |s| {
                let t = brouillon_core::theme::current_theme(theme_idx.get());
                s.color(t.fg_dim).margin_right(8.0)
            }),
            label(move || item.preview.clone()).style(move |s| {
                let t = brouillon_core::theme::current_theme(theme_idx.get());
                s.color(t.fg).flex_grow(1.0)
            })
        ))
        .style(|s| s.flex_row().items_center().width_full())
    )
    .style(move |s| {
        let t = brouillon_core::theme::current_theme(theme_idx.get());
        let base = s
            .height(ROW_HEIGHT)
            .width_full()
            .padding_horiz(8.0)
            .items_center();
        if is_selected {
            base.background(t.bg_current_line)
        } else {
            base
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use floem::reactive::{with_scope, Scope, create_rw_signal};

    fn run<F: FnOnce() + 'static>(f: F) {
        let scope = Scope::new();
        with_scope(scope, f);
        scope.dispose();
    }

    #[test]
    fn dimensions_are_valid() {
        assert!(ROW_HEIGHT > 0.0);
        assert!(MODAL_WIDTH > 0.0);
        assert!(MODAL_MAX_HEIGHT > ROW_HEIGHT);
    }

    #[test]
    fn clipboard_overlay_builds() {
        run(|| {
            let visible = create_rw_signal(false);
            let selected = create_rw_signal(0);
            let theme = create_rw_signal(0);
            let paste_cb: Arc<dyn Fn(String) + 'static> = Arc::new(|_| {});
            let _view = clipboard_overlay(visible, selected, theme, paste_cb);
        });
    }

    #[test]
    fn clipboard_overlay_builds_visible() {
        run(|| {
            let visible = create_rw_signal(true);
            let selected = create_rw_signal(0);
            let theme = create_rw_signal(0);
            let paste_cb: Arc<dyn Fn(String) + 'static> = Arc::new(|_| {});
            let _view = clipboard_overlay(visible, selected, theme, paste_cb);
        });
    }

    #[test]
    fn overlay_body_builds() {
        run(|| {
            let visible = create_rw_signal(true);
            let selected = create_rw_signal(0);
            let theme = create_rw_signal(0);
            let paste_cb: Arc<dyn Fn(String) + 'static> = Arc::new(|_| {});
            let _view = overlay_body(visible, selected, theme, paste_cb);
        });
    }

    #[test]
    fn row_view_builds_unselected() {
        run(|| {
            let item = clipboard::ClipboardItem {
                text: "hello".to_string(),
                preview: "hello".to_string(),
                timestamp: "12:00:00".to_string(),
            };
            let theme = create_rw_signal(0);
            let _view = row_view(item, false, theme);
        });
    }

    #[test]
    fn row_view_builds_selected() {
        run(|| {
            let item = clipboard::ClipboardItem {
                text: "hello".to_string(),
                preview: "hello".to_string(),
                timestamp: "12:00:00".to_string(),
            };
            let theme = create_rw_signal(0);
            let _view = row_view(item, true, theme);
        });
    }
}
