//! Keyboard Shortcuts dialog: the command table of `KeymapEditorModel` with search, key capture,
//! conflict warnings (replace / cancel), remove and reset. Every change is written to the user
//! keymap file at once and handed to the host, which swaps the live chord table.

use std::path::{Path, PathBuf};
use std::rc::Rc;

use brouillon_core::keymap::{chord_display, parse_bindings_toml, serialize_bindings, Binding};
use floem::event::{Event, EventListener};
use floem::keyboard::{Key, NamedKey};
use floem::reactive::{create_rw_signal, RwSignal, SignalGet, SignalUpdate, SignalWith};
use floem::views::{container, dyn_container, dyn_stack, label, scroll, stack, text_input, Decorators};
use floem::{AnyView, IntoView};

use crate::key_capture::{combo_from_parts, is_usable};
use crate::keymap_editor::{KeymapEditorModel, Row};
use crate::theme_ctx::active_theme;
use crate::ui_kit::{button_style, field_style, modal};

/// What the host provides.
#[derive(Clone)]
pub struct KeymapInit {
    /// `(id, caption)` of every registered command.
    pub commands: Vec<(String, String)>,
    /// Chords below the user layer (registry defaults + the Classic overlay when active).
    pub base: Rc<dyn Fn() -> Vec<Binding>>,
    /// The user keymap file.
    pub file: PathBuf,
    /// Called after the file was written (the host re-reads it and swaps its chord table).
    pub on_saved: Rc<dyn Fn()>,
}

/// A chord taken by other commands, waiting for the user's decision.
#[derive(Clone, PartialEq)]
struct Pending {
    id: String,
    chord: String,
    owners: Vec<String>,
}

fn read_user(file: &Path) -> Vec<Binding> {
    std::fs::read_to_string(file).map(|t| parse_bindings_toml(&t).0).unwrap_or_default()
}

fn write_user(file: &Path, user: &[Binding]) -> Result<(), String> {
    if let Some(dir) = file.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    brouillon_editor_engine::durable::durable_write(file, serialize_bindings(user).as_bytes()).map_err(|e| e.to_string())
}

pub fn shortcuts_overlay(visible: RwSignal<bool>, init: KeymapInit) -> impl IntoView {
    modal(visible, "Keyboard Shortcuts", 860.0, 600.0, move || body(init.clone()))
}

