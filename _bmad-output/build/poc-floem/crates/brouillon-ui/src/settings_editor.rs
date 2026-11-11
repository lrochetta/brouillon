//! Settings editor (Preferences): the known settings by category, with the
//! right control for each, their default, a per-setting reset, and writes to
//! `settings.toml` that keep comments and unknown keys.
//!
//! The model is pure (`SettingsModel`); `settings_overlay` is the Floem view.
//! Only settings the program really reads are listed.

use std::path::{Path, PathBuf};
use std::rc::Rc;

use brouillon_core::settings::{remove_key, set_raw_key, user_values, Layer, Settings};
use floem::event::{Event, EventListener, EventPropagation};
use floem::keyboard::{Key, NamedKey};
use floem::reactive::{create_rw_signal, RwSignal, SignalGet, SignalUpdate, SignalWith};
use floem::views::{container, dyn_stack, empty, label, scroll, stack, stack_from_iter, text_input, Decorators};
use floem::{AnyView, IntoView};
use toml::Value;

use crate::theme_ctx::active_theme;
use crate::link_view::{connect_tab, ConnectHost};
use crate::ui_kit::{button_style, field_style, modal};

/// How the value is edited.
#[derive(Debug, Clone, PartialEq)]
pub enum Kind {
    Bool,
    Int { min: i64, max: i64 },
    Text,
    /// `(stored value, label)`
    Choice(Vec<(String, String)>),
}

/// When a change takes effect.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Effect {
    /// Applied as soon as it is saved.
    Now,
    /// Read once at startup.
    Relaunch,
}

