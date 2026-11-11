#![allow(dead_code)]

//! Bundled font set. Goal: avoid `fontdb`'s full-system font enumeration
//! (~50 MB tax on Windows — measured in the early proof of concept) by shipping a curated subset.
//!
//! Embedding strategy: `include_bytes!` so the fonts ship inside the
//! release binary. Trade-off: +X MB binary (per-font), but predictable
//! cross-OS rendering and no system-font RAM tax. Budget allows up to
//! 30 MB binary ; currently 9.77 MB so there's ~20 MB headroom.
//!
//! ---
//!
//! # Floem 0.2.0 integration feasibility — outcome (C), partial bypass
//!
//! Researched 2026-05-19 against floem 0.2.0 tag. Findings:
//!
//! - `floem::text::FONT_SYSTEM` IS public and re-exports
//!   `floem_renderer::text::FONT_SYSTEM`, defined at
//!   `renderer/src/text/layout.rs:11` as
//!   `pub static FONT_SYSTEM: LazyLock<Mutex<FontSystem>>`. The initializer
//!   hard-codes `FontSystem::new()` (which scans system fonts via fontdb)
//!   plus a platform-specific `set_sans_serif_family` call. There is NO
//!   public hook to swap the FontSystem itself before first access.
//!
//! - However, because the lock is public, we CAN call
//!   `FONT_SYSTEM.lock().db_mut().load_font_data(bytes.to_vec())` at
//!   startup, BEFORE the first paint — `LazyLock` triggers initialization
//!   on that first `.lock()` call, then we mutate the inner database to
//!   add our bundled fonts. The bundled families become resolvable by
//!   `font_family("JetBrains Mono")`.
//!
//! - **What this DOES NOT do**: it does not prevent `FontSystem::new()`
//!   from running fontdb's full system-font enumeration (the ~50 MB RAM
//!   tax in measured in the early proof of concept). That enumeration is baked into the
//!   `LazyLock` initializer. **Closing the 50 MB gap requires an upstream
//!   patch to Floem** — specifically `renderer/src/text/layout.rs:11`
//!   needs to expose a `set_font_system(FontSystem)` API (or a
//!   `FONT_SYSTEM_INIT: OnceLock<FontSystem>` we can pre-populate), so
//!   downstream apps can substitute a `FontSystem::new_with_locale_and_db`
//!   built from a Database that has NEVER been touched by
//!   `load_system_fonts()`.
//!
//! - **Practical M1 plan**:
//!   1. *This round* — ship this module + the empty bundled list. View
//!      layer wires `fonts::install_into_floem()` at startup, calling
//!      `FONT_SYSTEM.lock().db_mut().load_font_data(...)` for each entry.
//!      Net RAM impact: 0 today (no fonts bundled yet), but the data
//!      pipeline is ready.
//!   2. *Once real fonts are bundled* — predictable rendering, but the
//!      ~50 MB fontdb tax remains.
//!   3. *To close the 50 MB gap* — open an issue / PR on
//!      `lapce/floem` proposing a `set_font_system` hook. Until that
//!      lands, we can either (a) live with the tax or (b) fork Floem and
//!      patch the line locally. **TODO(m1-upstream)**: file the Floem
//!      issue with this exact line reference: `renderer/src/text/layout.rs:11`.
//!
//! - Alternative cosmic-text constructors we'd want exposed:
//!   - `FontSystem::new_with_locale_and_db(locale, db)` — feeds a
//!     pre-populated, system-fonts-NOT-loaded Database.
//!   - `FontSystem::new_with_fonts(sources)` — same idea, iterator API.
//!     See `https://docs.rs/cosmic-text/latest/cosmic_text/struct.FontSystem.html`.
//!
//! Wider context for the view layer: do NOT call this module's installer
//! more than once. After `LazyLock` resolves, the database accepts
//! additions but the FontSystem cache may need eviction — out of scope
//! for the PoC, but a real concern if we add a "reload fonts" feature.

pub struct BundledFont {
    pub family: &'static str,
    pub style: FontStyle,
    pub weight: u16, // 400 = regular, 700 = bold
    pub data: &'static [u8],
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FontStyle {
    Normal,
    Italic,
    Oblique,
}

/// Returns the bundled fonts list. The view layer iterates this and feeds
/// them into the cosmic-text FontSystem at startup (see top doc-comment for
/// the Floem 0.2.0 integration recipe).
pub fn bundled_fonts() -> &'static [BundledFont] {
    BUNDLED
}

