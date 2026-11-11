//! BK-035 smart auto-indent: pure edit generators for Enter and closing characters.
//! Independent of BK-041 file-type profiles: `IndentProfile::for_language` is a small stand-in
//! that the orchestrator may replace with the real profile.

use crate::textops::{eol_of, line_of, split_lines};
use brouillon_core::contract::{CmdCtx, CmdOutcome, CommandSpec, EditSet, Region, TextEdit};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IndentProfile {
    /// One indent level ("\t" or N spaces).
    pub unit: String,
    /// Chars that open a block when they end the text before the caret.
    pub openers: Vec<char>,
}

impl IndentProfile {
    pub fn new(unit: &str) -> Self {
        Self { unit: unit.into(), openers: vec!['{', '[', '('] }
    }

    pub fn for_language(lang: &str) -> Self {
        let mut p = Self::new("    ");
        match lang.trim_start_matches('.').to_ascii_lowercase().as_str() {
            "py" | "python" | "yaml" | "yml" => p.openers.push(':'),
            "go" | "makefile" | "mk" => p.unit = "\t".into(),
            _ => {}
        }
        p
    }
}

impl Default for IndentProfile {
    fn default() -> Self {
        Self::new("    ")
    }
}

fn closer_of(open: char) -> Option<char> {
    match open {
        '{' => Some('}'),
        '[' => Some(']'),
        '(' => Some(')'),
        _ => None,
    }
}

/// Enter at `caret`: new line with the current indent, one extra level after an opener.
/// Between `{` and `}` the closer is moved to its own line and the caret placed on the body.
pub fn on_enter(text: &str, caret: usize, p: &IndentProfile) -> CmdOutcome {
    let ls = split_lines(text);
    let l = ls[line_of(&ls, caret)];
    let line = &text[l.start..l.end];
    let indent: String = line.chars().take_while(|c| *c == ' ' || *c == '\t').collect();
    let before = text[l.start..caret.min(l.end)].trim_end();
    let eol = eol_of(text);
    let opener = before.chars().next_back().filter(|c| p.openers.contains(c));
    let extra = if opener.is_some() { p.unit.as_str() } else { "" };
    let body = format!("{eol}{indent}{extra}");
    let after_is_closer = opener.and_then(closer_of).is_some_and(|c| text[caret..l.end].trim_start().starts_with(c));
    let caret_at = caret + body.len();
    if after_is_closer {
        let skip = text[caret..l.end].len() - text[caret..l.end].trim_start().len();
        return CmdOutcome {
            edits: EditSet(vec![TextEdit::new(caret, caret + skip, format!("{body}{eol}{indent}"))]),
            selection: Some(vec![Region::caret(caret_at)]),
            ..Default::default()
        };
    }
    if line.trim().is_empty() && caret >= l.end && !line.is_empty() {
        // do not leave trailing indentation on a blank line
        return CmdOutcome {
            selection: Some(vec![Region::caret(l.start + body.len())]),
            edits: EditSet(vec![TextEdit::new(l.start, caret, body)]),
            ..Default::default()
        };
    }
    CmdOutcome { edits: EditSet(vec![TextEdit::insert(caret, body)]), selection: Some(vec![Region::caret(caret_at)]), ..Default::default() }
}

/// Typing `ch` (a closing char) at `caret`: when only whitespace precedes it on the line,
/// the line is dedented one level before the char is inserted.
pub fn on_close_char(text: &str, caret: usize, ch: char, p: &IndentProfile) -> CmdOutcome {
    let ls = split_lines(text);
    let l = ls[line_of(&ls, caret)];
    let lead = &text[l.start..caret];
    let mut new_lead = lead.to_string();
    if matches!(ch, '}' | ']' | ')') && !lead.is_empty() && lead.chars().all(|c| c == ' ' || c == '\t') {
        new_lead = if let Some(s) = lead.strip_suffix(p.unit.as_str()) {
            s.to_string()
        } else if let Some(s) = lead.strip_suffix('\t') {
            s.to_string()
        } else {
            lead.trim_end_matches(' ').to_string()
        };
    }
    let text_in = format!("{new_lead}{ch}");
    CmdOutcome {
        edits: EditSet(vec![TextEdit::new(l.start, caret, text_in.clone())]),
        selection: Some(vec![Region::caret(l.start + text_in.len())]),
        ..Default::default()
    }
}

pub fn commands() -> Vec<CommandSpec> {
    vec![crate::textops::spec("newline_auto_indent", "Edit: New Line With Auto-indent", "Edit")]
}

/// `newline_auto_indent`: `args[0]` = language/extension (optional); uses the first selection head.
pub fn run(id: &str, ctx: &CmdCtx) -> Option<CmdOutcome> {
    if id != "newline_auto_indent" {
        return None;
    }
    let lang = ctx.args.first().cloned().or_else(|| ctx.path.and_then(|p| p.extension()).map(|e| e.to_string_lossy().into_owned())).unwrap_or_default();
    Some(on_enter(ctx.text, ctx.selection.first().map_or(0, |r| r.head), &IndentProfile::for_language(&lang)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn enter(text: &str, caret: usize) -> (String, usize) {
        let o = on_enter(text, caret, &IndentProfile::default());
        (o.edits.apply_to_str(text).unwrap(), o.selection.unwrap()[0].head)
    }

    #[test]
    fn replicates_indent() {
        assert_eq!(enter("  foo", 5), ("  foo\n  ".into(), 8));
        assert_eq!(enter("\tfoo\r\nbar", 4), ("\tfoo\r\n\t\r\nbar".into(), 7));
    }

    #[test]
    fn extra_indent_after_opener_and_split_braces() {
        assert_eq!(enter("fn a() {", 8), ("fn a() {\n    ".into(), 13));
        assert_eq!(enter("  if x {}", 8), ("  if x {\n      \n  }".into(), 15));
        assert_eq!(enter("a(", 2).0, "a(\n    ");
    }

    #[test]
    fn brace_then_close_yields_indented_body_and_dedented_brace() {
        let (t, caret) = enter("{", 1);
        assert_eq!(t, "{\n    ");
        let o = on_close_char(&t, caret, '}', &IndentProfile::default());
        assert_eq!(o.edits.apply_to_str(&t).unwrap(), "{\n}");
        // not dedented when text precedes the brace
        let o = on_close_char("  x ", 4, '}', &IndentProfile::default());
        assert_eq!(o.edits.apply_to_str("  x ").unwrap(), "  x }");
    }

    #[test]
    fn profile_and_command() {
        let p = IndentProfile::for_language("py");
        let o = on_enter("if x:", 5, &p);
        assert_eq!(o.edits.apply_to_str("if x:").unwrap(), "if x:\n    ");
        assert_eq!(IndentProfile::for_language("go").unit, "\t");
        let args = vec!["py".to_string()];
        let ctx = CmdCtx { text: "if x:", selection: &[Region::caret(5)], path: None, args: &args };
        assert_eq!(run("newline_auto_indent", &ctx).unwrap().edits.apply_to_str("if x:").unwrap(), "if x:\n    ");
        assert!(run("other", &ctx).is_none());
    }
}
