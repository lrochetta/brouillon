//! Abbreviation lists (BK-056): word + Space -> expansion, and the auto-close
//! bracket/quote pair table. Own TOML format plus a reader for the EditPlus
//! `.acp` text format (same line grammar as `.ctl`, see `cliptext`).

use crate::cliptext::{decode_text, expand_text, parse_clip_file, today};
use brouillon_core::contract::{CmdCtx, CmdOutcome, CommandSpec, EditSet, Region, TextEdit};
use serde::{Deserialize, Serialize};
use std::path::Path;

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct Abbrev {
    pub abbr: String,
    pub body: String,
}

/// What the shell must do when a character is typed (auto-close table).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AutoClose {
    /// Type normally.
    None,
    /// Type the opener and also insert this closer after the caret.
    InsertCloser(char),
    /// The closer is already next to the caret: move over it, insert nothing.
    SkipOver,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct AcpFile {
    #[serde(default)]
    pub title: String,
    /// Abbreviation matching is case sensitive.
    #[serde(default)]
    pub case_sensitive: bool,
    /// Two-char strings: opener then closer (`"{}"`, `"\"\""`).
    #[serde(default)]
    pub brackets: Vec<String>,
    #[serde(default, rename = "abbrev")]
    pub abbrevs: Vec<Abbrev>,
}

impl AcpFile {
    /// First matching abbreviation (case rule applied).
    pub fn lookup(&self, word: &str) -> Option<&Abbrev> {
        self.abbrevs.iter().find(|a| {
            if self.case_sensitive { a.abbr == word } else { a.abbr.eq_ignore_ascii_case(word) }
        })
    }

    /// Space was typed with the caret at byte `caret`: expand the word before it.
    /// `suppress` models the Shift/Win "no expansion" key. On `Some`, the Space
    /// itself must NOT be inserted.
    pub fn expand_before(&self, text: &str, caret: usize, suppress: bool) -> Option<CmdOutcome> {
        if suppress || !text.is_char_boundary(caret) {
            return None;
        }
        let head = &text[..caret];
        let start = head
            .char_indices()
            .rev()
            .take_while(|&(_, c)| c.is_alphanumeric() || c == '_')
            .last()
            .map(|(i, _)| i)?;
        let ab = self.lookup(&head[start..])?;
        let ex = expand_text(&ab.body, None, &today());
        Some(CmdOutcome {
            edits: EditSet(vec![TextEdit::new(start, caret, ex.text)]),
            selection: Some(vec![Region::caret(start + ex.caret)]),
            ..Default::default()
        })
    }

    /// Decide auto-close behaviour for a typed char; `next` is the char after the caret.
    pub fn auto_close(&self, typed: char, next: Option<char>, suppress: bool) -> AutoClose {
        if suppress {
            return AutoClose::None;
        }
        let pairs = || self.brackets.iter().filter_map(|b| {
            let mut c = b.chars();
            Some((c.next()?, c.next()?))
        });
        if next == Some(typed) && pairs().any(|(_, close)| close == typed) {
            return AutoClose::SkipOver;
        }
        match pairs().find(|&(open, _)| open == typed) {
            Some((_, close)) => AutoClose::InsertCloser(close),
            None => AutoClose::None,
        }
    }
}

/// Parse the EditPlus `.acp` text format.
pub fn parse_acp(text: &str) -> AcpFile {
    let raw = parse_clip_file(text);
    let mut f = AcpFile { title: String::new(), ..Default::default() };
    for (name, value) in &raw.header {
        match name.as_str() {
            "TITLE" => f.title = value.trim().to_string(),
            "CASE" => f.case_sensitive = value.trim().eq_ignore_ascii_case("y"),
            n if n.starts_with("BRACKET") && value.chars().count() >= 2 => {
                f.brackets.push(value.chars().take(2).collect());
            }
            _ => {}
        }
    }
    f.abbrevs = raw.clips.into_iter().map(|c| Abbrev { abbr: c.title, body: c.body }).collect();
    f
}

pub fn to_toml(f: &AcpFile) -> String {
    toml::to_string(f).unwrap_or_default()
}

pub fn from_toml(text: &str) -> Result<AcpFile, String> {
    toml::from_str(text).map_err(|e| e.to_string())
}

