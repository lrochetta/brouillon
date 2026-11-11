//! Importers for the settings of other editors, behind one [`Importer`] trait and a registry.
//!
//! Clean-room: every reader below is written from the observable data formats only (XML/JSON/INI
//! settings, shortcut maps, colour schemes, keyword lists, snippets, recent lists), documented in
//! internal design notes (not published). Nothing is read except in the editor's own per-user settings
//! folder, nothing is written there, and credentials / file contents / paths are never put in a
//! report. Anything not understood is reported as skipped with the reason, not guessed.
//!
//! Every importer produces the same [`Plan`] as the EditPlus one (outputs + imported / approximated /
//! skipped report), so the dry run, the apply step and the backups are shared.

use crate::editplus::decode_any;
use crate::editplus_full::{self as ep, Entry, Plan, Report, Status};
use crate::hardening::{too_large, MAX_ENTRIES, MAX_FILES};
use crate::stx::{self, BlockComment, Group, KeywordTable, LineComment};
use brouillon_core::{keymap, settings::set_raw_key, theme_import};
use brouillon_editor_engine::acp;
use std::collections::BTreeSet;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

/// Where per-user settings are looked for (injected so tests never touch real folders).
#[derive(Debug, Clone, Default)]
pub struct Env {
    pub appdata: Option<PathBuf>,
    /// Program folders (`ProgramFiles`...), only used to spot a portable install marker.
    pub program_dirs: Vec<PathBuf>,
}

impl Env {
    pub fn from_os() -> Self {
        let var = |n: &str| std::env::var_os(n).map(PathBuf::from);
        Self { appdata: var("APPDATA"), program_dirs: ["ProgramFiles", "ProgramFiles(x86)"].iter().filter_map(|n| var(n)).collect() }
    }
}

/// Inputs of a plan besides the source folder.
pub struct Ctx<'a> {
    /// Does this command id exist in the Brouillon registry?
    pub known: &'a dyn Fn(&str) -> bool,
    /// Current `settings.toml` of the destination (imported keys are merged into it).
    pub existing_settings: &'a str,
}

pub trait Importer {
    /// Stable id used by the UI and the command line.
    fn id(&self) -> &'static str;
    /// Name shown in the import dialog.
    fn label(&self) -> &'static str;
    /// The documented per-user folder, for hints.
    fn folder_hint(&self) -> &'static str;
    /// The per-user folder when this editor looks installed (read-only look).
    fn detect(&self, env: &Env) -> Option<PathBuf>;
    /// Dry-run plan: reads `dir`, writes nothing.
    fn plan(&self, dir: &Path, ctx: &Ctx) -> io::Result<Plan>;
}

pub fn registry() -> Vec<Box<dyn Importer>> {
    vec![Box::new(EditPlus), Box::new(NotepadPp), Box::new(Sublime), Box::new(EditPad)]
}

pub fn by_id(id: &str) -> Option<Box<dyn Importer>> {
    registry().into_iter().find(|i| i.id() == id)
}

/// One row of the survey: an editor, and where its settings were found (with how many items).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Found {
    pub id: &'static str,
    pub label: &'static str,
    pub folder_hint: &'static str,
    pub dir: Option<PathBuf>,
    /// Items (imported or approximated) a dry run would carry over; 0 when not found.
    pub count: usize,
}

pub fn carried(plan: &Plan) -> usize {
    plan.report.count(Status::Imported) + plan.report.count(Status::Approximated)
}

/// Every registered editor, with its folder and item count when detected.
pub fn survey(env: &Env) -> Vec<Found> {
    registry()
        .iter()
        .map(|i| {
            let dir = i.detect(env);
            let count = dir.as_deref().and_then(|d| i.plan(d, &Ctx { known: &|_| true, existing_settings: "" }).ok()).map_or(0, |p| carried(&p));
            Found { id: i.id(), label: i.label(), folder_hint: i.folder_hint(), dir, count }
        })
        .collect()
}

// ---------------------------------------------------------------------------
// EditPlus (wraps the existing importer)
// ---------------------------------------------------------------------------

struct EditPlus;

impl Importer for EditPlus {
    fn id(&self) -> &'static str {
        "editplus"
    }
    fn label(&self) -> &'static str {
        "EditPlus"
    }
    fn folder_hint(&self) -> &'static str {
        "%APPDATA%\\EditPlus"
    }
    fn detect(&self, env: &Env) -> Option<PathBuf> {
        env.appdata.as_ref().map(|p| p.join("EditPlus")).filter(|p| p.is_dir())
    }
    fn plan(&self, dir: &Path, ctx: &Ctx) -> io::Result<Plan> {
        let (user, program) = ep::detect_dirs();
        let defaults = user.filter(|u| u == dir).and(program);
        ep::plan_import_with_defaults(dir, defaults.as_deref().filter(|d| d.is_dir()), ctx.known, ctx.existing_settings)
    }
}

// ---------------------------------------------------------------------------
// Small readers (no XML/JSON-with-comments crate: bounded, no entity expansion)
// ---------------------------------------------------------------------------

#[derive(Debug, Default, Clone)]
struct Tag {
    name: String,
    attrs: Vec<(String, String)>,
    close: bool,
    /// Text up to the next tag (CDATA kept as is).
    text: String,
}

impl Tag {
    fn attr(&self, k: &str) -> Option<&str> {
        self.attrs.iter().find(|(n, _)| n == k).map(|(_, v)| v.as_str())
    }
}

fn unescape(s: &str) -> String {
    s.replace("&lt;", "<").replace("&gt;", ">").replace("&quot;", "\"").replace("&apos;", "'").replace("&amp;", "&")
}

/// Flat list of start/end tags in document order. Comments, declarations and DOCTYPE are skipped;
/// only the five predefined entities are decoded.
fn scan_xml(text: &str) -> Vec<Tag> {
    let mut out = Vec::new();
    let b = text.as_bytes();
    let mut i = 0;
    while let Some(off) = text[i..].find('<') {
        i += off;
        if out.len() >= MAX_ENTRIES {
            break;
        }
        let rest = &text[i..];
        if rest.starts_with("<!--") {
            i += rest.find("-->").map_or(rest.len(), |e| e + 3);
            continue;
        }
        if rest.starts_with("<![CDATA[") || rest.starts_with("<?") || rest.starts_with("<!") {
            i += rest.find('>').map_or(rest.len(), |e| e + 1);
            continue;
        }
        // tag end: first `>` outside quotes
        let (mut j, mut q) = (i + 1, 0u8);
        while j < b.len() {
            match (q, b[j]) {
                (0, b'"' | b'\'') => q = b[j],
                (x, c) if x != 0 && c == x => q = 0,
                (0, b'>') => break,
                _ => {}
            }
            j += 1;
        }
        if j >= b.len() {
            break;
        }
        let inner = &text[i + 1..j];
        let close = inner.starts_with('/');
        let inner = inner.trim_start_matches('/').trim_end_matches('/');
        let name_end = inner.find(|c: char| c.is_whitespace()).unwrap_or(inner.len());
        let mut tag = Tag { name: inner[..name_end].to_string(), close, ..Default::default() };
        let mut rest = inner[name_end..].trim_start();
        while let Some(eq) = rest.find('=') {
            let key = rest[..eq].trim().to_string();
            let after = rest[eq + 1..].trim_start();
            let Some(quote) = after.chars().next().filter(|c| *c == '"' || *c == '\'') else { break };
            let Some(end) = after[1..].find(quote) else { break };
            tag.attrs.push((key, unescape(&after[1..1 + end])));
            rest = after[end + 2..].trim_start();
        }
        let after = &text[j + 1..];
        let tend = after.find('<').unwrap_or(after.len());
        let raw = &after[..tend];
        tag.text = unescape(raw.trim());
        out.push(tag);
        i = j + 1;
    }
    out
}

