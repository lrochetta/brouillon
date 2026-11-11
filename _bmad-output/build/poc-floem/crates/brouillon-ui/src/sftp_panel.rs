use std::sync::{Arc, Mutex};
use floem::reactive::{RwSignal, SignalGet, SignalUpdate};
use floem::style::CursorStyle;
use floem::views::{empty, label, scroll, stack, text_input, Decorators};
use floem::IntoView;

use brouillon_editor_engine::sftp::{SftpClient, RemoteFile};
use brouillon_core::theme::current_theme;

#[derive(Clone)]
pub struct SftpPanelState {
    pub client: Arc<Mutex<SftpClient>>,
    pub host: RwSignal<String>,
    pub user: RwSignal<String>,
    pub pass: RwSignal<String>,
    pub port: RwSignal<String>,
    pub current_dir: RwSignal<String>,
    pub files: RwSignal<Vec<RemoteFile>>,
    pub is_connected: RwSignal<bool>,
}

impl SftpPanelState {
    pub fn new(host: String, user: String, pass: String, port: String) -> Self {
        Self {
            client: Arc::new(Mutex::new(SftpClient::new())),
            host: RwSignal::new(host),
            user: RwSignal::new(user),
            pass: RwSignal::new(pass),
            port: RwSignal::new(port),
            current_dir: RwSignal::new("/".to_string()),
            files: RwSignal::new(Vec::new()),
            is_connected: RwSignal::new(false),
        }
    }

    pub fn refresh_files(&self) {
        let dir = self.current_dir.get();
        if let Ok(c) = self.client.lock() {
            if c.is_connected() {
                if let Ok(file_list) = c.list_dir(&dir) {
                    let mut list = Vec::new();
                    // If not root, add a parent ".." entry
                    if dir != "/" && !dir.is_empty() {
                        // Find parent path
                        let mut parts: Vec<&str> = dir.split('/').filter(|s| !s.is_empty()).collect();
                        if !parts.is_empty() {
                            parts.pop();
                        }
                        let parent_path = if parts.is_empty() {
                            "/".to_string()
                        } else {
                            format!("/{}", parts.join("/"))
                        };
                        list.push(RemoteFile {
                            name: "..".to_string(),
                            path: parent_path,
                            is_dir: true,
                            size: 0,
                        });
                    }
                    list.extend(file_list);
                    self.files.set(list);
                }
            }
        }
    }
}

