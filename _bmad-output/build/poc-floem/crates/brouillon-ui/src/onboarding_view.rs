//! First-run screens, the settings import dialog and the About page.
//!
//! Everything that decides something is pure and unit-tested (`act_for`, `WizardState::dispatch`,
//! `ImportPreview`); the views only draw it. The host (main.rs) supplies the callbacks that touch
//! the disk, so this crate stays free of file-format knowledge.
//!
//! Keyboard: the first-run screens are driven from the host's key handler through
//! [`handle_key`] (arrows / Enter / letters, all shown on the buttons); the import dialog has a
//! text field (Enter scans, Ctrl+Enter applies); Esc closes About and Import, and skips the
//! first-run screens.

use std::path::{Path, PathBuf};
use std::rc::Rc;

use floem::event::{Event, EventListener, EventPropagation};
use floem::keyboard::{Key, NamedKey};
use floem::reactive::{create_rw_signal, RwSignal, SignalGet, SignalUpdate};
use floem::style::CursorStyle;
use floem::views::{container, dyn_container, dyn_stack, empty, label, scroll, stack, text_input, Decorators};
use floem::{AnyView, IntoView};

use crate::theme_ctx::active_theme;
use crate::ui_kit::{button_style, field_style, modal};

// ---------------------------------------------------------------- import model

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    Imported,
    Approximated,
    Skipped,
}

impl Status {
    pub fn tag(self) -> &'static str {
        match self {
            Status::Imported => "imported",
            Status::Approximated => "approximated",
            Status::Skipped => "skipped",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Row {
    pub status: Status,
    pub category: String,
    pub name: String,
    pub detail: String,
}

/// Result of a dry run, as the dialog shows it.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ImportPreview {
    pub rows: Vec<Row>,
    /// Files the import would write.
    pub files: usize,
    /// Source files deliberately never opened (credentials).
    pub unopened: Vec<String>,
}

/// Rows drawn at most; the totals always count every row.
pub const MAX_ROWS: usize = 400;

impl ImportPreview {
    pub fn count(&self, s: Status) -> usize {
        self.rows.iter().filter(|r| r.status == s).count()
    }

    pub fn summary(&self) -> String {
        format!(
            "{} imported, {} approximated, {} skipped; {} file(s) would be written.",
            self.count(Status::Imported),
            self.count(Status::Approximated),
            self.count(Status::Skipped),
            self.files
        )
    }

    /// Rows to draw: imported first, then approximated, then skipped; capped. Second value = rows not drawn.
    pub fn shown(&self) -> (Vec<Row>, usize) {
        let mut out: Vec<Row> = Vec::new();
        for st in [Status::Imported, Status::Approximated, Status::Skipped] {
            out.extend(self.rows.iter().filter(|r| r.status == st).cloned());
        }
        let hidden = out.len().saturating_sub(MAX_ROWS);
        out.truncate(MAX_ROWS);
        (out, hidden)
    }
}

/// One editor the import knows, with where its settings were found (read-only look).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EditorEntry {
    pub id: String,
    pub label: String,
    /// Usual settings folder, shown as a hint.
    pub hint: String,
    pub dir: Option<PathBuf>,
    /// Items a dry run would carry over (0 when not found).
    pub count: usize,
}

impl EditorEntry {
    /// Button caption: name and, when found, how many items.
    pub fn caption(&self) -> String {
        match &self.dir {
            Some(_) => format!("{} ({} found)", self.label, self.count),
            None => format!("{} (not found)", self.label),
        }
    }
}

/// The editor to select when the dialog opens: the first one found, else the first one.
pub fn initial_editor(entries: &[EditorEntry]) -> usize {
    entries.iter().position(|e| e.dir.is_some()).unwrap_or(0)
}

/// First-run line about the detected editors (works for none, one or several).
pub fn found_summary(entries: &[EditorEntry]) -> String {
    let found: Vec<String> = entries.iter().filter(|e| e.dir.is_some()).map(|e| format!("{} ({} items)", e.label, e.count)).collect();
    if found.is_empty() {
        let all: Vec<&str> = entries.iter().map(|e| e.label.as_str()).collect();
        format!("No settings of another editor were found ({}). You can still point to a folder, or skip this: Tools menu > Import Settings from Another Editor... does it later.", all.join(", "))
    } else {
        format!("Found settings of: {}. Review what would be imported (nothing is written until you press Apply).", found.join(", "))
    }
}

