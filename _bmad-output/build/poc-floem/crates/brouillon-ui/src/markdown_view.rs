//! Markdown preview panel for Brouillon.
//!
//! Renders styled markdown text segments reactively based on the active buffer content,
//! without an embedded browser or WebView2.

use std::sync::Arc;
use floem::reactive::{create_effect, create_rw_signal, RwSignal, SignalGet, SignalUpdate};
use floem::views::{label, scroll, Decorators};
use floem::text::Style as TextStyle;
use floem::IntoView;

use brouillon_editor_engine::buffer::Buffer;
use brouillon_core::theme::current_theme;
use crate::markdown_render::{render_markdown, MdSegment};

/// Renders the Markdown preview pane.
pub fn markdown_preview_panel(
    buffer_sig: RwSignal<Arc<Buffer>>,
    theme_idx: RwSignal<usize>,
) -> impl IntoView {
    let segments_sig = create_rw_signal(Vec::new());

    create_effect(move |_| {
        let buf = buffer_sig.get();
        if buf.is_lazy() {
            segments_sig.set(Vec::new()); // large file mode: no preview of a multi-megabyte document
            return;
        }
        let md_text = buf.text();
        let segments = render_markdown(&md_text);
        segments_sig.set(segments);
    });

    scroll(
        floem::views::dyn_stack(
            move || segments_sig.get(),
            move |seg: &MdSegment| {
                (
                    seg.text.clone(),
                    seg.style.bold,
                    seg.style.italic,
                    seg.style.code,
                    seg.style.heading_level,
                    seg.style.blockquote,
                    seg.style.list_indent,
                )
            },
            move |seg: MdSegment| {
                let text = seg.text.clone();
                let style = seg.style.clone();

                label(move || text.clone()).style(move |s| {
                    let t = current_theme(theme_idx.get());
                    let mut s = s.color(t.fg);

                    if style.bold {
                        s = s.font_bold();
                    }
                    if style.italic {
                        s = s.font_style(TextStyle::Italic);
                    }
                    if style.code {
                        s = s.font_family("Consolas, monospace".to_string())
                            .background(t.bg_chrome)
                            .color(t.accent)
                            .padding_horiz(4.0)
                            .border_radius(2.0);
                    }
                    if style.heading_level > 0 {
                        let size = match style.heading_level {
                            1 => 22.0,
                            2 => 19.0,
                            3 => 17.0,
                            _ => 15.0,
                        };
                        s = s.font_size(size)
                            .color(t.accent)
                            .font_bold()
                            .padding_vert(6.0);
                    }
                    if style.blockquote {
                        // Full text colour: the dim colour was hard to read on a whole paragraph.
                        s = s.color(t.fg)
                            .font_style(TextStyle::Italic)
                            .border_left(3.0)
                            .border_color(t.fg_dim)
                            .padding_left(8.0);
                    }
                    if style.list_indent > 0 {
                        s = s.padding_left((style.list_indent as f32 * 12.0) as f64);
                    }
                    s
                })
            },
        )
        .style(|s| s.flex_col().width_full().padding(16.0).gap(4.0)),
    )
    .style(move |s| {
        let t = current_theme(theme_idx.get());
        s.size_full().background(t.bg)
    })
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
    fn test_markdown_preview_creation() {
        run(|| {
            let buf_sig = create_rw_signal(Arc::new(Buffer::from_str("# Heading\nSome *italic* text.")));
            let theme_sig = create_rw_signal(0);
            let _v = markdown_preview_panel(buf_sig, theme_sig);
        });
    }

    #[test]
    fn test_markdown_preview_updates() {
        run(|| {
            let buf_sig = create_rw_signal(Arc::new(Buffer::from_str("")));
            let theme_sig = create_rw_signal(0);
            let _v = markdown_preview_panel(buf_sig, theme_sig);
            buf_sig.set(Arc::new(Buffer::from_str("## Heading 2\n- Item 1\n- Item 2")));
        });
    }
}
