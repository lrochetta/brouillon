//! Multi-language grammar registry for tree-sitter syntax highlighting.
//!
//! Extends the editor beyond Rust to support multiple programming languages.
//! Each language maps file extensions to a tree-sitter grammar + highlights
//! query. The registry is queried at file-open time to select the right
//! parser for syntax highlighting.
//!
//! M2 scope: Rust (already working), JavaScript, TypeScript, Python, JSON,
//! TOML, Markdown. Additional languages can be added by extending the
//! `LanguageId` enum and the `LANGUAGES` table.

use std::path::Path;

/// Supported language identifier.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum LanguageId {
    Rust,
    JavaScript,
    TypeScript,
    Python,
    Html,
    Css,
    Json,
    Toml,
    Markdown,
    PlainText,
    /// Any other language from the table-driven registry (`syntax` module);
    /// the payload is its registry index.
    Defined(u16),
}

impl LanguageId {
    /// Human-readable display name for the status bar.
    pub fn display_name(self) -> &'static str {
        match self {
            Self::Rust => "Rust",
            Self::JavaScript => "JavaScript",
            Self::TypeScript => "TypeScript",
            Self::Python => "Python",
            Self::Html => "HTML",
            Self::Css => "CSS",
            Self::Json => "JSON",
            Self::Toml => "TOML",
            Self::Markdown => "Markdown",
            Self::PlainText => "Plain Text",
            Self::Defined(i) => {
                let reg = crate::syntax::registry();
                if (i as usize) < reg.len() {
                    reg.def(i).name
                } else {
                    "Plain Text"
                }
            }
        }
    }
}

/// Language definition table entry.
struct LangDef {
    id: LanguageId,
    extensions: &'static [&'static str],
}

/// Static language definition table. Order doesn't matter — we search all
/// entries on each lookup (< 20 entries, so linear scan is fine).
const LANGUAGES: &[LangDef] = &[
    LangDef {
        id: LanguageId::Rust,
        extensions: &["rs"],
    },
    LangDef {
        id: LanguageId::JavaScript,
        extensions: &["js", "jsx", "mjs", "cjs"],
    },
    LangDef {
        id: LanguageId::TypeScript,
        extensions: &["ts", "tsx", "mts", "cts"],
    },
    LangDef {
        id: LanguageId::Python,
        extensions: &["py", "pyi", "pyw"],
    },
    LangDef {
        id: LanguageId::Html,
        extensions: &["html", "htm", "xhtml"],
    },
    LangDef {
        id: LanguageId::Css,
        extensions: &["css"],
    },
    LangDef {
        id: LanguageId::Json,
        extensions: &["json", "jsonc", "geojson"],
    },
    LangDef {
        id: LanguageId::Toml,
        extensions: &["toml"],
    },
    LangDef {
        id: LanguageId::Markdown,
        extensions: &["md", "markdown", "mdx"],
    },
];

/// Detect the language from a file path (file name, then extension).
///
/// Returns `LanguageId::PlainText` if nothing matches.
pub fn detect_language(path: &Path) -> LanguageId {
    detect_language_with_first_line(path, None)
}

/// Like [`detect_language`], but when the name and extension do not decide,
/// the first line of the file (shebang, `<?xml`, `<?php`, ...) is consulted.
pub fn detect_language_with_first_line(path: &Path, first_line: Option<&str>) -> LanguageId {
    let reg = crate::syntax::registry();
    if let Some(i) = reg.detect(path, first_line) {
        return from_definition(&reg, i);
    }

    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_lowercase());

    let ext = match ext {
        Some(e) => e,
        None => return LanguageId::PlainText,
    };

    for lang in LANGUAGES {
        if lang.extensions.iter().any(|e| *e == ext) {
            return lang.id;
        }
    }

    LanguageId::PlainText
}

/// [`detect_language_with_first_line`] fed from the document text itself
/// (first line, capped at 256 bytes on a char boundary).
pub fn detect_language_for_text(path: &Path, text: &str) -> LanguageId {
    let mut end = text.find('\n').unwrap_or(text.len()).min(256);
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    let line = text[..end].trim_end_matches('\r');
    detect_language_with_first_line(path, Some(line))
}

/// Registry definition -> id. The nine historical languages keep their enum
/// variant so existing matches and tests keep working.
fn from_definition(reg: &crate::syntax::Registry, i: u16) -> LanguageId {
    let name = reg.def(i).name;
    for lang in LANGUAGES {
        if lang.id.display_name().eq_ignore_ascii_case(name) {
            return lang.id;
        }
    }
    LanguageId::Defined(i)
}

/// Registry index of the table-driven definition for `lang` (None for Rust,
/// which uses tree-sitter, and for plain text).
pub fn syntax_index(lang: LanguageId) -> Option<u16> {
    let reg = crate::syntax::registry();
    match lang {
        LanguageId::Rust | LanguageId::PlainText => None,
        LanguageId::Defined(i) => ((i as usize) < reg.len()).then_some(i),
        other => reg.by_name(other.display_name()),
    }
}

/// Detect language from a filename string (convenience wrapper).
pub fn detect_language_from_name(filename: &str) -> LanguageId {
    detect_language(Path::new(filename))
}

