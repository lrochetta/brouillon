use std::path::PathBuf;
use std::sync::Arc;

use floem::reactive::{RwSignal, SignalGet, SignalUpdate, SignalWith};
use floem::views::{dyn_stack, empty, label, scroll, stack, Decorators};
use floem::IntoView;

use brouillon_core::theme::current_theme;
use brouillon_editor_engine::error_patterns::{OutputBuffer, Stream};
use brouillon_editor_engine::shell_runner::CommandResult;

/// Streaming output panel (BK-064/065): lines appear as the tool prints them,
/// a Stop button kills the process, clicking an error line calls `on_jump`
/// with (resolved path, line, col). `footer` holds the elapsed-time/exit text
/// once the run ends; `running` shows/hides the Stop button.
pub fn output_stream_view(
    buf: RwSignal<OutputBuffer>,
    running: RwSignal<bool>,
    footer: RwSignal<String>,
    on_stop: Arc<dyn Fn() + 'static>,
    on_jump: Arc<dyn Fn(PathBuf, usize, usize) + 'static>,
    theme_idx: RwSignal<usize>,
) -> impl IntoView {
    let header = stack((
        label(move || if running.get() { " Running...".to_string() } else { format!(" {}", footer.get()) })
            .style(move |s| s.flex_grow(1.0).color(current_theme(theme_idx.get()).fg_status)),
        label(|| " Stop ".to_string())
            .on_click_stop(move |_| (on_stop)())
            .style(move |s| {
                let t = current_theme(theme_idx.get());
                s.padding_horiz(8.0).background(t.error).color(t.bg).apply_if(!running.get(), |s| s.hide())
            }),
    ))
    .style(move |s| {
        let t = current_theme(theme_idx.get());
        s.width_full().height(22.0).items_center().background(t.bg_chrome).font_size(11.0)
            .border_bottom(1.0).border_color(t.border)
    });

    let lines = dyn_stack(
        move || buf.with(|b| b.lines.iter().enumerate().map(|(i, l)| (i, l.text.clone())).collect::<Vec<_>>()),
        |(i, t)| (*i, t.clone()),
        move |(idx, text)| {
            let on_jump = on_jump.clone();
            let stream = buf.with(|b| b.lines[idx].stream);
            label(move || text.clone())
                .on_click_stop(move |_| {
                    if let Some((p, l, c)) = buf.try_update(|b| b.activate(idx)).flatten() {
                        (on_jump)(p, l, c);
                    }
                })
                .style(move |s| {
                    let t = current_theme(theme_idx.get());
                    let color = match stream {
                        Stream::Stderr => t.error,
                        Stream::Info => t.fg_dim,
                        Stream::Stdout => t.fg,
                    };
                    s.width_full().padding_horiz(8.0).color(color)
                        .font_family("Consolas, Menlo, monospace".to_string()).font_size(11.0)
                })
        },
    )
    .style(|s| s.flex_col().width_full());

    stack((header, scroll(lines).style(|s| s.width_full().min_height(60.0).max_height(220.0))))
        .style(move |s| {
            let t = current_theme(theme_idx.get());
            s.flex_col().width_full().background(t.bg).border_top(2.0).border_color(t.accent)
        })
}

