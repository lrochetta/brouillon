//! Customizable keyboard shortcuts via TOML configuration.
//!
//! Loads user keybindings from `~/.brouillon/keymap.toml`, merges with
//! built-in defaults, and provides fast lookup by key combination.
//!
//! The keymap file format:
//! ```toml
//! [bindings]
//! "ctrl+o" = "file.open"
//! "ctrl+s" = "file.save"
//! "ctrl+shift+s" = "file.save_as"
//! "ctrl+z" = "edit.undo"
//! "ctrl+shift+z" = "edit.redo"
//! ```

use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// A key combination (modifiers + key).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct KeyCombo {
    pub ctrl: bool,
    pub shift: bool,
    pub alt: bool,
    /// The key name, lowercase (e.g. "o", "s", "f1", "enter", "tab")
    pub key: String,
}

impl KeyCombo {
    /// Create a new key combo.
    pub fn new(ctrl: bool, shift: bool, alt: bool, key: &str) -> Self {
        Self {
            ctrl,
            shift,
            alt,
            key: key.to_lowercase(),
        }
    }

    /// Parse a string like "ctrl+shift+s" into a KeyCombo.
    pub fn parse(s: &str) -> Option<Self> {
        let parts: Vec<&str> = s.split('+').map(|p| p.trim()).filter(|p| !p.is_empty()).collect();
        if parts.is_empty() {
            return None;
        }

        let mut ctrl = false;
        let mut shift = false;
        let mut alt = false;
        let mut key = None;

        for part in &parts {
            match part.to_lowercase().as_str() {
                "ctrl" | "control" => ctrl = true,
                "shift" => shift = true,
                "alt" => alt = true,
                "meta" | "cmd" | "super" => ctrl = true, // map meta to ctrl for cross-platform
                k => key = Some(k.to_string()),
            }
        }

        key.map(|k| Self {
            ctrl,
            shift,
            alt,
            key: k,
        })
    }

    /// Serialize to the canonical string form (e.g. "ctrl+shift+s").
    pub fn to_string_repr(&self) -> String {
        let mut parts = Vec::new();
        if self.ctrl {
            parts.push("ctrl");
        }
        if self.shift {
            parts.push("shift");
        }
        if self.alt {
            parts.push("alt");
        }
        parts.push(&self.key);
        parts.join("+")
    }
}

/// Action identifier — a dot-separated string like "file.open" or "edit.undo".
pub type ActionId = String;

/// A keymap binding: maps key combinations to action identifiers.
#[derive(Debug, Clone)]
pub struct Keymap {
    bindings: HashMap<KeyCombo, ActionId>,
}

impl Keymap {
    /// Create an empty keymap.
    pub fn empty() -> Self {
        Self {
            bindings: HashMap::new(),
        }
    }

    /// Create the built-in default keymap.
    pub fn default_keymap() -> Self {
        let mut bindings = HashMap::new();

        let defaults: &[(&str, &str)] = &[
            // File
            ("ctrl+o", "file.open"),
            ("ctrl+s", "file.save"),
            ("ctrl+shift+s", "file.save_as"),
            ("ctrl+n", "file.new"),
            ("ctrl+w", "file.close_tab"),
            // Edit
            ("ctrl+z", "edit.undo"),
            ("ctrl+shift+z", "edit.redo"),
            ("ctrl+x", "edit.cut"),
            ("ctrl+c", "edit.copy"),
            ("ctrl+v", "edit.paste"),
            ("ctrl+a", "edit.select_all"),
            // Search
            ("ctrl+f", "search.find"),
            ("ctrl+h", "search.replace"),
            ("ctrl+g", "search.go_to_line"),
            // View
            ("ctrl+t", "view.cycle_theme"),
            ("ctrl+shift+m", "view.toggle_preset"),
            ("ctrl+k", "view.command_palette"),
            ("ctrl+shift+v", "view.toggle_preview"),
            // Navigation
            ("ctrl+tab", "nav.next_tab"),
            ("ctrl+shift+tab", "nav.prev_tab"),
        ];

        for (combo_str, action) in defaults {
            if let Some(combo) = KeyCombo::parse(combo_str) {
                bindings.insert(combo, action.to_string());
            }
        }

        Self { bindings }
    }

