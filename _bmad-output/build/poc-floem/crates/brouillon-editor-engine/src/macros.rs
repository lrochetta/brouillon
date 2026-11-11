//! Keyboard macro: record what the user did (commands, typed text, a few editing keys) and play
//! it back. Pure state; the shell feeds `Recorder` from its dispatcher / key handler and replays
//! the steps through the same code paths it uses for live input.
//!
//! Recorded: commands run from menus/palette/chords (`cmd`), typed characters (`text`, runs are
//! merged), and the keys `enter`, `backspace`, `delete`, `left`, `right`, `up`, `down`, `home`,
//! `end` (`key`). Not recorded: macro commands themselves and commands that open dialogs or leave
//! the app. Mouse actions are not recorded. One macro slot, saved to `macro.toml`.

use serde::{Deserialize, Serialize};
use std::path::Path;

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Step {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cmd: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key: Option<String>,
}

impl Step {
    pub fn cmd(id: &str) -> Self {
        Self { cmd: Some(id.into()), ..Default::default() }
    }
    pub fn text(s: &str) -> Self {
        Self { text: Some(s.into()), ..Default::default() }
    }
    pub fn key(k: &str) -> Self {
        Self { key: Some(k.into()), ..Default::default() }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Macro {
    #[serde(default)]
    pub steps: Vec<Step>,
}

/// Keys the recorder accepts as `key` steps.
pub const KEYS: &[&str] = &["enter", "backspace", "delete", "left", "right", "up", "down", "home", "end"];

/// Commands that must never end up inside a macro.
pub fn is_recordable(cmd: &str) -> bool {
    !(cmd.starts_with("macro_")
        || cmd.starts_with("help_")
        || matches!(cmd, "app_quit" | "view_command_palette" | "file_open" | "file_save_as" | "file_close" | "link_kill" | "quick_open" | "goto_function" | "go_to"))
}

#[derive(Debug, Default)]
pub struct Recorder {
    recording: bool,
    steps: Vec<Step>,
}

impl Recorder {
    pub fn is_recording(&self) -> bool {
        self.recording
    }

    pub fn start(&mut self) {
        self.recording = true;
        self.steps.clear();
    }

    /// Stop and hand over the macro.
    pub fn stop(&mut self) -> Macro {
        self.recording = false;
        Macro { steps: std::mem::take(&mut self.steps) }
    }

    pub fn len(&self) -> usize {
        self.steps.len()
    }

    pub fn is_empty(&self) -> bool {
        self.steps.is_empty()
    }

    pub fn record_command(&mut self, id: &str) {
        if self.recording && is_recordable(id) {
            self.steps.push(Step::cmd(id));
        }
    }

    pub fn record_text(&mut self, s: &str) {
        if !self.recording || s.is_empty() {
            return;
        }
        if let Some(Step { text: Some(t), .. }) = self.steps.last_mut() {
            t.push_str(s);
        } else {
            self.steps.push(Step::text(s));
        }
    }

    pub fn record_key(&mut self, k: &str) {
        if self.recording && KEYS.contains(&k) {
            self.steps.push(Step::key(k));
        }
    }
}

impl Macro {
    pub fn is_empty(&self) -> bool {
        self.steps.is_empty()
    }

    pub fn to_toml(&self) -> String {
        toml::to_string(self).unwrap_or_default()
    }

    pub fn from_toml(s: &str) -> Option<Macro> {
        toml::from_str(s).ok()
    }

    pub fn save(&self, dir: &Path) -> std::io::Result<()> {
        std::fs::create_dir_all(dir)?;
        std::fs::write(dir.join("macro.toml"), self.to_toml())
    }

    pub fn load(dir: &Path) -> Option<Macro> {
        Macro::from_toml(&std::fs::read_to_string(dir.join("macro.toml")).ok()?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn records_commands_text_and_keys_in_order_and_merges_text_runs() {
        let mut r = Recorder::default();
        r.record_command("edit_copy"); // not recording yet
        r.start();
        r.record_text("a");
        r.record_text("bc");
        r.record_key("enter");
        r.record_command("case_upper");
        r.record_text("x");
        r.record_key("nonsense");
        let m = r.stop();
        assert!(!r.is_recording());
        assert_eq!(m.steps, vec![Step::text("abc"), Step::key("enter"), Step::cmd("case_upper"), Step::text("x")]);
    }

    #[test]
    fn macro_and_dialog_commands_are_not_recorded() {
        let mut r = Recorder::default();
        r.start();
        for id in ["macro_record_toggle", "macro_play", "app_quit", "file_open", "view_command_palette", "help_about"] {
            r.record_command(id);
        }
        assert!(r.is_empty());
        r.record_command("sort_asc");
        assert_eq!(r.len(), 1);
    }

    #[test]
    fn start_discards_the_previous_take() {
        let mut r = Recorder::default();
        r.start();
        r.record_text("old");
        r.start();
        assert!(r.stop().is_empty());
    }

    #[test]
    fn toml_roundtrip_and_disk() {
        let m = Macro { steps: vec![Step::text("héllo \"q\"\n"), Step::cmd("sort_asc"), Step::key("home")] };
        assert_eq!(Macro::from_toml(&m.to_toml()), Some(m.clone()));
        let d = tempfile::tempdir().unwrap();
        m.save(d.path()).unwrap();
        assert_eq!(Macro::load(d.path()), Some(m));
        assert_eq!(Macro::load(&d.path().join("nope")), None);
        assert_eq!(Macro::from_toml("steps = 3"), None);
    }
}
