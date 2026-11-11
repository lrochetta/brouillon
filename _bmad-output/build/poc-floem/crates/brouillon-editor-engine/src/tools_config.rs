//! User-tool definitions (BK-062) and the Lot 9 command entry points.
//!
//! ```toml
//! [[tool]]
//! name = "Build"
//! command = "cargo"
//! args = ["build", "--manifest-path", "${project}/Cargo.toml"]
//! group = 1          # 1..=20
//! slot = 1           # 1..=10 -> Ctrl+1 .. Ctrl+0
//! save_before = "all"   # none | current | all
//! output = "panel"      # panel | new_document | replace_selection | none
//! ```

use brouillon_core::contract::{CmdCtx, CmdOutcome, CommandSpec, PanelId};
use serde::Deserialize;
use std::collections::BTreeMap;

pub const MAX_GROUPS: u8 = 20;
pub const MAX_SLOTS: u8 = 10;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SavePolicy {
    #[default]
    None,
    Current,
    All,
}

/// Where captured stdout goes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OutputSink {
    #[default]
    Panel,
    NewDocument,
    /// Text filter: selection goes to stdin, stdout replaces it.
    ReplaceSelection,
    /// Fire and forget.
    None,
}

/// Opt-in automatic execution.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AutoRun {
    #[default]
    Never,
    OnOpen,
    OnSave,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct ToolDef {
    pub name: String,
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
    /// Initial directory template (variables allowed). Empty = file's directory.
    #[serde(default)]
    pub cwd: String,
    /// Run through the platform shell (variables are quoted) instead of directly.
    #[serde(default)]
    pub shell: bool,
    #[serde(default = "one")]
    pub group: u8,
    /// Ctrl+1..Ctrl+0 slot (1..=10) inside the group.
    #[serde(default)]
    pub slot: Option<u8>,
    /// Extensions (no dot) this tool is bound to; empty = all.
    #[serde(default)]
    pub file_types: Vec<String>,
    #[serde(default)]
    pub save_before: SavePolicy,
    #[serde(default)]
    pub output: OutputSink,
    #[serde(default)]
    pub env: BTreeMap<String, String>,
    #[serde(default)]
    pub auto_run: AutoRun,
}

fn one() -> u8 {
    1
}

const KNOWN_KEYS: &[&str] = &[
    "name", "command", "args", "cwd", "shell", "group", "slot", "file_types", "save_before", "output", "env", "auto_run",
];

/// Result of parsing a tools file: valid tools plus everything worth telling the user.
#[derive(Debug, Clone, Default)]
pub struct ToolsConfig {
    pub tools: Vec<ToolDef>,
    /// Unknown keys, invalid entries, slot conflicts.
    pub warnings: Vec<String>,
}

impl ToolsConfig {
    /// Tools of `group` that apply to `ext` (no dot), ordered by slot.
    pub fn for_group(&self, group: u8, ext: Option<&str>) -> Vec<&ToolDef> {
        let mut v: Vec<&ToolDef> = self
            .tools
            .iter()
            .filter(|t| t.group == group)
            .filter(|t| t.file_types.is_empty() || ext.is_some_and(|e| t.file_types.iter().any(|f| f.eq_ignore_ascii_case(e))))
            .collect();
        v.sort_by_key(|t| t.slot.unwrap_or(u8::MAX));
        v
    }

    /// The tool bound to Ctrl+`slot` in `group`.
    pub fn by_slot(&self, group: u8, slot: u8, ext: Option<&str>) -> Option<&ToolDef> {
        self.for_group(group, ext).into_iter().find(|t| t.slot == Some(slot))
    }
}