    /// Look up the action bound to a key combination.
    pub fn lookup(&self, combo: &KeyCombo) -> Option<&ActionId> {
        self.bindings.get(combo)
    }

    /// Insert or override a binding.
    pub fn bind(&mut self, combo: KeyCombo, action: ActionId) {
        self.bindings.insert(combo, action);
    }

    /// Remove a binding.
    pub fn unbind(&mut self, combo: &KeyCombo) -> Option<ActionId> {
        self.bindings.remove(combo)
    }

    /// Get the number of bindings.
    pub fn len(&self) -> usize {
        self.bindings.len()
    }

    /// Check if the keymap is empty.
    pub fn is_empty(&self) -> bool {
        self.bindings.is_empty()
    }

    /// Merge a user keymap on top of a base keymap.
    /// User bindings override base bindings for the same key combo.
    pub fn merge(base: &Keymap, user: &Keymap) -> Keymap {
        let mut merged = base.clone();
        for (combo, action) in &user.bindings {
            merged.bindings.insert(combo.clone(), action.clone());
        }
        merged
    }

    /// Get all bindings as an iterator.
    pub fn iter(&self) -> impl Iterator<Item = (&KeyCombo, &ActionId)> {
        self.bindings.iter()
    }

    /// Find the key combo bound to a specific action (reverse lookup).
    /// Returns the first match found (there may be multiple).
    pub fn find_combo_for_action(&self, action: &str) -> Option<&KeyCombo> {
        self.bindings.iter()
            .find(|(_, a)| a.as_str() == action)
            .map(|(k, _)| k)
    }
}

/// Load a user keymap from a TOML file.
///
/// The TOML format:
/// ```toml
/// [bindings]
/// "ctrl+o" = "file.open"
/// "ctrl+s" = "file.save"
/// ```
///
/// Returns an empty keymap if the file doesn't exist or is invalid.
pub fn load_keymap(path: &Path) -> Keymap {
    let content = match std::fs::read_to_string(path) {
        Ok(c) => c,
        Err(_) => return Keymap::empty(),
    };
    parse_keymap_toml(&content)
}

/// Parse a keymap from TOML content string.
pub fn parse_keymap_toml(content: &str) -> Keymap {
    let table: toml::Table = match content.parse() {
        Ok(t) => t,
        Err(_) => return Keymap::empty(),
    };

    let bindings_table = match table.get("bindings").and_then(|v| v.as_table()) {
        Some(t) => t,
        None => return Keymap::empty(),
    };

    let mut keymap = Keymap::empty();
    for (key_str, value) in bindings_table {
        if let (Some(combo), Some(action)) = (KeyCombo::parse(key_str), value.as_str()) {
            keymap.bind(combo, action.to_string());
        }
    }
    keymap
}

/// Serialize a keymap to TOML format string.
pub fn serialize_keymap(keymap: &Keymap) -> String {
    let mut lines = vec!["[bindings]".to_string()];
    let mut sorted: Vec<_> = keymap.iter()
        .map(|(k, v)| (k.to_string_repr(), v.clone()))
        .collect();
    sorted.sort_by(|a, b| a.0.cmp(&b.0));

    for (combo_str, action) in sorted {
        lines.push(format!("\"{}\" = \"{}\"", combo_str, action));
    }
    lines.join("\n")
}

/// Get the default keymap file path: `~/.brouillon/keymap.toml`.
pub fn keymap_path() -> Option<PathBuf> {
    dirs::home_dir().map(|h| h.join(".brouillon").join("keymap.toml"))
}

/// Load the effective keymap: defaults merged with user overrides.
pub fn load_effective_keymap() -> Keymap {
    let base = Keymap::default_keymap();
    match keymap_path() {
        Some(p) if p.exists() => {
            let user = load_keymap(&p);
            Keymap::merge(&base, &user)
        }
        _ => base,
    }
}

// ---------------------------------------------------------------------------
// Layered, context-aware bindings with chords (BK-068)
// ---------------------------------------------------------------------------

/// A chord: one or more key combos pressed in sequence (`ctrl+k ctrl+b`).
pub type ChordSeq = Vec<KeyCombo>;

