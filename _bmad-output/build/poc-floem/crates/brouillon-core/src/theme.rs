#![allow(dead_code)]

//! Editor color themes. Less-is-more design language. Each theme is a flat struct of `Color` slots ; new slots are added
//! as the view layer grows. Day-1 PoC's hardcoded `Color::rgb8` calls in
//! `main.rs` will be replaced with `theme.slot()` lookups next round (M1
//! view wire-up).
//!
//! Palettes are anchored to canonical upstream sources — no fabricated
//! hexes:
//! - Tokyo Night       — folke/tokyonight.nvim (`night` variant)
//! - Catppuccin Mocha  — catppuccin/catppuccin style-guide
//! - Rose Pine Moon    — rosepinetheme.com/palette
//! - Gruvbox Dark      — morhetz/gruvbox
//! - Solarized Dark    — ethanschoonover.com/solarized
//! - GitHub Light      — primer.style/foundations/color
//!
//! Accessibility wins over fidelity: where an upstream hex failed WCAG AA
//! (4.5:1 text, 3:1 interface elements; see `mode_and_surface_tests`), only its
//! lightness was moved, keeping the hue (mostly dim text, comments, Solarized).
//!
//! When a theme didn't expose an exact slot (e.g. a dedicated "FPS-ok"
//! green), the closest palette member was chosen — documented inline.

use floem::peniko::Color;

/// Flat bag of color slots used across the editor surface. Cheap to clone
/// (it's all `Copy`-able `Color`s and a `&'static str`), so it can be
/// stuffed into a reactive signal without lifetime games.
///
/// Slot list is intentionally minimal — 17 entries cover everything the
/// day-1 PoC currently hardcodes plus the bare minimum tree-sitter palette.
/// New slots get added as the view layer grows.
#[derive(Debug, Clone)]
pub struct Theme {
    pub name: &'static str,
    pub is_dark: bool,

    // Surfaces
    pub bg: Color,               // editor body background
    pub bg_chrome: Color,        // header / status / panels
    pub bg_current_line: Color,  // current-line highlight
    pub border: Color,           // 1px separators

    // Foregrounds
    pub fg: Color,        // body text
    pub fg_dim: Color,    // gutter, hints
    pub fg_status: Color, // status line text

    // Accents
    pub accent: Color,  // brand accent (caret, focus ring, primary buttons)
    pub fps_ok: Color,  // green-ish — FPS counter on healthy
    pub warn: Color,    // yellow / orange — warnings
    pub error: Color,   // red — errors

    // Syntax — minimum 6-class palette for tree-sitter
    pub syn_keyword: Color,
    pub syn_string: Color,
    pub syn_comment: Color,
    pub syn_type: Color,
    pub syn_number: Color,
    pub syn_punct: Color,

    // Syntax - table-driven lexer categories (keyword groups, tags, ...).
    pub syn_keyword2: Color,
    pub syn_keyword3: Color,
    pub syn_keyword4: Color,
    pub syn_keyword5: Color,
    pub syn_preproc: Color,
    pub syn_operator: Color,
    pub syn_function: Color,
    pub syn_tag: Color,
    pub syn_attribute: Color,
    pub syn_escape: Color,
    pub syn_variable: Color,
}

impl Theme {
    /// Tokyo Night (folke/tokyonight.nvim — `night` variant).
    /// Identity: deep indigo background, soft blue body text, blue accent.
    pub fn tokyo_night() -> Self {
        Self {
            name: "Tokyo Night",
            is_dark: true,
            // Surfaces — bg/bg_dark/bg_highlight from the palette.
            bg: Color::rgb8(0x1a, 0x1b, 0x26),
            bg_chrome: Color::rgb8(0x16, 0x16, 0x1e),
            bg_current_line: Color::rgb8(0x29, 0x2e, 0x42),
            border: Color::rgb8(0x15, 0x16, 0x1e),
            // Foregrounds — fg / fg_dark / fg_gutter.
            fg: Color::rgb8(0xc0, 0xca, 0xf5),
            fg_dim: Color::rgb8(0x79, 0x82, 0xad),
            fg_status: Color::rgb8(0xa9, 0xb1, 0xd6),
            // Accents — blue / green / yellow / red.
            accent: Color::rgb8(0x7a, 0xa2, 0xf7),
            fps_ok: Color::rgb8(0x9e, 0xce, 0x6a),
            warn: Color::rgb8(0xe0, 0xaf, 0x68),
            error: Color::rgb8(0xf7, 0x76, 0x8e),
            // Syntax — purple kw, green str, comment, cyan type, orange num, fg punct.
            syn_keyword: Color::rgb8(0xbb, 0x9a, 0xf7),
            syn_string: Color::rgb8(0x9e, 0xce, 0x6a),
            syn_comment: Color::rgb8(0x8d, 0x95, 0xb8),
            syn_type: Color::rgb8(0x7d, 0xcf, 0xff),
            syn_number: Color::rgb8(0xff, 0x9e, 0x64),
            syn_punct: Color::rgb8(0x89, 0xdd, 0xff),
            syn_keyword2: Color::rgb8(0x7a, 0xa2, 0xf7),
            syn_keyword3: Color::rgb8(0x73, 0xda, 0xca),
            syn_keyword4: Color::rgb8(0xe0, 0xaf, 0x68),
            syn_keyword5: Color::rgb8(0xff, 0x9e, 0x64),
            syn_preproc: Color::rgb8(0x2a, 0xc3, 0xde),
            syn_operator: Color::rgb8(0x89, 0xdd, 0xff),
            syn_function: Color::rgb8(0x7a, 0xa2, 0xf7),
            syn_tag: Color::rgb8(0xf7, 0x76, 0x8e),
            syn_attribute: Color::rgb8(0xe0, 0xaf, 0x68),
            syn_escape: Color::rgb8(0x89, 0xdd, 0xff),
            syn_variable: Color::rgb8(0xc0, 0xca, 0xf5),
        }
    }