/// Parse and validate. Never fails: bad entries are skipped with a warning.
pub fn parse(content: &str) -> ToolsConfig {
    let mut cfg = ToolsConfig::default();
    let root: toml::Table = match content.parse() {
        Ok(t) => t,
        Err(e) => {
            cfg.warnings.push(format!("tools file is not valid TOML: {e}"));
            return cfg;
        }
    };
    for key in root.keys().filter(|k| *k != "tool") {
        cfg.warnings.push(format!("unknown top-level key '{key}'"));
    }
    let Some(entries) = root.get("tool").and_then(|v| v.as_array()) else {
        return cfg;
    };
    for (i, entry) in entries.iter().enumerate() {
        let label = entry.get("name").and_then(|n| n.as_str()).map(str::to_string).unwrap_or_else(|| format!("#{}", i + 1));
        if let Some(tbl) = entry.as_table() {
            for key in tbl.keys().filter(|k| !KNOWN_KEYS.contains(&k.as_str())) {
                cfg.warnings.push(format!("tool '{label}': unknown key '{key}'"));
            }
        }
        let def: ToolDef = match entry.clone().try_into() {
            Ok(d) => d,
            Err(e) => {
                cfg.warnings.push(format!("tool '{label}' skipped: {}", e.message()));
                continue;
            }
        };
        if let Some(problem) = validate(&def) {
            cfg.warnings.push(format!("tool '{label}' skipped: {problem}"));
            continue;
        }
        if let Some(slot) = def.slot {
            if let Some(other) = cfg.tools.iter().find(|t| t.group == def.group && t.slot == Some(slot)) {
                cfg.warnings.push(format!(
                    "slot conflict: '{}' and '{}' both use slot {slot} in group {}; '{}' is ignored",
                    other.name, def.name, def.group, def.name
                ));
                continue;
            }
        }
        cfg.tools.push(def);
    }
    cfg
}

fn validate(d: &ToolDef) -> Option<String> {
    if d.name.trim().is_empty() {
        return Some("empty name".into());
    }
    if d.command.trim().is_empty() {
        return Some("empty command".into());
    }
    if !(1..=MAX_GROUPS).contains(&d.group) {
        return Some(format!("group {} outside 1..={MAX_GROUPS}", d.group));
    }
    if let Some(s) = d.slot {
        if !(1..=MAX_SLOTS).contains(&s) {
            return Some(format!("slot {s} outside 1..={MAX_SLOTS}"));
        }
    }
    None
}

/// Load `path`; a missing file is an empty config, not an error.
pub fn load(path: &std::path::Path) -> ToolsConfig {
    match std::fs::read_to_string(path) {
        Ok(s) => parse(&s),
        Err(_) => ToolsConfig::default(),
    }
}

// ---- command registry entry points -------------------------------------

const OUTPUT_PANEL: &str = "output";

/// Commands this module contributes to the registry. Actually launching a
/// tool needs process spawning, so the shell handles `tools_run_slot_N` by
/// calling `shell_runner::start_tool`; here they only reveal the panel.
pub fn commands() -> Vec<CommandSpec> {
    let spec = |id: &str, caption: &str, chord: Option<&str>| CommandSpec {
        id: id.into(),
        caption: caption.into(),
        category: "Tools".into(),
        default_chord: chord.map(Into::into),
        when: None, ..Default::default()
    };
    let mut v = vec![
        spec("tools_toggle_output", "Tools: Toggle Output Panel", Some("ctrl+`")),
        spec("tools_stop", "Tools: Stop Running Tool", None),
        spec("tools_next_error", "Tools: Next Error", Some("f4")),
        spec("tools_prev_error", "Tools: Previous Error", Some("shift+f4")),
        spec("tools_goto_file_at_selection", "Tools: Go to File at Selection", None),
    ];
    for n in 1..=MAX_SLOTS {
        let chord = format!("ctrl+{}", n % 10);
        v.push(spec(&format!("tools_run_slot_{n}"), &format!("Tools: Run Tool {n}"), Some(&chord)));
    }
    v
}

