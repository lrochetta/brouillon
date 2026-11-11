//! Language definition format: TOML schema (`Raw*`) and the compiled,
//! lookup-optimised `Syntax`. See `langs/README.md` for the user-facing docs.

use crate::highlight::SyntaxKind;
use serde::Deserialize;
use std::collections::HashMap;

/// Non-fatal problem found while loading a definition.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SyntaxError {
    /// File name (or builtin id) the problem comes from.
    pub source: String,
    /// 1-based line when known.
    pub line: Option<usize>,
    pub message: String,
}

impl std::fmt::Display for SyntaxError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.line {
            Some(l) => write!(f, "{}:{}: {}", self.source, l, self.message),
            None => write!(f, "{}: {}", self.source, self.message),
        }
    }
}

// ---------------------------------------------------------------------------
// Raw (serde) schema
// ---------------------------------------------------------------------------

/// A word list: either a TOML array or one whitespace-separated string.
#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
pub enum Words {
    List(Vec<String>),
    Text(String),
}

impl Words {
    fn into_vec(self) -> Vec<String> {
        match self {
            Words::List(v) => v,
            Words::Text(t) => t.split_whitespace().map(str::to_string).collect(),
        }
    }
}

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(deny_unknown_fields, default)]
pub struct RawString {
    pub open: String,
    pub close: Option<String>,
    pub escape: Option<String>,
    pub multiline: bool,
    pub doubled: bool,
    pub check_closing: bool,
    pub kind: Option<String>,
    pub escapes: Option<bool>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RawGroup {
    #[serde(default)]
    pub name: String,
    pub words: Words,
    pub kind: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(deny_unknown_fields, default)]
pub struct RawPrefix {
    pub chars: String,
    pub kind: Option<String>,
    /// Also colour `${...}`.
    pub braced: bool,
    /// Also colour `$(...)` and single special chars such as `$@` `$<` (make/shell).
    pub parens: bool,
}

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(deny_unknown_fields, default)]
pub struct RawLineStart {
    pub prefix: String,
    pub kind: String,
}

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(deny_unknown_fields, default)]
pub struct RawPre {
    pub start: String,
    pub angle_include: Vec<String>,
}

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(deny_unknown_fields, default)]
pub struct RawKeys {
    pub separators: String,
    pub kind: Option<String>,
    pub strings: bool,
    pub line_start_only: bool,
}

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(deny_unknown_fields, default)]
pub struct RawScript {
    pub begin: Vec<String>,
    pub end: String,
}

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct RawRawText {
    pub tag: String,
    pub lang: String,
}

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(deny_unknown_fields, default)]
pub struct RawMarkup {
    pub raw_text: Vec<RawRawText>,
    pub cdata: Option<bool>,
    pub pi: bool,
    pub entities: Option<bool>,
}

/// One language definition file, as written by the user.
#[derive(Debug, Clone, Deserialize, Default)]
#[serde(deny_unknown_fields, default)]
pub struct RawDef {
    pub name: String,
    pub extends: Option<String>,
    pub extensions: Vec<String>,
    pub filenames: Vec<String>,
    pub shebang: Vec<String>,
    pub first_line: Vec<String>,
    pub case_sensitive: Option<bool>,
    pub word_chars: String,
    pub number: Option<String>,
    pub operators: String,
    pub functions: Option<bool>,
    pub regex_literals: Option<bool>,
    pub nested_comments: Option<bool>,
    pub host: Option<String>,
    pub heredoc: Vec<String>,
    pub line_comments: Vec<String>,
    pub line_comments_col0: Vec<String>,
    pub block_comments: Vec<[String; 2]>,
    pub string: Vec<RawString>,
    pub keywords: Vec<RawGroup>,
    pub prefix: Vec<RawPrefix>,
    pub line_start: Vec<RawLineStart>,
    pub preprocessor: Option<RawPre>,
    pub keys: Option<RawKeys>,
    pub script: Option<RawScript>,
    pub markup: Option<RawMarkup>,
}

