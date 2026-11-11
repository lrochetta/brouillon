//! Settings cascade (BK-072): default < platform < user < file-type < project,
//! each a TOML table deep-merged over the previous one.

use toml::{Table, Value};

/// Built-in defaults; also the schema used to report unknown keys.
pub const DEFAULTS_TOML: &str = r#"
[editor]
tab_size = 4
# Width of one indent level and what the Tab key / smart Enter insert.
indent_size = 4
insert_spaces = true
# Enter keeps the indent of the current line.
auto_indent = true
# Show the line numbers in the gutter (View > Toggle Line Numbers).
line_numbers = true
# Soft wrap of long lines. wrap_column 0 = wrap at the window edge; otherwise at that column.
word_wrap = false
wrap_column = 0
# Editor text font; "" = the built-in monospace list, 0 = 13. Use a monospace family.
font_family = ""
font_size = 0

# Thin guide lines at each indentation level (View > Toggle Indent Guides).
indent_guides = true
# Column marker: a vertical line after this column; 0 = off (View > Toggle Column Ruler).
ruler_column = 0
# Overview of the whole document at the right edge (View > Toggle Minimap).
minimap = false

[ui]
# Left sidebar (Open Files + Browser): shown or hidden (View > Toggle Sidebar, Ctrl+B).
sidebar.visible = true
# Where the open-file tab bar sits: "top" or "bottom" (just above the status bar).
tabs.position = "top"

[files]
# Encoding of a new document ("" = UTF-8); an import from another editor fills it in.
default_encoding = ""
# UTF-8 files of at least this many MB open in "large file mode": the text stays on disk and is read on demand,
# so memory does not depend on the file size. Highlighting, wrap, folding, outline and word completion are off.
# 0 = never (always load the whole file into memory).
lazy_threshold_mb = 16

[theme]
# "dark", "light" or "system" (follow the operating system).
mode = "dark"
# Preferred theme name for the resolved appearance; empty = the default one.
name = ""

[thresholds]
large_file_bytes = 52428800
highlight_max_lines = 200000
search_max_results = 100000

[startup]
# Document opened when Brouillon starts with no file and no session to restore:
# "welcome" | "docs" | "changelog" | "none".
document = "welcome"
# Open the release notes once after the program was updated.
whats_new = true

[render]
# "cpu" (default, no graphics driver needed) or "gpu".
backend = "cpu"

[perf]
# After this long without key/mouse input, hand free memory back to the OS once (allocator
# collect + HeapCompact). Live data is never touched; takes effect at the next launch.
idle_trim = true
idle_trim_seconds = 30
# Also empty the working set when trimming. Lowers the number Task Manager shows, but pages fault
# back in on next use (a tiny hitch); the private bytes do not change. Off by default.
idle_trim_working_set = false

[firstrun]
# Set once the first-run screens were answered or skipped.
done = false

[updates]
# One signed-manifest request per launch (a plain GET, no identifier); off = no request at all. ON by default;
# turn it off on first-run screen 3 or in Preferences > Updates. An offline launch is unaffected.
check = true
# Download a newer signed version and start it by itself when no document has unsaved changes.
auto_install = true
# At most one check per this many hours (0 = every launch).
check_interval_hours = 24
# Empty = any channel; a manifest naming another channel is ignored.
channel = ""

[keymap]
# Overlay the classic Windows-editor shortcuts on the defaults.
classic = false

[backup]
enabled = true
max_count = 10
max_age_days = 30

[mcp]
# Brouillon Link: let a local AI program (MCP client) read open documents and propose edits.
# Off = nothing listens and no file is written.
enabled = false
# "read" | "ask" (default: edits are proposals you accept or reject).
mode = "ask"
# Advanced, all off unless you set them:
#   allow_apply = true        let a client apply edits directly and save (ceiling S3; default S2 = propose only)
#   http = true               also listen on 127.0.0.1 (random port, bearer token in the instance file)
#   auto_off_minutes = 15     switch the server off after N minutes without a client (this session only)
"#;

/// Cascade layers, lowest to highest precedence (Default is built in).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Layer {
    Platform,
    User,
    FileType,
    Project,
}

#[derive(Debug, Clone)]
pub struct Settings {
    merged: Table,
    /// Dotted keys present in an override layer but absent from the defaults.
    pub unknown_keys: Vec<String>,
    /// Layers that failed to parse (ignored), as readable messages.
    pub errors: Vec<String>,
}

fn merge(base: &mut Table, over: Table) {
    for (k, v) in over {
        match (base.get_mut(&k), v) {
            (Some(Value::Table(b)), Value::Table(o)) => merge(b, o),
            (_, v) => {
                base.insert(k, v);
            }
        }
    }
}