/// Parse `"ctrl+k ctrl+b"` into a chord sequence. `None` if any step is invalid.
pub fn parse_chord(s: &str) -> Option<ChordSeq> {
    let steps: Option<Vec<KeyCombo>> = s.split_whitespace().map(KeyCombo::parse).collect();
    steps.filter(|v| !v.is_empty())
}

/// Canonical string form of a chord sequence.
pub fn chord_to_string(chord: &[KeyCombo]) -> String {
    chord.iter().map(|c| c.to_string_repr()).collect::<Vec<_>>().join(" ")
}

/// Human form for menus/palette: `ctrl+shift+s` -> `Ctrl+Shift+S`.
pub fn chord_display(chord: &str) -> String {
    chord
        .split_whitespace()
        .map(|step| {
            step.split('+')
                .map(|p| {
                    let mut c = p.chars();
                    c.next().map(|f| f.to_uppercase().collect::<String>() + c.as_str()).unwrap_or_default()
                })
                .collect::<Vec<_>>()
                .join("+")
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// Value of one context key.
#[derive(Debug, Clone, PartialEq)]
pub enum CtxValue {
    Bool(bool),
    Int(i64),
    Str(String),
}

/// Runtime context a `when` expression is evaluated against
/// (selection_empty, num_selections, language, focus, preset, ...).
#[derive(Debug, Clone, Default)]
pub struct Context {
    values: HashMap<String, CtxValue>,
}

impl Context {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn set_bool(&mut self, k: &str, v: bool) -> &mut Self {
        self.values.insert(k.into(), CtxValue::Bool(v));
        self
    }
    pub fn set_int(&mut self, k: &str, v: i64) -> &mut Self {
        self.values.insert(k.into(), CtxValue::Int(v));
        self
    }
    pub fn set_str(&mut self, k: &str, v: &str) -> &mut Self {
        self.values.insert(k.into(), CtxValue::Str(v.into()));
        self
    }
    fn truthy(&self, k: &str) -> bool {
        match self.values.get(k) {
            Some(CtxValue::Bool(b)) => *b,
            Some(CtxValue::Int(i)) => *i != 0,
            Some(CtxValue::Str(s)) => !s.is_empty(),
            None => false,
        }
    }
    fn text(&self, k: &str) -> Option<String> {
        self.values.get(k).map(|v| match v {
            CtxValue::Bool(b) => b.to_string(),
            CtxValue::Int(i) => i.to_string(),
            CtxValue::Str(s) => s.clone(),
        })
    }
}

/// Evaluate a `when` expression: terms joined by `&&`; each term is `key`,
/// `!key`, `key == v`, `key != v`, `key > n`, `key < n`, `key >= n`, `key <= n`.
/// Unknown keys are false. `None`/empty expression is always true.
pub fn eval_when(expr: Option<&str>, ctx: &Context) -> bool {
    let Some(expr) = expr else { return true };
    expr.split("&&").map(str::trim).filter(|t| !t.is_empty()).all(|term| eval_term(term, ctx))
}

fn eval_term(term: &str, ctx: &Context) -> bool {
    for op in ["==", "!=", ">=", "<=", ">", "<"] {
        if let Some((k, v)) = term.split_once(op) {
            let (k, v) = (k.trim(), v.trim().trim_matches('"'));
            let cur = ctx.text(k);
            return match op {
                "==" => cur.as_deref() == Some(v),
                "!=" => cur.as_deref() != Some(v),
                _ => match (cur.and_then(|c| c.parse::<i64>().ok()), v.parse::<i64>().ok()) {
                    (Some(a), Some(b)) => match op {
                        ">=" => a >= b,
                        "<=" => a <= b,
                        ">" => a > b,
                        _ => a < b,
                    },
                    _ => false,
                },
            };
        }
    }
    match term.strip_prefix('!') {
        Some(k) => !ctx.truthy(k.trim()),
        None => ctx.truthy(term),
    }
}

/// One chord -> command binding, optionally restricted by a `when` context.
/// An empty `command` means "unbound" (shadows lower layers).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Binding {
    pub chord: ChordSeq,
    pub command: String,
    pub when: Option<String>,
}

impl Binding {
    pub fn new(chord: &str, command: &str, when: Option<&str>) -> Option<Self> {
        Some(Self {
            chord: parse_chord(chord)?,
            command: command.to_string(),
            when: when.map(str::to_string),
        })
    }
}

/// Two bindings of the same chord that are active in overlapping contexts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Conflict {
    pub chord: String,
    pub commands: Vec<String>,
}

fn overlaps(a: &Binding, b: &Binding) -> bool {
    a.chord == b.chord && (a.when == b.when || a.when.is_none() || b.when.is_none())
}

/// Report chords bound to different commands in overlapping contexts within ONE layer.
pub fn find_conflicts(layer: &[Binding]) -> Vec<Conflict> {
    let mut out: Vec<Conflict> = Vec::new();
    for (i, a) in layer.iter().enumerate() {
        for b in &layer[i + 1..] {
            if overlaps(a, b) && a.command != b.command {
                let chord = chord_to_string(&a.chord);
                match out.iter_mut().find(|c| c.chord == chord) {
                    Some(c) => {
                        for cmd in [&a.command, &b.command] {
                            if !c.commands.contains(cmd) {
                                c.commands.push(cmd.clone());
                            }
                        }
                    }
                    None => out.push(Conflict { chord, commands: vec![a.command.clone(), b.command.clone()] }),
                }
            }
        }
    }
    out
}

/// Concatenate layers in precedence order (later layers win on lookup).
pub fn compose(layers: &[&[Binding]]) -> Vec<Binding> {
    layers.iter().flat_map(|l| l.iter().cloned()).collect()
}

/// Command bound to `chord` in `ctx`: the last matching binding wins.
pub fn lookup_chord(bindings: &[Binding], chord: &[KeyCombo], ctx: &Context) -> Option<String> {
    bindings
        .iter()
        .rev()
        .find(|b| b.chord == chord && eval_when(b.when.as_deref(), ctx))
        .map(|b| b.command.clone())
        .filter(|c| !c.is_empty())
}

/// Chords bound to `command` (effective, i.e. not shadowed by a later binding
/// of the same chord and `when`).
pub fn chords_for(bindings: &[Binding], command: &str) -> Vec<String> {
    let mut out = Vec::new();
    for (i, b) in bindings.iter().enumerate() {
        let shadowed = bindings[i + 1..].iter().any(|n| n.chord == b.chord && n.when == b.when);
        if b.command == command && !shadowed {
            out.push(chord_to_string(&b.chord));
        }
    }
    out
}

/// Parse `[[bindings]] chord/command/when` TOML. Invalid entries are skipped
/// and described in the returned warnings.
pub fn parse_bindings_toml(content: &str) -> (Vec<Binding>, Vec<String>) {
    let mut warnings = Vec::new();
    let table: toml::Table = match content.parse() {
        Ok(t) => t,
        Err(e) => return (Vec::new(), vec![format!("invalid TOML: {e}")]),
    };
    let mut out = Vec::new();
    for (i, entry) in table.get("bindings").and_then(|v| v.as_array()).into_iter().flatten().enumerate() {
        let get = |k: &str| entry.get(k).and_then(|v| v.as_str());
        match (get("chord").and_then(parse_chord), get("command")) {
            (Some(chord), Some(command)) => out.push(Binding {
                chord,
                command: command.to_string(),
                when: get("when").map(str::to_string),
            }),
            _ => warnings.push(format!("bindings[{i}]: needs a valid `chord` and a `command`")),
        }
    }
    (out, warnings)
}

/// Serialize bindings to the `[[bindings]]` TOML form (round-trips with `parse_bindings_toml`).
pub fn serialize_bindings(bindings: &[Binding]) -> String {
    let mut s = String::new();
    for b in bindings {
        s.push_str("[[bindings]]\n");
        s.push_str(&format!("chord = {:?}\ncommand = {:?}\n", chord_to_string(&b.chord), b.command));
        if let Some(w) = &b.when {
            s.push_str(&format!("when = {:?}\n", w));
        }
        s.push('\n');
    }
    s
}

/// Default bindings declared by command specs (`default_chord` / `when`).
pub fn bindings_from_specs(specs: &[crate::contract::CommandSpec]) -> Vec<Binding> {
    specs
        .iter()
        .filter_map(|s| Binding::new(s.default_chord.as_deref()?, &s.id, s.when.as_deref()))
        .collect()
}

/// Outcome of feeding one key press to the chord resolver.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Resolution {
    Command(String),
    /// A prefix of a longer chord was typed; waiting for the next key.
    Pending,
    NoMatch,
}

