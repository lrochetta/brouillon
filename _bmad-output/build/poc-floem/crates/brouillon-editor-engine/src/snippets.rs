//! Cliptext / snippets engine (text snippets with a classic .ctl reader).
//!
//! Loads user-defined text snippets from `~/.brouillon/snippets.toml`,
//! supports tabstop placeholders (`$1`, `$2`, `$0` for final cursor),
//! and provides fuzzy-match lookup by trigger prefix.
//!
//! TOML format:
//! ```toml
//! [[snippet]]
//! trigger = "fn"
//! name = "Rust function"
//! language = "rust"
//! body = "fn ${1:name}(${2:args}) -> ${3:ReturnType} {\n    $0\n}"
//!
//! [[snippet]]
//! trigger = "for"
//! name = "For loop"
//! body = "for ${1:item} in ${2:collection} {\n    $0\n}"
//! ```

use std::path::{Path, PathBuf};

/// A single snippet definition.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Snippet {
    /// Trigger prefix that activates this snippet (e.g. "fn", "for", "if").
    pub trigger: String,
    /// Human-readable name for display in the completion menu.
    pub name: String,
    /// Optional language filter (e.g. "rust", "python"). Empty = all languages.
    #[serde(default)]
    pub language: String,
    /// Snippet body with tabstop placeholders.
    /// `$1`, `$2` = tabstops in order, `$0` = final cursor position.
    /// `${1:placeholder}` = tabstop with default text.
    pub body: String,
}

/// A parsed tabstop in a snippet body.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TabStop {
    /// Tabstop index (0 = final cursor position).
    pub index: usize,
    /// Byte offset in the expanded text where this tabstop is.
    pub offset: usize,
    /// Default placeholder text (empty if none).
    pub placeholder: String,
}

/// Result of expanding a snippet body.
#[derive(Debug, Clone)]
pub struct ExpandedSnippet {
    /// The expanded text with placeholders replaced by their defaults.
    pub text: String,
    /// Tabstops in order (index 1, 2, ... then 0 for final).
    pub tabstops: Vec<TabStop>,
}

/// Snippet collection with lookup.
#[derive(Debug, Clone)]
pub struct SnippetStore {
    snippets: Vec<Snippet>,
}

/// TOML wrapper for deserialization.
#[derive(Debug, serde::Deserialize)]
struct SnippetFile {
    #[serde(default)]
    snippet: Vec<Snippet>,
}

impl SnippetStore {
    /// Create an empty store.
    pub fn empty() -> Self {
        Self { snippets: Vec::new() }
    }

    /// Create a store from a list of snippets.
    pub fn new(snippets: Vec<Snippet>) -> Self {
        Self { snippets }
    }

    /// Load snippets from a TOML file. Returns empty store on error.
    pub fn load(path: &Path) -> Self {
        let content = match std::fs::read_to_string(path) {
            Ok(c) => c,
            Err(_) => return Self::empty(),
        };
        Self::parse(&content)
    }

    /// Parse snippets from TOML content.
    pub fn parse(content: &str) -> Self {
        match toml::from_str::<SnippetFile>(content) {
            Ok(f) => Self::new(f.snippet),
            Err(_) => Self::empty(),
        }
    }

    /// Find snippets matching a trigger prefix.
    /// If `language` is non-empty, only returns snippets for that language
    /// (plus language-agnostic snippets).
    pub fn find_by_prefix(&self, prefix: &str, language: &str) -> Vec<&Snippet> {
        self.snippets.iter()
            .filter(|s| {
                s.trigger.starts_with(prefix) &&
                (s.language.is_empty() || s.language == language)
            })
            .collect()
    }

    /// Find an exact trigger match.
    pub fn find_exact(&self, trigger: &str, language: &str) -> Option<&Snippet> {
        self.snippets.iter()
            .find(|s| {
                s.trigger == trigger &&
                (s.language.is_empty() || s.language == language)
            })
    }