fn flatten(prefix: &str, t: &Table, out: &mut Vec<String>) {
    for (k, v) in t {
        let key = if prefix.is_empty() { k.clone() } else { format!("{prefix}.{k}") };
        match v {
            Value::Table(sub) => flatten(&key, sub, out),
            _ => out.push(key),
        }
    }
}

impl Settings {
    /// Merge `layers` (any order; applied by `Layer` precedence) over the defaults.
    pub fn build(layers: &[(Layer, &str)]) -> Self {
        let mut merged: Table = DEFAULTS_TOML.parse().expect("DEFAULTS_TOML is valid");
        let mut known = Vec::new();
        flatten("", &merged, &mut known);
        let (mut unknown_keys, mut errors) = (Vec::new(), Vec::new());
        let mut sorted: Vec<_> = layers.to_vec();
        sorted.sort_by_key(|(l, _)| *l);
        for (layer, text) in sorted {
            match text.parse::<Table>() {
                Ok(t) => {
                    let mut keys = Vec::new();
                    flatten("", &t, &mut keys);
                    unknown_keys.extend(keys.into_iter().filter(|k| !known.contains(k)));
                    merge(&mut merged, t);
                }
                Err(e) => errors.push(format!("{layer:?} layer: {e}")),
            }
        }
        unknown_keys.sort();
        unknown_keys.dedup();
        Self { merged, unknown_keys, errors }
    }

    /// Effective value at a dotted path (`theme.mode`), from any layer or the defaults.
    pub fn get(&self, path: &str) -> Option<&Value> {
        let mut cur = &self.merged;
        let mut parts = path.split('.').peekable();
        while let Some(p) = parts.next() {
            let v = cur.get(p)?;
            if parts.peek().is_none() {
                return Some(v);
            }
            cur = v.as_table()?;
        }
        None
    }

    pub fn get_bool(&self, path: &str, default: bool) -> bool {
        self.get(path).and_then(Value::as_bool).unwrap_or(default)
    }
    pub fn get_i64(&self, path: &str, default: i64) -> i64 {
        self.get(path).and_then(Value::as_integer).unwrap_or(default)
    }
    pub fn get_str(&self, path: &str, default: &str) -> String {
        self.get(path).and_then(Value::as_str).unwrap_or(default).to_string()
    }

    /// `ui.sidebar.visible` (default shown).
    pub fn sidebar_visible(&self) -> bool {
        self.get_bool("ui.sidebar.visible", true)
    }
    /// `ui.tabs.position == "bottom"` (default top; anything else is top).
    pub fn tabs_at_bottom(&self) -> bool {
        self.get_str("ui.tabs.position", "top") == "bottom"
    }

    fn threshold(&self, key: &str) -> u64 {
        self.get_i64(&format!("thresholds.{key}"), 0).max(0) as u64
    }
    /// Files larger than this open in large-file mode.
    pub fn large_file_bytes(&self) -> u64 {
        self.threshold("large_file_bytes")
    }
    pub fn highlight_max_lines(&self) -> u64 {
        self.threshold("highlight_max_lines")
    }
    pub fn search_max_results(&self) -> u64 {
        self.threshold("search_max_results")
    }

    /// Effective settings as TOML text (for the side-by-side settings view).
    pub fn to_toml(&self) -> String {
        toml::to_string_pretty(&self.merged).unwrap_or_default()
    }
}

/// Set `key = "value"` inside `[section]` of a user settings file, keeping every
/// other line (comments included) as is. Creates the key or the section when absent.
pub fn set_string_key(text: &str, section: &str, key: &str, value: &str) -> String {
    set_raw_key(text, section, key, &format!("\"{}\"", value.replace('\\', "\\\\").replace('"', "\\\"")))
}