/// Stateful multi-step chord matcher with an injectable clock (milliseconds).
#[derive(Debug, Clone)]
pub struct ChordResolver {
    pending: ChordSeq,
    last_ms: u64,
    pub timeout_ms: u64,
}

impl Default for ChordResolver {
    fn default() -> Self {
        Self { pending: Vec::new(), last_ms: 0, timeout_ms: 1500 }
    }
}

impl ChordResolver {
    pub fn is_pending(&self) -> bool {
        !self.pending.is_empty()
    }

    pub fn reset(&mut self) {
        self.pending.clear();
    }

    /// Feed a key press at time `now_ms`. A longer chord sharing the typed
    /// prefix wins over a shorter exact match (the prefix stays pending).
    pub fn feed(&mut self, combo: KeyCombo, now_ms: u64, bindings: &[Binding], ctx: &Context) -> Resolution {
        if self.is_pending() && now_ms.saturating_sub(self.last_ms) > self.timeout_ms {
            self.pending.clear();
        }
        self.last_ms = now_ms;
        self.pending.push(combo.clone());
        let had_prefix = self.pending.len() > 1;
        match self.step(bindings, ctx) {
            Resolution::NoMatch if had_prefix => {
                self.pending = vec![combo];
                self.step(bindings, ctx)
            }
            r => r,
        }
    }

