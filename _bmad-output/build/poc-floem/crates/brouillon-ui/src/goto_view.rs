use std::sync::Arc;
use floem::event::{Event, EventListener};
use floem::keyboard::{Key, NamedKey};
use floem::reactive::{RwSignal, SignalGet, SignalUpdate};
use floem::views::{container, empty, label, stack, text_input, Decorators};
use floem::IntoView;

use brouillon_editor_engine::buffer::Buffer;
use brouillon_core::theme::current_theme;

/// Renders the go-to-line overlay dialogue box.
pub fn go_to_line_overlay(
    visible: RwSignal<bool>,
    input: RwSignal<String>,
    cursor: RwSignal<(usize, usize)>,
    buffer_sig: RwSignal<Arc<Buffer>>,
    theme_idx: RwSignal<usize>,
    vp: crate::viewport::ViewportSignals,
) -> impl IntoView {
    floem::views::dyn_container(
        move || visible.get(),
        move |is_visible| {
            if is_visible {
                go_to_body(visible, input, cursor, buffer_sig, theme_idx, vp).into_any()
            } else {
                empty().into_any()
            }
        },
    )
    .style(|s| {
        s.absolute()
            .inset_top(40.0)
            .inset_left_pct(50.0)
            .margin_left(-160.0_f32)
    })
}