/// Same as [`set_string_key`] for any TOML value given in its text form
/// (`true`, `42`, `"text"`): the caller guarantees `raw` is valid TOML.
pub fn set_raw_key(text: &str, section: &str, key: &str, raw: &str) -> String {
    let eol = if text.contains("\r\n") { "\r\n" } else { "\n" };
    let line = format!("{key} = {raw}");
    let mut out: Vec<String> = Vec::new();
    let (mut in_section, mut done, mut section_seen) = (false, false, false);
    let mut insert_at: Option<usize> = None; // index in `out` after the section's last line
    for l in text.lines() {
        let t = l.trim();
        if t.starts_with('[') && t.ends_with(']') {
            if in_section && !done && insert_at.is_none() {
                insert_at = Some(out.len());
            }
            in_section = t[1..t.len() - 1].trim() == section;
            section_seen |= in_section;
        } else if in_section && !done {
            if let Some((k, _)) = t.split_once('=') {
                if k.trim() == key {
                    out.push(line.clone());
                    done = true;
                    continue;
                }
            }
        }
        out.push(l.to_string());
    }
    if !done {
        if section_seen {
            let at = insert_at.unwrap_or(out.len());
            // Keep trailing blank lines of the section after the new key.
            let mut at = at;
            while at > 0 && out[at - 1].trim().is_empty() {
                at -= 1;
            }
            out.insert(at, line);
        } else {
            if out.last().is_some_and(|l| !l.trim().is_empty()) {
                out.push(String::new());
            }
            out.push(format!("[{section}]"));
            out.push(line);
        }
    }
    out.join(eol) + eol
}

/// Remove `key` from `[section]` (the key line only: comments and other keys stay).
/// Returns the text unchanged when the key is absent.
pub fn remove_key(text: &str, section: &str, key: &str) -> String {
    let eol = if text.contains("\r\n") { "\r\n" } else { "\n" };
    let mut in_section = false;
    let mut removed = false;
    let kept: Vec<&str> = text
        .lines()
        .filter(|l| {
            let t = l.trim();
            if t.starts_with('[') && t.ends_with(']') {
                in_section = t[1..t.len() - 1].trim() == section;
            } else if in_section && !removed {
                if let Some((k, _)) = t.split_once('=') {
                    if k.trim() == key {
                        removed = true;
                        return false;
                    }
                }
            }
            true
        })
        .collect();
    if !removed {
        return text.to_string();
    }
    kept.join(eol) + eol
}