/// JSON with `//` and `/* */` comments and trailing commas (the Sublime flavour).
fn parse_json_lenient(text: &str) -> Option<serde_json::Value> {
    let text = text.trim_start_matches('\u{feff}');
    let mut s = String::with_capacity(text.len());
    let (mut chars, mut in_str, mut esc) = (text.chars().peekable(), false, false);
    while let Some(c) = chars.next() {
        if in_str {
            s.push(c);
            match (esc, c) {
                (true, _) => esc = false,
                (false, '\\') => esc = true,
                (false, '"') => in_str = false,
                _ => {}
            }
            continue;
        }
        match (c, chars.peek()) {
            ('"', _) => {
                in_str = true;
                s.push(c);
            }
            ('/', Some('/')) => {
                while chars.peek().is_some_and(|c| *c != '\n') {
                    chars.next();
                }
            }
            ('/', Some('*')) => {
                chars.next();
                let mut prev = ' ';
                for c in chars.by_ref() {
                    if prev == '*' && c == '/' {
                        break;
                    }
                    prev = c;
                }
            }
            _ => s.push(c),
        }
    }
    // trailing commas before `]` / `}`
    let t: Vec<char> = s.chars().collect();
    let mut out = String::with_capacity(t.len());
    let mut in_str = false;
    let mut esc = false;
    for (i, &c) in t.iter().enumerate() {
        if in_str {
            match (esc, c) {
                (true, _) => esc = false,
                (false, '\\') => esc = true,
                (false, '"') => in_str = false,
                _ => {}
            }
        } else if c == '"' {
            in_str = true;
        } else if c == ',' && t[i + 1..].iter().find(|x| !x.is_whitespace()).is_some_and(|x| *x == ']' || *x == '}') {
            continue;
        }
        out.push(c);
    }
    serde_json::from_str(&out).ok()
}

fn read_text(path: &Path, label: &str, report: &mut Report) -> Option<String> {
    if too_large(path) {
        report.add("files", label, Status::Skipped, "larger than 4 MiB: not read");
        return None;
    }
    match fs::read(path) {
        Ok(b) => Some(decode_any(&b)),
        Err(_) => {
            report.add("files", label, Status::Skipped, "could not be read");
            None
        }
    }
}

fn files_in(dir: &Path) -> Vec<String> {
    let mut v: Vec<String> = fs::read_dir(dir)
        .into_iter()
        .flatten()
        .filter_map(Result::ok)
        .filter(|e| e.path().is_file())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    v.sort_by_key(|n| n.to_lowercase());
    v.truncate(MAX_FILES);
    v
}

fn slug(s: &str) -> String {
    let v: String = s.chars().map(|c| if c.is_ascii_alphanumeric() { c.to_ascii_lowercase() } else { '-' }).collect();
    let v = v.trim_matches('-').to_string();
    if v.is_empty() { "item".into() } else { v.chars().take(40).collect() }
}

fn unique(used: &mut BTreeSet<String>, base: String) -> String {
    let mut n = base.clone();
    let mut k = 2;
    while !used.insert(n.clone()) {
        n = format!("{base}-{k}");
        k += 1;
    }
    n
}

fn raw_str(s: &str) -> String {
    toml::Value::String(s.to_string()).to_string()
}

/// Settings writer shared by the editors: one key, one report row.
struct Sink<'a> {
    text: &'a mut String,
    report: &'a mut Report,
    done: BTreeSet<(&'static str, &'static str)>,
}

impl Sink<'_> {
    fn set(&mut self, name: &str, section: &'static str, key: &'static str, raw: String, status: Status, note: &str) {
        if !self.done.insert((section, key)) {
            self.report.add("settings", name, Status::Skipped, format!("`{section}.{key}` already set from an earlier key"));
            return;
        }
        *self.text = set_raw_key(self.text, section, key, &raw);
        let d = if note.is_empty() { format!("{section}.{key} = {raw}") } else { format!("{section}.{key} = {raw} ({note})") };
        self.report.add("settings", name, status, d);
    }
    fn skip(&mut self, name: &str, why: &str) {
        self.report.add("settings", name, Status::Skipped, why);
    }
    fn size(&mut self, name: &str, key: &'static str, v: i64, lo: i64, hi: i64, status: Status, note: &str) {
        self.set(name, "editor", key, v.abs().clamp(lo, hi).to_string(), status, note);
    }
}

const MAX_RECENT: usize = 10;

/// recent.toml from a list of paths. Paths are private: rows carry only the rank.
fn recent_toml(paths: Vec<String>, report: &mut Report) -> Option<String> {
    let mut keep: Vec<String> = Vec::new();
    for p in paths {
        if p.trim().is_empty() || keep.contains(&p) {
            continue;
        }
        if keep.len() < MAX_RECENT {
            keep.push(p);
            report.add("recent", format!("recent file {}", keep.len()), Status::Imported, "recent.toml");
        }
    }
    if keep.is_empty() {
        return None;
    }
    let mut t = toml::Table::new();
    t.insert("paths".into(), toml::Value::Array(keep.into_iter().map(toml::Value::String).collect()));
    Some(toml::to_string(&t).unwrap_or_default())
}