/// Run command `id`; `None` = not mine. Process-spawning and jump commands
/// need shell state (running handle, output buffer), so this only opens the
/// output panel; the shell then calls the runner / `OutputBuffer` APIs.
pub fn run(id: &str, ctx: &CmdCtx) -> Option<CmdOutcome> {
    let _ = ctx;
    let mine = id.starts_with("tools_run_slot_")
        || matches!(id, "tools_toggle_output" | "tools_stop" | "tools_next_error" | "tools_prev_error" | "tools_goto_file_at_selection");
    mine.then(|| CmdOutcome { open_panel: Some(PanelId(OUTPUT_PANEL.into())), ..Default::default() })
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"
[[tool]]
name = "Build"
command = "cargo"
args = ["build"]
slot = 1
save_before = "all"

[[tool]]
name = "Fmt"
command = "rustfmt"
slot = 2
file_types = ["rs"]
output = "replace_selection"
env = { RUST_LOG = "debug" }
auto_run = "on_save"
"#;

    #[test]
    fn parses_full_fixture() {
        let c = parse(SAMPLE);
        assert!(c.warnings.is_empty(), "{:?}", c.warnings);
        assert_eq!(c.tools.len(), 2);
        let b = &c.tools[0];
        assert_eq!((b.group, b.slot, b.save_before, b.output, b.auto_run, b.shell), (1, Some(1), SavePolicy::All, OutputSink::Panel, AutoRun::Never, false));
        let f = &c.tools[1];
        assert_eq!((f.output, f.auto_run), (OutputSink::ReplaceSelection, AutoRun::OnSave));
        assert_eq!(f.env["RUST_LOG"], "debug");
    }

    #[test]
    fn unknown_keys_warn_but_tool_loads() {
        let c = parse("[[tool]]\nname='a'\ncommand='x'\nbogus=1\n[extra]\nk=1\n");
        assert_eq!(c.tools.len(), 1);
        assert!(c.warnings.iter().any(|w| w.contains("unknown key 'bogus'")));
        assert!(c.warnings.iter().any(|w| w.contains("unknown top-level key 'extra'")));
    }

    #[test]
    fn invalid_entries_are_skipped_with_reason() {
        let c = parse("[[tool]]\nname='a'\ncommand=''\n[[tool]]\nname='b'\ncommand='x'\ngroup=21\n[[tool]]\nname='c'\ncommand='x'\nslot=11\n[[tool]]\ncommand='x'\n");
        assert!(c.tools.is_empty());
        assert_eq!(c.warnings.len(), 4, "{:?}", c.warnings);
        assert!(!parse("not [valid").warnings.is_empty());
    }

    #[test]
    fn slot_conflicts_reported_first_wins() {
        let c = parse("[[tool]]\nname='a'\ncommand='x'\nslot=1\n[[tool]]\nname='b'\ncommand='y'\nslot=1\n[[tool]]\nname='c'\ncommand='z'\nslot=1\ngroup=2\n");
        assert_eq!(c.tools.len(), 2);
        assert!(c.warnings.iter().any(|w| w.contains("slot conflict") && w.contains("'a'") && w.contains("'b'")));
        assert_eq!(c.by_slot(1, 1, None).unwrap().name, "a");
        assert_eq!(c.by_slot(2, 1, None).unwrap().name, "c");
    }

    #[test]
    fn file_type_binding_filters_group() {
        let c = parse(SAMPLE);
        assert_eq!(c.for_group(1, Some("rs")).len(), 2);
        assert_eq!(c.for_group(1, Some("txt")).len(), 1);
        assert_eq!(c.for_group(1, None).len(), 1);
        assert_eq!(c.for_group(1, Some("RS"))[1].name, "Fmt");
    }

    #[test]
    fn commands_are_unique_and_run_is_scoped() {
        let cmds = commands();
        let mut ids: Vec<_> = cmds.iter().map(|c| c.id.clone()).collect();
        ids.sort();
        ids.dedup();
        assert_eq!(ids.len(), cmds.len());
        assert_eq!(cmds.iter().find(|c| c.id == "tools_run_slot_10").unwrap().default_chord.as_deref(), Some("ctrl+0"));
        let ctx = CmdCtx::default();
        assert!(run("tools_next_error", &ctx).unwrap().open_panel.is_some());
        assert!(run("other_cmd", &ctx).is_none());
    }
}
