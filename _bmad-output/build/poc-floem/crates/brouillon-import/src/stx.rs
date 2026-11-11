//! One-shot `.stx` converter (BK-061, decision D2): extracts the keyword groups
//! and the comment/string tokens into a TOML keyword table. No `.stx` is ever
//! parsed at edit time; the table feeds a fallback highlighter.

use brouillon_editor_engine::cliptext::decode_text;
use serde::Serialize;

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize)]
pub struct LineComment {
    pub start: String,
    /// `^!` prefix in the source: only valid at column 0.
    pub column0_only: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize)]
pub struct BlockComment {
    pub open: String,
    pub close: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize)]
pub struct Group {
    pub name: String,
    pub words: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize)]
pub struct KeywordTable {
    pub title: String,
    pub case_sensitive: bool,
    pub delimiters: String,
    pub escape: String,
    pub quotes: Vec<String>,
    pub line_comments: Vec<LineComment>,
    pub block_comments: Vec<BlockComment>,
    pub groups: Vec<Group>,
}

const MAX_GROUPS: usize = 10;
const KNOWN: &[&str] = &[
    "TITLE", "KEYWORD", "DELIMITER", "QUOTATION", "LINECOMMENT", "COMMENTON", "COMMENTOFF", "ESCAPE", "CASE",
    "AUTOCASE", "PREFIX", "SUFFIX", "CONTINUE_QUOTE", "HTML_EMBEDDED", "SKIP_QUOTE", "AUTOCOMPLETE_QUOTE",
    "NESTED_COMMENT", "CHECK_CLOSINGQUOTE", "SCRIPT_BEGIN", "SCRIPT_END", "SPECIAL_STX", "NUMBER_PATTERN", "HEREDOC",
];

/// Split `#NAME=value` into (stem, digit suffix, value) when NAME is a known directive.
fn known_directive(line: &str) -> Option<(String, Option<char>, &str)> {
    let rest = line.strip_prefix('#')?;
    let (name, value) = rest.split_once('=').unwrap_or((rest, ""));
    let up = name.trim().to_ascii_uppercase();
    let (stem, digit) = match up.chars().last() {
        Some(d) if d.is_ascii_digit() && up.len() > 1 => (up[..up.len() - 1].to_string(), Some(d)),
        _ => (up, None),
    };
    KNOWN.contains(&stem.as_str()).then_some((stem, digit, value))
}

fn unescape(s: &str) -> String {
    let mut out = String::new();
    let mut it = s.chars().peekable();
    while let Some(c) = it.next() {
        if c == '^' && matches!(it.peek(), Some('^' | '#' | ';')) {
            out.push(it.next().unwrap_or('^'));
        } else {
            out.push(c);
        }
    }
    out
}