/// Returns `true` if the language has tree-sitter grammar support.
///
/// Currently only Rust has a compiled grammar. Other languages will be
/// enabled as their tree-sitter crates are added to Cargo.toml.
pub fn has_grammar(lang: LanguageId) -> bool {
    matches!(lang, LanguageId::Rust)
}

/// Get all supported file extensions for a language.
pub fn extensions_for(lang: LanguageId) -> &'static [&'static str] {
    for def in LANGUAGES {
        if def.id == lang {
            return def.extensions;
        }
    }
    &[]
}

/// Get all registered language IDs (excluding PlainText).
pub fn all_languages() -> Vec<LanguageId> {
    LANGUAGES.iter().map(|d| d.id).collect()
}

#[cfg(test)]
mod tests {
    #[test]
    fn first_line_decides_for_extensionless_files() {
        let p = Path::new("script");
        assert_eq!(detect_language_for_text(p, "#!/usr/bin/env python3\nprint(1)\n"), LanguageId::Python);
        assert_eq!(detect_language_for_text(p, "#!/bin/bash\r\necho hi\n").display_name(), "Shell");
        assert_eq!(detect_language_for_text(p, "plain words\n"), LanguageId::PlainText);
        assert_eq!(detect_language_for_text(Path::new("a.py"), ""), LanguageId::Python);
    }

    use super::*;

    #[test]
    fn detect_rust_by_extension() {
        assert_eq!(detect_language(Path::new("main.rs")), LanguageId::Rust);
        assert_eq!(detect_language(Path::new("/path/to/lib.rs")), LanguageId::Rust);
    }

    #[test]
    fn detect_javascript_variants() {
        assert_eq!(detect_language(Path::new("app.js")), LanguageId::JavaScript);
        assert_eq!(detect_language(Path::new("component.jsx")), LanguageId::JavaScript);
        assert_eq!(detect_language(Path::new("module.mjs")), LanguageId::JavaScript);
        assert_eq!(detect_language(Path::new("config.cjs")), LanguageId::JavaScript);
    }

    #[test]
    fn detect_typescript_variants() {
        assert_eq!(detect_language(Path::new("app.ts")), LanguageId::TypeScript);
        assert_eq!(detect_language(Path::new("page.tsx")), LanguageId::TypeScript);
    }

    #[test]
    fn detect_python() {
        assert_eq!(detect_language(Path::new("script.py")), LanguageId::Python);
        assert_eq!(detect_language(Path::new("stubs.pyi")), LanguageId::Python);
    }

    #[test]
    fn detect_json_variants() {
        assert_eq!(detect_language(Path::new("package.json")), LanguageId::Json);
        assert_eq!(detect_language(Path::new("settings.jsonc")), LanguageId::Json);
    }

    #[test]
    fn detect_toml() {
        assert_eq!(detect_language(Path::new("Cargo.toml")), LanguageId::Toml);
    }

    #[test]
    fn detect_markdown() {
        assert_eq!(detect_language(Path::new("README.md")), LanguageId::Markdown);
        assert_eq!(detect_language(Path::new("doc.markdown")), LanguageId::Markdown);
    }

    #[test]
    fn detect_plain_text_for_unknown() {
        assert_eq!(detect_language(Path::new("data.csv")), LanguageId::PlainText);
        assert_eq!(detect_language(Path::new("notes.xyz")), LanguageId::PlainText);
        assert_eq!(detect_language(Path::new(".gitignore")), LanguageId::PlainText);
    }

    #[test]
    fn detect_case_insensitive() {
        assert_eq!(detect_language(Path::new("APP.RS")), LanguageId::Rust);
        assert_eq!(detect_language(Path::new("index.JS")), LanguageId::JavaScript);
    }

    #[test]
    fn detect_from_name_string() {
        assert_eq!(detect_language_from_name("main.rs"), LanguageId::Rust);
        assert_eq!(detect_language_from_name("unknown.xyz"), LanguageId::PlainText);
    }

    #[test]
    fn has_grammar_only_rust() {
        assert!(has_grammar(LanguageId::Rust));
        assert!(!has_grammar(LanguageId::JavaScript));
        assert!(!has_grammar(LanguageId::PlainText));
    }

    #[test]
    fn extensions_for_rust() {
        assert_eq!(extensions_for(LanguageId::Rust), &["rs"]);
    }

    #[test]
    fn extensions_for_plain_text_is_empty() {
        assert!(extensions_for(LanguageId::PlainText).is_empty());
    }

    #[test]
    fn all_languages_excludes_plain_text() {
        let langs = all_languages();
        assert!(!langs.contains(&LanguageId::PlainText));
        assert!(langs.contains(&LanguageId::Rust));
        assert!(langs.contains(&LanguageId::JavaScript));
        assert_eq!(langs.len(), 9);
    }

    #[test]
    fn display_names_are_readable() {
        assert_eq!(LanguageId::Rust.display_name(), "Rust");
        assert_eq!(LanguageId::JavaScript.display_name(), "JavaScript");
        assert_eq!(LanguageId::PlainText.display_name(), "Plain Text");
    }
}
