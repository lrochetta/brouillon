//! Declarative command registry (BK-067): the single list palette, keymap,
//! menus and toolbar read. The shell concatenates every lot's `commands()`
//! and registers them here.

use std::collections::HashMap;

use crate::contract::{CmdCtx, CmdOutcome, CommandSpec, PanelId};
use crate::keymap::{eval_when, Context};

/// Why a spec was not registered.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RegistryError {
    DuplicateId(String),
    /// Ids are `[a-z0-9_.]+` (stable snake_case, optional dotted namespace).
    InvalidId(String),
}

impl std::fmt::Display for RegistryError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::DuplicateId(id) => write!(f, "duplicate command id `{id}`"),
            Self::InvalidId(id) => write!(f, "invalid command id `{id}` (use lowercase snake_case)"),
        }
    }
}

fn valid_id(id: &str) -> bool {
    !id.is_empty()
        && !id.starts_with(['.', '_'])
        && id.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' || c == '.')
}

#[derive(Debug, Clone, Default)]
pub struct Registry {
    specs: Vec<CommandSpec>,
    index: HashMap<String, usize>,
    last_run: Option<String>,
}

impl Registry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Register one spec; duplicates and malformed ids are rejected.
    pub fn register(&mut self, spec: CommandSpec) -> Result<(), RegistryError> {
        if !valid_id(&spec.id) {
            return Err(RegistryError::InvalidId(spec.id));
        }
        if self.index.contains_key(&spec.id) {
            return Err(RegistryError::DuplicateId(spec.id));
        }
        self.index.insert(spec.id.clone(), self.specs.len());
        self.specs.push(spec);
        Ok(())
    }

    /// Register every spec of a lot; rejected ones are returned, the rest kept.
    pub fn register_all(&mut self, specs: Vec<CommandSpec>) -> Vec<RegistryError> {
        specs.into_iter().filter_map(|s| self.register(s).err()).collect()
    }

    pub fn get(&self, id: &str) -> Option<&CommandSpec> {
        self.index.get(id).map(|&i| &self.specs[i])
    }

    pub fn contains(&self, id: &str) -> bool {
        self.index.contains_key(id)
    }

    pub fn len(&self) -> usize {
        self.specs.len()
    }

    pub fn is_empty(&self) -> bool {
        self.specs.is_empty()
    }

    /// All specs in registration order.
    pub fn all(&self) -> &[CommandSpec] {
        &self.specs
    }

    /// Specs whose `when` holds in `ctx` (palette view for the current preset/context).
    pub fn available(&self, ctx: &Context) -> Vec<&CommandSpec> {
        self.specs.iter().filter(|s| eval_when(s.when.as_deref(), ctx)).collect()
    }

    /// Specs of one category, in registration order.
    pub fn in_category<'a>(&'a self, category: &'a str) -> impl Iterator<Item = &'a CommandSpec> {
        self.specs.iter().filter(move |s| s.category == category)
    }

    /// Like `in_category`, ASCII case-insensitive (menu ids are lowercase).
    pub fn in_category_ci<'a>(&'a self, category: &'a str) -> impl Iterator<Item = &'a CommandSpec> {
        self.specs.iter().filter(move |s| s.category.eq_ignore_ascii_case(category))
    }

    /// Distinct categories in first-seen order.
    pub fn categories(&self) -> Vec<&str> {
        let mut out: Vec<&str> = Vec::new();
        for s in &self.specs {
            if !out.contains(&s.category.as_str()) {
                out.push(&s.category);
            }
        }
        out
    }

    /// Remember the last dispatched command (Inspector).
    pub fn record_run(&mut self, id: &str) {
        if self.contains(id) {
            self.last_run = Some(id.to_string());
        }
    }

    pub fn last_command(&self) -> Option<&CommandSpec> {
        self.last_run.as_deref().and_then(|id| self.get(id))
    }
}

fn spec(id: &str, caption: &str, category: &str, chord: Option<&str>) -> CommandSpec {
    CommandSpec {
        id: id.into(),
        caption: caption.into(),
        category: category.into(),
        default_chord: chord.map(Into::into),
        when: None, ..Default::default()
    }
}

/// Commands this module contributes to the registry.
pub fn commands() -> Vec<CommandSpec> {
    vec![
        spec("quick_open", "File: Quick Open", "File", Some("ctrl+p")),
        spec("preferences_open_settings", "Preferences: Open Settings", "Preferences", Some("ctrl+,")),
        spec("preferences_open_keymap", "Preferences: Open Keyboard Shortcuts", "Preferences", None),
    ]
}

/// Run command `id`; `None` = not mine.
pub fn run(id: &str, ctx: &CmdCtx) -> Option<CmdOutcome> {
    let _ = ctx;
    let panel = match id {
        "quick_open" => "quick_open",
        "preferences_open_settings" => "settings_editor",
        "preferences_open_keymap" => "keymap_editor",
        _ => return None,
    };
    Some(CmdOutcome { open_panel: Some(PanelId(panel.into())), ..Default::default() })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(id: &str, when: Option<&str>) -> CommandSpec {
        CommandSpec { id: id.into(), caption: format!("Cat: {id}"), category: "Cat".into(), when: when.map(Into::into), ..Default::default() }
    }

    #[test]
    fn rejects_duplicates_and_bad_ids() {
        let mut r = Registry::new();
        let errs = r.register_all(vec![s("a_b", None), s("a_b", None), s("Bad Id", None), s("text.sort_lines", None)]);
        assert_eq!(errs, vec![RegistryError::DuplicateId("a_b".into()), RegistryError::InvalidId("Bad Id".into())]);
        assert_eq!(r.len(), 2);
    }

    #[test]
    fn palette_view_filtered_by_context() {
        let mut r = Registry::new();
        r.register_all(vec![s("always", None), s("geek_only", Some("preset == geek")), s("sel", Some("!selection_empty"))]);
        let mut ctx = Context::new();
        ctx.set_str("preset", "moldu").set_bool("selection_empty", true);
        let ids: Vec<_> = r.available(&ctx).iter().map(|c| c.id.as_str()).collect();
        assert_eq!(ids, vec!["always"]);
        ctx.set_str("preset", "geek").set_bool("selection_empty", false);
        assert_eq!(r.available(&ctx).len(), 3);
    }

    #[test]
    fn last_command_tracking_ignores_unknown() {
        let mut r = Registry::new();
        r.register(s("x", None)).unwrap();
        r.record_run("nope");
        assert!(r.last_command().is_none());
        r.record_run("x");
        assert_eq!(r.last_command().unwrap().id, "x");
    }

    #[test]
    fn own_commands_register_cleanly_and_run() {
        let mut r = Registry::new();
        assert!(r.register_all(commands()).is_empty());
        for c in commands() {
            assert!(run(&c.id, &CmdCtx::default()).is_some(), "{}", c.id);
        }
        assert!(run("other", &CmdCtx::default()).is_none());
        assert_eq!(r.categories(), vec!["File", "Preferences"]);
    }
}
