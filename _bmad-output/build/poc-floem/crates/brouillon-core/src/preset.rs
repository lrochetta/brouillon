#![allow(dead_code)]

//! Moldu / Geek preset — the signature dual-mode UX.
//!
//! Same binary, two presets that reshape chrome and defaults at runtime.
//! Switchable globally with Cmd-Shift-M (binding lives in main.rs).
//!
//! Reference: internal design notes (not published) §2.

/// The two presets. Keep this enum SIZED 1 byte (no payload) so it's cheap
/// to copy through RwSignal.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Preset {
    Moldu,
    Geek,
}

/// Style of bracket-pair highlighting.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum BracketMatchStyle {
    Underline,
    SolidBox,
    Both,
    None,
}

impl BracketMatchStyle {
    /// Return the next style in the cycle.
    pub fn next(self) -> Self {
        match self {
            BracketMatchStyle::Underline => BracketMatchStyle::SolidBox,
            BracketMatchStyle::SolidBox => BracketMatchStyle::Both,
            BracketMatchStyle::Both => BracketMatchStyle::None,
            BracketMatchStyle::None => BracketMatchStyle::Underline,
        }
    }

    /// Human-readable label for status/settings.
    pub fn label(self) -> &'static str {
        match self {
            BracketMatchStyle::Underline => "Souligné",
            BracketMatchStyle::SolidBox => "Boîte",
            BracketMatchStyle::Both => "Les deux",
            BracketMatchStyle::None => "Désactivé",
        }
    }
}

/// What the editor LOOKS LIKE and DOES under a given preset.
///
/// This is a flat config, not a trait — easy to A/B-render in the Mode
/// Mirror first-run picker.
#[derive(Debug, Clone)]
pub struct PresetConfig {
    // ─── Visible UI affordances ─────────────────────────────────────────────
    pub show_tab_bar: bool,
    pub show_status_line: bool,
    pub show_minimap: bool,         // future M2+
    pub show_line_numbers: bool,
    pub show_gutter: bool,
    pub show_menu_bar: bool,        // Moldu-only ; Geek hides menus
    pub show_sidebar: bool,          // Moldu-only ; document selector pane
    pub show_palette_hint: bool,    // a "Press Cmd-K to do anything" hint, Geek
    pub bracket_style: BracketMatchStyle, // dynamic bracket highlighting style

    // ─── Mode behavior ──────────────────────────────────────────────────────
    pub modal_editing_enabled: bool,    // Vim-like, Geek-only opt-in
    pub keyboard_first: bool,           // Geek prefers keyboard ; Moldu = mouse first
    pub palette_on_startup: bool,       // open palette as homepage (Geek)
    pub friendly_dialogs: bool,         // native save/open dialogs vs minimalist (Moldu=true)
    pub show_inline_help_bubbles: bool, // tooltips on hover (Moldu=true)
    pub vim_style_status: bool,         // bottom-bar :command (Geek)

    // ─── Defaults ───────────────────────────────────────────────────────────
    pub default_font_size: f64,
    pub default_theme_name: &'static str, // matches a theme::ThemeRegistry name
    pub default_line_height: f64,         // multiplier of font_size

    // ─── Onboarding ─────────────────────────────────────────────────────────
    pub show_welcome_screen: bool,    // Moldu-only first-run
    pub show_keymap_cheatsheet: bool, // Geek first-run
}