    /// Catppuccin Mocha (catppuccin/catppuccin style-guide).
    /// Identity: warm dark base, lavender accent, pastel syntax.
    pub fn catppuccin_mocha() -> Self {
        Self {
            name: "Catppuccin Mocha",
            is_dark: true,
            // Surfaces — base / mantle / surface0 / crust.
            bg: Color::rgb8(0x1e, 0x1e, 0x2e),
            bg_chrome: Color::rgb8(0x18, 0x18, 0x25),
            bg_current_line: Color::rgb8(0x31, 0x32, 0x44),
            border: Color::rgb8(0x11, 0x11, 0x1b),
            // Foregrounds — text / overlay0 / subtext0.
            fg: Color::rgb8(0xcd, 0xd6, 0xf4),
            fg_dim: Color::rgb8(0x83, 0x86, 0x9b),
            fg_status: Color::rgb8(0xa6, 0xad, 0xc8),
            // Accents — lavender accent, green/yellow/red.
            accent: Color::rgb8(0xb4, 0xbe, 0xfe),
            fps_ok: Color::rgb8(0xa6, 0xe3, 0xa1),
            warn: Color::rgb8(0xf9, 0xe2, 0xaf),
            error: Color::rgb8(0xf3, 0x8b, 0xa8),
            // Syntax — mauve / green / overlay2 / yellow / peach / sky.
            syn_keyword: Color::rgb8(0xcb, 0xa6, 0xf7),
            syn_string: Color::rgb8(0xa6, 0xe3, 0xa1),
            syn_comment: Color::rgb8(0x98, 0x9b, 0xac),
            syn_type: Color::rgb8(0xf9, 0xe2, 0xaf),
            syn_number: Color::rgb8(0xfa, 0xb3, 0x87),
            syn_punct: Color::rgb8(0x89, 0xdc, 0xeb),
            syn_keyword2: Color::rgb8(0x89, 0xb4, 0xfa),
            syn_keyword3: Color::rgb8(0x94, 0xe2, 0xd5),
            syn_keyword4: Color::rgb8(0xf5, 0xc2, 0xe7),
            syn_keyword5: Color::rgb8(0xeb, 0xa0, 0xac),
            syn_preproc: Color::rgb8(0xf5, 0xc2, 0xe7),
            syn_operator: Color::rgb8(0x89, 0xdc, 0xeb),
            syn_function: Color::rgb8(0x89, 0xb4, 0xfa),
            syn_tag: Color::rgb8(0xf3, 0x8b, 0xa8),
            syn_attribute: Color::rgb8(0xf9, 0xe2, 0xaf),
            syn_escape: Color::rgb8(0xf2, 0xcd, 0xcd),
            syn_variable: Color::rgb8(0xf2, 0xcd, 0xcd),
        }
    }

    /// Rose Pine Moon (rosepinetheme.com).
    /// Identity: muted plum background, pink/iris accent, watercolor syntax.
    pub fn rose_pine_moon() -> Self {
        Self {
            name: "Rose Pine Moon",
            is_dark: true,
            // Surfaces — base / surface / overlay / highlight-med.
            bg: Color::rgb8(0x23, 0x21, 0x36),
            bg_chrome: Color::rgb8(0x2a, 0x27, 0x3f),
            bg_current_line: Color::rgb8(0x2a, 0x28, 0x3e),
            border: Color::rgb8(0x39, 0x35, 0x52),
            // Foregrounds — text / muted / subtle.
            fg: Color::rgb8(0xe0, 0xde, 0xf4),
            fg_dim: Color::rgb8(0x92, 0x8f, 0xa6),
            fg_status: Color::rgb8(0x91, 0x8d, 0xab),
            // Accents — love (pink) accent, foam (no green), gold, love.
            // Rose Pine has no true green ; foam (teal) stands in for fps_ok.
            accent: Color::rgb8(0xea, 0x9a, 0x97),
            fps_ok: Color::rgb8(0x9c, 0xcf, 0xd8),
            warn: Color::rgb8(0xf6, 0xc1, 0x77),
            error: Color::rgb8(0xeb, 0x6f, 0x92),
            // Syntax — iris kw, gold str, muted comment, foam type, gold num, subtle punct.
            syn_keyword: Color::rgb8(0xc4, 0xa7, 0xe7),
            syn_string: Color::rgb8(0xf6, 0xc1, 0x77),
            syn_comment: Color::rgb8(0xb2, 0xaf, 0xc0),
            syn_type: Color::rgb8(0x9c, 0xcf, 0xd8),
            syn_number: Color::rgb8(0xf6, 0xc1, 0x77),
            syn_punct: Color::rgb8(0xb1, 0xae, 0xc3),
            syn_keyword2: Color::rgb8(0x81, 0xbb, 0xd3),
            syn_keyword3: Color::rgb8(0xeb, 0x9e, 0x9b),
            syn_keyword4: Color::rgb8(0x9c, 0xcf, 0xd8),
            syn_keyword5: Color::rgb8(0xf1, 0x9c, 0xb4),
            syn_preproc: Color::rgb8(0xf1, 0x9c, 0xb4),
            syn_operator: Color::rgb8(0xb5, 0xb2, 0xc6),
            syn_function: Color::rgb8(0xeb, 0x9e, 0x9b),
            syn_tag: Color::rgb8(0x81, 0xbb, 0xd3),
            syn_attribute: Color::rgb8(0xc4, 0xa7, 0xe7),
            syn_escape: Color::rgb8(0x9c, 0xcf, 0xd8),
            syn_variable: Color::rgb8(0xeb, 0x9e, 0x9b),
        }
    }

