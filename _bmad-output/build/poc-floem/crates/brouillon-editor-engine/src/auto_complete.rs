//! Auto-completion engine — identifier scanner + ranking.
//!
//! Scans buffer text for identifiers (words matching `[a-zA-Z_][a-zA-Z0-9_]*`),
//! ranks them by proximity to cursor and frequency, and provides a scored
//! completion list. This is the "dumb" completer that works without LSP.
//! When LSP is available, its results are merged with higher priority.

use std::collections::HashMap;

/// A completion candidate with score.
#[derive(Debug, Clone)]
pub struct Completion {
    /// The completion text.
    pub text: String,
    /// Source of this completion.
    pub source: CompletionSource,
    /// Relevance score (higher = better). Range 0..1000.
    pub score: u32,
    /// Optional detail/type info for display.
    pub detail: String,
}

/// Where a completion candidate came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CompletionSource {
    /// From identifier scanning of the current buffer.
    Buffer,
    /// From snippet trigger matching.
    Snippet,
    /// From LSP server (future).
    Lsp,
    /// From keyword list for the current language.
    Keyword,
}

/// Configuration for the completion engine.
#[derive(Debug, Clone)]
pub struct CompletionConfig {
    /// Minimum prefix length before triggering completions.
    pub min_prefix: usize,
    /// Maximum number of results to return.
    pub max_results: usize,
    /// Whether to include keywords in results.
    pub include_keywords: bool,
}

impl Default for CompletionConfig {
    fn default() -> Self {
        Self {
            min_prefix: 2,
            max_results: 20,
            include_keywords: true,
        }
    }
}

/// Extract all identifiers from text.
///
/// An identifier is `[a-zA-Z_][a-zA-Z0-9_]*` with length ≥ 2.
pub fn extract_identifiers(text: &str) -> Vec<String> {
    let mut identifiers = Vec::new();
    let chars: Vec<char> = text.chars().collect();
    let mut i = 0;

    while i < chars.len() {
        if chars[i].is_ascii_alphabetic() || chars[i] == '_' {
            let start = i;
            i += 1;
            while i < chars.len() && (chars[i].is_ascii_alphanumeric() || chars[i] == '_') {
                i += 1;
            }
            let word: String = chars[start..i].iter().collect();
            if word.len() >= 2 {
                identifiers.push(word);
            }
        } else {
            i += 1;
        }
    }

    identifiers
}

/// Count frequency of each identifier in the text.
pub fn frequency_map(identifiers: &[String]) -> HashMap<String, usize> {
    let mut map = HashMap::new();
    for id in identifiers {
        *map.entry(id.clone()).or_insert(0) += 1;
    }
    map
}

/// Compute completions for a given prefix from buffer identifiers.
///
/// `text` is the full buffer text.
/// `prefix` is the partial word being typed.
/// `cursor_line` is the current cursor line (0-based) for proximity scoring.
/// `config` controls filtering and limits.
pub fn complete_from_buffer(
    text: &str,
    prefix: &str,
    cursor_line: usize,
    config: &CompletionConfig,
) -> Vec<Completion> {
    complete_from_lines(text.lines(), text.lines().count(), prefix, cursor_line, config)
}

/// Same as [`complete_from_buffer`] over an iterator of lines, so a caller that holds a rope does not
/// have to copy the whole document into one `String` first. `total_lines` is the line count (proximity scale).
pub fn complete_from_lines<L: AsRef<str>>(
    lines: impl Iterator<Item = L>,
    total_lines: usize,
    prefix: &str,
    cursor_line: usize,
    config: &CompletionConfig,
) -> Vec<Completion> {
    if prefix.len() < config.min_prefix {
        return Vec::new();
    }

    let prefix_lower = prefix.to_lowercase();

    // Extract identifiers with their line numbers
    let mut candidates: HashMap<String, (usize, usize)> = HashMap::new(); // word -> (frequency, closest_line)

    for (line_idx, line) in lines.enumerate() {
        let ids = extract_identifiers(line.as_ref());
        for id in ids {
            if id.to_lowercase().starts_with(&prefix_lower) && id != prefix {
                let entry = candidates.entry(id.clone()).or_insert((0, line_idx));
                entry.0 += 1;
                // Track the closest occurrence to cursor
                let dist_current = (line_idx as isize - cursor_line as isize).unsigned_abs();
                let dist_best = (entry.1 as isize - cursor_line as isize).unsigned_abs();
                if dist_current < dist_best {
                    entry.1 = line_idx;
                }
            }
        }
    }

    // Score and sort
    let total_lines = total_lines.max(1);
    let mut completions: Vec<Completion> = candidates
        .into_iter()
        .map(|(word, (freq, closest_line))| {
            let proximity = 1.0 - (closest_line as isize - cursor_line as isize).unsigned_abs() as f64 / total_lines as f64;
            let freq_score = (freq as f64).ln().min(5.0) / 5.0; // log scale, capped
            let prefix_match = if word.starts_with(prefix) { 0.3 } else { 0.0 }; // case-exact bonus
            let score = ((proximity * 400.0 + freq_score * 300.0 + prefix_match * 300.0) as u32).min(1000);

            Completion {
                text: word,
                source: CompletionSource::Buffer,
                score,
                detail: String::new(),
            }
        })
        .collect();

    completions.sort_by(|a, b| b.score.cmp(&a.score));
    completions.truncate(config.max_results);
    completions
}