impl RawDef {
    /// Overlay `self` (child) on `base`: scalar/list fields of the child win
    /// when set, keyword groups of the same name are merged, others appended.
    /// Identity fields (extensions, filenames, shebang, first_line, name) are
    /// never inherited.
    pub fn overlay_on(mut self, base: &RawDef) -> RawDef {
        macro_rules! inherit_vec {
            ($($f:ident),*) => { $( if self.$f.is_empty() { self.$f = base.$f.clone(); } )* };
        }
        macro_rules! inherit_opt {
            ($($f:ident),*) => { $( if self.$f.is_none() { self.$f = base.$f.clone(); } )* };
        }
        inherit_vec!(heredoc, line_comments, line_comments_col0, block_comments, string, prefix, line_start);
        inherit_opt!(
            case_sensitive, number, functions, regex_literals, nested_comments, host, preprocessor, keys, script,
            markup
        );
        if self.word_chars.is_empty() {
            self.word_chars = base.word_chars.clone();
        }
        if self.operators.is_empty() {
            self.operators = base.operators.clone();
        }
        let mut groups = base.keywords.clone();
        for g in std::mem::take(&mut self.keywords) {
            let new_words = g.words.clone().into_vec();
            match groups.iter_mut().find(|b| !g.name.is_empty() && b.name.eq_ignore_ascii_case(&g.name)) {
                Some(b) => {
                    let mut merged = b.words.clone().into_vec();
                    merged.extend(new_words);
                    b.words = Words::List(merged);
                }
                None => groups.push(g),
            }
        }
        self.keywords = groups;
        self.extends = None;
        self
    }
}

// ---------------------------------------------------------------------------
// Compiled form
// ---------------------------------------------------------------------------