    /// Gruvbox Dark (morhetz/gruvbox).
    /// Identity: warm sepia background, retro yellow-warm accent, earthy syntax.
    pub fn gruvbox_dark() -> Self {
        Self {
            name: "Gruvbox Dark",
            is_dark: true,
            // Surfaces — bg / bg0_h / bg1 / bg2.
            bg: Color::rgb8(0x28, 0x28, 0x28),
            bg_chrome: Color::rgb8(0x1d, 0x20, 0x21),
            bg_current_line: Color::rgb8(0x3c, 0x38, 0x36),
            border: Color::rgb8(0x50, 0x49, 0x45),
            // Foregrounds — fg / gray / fg4.
            fg: Color::rgb8(0xeb, 0xdb, 0xb2),
            fg_dim: Color::rgb8(0x9a, 0x8c, 0x7e),
            fg_status: Color::rgb8(0xa8, 0x99, 0x84),
            // Accents — yellow accent (Gruvbox identity), green/yellow/red.
            accent: Color::rgb8(0xfa, 0xbd, 0x2f),
            fps_ok: Color::rgb8(0xb8, 0xbb, 0x26),
            warn: Color::rgb8(0xfe, 0x80, 0x19),
            error: Color::rgb8(0xfb, 0x54, 0x41),
            // Syntax — red kw, green str, gray comment, aqua type, purple num, fg punct.
            syn_keyword: Color::rgb8(0xfc, 0x7d, 0x6e),
            syn_string: Color::rgb8(0xb8, 0xbb, 0x26),
            syn_comment: Color::rgb8(0xac, 0xa0, 0x95),
            syn_type: Color::rgb8(0x8e, 0xc0, 0x7c),
            syn_number: Color::rgb8(0xd6, 0x8d, 0xa1),
            syn_punct: Color::rgb8(0xae, 0xa0, 0x8d),
            syn_keyword2: Color::rgb8(0xfa, 0xbd, 0x2f),
            syn_keyword3: Color::rgb8(0x8c, 0xab, 0x9f),
            syn_keyword4: Color::rgb8(0xfe, 0x83, 0x1e),
            syn_keyword5: Color::rgb8(0xd7, 0x91, 0xa4),
            syn_preproc: Color::rgb8(0x8e, 0xc0, 0x7c),
            syn_operator: Color::rgb8(0xfe, 0x83, 0x1e),
            syn_function: Color::rgb8(0xb8, 0xbb, 0x26),
            syn_tag: Color::rgb8(0xfc, 0x7f, 0x70),
            syn_attribute: Color::rgb8(0xfa, 0xbd, 0x2f),
            syn_escape: Color::rgb8(0xfe, 0x83, 0x1e),
            syn_variable: Color::rgb8(0x8c, 0xab, 0x9f),
        }
    }