/// Placeholder byte slice. Real `include_bytes!` calls will replace this
/// once the licensing review (Apache-2 / OFL only) lands. See
/// `assets/fonts/README.md` for the candidate set and rollout plan.
const PLACEHOLDER: &[u8] = &[];

static BUNDLED: &[BundledFont] = &[
    // TODO(m1): replace `PLACEHOLDER` with `include_bytes!("../assets/fonts/JetBrainsMono-Regular.ttf")`.
    //          Family: JetBrains Mono (Apache 2). Source:
    //          https://github.com/JetBrains/JetBrainsMono/releases — pick a tagged release.
    BundledFont {
        family: "JetBrains Mono",
        style: FontStyle::Normal,
        weight: 400,
        data: PLACEHOLDER,
    },
    // TODO(m1): replace with `include_bytes!("../assets/fonts/JetBrainsMono-Bold.ttf")`.
    BundledFont {
        family: "JetBrains Mono",
        style: FontStyle::Normal,
        weight: 700,
        data: PLACEHOLDER,
    },
    // TODO(m1): replace with `include_bytes!("../assets/fonts/JetBrainsMono-Italic.ttf")`.
    BundledFont {
        family: "JetBrains Mono",
        style: FontStyle::Italic,
        weight: 400,
        data: PLACEHOLDER,
    },
    // TODO(m1): replace with `include_bytes!("../assets/fonts/Inter-Regular.otf")`.
    //          Family: Inter (OFL). Source: https://rsms.me/inter/
    BundledFont {
        family: "Inter",
        style: FontStyle::Normal,
        weight: 400,
        data: PLACEHOLDER,
    },
    // TODO(m1): replace with `include_bytes!("../assets/fonts/Inter-Medium.otf")`.
    BundledFont {
        family: "Inter",
        style: FontStyle::Normal,
        weight: 500,
        data: PLACEHOLDER,
    },
];

#[cfg(test)]
mod tests {
    use super::*;

    /// Pin the public contract: callers can iterate the list, read each
    /// field, and the slice length matches the static declaration above.
    /// When real fonts are bundled this test gains a `!data.is_empty()`
    /// assertion ; today every entry is a placeholder and an empty slice
    /// is the expected state.
    #[test]
    fn bundled_fonts_listing_is_well_formed() {
        let list = bundled_fonts();
        assert_eq!(list.len(), 5, "expected 5 placeholder entries");
        for f in list {
            assert!(!f.family.is_empty(), "family name must not be empty");
            assert!(
                f.weight == 400 || f.weight == 500 || f.weight == 700,
                "weight {} not in expected set",
                f.weight
            );
            // Placeholder phase: data IS empty. Flip this assertion once
            // real font bytes land (see TODO(m1) markers above).
            assert!(
                f.data.is_empty(),
                "{} {}: still expected to be a placeholder",
                f.family,
                f.weight
            );
        }
    }

    /// FontStyle is small and Copy ; this test pins the variant set so a
    /// future `Add` doesn't silently break match exhaustiveness elsewhere.
    #[test]
    fn font_style_variants_are_stable() {
        let _ = FontStyle::Normal;
        let _ = FontStyle::Italic;
        let _ = FontStyle::Oblique;
        assert_ne!(FontStyle::Normal, FontStyle::Italic);
    }

    #[test]
    fn jetbrains_mono_has_three_variants() {
        let jb_count = bundled_fonts().iter().filter(|f| f.family == "JetBrains Mono").count();
        assert_eq!(jb_count, 3, "JetBrains Mono: Regular, Bold, Italic");
    }

    #[test]
    fn inter_has_two_variants() {
        let inter_count = bundled_fonts().iter().filter(|f| f.family == "Inter").count();
        assert_eq!(inter_count, 2, "Inter: Regular, Medium");
    }

    #[test]
    fn all_weights_in_valid_range() {
        for f in bundled_fonts() {
            assert!(f.weight >= 100 && f.weight <= 900, "Weight {} out of range for {}", f.weight, f.family);
        }
    }

    #[test]
    fn italic_entry_exists() {
        let has_italic = bundled_fonts().iter().any(|f| f.style == FontStyle::Italic);
        assert!(has_italic, "At least one italic font should be bundled");
    }

    #[test]
    fn families_are_known_set() {
        let known = ["JetBrains Mono", "Inter"];
        for f in bundled_fonts() {
            assert!(known.contains(&f.family), "Unknown font family: {}", f.family);
        }
    }

    #[test]
    fn bundled_fonts_is_deterministic() {
        let a = bundled_fonts();
        let b = bundled_fonts();
        assert_eq!(a.len(), b.len());
        assert_eq!(a[0].family, b[0].family);
    }
}