/// Renders the output panel view at the bottom of the editor.
/// It shows execution status, exit code, execution duration, and captured output.
pub fn output_panel_view(
    output_visible_sig: RwSignal<bool>,
    output_panel_sig: RwSignal<Option<CommandResult>>,
    theme_idx: RwSignal<usize>,
) -> impl IntoView {
    floem::views::dyn_container(move || output_visible_sig.get(), move |visible| {
        if visible {
            Box::new(
                stack((
                    // Header bar
                    label(move || {
                        match output_panel_sig.get() {
                            Some(ref r) => format!(" ▶ {} | exit {} | {:.1}s ",
                                r.command_line,
                                r.exit_code.unwrap_or(-1),
                                r.duration.as_secs_f64()),
                            None => " ▶ Running...".to_string(),
                        }
                    }).style(move |s| {
                        let t = current_theme(theme_idx.get());
                        s.width_full().height(20.0).padding_horiz(8.0)
                            .background(t.bg_chrome).color(t.fg_status)
                            .font_size(11.0).border_bottom(1.0).border_color(t.border)
                    }),
                    // Output text
                    label(move || {
                        match output_panel_sig.get() {
                            Some(ref r) => {
                                let mut out = String::new();
                                if !r.stdout.is_empty() {
                                    out.push_str(&r.stdout);
                                }
                                if !r.stderr.is_empty() {
                                    if !out.is_empty() { out.push('\n'); }
                                    out.push_str(&r.stderr);
                                }
                                // Limit display to last 40 lines
                                let lines: Vec<&str> = out.lines().collect();
                                if lines.len() > 40 {
                                    format!("... ({} lines truncated)\n{}", lines.len() - 40,
                                        lines[lines.len() - 40..].join("\n"))
                                } else {
                                    out
                                }
                            }
                            None => "Waiting for output...".to_string(),
                        }
                    }).style(move |s| {
                        let t = current_theme(theme_idx.get());
                        s.width_full().min_height(60.0).max_height(200.0)
                            .padding(8.0).color(t.fg)
                            .font_size(11.0).font_family("Consolas, Menlo, monospace".to_string())
                    }),
                )).style(move |s| {
                    let t = current_theme(theme_idx.get());
                    s.flex_col().width_full()
                        .background(t.bg).border_top(2.0).border_color(t.accent)
                })
            ) as Box<dyn floem::View>
        } else {
            Box::new(empty()) as Box<dyn floem::View>
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use floem::reactive::{with_scope, Scope, create_rw_signal};
    use std::time::Duration;

    fn run<F: FnOnce() + 'static>(f: F) {
        let scope = Scope::new();
        with_scope(scope, f);
        scope.dispose();
    }

    #[test]
    fn stream_view_builds_and_click_resolves_target() {
        use brouillon_editor_engine::error_patterns::builtin_patterns;
        run(|| {
            let mut ob = OutputBuffer::new(PathBuf::from("/w"), builtin_patterns());
            ob.push("src/a.rs:5:2: error: x".into(), Stream::Stderr);
            let buf = create_rw_signal(ob);
            let _v = output_stream_view(
                buf,
                create_rw_signal(false),
                create_rw_signal(String::new()),
                Arc::new(|| {}),
                Arc::new(|_, _, _| {}),
                create_rw_signal(0),
            );
            let target = buf.try_update(|b| b.activate(0)).flatten().unwrap();
            assert_eq!(target, (PathBuf::from("/w").join("src/a.rs"), 5, 2));
        });
    }

    #[test]
    fn test_output_panel_view_creation() {
        run(|| {
            let visible_sig = create_rw_signal(false);
            let panel_sig = create_rw_signal(None);
            let theme_sig = create_rw_signal(0);
            let _v = output_panel_view(visible_sig, panel_sig, theme_sig);
        });
    }

    #[test]
    fn test_output_panel_invisible() {
        run(|| {
            let visible_sig = create_rw_signal(false);
            let panel_sig = create_rw_signal(None);
            let theme_sig = create_rw_signal(0);
            let _v = output_panel_view(visible_sig, panel_sig, theme_sig);
            assert!(!visible_sig.get());
        });
    }

    #[test]
    fn test_output_panel_visible_none() {
        run(|| {
            let visible_sig = create_rw_signal(true);
            let panel_sig = create_rw_signal(None);
            let theme_sig = create_rw_signal(0);
            let _v = output_panel_view(visible_sig, panel_sig, theme_sig);
            assert!(visible_sig.get());
            assert!(panel_sig.get().is_none());
        });
    }

    #[test]
    fn test_output_panel_visible_some() {
        run(|| {
            let visible_sig = create_rw_signal(true);
            let res = CommandResult {
                command_line: "cargo check".to_string(),
                stdout: "Everything is OK".to_string(),
                stderr: "".to_string(),
                exit_code: Some(0),
                duration: Duration::from_millis(150),
                success: true,
            };
            let panel_sig = create_rw_signal(Some(res));
            let theme_sig = create_rw_signal(0);
            let _v = output_panel_view(visible_sig, panel_sig, theme_sig);
            assert!(panel_sig.get().is_some());
        });
    }

    #[test]
    fn test_output_panel_exit_code_fallback() {
        run(|| {
            let visible_sig = create_rw_signal(true);
            let res = CommandResult {
                command_line: "cargo check".to_string(),
                stdout: "".to_string(),
                stderr: "Fatal error".to_string(),
                exit_code: None,
                duration: Duration::from_millis(50),
                success: false,
            };
            let panel_sig = create_rw_signal(Some(res));
            let theme_sig = create_rw_signal(0);
            let _v = output_panel_view(visible_sig, panel_sig, theme_sig);
            assert_eq!(panel_sig.get().unwrap().exit_code, None);
        });
    }

    #[test]
    fn test_output_panel_truncated_lines() {
        run(|| {
            let visible_sig = create_rw_signal(true);
            let mut stdout_big = String::new();
            for i in 0..100 {
                stdout_big.push_str(&format!("line {}\n", i));
            }
            let res = CommandResult {
                command_line: "cargo check".to_string(),
                stdout: stdout_big,
                stderr: "".to_string(),
                exit_code: Some(0),
                duration: Duration::from_millis(200),
                success: true,
            };
            let panel_sig = create_rw_signal(Some(res));
            let theme_sig = create_rw_signal(0);
            let _v = output_panel_view(visible_sig, panel_sig, theme_sig);
            assert!(panel_sig.get().unwrap().stdout.contains("line 99"));
        });
    }
}