    /// Solarized Dark (Ethan Schoonover).
    /// Identity: deep teal background, cyan/blue accent, scientific palette.
    pub fn solarized_dark() -> Self {
        Self {
            name: "Solarized Dark",
            is_dark: true,
            // Surfaces — base03 / base02 / base01.
            bg: Color::rgb8(0x00, 0x2b, 0x36),
            bg_chrome: Color::rgb8(0x07, 0x36, 0x42),
            bg_current_line: Color::rgb8(0x07, 0x36, 0x42),
            border: Color::rgb8(0x58, 0x6e, 0x75),
            // Foregrounds — base0 / base01 / base1.
            fg: Color::rgb8(0xaa, 0xb5, 0xb7),
            fg_dim: Color::rgb8(0x85, 0x9c, 0xa3),
            fg_status: Color::rgb8(0x93, 0xa1, 0xa1),
            // Accents — Solarized's signature cyan accent, green/yellow/red.
            accent: Color::rgb8(0x2c, 0xa9, 0xa0),
            fps_ok: Color::rgb8(0x8e, 0xa3, 0x00),
            warn: Color::rgb8(0xc2, 0x93, 0x00),
            error: Color::rgb8(0xe8, 0x77, 0x75),
            // Syntax — green kw (per Solarized spec), cyan str, base01 comment,
            // yellow type, magenta num, base0 punct.
            syn_keyword: Color::rgb8(0x8e, 0xa3, 0x00),
            syn_string: Color::rgb8(0x2c, 0xa9, 0xa0),
            syn_comment: Color::rgb8(0x85, 0x9c, 0xa3),
            syn_type: Color::rgb8(0xc2, 0x93, 0x00),
            syn_number: Color::rgb8(0xe1, 0x75, 0xa9),
            syn_punct: Color::rgb8(0x8b, 0x9b, 0x9d),
            syn_keyword2: Color::rgb8(0x4c, 0xa2, 0xdf),
            syn_keyword3: Color::rgb8(0x94, 0x98, 0xd4),
            syn_keyword4: Color::rgb8(0xec, 0x7b, 0x4c),
            syn_keyword5: Color::rgb8(0xe8, 0x7b, 0x79),
            syn_preproc: Color::rgb8(0xec, 0x7b, 0x4c),
            syn_operator: Color::rgb8(0x93, 0xa1, 0xa1),
            syn_function: Color::rgb8(0x4c, 0xa2, 0xdf),
            syn_tag: Color::rgb8(0xe8, 0x7b, 0x79),
            syn_attribute: Color::rgb8(0xc4, 0x95, 0x00),
            syn_escape: Color::rgb8(0xec, 0x7b, 0x4c),
            syn_variable: Color::rgb8(0x94, 0x98, 0xd4),
        }
    }

    /// GitHub Light (Primer foundations).
    /// Identity: paper-white background, GitHub blue accent, crisp syntax.
    pub fn github_light() -> Self {
        Self {
            name: "GitHub Light",
            is_dark: false,
            // Surfaces — canvas-default / canvas-subtle / canvas-inset / border-default.
            bg: Color::rgb8(0xff, 0xff, 0xff),
            bg_chrome: Color::rgb8(0xf6, 0xf8, 0xfa),
            bg_current_line: Color::rgb8(0xea, 0xee, 0xf2),
            border: Color::rgb8(0xd0, 0xd7, 0xde),
            // Foregrounds — fg-default / fg-subtle / fg-muted.
            fg: Color::rgb8(0x1f, 0x23, 0x28),
            fg_dim: Color::rgb8(0x6a, 0x73, 0x7d),
            fg_status: Color::rgb8(0x65, 0x6d, 0x76),
            // Accents — accent-fg, success-fg, attention-fg, danger-fg.
            accent: Color::rgb8(0x09, 0x69, 0xda),
            fps_ok: Color::rgb8(0x1a, 0x7f, 0x37),
            warn: Color::rgb8(0x9a, 0x67, 0x00),
            error: Color::rgb8(0xd1, 0x24, 0x2f),
            // Syntax — keyword red, string blue, comment muted, type orange-brown,
            // number deep-blue, punct default fg.
            syn_keyword: Color::rgb8(0xcf, 0x22, 0x2e),
            syn_string: Color::rgb8(0x0a, 0x30, 0x69),
            syn_comment: Color::rgb8(0x63, 0x6c, 0x75),
            syn_type: Color::rgb8(0x95, 0x38, 0x00),
            syn_number: Color::rgb8(0x05, 0x50, 0xae),
            syn_punct: Color::rgb8(0x24, 0x29, 0x2f),
            syn_keyword2: Color::rgb8(0x7c, 0x47, 0xdd),
            syn_keyword3: Color::rgb8(0x05, 0x50, 0xae),
            syn_keyword4: Color::rgb8(0x11, 0x63, 0x29),
            syn_keyword5: Color::rgb8(0x95, 0x38, 0x00),
            syn_preproc: Color::rgb8(0x66, 0x39, 0xba),
            syn_operator: Color::rgb8(0x24, 0x29, 0x2f),
            syn_function: Color::rgb8(0x66, 0x39, 0xba),
            syn_tag: Color::rgb8(0x11, 0x63, 0x29),
            syn_attribute: Color::rgb8(0x95, 0x38, 0x00),
            syn_escape: Color::rgb8(0x05, 0x50, 0xae),
            syn_variable: Color::rgb8(0x05, 0x50, 0xae),
        }
    }
}

/// Holds the full palette set and tracks which one is currently active.
/// Owns the themes by value — they're cheap (no heap allocation).
pub struct ThemeRegistry {
    pub themes: Vec<Theme>,
    pub active: usize,
}

impl Default for ThemeRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl ThemeRegistry {
    /// Build the registry with all 6 built-in themes. First entry — Tokyo
    /// Night — is the default active theme (matches the day-1 PoC's dark
    /// blue palette most closely).
    pub fn new() -> Self {
        Self {
            themes: vec![
                Theme::tokyo_night(),
                Theme::catppuccin_mocha(),
                Theme::rose_pine_moon(),
                Theme::gruvbox_dark(),
                Theme::solarized_dark(),
                Theme::github_light(),
            ],
            active: 0,
        }
    }