/// keymap.toml from (shown name, chord text, Brouillon command if mapped) rows.
fn keymap_toml(rows: Vec<(String, String, Option<String>)>, known: &dyn Fn(&str) -> bool, source: &str, report: &mut Report) -> Option<String> {
    let mut bindings: Vec<keymap::Binding> = Vec::new();
    for (name, chord, cmd) in rows {
        let Some(id) = cmd.filter(|c| known(c)) else {
            report.add("keys", name, Status::Skipped, "no Brouillon command for it");
            continue;
        };
        let Some(chord) = ep::normalize_chord(&chord) else {
            report.add("keys", name, Status::Skipped, format!("`{chord}` is not a key chord Brouillon can bind"));
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
    (!bindings.is_empty()).then(|| format!("# Imported from {source} keyboard settings.\n{}", keymap::serialize_bindings(&bindings)))
}

fn theme_output(text: &str, name: &str, prefix: &str, used: &mut BTreeSet<String>, outputs: &mut Vec<(String, String)>, report: &mut Report, from: fn(&str, &str) -> Result<(theme_import::ImportedTheme, theme_import::ImportReport), String>) {
    match from(text, name) {
        Ok((t, r)) if !t.colors.is_empty() => {
            let stem = unique(used, format!("{prefix}-{}", slug(name)));
            outputs.push((format!("themes/{stem}.toml"), t.to_toml()));
            report.add("themes", name, Status::Approximated, format!("{} colours -> themes/{stem}.toml; {} item(s) without a slot; Brouillon does not load user theme files yet", t.colors.len(), r.unknown.len()));
        }
        Ok(_) => report.add("themes", name, Status::Skipped, "no colour that maps to a Brouillon theme slot"),
        Err(e) => report.add("themes", name, Status::Skipped, format!("not a readable colour scheme: {}", crate::hardening::safe_label(&e))),
    }
}

fn settings_output(outputs: &mut Vec<(String, String)>, text: String, existing: &str) {
    if text != existing {
        outputs.push(("settings.toml".into(), text));
    }
}

// ---------------------------------------------------------------------------
// Notepad++
// ---------------------------------------------------------------------------

struct NotepadPp;

/// Windows virtual-key code (decimal, as stored in shortcut maps) to a key name.
fn vk_name(k: u32) -> Option<String> {
    Some(match k {
        8 => "backspace".into(),
        9 => "tab".into(),
        13 => "enter".into(),
        27 => "escape".into(),
        32 => "space".into(),
        33 => "pageup".into(),
        34 => "pagedown".into(),
        35 => "end".into(),
        36 => "home".into(),
        37 => "left".into(),
        38 => "up".into(),
        39 => "right".into(),
        40 => "down".into(),
        45 => "insert".into(),
        46 => "delete".into(),
        48..=57 | 65..=90 => char::from_u32(k)?.to_ascii_lowercase().to_string(),
        112..=135 => format!("f{}", k - 111),
        _ => return None,
    })
}

/// Numeric command ids whose meaning is visible in the study of the shipped files.
const NPP_COMMAND_IDS: &[(&str, &str)] = &[("41001", "file_new"), ("41006", "file_save")];

/// Run-command variables, rewritten to the names the shared tool converter knows.
fn npp_vars(s: &str) -> String {
    [
        ("$(FULL_CURRENT_PATH)", "$(FilePath)"),
        ("$(CURRENT_DIRECTORY)", "$(FileDir)"),
        ("$(FILE_NAME)", "$(FileName)"),
        ("$(NAME_PART)", "$(FileNameNoExt)"),
        ("$(EXT_PART)", "$(FileExt)"),
        ("$(CURRENT_WORD)", "$(CurWord)"),
        ("$(CURRENT_LINE)", "$(CurLine)"),
        ("$(CURRENT_COLUMN)", "$(CurCol)"),
    ]
    .iter()
    .fold(s.to_string(), |acc, (a, b)| acc.replace(a, b))
}

fn yes(v: Option<&str>) -> Option<bool> {
    match v?.trim().to_ascii_lowercase().as_str() {
        "yes" | "show" | "true" | "1" => Some(true),
        "no" | "hide" | "false" | "0" => Some(false),
        _ => None,
    }
}

impl Importer for NotepadPp {
    fn id(&self) -> &'static str {
        "notepadpp"
    }
    fn label(&self) -> &'static str {
        "Notepad++"
    }
    fn folder_hint(&self) -> &'static str {
        "%APPDATA%\\Notepad++"
    }
    fn detect(&self, env: &Env) -> Option<PathBuf> {
        let has_data = |p: &PathBuf| p.is_dir() && ["config.xml", "shortcuts.xml", "session.xml", "stylers.xml"].iter().any(|f| p.join(f).is_file());
        // A portable install keeps its settings beside the program (empty marker file).
        let portable = env.program_dirs.iter().map(|p| p.join("Notepad++")).find(|p| p.join("doLocalConf.xml").is_file() && has_data(p));
        env.appdata.as_ref().map(|p| p.join("Notepad++")).filter(has_data).or(portable)
    }

    fn plan(&self, dir: &Path, ctx: &Ctx) -> io::Result<Plan> {
        let (mut report, mut outputs) = (Report::default(), Vec::new());
        let mut settings = ctx.existing_settings.to_string();
        let mut recent: Vec<String> = Vec::new();
        let mut used = BTreeSet::new();
        let mut any = false;
        let mut sink_done = BTreeSet::new();

        // stylers.xml: default font + theme
        if let Some(t) = dir.join("stylers.xml").is_file().then(|| read_text(&dir.join("stylers.xml"), "stylers.xml", &mut report)).flatten() {
            any = true;
            let mut sink = Sink { text: &mut settings, report: &mut report, done: std::mem::take(&mut sink_done) };
            if let Some(d) = scan_xml(&t).into_iter().find(|x| x.name == "WidgetStyle" && x.attr("name") == Some("Default Style")) {
                if let Some(f) = d.attr("fontName").filter(|f| !f.is_empty()) {
                    sink.set("stylers.xml: Default Style font name", "editor", "font_family", raw_str(f), Status::Imported, "");
                }
                if let Some(n) = d.attr("fontSize").and_then(|v| v.trim().parse::<i64>().ok()) {
                    sink.size("stylers.xml: Default Style font size", "font_size", n, 6, 72, Status::Imported, "");
                }
            }
            sink_done = sink.done;
            theme_output(&t, "stylers", "npp", &mut used, &mut outputs, &mut report, |t, n| Ok(theme_import::from_notepadpp_xml(t, n)));
        }

        // config.xml: preferences + recent files
        if dir.join("config.xml").is_file() {
            if let Some(t) = read_text(&dir.join("config.xml"), "config.xml", &mut report) {
                any = true;
                let mut sink = Sink { text: &mut settings, report: &mut report, done: std::mem::take(&mut sink_done) };
                for g in scan_xml(&t) {
                    if g.close {
                        continue;
                    }
                    match g.name.as_str() {
                        "File" => {
                            if let Some(f) = g.attr("filename") {
                                recent.push(f.to_string());
                            }
                        }
                        "GUIConfig" => npp_gui_config(&g, &mut sink),
                        _ => {}
                    }
                }
                sink_done = sink.done;
            }
        }

        // session.xml: files open at last exit join the recent list
        if dir.join("session.xml").is_file() {
            if let Some(t) = read_text(&dir.join("session.xml"), "session.xml", &mut report) {
                any = true;
                recent.extend(scan_xml(&t).into_iter().filter(|x| x.name == "File" && !x.close).filter_map(|x| x.attr("filename").map(String::from)));
                report.add("session", "session.xml view/caret/fold state", Status::Skipped, "per-file view state: Brouillon restores its own session instead");
            }
        }
        let _ = sink_done;

        // shortcuts.xml
        let mut tools: Vec<Entry> = Vec::new();
        let mut keys: Vec<(String, String, Option<String>)> = Vec::new();
        if dir.join("shortcuts.xml").is_file() {
            if let Some(t) = read_text(&dir.join("shortcuts.xml"), "shortcuts.xml", &mut report) {
                any = true;
                npp_shortcuts(&t, &mut keys, &mut tools, &mut report);
            }
        }
        if let Some(k) = keymap_toml(keys, ctx.known, "Notepad++", &mut report) {
            outputs.push(("keymap.toml".into(), k));
        }
        if let Some(t) = (!tools.is_empty()).then(|| ep::import_tools(&tools, &mut report)).flatten() {
            outputs.push(("tools.toml".into(), t.replacen("EditPlus", "Notepad++", 1)));
        }
        if let Some(r) = recent_toml(recent, &mut report) {
            outputs.push(("recent.toml".into(), r));
        }

        // themes/*.xml
        let themes = dir.join("themes");
        for f in files_in(&themes).into_iter().filter(|f| f.to_lowercase().ends_with(".xml")).take(60) {
            any = true;
            if let Some(t) = read_text(&themes.join(&f), &format!("themes/{f}"), &mut report) {
                let name = f.trim_end_matches(".xml");
                theme_output(&t, name, "npp", &mut used, &mut outputs, &mut report, |t, n| Ok(theme_import::from_notepadpp_xml(t, n)));
            }
        }

        // user-defined languages
        let mut udl_files = vec![dir.join("userDefineLang.xml")];
        let udir = dir.join("userDefineLangs");
        udl_files.extend(files_in(&udir).into_iter().filter(|f| f.to_lowercase().ends_with(".xml")).map(|f| udir.join(f)));
        for p in udl_files.into_iter().filter(|p| p.is_file()).take(60) {
            let label = p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            if let Some(t) = read_text(&p, &label, &mut report) {
                any = true;
                npp_udl(&t, &mut used, &mut outputs, &mut report);
            }
        }

        for (f, why) in [
            ("contextMenu.xml", "editor pop-up menu layout: Brouillon's menus are fixed"),
            ("toolbarButtonsConf.xml", "toolbar layout: toolbars are not configurable yet"),
            ("langs.xml", "language catalogue: extension lists are not carried"),
            ("nativeLang.xml", "interface translation: not carried"),
        ] {
            if dir.join(f).is_file() {
                any = true;
                report.add("files", f, Status::Skipped, why);
            }
        }
        for sub in ["plugins", "functionList", "autoCompletion"] {
            if dir.join(sub).is_dir() {
                report.add("files", format!("{sub}/"), Status::Skipped, if sub == "plugins" { "plugins run native code: never read or copied" } else { "no Brouillon equivalent" });
            }
        }
        if !any {
            report.add("files", "Notepad++ folder", Status::Skipped, "no config.xml, shortcuts.xml, session.xml, stylers.xml, themes or user-defined languages found");
        }
        settings_output(&mut outputs, settings, ctx.existing_settings);
        outputs.sort_by(|a, b| a.0.cmp(&b.0));
        Ok(Plan { outputs, report })
    }
}