    fn step(&mut self, bindings: &[Binding], ctx: &Context) -> Resolution {
        let live = |b: &&Binding| eval_when(b.when.as_deref(), ctx);
        let n = self.pending.len();
        let longer = bindings.iter().filter(live).any(|b| b.chord.len() > n && b.chord[..n] == self.pending[..]);
        if longer {
            return Resolution::Pending;
        }
        let hit = lookup_chord(bindings, &self.pending, ctx);
        self.pending.clear();
        hit.map_or(Resolution::NoMatch, Resolution::Command)
    }
}

/// The Classic overlay (D5): familiar accelerator set, selectable in either
/// mode on top of Moldu/Geek without changing their defaults. Ids follow the
/// registry's snake_case scheme; ids no lot registers are dropped by
/// `retain_known`.
pub fn classic_bindings() -> Vec<Binding> {
    const TABLE: &[(&str, &str)] = &[
        ("ctrl+n", "file_new"), ("ctrl+o", "file_open"), ("ctrl+w", "file_close"),
        ("ctrl+s", "file_save"), ("ctrl+p", "file_print"), ("ctrl+shift+o", "project_load"),
        ("ctrl+tab", "tab_next"), ("ctrl+shift+tab", "tab_previous"), ("f12", "tab_last_visited"),
        ("ctrl+z", "edit_undo"), ("alt+backspace", "edit_undo"), ("ctrl+y", "edit_redo"),
        ("ctrl+x", "edit_cut"), ("shift+delete", "edit_cut"), ("ctrl+c", "edit_copy"),
        ("ctrl+insert", "edit_copy"), ("ctrl+v", "edit_paste"), ("shift+insert", "edit_paste"),
        ("ctrl+shift+v", "edit_paste_column"), ("ctrl+a", "select_all"), ("ctrl+r", "select_line"),
        ("ctrl+shift+w", "select_word"), ("ctrl+alt+w", "select_word_all"),
        ("ctrl+delete", "delete_word_end"), ("ctrl+shift+delete", "delete_line_end"),
        ("ctrl+backspace", "delete_word_back"), ("ctrl+j", "line_duplicate"),
        ("ctrl+=", "selection_duplicate"), ("ctrl+t", "text_transpose"),
        ("ctrl+i", "indent_increase"), ("ctrl+shift+i", "indent_decrease"),
        ("ctrl+shift+j", "lines_join"), ("ctrl+shift+k", "lines_split"),
        ("ctrl+u", "case_upper"), ("ctrl+l", "case_lower"), ("ctrl+k", "case_invert"),
        ("ctrl+shift+u", "case_capitalize"), ("ctrl+d", "insert_date_short"),
        ("ctrl+alt+d", "insert_date_long"), ("ctrl+m", "insert_time_short"),
        ("ctrl+shift+m", "insert_time_long"), ("ctrl+/", "comment_toggle"),
        ("ctrl+f", "find_open"), ("alt+f3", "find_open"), ("ctrl+h", "replace_open"),
        ("ctrl+f3", "find_next_word"), ("ctrl+shift+f3", "find_previous_word"),
        ("ctrl+shift+f", "find_in_files"), ("ctrl+g", "goto_line"),
        ("ctrl+shift+g", "goto_column"), ("ctrl+]", "bracket_match"),
        ("ctrl+shift+]", "bracket_select"), ("ctrl+f11", "goto_function"),
        ("ctrl+shift+l", "view_toggle_line_numbers"), ("ctrl+q", "macro_record_toggle"),
        ("ctrl+1", "tool_run_1"), ("ctrl+2", "tool_run_2"), ("ctrl+3", "tool_run_3"),
        ("ctrl+4", "tool_run_4"), ("ctrl+5", "tool_run_5"), ("ctrl+6", "tool_run_6"),
        ("ctrl+7", "tool_run_7"), ("ctrl+8", "tool_run_8"), ("ctrl+9", "tool_run_9"),
        ("ctrl+0", "tool_run_10"), ("ctrl+shift+f9", "markers_clear"),
        ("ctrl+shift+f11", "markers_list"),
    ];
    let mut out: Vec<Binding> = TABLE.iter().filter_map(|(c, id)| Binding::new(c, id, None)).collect();
    for n in 1..=10u32 {
        let key = n % 10;
        out.extend(Binding::new(&format!("ctrl+alt+{key}"), &format!("cliptext_insert_{n}"), None));
    }
    out
}