    /// Total number of snippets in the store.
    pub fn len(&self) -> usize {
        self.snippets.len()
    }

    /// Check if the store is empty.
    pub fn is_empty(&self) -> bool {
        self.snippets.is_empty()
    }

    /// Get all snippets as a slice.
    pub fn all(&self) -> &[Snippet] {
        &self.snippets
    }
}

/// Expand a snippet body, resolving tabstop placeholders.
///
/// Replaces `${N:placeholder}` with the placeholder text and records
/// the tabstop positions. Simple `$N` tabstops get empty placeholders.
pub fn expand_snippet(body: &str) -> ExpandedSnippet {
    let mut result = String::with_capacity(body.len());
    let mut tabstops: Vec<TabStop> = Vec::new();
    let chars: Vec<char> = body.chars().collect();
    let mut i = 0;

    while i < chars.len() {
        if chars[i] == '$' && i + 1 < chars.len() {
            if chars[i + 1] == '{' {
                // ${N:placeholder} form
                if let Some(close) = chars[i + 2..].iter().position(|&c| c == '}') {
                    let inner: String = chars[i + 2..i + 2 + close].iter().collect();
                    let (idx, placeholder) = if let Some(colon_pos) = inner.find(':') {
                        let idx_str = &inner[..colon_pos];
                        let ph = &inner[colon_pos + 1..];
                        (idx_str.parse::<usize>().unwrap_or(0), ph.to_string())
                    } else {
                        (inner.parse::<usize>().unwrap_or(0), String::new())
                    };

                    tabstops.push(TabStop {
                        index: idx,
                        offset: result.len(),
                        placeholder: placeholder.clone(),
                    });
                    result.push_str(&placeholder);
                    i += 2 + close + 1; // skip past }
                    continue;
                }
            } else if chars[i + 1].is_ascii_digit() {
                // $N simple form
                let idx = (chars[i + 1] as u8 - b'0') as usize;
                tabstops.push(TabStop {
                    index: idx,
                    offset: result.len(),
                    placeholder: String::new(),
                });
                i += 2;
                continue;
            }
        }

        // Handle \n and \t escapes
        if chars[i] == '\\' && i + 1 < chars.len() {
            match chars[i + 1] {
                'n' => { result.push('\n'); i += 2; continue; }
                't' => { result.push('\t'); i += 2; continue; }
                _ => {}
            }
        }

        result.push(chars[i]);
        i += 1;
    }

    // Sort tabstops: 1, 2, 3... then 0 (final cursor) at the end
    tabstops.sort_by_key(|t| if t.index == 0 { usize::MAX } else { t.index });

    ExpandedSnippet {
        text: result,
        tabstops,
    }
}

/// Default snippets file path: `~/.brouillon/snippets.toml`.
pub fn snippets_path() -> Option<PathBuf> {
    dirs::home_dir().map(|h| h.join(".brouillon").join("snippets.toml"))
}

/// Load the user's snippet store from the default path.
pub fn load_user_snippets() -> SnippetStore {
    match snippets_path() {
        Some(p) if p.exists() => SnippetStore::load(&p),
        _ => SnippetStore::empty(),
    }
}