/// Convert `.stx` text. Returns the table and warnings (ignored directives, caps).
pub fn convert_stx(text: &str) -> Result<(KeywordTable, Vec<String>), String> {
    let mut t = KeywordTable::default();
    let mut warn: Vec<String> = Vec::new();
    let mut seen_title = false;
    let mut in_group = false;
    // comment pairs by index (1, 2): (open, close)
    let mut pairs: [(String, String); 2] = Default::default();
    let mut quotes: [String; 3] = Default::default();
    let mut lines_c: [Option<LineComment>; 2] = Default::default();
    let slot = |d: Option<char>| if d == Some('2') { 1 } else { 0 };
    for line in text.lines() {
        if line.is_empty() || line.starts_with(';') {
            continue;
        }
        if let Some((stem, digit, value)) = known_directive(line) {
            if !seen_title && stem != "TITLE" {
                return Err("#TITLE must be the first statement".into());
            }
            in_group = false;
            match stem.as_str() {
                "TITLE" => { t.title = value.trim().to_string(); seen_title = true; }
                "KEYWORD" => {
                    if t.groups.len() >= MAX_GROUPS {
                        warn.push(format!("more than {MAX_GROUPS} keyword groups: '{value}' dropped"));
                    } else {
                        t.groups.push(Group { name: value.trim().to_string(), words: Vec::new() });
                        in_group = true;
                    }
                }
                "DELIMITER" => t.delimiters = value.to_string(),
                "ESCAPE" => t.escape = value.to_string(),
                "CASE" => t.case_sensitive = value.trim().eq_ignore_ascii_case("y"),
                "QUOTATION" => {
                    if let Some(i) = digit.and_then(|d| d.to_digit(10)).filter(|d| (1..=3).contains(d)) {
                        quotes[i as usize - 1] = value.to_string();
                    }
                }
                "LINECOMMENT" => {
                    let (start, c0) = match value.strip_prefix("^!") { Some(r) => (r, true), None => (value, false) };
                    if !start.is_empty() {
                        lines_c[slot(digit)] = Some(LineComment { start: unescape(start), column0_only: c0 });
                    }
                }
                "COMMENTON" => pairs[slot(digit)].0 = value.to_string(),
                "COMMENTOFF" => pairs[slot(digit)].1 = value.to_string(),
                other => warn.push(format!("ignored directive #{other}")),
            }
            continue;
        }
        if !seen_title {
            return Err("#TITLE must be the first statement".into());
        }
        if line == "#" {
            in_group = false;
        } else if in_group {
            if let Some(g) = t.groups.last_mut() {
                g.words.push(unescape(line));
            }
        }
    }
    if !seen_title {
        return Err("no #TITLE".into());
    }
    t.quotes = quotes.into_iter().filter(|q| !q.is_empty()).collect();
    t.line_comments = lines_c.into_iter().flatten().collect();
    t.block_comments = pairs
        .into_iter()
        .filter(|(o, c)| !o.is_empty() && !c.is_empty())
        .map(|(open, close)| BlockComment { open, close })
        .collect();
    Ok((t, warn))
}

/// Read raw `.stx` bytes (UTF-8 or Windows-1252) and convert.
pub fn convert_stx_bytes(bytes: &[u8]) -> Result<(KeywordTable, Vec<String>), String> {
    convert_stx(&decode_text(bytes))
}

pub fn to_toml(t: &KeywordTable) -> String {
    toml::to_string(t).unwrap_or_default()
}

// ---------------------------------------------------------------------------
// Brouillon language definition (syntax/langs format) from a converted table
// ---------------------------------------------------------------------------

#[derive(Serialize)]
struct StringOut {
    open: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    escape: Option<String>,
}

#[derive(Serialize)]
struct KeywordsOut {
    name: String,
    words: Vec<String>,
}

#[derive(Serialize)]
struct DefinitionOut {
    name: String,
    extensions: Vec<String>,
    case_sensitive: bool,
    line_comments: Vec<String>,
    line_comments_col0: Vec<String>,
    block_comments: Vec<[String; 2]>,
    string: Vec<StringOut>,
    keywords: Vec<KeywordsOut>,
}