pub(crate) const F_BLOCK: u8 = 1;
pub(crate) const F_LINE: u8 = 2;
pub(crate) const F_STR: u8 = 4;
pub(crate) const F_PREFIX: u8 = 8;
pub(crate) const F_OP: u8 = 16;
pub(crate) const F_HEREDOC: u8 = 32;
pub(crate) const F_PRE: u8 = 64;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Origin {
    Builtin,
    User,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum NumStyle {
    None,
    C,
    Css,
}

#[derive(Debug, Clone)]
pub(crate) struct LineRule {
    pub tok: Box<[u8]>,
    pub col0: bool,
}

#[derive(Debug, Clone)]
pub(crate) struct BlockRule {
    pub open: Box<[u8]>,
    pub close: Box<[u8]>,
    pub nested: bool,
}

#[derive(Debug, Clone)]
pub(crate) struct StrRule {
    pub open: Box<[u8]>,
    pub close: Box<[u8]>,
    pub escape: Option<u8>,
    pub multiline: bool,
    pub doubled: bool,
    pub check_closing: bool,
    pub kind: SyntaxKind,
    pub hl_escapes: bool,
}

#[derive(Debug, Clone)]
pub(crate) struct PrefixRule {
    pub ch: u8,
    pub kind: SyntaxKind,
    pub braced: bool,
    pub parens: bool,
}

#[derive(Debug, Clone)]
pub(crate) struct PreRule {
    pub start: Box<[u8]>,
    pub angle_include: Vec<String>,
}

#[derive(Debug, Clone)]
pub(crate) struct KeysRule {
    pub seps: Vec<u8>,
    pub kind: SyntaxKind,
    pub strings: bool,
    pub line_start_only: bool,
}

#[derive(Debug, Clone)]
pub(crate) struct ScriptRule {
    pub begin: Vec<Box<[u8]>>,
    pub end: Box<[u8]>,
}

#[derive(Debug, Clone)]
pub(crate) struct RawText {
    pub tag: String,
    pub lang: String,
    pub def: Option<u16>,
}

#[derive(Debug, Clone)]
pub(crate) struct MarkupRule {
    pub raw_text: Vec<RawText>,
    pub cdata: bool,
    pub pi: bool,
    pub entities: bool,
}

/// A compiled language definition.
#[derive(Debug, Clone)]
pub struct Syntax {
    pub name: &'static str,
    pub origin: Origin,
    pub source: String,
    pub extensions: Vec<String>,
    pub filenames: Vec<String>,
    pub shebang: Vec<String>,
    pub first_line: Vec<String>,
    pub case_sensitive: bool,
    pub(crate) host_name: Option<String>,
    pub(crate) host: Option<u16>,
    pub(crate) word_start: [bool; 256],
    pub(crate) word_cont: [bool; 256],
    pub(crate) flags: [u8; 256],
    pub(crate) line_comments: Vec<LineRule>,
    pub(crate) blocks: Vec<BlockRule>,
    pub(crate) strings: Vec<StrRule>,
    pub(crate) words: HashMap<Box<str>, SyntaxKind>,
    pub(crate) prefixes: Vec<PrefixRule>,
    pub(crate) line_start: Vec<(Box<[u8]>, SyntaxKind)>,
    pub(crate) pre: Option<PreRule>,
    pub(crate) heredoc: Vec<Box<[u8]>>,
    pub(crate) regex_literals: bool,
    pub(crate) functions: bool,
    pub(crate) number: NumStyle,
    pub(crate) keys: Option<KeysRule>,
    pub(crate) script: Option<ScriptRule>,
    pub(crate) markup: Option<MarkupRule>,
}

fn kind_of(name: &Option<String>, default: SyntaxKind) -> Result<SyntaxKind, String> {
    match name {
        None => Ok(default),
        Some(n) => SyntaxKind::from_name(n).ok_or_else(|| format!("unknown kind \"{n}\"")),
    }
}

/// 1-based line of the first line containing `needle` (best effort locator for
/// semantic errors that serde cannot span).
fn locate(text: &str, needle: &str) -> Option<usize> {
    if needle.is_empty() {
        return None;
    }
    text.lines().position(|l| l.contains(needle)).map(|i| i + 1)
}

fn single_byte(s: &str) -> Result<u8, String> {
    match s.as_bytes() {
        [b] => Ok(*b),
        _ => Err(format!("expected one ASCII character, got \"{s}\"")),
    }
}

impl Syntax {
    /// Compile a (already `extends`-resolved) raw definition. `text` is only
    /// used to attach line numbers to errors.
    pub fn compile(
        raw: RawDef,
        origin: Origin,
        source: &str,
        text: &str,
        name: &'static str,
    ) -> Result<Syntax, SyntaxError> {
        let err = |msg: String, needle: &str| SyntaxError {
            source: source.to_string(),
            line: locate(text, needle),
            message: msg,
        };
        if raw.name.trim().is_empty() {
            return Err(err("missing `name`".into(), "name"));
        }
        let case_sensitive = raw.case_sensitive.unwrap_or(true);
        let number = match raw.number.as_deref() {
            None | Some("c") => NumStyle::C,
            Some("css") => NumStyle::Css,
            Some("none") => NumStyle::None,
            Some(o) => return Err(err(format!("unknown number style \"{o}\" (c, css, none)"), o)),
        };

        let mut word_start = [false; 256];
        let mut word_cont = [false; 256];
        for b in 0u8..=255 {
            let alpha = b.is_ascii_alphabetic() || b == b'_' || b >= 0x80;
            word_start[b as usize] = alpha;
            word_cont[b as usize] = alpha || b.is_ascii_digit();
        }
        for b in raw.word_chars.bytes() {
            word_start[b as usize] = true;
            word_cont[b as usize] = true;
        }
        let mut flags = [0u8; 256];
        for b in raw.operators.bytes() {
            flags[b as usize] |= F_OP;
        }

        // Comments.
        let mut line_comments = Vec::new();
        for (list, col0) in [(&raw.line_comments, false), (&raw.line_comments_col0, true)] {
            for t in list {
                if t.is_empty() {
                    return Err(err("empty line comment token".into(), "line_comments"));
                }
                flags[t.as_bytes()[0] as usize] |= F_LINE;
                if !case_sensitive {
                    flags[t.as_bytes()[0].to_ascii_lowercase() as usize] |= F_LINE;
                    flags[t.as_bytes()[0].to_ascii_uppercase() as usize] |= F_LINE;
                }
                line_comments.push(LineRule { tok: t.as_bytes().into(), col0 });
            }
        }
        line_comments.sort_by_key(|r| std::cmp::Reverse(r.tok.len()));
        let mut blocks = Vec::new();
        for [o, c] in &raw.block_comments {
            if o.is_empty() || c.is_empty() {
                return Err(err("block comment needs non-empty open and close".into(), o));
            }
            flags[o.as_bytes()[0] as usize] |= F_BLOCK;
            blocks.push(BlockRule {
                open: o.as_bytes().into(),
                close: c.as_bytes().into(),
                nested: raw.nested_comments.unwrap_or(false),
            });
        }
        blocks.sort_by_key(|r| std::cmp::Reverse(r.open.len()));

        // Strings.
        let mut strings = Vec::new();
        for s in &raw.string {
            if s.open.is_empty() {
                return Err(err("string rule with empty `open`".into(), "[[string]]"));
            }
            let close = s.close.clone().unwrap_or_else(|| s.open.clone());
            let escape = match s.escape.as_deref() {
                None | Some("") => None,
                Some(e) => Some(single_byte(e).map_err(|m| err(m, e))?),
            };
            let kind = kind_of(&s.kind, SyntaxKind::String).map_err(|m| err(m, "kind"))?;
            flags[s.open.as_bytes()[0] as usize] |= F_STR;
            strings.push(StrRule {
                open: s.open.as_bytes().into(),
                close: close.as_bytes().into(),
                escape,
                multiline: s.multiline,
                doubled: s.doubled,
                check_closing: s.check_closing,
                kind,
                hl_escapes: s.escapes.unwrap_or(escape.is_some()),
            });
        }
        strings.sort_by_key(|r| std::cmp::Reverse(r.open.len()));

        // Keyword groups.
        if raw.keywords.len() > 10 {
            return Err(err(format!("{} keyword groups (max 10)", raw.keywords.len()), "[[keywords]]"));
        }
        let mut words: HashMap<Box<str>, SyntaxKind> = HashMap::new();
        for (i, g) in raw.keywords.into_iter().enumerate() {
            let kind = kind_of(&g.kind, SyntaxKind::keyword_group(i + 1)).map_err(|m| err(m, "kind"))?;
            for w in g.words.into_vec() {
                let key: Box<str> = if case_sensitive { w.into() } else { w.to_ascii_lowercase().into() };
                words.entry(key).or_insert(kind);
            }
        }

        // Prefixes.
        let mut prefixes = Vec::new();
        for p in &raw.prefix {
            let kind = kind_of(&p.kind, SyntaxKind::Variable).map_err(|m| err(m, "kind"))?;
            if p.chars.is_empty() {
                return Err(err("prefix rule with empty `chars`".into(), "[[prefix]]"));
            }
            for b in p.chars.bytes() {
                flags[b as usize] |= F_PREFIX;
                prefixes.push(PrefixRule { ch: b, kind, braced: p.braced, parens: p.parens });
            }
        }

        let mut line_start = Vec::new();
        for l in &raw.line_start {
            if l.prefix.is_empty() {
                return Err(err("line_start rule with empty `prefix`".into(), "[[line_start]]"));
            }
            let kind = SyntaxKind::from_name(&l.kind).ok_or_else(|| err(format!("unknown kind \"{}\"", l.kind), &l.kind))?;
            line_start.push((Box::<[u8]>::from(l.prefix.as_bytes()), kind));
        }

        let pre = match raw.preprocessor {
            Some(p) if !p.start.is_empty() => {
                flags[p.start.as_bytes()[0] as usize] |= F_PRE;
                Some(PreRule { start: p.start.as_bytes().into(), angle_include: p.angle_include })
            }
            Some(_) => return Err(err("preprocessor with empty `start`".into(), "[preprocessor]")),
            None => None,
        };

        let mut heredoc: Vec<Box<[u8]>> = Vec::new();
        for h in &raw.heredoc {
            if h.is_empty() {
                return Err(err("empty heredoc leader".into(), "heredoc"));
            }
            flags[h.as_bytes()[0] as usize] |= F_HEREDOC;
            heredoc.push(h.as_bytes().into());
        }
        heredoc.sort_by_key(|h| std::cmp::Reverse(h.len()));

        let keys = match raw.keys {
            Some(k) => {
                let seps = k.separators.bytes().collect::<Vec<u8>>();
                if seps.is_empty() {
                    return Err(err("keys.separators is empty".into(), "[keys]"));
                }
                Some(KeysRule {
                    seps,
                    kind: kind_of(&k.kind, SyntaxKind::Attribute).map_err(|m| err(m, "kind"))?,
                    strings: k.strings,
                    line_start_only: k.line_start_only,
                })
            }
            None => None,
        };

        let script = match raw.script {
            Some(s) => {
                if s.begin.is_empty() || s.end.is_empty() || s.begin.iter().any(|b| b.is_empty()) {
                    return Err(err("script needs `begin` tokens and an `end`".into(), "[script]"));
                }
                let mut begin: Vec<Box<[u8]>> = s.begin.iter().map(|b| b.as_bytes().into()).collect();
                begin.sort_by_key(|b| std::cmp::Reverse(b.len()));
                Some(ScriptRule { begin, end: s.end.as_bytes().into() })
            }
            None => None,
        };

        let markup = raw.markup.map(|m| MarkupRule {
            raw_text: m
                .raw_text
                .into_iter()
                .map(|r| RawText { tag: r.tag.to_ascii_lowercase(), lang: r.lang, def: None })
                .collect(),
            cdata: m.cdata.unwrap_or(true),
            pi: m.pi,
            entities: m.entities.unwrap_or(true),
        });

        Ok(Syntax {
            name,
            origin,
            source: source.to_string(),
            extensions: raw.extensions.iter().map(|e| e.trim_start_matches('.').to_ascii_lowercase()).collect(),
            filenames: raw.filenames,
            shebang: raw.shebang,
            first_line: raw.first_line.iter().map(|s| s.to_ascii_lowercase()).collect(),
            case_sensitive,
            host_name: raw.host,
            host: None,
            word_start,
            word_cont,
            flags,
            line_comments,
            blocks,
            strings,
            words,
            prefixes,
            line_start,
            pre,
            heredoc,
            regex_literals: raw.regex_literals.unwrap_or(false),
            functions: raw.functions.unwrap_or(false),
            number,
            keys,
            script,
            markup,
        })
    }

    /// Keyword/word lookup (case folded when the language is case-insensitive).
    pub(crate) fn word_kind(&self, w: &str) -> Option<SyntaxKind> {
        if self.case_sensitive {
            self.words.get(w).copied()
        } else if w.len() <= 48 {
            let mut buf = [0u8; 48];
            for (i, b) in w.bytes().enumerate() {
                buf[i] = b.to_ascii_lowercase();
            }
            self.words.get(std::str::from_utf8(&buf[..w.len()]).ok()?).copied()
        } else {
            self.words.get(w.to_ascii_lowercase().as_str()).copied()
        }
    }

    /// Number of keyword entries (all groups).
    pub fn word_count(&self) -> usize {
        self.words.len()
    }
}
