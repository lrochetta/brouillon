//! Folder tree panel (left side): lazy tree over a `Workspace`.
//!
//! One click selects (folders also expand/collapse), double-click or Enter opens a file, the
//! arrow keys move / expand / collapse while the panel has the keyboard (`TreeUi::focused`).
//! Expansion, selection and the last opened file are saved per workspace root after every change.

use std::path::{Path, PathBuf};
use std::rc::Rc;

use brouillon_editor_engine::workspace::{TreeRow, Workspace};
use floem::event::EventListener;
use floem::reactive::{create_rw_signal, RwSignal, SignalGet, SignalUpdate, SignalWith};
use floem::style::CursorStyle;
use floem::views::{dyn_container, empty, label, scroll, stack, Decorators};
use floem::IntoView;

use crate::theme_ctx::active_theme;

#[derive(Clone, Copy)]
pub struct TreeUi {
    pub visible: RwSignal<bool>,
    /// The panel owns the arrow keys / Enter (set by a click inside it, cleared by a click in the editor).
    pub focused: RwSignal<bool>,
    ws: RwSignal<Option<Workspace>>,
    rows: RwSignal<Vec<TreeRow>>,
    selected: RwSignal<Option<PathBuf>>,
    store: RwSignal<Option<PathBuf>>,
}

pub const LAST_FILE: &str = "last-workspace.txt";

impl TreeUi {
    pub fn new() -> Self {
        Self {
            visible: create_rw_signal(false),
            focused: create_rw_signal(false),
            ws: create_rw_signal(None),
            rows: create_rw_signal(Vec::new()),
            selected: create_rw_signal(None),
            store: create_rw_signal(None),
        }
    }

    /// Where per-workspace state is kept (`<settings dir>/workspaces`).
    pub fn set_store(&self, dir: PathBuf) {
        self.store.set(Some(dir));
    }

    pub fn root(&self) -> Option<PathBuf> {
        self.ws.with_untracked(|w| w.as_ref().map(|w| w.root.clone()))
    }

    pub fn last_file(&self) -> Option<PathBuf> {
        self.ws.with_untracked(|w| w.as_ref().and_then(|w| w.last_file.clone()))
    }

    /// Open `root` as the workspace (restoring what was remembered for it) and show the panel.
    pub fn open_folder(&self, root: &Path) {
        let mut w = Workspace::open_local(root);
        if let Some(dir) = self.store.get_untracked() {
            w.restore(&dir);
            let _ = std::fs::create_dir_all(&dir);
            let _ = std::fs::write(dir.join(LAST_FILE), root.to_string_lossy().as_bytes());
        }
        self.ws.set(Some(w));
        self.visible.set(true);
        self.refresh();
    }

    /// Show / hide the panel. Hiding forgets "reopen at launch"; showing remembers it again.
    pub fn set_visible(&self, on: bool) {
        self.visible.set(on);
        if let (Some(dir), Some(root)) = (self.store.get_untracked(), self.root()) {
            if on {
                let _ = std::fs::write(dir.join(LAST_FILE), root.to_string_lossy().as_bytes());
            } else {
                let _ = std::fs::remove_file(dir.join(LAST_FILE));
            }
        }
        if !on {
            self.focused.set(false);
        }
    }

    /// Root remembered as the last opened workspace, if it still exists.
    pub fn last_workspace(dir: &Path) -> Option<PathBuf> {
        let p = PathBuf::from(std::fs::read_to_string(dir.join(LAST_FILE)).ok()?.trim());
        p.is_dir().then_some(p)
    }

    fn refresh(&self) {
        let (rows, sel) = self.ws.with_untracked(|w| w.as_ref().map(|w| (w.rows(), w.selected.clone())).unwrap_or_default());
        self.rows.set(rows);
        self.selected.set(sel);
        if let (Some(dir), true) = (self.store.get_untracked(), self.ws.with_untracked(|w| w.is_some())) {
            self.ws.with_untracked(|w| {
                if let Some(w) = w {
                    let _ = w.save(&dir);
                }
            });
        }
    }

    /// Activate a row: toggles a folder, returns the file to open.
    pub fn activate(&self, path: &Path) -> Option<PathBuf> {
        let r = self.ws.try_update(|w| w.as_mut().and_then(|w| w.activate(path))).flatten();
        self.refresh();
        r
    }

    /// Single click: select, and toggle a folder.
    pub fn click(&self, row: &TreeRow) {
        self.focused.set(true);
        if row.is_dir {
            self.activate(&row.path);
        } else {
            self.ws.update(|w| {
                if let Some(w) = w {
                    w.selected = Some(row.path.clone());
                }
            });
            self.refresh();
        }
    }

