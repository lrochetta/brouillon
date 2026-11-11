//! The one reactive source of the active theme.
//!
//! Every surface reads its colours through [`active_theme`] (or the index
//! signal it wraps), so switching dark / light repaints the whole window and no
//! view keeps a private palette. The signal lives on the UI thread; the host
//! (main.rs) takes it with [`theme_idx_signal`] and hands it to the views that
//! still receive the index as a parameter.

use std::cell::Cell;

use brouillon_core::theme::{current_theme, Theme};
use floem::reactive::{create_rw_signal, RwSignal, SignalGet};
use floem::style::Style;
use floem::views::scroll::{Handle, ScrollClass, Track};
use floem::peniko::Brush;
use floem::views::{PlaceholderTextClass, TextInputClass};

thread_local! {
    static THEME_IDX: Cell<Option<RwSignal<usize>>> = const { Cell::new(None) };
}

/// Index (into `ThemeRegistry::themes`) of the active theme. Created on first use.
pub fn theme_idx_signal() -> RwSignal<usize> {
    THEME_IDX.with(|c| match c.get() {
        Some(s) => s,
        None => {
            let s = create_rw_signal(0);
            c.set(Some(s));
            s
        }
    })
}

/// The active theme. Subscribes the calling reactive scope (style closure,
/// effect) to theme changes.
pub fn active_theme() -> Theme {
    current_theme(theme_idx_signal().get())
}

/// Styles inherited by every descendant of the window root and not covered by
/// the plain colour slots: scroll bar handle / track, which floem defaults to
/// translucent black (invisible on a dark page).
pub fn inherited(s: Style, t: &Theme) -> Style {
    let (handle, fg, dim) = (t.fg_dim, t.fg, t.fg_dim);
    s.class(ScrollClass, move |s| {
        s.class(Handle, move |s| s.background(handle).border_radius(4.0))
            .class(Track, |s| s.background(floem::peniko::Color::TRANSPARENT))
    })
    // Text fields: caret/selection and placeholder default to black.
    .class(TextInputClass, move |s| {
        s.cursor_color(Brush::Solid(fg.multiply_alpha(0.6))).class(PlaceholderTextClass, move |s| s.color(dim))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Surfaces that take their colours from the theme must not grow literal
    /// colours again (the old tab bar / find panel / palette ignored the mode).
    /// menu_dropdown.rs is out: it derives its popup colours (computed and
    /// contrast-tested there) from the theme.
    #[test]
    fn themed_surfaces_have_no_magic_colours() {
        let files = [
            ("tab_bar.rs", include_str!("tab_bar.rs")),
            ("find_replace.rs", include_str!("find_replace.rs")),
            ("palette_view.rs", include_str!("palette_view.rs")),
            ("render_badge.rs", include_str!("render_badge.rs")),
        ];
        let re = regex::Regex::new(r"Color::(rgb|rgba|from|BLACK|WHITE|GRAY|RED|GREEN|BLUE)|0x[0-9a-fA-F]{6}\b").unwrap();
        for (name, src) in files {
            // Only the code above the tests module counts; comments are skipped.
            let code = src.split("#[cfg(test)]").next().unwrap();
            let code = code.lines().filter(|l| !l.trim_start().starts_with("//")).collect::<Vec<_>>().join("\n");
            assert!(!re.is_match(&code), "{name} contains a literal colour: {:?}", re.find(&code).map(|m| m.as_str()));
        }
    }

    #[test]
    fn inherited_style_builds_for_every_theme() {
        for t in &brouillon_core::theme::ThemeRegistry::new().themes {
            let _ = inherited(Style::new(), t);
        }
    }
}
