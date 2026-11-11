//! End-to-end import of a user's own EditPlus settings directory (Phase 5).
//!
//! Reads (never writes) the source folder and produces, in memory, the files of a
//! Brouillon data directory: `settings.toml`, `keymap.toml`, `tools.toml`,
//! `recent.toml`, `cliptext/`, `abbrev/`, `syntax/`, `templates/`. Every source
//! item ends up in the [`Report`] as imported, approximated or skipped, with the
//! reason. No network, no telemetry; secret keys are dropped and `reg_u.ini` is
//! never opened.
//!
//! The source layout is read from the observable structure of the files (INI
//! sections and keys, `#TITLE`/`#T=` text files); key names are matched
//! case-insensitively and ignoring spaces, so small variations between versions
//! do not matter. Anything not understood is reported, not guessed.

use crate::editplus::{decode_any, ini_to_table, is_secret_key, Report as RawReport};
use crate::hardening::{safe_label, value_has_credential, MAX_ENTRIES, MAX_LINE};
use crate::stx;
use brouillon_core::keymap;
use brouillon_core::settings::set_raw_key;
use brouillon_editor_engine::{acp, cliptext};
use std::collections::BTreeSet;
use std::fs;
use std::io;
use std::path::Path;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    /// Carried over and acted upon by Brouillon.
    Imported,
    /// Carried over in a different form, or stored but not yet read by the UI.
    Approximated,
    /// Not carried over.
    Skipped,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Item {
    pub category: &'static str,
    pub name: String,
    pub status: Status,
    pub detail: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Report {
    pub items: Vec<Item>,
    /// Source files that were deliberately not opened.
    pub unopened: Vec<String>,
}

impl Report {
    fn add(&mut self, category: &'static str, name: impl Into<String>, status: Status, detail: impl Into<String>) {
        // Names come from another program's files: scrubbed and bounded before they are shown.
        self.items.push(Item { category, name: crate::hardening::safe_label_n(&name.into(), 160), status, detail: detail.into() });
    }
    pub fn count(&self, s: Status) -> usize {
        self.items.iter().filter(|i| i.status == s).count()
    }
    pub fn of(&self, category: &str) -> Vec<&Item> {
        self.items.iter().filter(|i| i.category == category).collect()
    }

    /// Plain-text report: totals, then every item grouped by status.
    pub fn render(&self, dry_run: bool, dest: &str, files: &[String]) -> String {
        let mut s = String::new();
        s.push_str(if dry_run { "EditPlus import (dry run, nothing written)\n" } else { "EditPlus import\n" });
        s.push_str(&format!(
            "  imported: {}   approximated: {}   skipped: {}\n",
            self.count(Status::Imported),
            self.count(Status::Approximated),
            self.count(Status::Skipped)
        ));
        for (st, title) in [(Status::Imported, "Imported"), (Status::Approximated, "Approximated"), (Status::Skipped, "Skipped")] {
            let rows: Vec<&Item> = self.items.iter().filter(|i| i.status == st).collect();
            if rows.is_empty() {
                continue;
            }
            s.push_str(&format!("\n{title} ({})\n", rows.len()));
            for i in rows {
                s.push_str(&format!("  [{}] {}: {}\n", i.category, i.name, i.detail));
            }
        }
        if !self.unopened.is_empty() {
            s.push_str(&format!("\nNever opened (credentials): {}\n", self.unopened.join(", ")));
        }
        s.push_str(&format!("\n{} {} file(s) under {dest}\n", if dry_run { "Would write" } else { "Wrote" }, files.len()));
        for f in files {
            s.push_str(&format!("  {f}\n"));
        }
        s
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Plan {
    /// (path relative to the Brouillon data directory, content)
    pub outputs: Vec<(String, String)>,
    pub report: Report,
}

/// Files in the destination that [`apply`] copies to `<name>.pre-import.bak` before replacing them.
const BACKED_UP: &[&str] = &["keymap.toml", "tools.toml", "recent.toml"];

// ---------------------------------------------------------------------------
// INI reading
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
struct Entry {
    section: String,
    key: String,
    value: String,
}

fn norm(s: &str) -> String {
    s.chars().filter(|c| c.is_alphanumeric()).flat_map(char::to_lowercase).collect()
}

/// Ordered `[section] key=value` list. Secret keys are dropped and reported.
fn read_ini(text: &str, file: &str, report: &mut Report) -> Vec<Entry> {
    let mut out = Vec::new();
    let mut section = String::new();
    for line in text.lines() {
        let line = line.trim().trim_start_matches('\u{feff}');
        if line.is_empty() || line.starts_with(';') || line.len() > MAX_LINE {
            continue;
        }
        if let Some(s) = line.strip_prefix('[').and_then(|l| l.strip_suffix(']')) {
            section = if crate::hardening::section_is_credential(s) { "_hostile".to_string() } else { s.trim().to_string() };
            continue;
        }
        let Some((k, v)) = line.split_once('=') else { continue };
        let k = k.trim();
        if section == "_hostile" {
            continue;
        }
        if out.len() >= MAX_ENTRIES {
            report.add("files", file.to_string(), Status::Approximated, format!("more than {MAX_ENTRIES} entries: the rest was not read"));
            break;
        }
        if is_secret_key(k) {
            report.add("secrets", format!("{file}: [{}] {}", safe_label(&section), safe_label(k)), Status::Skipped, "credential-like key never copied");
            continue;
        }
        if value_has_credential(k, v) {
            report.add("secrets", format!("{file}: [{}] {}", safe_label(&section), safe_label(k)), Status::Skipped, "the value looked like it contained a credential: not copied");
            continue;
        }
        out.push(Entry { section: section.clone(), key: k.to_string(), value: v.trim().to_string() });
    }
    out
}

fn parse_bool(v: &str) -> Option<bool> {
    match v.trim().to_ascii_lowercase().as_str() {
        "1" | "y" | "yes" | "true" | "on" => Some(true),
        "0" | "n" | "no" | "false" | "off" | "" => Some(false),
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// Settings
// ---------------------------------------------------------------------------

#[derive(Clone, Copy)]
enum Kind {
    Int(i64, i64),
    Bool,
    Str,
}

/// (normalized key aliases, section, key, kind, status, note)
type Rule = (&'static [&'static str], &'static str, &'static str, Kind, Status, &'static str);

const RULES: &[Rule] = &[
    (&["tabsize", "tabspace", "tabwidth"], "editor", "tab_size", Kind::Int(1, 16), Status::Imported, ""),
    (&["indentsize", "indentspace"], "editor", "indent_size", Kind::Int(1, 16), Status::Imported, ""),
    (&["usespaces", "usespacesfortab", "inserttabsasspaces", "tabtospace", "insertspaces"], "editor", "insert_spaces", Kind::Bool, Status::Imported, ""),
    (&["wordwrap", "wordwrapping"], "editor", "word_wrap", Kind::Bool, Status::Imported, ""),
    (&["wrapcolumn", "wordwrapcolumn", "wrapwidth"], "editor", "wrap_column", Kind::Int(10, 1000), Status::Imported, ""),
    (&["fontname", "fontface", "facename", "font"], "editor", "font_family", Kind::Str, Status::Imported, ""),
    (&["fontsize", "fontpoint", "pointsize"], "editor", "font_size", Kind::Int(6, 72), Status::Imported, ""),
    (&["linenumber", "linenumbers", "showlinenumber", "showlinenumbers"], "editor", "line_numbers", Kind::Bool, Status::Imported, ""),
    (&["autoindent"], "editor", "auto_indent", Kind::Bool, Status::Imported, ""),
    (&["createbackup", "backupfile", "makebackup", "backup"], "backup", "enabled", Kind::Bool, Status::Imported, ""),
    (&["autoreload", "reloadchangedfile", "reloadmodifiedfile"], "files", "auto_reload_unmodified", Kind::Bool, Status::Imported, ""),
    (&["defaultencoding", "encoding", "defaultcodepage"], "files", "default_encoding", Kind::Str, Status::Imported, ""),
];

const NOT_CARRIED: &[(&str, &str)] = &[
    ("tabposition", "the tab bar layout is fixed"),
    ("multilinetabs", "the tab bar layout is fixed"),
    ("toolbar", "toolbars are not configurable yet"),
    ("windowpos", "window geometry is restored by the session instead"),
    ("placement", "window placement blob: not interpreted"),
    ("windowlist", "window list blob: not interpreted"),
    ("cliptextwindow", "panel layout: not carried (Brouillon opens its panels from the menu)"),
    ("rightsidepanel", "panel layout: not carried (Brouillon opens its panels from the menu)"),
    ("output", "output window layout: not carried"),
    ("workspacepath", "path of the old workspace folder: not carried"),
];

fn import_settings(entries: &[Entry], settings_text: &mut String, report: &mut Report) {
    let mut seen: BTreeSet<(&str, &str)> = BTreeSet::new();
    for e in entries {
        let nk = norm(&e.key);
        let name = format!("[{}] {}", e.section, e.key);
        let sect = norm(&e.section);
        // Tool, keyboard, recent-file, file-type sections are handled elsewhere.
        if is_tool_section(&e.section) || is_keyboard_section(&sect) || is_recent_section(&sect) || is_filetype_section(&sect) || is_restore_section(&sect) || is_tool_list_key(&sect, &nk) {
            continue;
        }
        if let Some((_, why)) = NOT_CARRIED.iter().find(|(k, _)| *k == nk) {
            report.add("settings", name, Status::Skipped, *why);
            continue;
        }
        if e.value.chars().any(|c| c.is_control() && c != '\t') {
            report.add("settings", name, Status::Skipped, "binary value (layout data): not interpreted, not guessed");
            continue;
        }
        // The font section's own `Name`/`Size` keys.
        let nk = if sect.contains("font") && nk == "name" { "fontname".to_string() } else if sect.contains("font") && nk == "size" { "fontsize".to_string() } else { nk };
        let Some((_, section, key, kind, status, note)) = RULES.iter().find(|r| r.0.contains(&nk.as_str())) else {
            report.add("settings", name, Status::Skipped, "no Brouillon equivalent");
            continue;
        };
        let raw = match kind {
            Kind::Bool => parse_bool(&e.value).map(|b| b.to_string()),
            // A wrap column of 0 means "wrap at the window edge": kept as 0, not raised to the minimum.
            Kind::Int(_, _) if *key == "wrap_column" && e.value.trim() == "0" => Some("0".to_string()),
            Kind::Int(lo, hi) => e.value.trim().parse::<i64>().ok().map(|n| n.abs().clamp(*lo, *hi).to_string()),
            Kind::Str if *key == "default_encoding" => match brouillon_core::codepages::supported(e.value.trim()) {
                Ok(cp) => Some(toml::Value::String(cp.id.to_string()).to_string()),
                Err(_) => {
                    report.add("settings", name, Status::Skipped, format!("encoding `{}` is not one Brouillon can convert", e.value));
                    continue;
                }
            },
            Kind::Str => (!e.value.is_empty()).then(|| toml::Value::String(e.value.clone()).to_string()),
        };
        let Some(raw) = raw else {
            report.add("settings", name, Status::Skipped, format!("value `{}` not understood", e.value));
            continue;
        };
        if !seen.insert((*section, *key)) {
            // Same target already set by an earlier key: first one wins.
            report.add("settings", name, Status::Skipped, format!("`{section}.{key}` already set from an earlier key"));
            continue;
        }
        *settings_text = set_raw_key(settings_text, section, key, &raw);
        let detail = if note.is_empty() { format!("{section}.{key} = {raw}") } else { format!("{section}.{key} = {raw} ({note})") };
        report.add("settings", name, *status, detail);
    }
}

// ---------------------------------------------------------------------------
// Tools
// ---------------------------------------------------------------------------

fn is_tool_section(s: &str) -> bool {
    let n = norm(s);
    let rest = n.strip_prefix("user").unwrap_or(&n);
    rest.strip_prefix("tool").is_some_and(|d| !d.is_empty() && d.chars().all(|c| c.is_ascii_digit()))
}

fn is_tool_list_section(norm_section: &str) -> bool {
    matches!(norm_section, "usertools" | "usertool" | "tools" | "tool")
}

fn is_tool_list_key(norm_section: &str, nk: &str) -> bool {
    is_tool_list_section(norm_section) && nk.trim_end_matches(|c: char| c.is_ascii_digit()) != nk
}

#[derive(Debug, Default, Clone)]
struct ToolRec {
    name: String,
    command: String,
    args: String,
    dir: String,
    capture: bool,
    save: bool,
    group: Option<u8>,
}

fn tool_field(r: &mut ToolRec, nk: &str, v: &str) {
    match nk {
        "menu" | "name" | "title" | "menutext" => r.name = v.to_string(),
        "command" | "cmd" => r.command = v.to_string(),
        "argument" | "arguments" | "args" | "param" | "parameters" => r.args = v.to_string(),
        "dir" | "initialdir" | "initdir" | "directory" | "workdir" => r.dir = v.to_string(),
        "capture" | "captureoutput" => r.capture = parse_bool(v).unwrap_or(false),
        "save" | "saveopenfiles" | "saveall" | "savebeforerun" | "savebeforeexecute" => r.save = parse_bool(v).unwrap_or(false),
        "group" => r.group = v.trim().parse().ok(),
        _ => {}
    }
}

fn collect_tools(entries: &[Entry]) -> Vec<(String, ToolRec)> {
    // (number, label, record): per-section tools (`[Tool3]`) and indexed keys (`Menu3=`) are both
    // supported and ordered by their number, as EditPlus numbers them.
    let mut found: Vec<(u32, String, ToolRec)> = Vec::new();
    let mut indexed: std::collections::BTreeMap<u32, ToolRec> = Default::default();
    for e in entries {
        let ns = norm(&e.section);
        let nk = norm(&e.key);
        if is_tool_section(&e.section) {
            if found.last().is_none_or(|(_, l, _)| *l != e.section) {
                let n = ns.trim_start_matches(|c: char| !c.is_ascii_digit()).parse().unwrap_or(u32::MAX);
                found.push((n, e.section.clone(), ToolRec::default()));
            }
            tool_field(&mut found.last_mut().unwrap().2, &nk, &e.value);
        } else if is_tool_list_section(&ns) {
            let stem = nk.trim_end_matches(|c: char| c.is_ascii_digit());
            if let Ok(n) = nk[stem.len()..].parse::<u32>() {
                tool_field(indexed.entry(n).or_default(), stem, &e.value);
            }
        }
    }
    found.extend(indexed.into_iter().map(|(n, r)| (n, format!("tool {n}"), r)));
    found.sort_by_key(|(n, _, _)| *n);
    found.into_iter().map(|(_, l, r)| (l, r)).collect()
}

/// EditPlus `$(Var)` to Brouillon `${var}`. Returns the text and the variables left untouched.
pub fn convert_vars(s: &str) -> (String, Vec<String>) {
    const MAP: &[(&str, &str)] = &[
        ("filepath", "file"), ("filedir", "dir"), ("filename", "filename"), ("filenamenoext", "name"),
        ("fileext", "ext"), ("curline", "line"), ("curcol", "col"), ("cursel", "selection"), ("curword", "word"),
    ];
    let (mut out, mut unknown) = (String::new(), Vec::new());
    let mut rest = s;
    while let Some(i) = rest.find("$(") {
        let Some(j) = rest[i..].find(')') else { break };
        out.push_str(&rest[..i]);
        let name = &rest[i + 2..i + j];
        match MAP.iter().find(|(k, _)| *k == norm(name)) {
            Some((_, to)) => out.push_str(&format!("${{{to}}}")),
            None => {
                unknown.push(format!("$({name})"));
                out.push_str(&rest[i..=i + j]);
            }
        }
        rest = &rest[i + j + 1..];
    }
    out.push_str(rest);
    (out, unknown)
}

/// Split a command-line argument string into arguments (double quotes group).
pub fn split_args(s: &str) -> Vec<String> {
    let (mut out, mut cur, mut quoted, mut any) = (Vec::new(), String::new(), false, false);
    for c in s.chars() {
        match c {
            '"' => {
                quoted = !quoted;
                any = true;
            }
            c if c.is_whitespace() && !quoted => {
                if any || !cur.is_empty() {
                    out.push(std::mem::take(&mut cur));
                    any = false;
                }
            }
            c => cur.push(c),
        }
    }
    if any || !cur.is_empty() {
        out.push(cur);
    }
    out
}

fn import_tools(entries: &[Entry], report: &mut Report) -> Option<String> {
    let recs = collect_tools(entries);
    let mut root = toml::Table::new();
    let mut tools: Vec<toml::Value> = Vec::new();
    let mut next_slot: u8 = 1;
    for (label, r) in recs {
        let name = r.name.replace('&', "");
        let shown = if name.is_empty() { label.clone() } else { name.clone() };
        if r.command.trim().is_empty() {
            report.add("tools", shown, Status::Skipped, "no command");
            continue;
        }
        if name.trim().is_empty() {
            report.add("tools", shown, Status::Skipped, "no menu text");
            continue;
        }
        let mut unknown = Vec::new();
        let mut conv = |s: &str| {
            let (t, u) = convert_vars(s);
            unknown.extend(u);
            t
        };
        let command = conv(r.command.trim());
        let args: Vec<toml::Value> = split_args(&r.args).iter().map(|a| toml::Value::String(conv(a))).collect();
        let cwd = conv(r.dir.trim());
        let mut t = toml::Table::new();
        t.insert("name".into(), name.into());
        let lower = command.to_ascii_lowercase();
        let batch = lower.ends_with(".bat") || lower.ends_with(".cmd");
        t.insert("command".into(), command.into());
        if !args.is_empty() {
            t.insert("args".into(), toml::Value::Array(args));
        }
        if !cwd.is_empty() {
            t.insert("cwd".into(), cwd.into());
        }
        if batch {
            t.insert("shell".into(), true.into());
        }
        let group = r.group.filter(|g| (1..=20).contains(g)).unwrap_or(1);
        t.insert("group".into(), i64::from(group).into());
        let mut notes = Vec::new();
        if group == 1 && next_slot <= 10 {
            t.insert("slot".into(), i64::from(next_slot).into());
            notes.push(format!("Ctrl+{}", next_slot % 10));
            next_slot += 1;
        }
        t.insert("save_before".into(), (if r.save { "all" } else { "none" }).into());
        t.insert("output".into(), (if r.capture { "panel" } else { "none" }).into());
        if batch {
            notes.push("run through the shell (.bat/.cmd)".into());
        }
        unknown.sort();
        unknown.dedup();
        let (status, detail) = if unknown.is_empty() {
            (Status::Imported, if notes.is_empty() { "tools.toml".to_string() } else { notes.join(", ") })
        } else {
            notes.push(format!("variables kept as typed, no Brouillon equivalent: {}", unknown.join(" ")));
            (Status::Approximated, notes.join("; "))
        };
        report.add("tools", shown, status, detail);
        tools.push(toml::Value::Table(t));
    }
    if tools.is_empty() {
        return None;
    }
    root.insert("tool".into(), toml::Value::Array(tools));
    Some(format!("# Imported from EditPlus user tools.\n{}", toml::to_string(&root).unwrap_or_default()))
}

// ---------------------------------------------------------------------------
// Keyboard shortcuts
// ---------------------------------------------------------------------------

fn is_keyboard_section(ns: &str) -> bool {
    matches!(ns, "keyboard" | "keymap" | "hotkeys" | "hotkey" | "shortcuts" | "shortcutkeys" | "accelerators" | "keyboardshortcuts")
}

/// EditPlus command captions (normalized) to Brouillon command ids.
const COMMAND_NAMES: &[(&str, &str)] = &[
    ("new", "file_new"), ("open", "file_open"), ("save", "file_save"), ("saveas", "file_save_as"), ("close", "file_close"),
    ("print", "file_print"), ("exit", "app_quit"), ("quit", "app_quit"), ("undo", "edit_undo"), ("redo", "edit_redo"),
    ("cut", "edit_cut"), ("copy", "edit_copy"), ("paste", "edit_paste"), ("pastecolumn", "edit_paste_column"),
    ("selectall", "select_all"), ("selectline", "select_line"), ("selectword", "select_word"),
    ("find", "search_open_find"), ("replace", "search_open_replace"), ("findnext", "search_find_next"),
    ("findprevious", "search_find_prev"), ("findinfiles", "find_in_files"), ("goto", "go_to"), ("gotoline", "go_to"),
    ("gotocolumn", "goto_column"), ("gotomatchingbracket", "bracket_match"), ("matchingbracket", "bracket_match"),
    ("duplicateline", "duplicate_line"), ("deleteline", "delete_line"), ("joinlines", "join_lines"),
    ("splitlines", "split_lines"), ("uppercase", "case_upper"), ("converttouppercase", "case_upper"),
    ("lowercase", "case_lower"), ("converttolowercase", "case_lower"), ("invertcase", "case_invert"),
    ("capitalize", "case_capitalize"), ("nexttab", "tab_next"), ("nextwindow", "tab_next"), ("previoustab", "tab_previous"),
    ("previouswindow", "tab_previous"), ("togglebookmark", "bookmark_toggle"), ("nextbookmark", "bookmark_next"),
    ("previousbookmark", "bookmark_prev"), ("clearbookmarks", "bookmark_clear"), ("comment", "toggle_line_comment"),
    ("togglecomment", "toggle_line_comment"), ("inserttime", "insert_time_short"), ("insertdate", "insert_date_short"),
];

/// `Ctrl+Shift+Del` to `ctrl+shift+delete`; `None` when the result is not a valid chord.
pub fn normalize_chord(s: &str) -> Option<String> {
    let parts: Vec<String> = s
        .split('+')
        .map(|p| {
            let p = p.trim().to_ascii_lowercase();
            match p.as_str() {
                "control" => "ctrl".into(),
                "del" => "delete".into(),
                "ins" => "insert".into(),
                "esc" => "escape".into(),
                "pgup" => "pageup".into(),
                "pgdn" => "pagedown".into(),
                "return" => "enter".into(),
                "bksp" | "bs" => "backspace".into(),
                _ => p,
            }
        })
        .collect();
    let (mods, keys): (Vec<&String>, Vec<&String>) = parts.iter().partition(|p| matches!(p.as_str(), "ctrl" | "shift" | "alt"));
    let valid_key = |k: &str| {
        k.chars().count() == 1
            || k.strip_prefix('f').and_then(|n| n.parse::<u8>().ok()).is_some_and(|n| (1..=24).contains(&n))
            || matches!(k, "enter" | "tab" | "space" | "backspace" | "delete" | "insert" | "home" | "end" | "pageup" | "pagedown" | "up" | "down" | "left" | "right" | "escape")
    };
    if keys.len() != 1 || !valid_key(keys[0]) {
        return None;
    }
    let joined = mods.iter().chain(keys.iter()).map(|s| s.as_str()).collect::<Vec<_>>().join("+");
    keymap::parse_chord(&joined).map(|c| keymap::chord_to_string(&c))
}

fn import_keys(entries: &[Entry], known: &dyn Fn(&str) -> bool, report: &mut Report) -> Option<String> {
    let mut bindings: Vec<keymap::Binding> = Vec::new();
    for e in entries.iter().filter(|e| is_keyboard_section(&norm(&e.section))) {
        let name = format!("{} = {}", e.key, e.value);
        if e.value.trim().is_empty() {
            continue; // an unassigned slot
        }
        let nk = norm(&e.key);
        if nk.chars().all(|c| c.is_ascii_digit()) {
            report.add("keys", name, Status::Skipped, "numeric command id: no public mapping to a Brouillon command");
            continue;
        }
        let id = if known(e.key.trim()) {
            Some(e.key.trim().to_string())
        } else {
            COMMAND_NAMES.iter().find(|(k, _)| *k == nk).map(|(_, id)| (*id).to_string())
        };
        let Some(id) = id.filter(|i| known(i)) else {
            report.add("keys", name, Status::Skipped, "no Brouillon command with this name");
            continue;
        };
        let Some(chord) = normalize_chord(&e.value) else {
            report.add("keys", name, Status::Skipped, format!("`{}` is not a valid key chord", e.value));
            continue;
        };
        if let Some(prev) = bindings.iter().find(|b| keymap::chord_to_string(&b.chord) == chord) {
            report.add("keys", name, Status::Skipped, format!("{chord} is already bound to {}", prev.command));
            continue;
        }
        if let Some(b) = keymap::Binding::new(&chord, &id, None) {
            report.add("keys", name, Status::Imported, format!("{chord} -> {id}"));
            bindings.push(b);
        }
    }
    (!bindings.is_empty()).then(|| format!("# Imported from EditPlus keyboard settings.\n{}", keymap::serialize_bindings(&bindings)))
}

// ---------------------------------------------------------------------------
// Recent files, file types
// ---------------------------------------------------------------------------

fn is_restore_section(ns: &str) -> bool {
    ns.starts_with("restore")
}

fn is_recent_section(ns: &str) -> bool {
    ns.contains("recent") || ns == "mru"
}

const MAX_RECENT: usize = 10;

fn import_recent(entries: &[Entry], report: &mut Report) -> Option<String> {
    let mut files: Vec<(u32, String)> = Vec::new();
    for e in entries.iter().filter(|e| is_recent_section(&norm(&e.section))) {
        let nk = norm(&e.key);
        let Some(n) = nk.strip_prefix("file").and_then(|d| d.parse::<u32>().ok()) else { continue };
        if !e.value.is_empty() {
            files.push((n, e.value.clone()));
        }
    }
    // Files that were open at the last exit (`[Restore\N] File=`), after the recent list.
    let mut skipped_keys: std::collections::BTreeMap<String, usize> = Default::default();
    let mut sec_no = 0u32;
    let mut last_sec = String::new();
    for e in entries.iter().filter(|e| is_restore_section(&norm(&e.section))) {
        if e.section != last_sec {
            last_sec = e.section.clone();
            sec_no += 1;
        }
        match norm(&e.key).as_str() {
            "count" => {}
            "file" if !e.value.is_empty() => files.push((1_000_000 + sec_no, e.value.clone())),
            "file" => {}
            other => *skipped_keys.entry(other.to_string()).or_default() += 1,
        }
    }
    for (k, n) in skipped_keys {
        report.add("restore", format!("[Restore\\N] {k} (x{n})"), Status::Skipped, "per-file state of the previous session: Brouillon restores its own session instead");
    }
    if files.is_empty() {
        return None;
    }
    files.sort();
    let mut keep: Vec<String> = Vec::new();
    for (_, p) in files {
        if keep.contains(&p) {
            continue;
        }
        if keep.len() < MAX_RECENT {
            report.add("recent", p.clone(), Status::Imported, "recent.toml");
            keep.push(p);
        } else {
            report.add("recent", p, Status::Skipped, format!("only the {MAX_RECENT} most recent files are kept"));
        }
    }
    let mut t = toml::Table::new();
    t.insert("paths".into(), toml::Value::Array(keep.into_iter().map(toml::Value::String).collect()));
    Some(toml::to_string(&t).unwrap_or_default())
}

fn is_filetype_section(ns: &str) -> bool {
    ns.starts_with("filetype") || ns == "syntax" || ns.starts_with("syntaxfile")
}

/// (stx file stem, extensions) pairs from `[FileTypeN]` style sections.
fn filetype_map(entries: &[Entry]) -> Vec<(String, Vec<String>)> {
    let mut out: Vec<(String, Vec<String>)> = Vec::new();
    let mut sections: Vec<&str> = entries.iter().map(|e| e.section.as_str()).filter(|s| is_filetype_section(&norm(s))).collect();
    sections.dedup();
    for s in sections {
        let get = |names: &[&str]| entries.iter().find(|e| e.section == s && names.contains(&norm(&e.key).as_str())).map(|e| e.value.clone());
        let (Some(exts), Some(stx)) = (get(&["extensions", "ext", "extension", "files"]), get(&["syntax", "syntaxfile", "stx"])) else { continue };
        let stem = Path::new(stx.trim()).file_stem().map(|s| s.to_string_lossy().to_lowercase()).unwrap_or_default();
        let list: Vec<String> = exts.split([';', ',', ' ']).map(|e| e.trim().trim_start_matches("*.").trim_start_matches('.').to_lowercase()).filter(|e| !e.is_empty()).collect();
        if !stem.is_empty() && !list.is_empty() {
            out.push((stem, list));
        }
    }
    out
}

// ---------------------------------------------------------------------------
// Driver
// ---------------------------------------------------------------------------

/// Build the import plan for `dir`. `known` says whether a command id exists in
/// the Brouillon registry. `existing_settings` is the current `settings.toml` of
/// the destination (imported keys are merged into it, other lines are kept).
pub fn plan_import(dir: &Path, known: &dyn Fn(&str) -> bool, existing_settings: &str) -> io::Result<Plan> {
    plan_import_with_defaults(dir, None, known, existing_settings)
}

/// The folders where the per-user settings and the program files of EditPlus usually are.
pub fn detect_dirs() -> (Option<std::path::PathBuf>, Option<std::path::PathBuf>) {
    let env = |n: &str| std::env::var_os(n).map(std::path::PathBuf::from);
    let user = env("APPDATA").map(|p| p.join("EditPlus")).filter(|p| p.is_dir());
    let program = ["ProgramFiles", "ProgramFiles(x86)"].iter().filter_map(|n| env(n)).map(|p| p.join("EditPlus")).find(|p| p.is_dir());
    (user, program)
}

fn same_text(a: &[u8], b: &[u8]) -> bool {
    let n = |x: &[u8]| decode_any(x).replace("\r\n", "\n").trim_end().to_string();
    n(a) == n(b)
}

/// Like [`plan_import`]. `defaults` is the EditPlus program folder: syntax, auto-completion,
/// cliptext and template files identical to the shipped ones are not imported again.
pub fn plan_import_with_defaults(dir: &Path, defaults: Option<&Path>, known: &dyn Fn(&str) -> bool, existing_settings: &str) -> io::Result<Plan> {
    let mut shipped = 0usize;
    let mut report = Report::default();
    let mut outputs: Vec<(String, String)> = Vec::new();
    let mut entries: Vec<Entry> = Vec::new();
    let mut names: Vec<_> = fs::read_dir(dir)?.filter_map(Result::ok).filter(|e| e.path().is_file()).map(|e| e.file_name().to_string_lossy().into_owned()).collect();
    names.sort_by_key(|n| n.to_lowercase());
    names.truncate(crate::hardening::MAX_FILES);
    let mut stx_files: Vec<(String, Vec<u8>)> = Vec::new();
    let mut used_stems: BTreeSet<String> = BTreeSet::new();
    let mut raw_unused = RawReport::default();
    let mut templates: Vec<(String, String)> = Vec::new();

    for name in &names {
        let lower = name.to_lowercase();
        let (stem, ext) = lower.rsplit_once('.').map_or((lower.as_str(), ""), |(s, e)| (s, e));
        let path = dir.join(name);
        let base = stem.strip_suffix("_u").unwrap_or(stem);
        let data_file = matches!(ext, "ctl" | "ect" | "acp" | "stx") || (stem.starts_with("template") && !ext.is_empty()) || (ext == "json" && stem == "snippets");
        if let (true, Some(d)) = (data_file, defaults) {
            if fs::read(&path).ok().zip(fs::read(d.join(name)).ok()).is_some_and(|(a, b)| same_text(&a, &b)) {
                shipped += 1;
                continue;
            }
        }
        match ext {
            "ini" if base == "reg" || base.starts_with("reg_") => report.unopened.push(name.clone()),
            "ini" if base.starts_with("ftp") || base.starts_with("sftp") => {
                report.unopened.push(name.clone());
                report.add("files", name.clone(), Status::Skipped, "remote-server credentials: not opened");
            }
            _ if crate::hardening::too_large(&path) => report.add("files", name.clone(), Status::Skipped, "larger than 4 MiB: not read"),
            "ini" if base.starts_with("workspace") || base.starts_with("regex") => {
                let table = ini_to_table(&decode_any(&fs::read(&path)?), name, &mut raw_unused);
                outputs.push((format!("ini/{stem}.toml"), toml::to_string(&table).unwrap_or_default()));
                report.add("files", name.clone(), Status::Approximated, format!("kept as data in ini/{stem}.toml, nothing reads it yet"));
            }
            "ini" => entries.extend(read_ini(&decode_any(&fs::read(&path)?), name, &mut report)),
            "ctl" | "ect" => {
                let lib = cliptext::parse_ctl(&decode_any(&fs::read(&path)?));
                let mut out_stem = stem.to_string();
                if !used_stems.insert(format!("clip/{out_stem}")) {
                    out_stem = format!("{stem}-{ext}");
                }
                outputs.push((format!("cliptext/{out_stem}.toml"), cliptext::to_toml(&lib)));
                report.add("cliptext", name.clone(), Status::Imported, format!("{} entries -> cliptext/{out_stem}.toml", lib.entries.len()));
            }
            "acp" => {
                let f = acp::parse_acp(&decode_any(&fs::read(&path)?));
                outputs.push((format!("abbrev/{stem}.toml"), acp::to_toml(&f)));
                report.add("autocomplete", name.clone(), Status::Imported, format!("{} abbreviations -> abbrev/{stem}.toml", f.abbrevs.len()));
            }
            "stx" => stx_files.push((stem.to_string(), fs::read(&path)?)),
            "json" if stem == "snippets" => {
                let text = decode_any(&fs::read(&path)?);
                match serde_json::from_str::<serde_json::Value>(&text) {
                    Ok(v) => {
                        let obj = v.as_object();
                        let syntaxes = obj.map_or(0, |o| o.len());
                        let entries: usize = obj
                            .into_iter()
                            .flatten()
                            .filter_map(|(_, s)| s.as_object())
                            .flat_map(|s| ["snippets", "abbreviations"].into_iter().filter_map(|k| s.get(k)?.as_object().map(|m| m.len())))
                            .sum();
                        outputs.push(("emmet/snippets.json".into(), text));
                        report.add("emmet", name.clone(), Status::Approximated, format!("{syntaxes} syntaxes, {entries} abbreviations kept as data in emmet/snippets.json; Brouillon has no Emmet expansion yet"));
                    }
                    Err(_) => report.add("emmet", name.clone(), Status::Skipped, "not valid JSON"),
                }
            }
            "js" if stem.starts_with("emmet") => {
                report.add("emmet", name.clone(), Status::Skipped, "custom Emmet configuration script: never run or copied (Brouillon has no Emmet engine)");
            }
            _ if stem.starts_with("template") && !ext.is_empty() => {
                templates.push((ext.to_string(), name.clone()));
                outputs.push((format!("templates/{name}"), decode_any(&fs::read(&path)?)));
            }
            _ => report.add("files", name.clone(), Status::Skipped, "not an EditPlus settings file Brouillon reads"),
        }
    }
    if shipped > 0 {
        report.add("defaults", "shipped EditPlus files", Status::Skipped, format!("{shipped} file(s) identical to the ones installed with EditPlus: not imported again"));
    }
    if !templates.is_empty() {
        use brouillon_editor_engine::templates::{Template, TemplateList};
        let mut list = TemplateList::default();
        for (ext, file) in &templates {
            let _ = list.add(Template { name: format!("New .{ext}"), file: file.clone() });
            report.add("templates", file.clone(), Status::Imported, format!("New .{ext} -> templates/{file}"));
        }
        outputs.push(("templates/templates.toml".into(), list.to_toml()));
    }

    // Syntax files, with extensions from the file-type sections when present.
    let ftmap = filetype_map(&entries);
    for (stem, bytes) in stx_files {
        match stx::convert_stx_bytes(&bytes) {
            Ok((table, warns)) => {
                let mut def: toml::Table = stx::to_definition_toml(&table).parse().unwrap_or_default();
                let exts: Vec<String> = ftmap.iter().filter(|(s, _)| *s == stem).flat_map(|(_, e)| e.clone()).collect();
                let has_exts = !exts.is_empty();
                if has_exts {
                    def.insert("extensions".into(), toml::Value::Array(exts.into_iter().map(toml::Value::String).collect()));
                }
                outputs.push((format!("syntax/{stem}.toml"), format!("# Imported from {stem}.stx\n{}", toml::to_string(&def).unwrap_or_default())));
                let groups = table.groups.len();
                let words: usize = table.groups.iter().map(|g| g.words.len()).sum();
                let mut d = format!("{groups} keyword groups, {words} words -> syntax/{stem}.toml");
                if !has_exts {
                    d.push_str("; extensions left empty (not in the INI)");
                }
                if !warns.is_empty() {
                    d.push_str(&format!("; {} warning(s): {}", warns.len(), warns.join(" | ")));
                }
                report.add("syntax", format!("{stem}.stx"), if has_exts && warns.is_empty() { Status::Imported } else { Status::Approximated }, d);
            }
            Err(e) => report.add("syntax", format!("{stem}.stx"), Status::Skipped, e),
        }
    }

    // INI content: settings, tools, keys, recent files.
    let mut settings_text = existing_settings.to_string();
    import_settings(&entries, &mut settings_text, &mut report);
    if let Some(t) = import_tools(&entries, &mut report) {
        outputs.push(("tools.toml".into(), t));
    }
    let keys = import_keys(&entries, known, &mut report);
    if let Some(k) = keys {
        outputs.push(("keymap.toml".into(), k));
    }
    if let Some(r) = import_recent(&entries, &mut report) {
        outputs.push(("recent.toml".into(), r));
    }
    // An EditPlus user expects the EditPlus-style shortcuts.
    settings_text = set_raw_key(&settings_text, "keymap", "classic", "true");
    report.add("settings", "keyboard preset", Status::Imported, "keymap.classic = true (the Classic preset is switched on)");
    outputs.push(("settings.toml".into(), settings_text));
    outputs.sort_by(|a, b| a.0.cmp(&b.0));
    Ok(Plan { outputs, report })
}

/// Write the plan under `dest`. Existing `keymap.toml`, `tools.toml` and
/// `recent.toml` are first copied to `<name>.pre-import.bak` (never overwriting
/// an earlier backup). Returns the relative paths written.
pub fn apply(plan: &Plan, dest: &Path) -> io::Result<Vec<String>> {
    let mut written = Vec::new();
    for (rel, content) in &plan.outputs {
        let p = dest.join(rel);
        if let Some(parent) = p.parent() {
            fs::create_dir_all(parent)?;
        }
        if BACKED_UP.contains(&rel.as_str()) && p.is_file() {
            let bak = dest.join(format!("{rel}.pre-import.bak"));
            if !bak.exists() {
                fs::copy(&p, &bak)?;
            }
        }
        fs::write(&p, content)?;
        written.push(rel.clone());
    }
    Ok(written)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::TempDir;
    use brouillon_editor_engine::tools_config;

    const INI: &str = "\
; synthetic EditPlus-style settings (own fixture)
[Settings]
TabSize=3
IndentSize=3
Use spaces for tab=1
Word wrap=1
Wrap column=100
Auto indent=1
Line number=1
Tab position=0
Window pos=10,10,800,600
Mystery Flag=7
[Font]
Name=Consolas
Size=-13
[Files]
Create backup=1
Auto reload=0
Default encoding=utf-8
Remote Password=hunter2
[Recent File List]
File1=C:\\work\\a.txt
File2=C:\\work\\b.txt
File3=C:\\work\\a.txt
File4=C:\\work\\c.txt
File5=C:\\work\\d.txt
File6=C:\\work\\e.txt
File7=C:\\work\\f.txt
File8=C:\\work\\g.txt
File9=C:\\work\\h.txt
File10=C:\\work\\i.txt
File11=C:\\work\\j.txt
File12=C:\\work\\k.txt
[Keyboard]
Save As=Ctrl+Alt+S
Duplicate Line=Ctrl+Shift+D
Upper Case=Ctrl+Shift+Del
Find=Ctrl+Shift+Del
Replace=Ctrl+Hyper+H
Frobnicate=Ctrl+F12
1234=Ctrl+F5
Undo=
[User Tools]
Menu1=&Build
Command1=make.bat
Argument1=-j4 \"$(FilePath)\"
Dir1=$(FileDir)
Capture1=1
Save1=1
Menu2=Lint
Command2=lint
Argument2=$(CurWord) $(Weird)
[Tool3]
Menu=Notes
Command=notepad.exe
Argument=$(FileName)
[FileType1]
Extensions=mini;mn
Syntax=mini.stx
";

    fn fixture() -> TempDir {
        let d = TempDir::new();
        let w = |n: &str, c: &[u8]| fs::write(d.path().join(n), c).unwrap();
        w("editplus.ini", INI.as_bytes());
        w("mini.stx", b"#TITLE=Mini\n#DELIMITER=,(){}\n#LINECOMMENT=--\n#KEYWORD=Words\nbegin\nend\n#\n");
        w("bad.stx", b"");
        w("snip.ctl", b"#TITLE=Snip\r\n#T=bold\r\n<b>^!</b>\r\n#\r\n#T=title only\r\n");
        w("extra.ect", b"#TITLE=Extra\r\n#T=x\r\nbody\r\n#\r\n");
        w("mini.acp", b"#TITLE=A\r\n#CASE=y\r\n#T=if\r\nif (^!)\r\n#\r\n");
        w("template.html", b"<p>^!</p>\r\n");
        w("workspace_u.ini", b"[Workspace]\nCount=1\n");
        w("reg_u.ini", b"Name=me\nCode=SECRET-LICENCE\n");
        w("ftp_u.ini", b"Host=h\nPassword=abc\n");
        w("readme.txt", b"hello");
        d
    }

    fn known(id: &str) -> bool {
        !matches!(id, "frobnicate") && id.chars().all(|c| c.is_ascii_lowercase() || c == '_')
    }

    fn plan() -> (TempDir, Plan) {
        let d = fixture();
        let p = plan_import(d.path(), &known, "").unwrap();
        (d, p)
    }

    fn out<'a>(p: &'a Plan, rel: &str) -> &'a str {
        &p.outputs.iter().find(|(r, _)| r == rel).unwrap_or_else(|| panic!("no output {rel}")).1
    }

    fn item<'a>(p: &'a Plan, cat: &str, needle: &str) -> &'a Item {
        p.report.items.iter().find(|i| i.category == cat && i.name.contains(needle)).unwrap_or_else(|| panic!("no {cat} item {needle}"))
    }

    #[test]
    fn settings_imported_approximated_and_skipped() {
        let (_d, p) = plan();
        let s: toml::Table = out(&p, "settings.toml").parse().unwrap();
        assert_eq!(s["editor"]["tab_size"].as_integer(), Some(3));
        assert_eq!(s["editor"]["word_wrap"].as_bool(), Some(true));
        assert_eq!(s["editor"]["font_family"].as_str(), Some("Consolas"));
        assert_eq!(s["editor"]["font_size"].as_integer(), Some(13), "negative pixel height becomes a positive size");
        assert_eq!(s["backup"]["enabled"].as_bool(), Some(true));
        assert_eq!(s["files"]["auto_reload_unmodified"].as_bool(), Some(false));
        assert_eq!(s["keymap"]["classic"].as_bool(), Some(true));
        assert_eq!(item(&p, "settings", "TabSize").status, Status::Imported);
        assert_eq!(item(&p, "settings", "Wrap column").status, Status::Imported);
        assert_eq!(item(&p, "settings", "Tab position").status, Status::Skipped);
        assert_eq!(item(&p, "settings", "Mystery").status, Status::Skipped);
        // the merged result is a valid Brouillon settings layer: only our approximated keys are "unknown"
        let st = brouillon_core::settings::Settings::build(&[(brouillon_core::settings::Layer::User, out(&p, "settings.toml"))]);
        assert!(st.errors.is_empty());
        assert_eq!(st.get_i64("editor.tab_size", 0), 3);
    }

    #[test]
    fn existing_settings_are_merged_not_replaced() {
        let d = fixture();
        let p = plan_import(d.path(), &known, "# mine\n[theme]\nmode = \"light\"\n[editor]\ntab_size = 8\n").unwrap();
        let s = out(&p, "settings.toml");
        assert!(s.contains("# mine") && s.contains("mode = \"light\""));
        let t: toml::Table = s.parse().unwrap();
        assert_eq!(t["editor"]["tab_size"].as_integer(), Some(3), "imported value wins on the same key");
    }

    #[test]
    fn tools_both_layouts_with_variables_and_slots() {
        let (_d, p) = plan();
        let cfg = tools_config::parse(out(&p, "tools.toml"));
        assert!(cfg.warnings.is_empty(), "{:?}", cfg.warnings);
        assert_eq!(cfg.tools.len(), 3);
        let build = cfg.tools.iter().find(|t| t.name == "Build").unwrap();
        assert_eq!(build.command, "make.bat");
        assert_eq!(build.args, vec!["-j4", "${file}"]);
        assert_eq!(build.cwd, "${dir}");
        assert!(build.shell);
        assert_eq!(build.slot, Some(1));
        assert_eq!(build.save_before, tools_config::SavePolicy::All);
        assert_eq!(build.output, tools_config::OutputSink::Panel);
        let lint = cfg.tools.iter().find(|t| t.name == "Lint").unwrap();
        assert_eq!(lint.args, vec!["${word}", "$(Weird)"]);
        assert_eq!(item(&p, "tools", "Lint").status, Status::Approximated);
        assert!(item(&p, "tools", "Lint").detail.contains("$(Weird)"));
        assert_eq!(item(&p, "tools", "Build").status, Status::Imported);
        assert_eq!(item(&p, "tools", "Notes").status, Status::Imported);
        assert_eq!(p.report.of("tools").len(), 3);
    }

    #[test]
    fn var_and_arg_helpers() {
        assert_eq!(convert_vars("$(FileDir)\\$(FileNameNoExt).o").0, "${dir}\\${name}.o");
        assert_eq!(convert_vars("$(Nope)").1, vec!["$(Nope)"]);
        assert_eq!(convert_vars("no vars $( open").0, "no vars $( open");
        assert_eq!(split_args(r#"a "b c" "" d"#), vec!["a", "b c", "", "d"]);
        assert!(split_args("   ").is_empty());
    }

    #[test]
    fn keyboard_names_chords_and_conflicts() {
        let (_d, p) = plan();
        let (b, w) = keymap::parse_bindings_toml(out(&p, "keymap.toml"));
        assert!(w.is_empty());
        let get = |c: &str| b.iter().find(|x| keymap::chord_to_string(&x.chord) == c).map(|x| x.command.clone());
        assert_eq!(get("ctrl+alt+s").as_deref(), Some("file_save_as"));
        assert_eq!(get("ctrl+shift+d").as_deref(), Some("duplicate_line"));
        assert_eq!(get("ctrl+shift+delete").as_deref(), Some("case_upper"));
        assert_eq!(b.len(), 3);
        assert!(item(&p, "keys", "Find").detail.contains("already bound"));
        assert!(item(&p, "keys", "Hyper").detail.contains("not a valid"));
        assert!(item(&p, "keys", "Frobnicate").detail.contains("no Brouillon command"));
        assert!(item(&p, "keys", "1234").detail.contains("numeric"));
        assert!(p.report.of("keys").iter().all(|i| !i.name.starts_with("Undo")), "empty slot is not an item");
        assert_eq!(normalize_chord("Control+Ins").as_deref(), Some("ctrl+insert"));
        assert_eq!(normalize_chord("F12").as_deref(), Some("f12"));
        assert!(normalize_chord("Ctrl+").is_none());
    }

    #[test]
    fn recent_files_deduped_and_capped() {
        let (_d, p) = plan();
        let t: toml::Table = out(&p, "recent.toml").parse().unwrap();
        let paths = t["paths"].as_array().unwrap();
        assert_eq!(paths.len(), 10);
        assert_eq!(paths[0].as_str(), Some("C:\\work\\a.txt"));
        assert_eq!(paths[1].as_str(), Some("C:\\work\\b.txt"));
        assert_eq!(p.report.of("recent").iter().filter(|i| i.status == Status::Skipped).count(), 1, "j and k beyond 10, a duplicate dropped silently");
    }

    #[test]
    fn stx_cliptext_acp_templates() {
        let (_d, p) = plan();
        let syn: toml::Table = out(&p, "syntax/mini.toml").parse().unwrap();
        assert_eq!(syn["extensions"].as_array().unwrap().len(), 2);
        assert_eq!(item(&p, "syntax", "mini.stx").status, Status::Imported);
        // the empty .stx: either skipped or approximated, never silently dropped
        assert!(p.report.items.iter().any(|i| i.category == "syntax" && i.name == "bad.stx"));
        let clip = cliptext::from_toml(out(&p, "cliptext/snip.toml")).unwrap();
        assert_eq!(clip.entries[0].body, "<b>^!</b>");
        assert_eq!(clip.entries.len(), 2);
        assert!(out(&p, "cliptext/extra.toml").contains("body"), ".ect is read as cliptext");
        assert!(acp::from_toml(out(&p, "abbrev/mini.toml")).unwrap().case_sensitive);
        assert!(out(&p, "templates/templates.toml").contains("template.html"));
    }

    #[test]
    fn secrets_and_licence_never_copied_or_opened() {
        let (_d, p) = plan();
        let all: String = p.outputs.iter().map(|(_, c)| c.as_str()).collect();
        assert!(!all.contains("hunter2") && !all.contains("SECRET-LICENCE") && !all.contains("abc"));
        assert!(p.report.unopened.contains(&"reg_u.ini".to_string()) && p.report.unopened.contains(&"ftp_u.ini".to_string()));
        assert!(item(&p, "secrets", "Remote Password").status == Status::Skipped);
        assert!(p.outputs.iter().all(|(r, _)| !r.contains("reg_u") && !r.contains("ftp")));
    }

    #[test]
    fn unknown_files_are_reported() {
        let (_d, p) = plan();
        assert_eq!(item(&p, "files", "readme.txt").status, Status::Skipped);
        assert_eq!(item(&p, "files", "workspace_u.ini").status, Status::Approximated);
    }

    #[test]
    fn apply_writes_backs_up_and_leaves_source() {
        let (src, p) = plan();
        let dest = TempDir::new();
        fs::write(dest.path().join("keymap.toml"), "# user\n").unwrap();
        let written = apply(&p, dest.path()).unwrap();
        assert_eq!(written.len(), p.outputs.len());
        assert_eq!(fs::read_to_string(dest.path().join("keymap.toml.pre-import.bak")).unwrap(), "# user\n");
        // second run keeps the first backup
        apply(&p, dest.path()).unwrap();
        assert_eq!(fs::read_to_string(dest.path().join("keymap.toml.pre-import.bak")).unwrap(), "# user\n");
        assert!(dest.path().join("tools.toml").is_file() && dest.path().join("cliptext/snip.toml").is_file());
        assert!(src.path().join("editplus.ini").is_file());
    }

    #[test]
    fn report_renders_totals_and_dry_run_header() {
        let (_d, p) = plan();
        let files: Vec<String> = p.outputs.iter().map(|o| o.0.clone()).collect();
        let r = p.report.render(true, "/dest", &files);
        assert!(r.starts_with("EditPlus import (dry run, nothing written)"));
        assert!(r.contains(&format!("imported: {}", p.report.count(Status::Imported))));
        assert!(r.contains("Would write") && r.contains("Skipped (") && r.contains("Approximated ("));
        assert!(r.contains("reg_u.ini"));
        assert!(p.report.render(false, "/dest", &files).contains("Wrote"));
    }

    #[test]
    fn empty_folder_still_enables_classic_only() {
        let d = TempDir::new();
        let p = plan_import(d.path(), &known, "").unwrap();
        assert_eq!(p.outputs.len(), 1);
        assert_eq!(p.report.items.len(), 1);
    }

    fn utf16(s: &str, bom: bool) -> Vec<u8> {
        let mut v = if bom { vec![0xFF, 0xFE] } else { Vec::new() };
        v.extend(s.encode_utf16().flat_map(|u| u.to_le_bytes()));
        v
    }

    /// Synthetic copy of the structure of a real per-user EditPlus folder (`_u` names, UTF-16).
    fn user_folder(bom: bool) -> TempDir {
        let d = TempDir::new();
        let w = |n: &str, c: Vec<u8>| fs::write(d.path().join(n), c).unwrap();
        w("editplus_u.ini", utf16("[Options]\r\nPlacement=\u{1}\u{2}\u{3}\r\nWindow List=\u{1}\u{2}\r\nWorkspace Path=C:\\x\r\nRight Side Panel=1\r\nTabSize=2\r\n", bom));
        w("restore_u.ini", utf16("[Restore]\r\nCount=2\r\n[Restore\\1]\r\nFile=C:\\w\\one.txt\r\nLine=4\r\nPlacement=1,2\r\n[Restore\\2]\r\nFile=C:\\w\\two.txt\r\nLine=9\r\n", bom));
        w("workspace_u.ini", utf16("[Workspace]\r\nCount=1\r\n[Workspace\\1]\r\nFile=C:\\w\\one.txt\r\n", bom));
        w("snippets.json", br#"{"variables":{"lang":"en"},"html":{"snippets":{"a":"b","c":"d"},"abbreviations":{"x":"y"}}}"#.to_vec());
        w("emmet_epp.js", b"module.exports = {};".to_vec());
        w("shipped.ctl", b"#TITLE=S\r\n#T=x\r\nbody\r\n#\r\n".to_vec());
        w("mine.ctl", b"#TITLE=M\r\n#T=y\r\nbody2\r\n#\r\n".to_vec());
        d
    }

    #[test]
    fn real_layout_utf16_with_and_without_bom() {
        for bom in [true, false] {
            let d = user_folder(bom);
            let defaults = TempDir::new();
            fs::write(defaults.path().join("shipped.ctl"), b"#TITLE=S\n#T=x\nbody\n#\n").unwrap();
            let p = plan_import_with_defaults(d.path(), Some(defaults.path()), &known, "").unwrap();
            let s: toml::Table = out(&p, "settings.toml").parse().unwrap();
            assert_eq!(s["editor"]["tab_size"].as_integer(), Some(2), "bom={bom}: UTF-16 INI is read");
            assert_eq!(item(&p, "settings", "Placement").status, Status::Skipped);
            assert_eq!(item(&p, "settings", "Window List").status, Status::Skipped);
            assert_eq!(item(&p, "settings", "Workspace Path").status, Status::Skipped);
            // open files at last exit go to the recent list; the rest of the per-file state is skipped
            let recent: toml::Table = out(&p, "recent.toml").parse().unwrap();
            assert_eq!(recent["paths"].as_array().unwrap().len(), 2);
            assert_eq!(item(&p, "restore", "placement").status, Status::Skipped);
            assert_eq!(item(&p, "restore", "line").status, Status::Skipped);
            assert!(p.outputs.iter().any(|(r, _)| r == "ini/workspace_u.toml"));
            // Emmet data is kept, its script is not
            assert!(item(&p, "emmet", "snippets.json").detail.contains("2 syntaxes, 3 abbreviations"), "{:?}", item(&p, "emmet", "snippets.json"));
            assert_eq!(item(&p, "emmet", "emmet_epp.js").status, Status::Skipped);
            assert!(!p.outputs.iter().any(|(r, _)| r.contains("emmet_epp")));
            // a file identical to the shipped default is not imported again, a changed one is
            assert!(p.outputs.iter().any(|(r, _)| r == "cliptext/mine.toml"));
            assert!(!p.outputs.iter().any(|(r, _)| r == "cliptext/shipped.toml"));
            assert!(item(&p, "defaults", "shipped").detail.starts_with("1 file(s)"));
        }
    }

    #[test]
    fn without_defaults_folder_nothing_is_dropped() {
        let d = user_folder(true);
        let p = plan_import(d.path(), &known, "").unwrap();
        assert!(p.outputs.iter().any(|(r, _)| r == "cliptext/shipped.toml"));
    }
}