/// Drop bindings whose command is not registered (keeps overlays honest).
pub fn retain_known(bindings: Vec<Binding>, known: impl Fn(&str) -> bool) -> Vec<Binding> {
    bindings.into_iter().filter(|b| b.command.is_empty() || known(&b.command)).collect()
}

#[cfg(test)]
mod layered_tests {
    use super::*;

    fn kc(s: &str) -> KeyCombo {
        KeyCombo::parse(s).unwrap()
    }

    #[test]
    fn chord_parse_and_roundtrip() {
        let c = parse_chord("ctrl+k ctrl+b").unwrap();
        assert_eq!(c.len(), 2);
        assert_eq!(chord_to_string(&c), "ctrl+k ctrl+b");
        assert!(parse_chord("").is_none());
    }

    #[test]
    fn when_expressions() {
        let mut ctx = Context::new();
        ctx.set_bool("selection_empty", true).set_int("num_selections", 3).set_str("language", "rust");
        assert!(eval_when(None, &ctx));
        assert!(eval_when(Some("selection_empty"), &ctx));
        assert!(!eval_when(Some("!selection_empty"), &ctx));
        assert!(eval_when(Some("num_selections > 1 && language == rust"), &ctx));
        assert!(!eval_when(Some("language != rust"), &ctx));
        assert!(!eval_when(Some("unknown_key"), &ctx));
    }

    #[test]
    fn duplicate_chord_same_context_is_reported() {
        let layer = vec![
            Binding::new("ctrl+k ctrl+b", "a", None).unwrap(),
            Binding::new("ctrl+k ctrl+b", "b", None).unwrap(),
            Binding::new("ctrl+x", "c", Some("selection_empty")).unwrap(),
            Binding::new("ctrl+x", "d", Some("!selection_empty")).unwrap(),
        ];
        let c = find_conflicts(&layer);
        assert_eq!(c.len(), 1);
        assert_eq!(c[0].chord, "ctrl+k ctrl+b");
        assert_eq!(c[0].commands, vec!["a", "b"]);
    }

    #[test]
    fn chord_sequence_and_timeout_reset() {
        let bs = vec![Binding::new("ctrl+k ctrl+b", "toggle_sidebar", None).unwrap()];
        let ctx = Context::new();
        let mut r = ChordResolver::default();
        assert_eq!(r.feed(kc("ctrl+k"), 0, &bs, &ctx), Resolution::Pending);
        assert_eq!(r.feed(kc("ctrl+b"), 100, &bs, &ctx), Resolution::Command("toggle_sidebar".into()));
        assert!(!r.is_pending());
        // timeout: second key arrives too late, so it is judged alone
        assert_eq!(r.feed(kc("ctrl+k"), 1000, &bs, &ctx), Resolution::Pending);
        assert_eq!(r.feed(kc("ctrl+b"), 1000 + 5000, &bs, &ctx), Resolution::NoMatch);
        assert!(!r.is_pending());
    }

