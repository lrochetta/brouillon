//! Editor preferences read from the settings (`[editor]`, `[files]`), including the ones an
//! EditPlus import fills in. Pure: the host reads them once and again after a settings change.
//!
//! Used for: the Tab key, the indent unit of the smart Enter, whether Enter keeps the indent,
//! and the encoding of a new document. `font_family`, `font_size`, `word_wrap` and `wrap_column`
//! are read by the editor view itself (`EditorOpts::apply_settings`); [`IGNORED_KEYS`] lists the
//! imported keys that nothing reads (none now).

use crate::settings::Settings;

/// Settings an import may write that the editor does not read yet.
pub const IGNORED_KEYS: &[&str] = &[];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EditorPrefs {
    /// Width of a tab stop, used by the Tab key when it inserts spaces.
    pub tab_size: usize,
    /// Width of one indent level (smart Enter after `{`, `:`...).
    pub indent_size: usize,
    /// Tab key and indent level use spaces (true) or a tab character (false).
    pub insert_spaces: bool,
    /// Enter copies the indent of the current line.
    pub auto_indent: bool,
    /// Codepage id of a new document ("" = UTF-8).
    pub default_encoding: String,
}

impl Default for EditorPrefs {
    fn default() -> Self {
        Self { tab_size: 4, indent_size: 4, insert_spaces: true, auto_indent: true, default_encoding: String::new() }
    }
}

impl EditorPrefs {
    pub fn from_settings(s: &Settings) -> Self {
        let d = Self::default();
        let size = |key: &str, dv: usize| s.get_i64(key, dv as i64).clamp(1, 16) as usize;
        let enc = s.get_str("files.default_encoding", "");
        Self {
            tab_size: size("editor.tab_size", d.tab_size),
            indent_size: size("editor.indent_size", d.indent_size),
            insert_spaces: s.get_bool("editor.insert_spaces", d.insert_spaces),
            auto_indent: s.get_bool("editor.auto_indent", d.auto_indent),
            default_encoding: crate::codepages::supported(&enc).map(|c| c.id.to_string()).unwrap_or_default(),
        }
    }

    /// One indent level as text.
    pub fn indent_unit(&self) -> String {
        if self.insert_spaces {
            " ".repeat(self.indent_size)
        } else {
            "\t".to_string()
        }
    }

    /// What the Tab key inserts when the caret follows `before` on its line.
    pub fn tab_text(&self, before: &str) -> String {
        if !self.insert_spaces {
            return "\t".to_string();
        }
        let col = before.chars().count();
        " ".repeat(self.tab_size - col % self.tab_size)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::settings::Layer;

    fn prefs(text: &str) -> EditorPrefs {
        EditorPrefs::from_settings(&Settings::build(&[(Layer::User, text)]))
    }

    #[test]
    fn defaults_without_settings() {
        assert_eq!(prefs(""), EditorPrefs::default());
    }

    #[test]
    fn imported_values_are_read() {
        let p = prefs("[editor]\ntab_size = 3\nindent_size = 2\ninsert_spaces = false\nauto_indent = false\n[files]\ndefault_encoding = \"windows-1252\"\n");
        assert_eq!((p.tab_size, p.indent_size, p.insert_spaces, p.auto_indent), (3, 2, false, false));
        assert_eq!(p.default_encoding, "windows-1252");
        assert_eq!(p.indent_unit(), "\t");
    }

    #[test]
    fn unknown_encoding_and_bad_sizes_fall_back() {
        let p = prefs("[editor]\ntab_size = 0\nindent_size = 500\n[files]\ndefault_encoding = \"klingon\"\n");
        assert_eq!((p.tab_size, p.indent_size), (1, 16));
        assert_eq!(p.default_encoding, "");
    }

    #[test]
    fn tab_key_reaches_the_next_stop() {
        let p = prefs("[editor]\ntab_size = 4\n");
        assert_eq!(p.tab_text(""), "    ");
        assert_eq!(p.tab_text("ab"), "  ");
        assert_eq!(p.tab_text("abcd"), "    ");
        assert_eq!(prefs("[editor]\ninsert_spaces = false\n").tab_text("ab"), "\t");
        assert_eq!(prefs("[editor]\nindent_size = 2\n").indent_unit(), "  ");
    }
}