fn body(init: KeymapInit) -> AnyView {
    let model = create_rw_signal(KeymapEditorModel::new(init.commands.clone(), (init.base)(), read_user(&init.file)));
    let filter = create_rw_signal(String::new());
    let recording: RwSignal<Option<String>> = create_rw_signal(None);
    let pending: RwSignal<Option<Pending>> = create_rw_signal(None);
    let note = create_rw_signal(String::new());

    // Write the user layer, then tell the host.
    let save: Rc<dyn Fn(String)> = {
        let init = init.clone();
        Rc::new(move |what: String| match write_user(&init.file, model.with_untracked(|m| m.user_bindings().to_vec()).as_slice()) {
            Ok(()) => {
                (init.on_saved)();
                note.set(format!("{what} - saved, active now."));
            }
            Err(e) => note.set(format!("Could not save {}: {e}", init.file.display())),
        })
    };

    let bind = {
        let save = save.clone();
        move |id: String, chord: String, force: bool| {
            let caption = model.with_untracked(|m| m.caption(&id));
            if force {
                model.update(|m| m.assign_force(&id, &chord));
            } else if model.try_update(|m| m.assign(&id, &chord)).is_some_and(|r| r.is_err()) {
                return;
            }
            save(format!("{} -> {caption}", chord_display(&chord)));
        }
    };

    let search = text_input(filter)
        .placeholder("Search commands...")
        .request_focus(|| {})
        .style(|s| field_style(s).width_full());

    // The banner: key capture, or a conflict waiting for a decision, or the last note.
    let banner = {
        let bind = bind.clone();
        dyn_container(
            move || (recording.get(), pending.get(), note.get()),
            move |(rec, pend, n)| -> AnyView {
                if let Some(id) = rec {
                    let caption = model.with_untracked(|m| m.caption(&id));
                    return capture_box(id, caption, model, recording, pending, note, bind.clone()).into_any();
                }
                if let Some(p) = pend {
                    let owners = p.owners.iter().map(|o| model.with_untracked(|m| m.caption(o))).collect::<Vec<_>>().join(", ");
                    let target = model.with_untracked(|m| m.caption(&p.id));
                    let (b, p1, p2) = (bind.clone(), p.clone(), p.clone());
                    let msg = format!("{} is already used by {owners}. Give it to {target} instead?", chord_display(&p.chord));
                    return stack((
                        label(move || msg.clone()).style(|s| s.color(active_theme().warn).flex_grow(1.0)),
                        label(|| "Replace".to_string())
                            .on_click_stop(move |_| {
                                pending.set(None);
                                b(p1.id.clone(), p1.chord.clone(), true);
                            })
                            .style(|s| button_style(s, true)),
                        label(|| "Cancel".to_string())
                            .on_click_stop(move |_| {
                                pending.set(None);
                                note.set(format!("{} left unchanged.", chord_display(&p2.chord)));
                            })
                            .style(|s| button_style(s, false)),
                    ))
                    .style(|s| s.flex_row().items_center().gap(8.0).width_full().padding_vert(4.0))
                    .into_any();
                }
                label(move || n.clone()).style(|s| s.font_size(11.0).color(active_theme().fg_dim).width_full()).into_any()
            },
        )
        .style(|s| s.width_full().min_height(30.0))
    };

    let header = stack((
        label(|| "Command".to_string()).style(|s| s.flex_grow(1.0)),
        label(|| "Shortcut".to_string()).style(|s| s.width(190.0)),
        label(|| "Source".to_string()).style(|s| s.width(60.0)),
        label(|| "".to_string()).style(|s| s.width(190.0)),
    ))
    .style(|s| s.flex_row().width_full().font_size(11.0).color(active_theme().fg_dim).padding_horiz(6.0));

    let list = dyn_stack(
        move || model.with(|m| m.rows(&filter.get())),
        |r: &Row| format!("{}|{}|{}", r.id, r.chords.join(","), r.customized),
        move |r: Row| command_row(r, model, recording, pending, note, save.clone()),
    )
    .style(|s| s.flex_col().width_full());

    let footer = label(move || {
        "Shortcuts apply at once. Menu hints and the command palette list refresh at the next launch. keymap.toml is rewritten from your bindings (comments are not kept).".to_string()
    })
    .style(|s| s.font_size(11.0).color(active_theme().fg_dim).width_full());

    stack((search, banner, header, scroll(list).style(|s| s.flex_grow(1.0).min_height(0.0).width_full().padding_right(8.0)), footer))
        .style(|s| s.flex_col().gap(6.0).flex_grow(1.0).min_height(0.0).width_full())
        .into_any()
}

/// "Press the new shortcut" box. It takes the keyboard and CONSUMES every key, so nothing reaches
/// the document or the chord table while it is waiting.
fn capture_box(
    id: String,
    caption: String,
    model: RwSignal<KeymapEditorModel>,
    recording: RwSignal<Option<String>>,
    pending: RwSignal<Option<Pending>>,
    note: RwSignal<String>,
    bind: impl Fn(String, String, bool) + 'static,
) -> impl IntoView {
    let shown = format!("Press the new shortcut for {caption}  (Esc cancels)");
    container(label(move || shown.clone()).style(|s| s.color(active_theme().bg)))
        .keyboard_navigable()
        .request_focus(|| {})
        .style(|s| {
            let t = active_theme();
            s.width_full().padding(8.0).background(t.accent).border_radius(3.0)
        })
        .on_event_stop(EventListener::KeyDown, move |e| {
            let Event::KeyDown(ke) = e else { return };
            if ke.key.repeat {
                return;
            }
            let (m, logical, physical) = (&ke.modifiers, &ke.key.logical_key, &ke.key.physical_key);
            if *logical == Key::Named(NamedKey::Escape) && !m.control() && !m.alt() && !m.shift() {
                recording.set(None);
                note.set("Cancelled.".into());
                return;
            }
            let Some(combo) = combo_from_parts(logical, physical, m.control(), m.shift(), m.alt(), m.altgr()) else { return };
            if !is_usable(&combo) {
                note.set("Use Ctrl or Alt with the key, or a function key.".into());
                return;
            }
            let chord = combo.to_string_repr();
            let owners = model.with_untracked(|m| m.conflicts_for(&id, &chord));
            recording.set(None);
            if owners.is_empty() {
                bind(id.clone(), chord, false);
            } else {
                pending.set(Some(Pending { id: id.clone(), chord, owners }));
            }
        })
}