fn npp_gui_config(g: &Tag, s: &mut Sink) {
    let name = g.attr("name").unwrap_or("").to_string();
    let label = |a: &str| format!("config.xml: {name} {a}");
    match name.as_str() {
        "TabSetting" => {
            if let Some(n) = g.attr("size").and_then(|v| v.trim().parse::<i64>().ok()) {
                s.size(&label("size"), "tab_size", n, 1, 16, Status::Imported, "");
                s.size(&label("size (as indent width)"), "indent_size", n, 1, 16, Status::Approximated, "Notepad++ indents by the tab width");
            }
            if let Some(b) = yes(g.attr("replaceBySpace")) {
                s.set(&label("replaceBySpace"), "editor", "insert_spaces", b.to_string(), Status::Imported, "");
            }
        }
        "ScintillaPrimaryView" => {
            if let Some(b) = yes(g.attr("lineNumberMargin")) {
                s.set(&label("lineNumberMargin"), "editor", "line_numbers", b.to_string(), Status::Imported, "");
            }
            if let Some(b) = yes(g.attr("Wrap")) {
                s.set(&label("Wrap"), "editor", "word_wrap", b.to_string(), Status::Imported, "");
            }
            let other = g.attrs.iter().filter(|(k, _)| !matches!(k.as_str(), "name" | "lineNumberMargin" | "Wrap")).count();
            if other > 0 {
                s.skip(&label(&format!("other view options (x{other})")), "no Brouillon equivalent (margins, guides, edge, zoom, white space)");
            }
        }
        "MaintainIndent" => {
            if let Some(b) = yes(Some(g.text.as_str())) {
                s.set(&label("value"), "editor", "auto_indent", b.to_string(), Status::Imported, "");
            }
        }
        "Backup" => {
            if let Some(a) = g.attr("action").and_then(|v| v.trim().parse::<i64>().ok()) {
                s.set(&label("action"), "backup", "enabled", (a != 0).to_string(), Status::Approximated, "any backup-on-save mode switches Brouillon's backup on");
            }
        }
        "NewDocDefaultSettings" => s.skip(&label("encoding / line ending"), "stored as numeric ids: not interpreted, not guessed"),
        "" => {}
        _ => s.skip(&format!("config.xml: {name}"), "no Brouillon equivalent"),
    }
}

fn npp_shortcuts(text: &str, keys: &mut Vec<(String, String, Option<String>)>, tools: &mut Vec<Entry>, report: &mut Report) {
    let mut section = String::new();
    let mut n_tools = 0u32;
    let (mut macros, mut plugin, mut scintilla) = (0, 0, 0);
    let chord_of = |t: &Tag| -> Option<String> {
        let vk = vk_name(t.attr("Key")?.trim().parse().ok()?)?;
        let mut parts = Vec::new();
        for (a, m) in [("Ctrl", "ctrl"), ("Alt", "alt"), ("Shift", "shift")] {
            if yes(t.attr(a)) == Some(true) {
                parts.push(m.to_string());
            }
        }
        parts.push(vk);
        Some(parts.join("+"))
    };
    for t in scan_xml(text) {
        if matches!(t.name.as_str(), "InternalCommands" | "Macros" | "UserDefinedCommands" | "PluginCommands" | "ScintillaKeys") {
            section = if t.close { String::new() } else { t.name.clone() };
            continue;
        }
        if t.close {
            continue;
        }
        match (section.as_str(), t.name.as_str()) {
            ("InternalCommands", "Command") => {
                let id = t.attr("id").unwrap_or("").to_string();
                let cmd = NPP_COMMAND_IDS.iter().find(|(k, _)| *k == id).map(|(_, c)| c.to_string());
                match chord_of(&t) {
                    Some(c) => keys.push((format!("shortcuts.xml: command {id}"), c, cmd)),
                    None => report.add("keys", format!("shortcuts.xml: command {id}"), Status::Skipped, "key code is not one Brouillon can bind"),
                }
            }
            ("Macros", "Macro") => macros += 1,
            ("UserDefinedCommands", "Command") => {
                n_tools += 1;
                let line = npp_vars(&t.text);
                let line = line.trim();
                let (cmd, args) = match line.strip_prefix('"') {
                    Some(r) => r.split_once('"').map_or((r, ""), |(c, a)| (c, a.trim())),
                    None => line.split_once(char::is_whitespace).map_or((line, ""), |(c, a)| (c, a.trim())),
                };
                let name = t.attr("name").unwrap_or("").to_string();
                let sec = "User Tools";
                for (k, v) in [("Menu", name), ("Command", cmd.to_string()), ("Argument", args.to_string())] {
                    tools.push(Entry { section: sec.into(), key: format!("{k}{n_tools}"), value: v });
                }
                if t.attr("Key").is_some_and(|k| k != "0") {
                    report.add("keys", format!("run command {n_tools} shortcut"), Status::Skipped, "tool shortcuts are the numbered slots Ctrl+1..0 in Brouillon");
                }
            }
            ("PluginCommands", "Command") => plugin += 1,
            ("ScintillaKeys", "ScintillaKey") => scintilla += 1,
            _ => {}
        }
    }
    if macros > 0 {
        report.add("macros", format!("{macros} macro(s)"), Status::Skipped, "recorded editor messages and command ids have no Brouillon equivalent");
    }
    if plugin > 0 {
        report.add("keys", format!("{plugin} plugin shortcut(s)"), Status::Skipped, "plugins are not supported");
    }
    if scintilla > 0 {
        report.add("keys", format!("{scintilla} Scintilla key(s)"), Status::Skipped, "editor-engine key bindings: no equivalent");
    }
}

fn npp_udl(text: &str, used: &mut BTreeSet<String>, outputs: &mut Vec<(String, String)>, report: &mut Report) {
    let mut defs: Vec<(String, String, KeywordTable)> = Vec::new();
    for t in scan_xml(text) {
        if t.close {
            continue;
        }
        match t.name.as_str() {
            "UserLang" => defs.push((t.attr("name").unwrap_or("").to_string(), t.attr("ext").unwrap_or("").to_string(), KeywordTable { title: t.attr("name").unwrap_or("").to_string(), case_sensitive: true, ..Default::default() })),
            "Global" => {
                if let Some(d) = defs.last_mut() {
                    d.2.case_sensitive = yes(t.attr("caseIgnored")) != Some(true);
                }
            }
            "Keywords" => {
                let Some(d) = defs.last_mut() else { continue };
                let words: Vec<String> = t.text.split_whitespace().map(String::from).collect();
                match t.attr("name").unwrap_or("") {
                    n if n.starts_with("Keywords") && !words.is_empty() => d.2.groups.push(Group { name: n.to_string(), words }),
                    "Comments" => {
                        for w in &words {
                            let (slot, v) = w.split_at(w.len().min(2));
                            let v = v.replace("((EOL))", "");
                            match (slot, v.is_empty()) {
                                ("00", false) => d.2.line_comments.push(LineComment { start: v, column0_only: false }),
                                ("03", false) => d.2.block_comments.push(BlockComment { open: v, close: String::new() }),
                                ("04", false) => {
                                    if let Some(b) = d.2.block_comments.last_mut().filter(|b| b.close.is_empty()) {
                                        b.close = v;
                                    }
                                }
                                _ => {}
                            }
                        }
                        d.2.block_comments.retain(|b| !b.close.is_empty());
                    }
                    "Delimiters" => {
                        // 8 pairs x [open, escape, close], slots 00..23
                        let slot = |p: usize, i: usize| {
                            let key = format!("{:02}", p * 3 + i);
                            words.iter().find_map(|w| w.strip_prefix(key.as_str())).map(|v| v.replace("((EOL))", "")).unwrap_or_default()
                        };
                        for p in 0..8 {
                            let (o, c) = (slot(p, 0), slot(p, 2));
                            if !o.is_empty() && o == c && o.chars().count() == 1 && !d.2.quotes.contains(&o) {
                                d.2.quotes.push(o);
                            }
                        }
                    }
                    _ => {}
                }
            }
            _ => {}
        }
    }
    for (name, ext, table) in defs {
        let label = if name.is_empty() { "unnamed language".to_string() } else { name.clone() };
        if table.groups.is_empty() && table.line_comments.is_empty() && table.block_comments.is_empty() {
            report.add("syntax", label, Status::Skipped, "no keyword list or comment marker");
            continue;
        }
        let mut def: toml::Table = stx::to_definition_toml(&table).parse().unwrap_or_default();
        let exts: Vec<toml::Value> = ext.split_whitespace().map(|e| toml::Value::String(e.trim_start_matches('.').to_lowercase())).collect();
        if !exts.is_empty() {
            def.insert("extensions".into(), toml::Value::Array(exts));
        }
        let stem = unique(used, format!("npp-{}", slug(&label)));
        let words: usize = table.groups.iter().map(|g| g.words.len()).sum();
        outputs.push((format!("syntax/{stem}.toml"), format!("# Imported from a Notepad++ user-defined language\n{}", toml::to_string(&def).unwrap_or_default())));
        report.add("syntax", label, Status::Approximated, format!("{} keyword groups, {words} words, comments and quotes -> syntax/{stem}.toml; folding, number rules, operators and styles are not carried", table.groups.len()));
    }
}

