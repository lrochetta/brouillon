//! UI side of the external-change guard (`brouillon_editor_engine::external`):
//! a non-modal bar under the tabs, a 1 s timer that stats the ACTIVE file only
//! (no thread, no watcher per file), and the quiet reload.
//!
//! The bar offers, depending on the situation:
//! * file changed on disk  -> Reload / Ignore (local edits are never overwritten),
//! * file gone from disk   -> Save As... / Ignore,
//! * a save was refused because the disk changed -> Overwrite / Reload from disk
//!   (lose my changes) / Save As... Nothing is written until one is chosen.

use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

use floem::action::exec_after;
use floem::style::CursorStyle;
use floem::reactive::{batch, RwSignal, SignalGet, SignalUpdate, SignalWith};
use floem::views::{button, dyn_container, empty, label, stack, Decorators};
use floem::IntoView;

use crate::cursor::Selection;
use crate::external::{self, Notice};
use crate::folding::FoldState;
use crate::mirror::Mirrors;
use crate::tabs::{DocMeta, TabManager};
use crate::theme::current_theme;

/// Message returned by a refused save (the caller turns it into the bar).
pub const CONFLICT: &str = "the file changed on disk; nothing was written";

const POLL: Duration = Duration::from_millis(1000);

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Banner {
    Changed(PathBuf),
    Gone(PathBuf),
    SaveConflict(PathBuf),
}

impl Banner {
    fn path(&self) -> &Path {
        match self {
            Banner::Changed(p) | Banner::Gone(p) | Banner::SaveConflict(p) => p,
        }
    }
}

#[derive(Clone, Copy)]
pub struct Ctx {
    pub tabs: RwSignal<TabManager>,
    pub mirrors: Mirrors,
    pub selection: RwSignal<Selection>,
    pub fold: RwSignal<FoldState>,
    pub status: RwSignal<String>,
    pub theme: RwSignal<usize>,
    pub banner: RwSignal<Option<Banner>>,
}

fn name_of(p: &Path) -> String {
    p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| p.display().to_string())
}

/// Replace the active tab's text with what is on disk. Cursor, selection and scroll
/// stay where they were (clamped); the document becomes unmodified. Undo history restarts.
pub fn reload_active(ctx: Ctx, announce: bool) -> bool {
    let Some(path) = ctx.tabs.with_untracked(|tm| tm.current().path.clone()) else { return false };
    // A routed document (PDF, Word...) has no text to reload: its bytes are shown by the hex view.
    if ctx.tabs.with_untracked(|tm| tm.current().kind.routed_to_viewer()) {
        external::note(&path);
        return false;
    }
    // Same loader as opening a tab: a huge UTF-8 file streams into the rope (no `String` copy, no clone of it).
    let (loaded_buf, fresh_meta) = match crate::tabs::load_buffer(&path) {
        Ok(l) => l,
        Err(e) => {
            ctx.status.set(format!("Could not reload {}: {e}", name_of(&path)));
            return false;
        }
    };
    let buffer = Arc::new(loaded_buf);
    let buf_for_fold = buffer.clone();
    let (line, col) = ctx.tabs.with_untracked(|tm| tm.current().cursor);
    let cur = (line.min(buffer.line_count().max(1)), col);
    ctx.tabs.update(|tm| {
        let t = tm.current_mut();
        let fresh = fresh_meta;
        t.meta = DocMeta { readonly: t.meta.readonly, markers: std::mem::take(&mut t.meta.markers), ..fresh };
        t.encoding = t.meta.legacy_encoding();
        t.eol = t.meta.eol_mode();
        t.buffer = buffer;
        t.cursor = cur;
        t.dirty = t.sketch_unsaved();
    });
    batch(|| {
        ctx.mirrors.pull_from_active(ctx.tabs);
        ctx.selection.set(Selection::new(cur, cur, false));
        ctx.fold.set(crate::folding::detect_for_open(buf_for_fold.rope()));
    });
    external::note(&path);
    ctx.banner.set(None);
    if announce {
        ctx.status.set(format!("Reloaded {}: it was changed on disk by another program", name_of(&path)));
    }
    true
}

/// One check of the active file (called by the timer; also usable on window focus).
pub fn check_active(ctx: Ctx, auto_reload_unmodified: bool) {
    let (path, modified) = ctx.tabs.with_untracked(|tm| {
        let t = tm.current();
        (t.path.clone(), t.buffer.is_dirty() || t.sketch_unsaved())
    });
    let current = ctx.banner.get_untracked();
    let Some(path) = path else {
        if current.is_some() {
            ctx.banner.set(None);
        }
        return;
    };
    // A bar about another tab's file does not follow the user to this tab.
    let current = current.filter(|b| b.path() == path);
    let disk = external::disk_state_for_banner(&path);
    let next = match external::on_poll(modified, disk, auto_reload_unmodified) {
        Notice::Nothing => match current {
            // A refused save keeps its bar until the disk and the document agree again.
            Some(b @ Banner::SaveConflict(_)) if external::disk_state(&path) != external::Disk::Unchanged => Some(b),
            _ => None,
        },
        Notice::Reload => {
            reload_active(ctx, true);
            return;
        }
        Notice::Changed => match current {
            Some(b @ Banner::SaveConflict(_)) => Some(b),
            _ => Some(Banner::Changed(path)),
        },
        Notice::Gone => Some(Banner::Gone(path)),
    };
    if ctx.banner.get_untracked() != next {
        ctx.banner.set(next);
    }
}