/// What the import dialog needs from the host.
#[derive(Clone)]
pub struct ImportHost {
    /// Every known editor, with its settings folder when it was found.
    pub detect: Rc<dyn Fn() -> Vec<EditorEntry>>,
    /// Dry run for the editor with this id: read the folder, write nothing.
    pub preview: Rc<dyn Fn(&str, &Path) -> Result<ImportPreview, String>>,
    /// Write the import and reload what can be reloaded. Returns the message to show.
    pub apply: Rc<dyn Fn(&str, &Path) -> Result<String, String>>,
    /// Native folder picker (`None` = cancelled or unavailable).
    pub pick_folder: Rc<dyn Fn() -> Option<PathBuf>>,
}

fn status_color(s: Status) -> floem::peniko::Color {
    let t = active_theme();
    match s {
        Status::Imported => t.accent,
        Status::Approximated => t.fg,
        Status::Skipped => t.fg_dim,
    }
}

fn button(text: impl Fn() -> String + 'static, selected: impl Fn() -> bool + 'static, on: impl Fn() + 'static) -> AnyView {
    label(text).on_click_stop(move |_| on()).style(move |s| button_style(s, selected()).flex_shrink(0.0)).into_any()
}

fn dim(text: impl Fn() -> String + 'static) -> impl IntoView {
    label(text).style(|s| s.font_size(11.0).color(active_theme().fg_dim).max_width(780.0))
}

/// "Import settings from another editor..." dialog.
pub fn import_overlay(visible: RwSignal<bool>, host: ImportHost) -> impl IntoView {
    modal(visible, "Import settings from another editor", 860.0, 600.0, move || import_body(host.clone()))
}