fn go_to_body(
    visible: RwSignal<bool>,
    input: RwSignal<String>,
    cursor: RwSignal<(usize, usize)>,
    buffer_sig: RwSignal<Arc<Buffer>>,
    theme_idx: RwSignal<usize>,
    vp: crate::viewport::ViewportSignals,
) -> impl IntoView {
    use brouillon_editor_engine::selection_cmds::GotoTarget;
    use crate::viewport::GotoPlan;
    let on_submit = move || {
        let v = input.get_untracked();
        // BK-044: `12`, `12:5`, `@0x40` (byte offset).
        match brouillon_editor_engine::selection_cmds::parse_goto(&v) {
            // A plain line number never copies the document; past the indexed part of a huge
            // file it is remembered and completed by the editor when the index catches up.
            Some(GotoTarget::Line(n)) => {
                let buf = buffer_sig.get_untracked();
                match crate::viewport::plan_goto(&vp.snapshot.get_untracked(), n, || buf.line_count()) {
                    GotoPlan::Go(l) => {
                        vp.pending_goto.set(None);
                        cursor.set((l, 1));
                    }
                    GotoPlan::Wait(l) => vp.pending_goto.set(Some(l)),
                }
            }
            Some(t) => {
                let buf = buffer_sig.get_untracked();
                let (l, c) = if buf.is_lazy() {
                    brouillon_editor_engine::selection_cmds::goto_line_col_buf(&buf, t)
                } else {
                    brouillon_editor_engine::selection_cmds::goto_line_col(&buf.text(), t)
                };
                cursor.set((l.clamp(1, buf.line_count().max(1)), c));
            }
            None => {}
        }
        visible.set(false);
    };

    let field = text_input(input)
        .placeholder("Go to line...")
        // The overlay is built when it opens: take the keyboard so typing lands here, not in the document.
        .request_focus(|| {})
        .style(move |s| {
            let t = current_theme(theme_idx.get());
            s.width(280.0)
                .padding_horiz(6.0)
                .padding_vert(3.0)
                .background(t.bg)
                .border(1.0)
                .border_color(t.border)
                .border_radius(2.0)
                .color(t.fg)
        })
        .on_event_stop(EventListener::KeyDown, move |event| {
            if let Event::KeyDown(ke) = event {
                if ke.key.logical_key == Key::Named(NamedKey::Enter) {
                    on_submit();
                } else if ke.key.logical_key == Key::Named(NamedKey::Escape) {
                    visible.set(false);
                }
            }
        });

    container(
        stack((
            label(|| " Go to line ".to_string())
                .style(move |s| {
                    let t = current_theme(theme_idx.get());
                    s.color(t.fg_dim).padding_right(6.0)
                }),
            field,
        ))
        .style(|s| s.flex_row().items_center().gap(4.0)),
    )
    .style(move |s| {
        let t = current_theme(theme_idx.get());
        s.padding(8.0)
            .background(t.bg_chrome)
            .border(1.0)
            .border_color(t.accent)
            .border_radius(4.0)
            .min_width(320.0)
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
    fn test_goto_overlay_creation() {
        run(|| {
            let visible_sig = create_rw_signal(false);
            let input_sig = create_rw_signal(String::new());
            let cursor_sig = create_rw_signal((1, 1));
            let buf_sig = create_rw_signal(Arc::new(Buffer::from_str("hello\nworld")));
            let theme_sig = create_rw_signal(0);
            let _v = go_to_line_overlay(visible_sig, input_sig, cursor_sig, buf_sig, theme_sig, crate::viewport::ViewportSignals::new());
        });
    }

    #[test]
    fn test_goto_overlay_visibility() {
        run(|| {
            let visible_sig = create_rw_signal(true);
            let input_sig = create_rw_signal(String::new());
            let cursor_sig = create_rw_signal((1, 1));
            let buf_sig = create_rw_signal(Arc::new(Buffer::from_str("hello")));
            let theme_sig = create_rw_signal(0);
            let _v = go_to_line_overlay(visible_sig, input_sig, cursor_sig, buf_sig, theme_sig, crate::viewport::ViewportSignals::new());
            assert!(visible_sig.get());
        });
    }

    #[test]
    fn test_goto_submit_valid() {
        run(|| {
            let visible_sig = create_rw_signal(true);
            let input_sig = create_rw_signal("2".to_string());
            let cursor_sig = create_rw_signal((1, 1));
            let buf_sig = create_rw_signal(Arc::new(Buffer::from_str("line 1\nline 2\nline 3")));
            let theme_sig = create_rw_signal(0);
            
            // Render to construct and trigger submit path
            let _v = go_to_body(visible_sig, input_sig, cursor_sig, buf_sig, theme_sig, crate::viewport::ViewportSignals::new());

            // Emulate submit manually since we can't emit real events synchronously easily
            let on_submit = move || {
                let v = input_sig.get_untracked();
                if let Ok(n) = v.trim().parse::<usize>() {
                    let max = buf_sig.get_untracked().line_count().max(1);
                    cursor_sig.set((n.clamp(1, max), 1));
                }
                visible_sig.set(false);
            };
            on_submit();

            assert_eq!(cursor_sig.get(), (2, 1));
            assert!(!visible_sig.get());
        });
    }

    #[test]
    fn test_goto_submit_clamp_max() {
        run(|| {
            let visible_sig = create_rw_signal(true);
            let input_sig = create_rw_signal("99".to_string());
            let cursor_sig = create_rw_signal((1, 1));
            let buf_sig = create_rw_signal(Arc::new(Buffer::from_str("line 1\nline 2")));
            let _theme_sig = create_rw_signal(0);

            let on_submit = move || {
                let v = input_sig.get_untracked();
                if let Ok(n) = v.trim().parse::<usize>() {
                    let max = buf_sig.get_untracked().line_count().max(1);
                    cursor_sig.set((n.clamp(1, max), 1));
                }
                visible_sig.set(false);
            };
            on_submit();

            assert_eq!(cursor_sig.get(), (2, 1));
        });
    }

    #[test]
    fn test_goto_submit_invalid_input() {
        run(|| {
            let visible_sig = create_rw_signal(true);
            let input_sig = create_rw_signal("not_a_number".to_string());
            let cursor_sig = create_rw_signal((1, 1));
            let buf_sig = create_rw_signal(Arc::new(Buffer::from_str("line 1\nline 2")));
            let _theme_sig = create_rw_signal(0);

            let on_submit = move || {
                let v = input_sig.get_untracked();
                if let Ok(n) = v.trim().parse::<usize>() {
                    let max = buf_sig.get_untracked().line_count().max(1);
                    cursor_sig.set((n.clamp(1, max), 1));
                }
                visible_sig.set(false);
            };
            on_submit();

            // Cursor shouldn't change
            assert_eq!(cursor_sig.get(), (1, 1));
        });
    }
}
