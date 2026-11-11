use floem::event::{Event, EventListener};
use floem::keyboard::{Key, NamedKey};
use floem::reactive::{RwSignal, SignalGet, SignalUpdate, SignalWith};
use floem::views::{container, empty, label, stack, text_input, virtual_list, Decorators, VirtualDirection, VirtualItemSize};
use floem::IntoView;

use brouillon_core::theme::current_theme;

#[derive(Clone, Debug)]
pub struct ChatMessage {
    pub is_user: bool,
    pub content: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PrivacyDot {
    Local,      // Green (Local / Offline)
    Cloud,      // Yellow (Cloud API)
    Sensitive,  // Red (Sensitive data exposed)
}

impl PrivacyDot {
    pub fn label(&self) -> &'static str {
        match self {
            Self::Local => "Local AI (Offline)",
            Self::Cloud => "Cloud AI (Gemini/Cloud)",
            Self::Sensitive => "Cloud AI (Sensitive Data Warning)",
        }
    }
}

/// Renders the Tokyo-Night styled AI Assistant panel.
pub fn ai_panel_view(
    visible: RwSignal<bool>,
    messages: RwSignal<Vec<ChatMessage>>,
    input_text: RwSignal<String>,
    privacy: RwSignal<PrivacyDot>,
    theme_idx: RwSignal<usize>,
    on_send_message: impl Fn(String) + Clone + 'static,
) -> impl IntoView {
    floem::views::dyn_container(
        move || visible.get(),
        move |is_visible| {
            if is_visible {
                ai_panel_body(messages, input_text, privacy, theme_idx, on_send_message.clone()).into_any()
            } else {
                empty().into_any()
            }
        },
    )
    .style(move |s| {
        // A hidden panel must not reserve its 320 px (two of them squeezed the
        // editor column to ~280 px, centred between empty areas).
        if visible.get() {
            s.width(320.0).height_full().border_left(1.0)
        } else {
            s.display(floem::style::Display::None)
        }
    })
}