// ---------------------------------------------------------------------------
// Sublime Text
// ---------------------------------------------------------------------------

struct Sublime;

/// Sublime command names (as written in key maps) to Brouillon command ids.
const SUBLIME_COMMANDS: &[(&str, &str)] = &[
    ("new_file", "file_new"), ("prompt_open_file", "file_open"), ("save", "file_save"), ("prompt_save_as", "file_save_as"),
    ("close", "file_close"), ("undo", "edit_undo"), ("redo", "edit_redo"), ("cut", "edit_cut"), ("copy", "edit_copy"),
    ("paste", "edit_paste"), ("select_all", "select_all"), ("duplicate_line", "duplicate_line"), ("join_lines", "join_lines"),
    ("next_view", "tab_next"), ("prev_view", "tab_previous"), ("find_next", "search_find_next"), ("find_prev", "search_find_prev"),
    ("toggle_bookmark", "bookmark_toggle"), ("next_bookmark", "bookmark_next"), ("prev_bookmark", "bookmark_prev"),
    ("clear_bookmarks", "bookmark_clear"), ("swap_case", "case_invert"), ("title_case", "case_capitalize"),
];

fn sublime_command(cmd: &str, args: Option<&serde_json::Value>) -> Option<String> {
    let arg = |k: &str| args.and_then(|a| a.get(k));
    let s = |k: &str| arg(k).and_then(|v| v.as_str()).unwrap_or("");
    let id = match cmd {
        "show_panel" => match s("panel") {
            "find" => "search_open_find",
            "replace" => "search_open_replace",
            "find_in_files" => "find_in_files",
            _ => return None,
        },
        "show_overlay" if s("overlay") == "goto" && s("show_files") != "true" => "go_to",
        "toggle_comment" if arg("block").and_then(|v| v.as_bool()) != Some(true) => "toggle_line_comment",
        "upper_case" => "case_upper",
        "lower_case" => "case_lower",
        c => SUBLIME_COMMANDS.iter().find(|(k, _)| *k == c)?.1,
    };
    Some(id.to_string())
}

impl Importer for Sublime {
    fn id(&self) -> &'static str {
        "sublime"
    }
    fn label(&self) -> &'static str {
        "Sublime Text"
    }
    fn folder_hint(&self) -> &'static str {
        "%APPDATA%\\Sublime Text"
    }
    fn detect(&self, env: &Env) -> Option<PathBuf> {
        let a = env.appdata.as_ref()?;
        ["Sublime Text", "Sublime Text 3"].iter().map(|n| a.join(n)).find(|p| p.join("Packages").join("User").is_dir())
    }

    fn plan(&self, dir: &Path, ctx: &Ctx) -> io::Result<Plan> {
        let (mut report, mut outputs) = (Report::default(), Vec::new());
        let user = if dir.join("Packages").join("User").is_dir() { dir.join("Packages").join("User") } else { dir.to_path_buf() };
        let mut settings = ctx.existing_settings.to_string();
        let mut used = BTreeSet::new();
        let mut keys: Vec<(String, String, Option<String>)> = Vec::new();
        let mut snippets = acp::AcpFile { title: "Sublime snippets".into(), ..Default::default() };
        let mut seen = 0usize;
        for f in files_in(&user) {
            let lower = f.to_lowercase();
            let path = user.join(&f);
            let ext = lower.rsplit_once('.').map_or("", |(_, e)| e);
            seen += 1;
            if lower == "preferences.sublime-settings" {
                if let Some(t) = read_text(&path, &f, &mut report) {
                    sublime_prefs(&t, &mut settings, &mut report);
                }
            } else if ext == "sublime-keymap" {
                let other = ["(linux)", "(osx)"].iter().any(|p| lower.contains(p));
                if other {
                    report.add("keys", f, Status::Skipped, "key map of another platform");
                } else if let Some(t) = read_text(&path, &f, &mut report) {
                    sublime_keymap(&t, &mut keys, &mut report);
                }
            } else if ext == "sublime-color-scheme" || ext == "tmtheme" {
                if let Some(t) = read_text(&path, &f, &mut report) {
                    let name = f.rsplit_once('.').map_or(f.as_str(), |(n, _)| n);
                    theme_output(&t, name, "sublime", &mut used, &mut outputs, &mut report, theme_import::from_textmate);
                }
            } else if ext == "sublime-snippet" {
                if let Some(t) = read_text(&path, &f, &mut report) {
                    sublime_snippet(&t, &f, &mut snippets, &mut report);
                }
            } else {
                let why = match ext {
                    "sublime-syntax" | "tmlanguage" => "scope-based grammar, not a keyword list: not converted",
                    "sublime-build" => "build system: use Brouillon's user tools instead",
                    "sublime-macro" => "macro of editor commands: no equivalent",
                    "sublime-project" | "sublime-workspace" => "project or workspace file: not carried",
                    "sublime-menu" | "sublime-commands" | "sublime-mousemap" | "sublime-theme" | "sublime-completions" | "tmpreferences" => "no Brouillon equivalent",
                    "py" | "pyc" => "plugin code: never read or copied",
                    "sublime-settings" => "settings of another package",
                    _ => "not a settings file Brouillon reads",
                };
                report.add("files", f, Status::Skipped, why);
            }
        }
        if !snippets.abbrevs.is_empty() {
            outputs.push(("abbrev/sublime-snippets.toml".into(), acp::to_toml(&snippets)));
        }
        if let Some(k) = keymap_toml(keys, ctx.known, "Sublime Text", &mut report) {
            outputs.push(("keymap.toml".into(), k));
        }
        // recent files from the session file, when it lists them
        let mut recent = Vec::new();
        for f in ["Local/Session.sublime_session", "Local/Auto Save Session.sublime_session"] {
            let p = dir.join(f);
            if p.is_file() {
                if let Some(v) = read_text(&p, f, &mut report).and_then(|t| parse_json_lenient(&t)) {
                    recent.extend(find_str_array(&v, "file_history", 3));
                }
            }
        }
        if let Some(r) = recent_toml(recent, &mut report) {
            outputs.push(("recent.toml".into(), r));
        }
        if seen == 0 && outputs.is_empty() {
            report.add("files", "Sublime Text folder", Status::Skipped, "no user settings, key map, colour scheme or snippet found (looked in Packages/User)");
        }
        if user.join("Packages").is_dir() || dir.join("Packages").join("User").is_dir() {
            let n = fs::read_dir(&user).into_iter().flatten().filter_map(Result::ok).filter(|e| e.path().is_dir()).count();
            if n > 0 {
                report.add("files", format!("{n} sub-folder(s) of Packages/User"), Status::Skipped, "plugins and package data are not scanned");
            }
        }
        settings_output(&mut outputs, settings, ctx.existing_settings);
        outputs.sort_by(|a, b| a.0.cmp(&b.0));
        Ok(Plan { outputs, report })
    }
}

fn find_str_array(v: &serde_json::Value, key: &str, depth: u8) -> Vec<String> {
    let Some(o) = v.as_object() else { return Vec::new() };
    if let Some(a) = o.get(key).and_then(|a| a.as_array()) {
        return a.iter().filter_map(|x| x.as_str().map(String::from)).collect();
    }
    if depth == 0 {
        return Vec::new();
    }
    o.values().flat_map(|c| find_str_array(c, key, depth - 1)).collect()
}

