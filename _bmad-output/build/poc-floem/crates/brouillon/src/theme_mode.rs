//! Dark / light / system mode: reads `theme.mode` + `theme.name` from the user
//! settings, resolves them to one of the built-in themes, persists changes, and
//! keeps the Windows title bar in step. The active theme itself lives in
//! `brouillon_ui::theme_ctx` (one reactive index read by every surface).

use std::path::PathBuf;
use std::sync::Mutex;
use std::time::Duration;

use brouillon_core::settings::{set_string_key, Layer, Settings};
use brouillon_core::theme::{ThemeMode, ThemeRegistry};
use brouillon_ui::theme_ctx::theme_idx_signal;
use floem::action::exec_after;
use floem::reactive::{RwSignal, SignalGet, SignalUpdate};

struct State {
    file: PathBuf,
    mode: ThemeMode,
    /// Last explicitly chosen theme name ("" = none).
    name: String,
}

static STATE: Mutex<Option<State>> = Mutex::new(None);

/// How often the OS light/dark switch is re-read while the mode is `system`.
const SYSTEM_POLL: Duration = Duration::from_secs(2);

/// `(mode, theme name)` from the text of the user settings file.
pub fn parse_settings(text: &str) -> (ThemeMode, String) {
    let s = Settings::build(&[(Layer::User, text)]);
    (ThemeMode::parse(&s.get_str("theme.mode", "dark")).unwrap_or_default(), s.get_str("theme.name", ""))
}

/// Index of the theme for `mode` given the OS preference (`None` = unknown).
pub fn resolve(mode: ThemeMode, name: &str, system_light: Option<bool>) -> usize {
    let name = (!name.trim().is_empty()).then_some(name);
    ThemeRegistry::new().pick(mode.resolves_dark(system_light), name)
}

/// OS preference: `Some(true)` = light apps. Always `None` off Windows.
pub fn system_prefers_light() -> Option<bool> {
    #[cfg(windows)]
    {
        crate::win_theme::apps_use_light_theme()
    }
    #[cfg(not(windows))]
    {
        None
    }
}

fn is_dark(idx: usize) -> bool {
    brouillon_core::theme::current_theme(idx).is_dark
}

fn apply_titlebar(_dark: bool) {
    #[cfg(windows)]
    crate::win_theme::set_titlebar_dark(_dark);
}

/// Read the saved mode, select the theme, start the title-bar sync and the
/// system-mode watcher. Call once from the UI thread; returns the theme index
/// signal every view shares.
pub fn init(settings_file: PathBuf) -> RwSignal<usize> {
    let text = std::fs::read_to_string(&settings_file).unwrap_or_default();
    let (mode, name) = parse_settings(&text);
    let idx = resolve(mode, &name, system_prefers_light());
    let sig = theme_idx_signal();
    sig.set(idx);
    *STATE.lock().unwrap() = Some(State { file: settings_file, mode, name });
    eprintln!("[theme] mode {} -> {}", mode.as_str(), brouillon_core::theme::current_theme(idx).name);

    // The window does not exist yet: retry for a few seconds on a helper thread.
    #[cfg(windows)]
    {
        let dark = is_dark(idx);
        std::thread::spawn(move || {
            for _ in 0..200 {
                std::thread::sleep(Duration::from_millis(50));
                if crate::win_theme::set_titlebar_dark(dark) {
                    return;
                }
            }
        });
    }
    watch_system();
    sig
}

/// Re-read the OS switch every few seconds; only acts while the mode is `system`.
fn watch_system() {
    exec_after(SYSTEM_POLL, |_| {
        let (mode, name) = match STATE.lock().unwrap().as_ref() {
            Some(s) => (s.mode, s.name.clone()),
            None => return,
        };
        if mode == ThemeMode::System {
            show(resolve(mode, &name, system_prefers_light()));
        }
        watch_system();
    });
}

fn show(idx: usize) {
    let sig = theme_idx_signal();
    if sig.get_untracked() != idx {
        sig.set(idx);
        eprintln!("[theme] -> {}", brouillon_core::theme::current_theme(idx).name);
    }
    apply_titlebar(is_dark(idx));
}