fn command_row(
    r: Row,
    model: RwSignal<KeymapEditorModel>,
    recording: RwSignal<Option<String>>,
    pending: RwSignal<Option<Pending>>,
    note: RwSignal<String>,
    save: Rc<dyn Fn(String)>,
) -> impl IntoView {
    let chords = if r.chords.is_empty() { "-".to_string() } else { r.chords.iter().map(|c| chord_display(c)).collect::<Vec<_>>().join(", ") };
    let source = if r.customized { "yours" } else { "default" };
    let (id_set, id_clear, id_reset) = (r.id.clone(), r.id.clone(), r.id.clone());
    let (save_clear, save_reset) = (save.clone(), save);
    let customized = r.customized;
    let caption = r.caption.clone();

    let set = label(|| "Set...".to_string())
        .on_click_stop(move |_| {
            pending.set(None);
            note.set(String::new());
            recording.set(Some(id_set.clone()));
        })
        .style(|s| button_style(s, false));
    let clear = label(|| "Remove".to_string())
        .on_click_stop(move |_| {
            let cap = model.with_untracked(|m| m.caption(&id_clear));
            model.update(|m| m.clear(&id_clear));
            save_clear(format!("{cap}: shortcuts removed"));
        })
        .style(|s| button_style(s, false));
    let reset = label(|| "Reset".to_string())
        .on_click_stop(move |_| {
            let cap = model.with_untracked(|m| m.caption(&id_reset));
            model.update(|m| m.reset(&id_reset));
            save_reset(format!("{cap}: back to the default"));
        })
        .style(move |s| {
            let s = button_style(s, false);
            if customized { s } else { s.hide() }
        });

    container(
        stack((
            label(move || caption.clone()).style(|s| s.flex_grow(1.0).min_width(0.0)),
            label(move || chords.clone()).style(|s| s.width(190.0).color(active_theme().accent)),
            label(move || source.to_string()).style(move |s| s.width(60.0).font_size(11.0).color(if customized { active_theme().warn } else { active_theme().fg_dim })),
            stack((set, clear, reset)).style(|s| s.flex_row().gap(4.0).width(190.0)),
        ))
        .style(|s| s.flex_row().items_center().width_full()),
    )
    .style(|s| {
        let t = active_theme();
        s.width_full().padding_horiz(6.0).padding_vert(4.0).border_bottom(1.0).border_color(t.border).hover(move |h| h.background(t.bg_current_line))
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use floem::reactive::{with_scope, Scope};

    fn init(dir: &Path) -> KeymapInit {
        KeymapInit {
            commands: vec![("file_save".into(), "File: Save".into()), ("find_open".into(), "Search: Find".into())],
            base: Rc::new(|| vec![Binding::new("ctrl+s", "file_save", None).unwrap()]),
            file: dir.join("keymap.toml"),
            on_saved: Rc::new(|| {}),
        }
    }

    #[test]
    fn the_user_layer_round_trips_through_the_file() {
        let dir = std::env::temp_dir().join(format!("brouillon-keymap-ui-{}", std::process::id()));
        let i = init(&dir);
        assert!(read_user(&i.file).is_empty(), "no file yet");
        let mut m = KeymapEditorModel::new(i.commands.clone(), (i.base)(), Vec::new());
        m.assign_force("find_open", "ctrl+s");
        write_user(&i.file, m.user_bindings()).unwrap();
        let on_disk = std::fs::read_to_string(&i.file).unwrap();
        assert!(on_disk.contains("chord = \"ctrl+s\"") && on_disk.contains("command = \"find_open\""), "{on_disk}");
        let back = KeymapEditorModel::new(i.commands.clone(), (i.base)(), read_user(&i.file));
        assert_eq!(back.rows("find")[0].chords, vec!["ctrl+s"]);
        assert!(back.rows("save")[0].chords.is_empty());
        // Resetting empties the file's bindings again.
        let mut m = back;
        m.reset("file_save");
        write_user(&i.file, m.user_bindings()).unwrap();
        assert!(read_user(&i.file).is_empty());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn overlay_builds_open_and_closed() {
        let scope = Scope::new();
        with_scope(scope, || {
            let dir = std::env::temp_dir().join("brouillon-keymap-ui-build");
            let v = create_rw_signal(true);
            let _ = shortcuts_overlay(v, init(&dir));
            v.set(false);
            let _ = shortcuts_overlay(v, init(&dir));
        });
        scope.dispose();
    }
}