    /// Keyboard: `up`/`down`/`left`/`right`/`enter`. Returns the file to open for Enter.
    pub fn key(&self, key: &str) -> Option<PathBuf> {
        match key {
            "up" | "down" => {
                self.ws.update(|w| {
                    if let Some(w) = w {
                        w.move_selection(if key == "up" { -1 } else { 1 });
                    }
                });
                self.refresh();
                None
            }
            "left" | "right" => {
                if let Some(sel) = self.selected.get_untracked() {
                    let is_dir = self.rows.with_untracked(|r| r.iter().find(|x| x.path == sel).map(|x| (x.is_dir, x.expanded)));
                    self.ws.update(|w| {
                        if let Some(w) = w {
                            match (key, is_dir) {
                                ("right", Some((true, false))) => w.expand(&sel),
                                ("left", Some((true, true))) => w.collapse(&sel),
                                ("left", _) => {
                                    if let Some(parent) = sel.parent() {
                                        if parent != w.root {
                                            w.collapse(parent);
                                            w.selected = Some(parent.to_path_buf());
                                        }
                                    }
                                }
                                _ => {}
                            }
                        }
                    });
                    self.refresh();
                }
                None
            }
            "enter" => {
                let sel = self.selected.get_untracked()?;
                self.activate(&sel)
            }
            _ => None,
        }
    }
}

impl Default for TreeUi {
    fn default() -> Self {
        Self::new()
    }
}

const ROW_H: f64 = 20.0;

fn row_view(row: TreeRow, ui: TreeUi, open: Rc<dyn Fn(PathBuf)>) -> impl IntoView {
    let glyph = if !row.is_dir {
        " "
    } else if row.expanded {
        "\u{25BE}"
    } else {
        "\u{25B8}"
    };
    let name = row.name.clone();
    let is_dir = row.is_dir;
    let depth = row.depth;
    let sel_path = row.path.clone();
    let click_row = row.clone();
    let dbl_path = row.path.clone();
    stack((label(move || glyph.to_string()).style(|s| s.width(14.0)), label(move || name.clone())))
        .on_click_stop(move |_| ui.click(&click_row))
        .on_event_stop(EventListener::DoubleClick, move |_| {
            if !is_dir {
                open(dbl_path.clone());
            }
        })
        .style(move |s| {
            let t = active_theme();
            let selected = ui.selected.get().as_deref() == Some(sel_path.as_path());
            let base = s
                .flex_row()
                .items_center()
                .width_full()
                .height(ROW_H)
                .padding_left(8.0 + depth as f64 * 12.0)
                .font_size(12.0)
                .color(if is_dir { t.fg } else { t.fg_dim })
                .cursor(CursorStyle::Pointer)
                .hover(move |h| h.background(t.fg.multiply_alpha(0.06)));
            if selected {
                base.background(t.accent.multiply_alpha(0.18)).color(t.fg)
            } else {
                base
            }
        })
}

/// The panel. `open` is called with the file chosen by double-click (Enter goes through `TreeUi::key`).
pub fn file_tree_panel(ui: TreeUi, open: Rc<dyn Fn(PathBuf)>) -> impl IntoView {
    dyn_container(
        move || ui.visible.get() && ui.ws.with(|w| w.is_some()),
        move |show| {
            if !show {
                return empty().into_any();
            }
            let open = open.clone();
            let header = label(move || ui.ws.with(|w| w.as_ref().map(|w| w.name().to_uppercase()).unwrap_or_default())).style(|s| {
                let t = active_theme();
                s.color(t.fg_dim).font_size(10.0).padding_horiz(10.0).padding_vert(6.0)
            });
            let list = floem::views::dyn_stack(
                move || ui.rows.get().into_iter().map(|r| (r.clone(), ui.selected.get())).collect::<Vec<_>>(),
                // Expansion and selection are baked into a row, so they belong to its key.
                move |(r, sel): &(TreeRow, Option<PathBuf>)| (r.path.clone(), r.expanded, sel.as_ref() == Some(&r.path)),
                move |(r, _): (TreeRow, Option<PathBuf>)| row_view(r, ui, open.clone()),
            )
            .style(|s| s.flex_col().width_full());
            scroll(stack((header, list)).style(|s| s.flex_col().width_full()))
                .style(|s| {
                    let t = active_theme();
                    s.flex_col().width(220.0).height_full().background(t.bg_chrome).border_right(1.0).border_color(t.border)
                })
                .into_any()
        },
    )
}