    #[test]
    fn context_selects_binding() {
        let bs = vec![
            Binding::new("ctrl+x", "cut_line", Some("selection_empty")).unwrap(),
            Binding::new("ctrl+x", "cut", Some("!selection_empty")).unwrap(),
        ];
        let mut ctx = Context::new();
        ctx.set_bool("selection_empty", true);
        assert_eq!(lookup_chord(&bs, &[kc("ctrl+x")], &ctx).as_deref(), Some("cut_line"));
        ctx.set_bool("selection_empty", false);
        assert_eq!(lookup_chord(&bs, &[kc("ctrl+x")], &ctx).as_deref(), Some("cut"));
    }

    #[test]
    fn classic_overlay_changes_resolution_and_base_is_untouched() {
        let base = vec![Binding::new("ctrl+k", "palette_open", None).unwrap()];
        let classic = classic_bindings();
        let ctx = Context::new();
        let k = [kc("ctrl+k")];
        assert_eq!(lookup_chord(&base, &k, &ctx).as_deref(), Some("palette_open"));
        let with = compose(&[&base, &classic]);
        assert_eq!(lookup_chord(&with, &k, &ctx).as_deref(), Some("case_invert"));
        assert_eq!(lookup_chord(&base, &k, &ctx).as_deref(), Some("palette_open"));
        // user layer beats Classic; empty command unbinds
        let user = vec![Binding::new("ctrl+k", "", None).unwrap()];
        assert_eq!(lookup_chord(&compose(&[&base, &classic, &user]), &k, &ctx), None);
    }

    #[test]
    fn classic_has_no_internal_conflicts() {
        assert!(find_conflicts(&classic_bindings()).is_empty());
    }

    #[test]
    fn bindings_toml_roundtrip_and_warnings() {
        let src = "[[bindings]]\nchord = \"ctrl+k ctrl+b\"\ncommand = \"x\"\nwhen = \"num_selections > 1\"\n\n[[bindings]]\ncommand = \"broken\"\n";
        let (b, w) = parse_bindings_toml(src);
        assert_eq!(b.len(), 1);
        assert_eq!(w.len(), 1);
        let (again, w2) = parse_bindings_toml(&serialize_bindings(&b));
        assert_eq!(again, b);
        assert!(w2.is_empty());
    }

