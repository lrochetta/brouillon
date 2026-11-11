//! "Lite" diagnostics from what the editor really has today (there is no language server yet):
//! unbalanced brackets in code-like files, JSON syntax errors, encoding damage and mixed line
//! endings. An empty list means "none of these checks found anything", not "the code compiles".

use crate::api::Diagnostic;

const MAX_ITEMS: usize = 50;

/// Languages whose brackets are checked (extension, lower case).
const BRACKET_LANGS: [&str; 22] = [
    "rs", "js", "jsx", "ts", "tsx", "json", "c", "h", "cpp", "hpp", "cc", "java", "go", "cs", "css", "scss", "py", "php", "kt", "swift", "toml", "sh",
];

fn d(line: usize, col: usize, severity: &str, message: String, source: &str) -> Diagnostic {
    Diagnostic { line, col, severity: severity.into(), message, source: source.into() }
}

pub fn lite(text: &str, lang: &str) -> Vec<Diagnostic> {
    let mut out = Vec::new();
    encoding(text, &mut out);
    eol(text, &mut out);
    if lang == "json" {
        if let Err(e) = serde_json::from_str::<serde_json::Value>(text) {
            out.push(d(e.line().max(1), e.column().max(1), "error", format!("JSON syntax error: {}", e.to_string().split(" at line").next().unwrap_or("invalid")), "json"));
        }
    } else if BRACKET_LANGS.contains(&lang) {
        brackets(text, lang, &mut out);
    }
    out.sort_by_key(|x| (x.line, x.col));
    out.truncate(MAX_ITEMS);
    out
}

fn encoding(text: &str, out: &mut Vec<Diagnostic>) {
    let mut line = 1;
    let mut col = 0;
    let mut seen_repl = false;
    let mut seen_nul = false;
    for (i, c) in text.char_indices() {
        col += 1;
        match c {
            '\n' => {
                line += 1;
                col = 0;
            }
            '\u{FFFD}' if !seen_repl => {
                seen_repl = true;
                out.push(d(line, col, "warning", "Replacement character: the file was probably decoded with the wrong encoding".into(), "encoding"));
            }
            '\0' if !seen_nul => {
                seen_nul = true;
                out.push(d(line, col, "warning", "NUL character: this may be a binary file".into(), "encoding"));
            }
            '\u{FEFF}' if i > 0 => out.push(d(line, col, "warning", "Byte-order mark in the middle of the text".into(), "encoding")),
            _ => {}
        }
    }
}

fn eol(text: &str, out: &mut Vec<Diagnostic>) {
    let (mut crlf, mut lf, mut cr) = (0usize, 0usize, 0usize);
    let b = text.as_bytes();
    for (i, &x) in b.iter().enumerate() {
        match x {
            b'\n' if i > 0 && b[i - 1] == b'\r' => crlf += 1,
            b'\n' => lf += 1,
            b'\r' if b.get(i + 1) != Some(&b'\n') => cr += 1,
            _ => {}
        }
    }
    let kinds = [crlf, lf, cr].iter().filter(|n| **n > 0).count();
    if kinds > 1 {
        // Point at the first line whose ending is the minority one.
        let minority = if crlf >= lf && crlf >= cr { "lf" } else if lf >= cr { "crlf" } else { "lf" };
        let mut line = 1;
        let mut where_ = 1;
        for (i, &x) in b.iter().enumerate() {
            if x == b'\n' {
                let is_crlf = i > 0 && b[i - 1] == b'\r';
                if (minority == "crlf" && is_crlf) || (minority == "lf" && !is_crlf) {
                    where_ = line;
                    break;
                }
                line += 1;
            }
        }
        out.push(d(where_, 1, "warning", format!("Mixed line endings (CRLF {crlf}, LF {lf}, CR {cr})"), "eol"));
    }
}

