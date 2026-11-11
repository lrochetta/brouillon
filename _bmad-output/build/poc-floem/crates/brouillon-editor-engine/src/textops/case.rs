//! BK-028 case conversion. Empty selection acts on the word at the caret.

use super::*;

pub fn commands() -> Vec<CommandSpec> {
    [
        ("case_upper", "Case: UPPERCASE"),
        ("case_lower", "Case: lowercase"),
        ("case_invert", "Case: iNVERT cASE"),
        ("case_capitalize", "Case: Capitalize Each Word"),
        ("case_sentence", "Case: Sentence case"),
        ("case_snake", "Case: snake_case"),
        ("case_camel", "Case: camelCase"),
        ("case_pascal", "Case: PascalCase"),
        ("case_kebab", "Case: kebab-case"),
        ("case_upper_blend", "Case: UPPERCASE (only wrong-case letters)"),
        ("case_lower_blend", "Case: lowercase (only wrong-case letters)"),
    ]
    .iter()
    .map(|(id, cap)| spec(id, cap, "Case"))
    .collect()
}

pub fn run(id: &str, ctx: &CmdCtx) -> Option<CmdOutcome> {
    let t = ctx.text;
    let f: fn(&str) -> String = match id {
        "case_upper" | "case_upper_blend" => upper,
        "case_lower" | "case_lower_blend" => lower,
        "case_invert" => invert,
        "case_capitalize" => capitalize,
        "case_sentence" => sentence,
        "case_snake" => |s| join_words(s, "_", false),
        "case_kebab" => |s| join_words(s, "-", false),
        "case_camel" => |s| camel(s, false),
        "case_pascal" => |s| camel(s, true),
        _ => return None,
    };
    let blend = id.ends_with("_blend");
    let mut edits = Vec::new();
    for r in ctx.selection {
        let (s, e) = if r.is_caret() {
            match word_at(t, r.head) {
                Some(w) => w,
                None => continue,
            }
        } else {
            (r.start(), r.end())
        };
        if blend {
            let mut pos = s;
            for ch in t[s..e].chars() {
                let one = ch.to_string();
                let new = f(&one);
                edits.push(TextEdit::new(pos, pos + ch.len_utf8(), new));
                pos += ch.len_utf8();
            }
        } else {
            edits.push(TextEdit::new(s, e, f(&t[s..e])));
        }
    }
    Some(outcome(t, edits))
}

pub fn upper(s: &str) -> String {
    s.to_uppercase()
}

pub fn lower(s: &str) -> String {
    s.to_lowercase()
}

pub fn invert(s: &str) -> String {
    s.chars()
        .map(|c| if c.is_lowercase() { c.to_uppercase().collect::<String>() } else if c.is_uppercase() { c.to_lowercase().collect() } else { c.to_string() })
        .collect()
}

fn title_char(c: char) -> String {
    c.to_uppercase().collect()
}

pub fn capitalize(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut start = true;
    for c in s.chars() {
        if c.is_alphanumeric() {
            if start { out.push_str(&title_char(c)) } else { out.extend(c.to_lowercase()) }
            start = false;
        } else {
            out.push(c);
            start = c != '\'' && c != '\u{2019}';
        }
    }
    out
}

pub fn sentence(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut start = true;
    for c in s.chars() {
        if c.is_alphabetic() {
            if start { out.push_str(&title_char(c)) } else { out.extend(c.to_lowercase()) }
            start = false;
        } else {
            out.push(c);
            if matches!(c, '.' | '!' | '?' | '\n') {
                start = true;
            }
        }
    }
    out
}

/// Split into words on non-alphanumerics and lower->Upper / letter<->digit boundaries.
pub fn words(s: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut cur = String::new();
    let mut prev: Option<char> = None;
    let chars: Vec<char> = s.chars().collect();
    for (i, &c) in chars.iter().enumerate() {
        if !c.is_alphanumeric() {
            if !cur.is_empty() {
                out.push(std::mem::take(&mut cur));
            }
            prev = None;
            continue;
        }
        let next_lower = chars.get(i + 1).is_some_and(|n| n.is_lowercase());
        if let Some(p) = prev {
            let boundary = (p.is_lowercase() && c.is_uppercase()) || (p.is_alphabetic() != c.is_alphabetic()) || (p.is_uppercase() && c.is_uppercase() && next_lower);
            if boundary && !cur.is_empty() {
                out.push(std::mem::take(&mut cur));
            }
        }
        cur.push(c);
        prev = Some(c);
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

fn join_words(s: &str, sep: &str, _upper: bool) -> String {
    words(s).iter().map(|w| w.to_lowercase()).collect::<Vec<_>>().join(sep)
}

fn camel(s: &str, pascal: bool) -> String {
    words(s)
        .iter()
        .enumerate()
        .map(|(i, w)| {
            let lw = w.to_lowercase();
            if i == 0 && !pascal {
                lw
            } else {
                let mut cs = lw.chars();
                cs.next().map(|c| title_char(c) + cs.as_str()).unwrap_or_default()
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::textops::{apply, sel};

    #[test]
    fn unicode_rules() {
        assert_eq!(upper("straße"), "STRASSE");
        assert_eq!(lower("ΟΔΥΣΣΕΥΣ"), "οδυσσευς"); // final sigma
        assert_eq!(lower("İ"), "i\u{307}");
        assert_eq!(invert("aBc1"), "AbC1");
        assert_eq!(capitalize("hello wORLD it's"), "Hello World It's");
        assert_eq!(sentence("HELLO. wORLD! ok"), "Hello. World! Ok");
    }

    #[test]
    fn identifiers() {
        assert_eq!(join_words("HelloWorld foo-bar", "_", false), "hello_world_foo_bar");
        assert_eq!(camel("hello_world", false), "helloWorld");
        assert_eq!(camel("hello world", true), "HelloWorld");
        assert_eq!(join_words("XMLHttpRequest", "-", false), "xml-http-request");
    }

    #[test]
    fn selection_and_word_default() {
        assert_eq!(apply("case_upper", "foo bar", &sel(0, 3), &[]), "FOO bar");
        // empty selection acts on the word
        assert_eq!(apply("case_upper", "foo bar", &sel(5, 5), &[]), "foo BAR");
        assert_eq!(apply("case_upper", "   ", &sel(1, 1), &[]), "   ");
    }

    #[test]
    fn blend_edits_only_wrong_case_letters() {
        let t = "aBcD";
        let ctx = CmdCtx { text: t, selection: &sel(0, 4), path: None, args: &[] };
        let o = run("case_lower_blend", &ctx).unwrap();
        assert_eq!(o.edits.len(), 2);
        assert_eq!(o.edits.apply_to_str(t).unwrap(), "abcd");
    }
}