/// `.acp` is read as EditPlus text, anything else as TOML.
pub fn load_acp(path: &Path) -> Result<AcpFile, String> {
    let bytes = std::fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let text = decode_text(&bytes);
    if path.extension().is_some_and(|e| e.eq_ignore_ascii_case("acp")) {
        Ok(parse_acp(&text))
    } else {
        from_toml(&text)
    }
}

pub fn commands() -> Vec<CommandSpec> {
    vec![CommandSpec {
        id: "abbrev_expand".into(),
        caption: "Edit: Expand abbreviation".into(),
        category: "Edit".into(),
        default_chord: None,
        when: Some("selection_empty".into()), ..Default::default()
    }]
}

/// `abbrev_expand`: `args[0]` = abbreviation file; expands the word before the caret.
pub fn run(id: &str, ctx: &CmdCtx) -> Option<CmdOutcome> {
    if id != "abbrev_expand" {
        return None;
    }
    let miss = |m: String| Some(CmdOutcome { message: Some(m), ..Default::default() });
    let Some(path) = ctx.args.first() else { return miss("abbreviations: no file".into()) };
    let file = match load_acp(Path::new(path)) {
        Ok(f) => f,
        Err(e) => return miss(e),
    };
    let caret = ctx.selection.first().map_or(0, |r| r.head);
    file.expand_before(ctx.text, caret, false)
        .or_else(|| miss("abbreviations: nothing to expand".into()))
}

#[cfg(test)]
mod tests {
    use super::*;

    const SRC: &str = "; synthetic\n#TITLE=Mini\n#CASE=y\n#BRACKET1={}\n#BRACKET2=\"\"\n#T=if\nif (^!)\n{\n}\n#T=ret\nreturn ^!;\n#\n";

    #[test]
    fn parse_acp_header_and_clips() {
        let f = parse_acp(SRC);
        assert_eq!(f.title, "Mini");
        assert!(f.case_sensitive);
        assert_eq!(f.brackets, vec!["{}", "\"\""]);
        assert_eq!(f.abbrevs.len(), 2);
        assert_eq!(f.abbrevs[0].body, "if (^!)\n{\n}");
        assert_eq!(from_toml(&to_toml(&f)).unwrap(), f);
    }

    #[test]
    fn fn_plus_space_expands_when_rule_exists() {
        let f = AcpFile { abbrevs: vec![Abbrev { abbr: "fn".into(), body: "fn ^!() {}".into() }], ..Default::default() };
        let text = "x = 1;\nfn";
        let o = f.expand_before(text, text.len(), false).unwrap();
        assert_eq!(o.edits.apply_to_str(text).unwrap(), "x = 1;\nfn () {}");
        assert_eq!(o.selection.unwrap()[0].head, text.len() + 1);
        // suppress key, unknown word, mid-word text
        assert!(f.expand_before(text, text.len(), true).is_none());
        assert!(f.expand_before("zz", 2, false).is_none());
        assert!(f.expand_before("", 0, false).is_none());
    }

    #[test]
    fn case_rule() {
        let mut f = parse_acp(SRC);
        assert!(f.lookup("IF").is_none());
        f.case_sensitive = false;
        assert!(f.lookup("IF").is_some());
    }

    #[test]
    fn auto_close_skips_over_typed_closer() {
        let f = parse_acp(SRC);
        assert_eq!(f.auto_close('{', None, false), AutoClose::InsertCloser('}'));
        assert_eq!(f.auto_close('}', Some('}'), false), AutoClose::SkipOver);
        assert_eq!(f.auto_close('}', Some('x'), false), AutoClose::None);
        assert_eq!(f.auto_close('"', Some('"'), false), AutoClose::SkipOver);
        assert_eq!(f.auto_close('"', Some('a'), false), AutoClose::InsertCloser('"'));
        assert_eq!(f.auto_close('{', None, true), AutoClose::None);
    }

    #[test]
    fn run_command_from_file() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("m.acp");
        std::fs::write(&p, SRC).unwrap();
        let args = [p.to_string_lossy().into_owned()];
        let sel = [Region::caret(3)];
        let ctx = CmdCtx { text: "if(", selection: &sel, args: &args, ..Default::default() };
        // caret after "(" -> no word -> message only
        assert!(run("abbrev_expand", &ctx).unwrap().edits.is_empty());
        let sel = [Region::caret(2)];
        let ctx = CmdCtx { text: "if", selection: &sel, args: &args, ..Default::default() };
        assert!(!run("abbrev_expand", &ctx).unwrap().edits.is_empty());
        assert!(run("nope", &ctx).is_none());
    }
}