/// Start the 1 s timer. Cost per tick: one `stat` of the active file.
pub fn start_polling(ctx: Ctx, auto_reload_unmodified: bool) {
    exec_after(POLL, move |_| {
        check_active(ctx, auto_reload_unmodified);
        start_polling(ctx, auto_reload_unmodified);
    });
}

/// A save was refused: raise the bar and say why.
pub fn show_save_conflict(ctx: Ctx, path: &Path) {
    ctx.banner.set(Some(Banner::SaveConflict(path.to_path_buf())));
    ctx.status.set(format!("{} changed on disk: not saved. Choose Overwrite, Reload or Save As", name_of(path)));
}

fn text_button(ctx: Ctx, caption: &'static str, primary: bool, on: impl Fn() + 'static) -> impl IntoView {
    button(label(move || caption.to_string()))
        .action(on)
        .style(move |s| {
            let t = current_theme(ctx.theme.get());
            s.padding_horiz(10.0)
                .padding_vert(3.0)
                .margin_left(8.0)
                .border(1.0)
                .border_radius(3.0)
                .border_color(t.border)
                .background(if primary { t.accent } else { t.bg })
                .color(if primary { t.bg } else { t.fg })
                .cursor(CursorStyle::Pointer)
        })
}

/// The bar. `save_to(path)` saves the active document to a path (the shell's save);
/// `save_as()` opens the Save As flow.
pub fn banner_view(ctx: Ctx, save_to: Rc<dyn Fn(PathBuf)>, save_as: Rc<dyn Fn()>) -> impl IntoView {
    dyn_container(
        move || ctx.banner.get(),
        move |b| {
            let Some(b) = b else { return empty().into_any() };
            let path = b.path().to_path_buf();
            let name = name_of(&path);
            let (msg, buttons): (String, Vec<floem::AnyView>) = match &b {
                Banner::Changed(_) => {
                    let dirty = ctx.tabs.with_untracked(|tm| tm.current().buffer.is_dirty());
                    let msg = if dirty {
                        format!("{name} changed on disk. Your unsaved changes are kept; nothing is overwritten.")
                    } else {
                        format!("{name} changed on disk.")
                    };
                    let p2 = path.clone();
                    (
                        msg,
                        vec![
                            text_button(ctx, "Reload", true, move || {
                                reload_active(ctx, true);
                            })
                            .into_any(),
                            text_button(ctx, "Ignore", false, move || {
                                external::dismiss(&p2);
                                ctx.banner.set(None);
                            })
                            .into_any(),
                        ],
                    )
                }
                Banner::Gone(_) => {
                    let p = path.clone();
                    let sa = save_as.clone();
                    (
                        format!("{name} was deleted, renamed or can no longer be read. Your text is still here."),
                        vec![
                            text_button(ctx, "Save As...", true, move || (sa)()).into_any(),
                            text_button(ctx, "Ignore", false, move || {
                                external::dismiss(&p);
                                ctx.banner.set(None);
                            })
                            .into_any(),
                        ],
                    )
                }
                Banner::SaveConflict(_) => {
                    let (p_over, st) = (path.clone(), save_to.clone());
                    let sa = save_as.clone();
                    (
                        format!("{name} was changed on disk by another program. Nothing was written."),
                        vec![
                            text_button(ctx, "Overwrite", false, move || {
                                // Accept the disk as it is now, then save over it.
                                external::note(&p_over);
                                ctx.banner.set(None);
                                (st)(p_over.clone());
                            })
                            .into_any(),
                            text_button(ctx, "Reload from disk (lose my changes)", false, move || {
                                reload_active(ctx, true);
                            })
                            .into_any(),
                            text_button(ctx, "Save As...", true, move || (sa)()).into_any(),
                        ],
                    )
                }
            };
            let warn_conflict = matches!(b, Banner::SaveConflict(_));
            stack((
                label(move || msg.clone()).style(move |s| {
                    let t = current_theme(ctx.theme.get());
                    s.flex_grow(1.0).color(t.fg).padding_left(8.0).color(if warn_conflict { t.error } else { t.fg })
                }),
                floem::views::stack_from_iter(buttons).style(|s| s.flex_row().items_center()),
            ))
            .style(move |s| {
                let t = current_theme(ctx.theme.get());
                s.flex_row()
                    .items_center()
                    .width_full()
                    .padding_vert(5.0)
                    .padding_horiz(6.0)
                    .background(t.bg_chrome)
                    .border_bottom(1.0)
                    .border_color(t.warn)
            })
            .into_any()
        },
    )
    .style(|s| s.width_full())
}
