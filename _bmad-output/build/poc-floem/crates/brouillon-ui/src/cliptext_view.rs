//! Cliptext side panel: the libraries of the user's `cliptext` folder, the entries of the chosen
//! one with a preview, insertion at the caret by click or Enter (`Ctrl+Alt+1..0` do the same for
//! the first ten), and a minimal edit: add the selected text, delete an entry, open the library
//! file in a tab, reload it. The panel does not take the keyboard from the document.

use std::path::PathBuf;
use std::rc::Rc;

use brouillon_editor_engine::cliptext::Clip;
use floem::event::{Event, EventListener, EventPropagation};
use floem::keyboard::{Key, NamedKey};
use floem::reactive::{create_rw_signal, RwSignal, SignalGet, SignalUpdate, SignalWith};
use floem::style::CursorStyle;
use floem::views::{dyn_container, dyn_stack, empty, label, scroll, stack, stack_from_iter, text_input, Decorators};
use floem::{AnyView, IntoView};

use crate::cliptext_panel::CliptextPanel;
use crate::cliptext_store::{active_library, clip_from_selection, list_libraries, preview, save_library, set_active};
use crate::theme_ctx::active_theme;
use crate::ui_kit::{button_style, field_style};

/// What the host provides.
#[derive(Clone)]
pub struct ClipHost {
    /// The user's cliptext folder.
    pub dir: PathBuf,
    /// Insert an entry at the caret (replacing the selection), as one undo step.
    pub insert: Rc<dyn Fn(Clip)>,
    /// The currently selected text of the document ("" = none).
    pub selection: Rc<dyn Fn() -> String>,
    /// Open a file in a tab.
    pub open_file: Rc<dyn Fn(PathBuf)>,
}

#[derive(Clone, PartialEq)]
struct Item {
    idx: usize,
    digit: String,
    title: String,
    preview: String,
    selected: bool,
}

struct Libs {
    panel: CliptextPanel,
    paths: Vec<PathBuf>,
}

fn load(dir: &std::path::Path) -> Libs {
    let listed = list_libraries(dir);
    let paths: Vec<PathBuf> = listed.iter().map(|(p, _)| p.clone()).collect();
    let libs = listed
        .into_iter()
        .map(|(p, l)| (p.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default(), l))
        .collect();
    let mut panel = CliptextPanel::new(libs);
    let active = active_library(dir);
    if let Some(i) = paths.iter().position(|p| *p == active) {
        panel.select_library(i);
    }
    Libs { panel, paths }
}

pub fn cliptext_side_panel(visible: RwSignal<bool>, host: ClipHost) -> impl IntoView {
    dyn_container(
        move || visible.get(),
        move |show| if show { body(host.clone(), visible).into_any() } else { empty().into_any() },
    )
    .style(move |s| {
        if visible.get() {
            s.width(300.0).height_full().border_left(1.0).border_color(active_theme().border)
        } else {
            s.display(floem::style::Display::None)
        }
    })
}