    /// Borrow the currently active theme. Cheap — just an indexed `&Vec`
    /// access ; the caller can read slots without cloning.
    pub fn current(&self) -> &Theme {
        &self.themes[self.active]
    }

    /// Cycle forward, wrapping past the last theme.
    pub fn next(&mut self) {
        self.active = (self.active + 1) % self.themes.len();
    }

    /// Cycle backward, wrapping before the first theme.
    pub fn prev(&mut self) {
        self.active = if self.active == 0 {
            self.themes.len() - 1
        } else {
            self.active - 1
        };
    }

    /// Switch to the theme matching `name`. Case-insensitive — users typing
    /// `tokyo night` in a command palette should match `Tokyo Night`.
    /// Returns `true` when the active theme actually changed (so callers
    /// can skip a re-paint on no-op).
    pub fn switch_to(&mut self, name: &str) -> bool {
        let needle = name.trim().to_ascii_lowercase();
        if let Some(idx) = self
            .themes
            .iter()
            .position(|t| t.name.to_ascii_lowercase() == needle)
        {
            if idx != self.active {
                self.active = idx;
                return true;
            }
        }
        false
    }

    /// Lookup a theme by name without mutating active state. Same
    /// case-insensitive contract as `switch_to`.
    pub fn by_name(&self, name: &str) -> Option<&Theme> {
        let needle = name.trim().to_ascii_lowercase();
        self.themes
            .iter()
            .find(|t| t.name.to_ascii_lowercase() == needle)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The registry must ship with exactly the 6 built-in themes ordered
    /// as listed in `new()` — index stability matters for any future
    /// "remember last theme" persistence.
    #[test]
    fn registry_has_six_themes_in_order() {
        let r = ThemeRegistry::new();
        assert_eq!(r.themes.len(), 6);
        assert_eq!(r.themes[0].name, "Tokyo Night");
        assert_eq!(r.themes[1].name, "Catppuccin Mocha");
        assert_eq!(r.themes[2].name, "Rose Pine Moon");
        assert_eq!(r.themes[3].name, "Gruvbox Dark");
        assert_eq!(r.themes[4].name, "Solarized Dark");
        assert_eq!(r.themes[5].name, "GitHub Light");
        assert_eq!(r.active, 0);
    }

    /// `next()` must wrap from the last entry back to index 0 — the
    /// command-palette `Cmd+K Cmd+T`-style cycler relies on it.
    #[test]
    fn next_cycles_and_wraps() {
        let mut r = ThemeRegistry::new();
        for expected in 1..6 {
            r.next();
            assert_eq!(r.active, expected);
        }
        r.next(); // wrap
        assert_eq!(r.active, 0);
    }

    /// `prev()` must wrap from index 0 back to the last entry.
    #[test]
    fn prev_cycles_and_wraps() {
        let mut r = ThemeRegistry::new();
        r.prev(); // 0 -> last
        assert_eq!(r.active, 5);
        r.prev();
        assert_eq!(r.active, 4);
    }

    /// `switch_to` is case-insensitive ; returns `true` only when active
    /// actually changes ; returns `false` for unknown names without
    /// mutating state.
    #[test]
    fn switch_to_is_case_insensitive_and_idempotent() {
        let mut r = ThemeRegistry::new();
        assert!(r.switch_to("Catppuccin Mocha"));
        assert_eq!(r.active, 1);
        // No-op when already active.
        assert!(!r.switch_to("catppuccin mocha"));
        assert_eq!(r.active, 1);
        // Mixed case still matches.
        assert!(r.switch_to("GITHUB LIGHT"));
        assert_eq!(r.themes[r.active].name, "GitHub Light");
        // Unknown name leaves state alone.
        let before = r.active;
        assert!(!r.switch_to("Monokai"));
        assert_eq!(r.active, before);
    }

    /// `by_name` mirrors `switch_to`'s lookup without mutation.
    #[test]
    fn by_name_finds_themes_case_insensitive() {
        let r = ThemeRegistry::new();
        assert!(r.by_name("Tokyo Night").is_some());
        assert!(r.by_name("tokyo night").is_some());
        assert!(r.by_name("  Rose Pine Moon  ").is_some());
        assert!(r.by_name("Dracula").is_none());
    }

    /// Sanity: each theme exposes a distinct accent. If two collide we
    /// either fat-fingered a hex or the palette mapping picked the wrong
    /// slot — both are bugs we want to catch at compile-time of the
    /// test suite, not by eyeballing a screenshot.
    #[test]
    fn all_themes_have_distinct_accents() {
        let r = ThemeRegistry::new();
        let mut accents: Vec<u32> = r
            .themes
            .iter()
            .map(|t| {
                let c = t.accent;
                ((c.r as u32) << 16) | ((c.g as u32) << 8) | (c.b as u32)
            })
            .collect();
        accents.sort_unstable();
        accents.dedup();
        assert_eq!(
            accents.len(),
            r.themes.len(),
            "two themes share an accent — palette mapping bug"
        );
    }

    /// `is_dark` must match the design intent — five dark themes + one
    /// light. Catches a future copy-paste mistake on the flag.
    #[test]
    fn is_dark_flags_are_correct() {
        let r = ThemeRegistry::new();
        let names_dark: Vec<&str> = r
            .themes
            .iter()
            .filter(|t| t.is_dark)
            .map(|t| t.name)
            .collect();
        assert_eq!(names_dark.len(), 5);
        assert!(!r.by_name("GitHub Light").unwrap().is_dark);
        assert!(r.by_name("Tokyo Night").unwrap().is_dark);
        assert!(r.by_name("Catppuccin Mocha").unwrap().is_dark);
        assert!(r.by_name("Rose Pine Moon").unwrap().is_dark);
        assert!(r.by_name("Gruvbox Dark").unwrap().is_dark);
        assert!(r.by_name("Solarized Dark").unwrap().is_dark);
    }

    /// Default impl matches `new()` — protects against a future refactor
    /// silently changing the default theme.
    #[test]
    fn default_matches_new() {
        let a = ThemeRegistry::default();
        let b = ThemeRegistry::new();
        assert_eq!(a.themes.len(), b.themes.len());
        assert_eq!(a.active, b.active);
        assert_eq!(a.current().name, b.current().name);
    }

    #[test]
    fn all_themes_have_non_empty_names() {
        let r = ThemeRegistry::new();
        for t in &r.themes {
            assert!(!t.name.is_empty());
        }
    }

    #[test]
    fn all_themes_have_non_zero_syntax_colors() {
        let r = ThemeRegistry::new();
        for t in &r.themes {
            // Syntax keyword color should never be fully black (a typo would leave default 0,0,0)
            let kw = t.syn_keyword;
            assert!(kw.r > 0 || kw.g > 0 || kw.b > 0, "Theme '{}' has black syn_keyword", t.name);
        }
    }

    #[test]
    fn current_theme_wraps_around() {
        let r = ThemeRegistry::new();
        let count = r.themes.len();
        let t0 = current_theme(0);
        let t_wrapped = current_theme(count);
        assert_eq!(t0.name, t_wrapped.name, "Index should wrap around theme count");
    }

    #[test]
    fn builtin_themes_are_built_once_and_match_the_registry() {
        let fresh = ThemeRegistry::new();
        let cached = builtin_themes();
        assert_eq!(cached.len(), fresh.themes.len());
        for (a, b) in cached.iter().zip(&fresh.themes) {
            assert_eq!(a.name, b.name);
            assert_eq!(a.bg, b.bg);
        }
        assert!(std::ptr::eq(builtin_themes().as_ptr(), cached.as_ptr()), "same allocation on every call");
    }

    #[test]
    fn current_theme_each_index_valid() {
        let r = ThemeRegistry::new();
        for i in 0..r.themes.len() {
            let t = current_theme(i);
            assert!(!t.name.is_empty());
        }
    }

    #[test]
    fn theme_count_is_six() {
        let r = ThemeRegistry::new();
        assert_eq!(r.themes.len(), 6);
    }

    #[test]
    fn all_themes_have_distinct_bg() {
        let r = ThemeRegistry::new();
        let mut bgs: Vec<u32> = r.themes.iter()
            .map(|t| ((t.bg.r as u32) << 16) | ((t.bg.g as u32) << 8) | (t.bg.b as u32))
            .collect();
        bgs.sort_unstable();
        bgs.dedup();
        assert_eq!(bgs.len(), r.themes.len(), "Two themes share same bg color");
    }
}

/// Helper to look up the active theme by index.
pub fn current_theme(idx: usize) -> Theme {
    let all = builtin_themes();
    all[idx % all.len()].clone()
}

/// The built-in themes, built once. Style closures run on every style pass; building a fresh
/// `ThemeRegistry` there allocated and filled ~4 KB per call, for nothing.
pub fn builtin_themes() -> &'static [Theme] {
    static THEMES: std::sync::OnceLock<Vec<Theme>> = std::sync::OnceLock::new();
    THEMES.get_or_init(|| ThemeRegistry::new().themes)
}