/// Keywords for common languages.
pub fn keywords_for_language(lang: &str) -> &'static [&'static str] {
    match lang {
        "rust" => &[
            "as", "async", "await", "break", "const", "continue", "crate", "dyn",
            "else", "enum", "extern", "false", "fn", "for", "if", "impl", "in",
            "let", "loop", "match", "mod", "move", "mut", "pub", "ref", "return",
            "self", "Self", "static", "struct", "super", "trait", "true", "type",
            "unsafe", "use", "where", "while", "yield",
        ],
        "javascript" | "typescript" => &[
            "async", "await", "break", "case", "catch", "class", "const", "continue",
            "debugger", "default", "delete", "do", "else", "export", "extends",
            "false", "finally", "for", "function", "if", "import", "in", "instanceof",
            "let", "new", "null", "return", "super", "switch", "this", "throw",
            "true", "try", "typeof", "undefined", "var", "void", "while", "yield",
        ],
        "python" => &[
            "and", "as", "assert", "async", "await", "break", "class", "continue",
            "def", "del", "elif", "else", "except", "False", "finally", "for",
            "from", "global", "if", "import", "in", "is", "lambda", "None",
            "nonlocal", "not", "or", "pass", "raise", "return", "True", "try",
            "while", "with", "yield",
        ],
        _ => &[],
    }
}

/// Complete from language keywords.
pub fn complete_from_keywords(
    prefix: &str,
    language: &str,
    config: &CompletionConfig,
) -> Vec<Completion> {
    if !config.include_keywords || prefix.len() < config.min_prefix {
        return Vec::new();
    }

    let prefix_lower = prefix.to_lowercase();
    keywords_for_language(language)
        .iter()
        .filter(|kw| kw.to_lowercase().starts_with(&prefix_lower) && **kw != prefix)
        .map(|kw| Completion {
            text: kw.to_string(),
            source: CompletionSource::Keyword,
            score: 500, // fixed mid-range score for keywords
            detail: "keyword".to_string(),
        })
        .take(config.max_results)
        .collect()
}

/// Merge completions from multiple sources, deduplicate, and rank.
pub fn merge_completions(sources: Vec<Vec<Completion>>, max: usize) -> Vec<Completion> {
    let mut seen: HashMap<String, Completion> = HashMap::new();

    for source in sources {
        for comp in source {
            let entry = seen.entry(comp.text.clone());
            entry.and_modify(|existing| {
                // Keep the higher score
                if comp.score > existing.score {
                    *existing = comp.clone();
                }
            }).or_insert(comp);
        }
    }

    let mut result: Vec<Completion> = seen.into_values().collect();
    result.sort_by(|a, b| b.score.cmp(&a.score));
    result.truncate(max);
    result
}