fn sublime_prefs(text: &str, settings: &mut String, report: &mut Report) {
    let Some(v) = parse_json_lenient(text) else {
        report.add("settings", "Preferences.sublime-settings", Status::Skipped, "not valid JSON");
        return;
    };
    let Some(obj) = v.as_object() else { return };
    let mut s = Sink { text: settings, report, done: BTreeSet::new() };
    for (k, val) in obj {
        let name = format!("Preferences: {}", crate::hardening::safe_label(k));
        let int = val.as_f64().map(|f| f.round() as i64);
        match (k.as_str(), val) {
            ("tab_size", _) if int.is_some() => {
                let n = int.unwrap_or(4);
                s.size(&name, "tab_size", n, 1, 16, Status::Imported, "");
                s.size(&format!("{name} (as indent width)"), "indent_size", n, 1, 16, Status::Approximated, "Sublime indents by the tab width");
            }
            ("translate_tabs_to_spaces", serde_json::Value::Bool(b)) => s.set(&name, "editor", "insert_spaces", b.to_string(), Status::Imported, ""),
            ("auto_indent", serde_json::Value::Bool(b)) => s.set(&name, "editor", "auto_indent", b.to_string(), Status::Imported, ""),
            ("line_numbers", serde_json::Value::Bool(b)) => s.set(&name, "editor", "line_numbers", b.to_string(), Status::Imported, ""),
            ("font_face", serde_json::Value::String(f)) if !f.is_empty() => s.set(&name, "editor", "font_family", raw_str(f), Status::Imported, ""),
            ("font_size", _) if int.is_some() => s.size(&name, "font_size", int.unwrap_or(12), 6, 72, Status::Imported, ""),
            ("word_wrap", serde_json::Value::Bool(b)) => s.set(&name, "editor", "word_wrap", b.to_string(), Status::Imported, ""),
            ("word_wrap", serde_json::Value::String(a)) if a == "auto" => s.set(&name, "editor", "word_wrap", "true".into(), Status::Approximated, "\"auto\" wraps in Brouillon"),
            ("wrap_width", _) if int.is_some() => {
                let n = int.unwrap_or(0);
                if n == 0 {
                    s.set(&name, "editor", "wrap_column", "0".into(), Status::Imported, "");
                } else {
                    s.size(&name, "wrap_column", n, 10, 1000, Status::Imported, "");
                }
            }
            ("default_encoding", serde_json::Value::String(e)) => match brouillon_core::codepages::supported(e.trim()) {
                Ok(cp) => s.set(&name, "files", "default_encoding", raw_str(cp.id), Status::Imported, ""),
                Err(_) => s.skip(&name, "encoding name is not one Brouillon can convert"),
            },
            ("color_scheme" | "light_color_scheme" | "dark_color_scheme" | "theme" | "light_theme" | "dark_theme", _) => {
                s.skip(&name, "refers to a scheme by package path, which Brouillon cannot resolve (scheme files in Packages/User are imported as themes)")
            }
            _ => s.skip(&name, "no Brouillon equivalent"),
        }
    }
}

fn sublime_keymap(text: &str, keys: &mut Vec<(String, String, Option<String>)>, report: &mut Report) {
    let Some(arr) = parse_json_lenient(text).and_then(|v| v.as_array().cloned()) else {
        report.add("keys", "key map", Status::Skipped, "not a JSON list of bindings");
        return;
    };
    for (i, b) in arr.iter().enumerate() {
        let name = format!("key map entry {}", i + 1);
        let ks: Vec<&str> = b.get("keys").and_then(|k| k.as_array()).map(|a| a.iter().filter_map(|x| x.as_str()).collect()).unwrap_or_default();
        let cmd = b.get("command").and_then(|c| c.as_str()).unwrap_or("");
        if b.get("context").and_then(|c| c.as_array()).is_some_and(|c| !c.is_empty()) {
            report.add("keys", format!("{name}: {cmd}"), Status::Skipped, "depends on a context (selection, scope, panel...): Brouillon bindings have none");
        } else if ks.len() != 1 {
            report.add("keys", format!("{name}: {cmd}"), Status::Skipped, "multi-key chord sequence: not supported");
        } else {
            keys.push((format!("{name}: {cmd} ({})", ks[0]), ks[0].to_string(), sublime_command(cmd, b.get("args"))));
        }
    }
}

/// Snippet body to the abbreviation grammar: the first tab stop becomes the caret marker, other
/// stops and variables are flattened to their default text. Returns true when something was flattened.
fn snippet_body(src: &str) -> (String, bool) {
    fn walk(s: &[char], i: &mut usize, out: &mut String, first: &mut Option<u32>, flat: &mut bool, until_brace: bool) {
        while *i < s.len() {
            let c = s[*i];
            if until_brace && c == '}' {
                return;
            }
            *i += 1;
            match c {
                '\\' if *i < s.len() && matches!(s[*i], '$' | '\\' | '}') => {
                    out.push(s[*i]);
                    *i += 1;
                }
                '^' => out.push_str("^^"),
                '$' if *i < s.len() => {
                    let braced = s[*i] == '{';
                    if braced {
                        *i += 1;
                    }
                    let start = *i;
                    while *i < s.len() && (s[*i].is_ascii_alphanumeric() || s[*i] == '_') {
                        *i += 1;
                    }
                    let id: String = s[start..*i].iter().collect();
                    let num = id.parse::<u32>().ok();
                    if num.is_none() {
                        *flat = true; // $TM_FILENAME and friends: dropped
                    }
                    let mut inner = String::new();
                    if braced {
                        if *i < s.len() && s[*i] == ':' {
                            *i += 1;
                            walk(s, i, &mut inner, first, flat, true);
                        }
                        while *i < s.len() && s[*i] != '}' {
                            *i += 1;
                        }
                        *i += 1; // closing brace
                    }
                    match num {
                        Some(n) if (n != 0 && first.is_none_or(|f| n < f)) || (n == 0 && first.is_none()) => {
                            *first = Some(if n == 0 { u32::MAX } else { n });
                            out.push_str("^!");
                            out.push_str(&inner);
                        }
                        Some(_) => {
                            *flat = true;
                            out.push_str(&inner);
                        }
                        None => out.push_str(&inner),
                    }
                }
                c => out.push(c),
            }
        }
    }
    let chars: Vec<char> = src.chars().collect();
    let (mut i, mut out, mut first, mut flat) = (0, String::new(), None, false);
    walk(&chars, &mut i, &mut out, &mut first, &mut flat, false);
    // only one caret marker survives: keep the first
    let mut seen = false;
    let out = out.split("^!").enumerate().fold(String::new(), |mut acc, (n, part)| {
        if n > 0 {
            acc.push_str(if seen { "" } else { "^!" });
            seen = true;
        }
        acc.push_str(part);
        acc
    });
    (out, flat)
}

fn sublime_snippet(text: &str, file: &str, out: &mut acp::AcpFile, report: &mut Report) {
    let trigger = scan_xml(text).into_iter().find(|t| t.name == "tabTrigger" && !t.close).map(|t| t.text).unwrap_or_default();
    let body = text.split_once("<![CDATA[").and_then(|(_, r)| r.split_once("]]>")).map(|(b, _)| b.to_string());
    match (trigger.trim(), body) {
        ("", _) => report.add("snippets", file, Status::Skipped, "no tab trigger: Brouillon expands abbreviations by word"),
        (_, None) => report.add("snippets", file, Status::Skipped, "no snippet content found"),
        (t, Some(b)) => {
            let (body, flat) = snippet_body(&b);
            out.abbrevs.push(acp::Abbrev { abbr: t.to_string(), body });
            let (st, d) = if flat { (Status::Approximated, "abbrev/sublime-snippets.toml; extra tab stops, mirrors and variables flattened to their default text") } else { (Status::Imported, "abbrev/sublime-snippets.toml") };
            report.add("snippets", file, st, d);
        }
    }
}

// ---------------------------------------------------------------------------
// EditPad Lite
// ---------------------------------------------------------------------------

struct EditPad;