/// Opacity of the accent colour painted behind selected text.
pub const SELECTION_ALPHA: f32 = 0.25;

/// WCAG 2.x relative luminance of an sRGB colour.
fn luminance(c: Color) -> f64 {
    let lin = |v: u8| {
        let v = v as f64 / 255.0;
        if v <= 0.03928 { v / 12.92 } else { ((v + 0.055) / 1.055).powf(2.4) }
    };
    0.2126 * lin(c.r) + 0.7152 * lin(c.g) + 0.0722 * lin(c.b)
}

/// WCAG 2.x contrast ratio (1.0 ..= 21.0) between two opaque colours.
pub fn contrast_ratio(a: Color, b: Color) -> f64 {
    let (la, lb) = (luminance(a), luminance(b));
    let (hi, lo) = if la > lb { (la, lb) } else { (lb, la) };
    (hi + 0.05) / (lo + 0.05)
}

/// Appearance the user asked for (`theme.mode` in settings.toml).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ThemeMode {
    #[default]
    Dark,
    Light,
    /// Follow the operating system's "apps use light/dark theme" switch.
    System,
}

/// Theme shown first in dark / light appearance when the user named none.
pub const DEFAULT_DARK_THEME: &str = "Tokyo Night";
pub const DEFAULT_LIGHT_THEME: &str = "GitHub Light";