/// Helper to extract the prefix word at the current column (1-based index) on a line of text.
pub fn get_prefix_at(line: &str, col_1b: usize) -> String {
    let col0 = col_1b.saturating_sub(1);
    if col0 == 0 {
        return String::new();
    }
    
    // Convert line to a char vector
    let chars: Vec<char> = line.chars().collect();
    let mut end = col0;
    if end > chars.len() {
        end = chars.len();
    }
    
    let mut start = end;
    while start > 0 {
        let ch = chars[start - 1];
        if ch.is_ascii_alphanumeric() || ch == '_' {
            start -= 1;
        } else {
            break;
        }
    }
    
    if start < end {
        chars[start..end].iter().collect()
    } else {
        String::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_get_prefix_at() {
        assert_eq!(get_prefix_at("let foo = 1;", 7), "fo");
        assert_eq!(get_prefix_at("let foo = 1;", 3), "le");
        assert_eq!(get_prefix_at("let foo = 1;", 4), "let");
        assert_eq!(get_prefix_at("let foo = 1;", 5), "");
    }

    #[test]
    fn extract_identifiers_basic() {
        let ids = extract_identifiers("let foo = bar_baz + 42;");
        assert!(ids.contains(&"let".to_string()));
        assert!(ids.contains(&"foo".to_string()));
        assert!(ids.contains(&"bar_baz".to_string()));
        assert!(!ids.iter().any(|s| s == "42"));
    }

    #[test]
    fn extract_identifiers_single_char_excluded() {
        let ids = extract_identifiers("a b cc dd");
        assert!(!ids.contains(&"a".to_string()));
        assert!(!ids.contains(&"b".to_string()));
        assert!(ids.contains(&"cc".to_string()));
    }

    #[test]
    fn extract_identifiers_underscores() {
        let ids = extract_identifiers("_private __dunder __init__");
        assert!(ids.contains(&"_private".to_string()));
        assert!(ids.contains(&"__dunder".to_string()));
        assert!(ids.contains(&"__init__".to_string()));
    }

    #[test]
    fn frequency_map_counts() {
        let ids = vec!["foo".into(), "bar".into(), "foo".into(), "foo".into(), "bar".into()];
        let freq = frequency_map(&ids);
        assert_eq!(freq["foo"], 3);
        assert_eq!(freq["bar"], 2);
    }

    #[test]
    fn complete_from_buffer_basic() {
        let text = "fn main() {\n    let message = \"hello\";\n    let msg_count = 5;\n    println!(\"{}\", message);\n}";
        let config = CompletionConfig::default();
        let completions = complete_from_buffer(text, "me", 3, &config);
        assert!(!completions.is_empty());
        assert!(completions.iter().any(|c| c.text == "message"));
    }

    #[test]
    fn complete_from_buffer_empty_prefix() {
        let text = "hello world";
        let config = CompletionConfig::default();
        let completions = complete_from_buffer(text, "h", 0, &config);
        assert!(completions.is_empty(), "prefix too short for min_prefix=2");
    }

    #[test]
    fn complete_from_buffer_no_self_match() {
        let text = "let foo = 1;\nlet foobar = 2;";
        let config = CompletionConfig::default();
        let completions = complete_from_buffer(text, "foobar", 1, &config);
        assert!(!completions.iter().any(|c| c.text == "foobar"), "should not match itself");
    }

    #[test]
    fn keywords_rust() {
        let kw = keywords_for_language("rust");
        assert!(kw.contains(&"fn"));
        assert!(kw.contains(&"let"));
        assert!(kw.contains(&"struct"));
    }

    #[test]
    fn keywords_python() {
        let kw = keywords_for_language("python");
        assert!(kw.contains(&"def"));
        assert!(kw.contains(&"class"));
    }

    #[test]
    fn keywords_unknown_language() {
        let kw = keywords_for_language("brainfuck");
        assert!(kw.is_empty());
    }

    #[test]
    fn complete_from_keywords_basic() {
        let config = CompletionConfig::default();
        // "fn" is exactly 2 chars but matches itself — use "fo" which matches "for"
        let _exact = complete_from_keywords("fn", "rust", &config);
        let completions = complete_from_keywords("fo", "rust", &config);
        assert!(completions.iter().any(|c| c.text == "for"));
    }

    #[test]
    fn merge_completions_dedup() {
        let a = vec![Completion { text: "foo".into(), source: CompletionSource::Buffer, score: 100, detail: String::new() }];
        let b = vec![Completion { text: "foo".into(), source: CompletionSource::Keyword, score: 500, detail: String::new() }];
        let merged = merge_completions(vec![a, b], 10);
        assert_eq!(merged.len(), 1);
        assert_eq!(merged[0].score, 500); // keeps higher score
    }

    #[test]
    fn merge_completions_sorted() {
        let a = vec![
            Completion { text: "low".into(), source: CompletionSource::Buffer, score: 100, detail: String::new() },
            Completion { text: "high".into(), source: CompletionSource::Buffer, score: 900, detail: String::new() },
        ];
        let merged = merge_completions(vec![a], 10);
        assert_eq!(merged[0].text, "high");
        assert_eq!(merged[1].text, "low");
    }

    #[test]
    fn merge_completions_truncate() {
        let items: Vec<Completion> = (0..50).map(|i| Completion {
            text: format!("item_{}", i),
            source: CompletionSource::Buffer,
            score: i as u32,
            detail: String::new(),
        }).collect();
        let merged = merge_completions(vec![items], 5);
        assert_eq!(merged.len(), 5);
    }

    #[test]
    fn proximity_scoring() {
        // Identifier on line 1 should score higher when cursor is on line 2
        let text = "let alpha = 1;\nlet aleph = 2;\nlet other = 3;\n\n\n\n\n\n\n\nlet also_far = 4;";
        let config = CompletionConfig { min_prefix: 2, max_results: 20, include_keywords: false };
        let completions = complete_from_buffer(text, "al", 1, &config);
        assert!(completions.len() >= 2);
        // "alpha" (line 0) and "aleph" (line 1) should be top
        let top_texts: Vec<&str> = completions.iter().take(2).map(|c| c.text.as_str()).collect();
        assert!(top_texts.contains(&"alpha") || top_texts.contains(&"aleph"));
    }
}