impl Importer for EditPad {
    fn id(&self) -> &'static str {
        "editpad"
    }
    fn label(&self) -> &'static str {
        "EditPad Lite"
    }
    fn folder_hint(&self) -> &'static str {
        "%APPDATA%\\JGsoft\\EditPad Lite 8"
    }
    fn detect(&self, env: &Env) -> Option<PathBuf> {
        let a = env.appdata.as_ref()?.join("JGsoft");
        ["EditPad Lite 8", "EditPad Lite 7"].iter().map(|n| a.join(n)).find(|p| p.is_dir())
    }

    fn plan(&self, dir: &Path, ctx: &Ctx) -> io::Result<Plan> {
        let mut report = Report::default();
        let mut outputs = Vec::new();
        let mut settings = ctx.existing_settings.to_string();
        report.add("files", "EditPad Lite settings format", Status::Skipped, "its settings location and file format are not publicly documented: only an exported .ini is read, and only keys whose name matches a known setting");
        let mut tmp = Report::default();
        let mut entries: Vec<Entry> = Vec::new();
        for f in files_in(dir) {
            if f.to_lowercase().ends_with(".ini") {
                if let Some(t) = read_text(&dir.join(&f), &f, &mut report) {
                    entries.extend(ep::read_ini(&t, &f, &mut tmp));
                }
            } else {
                report.add("files", f, Status::Skipped, "not a format Brouillon reads");
            }
        }
        let mut found = Report::default();
        ep::import_settings(&entries, &mut settings, &mut found);
        for mut i in found.items {
            if i.status == Status::Imported {
                i.status = Status::Approximated;
                i.detail.push_str("; matched by key name only");
            }
            report.items.push(i);
        }
        report.items.extend(tmp.items);
        settings_output(&mut outputs, settings, ctx.existing_settings);
        Ok(Plan { outputs, report })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::TempDir;

    fn known(id: &str) -> bool {
        id.chars().all(|c| c.is_ascii_lowercase() || c == '_')
    }

    fn plan_of(i: &dyn Importer, d: &Path) -> Plan {
        i.plan(d, &Ctx { known: &known, existing_settings: "" }).unwrap()
    }

    fn put(root: &Path, rel: &str, text: &str) {
        let p = root.join(rel);
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(p, text).unwrap();
    }

    fn out<'a>(p: &'a Plan, rel: &str) -> &'a str {
        p.outputs.iter().find(|(r, _)| r == rel).map(|(_, t)| t.as_str()).unwrap_or_else(|| panic!("no output {rel}: {:?}", p.outputs.iter().map(|o| &o.0).collect::<Vec<_>>()))
    }

    fn row<'a>(p: &'a Plan, needle: &str) -> &'a ep::Item {
        p.report.items.iter().find(|i| i.name.contains(needle)).unwrap_or_else(|| panic!("no row {needle}"))
    }

    const NPP_CONFIG: &str = r#"<?xml version="1.0" encoding="UTF-8" ?>
<NotepadPlus>
  <GUIConfigs>
    <GUIConfig name="TabSetting" size="3" replaceBySpace="yes" />
    <GUIConfig name="ScintillaPrimaryView" lineNumberMargin="hide" Wrap="yes" zoom="2" edge="line" />
    <GUIConfig name="MaintainIndent">no</GUIConfig>
    <GUIConfig name="Backup" action="2" />
    <GUIConfig name="NewDocDefaultSettings" format="0" encoding="4" />
    <GUIConfig name="DockingManager" leftWidth="200" />
  </GUIConfigs>
  <History nbMaxFile="10"><File filename="C:\secret\one.txt" /><File filename="C:\secret\two.txt" /></History>