impl ThemeMode {
    /// `dark` / `light` / `system`, case-insensitive; anything else is `None`.
    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "dark" => Some(Self::Dark),
            "light" => Some(Self::Light),
            "system" => Some(Self::System),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Dark => "dark",
            Self::Light => "light",
            Self::System => "system",
        }
    }

    /// Does this mode resolve to a dark appearance? `system_light` is the OS
    /// preference (`None` when it cannot be read: dark, the historical default).
    pub fn resolves_dark(self, system_light: Option<bool>) -> bool {
        match self {
            Self::Dark => true,
            Self::Light => false,
            Self::System => !system_light.unwrap_or(false),
        }
    }
}

impl ThemeRegistry {
    /// Index of the theme to show for the wanted appearance: `preferred` (by
    /// name) when it exists and has the right kind, else the default dark /
    /// light theme.
    pub fn pick(&self, want_dark: bool, preferred: Option<&str>) -> usize {
        let fits = |t: &Theme| t.is_dark == want_dark;
        if let Some(i) = preferred.and_then(|n| self.themes.iter().position(|t| t.name.eq_ignore_ascii_case(n.trim()) && fits(t))) {
            return i;
        }
        let default = if want_dark { DEFAULT_DARK_THEME } else { DEFAULT_LIGHT_THEME };
        self.themes.iter().position(|t| t.name == default).unwrap_or(0)
    }
}

/// WCAG 2.x contrast checks for the table-driven syntax slots.
#[cfg(test)]
mod syntax_contrast_tests {
    use super::*;

    use super::contrast_ratio as contrast;

    fn new_slots(t: &Theme) -> [(&'static str, Color); 11] {
        [
            ("syn_keyword2", t.syn_keyword2),
            ("syn_keyword3", t.syn_keyword3),
            ("syn_keyword4", t.syn_keyword4),
            ("syn_keyword5", t.syn_keyword5),
            ("syn_preproc", t.syn_preproc),
            ("syn_operator", t.syn_operator),
            ("syn_function", t.syn_function),
            ("syn_tag", t.syn_tag),
            ("syn_attribute", t.syn_attribute),
            ("syn_escape", t.syn_escape),
            ("syn_variable", t.syn_variable),
        ]
    }

    /// Every new syntax colour reaches WCAG AA (4.5:1) on both the editor
    /// background and the current-line highlight, in dark and light themes.
    #[test]
    fn new_syntax_slots_meet_wcag_aa() {
        let r = ThemeRegistry::new();
        assert!(r.themes.iter().any(|t| t.is_dark) && r.themes.iter().any(|t| !t.is_dark));
        for t in &r.themes {
            for (name, c) in new_slots(t) {
                for (what, bg) in [("bg", t.bg), ("current line", t.bg_current_line)] {
                    let ratio = contrast(c, bg);
                    assert!(ratio >= 4.5, "{} {} on {}: {:.2}:1 < 4.5:1", t.name, name, what, ratio);
                }
            }
        }
    }

    /// Within one theme the keyword groups must be told apart.
    #[test]
    fn keyword_group_colors_are_distinct_per_theme() {
        let r = ThemeRegistry::new();
        for t in &r.themes {
            let ks = [t.syn_keyword, t.syn_keyword2, t.syn_keyword3, t.syn_keyword4, t.syn_keyword5];
            for i in 0..ks.len() {
                for j in (i + 1)..ks.len() {
                    let (a, b) = (ks[i], ks[j]);
                    assert!((a.r, a.g, a.b) != (b.r, b.g, b.b), "{}: keyword groups {} and {} share a colour", t.name, i + 1, j + 1);
                }
            }
        }
    }
}

/// Dark / light modes: resolution rules and WCAG AA on the key colour pairs of every theme.
#[cfg(test)]
mod mode_and_surface_tests {
    use super::*;

    #[test]
    fn mode_parses_and_round_trips() {
        for m in [ThemeMode::Dark, ThemeMode::Light, ThemeMode::System] {
            assert_eq!(ThemeMode::parse(m.as_str()), Some(m));
        }
        assert_eq!(ThemeMode::parse("  LIGHT "), Some(ThemeMode::Light));
        assert_eq!(ThemeMode::parse("auto"), None);
        assert_eq!(ThemeMode::parse(""), None);
        assert_eq!(ThemeMode::default(), ThemeMode::Dark);
    }

    #[test]
    fn mode_resolution_follows_the_system_only_in_system_mode() {
        assert!(ThemeMode::Dark.resolves_dark(Some(true)));
        assert!(!ThemeMode::Light.resolves_dark(Some(false)));
        assert!(!ThemeMode::System.resolves_dark(Some(true)), "OS prefers light -> light");
        assert!(ThemeMode::System.resolves_dark(Some(false)), "OS prefers dark -> dark");
        assert!(ThemeMode::System.resolves_dark(None), "unreadable OS value -> dark");
    }