fn body(host: ClipHost, visible: RwSignal<bool>) -> AnyView {
    let libs = create_rw_signal(load(&host.dir));
    let filter = create_rw_signal(String::new());
    let selected: RwSignal<Option<usize>> = create_rw_signal(None);
    let note = create_rw_signal(String::new());

    let items = move || -> Vec<Item> {
        let f = filter.get().to_lowercase();
        let sel = selected.get();
        libs.with(|l| {
            l.panel
                .library()
                .map(|lib| {
                    lib.entries
                        .iter()
                        .enumerate()
                        .filter(|(_, c)| f.is_empty() || c.title.to_lowercase().contains(&f) || c.body.to_lowercase().contains(&f))
                        .map(|(i, c)| Item { idx: i, digit: if i < 10 { ((i + 1) % 10).to_string() } else { String::new() }, title: c.title.clone(), preview: preview(c), selected: sel == Some(i) })
                        .collect()
                })
                .unwrap_or_default()
        })
    };

    let insert_at = {
        let insert = host.insert.clone();
        move |idx: usize| {
            if let Some(clip) = libs.with_untracked(|l| l.panel.library().and_then(|lib| lib.entries.get(idx).cloned())) {
                note.set(format!("Inserted \"{}\"", clip.title));
                insert(clip);
            }
        }
    };

    let save_current = move || {
        let (path, lib) = libs.with_untracked(|l| (l.paths.get(l.panel.selected()).cloned(), l.panel.library().cloned()));
        if let (Some(p), Some(lib)) = (path, lib) {
            if let Err(e) = save_library(&p, &lib) {
                note.set(format!("Not saved: {e}"));
            }
        }
    };

    // Library selector (hidden when there is only one library).
    let selector = dyn_container(
        move || libs.with(|l| (l.panel.library_names(), l.panel.selected())),
        move |(names, sel)| -> AnyView {
            if names.len() < 2 {
                return empty().into_any();
            }
            stack_from_iter(names.into_iter().enumerate().map(|(i, n)| {
                label(move || n.clone())
                    .on_click_stop(move |_| {
                        libs.update(|l| {
                            l.panel.select_library(i);
                            if let Some(p) = l.paths.get(i) {
                                set_active(p);
                            }
                        });
                        selected.set(None);
                    })
                    .style(move |s| button_style(s, i == sel))
            }))
            .style(|s| s.flex_row().flex_wrap(floem::taffy::FlexWrap::Wrap).gap(4.0))
            .into_any()
        },
    );

    let header = stack((
        label(|| "Cliptext".to_string()).style(|s| s.font_bold().font_size(14.0).color(active_theme().accent).flex_grow(1.0)),
        label(|| " x ".to_string()).on_click_stop(move |_| visible.set(false)).style(|s| {
            let t = active_theme();
            s.color(t.fg_dim).cursor(CursorStyle::Pointer).hover(move |h| h.color(t.accent))
        }),
    ))
    .style(|s| s.flex_row().items_center().width_full());

    let insert_for_keys = insert_at.clone();
    let search = text_input(filter).placeholder("Filter entries...").style(|s| field_style(s).width_full());

    let list = dyn_stack(
        items,
        move |i: &Item| format!("{}|{}|{}|{}", i.idx, i.title, i.selected, libs.with_untracked(|l| l.panel.selected())),
        move |it: Item| {
            let insert_at = insert_at.clone();
            let idx = it.idx;
            let (title, prev, digit, sel) = (it.title.clone(), it.preview.clone(), it.digit.clone(), it.selected);
            stack((
                label(move || digit.clone()).style(|s| s.width(16.0).font_size(11.0).color(active_theme().fg_dim)),
                stack((
                    label(move || title.clone()).style(|s| s.font_bold().font_size(12.0)),
                    label(move || prev.clone()).style(|s| s.font_size(11.0).color(active_theme().fg_dim)),
                ))
                .style(|s| s.flex_col().min_width(0.0).flex_grow(1.0)),
            ))
            .on_click_stop(move |_| {
                selected.set(Some(idx));
                insert_at(idx);
            })
            .style(move |s| {
                let t = active_theme();
                let s = s.flex_row().items_center().width_full().padding_horiz(8.0).padding_vert(5.0).border_bottom(1.0).border_color(t.border).cursor(CursorStyle::Pointer);
                if sel { s.background(t.bg_current_line) } else { s.hover(move |h| h.background(t.bg_current_line)) }
            })
        },
    )
    .style(|s| s.flex_col().width_full());

    let add = label(|| "Add selection".to_string())
        .on_click_stop({
            let selection = host.selection.clone();
            move |_| match clip_from_selection(&selection()) {
                None => note.set("Select some text in the document first.".into()),
                Some(c) => {
                    let title = c.title.clone();
                    libs.update(|l| l.panel.add(c));
                    save_current();
                    note.set(format!("Added \"{title}\"."));
                }
            }
        })
        .style(|s| button_style(s, false));
    let delete = label(|| "Delete".to_string())
        .on_click_stop(move |_| match selected.get_untracked() {
            None => note.set("Click an entry first.".into()),
            Some(i) => {
                let removed = libs.try_update(|l| l.panel.remove(i)).flatten();
                selected.set(None);
                if let Some(c) = removed {
                    save_current();
                    note.set(format!("Deleted \"{}\".", c.title));
                }
            }
        })
        .style(|s| button_style(s, false));
    let edit_file = label(|| "Edit file".to_string())
        .on_click_stop({
            let open = host.open_file.clone();
            move |_| {
                if let Some(p) = libs.with_untracked(|l| l.paths.get(l.panel.selected()).cloned()) {
                    open(p);
                    note.set("Opened in a tab. Save it, then Reload.".into());
                }
            }
        })
        .style(|s| button_style(s, false));
    let reload = label(|| "Reload".to_string())
        .on_click_stop({
            let dir = host.dir.clone();
            move |_| {
                libs.set(load(&dir));
                selected.set(None);
                note.set("Reloaded from disk.".into());
            }
        })
        .style(|s| button_style(s, false));

    let status = label(move || {
        let n = note.get();
        if n.is_empty() {
            "Click an entry to insert it at the caret. Ctrl+Alt+1..0 insert the first ten.".to_string()
        } else {
            n
        }
    })
    .style(|s| s.font_size(11.0).color(active_theme().fg_dim).width_full());

    let keys = {
        let insert_at = insert_for_keys;
        move |e: &Event| {
            let Event::KeyDown(ke) = e else { return EventPropagation::Continue };
            let count = libs.with_untracked(|l| l.panel.library().map_or(0, |lib| lib.entries.len()));
            match &ke.key.logical_key {
                Key::Named(NamedKey::ArrowDown) if count > 0 => {
                    selected.update(|s| *s = Some(s.map_or(0, |i| (i + 1).min(count - 1))));
                    EventPropagation::Stop
                }
                Key::Named(NamedKey::ArrowUp) if count > 0 => {
                    selected.update(|s| *s = Some(s.map_or(0, |i| i.saturating_sub(1))));
                    EventPropagation::Stop
                }
                Key::Named(NamedKey::Enter) => match selected.get_untracked() {
                    Some(i) => {
                        insert_at(i);
                        EventPropagation::Stop
                    }
                    None => EventPropagation::Continue,
                },
                _ => EventPropagation::Continue,
            }
        }
    };

    stack((
        header,
        selector,
        search,
        scroll(list).style(|s| s.flex_grow(1.0).min_height(0.0).width_full()),
        stack((add, delete, edit_file, reload)).style(|s| s.flex_row().flex_wrap(floem::taffy::FlexWrap::Wrap).gap(4.0)),
        status,
    ))
    .keyboard_navigable()
    .on_event(EventListener::KeyDown, keys)
    .style(|s| {
        let t = active_theme();
        s.flex_col().gap(6.0).padding(10.0).size_full().background(t.bg_chrome).color(t.fg)
    })
    .into_any()
}