impl Preset {
    /// Return the canonical config for this preset.
    pub fn config(self) -> PresetConfig {
        match self {
            Preset::Moldu => PresetConfig {
                // Visible UI affordances
                show_tab_bar: true,
                show_status_line: true,
                show_minimap: true,
                show_line_numbers: true,
                show_gutter: true,
                show_menu_bar: true,
                show_sidebar: true,
                show_palette_hint: false,
                bracket_style: BracketMatchStyle::Both,
                // Mode behavior
                modal_editing_enabled: false,
                keyboard_first: false,
                palette_on_startup: false,
                friendly_dialogs: true,
                show_inline_help_bubbles: true,
                vim_style_status: false,
                // Defaults
                default_font_size: 14.0,
                default_theme_name: "Tokyo Night",
                default_line_height: 1.5,
                // Onboarding
                show_welcome_screen: true,
                show_keymap_cheatsheet: false,
            },
            Preset::Geek => PresetConfig {
                // Visible UI affordances
                show_tab_bar: true,
                show_status_line: true,
                show_minimap: false,
                show_line_numbers: true,
                show_gutter: true,
                show_menu_bar: false,
                show_sidebar: false,
                show_palette_hint: true,
                bracket_style: BracketMatchStyle::Underline,
                // Mode behavior
                modal_editing_enabled: false, // opt-in only
                keyboard_first: true,
                palette_on_startup: true,
                friendly_dialogs: false,
                show_inline_help_bubbles: false,
                vim_style_status: true,
                // Defaults
                default_font_size: 13.0,
                default_theme_name: "Tokyo Night",
                default_line_height: 1.4,
                // Onboarding
                show_welcome_screen: false,
                show_keymap_cheatsheet: true,
            },
        }
    }

    /// Switch to the other preset.
    pub fn flipped(self) -> Self {
        match self {
            Preset::Moldu => Preset::Geek,
            Preset::Geek => Preset::Moldu,
        }
    }

    /// Alias to flipped for toggling.
    pub fn toggle(self) -> Self {
        self.flipped()
    }

    /// Short human-readable name. Used for UI labels and telemetry.
    pub fn name(self) -> &'static str {
        match self {
            Preset::Moldu => "Moldu",
            Preset::Geek => "Geek",
        }
    }

    /// One-line description for the Mode Mirror picker.
    pub fn description(self) -> &'static str {
        match self {
            Preset::Moldu => {
                "Mainstream, GUI-first. Menus, tooltips, native dialogs, friendly defaults."
            }
            Preset::Geek => {
                "Keyboard-first. Command palette as homepage, minimalist chrome, opt-in modal editing."
            }
        }
    }
}