/// Built-in default snippets for common patterns.
pub fn builtin_snippets() -> SnippetStore {
    SnippetStore::new(vec![
        Snippet {
            trigger: "fn".into(),
            name: "Function".into(),
            language: "rust".into(),
            body: "fn ${1:name}(${2:args}) {\n    $0\n}".into(),
        },
        Snippet {
            trigger: "for".into(),
            name: "For loop".into(),
            language: String::new(),
            body: "for ${1:item} in ${2:collection} {\n    $0\n}".into(),
        },
        Snippet {
            trigger: "if".into(),
            name: "If block".into(),
            language: String::new(),
            body: "if ${1:condition} {\n    $0\n}".into(),
        },
        Snippet {
            trigger: "match".into(),
            name: "Match expression".into(),
            language: "rust".into(),
            body: "match ${1:expr} {\n    ${2:pattern} => $0,\n}".into(),
        },
        Snippet {
            trigger: "test".into(),
            name: "Test function".into(),
            language: "rust".into(),
            body: "#[test]\nfn ${1:test_name}() {\n    $0\n}".into(),
        },
        Snippet {
            trigger: "def".into(),
            name: "Python function".into(),
            language: "python".into(),
            body: "def ${1:name}(${2:args}):\n    $0".into(),
        },
    ])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn expand_simple_tabstops() {
        let expanded = expand_snippet("Hello $1 World $2");
        assert_eq!(expanded.text, "Hello  World ");
        assert_eq!(expanded.tabstops.len(), 2);
        assert_eq!(expanded.tabstops[0].index, 1);
        assert_eq!(expanded.tabstops[1].index, 2);
    }

    #[test]
    fn expand_placeholder_tabstops() {
        let expanded = expand_snippet("fn ${1:name}(${2:args}) { $0 }");
        assert_eq!(expanded.text, "fn name(args) {  }");
        assert_eq!(expanded.tabstops.len(), 3);
        assert_eq!(expanded.tabstops[0].placeholder, "name");
        assert_eq!(expanded.tabstops[1].placeholder, "args");
        // $0 is last
        assert_eq!(expanded.tabstops[2].index, 0);
    }

    #[test]
    fn expand_final_cursor_is_last() {
        let expanded = expand_snippet("$0 first $1 second $2");
        assert_eq!(expanded.tabstops.last().unwrap().index, 0);
    }

    #[test]
    fn expand_escape_sequences() {
        let expanded = expand_snippet("line1\\nline2\\ttab");
        assert_eq!(expanded.text, "line1\nline2\ttab");
    }

    #[test]
    fn parse_toml_snippets() {
        let toml = r#"
[[snippet]]
trigger = "fn"
name = "Function"
body = "fn ${1:name}() {}"

[[snippet]]
trigger = "for"
name = "For loop"
language = "rust"
body = "for $1 in $2 { $0 }"
"#;
        let store = SnippetStore::parse(toml);
        assert_eq!(store.len(), 2);
        assert_eq!(store.all()[0].trigger, "fn");
        assert_eq!(store.all()[1].language, "rust");
    }

    #[test]
    fn find_by_prefix() {
        let store = builtin_snippets();
        let matches = store.find_by_prefix("f", "rust");
        assert!(matches.iter().any(|s| s.trigger == "fn"));
        assert!(matches.iter().any(|s| s.trigger == "for"));
    }

    #[test]
    fn find_by_prefix_language_filter() {
        let store = builtin_snippets();
        let matches = store.find_by_prefix("def", "python");
        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0].trigger, "def");

        // "def" shouldn't match for rust
        let matches = store.find_by_prefix("def", "rust");
        assert!(matches.is_empty());
    }

    #[test]
    fn find_exact_trigger() {
        let store = builtin_snippets();
        let s = store.find_exact("match", "rust");
        assert!(s.is_some());
        assert_eq!(s.unwrap().name, "Match expression");
    }

    #[test]
    fn find_exact_not_found() {
        let store = builtin_snippets();
        assert!(store.find_exact("nonexistent", "rust").is_none());
    }

    #[test]
    fn empty_store() {
        let store = SnippetStore::empty();
        assert!(store.is_empty());
        assert_eq!(store.len(), 0);
    }

    #[test]
    fn builtin_snippets_count() {
        let store = builtin_snippets();
        assert!(store.len() >= 5);
    }

    #[test]
    fn tabstop_offsets_are_correct() {
        let expanded = expand_snippet("A${1:bc}D${2:ef}G");
        // Text: "AbcDefG"
        assert_eq!(expanded.text, "AbcDefG");
        assert_eq!(expanded.tabstops[0].offset, 1); // "bc" starts at 1
        assert_eq!(expanded.tabstops[1].offset, 4); // "ef" starts at 4
    }
}