    #[test]
    fn pick_honours_the_preferred_theme_only_when_the_kind_fits() {
        let r = ThemeRegistry::new();
        let name = |i: usize| r.themes[i].name;
        assert_eq!(name(r.pick(true, None)), DEFAULT_DARK_THEME);
        assert_eq!(name(r.pick(false, None)), DEFAULT_LIGHT_THEME);
        assert_eq!(name(r.pick(true, Some("gruvbox dark"))), "Gruvbox Dark");
        assert_eq!(name(r.pick(false, Some("Gruvbox Dark"))), DEFAULT_LIGHT_THEME, "a dark theme is never picked for light");
        assert_eq!(name(r.pick(true, Some("nope"))), DEFAULT_DARK_THEME);
        assert!(!r.themes[r.pick(false, None)].is_dark && r.themes[r.pick(true, None)].is_dark);
    }

    #[test]
    fn light_theme_has_a_pure_white_page() {
        let t = ThemeRegistry::new().themes.into_iter().find(|t| !t.is_dark).unwrap();
        assert_eq!((t.bg.r, t.bg.g, t.bg.b), (255, 255, 255));
    }

    /// Selection background as painted: accent at 25 % over the editor background.
    fn selection_bg(t: &Theme) -> Color {
        let a = SELECTION_ALPHA as f64;
        let mix = |f: u8, b: u8| (f as f64 * a + b as f64 * (1.0 - a)).round() as u8;
        Color::rgb8(mix(t.accent.r, t.bg.r), mix(t.accent.g, t.bg.g), mix(t.accent.b, t.bg.b))
    }

    /// Key pairs, WCAG 2.x AA: 4.5:1 for text, 3:1 for interface elements
    /// (focus ring / active-tab underline = accent, input borders and scrollbar
    /// handles = fg_dim). `border` is a decorative separator and is exempt.
    #[test]
    fn every_theme_meets_wcag_aa_on_its_key_pairs() {
        let mut bad = Vec::new();
        for t in &ThemeRegistry::new().themes {
            let sel = selection_bg(t);
            let mut check = |what: &str, a: Color, b: Color, min: f64| {
                let r = contrast_ratio(a, b);
                if r < min {
                    bad.push(format!("{} {what}: {r:.2}:1 < {min}:1", t.name));
                }
            };
            for (n, bg) in [("bg", t.bg), ("chrome", t.bg_chrome), ("line", t.bg_current_line)] {
                check(&format!("fg on {n}"), t.fg, bg, 4.5);
            }
            check("fg on selection", t.fg, sel, 4.5);
            for (n, bg) in [("bg", t.bg), ("chrome", t.bg_chrome)] {
                check(&format!("fg_dim on {n}"), t.fg_dim, bg, 4.5);
                check(&format!("accent on {n}"), t.accent, bg, 4.5);
            }
            check("fg_status on chrome", t.fg_status, t.bg_chrome, 4.5);
            for (n, c) in [("fps_ok", t.fps_ok), ("warn", t.warn), ("error", t.error)] {
                check(&format!("{n} on chrome"), c, t.bg_chrome, 4.5);
                check(&format!("{n} on bg"), c, t.bg, 4.5);
            }
            let syn = [
                ("keyword", t.syn_keyword), ("string", t.syn_string), ("comment", t.syn_comment), ("type", t.syn_type),
                ("number", t.syn_number), ("punct", t.syn_punct), ("keyword2", t.syn_keyword2), ("keyword3", t.syn_keyword3),
                ("keyword4", t.syn_keyword4), ("keyword5", t.syn_keyword5), ("preproc", t.syn_preproc), ("operator", t.syn_operator),
                ("function", t.syn_function), ("tag", t.syn_tag), ("attribute", t.syn_attribute), ("escape", t.syn_escape),
                ("variable", t.syn_variable),
            ];
            for (n, c) in syn {
                check(&format!("syn_{n} on bg"), c, t.bg, 4.5);
                check(&format!("syn_{n} on line"), c, t.bg_current_line, 4.5);
                // Selected syntax-coloured text: transient, 3:1 (UI-element level).
                check(&format!("syn_{n} on selection"), c, sel, 3.0);
            }
            // Interface elements, 3:1.
            for (n, bg) in [("bg", t.bg), ("chrome", t.bg_chrome)] {
                check(&format!("ui accent on {n}"), t.accent, bg, 3.0);
                check(&format!("ui fg_dim on {n}"), t.fg_dim, bg, 3.0);
            }
        }
        assert!(bad.is_empty(), "{} failing pairs:\n{}", bad.len(), bad.join("\n"));
    }

    /// The chrome (tab bar, menus, panels) must differ from the editor page,
    /// and each mode must really be dark or light.
    #[test]
    fn chrome_differs_from_page_and_modes_are_really_dark_or_light() {
        for t in &ThemeRegistry::new().themes {
            assert!(contrast_ratio(t.bg_chrome, t.bg) > 1.02, "{}: chrome indistinguishable from the page", t.name);
            let l = luminance(t.bg);
            assert_eq!(t.is_dark, l < 0.2, "{}: is_dark flag disagrees with the background luminance {l:.3}", t.name);
            assert_eq!(t.is_dark, luminance(t.bg_chrome) < 0.2, "{}: chrome is on the wrong side", t.name);
        }
    }
}