impl Effect {
    pub fn text(self) -> &'static str {
        match self {
            Effect::Now => "applies immediately",
            Effect::Relaunch => "applies at next launch",
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Def {
    /// Dotted key, `section.key`.
    pub key: &'static str,
    pub category: &'static str,
    pub label: &'static str,
    pub help: &'static str,
    pub kind: Kind,
    pub effect: Effect,
}

impl Def {
    fn section_key(&self) -> (&str, &str) {
        self.key.split_once('.').unwrap_or(("", self.key))
    }
}

const UPDATE_TRANSPARENCY: &str = "When on, a few seconds after start (at most once per the interval set below) Brouillon downloads one small signed file from brouillon.rochetta.fr to learn whether a newer version exists. The request is a plain GET: no identifier, no cookie, no query string, no version or system details (generic User-Agent \"Brouillon-update/1\"), no document, no usage data. Off means no request at all, ever, and it is the default: nothing is sent until you turn this on.";

fn choice(items: &[(&str, &str)]) -> Kind {
    Kind::Choice(items.iter().map(|(v, l)| (v.to_string(), l.to_string())).collect())
}

/// Every setting the editor offers, in display order.
pub fn catalog() -> Vec<Def> {
    let mut themes = vec![("".to_string(), "Automatic".to_string())];
    themes.extend(brouillon_core::theme::ThemeRegistry::new().themes.iter().map(|t| (t.name.to_string(), t.name.to_string())));
    vec![
        Def {
            key: "theme.mode",
            category: "Appearance",
            label: "Theme mode",
            help: "Dark, light, or follow the operating system's light / dark switch.",
            kind: choice(&[("dark", "Dark"), ("light", "Light"), ("system", "Follow system")]),
            effect: Effect::Now,
        },
        Def {
            key: "theme.name",
            category: "Appearance",
            label: "Preferred theme",
            help: "Used when it matches the resolved mode; Automatic picks the default theme of that mode.",
            kind: Kind::Choice(themes),
            effect: Effect::Now,
        },
        Def {
            key: "render.backend",
            category: "Display",
            label: "Rendering",
            help: "CPU (default): the processor draws the window; no graphics driver needed and the smallest memory use (about 61 MB for an empty document on Windows). GPU: Direct3D 12 on Windows, the graphics card draws; about 150 MB for an empty document, and if the GPU cannot start the program falls back to the CPU. Other values (dx12, vulkan, gl, metal) can be written in settings.toml.",
            kind: choice(&[("cpu", "CPU"), ("gpu", "GPU")]),
            effect: Effect::Relaunch,
        },
        Def {
            key: "startup.document",
            category: "Startup",
            label: "Document at startup",
            help: "What opens when Brouillon starts with no file and no session to restore.",
            kind: choice(&[("welcome", "Welcome"), ("docs", "Documentation"), ("changelog", "Release notes"), ("none", "Nothing")]),
            effect: Effect::Relaunch,
        },
        Def {
            key: "startup.whats_new",
            category: "Startup",
            label: "Show release notes after an update",
            help: "Opens the release notes once, the first time the program starts after being updated.",
            kind: Kind::Bool,
            effect: Effect::Relaunch,
        },
        Def {
            key: "updates.check",
            category: "Updates",
            label: "Check for updates",
            help: UPDATE_TRANSPARENCY,
            kind: Kind::Bool,
            effect: Effect::Relaunch,
        },
        Def {
            key: "updates.auto_install",
            category: "Updates",
            label: "Download and install updates automatically",
            help: "A newer signed version is downloaded in the background and checked (signature, size, SHA-256). It then starts by itself only when no document has unsaved changes (unsaved work is journaled first); otherwise a banner waits for your choice or for you to quit. Off: you are only told that an update exists. Portable copies never install anything.",
            kind: Kind::Bool,
            effect: Effect::Relaunch,
        },
        Def {
            key: "updates.check_interval_hours",
            category: "Updates",
            label: "Check at most every (hours)",
            help: "Throttle: Brouillon asks for the update file at most once in this many hours. 0 means at every launch.",
            kind: Kind::Int { min: 0, max: 8760 },
            effect: Effect::Relaunch,
        },
        Def {
            key: "updates.channel",
            category: "Updates",
            label: "Update channel",
            help: "Empty accepts any. A signed manifest that names a different channel is ignored.",
            kind: Kind::Text,
            effect: Effect::Relaunch,
        },
        Def {
            key: "backup.enabled",
            category: "Files",
            label: "Keep a backup when saving",
            help: "Before a file is overwritten, the previous version is copied to the backup folder of the data directory.",
            kind: Kind::Bool,
            effect: Effect::Relaunch,
        },
        Def {
            key: "backup.max_count",
            category: "Files",
            label: "Backups kept per file",
            help: "Older backups beyond this number are deleted.",
            kind: Kind::Int { min: 0, max: 1000 },
            effect: Effect::Relaunch,
        },
        Def {
            key: "backup.max_age_days",
            category: "Files",
            label: "Backup age limit (days)",
            help: "Backups older than this are deleted.",
            kind: Kind::Int { min: 0, max: 36500 },
            effect: Effect::Relaunch,
        },
        Def {
            key: "mcp.enabled",
            category: "Agents",
            label: "Agents (MCP server)",
            help: "Lets an AI program on this computer (Claude Code or any MCP client) read your open documents and propose edits that you accept or reject. Off by default: when off, nothing listens and no file is written. See the Connect tab.",
            kind: Kind::Bool,
            effect: Effect::Now,
        },
        Def {
            key: "editor.tab_size",
            category: "Editing",
            label: "Tab width",
            help: "Width of a tab stop: the Tab key inserts spaces up to the next multiple of it (when spaces are used).",
            kind: Kind::Int { min: 1, max: 16 },
            effect: Effect::Now,
        },
        Def {
            key: "editor.indent_size",
            category: "Editing",
            label: "Indent width",
            help: "Width of one indent level added by Enter after an opening brace or colon.",
            kind: Kind::Int { min: 1, max: 16 },
            effect: Effect::Now,
        },
        Def {
            key: "editor.insert_spaces",
            category: "Editing",
            label: "Insert spaces for Tab",
            help: "On: the Tab key and the indent level use spaces. Off: they use a tab character.",
            kind: Kind::Bool,
            effect: Effect::Now,
        },
        Def {
            key: "editor.auto_indent",
            category: "Editing",
            label: "Auto-indent on Enter",
            help: "A new line starts with the indentation of the current one (and one level deeper after an opening brace).",
            kind: Kind::Bool,
            effect: Effect::Now,
        },
        Def {
            key: "editor.line_numbers",
            category: "Editing",
            label: "Show line numbers",
            help: "The line-number gutter (also View > Toggle Line Numbers).",
            kind: Kind::Bool,
            effect: Effect::Now,
        },
        Def {
            key: "ui.sidebar.visible",
            category: "Display",
            label: "Show the sidebar",
            help: "The left column with Open Files and the Browser (also the icon left of the File menu, View > Toggle Sidebar and Ctrl+B).",
            kind: Kind::Bool,
            effect: Effect::Now,
        },
        Def {
            key: "ui.tabs.position",
            category: "Display",
            label: "Tab bar position",
            help: "Top (under the menu) or bottom (just above the status bar). Also the second icon left of the File menu and View > Tabs at Top / Bottom.",
            kind: choice(&[("top", "Top"), ("bottom", "Bottom")]),
            effect: Effect::Now,
        },
        Def {
            key: "files.default_encoding",
            category: "Files",
            label: "Encoding of new documents",
            help: "Codepage id such as utf-8, windows-1252 or shift_jis. Empty = UTF-8. Files you open keep the encoding they are detected with.",
            kind: Kind::Text,
            effect: Effect::Now,
        },
        Def {
            key: "keymap.classic",
            category: "Keyboard",
            label: "Classic shortcut set",
            help: "Overlay the classic Windows-editor shortcuts (Ctrl+Tab, Ctrl+R, ...) on the defaults. Your own shortcuts always win over both.",
            kind: Kind::Bool,
            effect: Effect::Now,
        },
    ]
}

/// One displayed setting.
#[derive(Debug, Clone, PartialEq)]
pub struct Row {
    pub def: Def,
    /// Effective value.
    pub value: Value,
    /// Text for the control and for display.
    pub shown: String,
    pub is_default: bool,
    pub default_shown: String,
}

/// Header or setting line of the list.
#[derive(Debug, Clone, PartialEq)]
pub enum Item {
    Header(&'static str),
    Setting(Row),
}

#[derive(Debug, Clone)]
pub struct SettingsModel {
    text: String,
    defs: Vec<Def>,
}

fn value_text(def: &Def, v: &Value) -> String {
    let raw = match v {
        Value::String(s) => s.clone(),
        Value::Boolean(b) => if *b { "On" } else { "Off" }.to_string(),
        Value::Integer(i) => i.to_string(),
        other => other.to_string(),
    };
    match &def.kind {
        Kind::Choice(opts) => opts.iter().find(|(v, _)| *v == raw).map_or(raw, |(_, l)| l.clone()),
        _ => raw,
    }
}

impl SettingsModel {
    /// `text` is the content of the user's `settings.toml` (may be empty).
    pub fn new(text: String) -> Self {
        Self { text, defs: catalog() }
    }

    /// The file text including every change so far.
    pub fn text(&self) -> &str {
        &self.text
    }

    pub fn def(&self, key: &str) -> Option<&Def> {
        self.defs.iter().find(|d| d.key == key)
    }

    fn row(&self, def: &Def, effective: &Settings, defaults: &Settings, user: &[(String, Value)]) -> Row {
        let value = effective.get(def.key).cloned().unwrap_or(Value::String(String::new()));
        let default = defaults.get(def.key).cloned().unwrap_or(Value::String(String::new()));
        Row {
            shown: value_text(def, &value),
            default_shown: value_text(def, &default),
            is_default: !user.iter().any(|(k, _)| k == def.key),
            def: def.clone(),
            value,
        }
    }

    /// Settings matching every word of `filter` (label, help, key, category), grouped under headers.
    pub fn items(&self, filter: &str) -> Vec<Item> {
        let effective = Settings::build(&[(Layer::User, self.text.as_str())]);
        let defaults = Settings::build(&[]);
        let user = user_values(&self.text);
        let words: Vec<String> = filter.split_whitespace().map(str::to_lowercase).collect();
        let mut out: Vec<Item> = Vec::new();
        for def in &self.defs {
            let hay = format!("{} {} {} {}", def.label, def.help, def.key, def.category).to_lowercase();
            if !words.iter().all(|w| hay.contains(w)) {
                continue;
            }
            if !out.iter().any(|i| matches!(i, Item::Header(c) if *c == def.category)) {
                out.push(Item::Header(def.category));
            }
            out.push(Item::Setting(self.row(def, &effective, &defaults, &user)));
        }
        out
    }

    /// Validate `input` for `key` and write it into the text. `Err` = readable reason, nothing changed.
    pub fn set(&mut self, key: &str, input: &str) -> Result<(), String> {
        let def = self.def(key).ok_or_else(|| format!("unknown setting `{key}`"))?.clone();
        let raw = match &def.kind {
            Kind::Bool => match input.trim().to_lowercase().as_str() {
                "true" | "on" | "yes" => "true".to_string(),
                "false" | "off" | "no" => "false".to_string(),
                _ => return Err("expected On or Off".into()),
            },
            Kind::Int { min, max } => {
                let n: i64 = input.trim().parse().map_err(|_| "expected a whole number".to_string())?;
                if n < *min || n > *max {
                    return Err(format!("expected a number between {min} and {max}"));
                }
                n.to_string()
            }
            Kind::Text => Value::String(input.to_string()).to_string(),
            Kind::Choice(opts) => {
                if !opts.iter().any(|(v, _)| v == input) {
                    return Err(format!("`{input}` is not one of the choices"));
                }
                Value::String(input.to_string()).to_string()
            }
        };
        let (section, k) = def.section_key();
        self.text = set_raw_key(&self.text, section, k, &raw);
        Ok(())
    }

    /// Back to the default: the key is removed from the file (a default is not written down).
    pub fn reset(&mut self, key: &str) {
        if let Some(def) = self.def(key) {
            let (section, k) = def.section_key();
            self.text = remove_key(&self.text, section, k);
        }
    }
}

fn read_text(file: &Path) -> String {
    std::fs::read_to_string(file).unwrap_or_default()
}

/// Durable write of the settings file (creates the folder).
fn write_text(file: &Path, text: &str) -> Result<(), String> {
    if let Some(dir) = file.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    brouillon_editor_engine::durable::durable_write(file, text.as_bytes()).map_err(|e| e.to_string())
}

#[derive(Clone)]
enum Op {
    Set(String),
    Reset,
}

/// The Preferences dialog. `on_changed(key)` runs after a change was written, so the host can
/// apply the settings that take effect immediately.
pub fn settings_overlay(visible: RwSignal<bool>, file: PathBuf, on_changed: Rc<dyn Fn(&str)>, connect: Option<ConnectHost>) -> impl IntoView {
    modal(visible, "Preferences", 780.0, 560.0, move || body(file.clone(), on_changed.clone(), connect.clone()))
}

fn mcp_enabled(text: &str) -> bool {
    Settings::build(&[(Layer::User, text)]).get_bool("mcp.enabled", false)
}

fn body(file: PathBuf, on_changed: Rc<dyn Fn(&str)>, connect: Option<ConnectHost>) -> AnyView {
    let initial = read_text(&file);
    let mcp_on = create_rw_signal(mcp_enabled(&initial));
    let model = create_rw_signal(SettingsModel::new(initial));
    let filter = create_rw_signal(String::new());
    let note = create_rw_signal(String::new());
    // "settings" or "connect"; "Try an agent" asks for Connect with the Claude Code line highlighted.
    let highlight = connect.as_ref().is_some_and(|h| h.start_connect.get_untracked());
    if let Some(h) = &connect {
        h.start_connect.set(false);
    }
    let tab = create_rw_signal(if highlight { "connect" } else { "settings" });

    let commit: Rc<dyn Fn(&str, Op)> = {
        let file = file.clone();
        Rc::new(move |key: &str, op: Op| {
            // The file may have been edited by hand since the dialog opened: start from disk.
            let mut m = SettingsModel::new(read_text(&file));
            let result = match &op {
                Op::Set(v) => m.set(key, v),
                Op::Reset => {
                    m.reset(key);
                    Ok(())
                }
            };
            match result {
                Err(e) => note.set(format!("Not saved - {e}")),
                Ok(()) => match write_text(&file, m.text()) {
                    Err(e) => note.set(format!("Could not save {}: {e}", file.display())),
                    Ok(()) => {
                        let effect = m.def(key).map_or(Effect::Relaunch, |d| d.effect);
                        let verb = if matches!(op, Op::Reset) { "reset to default" } else { "saved" };
                        note.set(format!("{key}: {verb}, {}.", effect.text()));
                        mcp_on.set(mcp_enabled(m.text()));
                        model.set(m);
                        on_changed(key);
                    }
                },
            }
        })
    };

    let search = text_input(filter)
        .placeholder("Search settings...")
        .request_focus(|| {})
        .style(|s| field_style(s).width_full());

    let list = dyn_stack(
        move || model.with(|m| m.items(&filter.get())),
        |i: &Item| match i {
            Item::Header(c) => format!("h:{c}"),
            Item::Setting(r) => format!("s:{}:{}:{}", r.def.key, r.shown, r.is_default),
        },
        {
            let commit = commit.clone();
            move |item: Item| match item {
                Item::Header(c) => label(move || c.to_string())
                    .style(|s| {
                        let t = active_theme();
                        s.font_bold().font_size(13.0).color(t.accent).margin_top(10.0).padding_bottom(2.0).width_full()
                    })
                    .into_any(),
                Item::Setting(r) => setting_row(r, commit.clone()).into_any(),
            }
        },
    )
    .style(|s| s.flex_col().width_full().gap(2.0));

    let footer = label(move || {
        let n = note.get();
        if n.is_empty() {
            format!("File: {}", file.display())
        } else {
            n
        }
    })
    .style(|s| s.font_size(11.0).color(active_theme().fg_dim).width_full());

    // Tabs sit on the search row so the rows below keep their place.
    let tab_button = |id: &'static str, text: &'static str| {
        label(move || text.to_string())
            .on_click_stop(move |_| tab.set(id))
            .style(move |s| button_style(s, tab.get() == id).flex_shrink(0.0))
    };
    let top: AnyView = if connect.is_some() {
        stack((search.style(|s| s.flex_grow(1.0).min_width(0.0)), tab_button("settings", "Settings"), tab_button("connect", "Connect")))
            .style(|s| s.flex_row().items_center().gap(6.0).width_full())
            .into_any()
    } else {
        search.into_any()
    };

    let settings_view = scroll(list).style(|s| s.flex_grow(1.0).min_height(0.0).width_full().padding_right(8.0));
    let connect_view: AnyView = match connect {
        Some(host) => {
            let commit = commit.clone();
            let toggle: Rc<dyn Fn(bool)> = Rc::new(move |on| commit("mcp.enabled", Op::Set(on.to_string())));
            scroll(connect_tab(host, mcp_on, toggle, highlight)).style(|s| s.flex_grow(1.0).min_height(0.0).width_full().padding_right(8.0)).into_any()
        }
        None => empty().into_any(),
    };
    let content = stack((
        settings_view.style(move |s| if tab.get() == "settings" { s } else { s.hide() }),
        connect_view.style(move |s| if tab.get() == "connect" { s.flex_grow(1.0).min_height(0.0).width_full() } else { s.hide() }),
    ))
    .style(|s| s.flex_col().flex_grow(1.0).min_height(0.0).width_full());

    stack((top, content, footer))
    .style(|s| s.flex_col().gap(8.0).flex_grow(1.0).min_height(0.0).width_full())
    .into_any()
}

fn setting_row(r: Row, commit: Rc<dyn Fn(&str, Op)>) -> impl IntoView {
    let key = r.def.key;
    let control: AnyView = match &r.def.kind {
        Kind::Bool => {
            let on = matches!(r.value, Value::Boolean(true));
            let c = commit.clone();
            label(move || if on { "[x] On".to_string() } else { "[ ] Off".to_string() })
                .on_click_stop(move |_| c(key, Op::Set((!on).to_string())))
                .style(move |s| button_style(s, on))
                .into_any()
        }
        Kind::Choice(opts) => {
            let current = match &r.value {
                Value::String(s) => s.clone(),
                _ => String::new(),
            };
            let buttons: Vec<AnyView> = opts
                .iter()
                .map(|(v, l)| {
                    let (v, l) = (v.clone(), l.clone());
                    let selected = v == current;
                    let c = commit.clone();
                    label(move || l.clone())
                        .on_click_stop(move |_| c(key, Op::Set(v.clone())))
                        .style(move |s| button_style(s, selected))
                        .into_any()
                })
                .collect();
            stack_from_iter(buttons).style(|s| s.flex_row().flex_wrap(floem::taffy::FlexWrap::Wrap).gap(4.0)).into_any()
        }
        Kind::Int { .. } | Kind::Text => {
            let sig = create_rw_signal(match &r.value {
                Value::String(s) => s.clone(),
                other => other.to_string(),
            });
            let (c1, c2) = (commit.clone(), commit.clone());
            let field = text_input(sig)
                .style(|s| field_style(s).width(160.0))
                .on_event(EventListener::KeyDown, move |e| {
                    if let Event::KeyDown(ke) = e {
                        if ke.key.logical_key == Key::Named(NamedKey::Enter) {
                            c1(key, Op::Set(sig.get_untracked()));
                            return EventPropagation::Stop;
                        }
                    }
                    EventPropagation::Continue
                });
            let set = label(|| "Set".to_string())
                .on_click_stop(move |_| c2(key, Op::Set(sig.get_untracked())))
                .style(|s| button_style(s, false));
            stack((field, set)).style(|s| s.flex_row().items_center().gap(6.0)).into_any()
        }
    };

    let is_default = r.is_default;
    let c = commit;
    let reset = label(|| "Reset".to_string())
        .on_click_stop(move |_| c(key, Op::Reset))
        .style(move |s| {
            let s = button_style(s, false);
            if is_default { s.hide() } else { s }
        });
    let default_text = format!("Default: {}", r.default_shown);
    let effect = r.def.effect.text();
    let (title, help) = (r.def.label, r.def.help);

    let left = stack((
        label(move || title.to_string()).style(|s| s.font_bold().font_size(13.0)),
        label(move || help.to_string()).style(|s| s.font_size(11.0).color(active_theme().fg_dim).max_width(380.0)),
    ))
    .style(|s| s.flex_col().gap(2.0).width(400.0).flex_shrink(0.0));
    let right = stack((
        control,
        stack((
            label(move || default_text.clone()).style(|s| s.font_size(11.0).color(active_theme().fg_dim)),
            label(move || format!("- {effect}")).style(|s| s.font_size(11.0).color(active_theme().fg_dim)),
            reset,
        ))
        .style(|s| s.flex_row().items_center().gap(8.0)),
    ))
    .style(|s| s.flex_col().items_start().gap(4.0).flex_grow(1.0).min_width(0.0));

    container(stack((left, right)).style(|s| s.flex_row().gap(12.0).width_full()))
        .style(|s| {
            let t = active_theme();
            s.width_full().padding_vert(8.0).border_bottom(1.0).border_color(t.border)
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn model(text: &str) -> SettingsModel {
        SettingsModel::new(text.to_string())
    }

    fn row(m: &SettingsModel, key: &str) -> Row {
        m.items("")
            .into_iter()
            .find_map(|i| match i {
                Item::Setting(r) if r.def.key == key => Some(r),
                _ => None,
            })
            .unwrap_or_else(|| panic!("{key} not listed"))
    }

    #[test]
    fn every_listed_setting_is_a_known_default_of_the_right_type() {
        let defaults = Settings::build(&[]);
        for d in catalog() {
            let v = defaults.get(d.key).unwrap_or_else(|| panic!("{} has no default in DEFAULTS_TOML", d.key));
            match (&d.kind, v) {
                (Kind::Bool, Value::Boolean(_)) | (Kind::Int { .. }, Value::Integer(_)) | (Kind::Text, Value::String(_)) => {}
                (Kind::Choice(opts), Value::String(s)) => assert!(opts.iter().any(|(o, _)| o == s), "{}: default `{s}` is not a choice", d.key),
                (k, v) => panic!("{}: {k:?} does not fit default {v:?}", d.key),
            }
            assert!(d.key.contains('.'), "{}", d.key);
        }
        // No duplicates.
        let mut keys: Vec<_> = catalog().iter().map(|d| d.key).collect();
        keys.sort_unstable();
        let n = keys.len();
        keys.dedup();
        assert_eq!(keys.len(), n);
    }

    #[test]
    fn required_settings_are_present() {
        let keys: Vec<_> = catalog().iter().map(|d| d.key).collect();
        for k in ["theme.mode", "render.backend", "updates.check", "updates.auto_install", "updates.check_interval_hours", "updates.channel", "startup.document", "backup.enabled", "keymap.classic"] {
            assert!(keys.contains(&k), "{k}");
        }
        // The update toggle carries the transparency text.
        assert!(catalog().iter().find(|d| d.key == "updates.check").unwrap().help.contains("no identifier"));
        assert!(catalog().iter().find(|d| d.key == "render.backend").unwrap().help.contains("GPU"));
    }

    #[test]
    fn defaults_are_shown_until_changed_and_reset_restores_them() {
        let mut m = model("");
        let r = row(&m, "theme.mode");
        assert!(r.is_default && r.shown == "Dark" && r.default_shown == "Dark");
        m.set("theme.mode", "light").unwrap();
        let r = row(&m, "theme.mode");
        assert!(!r.is_default && r.shown == "Light" && r.default_shown == "Dark", "{r:?}");
        assert_eq!(m.text(), "[theme]\nmode = \"light\"\n");
        m.reset("theme.mode");
        assert!(row(&m, "theme.mode").is_default);
        assert!(!m.text().contains("mode"));
    }

    #[test]
    fn writing_keeps_comments_and_unknown_keys() {
        let src = "# my settings\n[updates]\n# off while testing\ncheck = true\n\n[plugin_x]\nanything = 1\n";
        let mut m = model(src);
        m.set("updates.check", "false").unwrap();
        m.set("backup.max_count", "3").unwrap();
        let t = m.text();
        assert!(t.contains("# my settings") && t.contains("# off while testing") && t.contains("[plugin_x]\nanything = 1"), "{t}");
        assert!(t.contains("check = false") && t.contains("max_count = 3"));
        let s = Settings::build(&[(Layer::User, t)]);
        assert!(!s.get_bool("updates.check", true));
        assert_eq!(s.get_i64("backup.max_count", 0), 3);
        assert_eq!(s.unknown_keys, vec!["plugin_x.anything"]);
    }

    #[test]
    fn invalid_input_is_refused_and_changes_nothing() {
        let mut m = model("[backup]\nmax_count = 5\n");
        let before = m.text().to_string();
        assert!(m.set("backup.max_count", "abc").is_err());
        assert!(m.set("backup.max_count", "-1").unwrap_err().contains("between"));
        assert!(m.set("backup.max_count", "100000").is_err());
        assert!(m.set("theme.mode", "purple").is_err());
        assert!(m.set("updates.check", "maybe").is_err());
        assert!(m.set("nope.nothing", "1").is_err());
        assert_eq!(m.text(), before);
        assert!(m.set("backup.max_count", " 7 ").is_ok());
        assert_eq!(row(&m, "backup.max_count").value, Value::Integer(7));
    }

    #[test]
    fn search_filters_by_label_help_key_and_category() {
        let m = model("");
        let keys = |f: &str| -> Vec<&'static str> {
            m.items(f).into_iter().filter_map(|i| if let Item::Setting(r) = i { Some(r.def.key) } else { None }).collect()
        };
        assert!(keys("").len() >= 10);
        assert_eq!(keys("render"), vec!["render.backend"]);
        assert_eq!(keys("UPDATES").len(), 4);
        assert!(keys("signed manifest").contains(&"updates.check") || keys("signed").contains(&"updates.check"));
        assert!(keys("zzzzzz").is_empty());
        // Headers appear only for categories that still have a match.
        let h: Vec<_> = m.items("backup").into_iter().filter(|i| matches!(i, Item::Header(_))).collect();
        assert_eq!(h, vec![Item::Header("Files")]);
    }

    #[test]
    fn a_hand_written_value_outside_the_choices_is_still_shown() {
        let m = model("[render]\nbackend = \"vulkan\"\n");
        let r = row(&m, "render.backend");
        assert_eq!(r.shown, "vulkan");
        assert!(!r.is_default);
    }

    #[test]
    fn bool_toggle_round_trip() {
        let mut m = model("");
        assert_eq!(row(&m, "updates.auto_install").value, Value::Boolean(true));
        m.set("updates.auto_install", "false").unwrap();
        assert_eq!(row(&m, "updates.auto_install").shown, "Off");
        m.set("updates.auto_install", "on").unwrap();
        assert_eq!(row(&m, "updates.auto_install").shown, "On");
    }

    #[test]
    fn dialog_writes_the_file_durably() {
        let dir = std::env::temp_dir().join(format!("brouillon-settings-ui-{}", std::process::id()));
        let file = dir.join("settings.toml");
        let mut m = model("");
        m.set("theme.mode", "system").unwrap();
        write_text(&file, m.text()).unwrap();
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "[theme]\nmode = \"system\"\n");
        assert_eq!(read_text(&file), m.text());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn overlay_builds() {
        use floem::reactive::{with_scope, Scope};
        let scope = Scope::new();
        with_scope(scope, || {
            let v = create_rw_signal(true);
            let _ = settings_overlay(v, std::env::temp_dir().join("brouillon-nonexistent-settings.toml"), Rc::new(|_| {}), None);
            let host = ConnectHost { ui: crate::link_view::LinkUi::new(), snippet: Rc::new(|| "claude mcp add x".into()), copy: Rc::new(|_| {}), test: Rc::new(|| {}), start_connect: create_rw_signal(true) };
            let _ = settings_overlay(v, std::env::temp_dir().join("brouillon-nonexistent-settings.toml"), Rc::new(|_| {}), Some(host));
        });
        scope.dispose();
    }

    #[test]
    fn agents_are_off_by_default_and_the_toggle_round_trips() {
        let mut m = model("");
        let r = row(&m, "mcp.enabled");
        assert_eq!(r.value, Value::Boolean(false));
        assert!(r.is_default);
        assert!(!mcp_enabled(m.text()));
        m.set("mcp.enabled", "true").unwrap();
        assert!(mcp_enabled(m.text()));
        assert!(m.text().contains("[mcp]") && m.text().contains("enabled = true"), "{}", m.text());
        m.set("mcp.enabled", "false").unwrap();
        assert!(!mcp_enabled(m.text()));
        assert!(mcp_enabled("[mcp]\nenabled = true\n"));
        assert!(!mcp_enabled("[mcp]\nenabled = \"yes\"\n"), "garbage means off");
    }
}