    #[test]
    fn specs_yield_default_bindings() {
        let spec = crate::contract::CommandSpec {
            id: "file_save".into(),
            default_chord: Some("ctrl+s".into()),
            ..Default::default()
        };
        let b = bindings_from_specs(&[spec]);
        assert_eq!(chords_for(&b, "file_save"), vec!["ctrl+s"]);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_key_combo_simple() {
        let combo = KeyCombo::parse("ctrl+s").unwrap();
        assert!(combo.ctrl);
        assert!(!combo.shift);
        assert!(!combo.alt);
        assert_eq!(combo.key, "s");
    }

    #[test]
    fn parse_key_combo_multi_modifier() {
        let combo = KeyCombo::parse("ctrl+shift+s").unwrap();
        assert!(combo.ctrl);
        assert!(combo.shift);
        assert!(!combo.alt);
        assert_eq!(combo.key, "s");
    }

    #[test]
    fn parse_key_combo_alt() {
        let combo = KeyCombo::parse("alt+f4").unwrap();
        assert!(!combo.ctrl);
        assert!(!combo.shift);
        assert!(combo.alt);
        assert_eq!(combo.key, "f4");
    }

    #[test]
    fn parse_key_combo_empty() {
        assert!(KeyCombo::parse("").is_none());
    }

    #[test]
    fn parse_key_combo_meta_maps_to_ctrl() {
        let combo = KeyCombo::parse("meta+s").unwrap();
        assert!(combo.ctrl);
        assert_eq!(combo.key, "s");
    }

    #[test]
    fn key_combo_roundtrip() {
        let combo = KeyCombo::new(true, true, false, "s");
        let repr = combo.to_string_repr();
        assert_eq!(repr, "ctrl+shift+s");
        let parsed = KeyCombo::parse(&repr).unwrap();
        assert_eq!(parsed, combo);
    }

    #[test]
    fn default_keymap_has_bindings() {
        let km = Keymap::default_keymap();
        assert!(km.len() >= 15, "default keymap should have at least 15 bindings, got {}", km.len());
    }

    #[test]
    fn default_keymap_lookup() {
        let km = Keymap::default_keymap();
        let combo = KeyCombo::parse("ctrl+o").unwrap();
        assert_eq!(km.lookup(&combo), Some(&"file.open".to_string()));
        let combo_v = KeyCombo::parse("ctrl+shift+v").unwrap();
        assert_eq!(km.lookup(&combo_v), Some(&"view.toggle_preview".to_string()));
    }

    #[test]
    fn lookup_missing_returns_none() {
        let km = Keymap::default_keymap();
        let combo = KeyCombo::parse("ctrl+shift+alt+q").unwrap();
        assert_eq!(km.lookup(&combo), None);
    }

    #[test]
    fn merge_user_overrides_base() {
        let base = Keymap::default_keymap();
        let mut user = Keymap::empty();
        user.bind(KeyCombo::parse("ctrl+s").unwrap(), "custom.save".to_string());

        let merged = Keymap::merge(&base, &user);
        let combo = KeyCombo::parse("ctrl+s").unwrap();
        assert_eq!(merged.lookup(&combo), Some(&"custom.save".to_string()));

        // Base binding still present for non-overridden keys
        let open_combo = KeyCombo::parse("ctrl+o").unwrap();
        assert_eq!(merged.lookup(&open_combo), Some(&"file.open".to_string()));
    }

    #[test]
    fn parse_toml_basic() {
        let toml = r#"
[bindings]
"ctrl+p" = "custom.print"
"alt+x" = "custom.execute"
"#;
        let km = parse_keymap_toml(toml);
        assert_eq!(km.len(), 2);
        assert_eq!(
            km.lookup(&KeyCombo::parse("ctrl+p").unwrap()),
            Some(&"custom.print".to_string())
        );
    }

    #[test]
    fn parse_toml_empty() {
        let km = parse_keymap_toml("");
        assert!(km.is_empty());
    }

    #[test]
    fn parse_toml_invalid() {
        let km = parse_keymap_toml("this is not valid toml {{{");
        assert!(km.is_empty());
    }

    #[test]
    fn serialize_roundtrip() {
        let mut km = Keymap::empty();
        km.bind(KeyCombo::parse("ctrl+s").unwrap(), "file.save".to_string());
        km.bind(KeyCombo::parse("ctrl+o").unwrap(), "file.open".to_string());

        let serialized = serialize_keymap(&km);
        let parsed = parse_keymap_toml(&serialized);
        assert_eq!(parsed.len(), 2);
        assert_eq!(
            parsed.lookup(&KeyCombo::parse("ctrl+s").unwrap()),
            Some(&"file.save".to_string())
        );
    }

    #[test]
    fn bind_and_unbind() {
        let mut km = Keymap::empty();
        let combo = KeyCombo::parse("ctrl+q").unwrap();
        km.bind(combo.clone(), "app.quit".to_string());
        assert_eq!(km.lookup(&combo), Some(&"app.quit".to_string()));

        km.unbind(&combo);
        assert_eq!(km.lookup(&combo), None);
    }

    #[test]
    fn find_combo_for_action() {
        let km = Keymap::default_keymap();
        let combo = km.find_combo_for_action("file.open");
        assert!(combo.is_some());
        assert_eq!(combo.unwrap().key, "o");
        assert!(combo.unwrap().ctrl);
    }

    #[test]
    fn find_combo_for_unknown_action() {
        let km = Keymap::default_keymap();
        assert!(km.find_combo_for_action("nonexistent.action").is_none());
    }

    #[test]
    fn unbind_nonexistent_does_not_panic() {
        let mut km = Keymap::empty();
        let combo = KeyCombo::parse("ctrl+alt+shift+k").unwrap();
        km.unbind(&combo);
        assert!(km.lookup(&combo).is_none());
    }
}