fn import_body(host: ImportHost) -> AnyView {
    let entries = (host.detect)();
    let first = initial_editor(&entries);
    let sel = create_rw_signal(first);
    let path = create_rw_signal(entries.get(first).and_then(|e| e.dir.as_ref()).map(|p| p.display().to_string()).unwrap_or_default());
    let detected_first = entries.get(first).is_some_and(|e| e.dir.is_some());
    let ids: Rc<Vec<EditorEntry>> = Rc::new(entries);
    let preview: RwSignal<Option<Result<ImportPreview, String>>> = create_rw_signal(None);
    let message = create_rw_signal(String::new());
    let applied = create_rw_signal(false);

    let scan: Rc<dyn Fn()> = {
        let (host, ids) = (host.clone(), ids.clone());
        Rc::new(move || {
            applied.set(false);
            let p = path.get_untracked();
            let Some(ed) = ids.get(sel.get_untracked()) else { return };
            if p.trim().is_empty() {
                preview.set(Some(Err(format!("Type or pick the settings folder of {} (usually {}).", ed.label, ed.hint))));
                return;
            }
            message.set(String::new());
            preview.set(Some((host.preview)(&ed.id, Path::new(p.trim()))));
        })
    };
    let apply: Rc<dyn Fn()> = {
        let (host, ids) = (host.clone(), ids.clone());
        Rc::new(move || {
            if applied.get_untracked() || !matches!(preview.get_untracked(), Some(Ok(_))) {
                return;
            }
            let p = path.get_untracked();
            let Some(ed) = ids.get(sel.get_untracked()) else { return };
            match (host.apply)(&ed.id, Path::new(p.trim())) {
                Ok(m) => {
                    applied.set(true);
                    message.set(m);
                }
                Err(e) => message.set(format!("Not applied: {e}")),
            }
        })
    };
    if detected_first {
        scan();
    }

    let field = {
        let (scan, apply) = (scan.clone(), apply.clone());
        text_input(path)
            .placeholder("Settings folder of the selected editor")
            .request_focus(|| {})
            .style(|s| field_style(s).flex_grow(1.0).min_width(0.0))
            .on_event(EventListener::KeyDown, move |e| {
                if let Event::KeyDown(ke) = e {
                    if ke.key.logical_key == Key::Named(NamedKey::Enter) {
                        if ke.modifiers.control() {
                            apply();
                        } else {
                            scan();
                        }
                        return EventPropagation::Stop;
                    }
                }
                EventPropagation::Continue
            })
    };
    let browse = {
        let (host, scan) = (host.clone(), scan.clone());
        button(
            || "Browse...".into(),
            || false,
            move || {
                if let Some(p) = (host.pick_folder)() {
                    path.set(p.display().to_string());
                    scan();
                }
            },
        )
    };
    let scan_btn = {
        let scan = scan.clone();
        button(|| "Scan (dry run)".into(), || false, move || scan())
    };
    let editor_buttons: Vec<AnyView> = ids
        .iter()
        .enumerate()
        .map(|(i, e)| {
            let (cap, dir, scan) = (e.caption(), e.dir.clone(), scan.clone());
            button(
                move || cap.clone(),
                move || sel.get() == i,
                move || {
                    sel.set(i);
                    path.set(dir.as_ref().map(|p| p.display().to_string()).unwrap_or_default());
                    preview.set(None);
                    applied.set(false);
                    message.set(String::new());
                    if dir.is_some() {
                        scan();
                    }
                },
            )
        })
        .collect();
    let picker = floem::views::stack_from_iter(editor_buttons).style(|s| s.flex_row().gap(6.0));
    let top = stack((field, browse, scan_btn)).style(|s| s.flex_row().items_center().gap(6.0).width_full());

    let summary = label(move || match preview.get() {
        Some(Ok(p)) => p.summary(),
        Some(Err(e)) => e,
        None => "Nothing scanned yet.".to_string(),
    })
    .style(|s| s.font_size(12.0).color(active_theme().accent).max_width(800.0));

    let list = dyn_stack(
        move || match preview.get() {
            Some(Ok(p)) => {
                let (rows, hidden) = p.shown();
                let mut v: Vec<(usize, Option<Row>, usize)> = rows.into_iter().enumerate().map(|(i, r)| (i, Some(r), 0)).collect();
                if hidden > 0 {
                    v.push((usize::MAX, None, hidden));
                }
                v
            }
            _ => Vec::new(),
        },
        |(i, _, _)| *i,
        |(_, row, hidden)| match row {
            Some(r) => {
                let st = r.status;
                stack((
                    label(move || format!("[{}]", st.tag())).style(move |s| s.font_size(11.0).color(status_color(st)).width(92.0).flex_shrink(0.0)),
                    label(move || format!("{}: {} - {}", r.category, r.name, r.detail)).style(|s| s.font_size(11.0).min_width(0.0).flex_grow(1.0)),
                ))
                .style(|s| s.flex_row().gap(6.0).width_full())
                .into_any()
            }
            None => label(move || format!("... and {hidden} more row(s) not drawn (the totals above count them)")).style(|s| s.font_size(11.0).color(active_theme().fg_dim)).into_any(),
        },
    )
    .style(|s| s.flex_col().gap(2.0).width_full());
    let list = scroll(list).style(|s| s.flex_grow(1.0).min_height(0.0).width_full().padding_right(8.0));

    let apply_btn = {
        let apply = apply.clone();
        label(move || if applied.get() { "Applied".to_string() } else { "Apply import (Ctrl+Enter)".to_string() })
            .on_click_stop(move |_| apply())
            .style(move |s| {
                let ok = matches!(preview.get(), Some(Ok(_))) && !applied.get();
                let s = button_style(s, ok).flex_shrink(0.0);
                if ok { s } else { s.cursor(CursorStyle::Default) }
            })
    };
    let footer = stack((
        label(move || message.get()).style(|s| s.font_size(12.0).color(active_theme().accent).flex_grow(1.0).min_width(0.0).max_width(640.0)),
        apply_btn,
    ))
    .style(|s| s.flex_row().items_center().gap(8.0).width_full());

    stack((
        dim(|| "Reads the settings folder of the editor you pick and never changes it. Scanning writes nothing; Apply writes into your Brouillon settings folder (every file it replaces is first copied to .pre-import.bak). Secret keys and the licence file are never read.".to_string()),
        picker,
        top,
        summary,
        list,
        footer,
    ))
    .style(|s| s.flex_col().gap(8.0).flex_grow(1.0).min_height(0.0).width_full())
    .into_any()
}