fn persist(st: &State) {
    let text = std::fs::read_to_string(&st.file).unwrap_or_default();
    let text = set_string_key(&text, "theme", "mode", st.mode.as_str());
    let text = set_string_key(&text, "theme", "name", &st.name);
    if let Some(dir) = st.file.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    if let Err(e) = std::fs::write(&st.file, text) {
        eprintln!("[theme] could not save the theme setting: {e}");
    }
}

/// The settings file was changed by someone else (the Preferences dialog): re-read the mode and
/// the preferred theme from it and show the result now.
pub fn reload() {
    let idx = {
        let mut g = STATE.lock().unwrap();
        let Some(st) = g.as_mut() else { return };
        let text = std::fs::read_to_string(&st.file).unwrap_or_default();
        (st.mode, st.name) = parse_settings(&text);
        resolve(st.mode, &st.name, system_prefers_light())
    };
    show(idx);
}

/// Menu / palette: switch to `mode` (keeping the preferred theme when its kind fits) and save it.
pub fn set_mode(mode: ThemeMode) {
    let idx = {
        let mut g = STATE.lock().unwrap();
        let Some(st) = g.as_mut() else { return };
        st.mode = mode;
        persist(st);
        resolve(mode, &st.name, system_prefers_light())
    };
    show(idx);
}

/// Ctrl+T / "Cycle Theme": next of the 6 themes; the choice fixes the mode to the theme's kind and is saved.
pub fn cycle() {
    let sig = theme_idx_signal();
    let reg = ThemeRegistry::new();
    let next = (sig.get_untracked() + 1) % reg.themes.len();
    let t = &reg.themes[next];
    if let Some(st) = STATE.lock().unwrap().as_mut() {
        st.mode = if t.is_dark { ThemeMode::Dark } else { ThemeMode::Light };
        st.name = t.name.to_string();
        persist(st);
    }
    show(next);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn name_of(i: usize) -> &'static str {
        ThemeRegistry::new().themes[i].name
    }

    #[test]
    fn settings_text_to_mode_and_name() {
        assert_eq!(parse_settings(""), (ThemeMode::Dark, String::new()));
        assert_eq!(parse_settings("[theme]\nmode = \"light\"\nname = \"GitHub Light\""), (ThemeMode::Light, "GitHub Light".into()));
        assert_eq!(parse_settings("[theme]\nmode = \"System\"").0, ThemeMode::System);
        assert_eq!(parse_settings("[theme]\nmode = \"purple\"").0, ThemeMode::Dark, "unknown value falls back to dark");
    }

    #[test]
    fn resolution_by_mode_and_os_preference() {
        assert_eq!(name_of(resolve(ThemeMode::Dark, "", Some(true))), "Tokyo Night");
        assert_eq!(name_of(resolve(ThemeMode::Light, "", Some(false))), "GitHub Light");
        assert_eq!(name_of(resolve(ThemeMode::System, "", Some(true))), "GitHub Light");
        assert_eq!(name_of(resolve(ThemeMode::System, "", Some(false))), "Tokyo Night");
        assert_eq!(name_of(resolve(ThemeMode::System, "", None)), "Tokyo Night");
        assert_eq!(name_of(resolve(ThemeMode::System, "Gruvbox Dark", Some(false))), "Gruvbox Dark");
        assert_eq!(name_of(resolve(ThemeMode::System, "Gruvbox Dark", Some(true))), "GitHub Light", "saved dark theme ignored in light");
        assert_eq!(name_of(resolve(ThemeMode::Dark, "  ", None)), "Tokyo Night");
    }

    #[test]
    fn saved_choice_round_trips_through_the_settings_text() {
        let text = set_string_key(&set_string_key("# keep me\n", "theme", "mode", "system"), "theme", "name", "Rose Pine Moon");
        assert!(text.starts_with("# keep me"));
        assert_eq!(parse_settings(&text), (ThemeMode::System, "Rose Pine Moon".into()));
    }

    #[cfg(windows)]
    #[test]
    fn the_os_preference_is_readable_on_windows() {
        // Machines without the key return None; either answer must simply not panic.
        let _ = system_prefers_light();
    }
}