fn ai_panel_body(
    messages: RwSignal<Vec<ChatMessage>>,
    input_text: RwSignal<String>,
    privacy: RwSignal<PrivacyDot>,
    theme_idx: RwSignal<usize>,
    on_send_message: impl Fn(String) + Clone + 'static,
) -> impl IntoView {
    // Header with Title and Privacy Dot
    let privacy_indicator = move || {
        let p = privacy.get();
        let dot_color = move || {
            let t = current_theme(theme_idx.get());
            match p {
                PrivacyDot::Local => t.fps_ok,
                PrivacyDot::Cloud => t.warn,
                PrivacyDot::Sensitive => t.error,
            }
        };

        stack((
            // Small color dot
            container(empty())
                .style(move |s| {
                    s.width(8.0)
                        .height(8.0)
                        .border_radius(4.0)
                        .background(dot_color())
                }),
            label(move || format!(" {}", p.label()))
                .style(move |s| {
                    let t = current_theme(theme_idx.get());
                    s.color(t.fg_dim).font_size(11.0)
                }),
        ))
        .style(|s| s.flex_row().items_center())
    };

    let header = stack((
        label(|| "AI Assistant".to_string())
            .style(move |s| {
                let t = current_theme(theme_idx.get());
                s.font_bold().color(t.accent).font_size(14.0)
            }),
        privacy_indicator(),
    ))
    .style(move |s| {
        let t = current_theme(theme_idx.get());
        s.flex_col()
            .gap(4.0)
            .padding(10.0)
            .border_bottom(1.0)
            .border_color(t.border)
            .background(t.bg_chrome)
    });

    // Chat History view
    let chat_list = virtual_list(
        VirtualDirection::Vertical,
        VirtualItemSize::Fixed(Box::new(|| 45.0)),
        move || {
            let len = messages.with(|m| m.len());
            let vec: im::Vector<usize> = (0..len).collect();
            vec
        },
        move |idx: &usize| *idx,
        move |idx: usize| {
            let msg = messages.with(|m| m.get(idx).cloned().unwrap());
            let is_user = msg.is_user;
            let content = msg.content.clone();
            stack((
                label(move || if is_user { "You:".to_string() } else { "AI:".to_string() })
                    .style(move |s| {
                        let t = current_theme(theme_idx.get());
                        s.font_bold()
                            .color(if is_user { t.accent } else { t.syn_keyword })
                            .font_size(11.0)
                            .margin_bottom(2.0)
                    }),
                label(move || content.clone())
                    .style(move |s| {
                        let t = current_theme(theme_idx.get());
                        s.color(t.fg)
                            .font_size(12.0)
                    }),
            ))
            .style(move |s| {
                let t = current_theme(theme_idx.get());
                s.flex_col()
                    .padding(8.0)
                    .margin_horiz(8.0)
                    .margin_vert(4.0)
                    .border_radius(4.0)
                    .background(if is_user { t.bg_current_line } else { t.bg })
            })
        },
    )
    .style(|s| s.flex_col().flex_grow(1.0).height(0.0));

    // Fast actions row
    let explain_cb = on_send_message.clone();
    let fix_cb = on_send_message.clone();
    let tests_cb = on_send_message.clone();

    let explain_btn = label(|| "Explain".to_string())
        .style(move |s| {
            let t = current_theme(theme_idx.get());
            s.padding_horiz(8.0)
                .padding_vert(4.0)
                .border(1.0)
                .border_color(t.border)
                .border_radius(4.0)
                .background(t.bg_chrome)
                .color(t.fg)
                .font_size(11.0)
        })
        .on_click_stop(move |_| {
            explain_cb("Explain the current file/selection".to_string());
        });

    let fix_btn = label(|| "Fix Lints".to_string())
        .style(move |s| {
            let t = current_theme(theme_idx.get());
            s.padding_horiz(8.0)
                .padding_vert(4.0)
                .border(1.0)
                .border_color(t.border)
                .border_radius(4.0)
                .background(t.bg_chrome)
                .color(t.fg)
                .font_size(11.0)
        })
        .on_click_stop(move |_| {
            fix_cb("Suggest fixes for any compilation/lint errors".to_string());
        });

    let tests_btn = label(|| "Gen Tests".to_string())
        .style(move |s| {
            let t = current_theme(theme_idx.get());
            s.padding_horiz(8.0)
                .padding_vert(4.0)
                .border(1.0)
                .border_color(t.border)
                .border_radius(4.0)
                .background(t.bg_chrome)
                .color(t.fg)
                .font_size(11.0)
        })
        .on_click_stop(move |_| {
            tests_cb("Generate unit tests for the current code".to_string());
        });

    let action_row = stack((explain_btn, fix_btn, tests_btn))
        .style(|s| {
            s.flex_row()
                .padding_horiz(10.0)
                .padding_vert(6.0)
                .gap(6.0)
                .items_center()
        });

    // Chat input text field
    let send_cb = on_send_message.clone();
    let text_val = input_text;
    let input_field = text_input(input_text)
        .placeholder("Ask assistant...")
        .style(move |s| {
            let t = current_theme(theme_idx.get());
            s.flex_grow(1.0)
                .padding_horiz(8.0)
                .padding_vert(6.0)
                .border(1.0)
                .border_color(t.border)
                .border_radius(4.0)
                .background(t.bg)
                .color(t.fg)
                .font_size(12.0)
        })
        .on_event_stop(EventListener::KeyDown, move |event| {
            if let Event::KeyDown(ke) = event {
                if ke.key.logical_key == Key::Named(NamedKey::Enter) {
                    let content = text_val.get_untracked();
                    if !content.trim().is_empty() {
                        send_cb(content.clone());
                        text_val.set(String::new());
                    }
                }
            }
        });

    let send_btn = label(|| "Send".to_string())
        .style(move |s| {
            let t = current_theme(theme_idx.get());
            s.padding_horiz(12.0)
                .padding_vert(6.0)
                .background(t.accent)
                .color(t.bg)
                .font_bold()
                .border_radius(4.0)
                .font_size(12.0)
        })
        .on_click_stop(move |_| {
            let content = text_val.get_untracked();
            if !content.trim().is_empty() {
                on_send_message(content.clone());
                text_val.set(String::new());
            }
        });

    let input_row = stack((input_field, send_btn))
        .style(move |s| {
            let t = current_theme(theme_idx.get());
            s.flex_row()
                .padding(10.0)
                .gap(8.0)
                .border_top(1.0)
                .border_color(t.border)
                .background(t.bg_chrome)
        });

    stack((header, chat_list, action_row, input_row))
        .style(move |s| {
            let t = current_theme(theme_idx.get());
            s.flex_col()
                .height_full()
                .background(t.bg_chrome)
                .border_left(1.0)
                .border_color(t.border)
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
    fn test_ai_panel_builds() {
        run(|| {
            let visible = create_rw_signal(true);
            let messages = create_rw_signal(vec![
                ChatMessage { is_user: false, content: "Hello, I am your local assistant.".to_string() }
            ]);
            let input = create_rw_signal(String::new());
            let privacy = create_rw_signal(PrivacyDot::Local);
            let theme = create_rw_signal(0);

            let _view = ai_panel_view(visible, messages, input, privacy, theme, |_| {});
        });
    }
}