pub fn sftp_panel_view(
    visible: RwSignal<bool>,
    state: SftpPanelState,
    theme_idx: RwSignal<usize>,
    on_open_file: impl Fn(String, String) + Clone + 'static,
) -> impl IntoView {
    floem::views::dyn_container(
        move || visible.get(),
        move |is_visible| {
            if is_visible {
                sftp_panel_body(state.clone(), theme_idx, on_open_file.clone()).into_any()
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

fn sftp_panel_body(
    state: SftpPanelState,
    theme_idx: RwSignal<usize>,
    on_open_file: impl Fn(String, String) + Clone + 'static,
) -> impl IntoView {
    // Header
    let header = label(|| "SFTP SIMULATION - local folder, no network".to_string())
        .style(move |s| {
            let t = current_theme(theme_idx.get());
            s.font_bold()
                .color(t.accent)
                .font_size(14.0)
                .padding(10.0)
                .border_bottom(1.0)
                .border_color(t.border)
                .background(t.bg_chrome)
        });

    // Main content shifts depending on connection state
    let body_content = move || {
        let is_conn = state.is_connected.get();
        if is_conn {
            connected_view(state.clone(), theme_idx, on_open_file.clone()).into_any()
        } else {
            login_view(state.clone(), theme_idx).into_any()
        }
    };

    stack((header, floem::views::dyn_view(body_content)))
        .style(move |s| {
            let t = current_theme(theme_idx.get());
            s.flex_col()
                .height_full()
                .background(t.bg_chrome)
                .border_left(1.0)
                .border_color(t.border)
        })
}

fn login_view(state: SftpPanelState, theme_idx: RwSignal<usize>) -> impl IntoView {
    let host_input = text_input(state.host)
        .placeholder("Host (e.g. sample-sftp)")
        .style(move |s| {
            let t = current_theme(theme_idx.get());
            s.width_full()
                .padding(6.0)
                .border(1.0)
                .border_color(t.border)
                .border_radius(4.0)
                .background(t.bg)
                .color(t.fg)
        });

    let user_input = text_input(state.user)
        .placeholder("User")
        .style(move |s| {
            let t = current_theme(theme_idx.get());
            s.width_full()
                .padding(6.0)
                .border(1.0)
                .border_color(t.border)
                .border_radius(4.0)
                .background(t.bg)
                .color(t.fg)
        });

    let pass_input = text_input(state.pass)
        .placeholder("Password")
        .style(move |s| {
            let t = current_theme(theme_idx.get());
            s.width_full()
                .padding(6.0)
                .border(1.0)
                .border_color(t.border)
                .border_radius(4.0)
                .background(t.bg)
                .color(t.fg)
        });

    let port_input = text_input(state.port)
        .placeholder("Port")
        .style(move |s| {
            let t = current_theme(theme_idx.get());
            s.width_full()
                .padding(6.0)
                .border(1.0)
                .border_color(t.border)
                .border_radius(4.0)
                .background(t.bg)
                .color(t.fg)
        });

    let connect_state = state.clone();
    let connect_btn = label(|| "Connect".to_string())
        .style(move |s| {
            let t = current_theme(theme_idx.get());
            s.padding_horiz(16.0)
                .padding_vert(8.0)
                .background(t.accent)
                .color(t.bg)
                .font_bold()
                .border_radius(4.0)
                .font_size(12.0)
                .items_center()
                .justify_center()
                .cursor(CursorStyle::Pointer)
        })
        .on_click_stop(move |_| {
            let host = connect_state.host.get_untracked();
            let user = connect_state.user.get_untracked();
            let pass = connect_state.pass.get_untracked();
            let port_str = connect_state.port.get_untracked();
            let port = port_str.parse::<u16>().unwrap_or(22);

            if let Ok(mut c) = connect_state.client.lock() {
                if c.connect(&host, &user, &pass, port).is_ok() {
                    connect_state.current_dir.set("/".to_string());
                    connect_state.is_connected.set(true);
                    connect_state.refresh_files();
                }
            }
        });

    stack((
        label(|| "Connection details".to_string())
            .style(move |s| {
                let t = current_theme(theme_idx.get());
                s.color(t.fg_dim).font_size(11.0).margin_bottom(6.0)
            }),
        host_input,
        user_input,
        pass_input,
        port_input,
        connect_btn,
    ))
    .style(|s| {
        s.flex_col()
            .padding(15.0)
            .gap(10.0)
    })
}

fn connected_view(
    state: SftpPanelState,
    theme_idx: RwSignal<usize>,
    on_open_file: impl Fn(String, String) + Clone + 'static,
) -> impl IntoView {
    let disconnect_state = state.clone();
    let disconnect_btn = label(|| "Disconnect".to_string())
        .style(move |s| {
            let t = current_theme(theme_idx.get());
            s.padding_horiz(8.0)
                .padding_vert(4.0)
                .border(1.0)
                .border_color(t.border)
                .border_radius(4.0)
                .background(t.bg_chrome)
                .color(t.error)
                .cursor(CursorStyle::Pointer)
                .font_size(11.0)
        })
        .on_click_stop(move |_| {
            if let Ok(mut c) = disconnect_state.client.lock() {
                c.disconnect();
            }
            disconnect_state.is_connected.set(false);
            disconnect_state.files.set(Vec::new());
        });

    let current_dir_label = label(move || format!("Dir: {}", state.current_dir.get()))
        .style(move |s| {
            let t = current_theme(theme_idx.get());
            s.color(t.fg).font_bold().font_size(12.0)
        });

    let conn_status = stack((current_dir_label, disconnect_btn))
        .style(|s| {
            s.flex_row()
                .items_center()
                .justify_between()
                .padding(10.0)
        });

    let file_list_state = state.clone();
    let list_view = move || {
        let files_state = file_list_state.clone();
        let click_open = on_open_file.clone();
        let scroll_content = floem::views::dyn_stack(
            move || files_state.files.get(),
            move |file: &RemoteFile| file.path.clone(),
            {
                let click_state = file_list_state.clone();
                move |file: RemoteFile| {
                    let is_dir = file.is_dir;
                    let path = file.path.clone();
                    let name = file.name.clone();
                    let icon = if is_dir { "📁" } else { "📄" };
                    
                    let click_state_inner = click_state.clone();
                    let click_open_inner = click_open.clone();
                    let label_text = format!("{} {}", icon, name);

                    label(move || label_text.clone())
                        .style(move |s| {
                            let t = current_theme(theme_idx.get());
                            s.width_full()
                                .padding_horiz(12.0)
                                .padding_vert(6.0)
                                .color(if is_dir { t.syn_keyword } else { t.fg })
                                .font_size(12.0)
                                .cursor(CursorStyle::Pointer)
                                .hover(move |s| {
                                    s.background(t.bg_current_line)
                                })
                        })
                        .on_click_stop(move |_| {
                            if is_dir {
                                click_state_inner.current_dir.set(path.clone());
                                click_state_inner.refresh_files();
                            } else {
                                if let Ok(c) = click_state_inner.client.lock() {
                                    if let Ok(content) = c.read_file(&path) {
                                        click_open_inner(path.clone(), content);
                                    }
                                }
                            }
                        })
                }
            }
        )
        .style(|s| s.flex_col().width_full());

        scroll(scroll_content)
            .style(|s| s.flex_grow(1.0).height(0.0).width_full())
    };

    stack((conn_status, floem::views::dyn_view(list_view)))
        .style(|s| s.flex_col().flex_grow(1.0).height(0.0).width_full())
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
    fn test_sftp_panel_builds() {
        run(|| {
            let visible = create_rw_signal(true);
            let state = SftpPanelState::new("localhost".to_string(), "user".to_string(), "pass".to_string(), "22".to_string());
            let theme = create_rw_signal(0);
            let _view = sftp_panel_view(visible, state, theme, |_, _| {});
        });
    }
}