/// Keys written in a user settings file, as `(dotted key, TOML value)` pairs.
pub fn user_values(text: &str) -> Vec<(String, Value)> {
    fn walk(prefix: &str, t: &Table, out: &mut Vec<(String, Value)>) {
        for (k, v) in t {
            let key = if prefix.is_empty() { k.clone() } else { format!("{prefix}.{k}") };
            match v {
                Value::Table(sub) => walk(&key, sub, out),
                v => out.push((key, v.clone())),
            }
        }
    }
    let mut out = Vec::new();
    if let Ok(t) = text.parse::<Table>() {
        walk("", &t, &mut out);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn set_string_key_replaces_inserts_and_creates() {
        let src = "# mine\n[editor]\ntab_size = 2\n\n[theme]\n# pick\nmode = \"dark\"\nname = \"x\"\n\n[render]\nbackend = \"cpu\"\n";
        let r = set_string_key(src, "theme", "mode", "light");
        assert!(r.contains("mode = \"light\"") && !r.contains("\"dark\"") && r.contains("# pick") && r.contains("# mine"));
        // Missing key in an existing section: inserted inside it, not in the next one.
        let r = set_string_key("[theme]\nname = \"a\"\n\n[render]\nbackend = \"cpu\"\n", "theme", "mode", "system");
        let s = Settings::build(&[(Layer::User, r.as_str())]);
        assert_eq!(s.get_str("theme.mode", ""), "system");
        assert_eq!(s.get_str("theme.name", ""), "a");
        assert_eq!(s.get_str("render.backend", ""), "cpu");
        // Missing section: appended; empty file; CRLF kept; quotes escaped.
        assert_eq!(set_string_key("", "theme", "mode", "dark"), "[theme]\nmode = \"dark\"\n");
        let r = set_string_key("[editor]\r\ntab_size = 2\r\n", "theme", "name", "a\"b");
        assert!(r.contains("\r\n[theme]\r\nname = \"a\\\"b\"\r\n"), "{r:?}");
        assert_eq!(Settings::build(&[(Layer::User, r.as_str())]).get_str("theme.name", ""), "a\"b");
        // Same key in another section is left alone.
        let r = set_string_key("[render]\nmode = \"x\"\n[theme]\n", "theme", "mode", "light");
        assert!(r.contains("[render]\nmode = \"x\"") && r.contains("[theme]\nmode = \"light\""));
        assert!(Settings::build(&[(Layer::User, r.as_str())]).unknown_keys == vec!["render.mode"]);
    }

    #[test]
    fn raw_values_set_and_removed_keep_comments() {
        let src = "# top\n[updates]\n# why\ncheck = true\nauto_install = true\n\n[backup]\nmax_count = 10\n";
        let r = set_raw_key(src, "updates", "check", "false");
        assert!(r.contains("check = false") && r.contains("# why") && r.contains("auto_install = true") && r.contains("# top"));
        let s = Settings::build(&[(Layer::User, r.as_str())]);
        assert!(!s.get_bool("updates.check", true) && s.get_bool("updates.auto_install", false));
        assert!(s.unknown_keys.is_empty(), "{:?}", s.unknown_keys);
        let back = remove_key(&r, "updates", "check");
        assert!(!back.contains("check") && back.contains("# why") && back.contains("auto_install = true"));
        assert_eq!(remove_key(&back, "updates", "check"), back, "absent key: unchanged");
        // A same-named key in another section survives.
        assert_eq!(remove_key("[a]\nx = 1\n[b]\nx = 2\n", "b", "x"), "[a]\nx = 1\n[b]\n");
        let v = user_values("[backup]\nmax_count = 3\n[theme]\nmode = \"light\"");
        assert_eq!(v.len(), 2);
        assert_eq!(v[0].0, "backup.max_count");
        assert!(user_values("{{{").is_empty());
    }

    #[test]
    fn theme_keys_are_known_defaults() {
        let s = Settings::build(&[(Layer::User, "[theme]\nmode = \"light\"\nname = \"GitHub Light\"")]);
        assert!(s.unknown_keys.is_empty(), "{:?}", s.unknown_keys);
        assert_eq!(s.get_str("theme.mode", ""), "light");
        assert_eq!(Settings::build(&[]).get_str("theme.mode", ""), "dark");
    }

    #[test]
    fn precedence_default_platform_user_filetype_project() {
        let layers = [
            (Layer::Project, "[editor]\ntab_size = 2"),
            (Layer::Platform, "[editor]\ntab_size = 8\nword_wrap = true"),
            (Layer::FileType, "[editor]\ntab_size = 3"),
            (Layer::User, "[editor]\ntab_size = 6"),
        ];
        let s = Settings::build(&layers);
        assert_eq!(s.get_i64("editor.tab_size", 0), 2, "project wins");
        assert!(s.get_bool("editor.word_wrap", false), "platform value survives deep merge");
        let s = Settings::build(&layers[1..]);
        assert_eq!(s.get_i64("editor.tab_size", 0), 3, "file-type beats user beats platform");
        assert_eq!(Settings::build(&[]).get_i64("editor.tab_size", 0), 4, "defaults");
    }

    #[test]
    fn unknown_keys_and_bad_layers_reported() {
        let s = Settings::build(&[(Layer::User, "[editor]\ntab_sise = 2\n[nope]\nx = 1"), (Layer::Project, "{{{")]);
        assert_eq!(s.unknown_keys, vec!["editor.tab_sise", "nope.x"]);
        assert_eq!(s.errors.len(), 1);
        assert_eq!(s.get_i64("editor.tab_size", 0), 4);
    }

    #[test]
    fn thresholds_reach_consumers_through_typed_getters() {
        let s = Settings::build(&[(Layer::User, "[thresholds]\nlarge_file_bytes = 1000")]);
        assert_eq!(s.large_file_bytes(), 1000);
        assert_eq!(s.highlight_max_lines(), 200_000);
        assert_eq!(s.search_max_results(), 100_000);
        assert!(s.to_toml().contains("large_file_bytes = 1000"));
    }

    #[test]
    fn layout_defaults_are_sidebar_shown_tabs_top() {
        let s = Settings::build(&[]);
        assert!(s.sidebar_visible());
        assert!(!s.tabs_at_bottom());
        assert!(s.unknown_keys.is_empty());
    }

    #[test]
    fn layout_choices_round_trip_through_the_settings_file() {
        let mut text = String::new();
        text = set_raw_key(&text, "ui", "sidebar.visible", "false");
        text = set_raw_key(&text, "ui", "tabs.position", "\"bottom\"");
        let s = Settings::build(&[(Layer::User, text.as_str())]);
        assert!(!s.sidebar_visible());
        assert!(s.tabs_at_bottom());
        assert!(s.unknown_keys.is_empty(), "{:?}", s.unknown_keys);
        // Writing again replaces the line instead of adding one.
        let text = set_raw_key(&text, "ui", "sidebar.visible", "true");
        assert_eq!(text.matches("sidebar.visible").count(), 1);
        let s = Settings::build(&[(Layer::User, text.as_str())]);
        assert!(s.sidebar_visible() && s.tabs_at_bottom());
        // A damaged value falls back to the default.
        let s = Settings::build(&[(Layer::User, "[ui]
tabs.position = \"left\"
sidebar.visible = 3
")]);
        assert!(!s.tabs_at_bottom() && s.sidebar_visible());
    }
}