#[cfg(test)]
mod tests {
    use super::*;
    use floem::reactive::{with_scope, Scope};

    fn host(dir: PathBuf) -> ClipHost {
        ClipHost { dir, insert: Rc::new(|_| {}), selection: Rc::new(String::new), open_file: Rc::new(|_| {}) }
    }

    #[test]
    fn load_creates_the_default_library_and_selects_it() {
        let dir = std::env::temp_dir().join(format!("brouillon-cliptext-view-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let l = load(&dir);
        assert_eq!(l.paths.len(), 1);
        assert_eq!(l.panel.library_names(), vec!["Default"]);
        assert_eq!(l.panel.rows().len(), 10);
        assert!(l.panel.rows()[0].starts_with("1 MIT license header"), "{:?}", l.panel.rows()[0]);
        assert!(l.panel.rows()[9].starts_with("0 "), "the tenth entry is Ctrl+Alt+0");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn panel_builds_open_and_closed() {
        let dir = std::env::temp_dir().join(format!("brouillon-cliptext-view-b-{}", std::process::id()));
        let scope = Scope::new();
        with_scope(scope, || {
            let v = create_rw_signal(true);
            let _ = cliptext_side_panel(v, host(dir.clone()));
            v.set(false);
            let _ = cliptext_side_panel(v, host(dir.clone()));
        });
        scope.dispose();
        std::fs::remove_dir_all(&dir).ok();
    }
}