// ---------------------------------------------------------------- first run

pub const SCREENS: usize = 3;

/// Answers collected by the first-run screens.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FirstRunChoices {
    pub classic: bool,
    /// "dark" | "light" | "system"
    pub theme: String,
    pub updates_check: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Act {
    Next,
    Back,
    Skip,
    Classic(bool),
    Theme(&'static str),
    ToggleUpdates,
    Import,
    TryAgent,
}

/// Key to action for screen `screen` (0-based). Pure.
pub fn act_for(screen: usize, key: &Key) -> Option<Act> {
    let ch = |k: &Key| match k {
        Key::Character(s) => s.chars().next().map(|c| c.to_ascii_lowercase()),
        _ => None,
    };
    match key {
        Key::Named(NamedKey::Enter | NamedKey::ArrowRight) => return Some(Act::Next),
        Key::Named(NamedKey::ArrowLeft | NamedKey::Backspace) => return Some(Act::Back),
        Key::Named(NamedKey::Escape) => return Some(Act::Skip),
        _ => {}
    }
    Some(match (screen, ch(key)?) {
        (_, 'n') => Act::Next,
        (_, 'b') => Act::Back,
        (0, '1' | 'c') => Act::Classic(true),
        (0, '2' | 'f') => Act::Classic(false),
        (0, 'd') => Act::Theme("dark"),
        (0, 'l') => Act::Theme("light"),
        (0, 's') => Act::Theme("system"),
        (1, 'i') => Act::Import,
        (1, 's') => Act::Next,
        (2, 'u') => Act::ToggleUpdates,
        (2, 'a') => Act::TryAgent,
        _ => return None,
    })
}

/// Host side of the first-run screens.
#[derive(Clone)]
pub struct WizardHost {
    pub import: ImportHost,
    /// Show this theme mode right now (the choice is saved by `finish`).
    pub preview_theme: Rc<dyn Fn(&str)>,
    /// Save the answers and mark the first run done.
    pub finish: Rc<dyn Fn(&FirstRunChoices)>,
    /// Open the import dialog on top.
    pub open_import: Rc<dyn Fn()>,
    /// Open Preferences > Connect with the Claude Code snippet highlighted.
    pub try_agent: Rc<dyn Fn()>,
}

#[derive(Clone, Copy)]
pub struct WizardState {
    pub visible: RwSignal<bool>,
    pub screen: RwSignal<usize>,
    pub classic: RwSignal<bool>,
    pub theme: RwSignal<&'static str>,
    pub updates: RwSignal<bool>,
}

impl WizardState {
    /// `theme` is the current mode so the screen starts on what is already shown.
    pub fn new(visible: RwSignal<bool>, theme: &'static str) -> Self {
        Self { visible, screen: create_rw_signal(0), classic: create_rw_signal(false), theme: create_rw_signal(theme), updates: create_rw_signal(true) }
    }

    pub fn choices(&self) -> FirstRunChoices {
        FirstRunChoices { classic: self.classic.get_untracked(), theme: self.theme.get_untracked().to_string(), updates_check: self.updates.get_untracked() }
    }

    fn finish(&self, host: &WizardHost) {
        (host.finish)(&self.choices());
        self.visible.set(false);
    }

    pub fn dispatch(&self, act: Act, host: &WizardHost) {
        let screen = self.screen.get_untracked();
        match act {
            Act::Next if screen + 1 >= SCREENS => self.finish(host),
            Act::Next => self.screen.set(screen + 1),
            Act::Back => self.screen.set(screen.saturating_sub(1)),
            // Skip keeps the answers given so far; updates stay on unless turned off.
            Act::Skip => self.finish(host),
            Act::Classic(c) => self.classic.set(c),
            Act::Theme(t) => {
                self.theme.set(t);
                (host.preview_theme)(t);
            }
            Act::ToggleUpdates => self.updates.update(|u| *u = !*u),
            Act::Import => (host.open_import)(),
            Act::TryAgent => {
                self.finish(host);
                (host.try_agent)();
            }
        }
    }
}

/// Route a key press to the first-run screens. `true` = consumed.
pub fn handle_key(state: &WizardState, host: &WizardHost, key: &Key) -> bool {
    if !state.visible.get_untracked() {
        return false;
    }
    if let Some(act) = act_for(state.screen.get_untracked(), key) {
        state.dispatch(act, host);
    }
    true // the screens own the keyboard while they are up
}

fn title(text: &'static str) -> impl IntoView {
    label(move || text.to_string()).style(|s| s.font_bold().font_size(16.0).color(active_theme().accent))
}

fn para(text: &'static str) -> impl IntoView {
    label(move || text.to_string()).style(|s| s.font_size(12.0).max_width(640.0))
}

/// The three first-run screens.
pub fn firstrun_overlay(state: WizardState, host: WizardHost) -> impl IntoView {
    let visible = state.visible;
    dyn_container(
        move || visible.get(),
        move |show| {
            if !show {
                return empty().into_any();
            }
            let host = host.clone();
            let body = {
                let host = host.clone();
                dyn_container(move || state.screen.get(), move |n| screen_view(state, host.clone(), n))
            };
            let nav = {
                let (h1, h2, h3) = (host.clone(), host.clone(), host.clone());
                stack((
                    button(|| "Skip setup (Esc)".into(), || false, move || state.dispatch(Act::Skip, &h1)),
                    empty().style(|s| s.flex_grow(1.0)),
                    label(move || format!("Step {} of {SCREENS}", state.screen.get() + 1)).style(|s| s.font_size(11.0).color(active_theme().fg_dim)),
                    button(|| "Back (Left)".into(), || false, move || state.dispatch(Act::Back, &h2)).style(move |s| if state.screen.get() == 0 { s.hide() } else { s }),
                    button(
                        move || if state.screen.get() + 1 >= SCREENS { "Finish (Enter)".to_string() } else { "Next (Enter)".to_string() },
                        || true,
                        move || state.dispatch(Act::Next, &h3),
                    ),
                ))
                .style(|s| s.flex_row().items_center().gap(8.0).width_full())
            };
            let card = stack((
                label(|| "Welcome to Brouillon".to_string()).style(|s| s.font_size(12.0).color(active_theme().fg_dim)),
                container(body).style(|s| s.flex_grow(1.0).min_height(0.0).width_full()),
                nav,
            ))
            .style(|s| {
                let t = active_theme();
                s.flex_col().gap(10.0).padding(16.0).width(700.0).height(380.0).max_height_pct(92.0).background(t.bg_chrome).color(t.fg).border(1.0).border_color(t.accent).border_radius(4.0)
            })
            .on_click_stop(|_| {});
            container(card)
                .style(|s| s.size_full().flex_col().items_center().padding_top(60.0).background(active_theme().bg.multiply_alpha(0.7)))
                .into_any()
        },
    )
    .style(move |s| if visible.get() { s.absolute().inset_left(0.0).inset_top(0.0).size_full() } else { s.display(floem::style::Display::None) })
}

fn choice_row(items: Vec<(String, AnyView)>) -> AnyView {
    let rows: Vec<AnyView> = items
        .into_iter()
        .map(|(name, buttons)| {
            stack((label(move || name.clone()).style(|s| s.font_size(12.0).font_bold().width(90.0).flex_shrink(0.0)), buttons)).style(|s| s.flex_row().items_center().gap(8.0)).into_any()
        })
        .collect();
    floem::views::stack_from_iter(rows).style(|s| s.flex_col().gap(10.0)).into_any()
}

fn screen_view(state: WizardState, host: WizardHost, n: usize) -> AnyView {
    match n {
        0 => {
            let (h1, h2, h3, h4, h5, h6) = (host.clone(), host.clone(), host.clone(), host.clone(), host.clone(), host.clone());
            let keymap = stack((
                button(|| "Classic, EditPlus-style (C)".into(), move || state.classic.get(), move || state.dispatch(Act::Classic(true), &h1)),
                button(|| "Default (F)".into(), move || !state.classic.get(), move || state.dispatch(Act::Classic(false), &h2)),
            ))
            .style(|s| s.flex_row().gap(6.0))
            .into_any();
            let theme = stack((
                button(|| "Dark (D)".into(), move || state.theme.get() == "dark", move || state.dispatch(Act::Theme("dark"), &h3)),
                button(|| "Light (L)".into(), move || state.theme.get() == "light", move || state.dispatch(Act::Theme("light"), &h4)),
                button(|| "Follow system (S)".into(), move || state.theme.get() == "system", move || state.dispatch(Act::Theme("system"), &h5)),
            ))
            .style(|s| s.flex_row().gap(6.0))
            .into_any();
            let _ = h6;
            stack((
                title("Keyboard and look"),
                para("Pick the shortcuts you know. Classic keeps the EditPlus chords (for example Ctrl+F11 for Go to Function); you can change any of them later in Preferences."),
                choice_row(vec![("Shortcuts".into(), keymap), ("Theme".into(), theme)]),
                para("Nothing here is final: Preferences (Ctrl+,) has the same choices."),
            ))
            .style(|s| s.flex_col().gap(12.0))
            .into_any()
        }
        1 => {
            let h = host.clone();
            let msg: String = found_summary(&(host.import.detect)());
            stack((
                title("Coming from another editor?"),
                label(move || msg.clone()).style(|s| s.font_size(12.0).max_width(640.0)),
                stack((
                    button(|| "Review and import... (I)".into(), || true, move || state.dispatch(Act::Import, &h)),
                    button(|| "Skip (S)".into(), || false, {
                        let h = host.clone();
                        move || {
                            let _ = &h;
                            state.screen.set(2)
                        }
                    }),
                ))
                .style(|s| s.flex_row().gap(8.0)),
                para("Settings, shortcuts, user tools, recent files, syntax files, auto-completion, cliptext and templates are carried over; every item is listed as imported, approximated or skipped, with the reason."),
            ))
            .style(|s| s.flex_col().gap(12.0))
            .into_any()
        }
        _ => {
            let (h1, h2) = (host.clone(), host);
            stack((
                title("Privacy"),
                para("Brouillon has no telemetry. It never sends your documents, file names or usage anywhere, and it has no account."),
                para("The only request it can make is an optional update check: one small signed file from brouillon.rochetta.fr, with no identifier, no version and no cookie. It is off until you turn it on, and starts at the next launch. When on, a newer signed version is also downloaded and installed by itself, but only when nothing is unsaved (Settings > Updates turns that off)."),
                button(
                    move || if state.updates.get() { "[x] Check for updates (U)".to_string() } else { "[ ] Check for updates (U): off".to_string() },
                    move || state.updates.get(),
                    move || state.dispatch(Act::ToggleUpdates, &h1),
                ),
                para("Optional: let an AI program such as Claude Code read your open documents and propose edits, each one for you to accept or reject. It is off by default and listens only on this computer."),
                button(|| "Try an agent... (A)".into(), || false, move || state.dispatch(Act::TryAgent, &h2)),
            ))
            .style(|s| s.flex_col().gap(10.0).items_start())
            .into_any()
        }
    }
}

// ---------------------------------------------------------------- about

/// About page: `rows` are (label, value) pairs, `notes` plain lines below.
pub fn about_overlay(visible: RwSignal<bool>, rows: Vec<(String, String)>, notes: Vec<String>, copy: Rc<dyn Fn(String)>) -> impl IntoView {
    modal(visible, "About Brouillon", 560.0, 420.0, move || {
        let table: Vec<AnyView> = rows
            .iter()
            .cloned()
            .map(|(k, v)| {
                stack((
                    label(move || k.clone()).style(|s| s.font_size(12.0).color(active_theme().fg_dim).width(110.0).flex_shrink(0.0)),
                    label(move || v.clone()).style(|s| s.font_size(12.0).min_width(0.0)),
                ))
                .style(|s| s.flex_row().gap(8.0))
                .into_any()
            })
            .collect();
        let notes: Vec<AnyView> = notes.iter().cloned().map(|n| label(move || n.clone()).style(|s| s.font_size(12.0).max_width(520.0)).into_any()).collect();
        let text_for_copy = rows.iter().map(|(k, v)| format!("{k}: {v}")).collect::<Vec<_>>().join("\n");
        let copy = copy.clone();
        stack((
            label(|| "Brouillon".to_string()).style(|s| s.font_bold().font_size(20.0).color(active_theme().accent)),
            floem::views::stack_from_iter(table).style(|s| s.flex_col().gap(4.0)),
            floem::views::stack_from_iter(notes).style(|s| s.flex_col().gap(6.0).margin_top(8.0)),
            button(|| "Copy version info".into(), || false, move || copy(text_for_copy.clone())),
        ))
        .style(|s| s.flex_col().gap(10.0).items_start())
        .into_any()
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use floem::reactive::{with_scope, Scope};
    use std::cell::RefCell;

    fn run<F: FnOnce() + 'static>(f: F) {
        let scope = Scope::new();
        with_scope(scope, f);
        scope.dispose();
    }

    fn c(s: &str) -> Key {
        Key::Character(s.into())
    }

    #[derive(Default)]
    struct Log(RefCell<Vec<String>>);

    fn host(log: Rc<Log>) -> WizardHost {
        let l = |tag: &'static str| {
            let log = log.clone();
            move || log.0.borrow_mut().push(tag.to_string())
        };
        let (l1, l2, l3) = (l("import"), l("agent"), log.clone());
        let l4 = log.clone();
        WizardHost {
            import: ImportHost {
                detect: Rc::new(Vec::new),
                preview: Rc::new(|_, _| Ok(ImportPreview::default())),
                apply: Rc::new(|_, _| Ok(String::new())),
                pick_folder: Rc::new(|| None),
            },
            preview_theme: Rc::new(move |t| l3.0.borrow_mut().push(format!("theme:{t}"))),
            finish: Rc::new(move |ch| l4.0.borrow_mut().push(format!("finish:{}:{}:{}", ch.classic, ch.theme, ch.updates_check))),
            open_import: Rc::new(l1),
            try_agent: Rc::new(l2),
        }
    }

    fn entry(id: &str, found: Option<usize>) -> EditorEntry {
        EditorEntry { id: id.into(), label: id.to_uppercase(), hint: "h".into(), dir: found.map(|_| PathBuf::from("x")), count: found.unwrap_or(0) }
    }

    #[test]
    fn the_dialog_copes_with_zero_one_and_several_found_editors() {
        let none = vec![entry("a", None), entry("b", None)];
        assert_eq!(initial_editor(&none), 0);
        assert!(found_summary(&none).starts_with("No settings of another editor were found (A, B)"));
        assert_eq!(none[0].caption(), "A (not found)");
        let one = vec![entry("a", None), entry("b", Some(4))];
        assert_eq!(initial_editor(&one), 1);
        assert_eq!(found_summary(&one), "Found settings of: B (4 items). Review what would be imported (nothing is written until you press Apply).");
        let many = vec![entry("a", Some(2)), entry("b", Some(4))];
        assert_eq!((initial_editor(&many), many[1].caption()), (0, "B (4 found)".to_string()));
        assert!(found_summary(&many).contains("A (2 items), B (4 items)"));
        assert_eq!(initial_editor(&[]), 0);
    }

    #[test]
    fn keys_map_to_actions_per_screen() {
        assert_eq!(act_for(0, &Key::Named(NamedKey::Enter)), Some(Act::Next));
        assert_eq!(act_for(1, &Key::Named(NamedKey::ArrowLeft)), Some(Act::Back));
        assert_eq!(act_for(2, &Key::Named(NamedKey::Escape)), Some(Act::Skip));
        assert_eq!(act_for(0, &c("1")), Some(Act::Classic(true)));
        assert_eq!(act_for(0, &c("2")), Some(Act::Classic(false)));
        assert_eq!((act_for(0, &c("C")), act_for(0, &c("f"))), (Some(Act::Classic(true)), Some(Act::Classic(false))), "letters work on AZERTY, where 1 and 2 need Shift");
        assert_eq!(act_for(0, &c("L")), Some(Act::Theme("light")));
        assert_eq!(act_for(1, &c("i")), Some(Act::Import));
        assert_eq!(act_for(2, &c("u")), Some(Act::ToggleUpdates));
        assert_eq!(act_for(2, &c("a")), Some(Act::TryAgent));
        // letters of another screen do nothing
        assert_eq!(act_for(2, &c("l")), None);
        assert_eq!(act_for(0, &c("u")), None);
    }

    #[test]
    fn three_screens_then_finish_with_the_answers() {
        run(|| {
            let log = Rc::new(Log::default());
            let h = host(log.clone());
            let st = WizardState::new(create_rw_signal(true), "dark");
            assert!(st.updates.get_untracked(), "updates start on");
            assert!(handle_key(&st, &h, &c("1")));
            assert!(handle_key(&st, &h, &c("l")));
            assert!(handle_key(&st, &h, &Key::Named(NamedKey::Enter)));
            assert_eq!(st.screen.get_untracked(), 1);
            assert!(handle_key(&st, &h, &c("i")));
            assert!(handle_key(&st, &h, &Key::Named(NamedKey::ArrowRight)));
            assert_eq!(st.screen.get_untracked(), 2);
            assert!(handle_key(&st, &h, &c("u"))); // toggles the default-on check off
            assert!(!st.updates.get_untracked());
            assert!(handle_key(&st, &h, &Key::Named(NamedKey::ArrowLeft)));
            assert_eq!(st.screen.get_untracked(), 1);
            assert!(handle_key(&st, &h, &Key::Named(NamedKey::Enter)));
            assert!(handle_key(&st, &h, &Key::Named(NamedKey::Enter)));
            assert!(!st.visible.get_untracked());
            assert_eq!(*log.0.borrow(), vec!["theme:light", "import", "finish:true:light:false"]);
            // closed: keys are not consumed any more
            assert!(!handle_key(&st, &h, &c("1")));
        });
    }

    #[test]
    fn skip_keeps_updates_on_and_try_agent_finishes_first() {
        run(|| {
            let log = Rc::new(Log::default());
            let h = host(log.clone());
            let st = WizardState::new(create_rw_signal(true), "dark");
            handle_key(&st, &h, &Key::Named(NamedKey::Escape));
            assert_eq!(*log.0.borrow(), vec!["finish:false:dark:true"]);
            let log = Rc::new(Log::default());
            let h = host(log.clone());
            let st = WizardState::new(create_rw_signal(true), "dark");
            st.screen.set(2);
            handle_key(&st, &h, &c("a"));
            assert_eq!(*log.0.borrow(), vec!["finish:false:dark:true", "agent"]);
        });
    }

    #[test]
    fn preview_counts_orders_and_caps_rows() {
        let row = |st, n: usize| Row { status: st, category: "c".into(), name: format!("n{n}"), detail: "d".into() };
        let mut rows: Vec<Row> = (0..MAX_ROWS + 5).map(|i| row(Status::Skipped, i)).collect();
        rows.push(row(Status::Imported, 9999));
        rows.push(row(Status::Approximated, 9998));
        let p = ImportPreview { rows, files: 3, unopened: vec![] };
        assert_eq!(p.count(Status::Skipped), MAX_ROWS + 5);
        assert!(p.summary().starts_with("1 imported, 1 approximated, 405 skipped; 3 file(s)"), "{}", p.summary());
        let (shown, hidden) = p.shown();
        assert_eq!(shown.len(), MAX_ROWS);
        assert_eq!(hidden, 7);
        assert_eq!((shown[0].status, shown[1].status), (Status::Imported, Status::Approximated));
    }

    #[test]
    fn views_build() {
        run(|| {
            let log = Rc::new(Log::default());
            let h = host(log);
            let st = WizardState::new(create_rw_signal(true), "dark");
            for n in 0..SCREENS {
                st.screen.set(n);
                let _ = screen_view(st, h.clone(), n);
            }
            let _ = firstrun_overlay(st, h.clone());
            let _ = import_overlay(create_rw_signal(false), h.import.clone());
            let _ = about_overlay(create_rw_signal(false), vec![("Version".into(), "1".into())], vec!["x".into()], Rc::new(|_| {}));
            let _ = import_body(h.import);
        });
    }
}
