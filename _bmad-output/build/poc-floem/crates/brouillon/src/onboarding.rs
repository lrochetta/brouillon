//! First-run and About: the pure parts (what to write, when to show, what the About page says).
//! The screens themselves are `brouillon_ui::onboarding_view`; `main.rs` wires them.

use crate::buildinfo;
use crate::settings::{set_raw_key, set_string_key, Layer, Settings};
use brouillon_ui::onboarding_view::FirstRunChoices;

pub const SETTING_DONE: &str = "firstrun.done";
pub const AUTHOR: &str = "Laurent Rochetta";
pub const SITE: &str = "https://brouillon.rochetta.fr";

/// First-run screens are shown once: until `firstrun.done` is written. `BROUILLON_NO_FIRSTRUN=1`
/// (automated tests, kiosk installs) never shows them and writes nothing.
pub fn should_show(settings: &Settings, env_skip: bool) -> bool {
    !env_skip && !settings.get_bool(SETTING_DONE, false)
}

/// The settings file after the first-run answers: Classic keymap, theme mode, the update-check
/// choice (on unless turned off) and the "done" marker. Comments and unknown keys are kept.
pub fn finish_text(existing: &str, ch: &FirstRunChoices) -> String {
    let t = set_raw_key(existing, "keymap", "classic", &ch.classic.to_string());
    let t = set_string_key(&t, "theme", "mode", &ch.theme);
    let t = set_raw_key(&t, "updates", "check", &ch.updates_check.to_string());
    set_raw_key(&t, "firstrun", "done", "true")
}

pub fn settings_of(text: &str) -> Settings {
    Settings::build(&[(Layer::User, text)])
}

/// (label, value) rows of the About page.
pub fn about_rows() -> Vec<(String, String)> {
    let row = |k: &str, v: &str| (k.to_string(), v.to_string());
    vec![
        row("Version", buildinfo::VERSION),
        row("Build", buildinfo::BUILD),
        row("Commit", buildinfo::COMMIT),
        row("Built", buildinfo::DATE_UTC),
        row("Licence", "MIT"),
        row("Author", AUTHOR),
        row("Project site", SITE),
    ]
}

pub fn about_notes() -> Vec<String> {
    vec![
        format!("Copyright (c) 2026 {AUTHOR}. Free and open source under the MIT licence."),
        "No telemetry, no account. The only network request Brouillon can make is the optional update check, which is off until you turn it on.".to_string(),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn choices(classic: bool, theme: &str, updates: bool) -> FirstRunChoices {
        FirstRunChoices { classic, theme: theme.into(), updates_check: updates }
    }

    #[test]
    fn shown_once_and_never_under_the_test_switch() {
        assert!(should_show(&settings_of(""), false));
        assert!(!should_show(&settings_of(""), true));
        let done = finish_text("", &choices(false, "dark", false));
        assert!(!should_show(&settings_of(&done), false));
    }

    #[test]
    fn update_check_is_on_until_the_user_turns_it_off() {
        // Fresh install: the default is ON, the kill switch still wins.
        assert!(crate::update_boot::enabled(&settings_of(""), false));
        assert!(!crate::update_boot::enabled(&settings_of(""), true));
        // Declining on screen 3 turns it off ...
        for ch in [choices(false, "dark", false)] {
            assert!(!crate::update_boot::enabled(&settings_of(&finish_text("", &ch)), false));
        }
        // ... accepting keeps it on, and the dev/test kill switch still wins.
        let on = settings_of(&finish_text("", &choices(true, "light", true)));
        assert!(crate::update_boot::enabled(&on, false));
        assert!(!crate::update_boot::enabled(&on, true));
    }

    #[test]
    fn answers_are_written_and_the_rest_of_the_file_is_kept() {
        let t = finish_text("# mine\n[theme]\nname = \"x\"\n[plugin_x]\nkeep = 1\n", &choices(true, "system", true));
        let s = settings_of(&t);
        assert!(s.get_bool("keymap.classic", false) && s.get_bool("updates.check", false) && s.get_bool(SETTING_DONE, false));
        assert_eq!(s.get_str("theme.mode", ""), "system");
        assert_eq!(s.get_str("theme.name", ""), "x");
        assert!(t.contains("# mine") && t.contains("[plugin_x]\nkeep = 1"), "{t}");
        assert!(s.errors.is_empty());
    }

    #[test]
    fn about_page_has_every_required_field() {
        let rows = about_rows();
        let get = |k: &str| rows.iter().find(|(l, _)| l == k).map(|(_, v)| v.clone()).unwrap_or_default();
        assert_eq!(get("Version"), buildinfo::VERSION);
        assert_eq!(get("Build"), buildinfo::BUILD);
        assert_eq!(get("Commit"), buildinfo::COMMIT);
        assert_eq!(get("Built"), buildinfo::DATE_UTC);
        assert_eq!(get("Licence"), "MIT");
        assert!(get("Author").contains("Laurent Rochetta"));
        assert!(get("Project site").starts_with("https://"));
        assert!(about_notes().iter().any(|n| n.contains("No telemetry")));
    }
}