fn brackets(text: &str, lang: &str, out: &mut Vec<Diagnostic>) {
    let hash_comments = matches!(lang, "py" | "toml" | "sh" | "php");
    let slash_comments = !matches!(lang, "py" | "toml" | "sh");
    let mut stack: Vec<(char, usize, usize)> = Vec::new();
    let (mut line, mut col) = (1usize, 0usize);
    let mut it = text.chars().peekable();
    let mut in_block = false;
    while let Some(c) = it.next() {
        col += 1;
        if c == '\n' {
            line += 1;
            col = 0;
            continue;
        }
        if in_block {
            if c == '*' && it.peek() == Some(&'/') {
                it.next();
                col += 1;
                in_block = false;
            }
            continue;
        }
        match c {
            '/' if slash_comments && it.peek() == Some(&'/') => {
                while it.peek().map_or(false, |n| *n != '\n') {
                    it.next();
                }
            }
            '/' if slash_comments && it.peek() == Some(&'*') => {
                it.next();
                col += 1;
                in_block = true;
            }
            '#' if hash_comments => {
                while it.peek().map_or(false, |n| *n != '\n') {
                    it.next();
                }
            }
            '\'' if lang == "rs" => {
                // A char literal ('x', '\n', '\'') swallows its content; a lifetime ('a) does not.
                let mut look = it.clone();
                let mut taken = 0;
                match look.next() {
                    Some('\\') => {
                        look.next();
                        taken = 2;
                    }
                    Some(x) if x != '\n' => taken = 1,
                    _ => {}
                }
                if taken > 0 && look.next() == Some('\'') {
                    for _ in 0..=taken {
                        it.next();
                        col += 1;
                    }
                }
            }
            '"' | '\'' => {
                // A string on one line; an unterminated quote simply ends at the line end.
                while let Some(&n) = it.peek() {
                    if n == '\n' {
                        break;
                    }
                    it.next();
                    col += 1;
                    if n == '\\' {
                        if it.peek().map_or(false, |m| *m != '\n') {
                            it.next();
                            col += 1;
                        }
                    } else if n == c {
                        break;
                    }
                }
            }
            '(' | '[' | '{' => stack.push((c, line, col)),
            ')' | ']' | '}' => {
                let want = match c {
                    ')' => '(',
                    ']' => '[',
                    _ => '{',
                };
                match stack.last() {
                    Some(&(o, _, _)) if o == want => {
                        stack.pop();
                    }
                    _ => out.push(d(line, col, "error", format!("Unmatched '{c}'"), "brackets")),
                }
            }
            _ => {}
        }
    }
    for (o, l, c) in stack {
        out.push(d(l, c, "error", format!("Unclosed '{o}'"), "brackets"));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn msgs(t: &str, lang: &str) -> Vec<String> {
        lite(t, lang).into_iter().map(|x| format!("{}:{} {}", x.line, x.col, x.message)).collect()
    }

    #[test]
    fn clean_code_has_no_findings() {
        assert!(lite("fn main() {\n    let a = [1, 2];\n    println!(\"}}\");\n}\n", "rs").is_empty());
        assert!(lite("", "rs").is_empty());
    }

    #[test]
    fn unclosed_and_unmatched_brackets_are_located() {
        assert_eq!(msgs("fn a() {\n  call(\n}\n", "rs"), ["1:8 Unclosed '{'", "2:7 Unclosed '('", "3:1 Unmatched '}'"].map(String::from));
        assert_eq!(msgs("x = 1)\n", "py"), ["1:6 Unmatched ')'"].map(String::from));
    }

    #[test]
    fn strings_and_comments_do_not_count() {
        assert!(lite("let s = \"(((\"; // ) ) )\n/* ] */\nlet c = 'a';\n", "rs").is_empty());
        assert!(lite("x = '(' # )\n", "py").is_empty());
        // A Rust lifetime is not a string.
        assert!(lite("fn f<'a>(x: &'a str) -> &'a str { x }\n", "rs").is_empty());
    }

    #[test]
    fn prose_is_not_bracket_checked() {
        assert!(lite("a smiley :-( and (an aside\n", "text").is_empty());
        assert!(lite("a smiley :-( and (an aside\n", "md").is_empty());
    }

    #[test]
    fn json_errors_have_a_position() {
        let r = lite("{\n  \"a\": 1,\n}\n", "json");
        assert_eq!(r.len(), 1);
        assert_eq!((r[0].severity.as_str(), r[0].source.as_str()), ("error", "json"));
        assert_eq!(r[0].line, 3);
        assert!(lite("{\"a\": [1, 2]}", "json").is_empty());
    }

    #[test]
    fn encoding_damage_and_mixed_eol() {
        let r = msgs("ok\nbad \u{FFFD} here\n", "text");
        assert_eq!(r.len(), 1);
        assert!(r[0].starts_with("2:5 Replacement character"), "{r:?}");
        assert!(msgs("a\0b", "text")[0].contains("NUL"));
        assert!(msgs("a\r\nb\nc\r\nd\r\n", "text")[0].starts_with("2:1 Mixed line endings"));
        assert!(lite("a\r\nb\r\n", "text").is_empty());
        assert!(lite("a\nb\n", "text").is_empty());
    }

    #[test]
    fn output_is_capped_and_sorted() {
        let t = ")".repeat(80);
        let r = lite(&t, "rs");
        assert_eq!(r.len(), MAX_ITEMS);
        assert!(r.windows(2).all(|w| (w[0].line, w[0].col) <= (w[1].line, w[1].col)));
    }
}