</NotepadPlus>"#;

    fn npp_fixture() -> TempDir {
        let d = TempDir::new();
        put(d.path(), "config.xml", NPP_CONFIG);
        put(
            d.path(),
            "shortcuts.xml",
            r#"<NotepadPlus>
<InternalCommands><Command id="41006" Ctrl="yes" Alt="yes" Shift="no" Key="83" /><Command id="40000" Ctrl="yes" Alt="no" Shift="no" Key="113" /></InternalCommands>
<Macros><Macro name="m" Ctrl="no" Alt="no" Shift="no" Key="0"><Action type="0" message="2170" wParam="0" lParam="0" sParam="" /></Macro></Macros>
<UserDefinedCommands><Command name="Run it" Ctrl="no" Alt="no" Shift="no" Key="0">"C:\tools\x.exe" -f "$(FULL_CURRENT_PATH)" --odd $(NPP_DIRECTORY)</Command></UserDefinedCommands>
</NotepadPlus>"#,
        );
        put(d.path(), "session.xml", r#"<NotepadPlus><Session activeView="0"><mainView activeIndex="0"><File firstVisibleLine="0" filename="C:\secret\two.txt" /><File filename="C:\secret\three.txt" /></mainView></Session></NotepadPlus>"#);
        put(
            d.path(),
            "stylers.xml",
            r#"<NotepadPlus><LexerStyles/><GlobalStyles><WidgetStyle name="Default Style" styleID="32" fgColor="DCDCDC" bgColor="1E1E1E" fontName="Consolas" fontStyle="0" fontSize="11" /></GlobalStyles></NotepadPlus>"#,
        );
        put(d.path(), "themes/Mine.xml", r#"<NotepadPlus><GlobalStyles><WidgetStyle name="Default Style" fgColor="000000" bgColor="FFFFFF" /></GlobalStyles></NotepadPlus>"#);
        put(
            d.path(),
            "userDefineLang.xml",
            r#"<NotepadPlus><UserLang name="MiniLang" ext="mini ml" udlVersion="2.1"><Settings><Global caseIgnored="yes" /></Settings><KeywordLists>
<Keywords name="Comments">00// 01 02 03/* 04*/</Keywords><Keywords name="Keywords1">if else while</Keywords><Keywords name="Keywords2"></Keywords>
<Keywords name="Delimiters">00" 01 02" 03 04 05 06 07 08</Keywords></KeywordLists><Styles/></UserLang></NotepadPlus>"#,
        );
        put(d.path(), "plugins/x.dll", "binary");
        d
    }

    #[test]
    fn notepadpp_maps_settings_keys_tools_recent_themes_and_languages() {
        let d = npp_fixture();
        let p = plan_of(&NotepadPp, d.path());
        let s = out(&p, "settings.toml");
        for needle in ["tab_size = 3", "insert_spaces = true", "line_numbers = false", "word_wrap = true", "auto_indent = false", "enabled = true", "font_family = \"Consolas\"", "font_size = 11"] {
            assert!(s.contains(needle), "{needle} in {s}");
        }
        assert_eq!(row(&p, "TabSetting size").status, Status::Imported);
        assert_eq!(row(&p, "Backup").status, Status::Approximated);
        assert_eq!(row(&p, "NewDocDefaultSettings").status, Status::Skipped);
        assert_eq!(row(&p, "DockingManager").status, Status::Skipped);
        // keys: id 41006 -> file_save with ctrl+alt+s; unknown id and macro skipped with reasons
        assert!(out(&p, "keymap.toml").contains("ctrl+alt+s"));
        assert_eq!(row(&p, "command 40000").status, Status::Skipped);
        assert_eq!(row(&p, "macro").status, Status::Skipped);
        // user command -> tool, variables converted, unknown kept and flagged
        let tools = out(&p, "tools.toml");
        assert!(tools.contains("Run it") && tools.contains("x.exe") && tools.contains("${file}"), "{tools}");
        assert_eq!(row(&p, "Run it").status, Status::Approximated);
        // recent: config history then session, de-duplicated, paths never in the report
        let rec = out(&p, "recent.toml");
        assert_eq!(rec.matches("secret").count(), 3, "{rec}");
        assert!(!p.report.render(true, "x", &[]).contains("secret"));
        // themes and udl
        assert!(out(&p, "themes/npp-stylers.toml").contains("is_dark = true"));
        assert!(out(&p, "themes/npp-mine.toml").contains("#ffffff"));
        let syn = out(&p, "syntax/npp-minilang.toml");
        assert!(syn.contains("while") && syn.contains("mini") && syn.contains("//"), "{syn}");
        assert_eq!(row(&p, "plugins/").status, Status::Skipped);
    }

    #[test]
    fn notepadpp_detects_beside_the_program_when_portable_and_in_appdata() {
        let d = TempDir::new();
        let env = Env { appdata: Some(d.path().join("roaming")), program_dirs: vec![d.path().join("pf")] };
        assert!(NotepadPp.detect(&env).is_none());
        put(&d.path().join("roaming"), "Notepad++/config.xml", "<NotepadPlus/>");
        assert!(NotepadPp.detect(&env).is_some());
        let env2 = Env { appdata: None, program_dirs: vec![d.path().join("pf")] };
        assert!(NotepadPp.detect(&env2).is_none());
        put(&d.path().join("pf"), "Notepad++/doLocalConf.xml", "");
        assert!(NotepadPp.detect(&env2).is_none(), "marker alone is not enough");
        put(&d.path().join("pf"), "Notepad++/session.xml", "<NotepadPlus/>");
        assert!(NotepadPp.detect(&env2).is_some());
    }

    fn sublime_fixture() -> TempDir {
        let d = TempDir::new();
        let u = "Packages/User/";
        put(
            d.path(),
            &format!("{u}Preferences.sublime-settings"),
            "{\n // comment\n \"tab_size\": 2,\n \"translate_tabs_to_spaces\": true,\n \"font_face\": \"Fira Code\", \"font_size\": 13.0,\n \"word_wrap\": \"auto\", \"wrap_width\": 100,\n \"default_encoding\": \"windows-1252\", \"color_scheme\": \"Packages/X/Y.sublime-color-scheme\",\n \"draw_white_space\": \"all\", /* block */\n}\n",
        );
        put(
            d.path(),
            &format!("{u}Default (Windows).sublime-keymap"),
            r#"[
 { "keys": ["ctrl+shift+d"], "command": "duplicate_line" },
 { "keys": ["ctrl+alt+f"], "command": "show_panel", "args": {"panel": "find_in_files"} },
 { "keys": ["ctrl+k", "ctrl+u"], "command": "upper_case" },
 { "keys": ["tab"], "command": "next_field", "context": [{"key": "has_next_field"}] },
 { "keys": ["f5"], "command": "mystery" },
 { "keys": ["ctrl+shift+d"], "command": "join_lines" },
]"#,
        );
        put(d.path(), &format!("{u}Default (Linux).sublime-keymap"), "[]");
        put(d.path(), &format!("{u}Mine.sublime-color-scheme"), r##"{"name":"Mine","globals":{"background":"#101010","foreground":"#eeeeee"},"rules":[{"scope":"comment","foreground":"#888888"}]}"##);
        put(d.path(), &format!("{u}ok.sublime-snippet"), "<snippet><content><![CDATA[Ok(${1:result}, ${2:other}) $TM_FILENAME ^x $0]]></content><tabTrigger>ok</tabTrigger><scope>source.rust</scope></snippet>");
        put(d.path(), &format!("{u}notrig.sublime-snippet"), "<snippet><content><![CDATA[x]]></content></snippet>");
        put(d.path(), &format!("{u}plug.py"), "import os; os.system('boom')");
        put(d.path(), &format!("{u}x.sublime-build"), "{}");
        put(d.path(), &format!("{u}sub/deep.txt"), "x");
        put(d.path(), "Local/Session.sublime_session", r#"{"settings": {"file_history": ["C:\\secret\\a.txt", "C:\\secret\\b.txt"]}}"#);
        d
    }

    #[test]
    fn sublime_maps_preferences_keymap_scheme_snippets_and_recent() {
        let d = sublime_fixture();
        let p = plan_of(&Sublime, d.path());
        let s = out(&p, "settings.toml");
        for needle in ["tab_size = 2", "insert_spaces = true", "font_family = \"Fira Code\"", "font_size = 13", "word_wrap = true", "wrap_column = 100", "default_encoding"] {
            assert!(s.contains(needle), "{needle} in {s}");
        }
        assert_eq!(row(&p, "word_wrap").status, Status::Approximated);
        assert_eq!(row(&p, "color_scheme").status, Status::Skipped);
        assert_eq!(row(&p, "draw_white_space").status, Status::Skipped);
        let k = out(&p, "keymap.toml");
        assert!(k.contains("ctrl+shift+d") && k.contains("duplicate_line") && k.contains("find_in_files"), "{k}");
        assert!(row(&p, "upper_case").detail.contains("multi-key"));
        assert!(row(&p, "next_field").detail.contains("context"));
        assert!(row(&p, "mystery").detail.contains("no Brouillon command"));
        assert!(row(&p, "join_lines").detail.contains("already bound"));
        assert!(row(&p, "Linux").detail.contains("another platform"));
        assert!(out(&p, "themes/sublime-mine.toml").contains("#101010"));
        let a = out(&p, "abbrev/sublime-snippets.toml");
        assert!(a.contains("abbr = \"ok\"") && a.contains("Ok(^!result, other)") && a.contains("^^x"), "{a}");
        assert_eq!(row(&p, "ok.sublime-snippet").status, Status::Approximated);
        assert_eq!(row(&p, "notrig").status, Status::Skipped);
        assert!(row(&p, "plug.py").detail.contains("never read"));
        assert!(out(&p, "recent.toml").contains("a.txt"));
        assert!(!p.report.render(true, "x", &[]).contains("secret") && !p.report.render(true, "x", &[]).contains("boom"));
    }

    #[test]
    fn sublime_detection_needs_the_user_package_folder() {
        let d = TempDir::new();
        let env = Env { appdata: Some(d.path().to_path_buf()), program_dirs: vec![] };
        fs::create_dir_all(d.path().join("Sublime Text")).unwrap();
        assert!(Sublime.detect(&env).is_none());
        fs::create_dir_all(d.path().join("Sublime Text 3/Packages/User")).unwrap();
        assert_eq!(Sublime.detect(&env), Some(d.path().join("Sublime Text 3")));
    }

    #[test]
    fn editpad_reads_only_an_exported_ini_by_key_meaning_and_says_so() {
        let d = TempDir::new();
        put(d.path(), "export.ini", "[Options]\nTabSize=6\nPassword=hunter2\nMystery=1\n");
        put(d.path(), "other.bin", "x");
        let p = plan_of(&EditPad, d.path());
        assert!(out(&p, "settings.toml").contains("tab_size = 6"));
        assert_eq!(row(&p, "TabSize").status, Status::Approximated);
        assert_eq!(row(&p, "format").status, Status::Skipped);
        assert_eq!(row(&p, "Mystery").status, Status::Skipped);
        assert!(!p.report.render(true, "x", &[]).contains("hunter2"));
        // nothing to read: only the honest row
        let e = TempDir::new();
        let p = plan_of(&EditPad, e.path());
        assert!(p.outputs.is_empty() && p.report.count(Status::Skipped) == 1);
    }

    #[test]
    fn survey_copes_with_zero_one_and_several_editors_and_counts_items() {
        let d = TempDir::new();
        let env = Env { appdata: Some(d.path().to_path_buf()), program_dirs: vec![] };
        let s = survey(&env);
        assert_eq!(s.len(), 4);
        assert!(s.iter().all(|f| f.dir.is_none() && f.count == 0));
        put(d.path(), "Notepad++/config.xml", NPP_CONFIG);
        let s = survey(&env);
        let npp = s.iter().find(|f| f.id == "notepadpp").unwrap();
        assert!(npp.dir.is_some() && npp.count >= 6, "{npp:?}");
        assert_eq!(s.iter().filter(|f| f.dir.is_some()).count(), 1);
        put(d.path(), "Sublime Text/Packages/User/Preferences.sublime-settings", "{\"tab_size\": 8}");
        put(d.path(), "EditPlus/editplus.ini", "[Settings]\nTabSize=2\n");
        let s = survey(&env);
        assert_eq!(s.iter().filter(|f| f.dir.is_some()).count(), 3);
        assert!(by_id("sublime").is_some() && by_id("vim").is_none());
    }

    #[test]
    fn hostile_input_is_bounded_and_never_panics() {
        let d = TempDir::new();
        put(d.path(), "config.xml", &format!("<NotepadPlus>{}<GUIConfig name=\"TabSetting\" size=\"999\" replaceBySpace=\"maybe\"", "<GUIConfig name=\"a\"/>".repeat(70_000)));
        put(d.path(), "shortcuts.xml", "<<<>>> <Command id=\"1\" Key=\"zz\" &amp; </UserDefinedCommands>");
        put(d.path(), "stylers.xml", "\u{0}\u{0}<WidgetStyle name=\"Default Style\" fgColor=\"zzz\"");
        let p = plan_of(&NotepadPp, d.path());
        assert!(p.report.items.len() < 100_000);
        let s = TempDir::new();
        put(s.path(), "Packages/User/Preferences.sublime-settings", "{\"tab_size\": \"x\", \"font_size\": 1e99, [[[");
        put(s.path(), "Packages/User/a.sublime-keymap", "{\"not\": \"a list\"}");
        let _ = plan_of(&Sublime, s.path());
        assert_eq!(snippet_body("$").0, "$");
        assert_eq!(snippet_body("${1:${2:x}} ${").0.chars().filter(|c| *c == '^').count(), 1);
    }

    #[test]
    fn lenient_json_keeps_comment_markers_inside_strings() {
        let v = parse_json_lenient("{\"a\": \"http://x/*y*/\", // c\n \"b\": [1, 2,],}").unwrap();
        assert_eq!(v["a"], "http://x/*y*/");
        assert_eq!(v["b"].as_array().unwrap().len(), 2);
    }
}