/// Default preset for a fresh install ; first-run Mode Mirror picker can
/// override this.
pub const DEFAULT_PRESET: Preset = Preset::Moldu;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn moldu_defaults_are_friendly() {
        let cfg = Preset::Moldu.config();
        assert!(cfg.show_menu_bar, "Moldu should show the menu bar");
        assert!(cfg.show_welcome_screen, "Moldu should greet on first run");
        assert!(cfg.friendly_dialogs, "Moldu should use native dialogs");
        assert!(cfg.show_inline_help_bubbles, "Moldu should show tooltips");
        assert!(cfg.show_minimap, "Moldu wants the overview minimap");
        assert!(!cfg.keyboard_first, "Moldu is mouse-first");
        assert!(!cfg.palette_on_startup, "Moldu opens to a normal editor view");
        assert!(!cfg.vim_style_status, "Moldu doesn't need vim status");
        assert!(!cfg.show_palette_hint, "Moldu doesn't need the Cmd-K hint");
        assert!(!cfg.modal_editing_enabled, "Modal editing is off by default");
    }

    #[test]
    fn geek_defaults_are_keyboard_first() {
        let cfg = Preset::Geek.config();
        assert!(cfg.keyboard_first, "Geek is keyboard-first");
        assert!(cfg.palette_on_startup, "Geek opens to the palette");
        assert!(cfg.show_palette_hint, "Geek surfaces Cmd-K discoverability");
        assert!(cfg.vim_style_status, "Geek wants the : command line");
        assert!(cfg.show_keymap_cheatsheet, "Geek first-run = cheatsheet");
        assert!(!cfg.show_menu_bar, "Geek hides menus in favor of palette");
        assert!(!cfg.show_welcome_screen, "Geek skips the welcome screen");
        assert!(!cfg.friendly_dialogs, "Geek prefers minimalist dialogs");
        assert!(!cfg.show_inline_help_bubbles, "Geek hides tooltips");
        assert!(!cfg.show_minimap, "Geek prefers minimalism");
        assert!(
            !cfg.modal_editing_enabled,
            "Modal editing is opt-in even in Geek"
        );
    }

    #[test]
    fn flipped_toggles_between_moldu_and_geek() {
        assert_eq!(Preset::Moldu.flipped(), Preset::Geek);
        assert_eq!(Preset::Geek.flipped(), Preset::Moldu);
        // Double-flip is identity.
        assert_eq!(Preset::Moldu.flipped().flipped(), Preset::Moldu);
        assert_eq!(Preset::Geek.flipped().flipped(), Preset::Geek);
    }

    #[test]
    fn name_returns_correct_string() {
        assert_eq!(Preset::Moldu.name(), "Moldu");
        assert_eq!(Preset::Geek.name(), "Geek");
    }

    #[test]
    fn default_preset_is_moldu() {
        assert_eq!(DEFAULT_PRESET, Preset::Moldu);
    }

    #[test]
    fn description_is_nonempty() {
        assert!(!Preset::Moldu.description().is_empty());
        assert!(!Preset::Geek.description().is_empty());
        // And the two descriptions should be distinct — otherwise the
        // Mode Mirror picker would show the user the same blurb twice.
        assert_ne!(Preset::Moldu.description(), Preset::Geek.description());
    }

    #[test]
    fn font_size_is_smaller_in_geek() {
        let moldu = Preset::Moldu.config();
        let geek = Preset::Geek.config();
        assert!(
            geek.default_font_size < moldu.default_font_size,
            "Geek font ({}) should be smaller than Moldu ({})",
            geek.default_font_size,
            moldu.default_font_size
        );
        assert!(
            geek.default_line_height < moldu.default_line_height,
            "Geek line-height ({}) should be tighter than Moldu ({})",
            geek.default_line_height,
            moldu.default_line_height
        );
    }

    #[test]
    fn inline_help_disabled_in_geek() {
        assert!(!Preset::Geek.config().show_inline_help_bubbles);
        assert!(Preset::Moldu.config().show_inline_help_bubbles);
    }

    #[test]
    fn preset_enum_is_one_byte() {
        // Documents the "cheap to copy through RwSignal" contract from the
        // doc-comment on `Preset`. If a future contributor adds a payload,
        // this assertion will fail loudly.
        assert_eq!(std::mem::size_of::<Preset>(), 1);
    }

    #[test]
    fn both_presets_share_default_theme() {
        assert_eq!(
            Preset::Moldu.config().default_theme_name,
            Preset::Geek.config().default_theme_name
        );
    }

    #[test]
    fn bracket_style_cycles_back() {
        let start = BracketMatchStyle::Underline;
        let cycled = start.next().next().next().next();
        assert_eq!(start, cycled, "Four nexts should return to start");
    }

    #[test]
    fn bracket_style_labels_non_empty() {
        let styles = [BracketMatchStyle::Underline, BracketMatchStyle::SolidBox,
                      BracketMatchStyle::Both, BracketMatchStyle::None];
        for s in styles {
            assert!(!s.label().is_empty());
        }
    }

    #[test]
    fn toggle_preset() {
        let p = Preset::Moldu;
        let toggled = p.toggle();
        assert_eq!(toggled, Preset::Geek);
        assert_eq!(toggled.toggle(), Preset::Moldu);
    }

    #[test]
    fn geek_hides_sidebar() {
        assert!(!Preset::Geek.config().show_sidebar, "Geek should hide sidebar");
    }

    #[test]
    fn config_values_are_positive() {
        for p in [Preset::Moldu, Preset::Geek] {
            let c = p.config();
            assert!(c.default_font_size > 0.0);
            assert!(c.default_line_height > 0.0);
        }
    }
}
