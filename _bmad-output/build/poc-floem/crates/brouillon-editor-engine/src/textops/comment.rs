//! BK-032 comment toggle. Language = `args[0]` (name or extension) else the extension of `ctx.path`.

use super::*;
use brouillon_core::comment_tokens::{for_language, CommentTokens};

pub fn commands() -> Vec<CommandSpec> {
    vec![
        spec("toggle_line_comment", "Edit: Toggle Line Comment", "Comment"),
        spec("toggle_block_comment", "Edit: Toggle Block Comment", "Comment"),
    ]
}

pub fn run(id: &str, ctx: &CmdCtx) -> Option<CmdOutcome> {
    if id != "toggle_line_comment" && id != "toggle_block_comment" {
        return None;
    }
    let t = ctx.text;
    let lang = ctx.args.first().cloned().or_else(|| ctx.path.and_then(|p| p.extension()).map(|e| e.to_string_lossy().into_owned()));
    let Some(tokens) = lang.as_deref().and_then(for_language) else {
        return Some(message("No comment syntax known for this file type"));
    };
    let ls = split_lines(t);
    Some(if id == "toggle_line_comment" {
        outcome(t, toggle_lines(t, &ls, ctx.selection, tokens))
    } else {
        match tokens.block {
            Some(b) => outcome(t, toggle_block(t, &ls, ctx.selection, b)),
            None => message("No block comment syntax for this file type"),
        }
    })
}

fn toggle_lines(t: &str, ls: &[Line], sel: &[Region], tk: CommentTokens) -> Vec<TextEdit> {
    let (a, b) = target_range(t, ls, sel, false);
    let (open, close) = match (tk.line, tk.block) {
        (Some(l), _) => (l, ""),
        (None, Some((o, c))) => (o, c),
        _ => return Vec::new(),
    };
    let content = |i: usize| &t[ls[i].start..ls[i].end];
    let body: Vec<usize> = (a..=b).filter(|&i| !content(i).trim().is_empty()).collect();
    let commented = |i: usize| {
        let c = content(i).trim();
        c.starts_with(open) && c.ends_with(close)
    };
    if body.is_empty() {
        return Vec::new();
    }
    let mut edits = Vec::new();
    if body.iter().all(|&i| commented(i)) {
        for &i in &body {
            let c = content(i);
            let lead = c.len() - c.trim_start().len();
            let after = lead + open.len();
            let rest = &c[after..];
            let rest = rest.strip_prefix(' ').unwrap_or(rest);
            let trail_start = c.trim_end().len() - close.len();
            let head = c.len() - rest.len();
            edits.push(TextEdit::delete(ls[i].start + lead, ls[i].start + head));
            if !close.is_empty() {
                let end = ls[i].start + trail_start;
                let end_from = if t[..end].ends_with(' ') && end - 1 >= ls[i].start + head { end - 1 } else { end };
                edits.push(TextEdit::delete(end_from, ls[i].start + c.trim_end().len()));
            }
        }
    } else {
        let indent = body.iter().map(|&i| content(i).len() - content(i).trim_start().len()).min().unwrap_or(0);
        for &i in &body {
            edits.push(TextEdit::insert(ls[i].start + indent, format!("{open} ")));
            if !close.is_empty() {
                edits.push(TextEdit::insert(ls[i].start + content(i).trim_end().len(), format!(" {close}")));
            }
        }
    }
    edits
}

fn toggle_block(t: &str, ls: &[Line], sel: &[Region], (open, close): (&str, &str)) -> Vec<TextEdit> {
    let mut edits = Vec::new();
    for r in sel {
        let (s, e) = if r.is_caret() {
            let l = ls[line_of(ls, r.head)];
            (l.start, l.end)
        } else {
            (r.start(), r.end())
        };
        let seg = &t[s..e];
        let trimmed = seg.trim();
        if trimmed.len() >= open.len() + close.len() && trimmed.starts_with(open) && trimmed.ends_with(close) {
            let lead = seg.len() - seg.trim_start().len();
            let mut a = s + lead + open.len();
            if t[a..].starts_with(' ') {
                a += 1;
            }
            let mut b = s + lead + trimmed.len() - close.len();
            if b > a && t[..b].ends_with(' ') {
                b -= 1;
            }
            edits.push(TextEdit::delete(s + lead, a));
            edits.push(TextEdit::delete(b, s + lead + trimmed.len()));
        } else if !trimmed.is_empty() {
            edits.push(TextEdit::insert(s + (seg.len() - seg.trim_start().len()), format!("{open} ")));
            edits.push(TextEdit::insert(s + seg.trim_end().len(), format!(" {close}")));
        }
    }
    edits
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::textops::{apply, sel};

    #[test]
    fn toggle_twice_is_identity() {
        for (lang, text) in [("rs", "fn a() {\n    let x = 1;\n\n    y();\n}\n"), ("py", "if x:\n    pass\n"), ("html", "<p>a</p>\n  <b>c</b>")] {
            let all = sel(0, text.len());
            let once = apply("toggle_line_comment", text, &all, &[lang]);
            assert_ne!(once, text, "{lang}");
            let twice = apply("toggle_line_comment", &once, &sel(0, once.len()), &[lang]);
            assert_eq!(twice, text, "{lang}");
        }
    }

    #[test]
    fn formats() {
        assert_eq!(apply("toggle_line_comment", "a\n  b", &sel(0, 5), &["rs"]), "// a\n//   b");
        assert_eq!(apply("toggle_line_comment", "  a\n  b", &sel(0, 7), &["py"]), "  # a\n  # b");
        assert_eq!(apply("toggle_line_comment", "<p>", &sel(0, 0), &["html"]), "<!-- <p> -->");
    }

    #[test]
    fn uncomment_with_and_without_space() {
        assert_eq!(apply("toggle_line_comment", "//a\n// b", &sel(0, 8), &["rs"]), "a\nb");
        assert_eq!(apply("toggle_line_comment", "#a", &sel(0, 0), &["py"]), "a");
    }

    #[test]
    fn mixed_selection_comments_everything() {
        assert_eq!(apply("toggle_line_comment", "// a\nb", &sel(0, 7), &["rs"]), "// // a\n// b");
    }

    #[test]
    fn block_toggle_and_path_language() {
        assert_eq!(apply("toggle_block_comment", "abc def", &sel(0, 3), &["rs"]), "/* abc */ def");
        assert_eq!(apply("toggle_block_comment", "/* abc */ def", &sel(0, 9), &["rs"]), "abc def");
        let ctx = CmdCtx { text: "x", selection: &[], path: Some(std::path::Path::new("a.py")), args: &[] };
        assert_eq!(run("toggle_line_comment", &ctx).unwrap().edits.apply_to_str("x").unwrap(), "# x");
        let ctx = CmdCtx { text: "x", selection: &[], path: None, args: &[] };
        assert!(run("toggle_line_comment", &ctx).unwrap().message.is_some());
    }
}