/// Render a converted `.stx` table as a Brouillon language definition (the TOML
/// format of `brouillon_editor_engine::syntax`). The result drops into the user
/// `syntax/` folder; `.stx` files carry no file extensions, so `extensions` is
/// left empty for the user to fill in.
pub fn to_definition_toml(t: &KeywordTable) -> String {
    let escape = (!t.escape.trim().is_empty()).then(|| t.escape.trim().to_string());
    let def = DefinitionOut {
        name: t.title.clone(),
        extensions: Vec::new(),
        case_sensitive: t.case_sensitive,
        line_comments: t.line_comments.iter().filter(|c| !c.column0_only).map(|c| c.start.clone()).collect(),
        line_comments_col0: t.line_comments.iter().filter(|c| c.column0_only).map(|c| c.start.clone()).collect(),
        block_comments: t.block_comments.iter().map(|b| [b.open.clone(), b.close.clone()]).collect(),
        string: t.quotes.iter().filter(|q| q.chars().count() == 1).map(|q| StringOut { open: q.clone(), escape: escape.clone() }).collect(),
        keywords: t.groups.iter().map(|g| KeywordsOut { name: g.name.clone(), words: g.words.clone() }).collect(),
    };
    let body = toml::to_string(&def).unwrap_or_default();
    format!("# Imported from an EditPlus .stx file. Fill in `extensions`.
{body}")
}

#[cfg(test)]
mod tests {
    use super::*;

    const SYNTH: &str = "#TITLE=MiniLang\n; synthetic\n#DELIMITER=,(){}\n#QUOTATION1=\"\n#QUOTATION2='\n#LINECOMMENT=--\n#LINECOMMENT2=^!#\n#COMMENTON=(*\n#COMMENTOFF=*)\n#ESCAPE=\\\n#CASE=n\n#AUTOCASE=y\n#PREFIX3=$\n\n#KEYWORD=Reserved\nbegin\nend\n^#pragma\n#zlib_thing\n#\nignored_after_end\n#KEYWORD=Types\nint\n; inner comment\ntext\n";

    #[test]
    fn extracts_groups_and_tokens() {
        let (t, warn) = convert_stx(SYNTH).unwrap();
        assert_eq!(t.title, "MiniLang");
        assert_eq!(t.quotes, vec!["\"", "'"]);
        assert_eq!(t.line_comments, vec![
            LineComment { start: "--".into(), column0_only: false },
            LineComment { start: "#".into(), column0_only: true },
        ]);
        assert_eq!(t.block_comments, vec![BlockComment { open: "(*".into(), close: "*)".into() }]);
        assert_eq!(t.groups.len(), 2);
        assert_eq!(t.groups[0].words, vec!["begin", "end", "#pragma", "#zlib_thing"]);
        assert_eq!(t.groups[1].words, vec!["int", "text"]);
        assert!(!t.case_sensitive);
        assert_eq!(warn, vec!["ignored directive #AUTOCASE", "ignored directive #PREFIX"]);
        assert!(to_toml(&t).contains("[[groups]]"));
    }

    #[test]
    fn title_must_come_first() {
        assert!(convert_stx("#DELIMITER=,\n#TITLE=X\n").is_err());
        assert!(convert_stx("; only a comment\n").is_err());
    }

    #[test]
    fn definition_toml_loads_in_the_syntax_registry() {
        use brouillon_editor_engine::highlight::SyntaxKind;
        use brouillon_editor_engine::syntax::{Registry, SyntaxDoc};
        let (t, _) = convert_stx(SYNTH).unwrap();
        let toml = to_definition_toml(&t);
        let reg = Registry::build(&[("MiniLang.toml".to_string(), toml.replace("extensions = []", "extensions = [\"mini\"]"))]);
        assert!(reg.errors().is_empty(), "{:?}", reg.errors());
        let i = reg.detect(std::path::Path::new("a.mini"), None).expect("detected by the filled-in extension");
        assert_eq!(reg.def(i).name, "MiniLang");
        let src = "BEGIN x -- c
\"q\" int
";
        let doc = SyntaxDoc::new(std::sync::Arc::new(reg), i, src);
        let kinds: Vec<(String, SyntaxKind)> =
            doc.spans_in_range(0..src.len()).into_iter().map(|s| (src[s.byte_range].to_string(), s.kind)).collect();
        assert!(kinds.contains(&("BEGIN".to_string(), SyntaxKind::Keyword)), "{kinds:?}"); // case-insensitive
        assert!(kinds.contains(&("-- c".to_string(), SyntaxKind::Comment)));
        assert!(kinds.contains(&("\"q\"".to_string(), SyntaxKind::String)));
        assert!(kinds.contains(&("int".to_string(), SyntaxKind::Keyword2)));
    }

    #[test]
    fn group_cap_warns() {
        let mut s = String::from("#TITLE=T\n");
        for i in 0..11 {
            s.push_str(&format!("#KEYWORD=g{i}\nw{i}\n"));
        }
        let (t, warn) = convert_stx(&s).unwrap();
        assert_eq!(t.groups.len(), 10);
        assert_eq!(warn.len(), 1);
    }
}
