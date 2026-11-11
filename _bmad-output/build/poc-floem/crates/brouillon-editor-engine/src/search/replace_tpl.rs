//! Replacement templates (BK-017, BK-021).
//!
//! Syntax (clean-room, from behavioural specs):
//! - `\1`..`\9`, `\0`, `$0`..`$9`, `${n}`, `$&` : capture groups / whole match
//! - `\t` `\n` `\r` : control characters (`\n` is LF; Lot 1 normalises EOLs)
//! - `\U` `\L` : upper / lower-case the following text, `\E` ends the innermost
//!   case scope (a stray `\E` is a no-op); scopes nest
//! - `\\` `\$` `$$` : literal backslash / dollar; any other `\x` stays literal
//! - placeholders (opt-in, BK-021): `%MATCH%`, `%GROUPn%`, `%LINE%`, with an
//!   optional `+k` / `-k` offset and `:Zw` (zero) / `:Sw` (space) padding,
//!   e.g. `%LINE+1:Z4%`

/// Case-conversion scope opened by `\U` / `\L`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Case {
    Upper,
    Lower,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Part {
    Lit(String),
    Group(usize),
    Line { offset: i64, pad: Option<(char, usize)> },
    Begin(Case),
    End,
}

/// What the template needs to know about one match.
pub struct MatchCtx<'a> {
    /// Text of capture group `n` (0 = whole match); `None` if unmatched.
    pub group: &'a dyn Fn(usize) -> Option<&'a str>,
    /// 1-based line of the match start (only read when `needs_line()`).
    pub line: usize,
}

/// A parsed replacement string.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Template {
    parts: Vec<Part>,
    adapt_case: bool,
}

impl Template {
    /// Parse `src`. Never fails: unknown escapes are literal.
    pub fn parse(src: &str) -> Self {
        Self::parse_inner(src, false)
    }

    /// Like [`parse`](Self::parse) but also recognises `%MATCH%` placeholders.
    pub fn parse_with_placeholders(src: &str) -> Self {
        Self::parse_inner(src, true)
    }

    /// Adapt the replacement's case to the matched text ("Foo"/"FOO"/"foo").
    pub fn with_adapt_case(mut self, on: bool) -> Self {
        self.adapt_case = on;
        self
    }

    /// True if expansion reads `MatchCtx::line`.
    pub fn needs_line(&self) -> bool {
        self.parts.iter().any(|p| matches!(p, Part::Line { .. }))
    }

    fn parse_inner(src: &str, placeholders: bool) -> Self {
        let mut parts: Vec<Part> = Vec::new();
        let mut lit = String::new();
        let flush = |lit: &mut String, parts: &mut Vec<Part>| {
            if !lit.is_empty() {
                parts.push(Part::Lit(std::mem::take(lit)));
            }
        };
        let chars: Vec<char> = src.chars().collect();
        let mut i = 0;
        while i < chars.len() {
            let c = chars[i];
            match c {
                '\\' if i + 1 < chars.len() => {
                    let n = chars[i + 1];
                    i += 2;
                    match n {
                        '0'..='9' => {
                            flush(&mut lit, &mut parts);
                            parts.push(Part::Group(n as usize - '0' as usize));
                        }
                        't' => lit.push('\t'),
                        'n' => lit.push('\n'),
                        'r' => lit.push('\r'),
                        'U' | 'L' => {
                            flush(&mut lit, &mut parts);
                            parts.push(Part::Begin(if n == 'U' { Case::Upper } else { Case::Lower }));
                        }
                        'E' => {
                            flush(&mut lit, &mut parts);
                            parts.push(Part::End);
                        }
                        '\\' | '$' | '%' => lit.push(n),
                        other => {
                            lit.push('\\');
                            lit.push(other);
                        }
                    }
                }
                '$' if i + 1 < chars.len() => {
                    let n = chars[i + 1];
                    if n.is_ascii_digit() {
                        flush(&mut lit, &mut parts);
                        parts.push(Part::Group(n as usize - '0' as usize));
                        i += 2;
                    } else if n == '&' {
                        flush(&mut lit, &mut parts);
                        parts.push(Part::Group(0));
                        i += 2;
                    } else if n == '$' {
                        lit.push('$');
                        i += 2;
                    } else if n == '{' {
                        let close = chars[i + 2..].iter().position(|&c| c == '}');
                        let num = close.and_then(|p| {
                            chars[i + 2..i + 2 + p].iter().collect::<String>().parse::<usize>().ok().map(|v| (v, p))
                        });
                        if let Some((v, p)) = num {
                            flush(&mut lit, &mut parts);
                            parts.push(Part::Group(v));
                            i += 3 + p;
                        } else {
                            lit.push('$');
                            i += 1;
                        }
                    } else {
                        lit.push('$');
                        i += 1;
                    }
                }
                '%' if placeholders => {
                    let close = chars[i + 1..].iter().position(|&c| c == '%');
                    let part = close.and_then(|p| {
                        let body: String = chars[i + 1..i + 1 + p].iter().collect();
                        parse_placeholder(&body).map(|part| (part, p))
                    });
                    if let Some((part, p)) = part {
                        flush(&mut lit, &mut parts);
                        parts.push(part);
                        i += 2 + p;
                    } else {
                        lit.push('%');
                        i += 1;
                    }
                }
                other => {
                    lit.push(other);
                    i += 1;
                }
            }
        }
        flush(&mut lit, &mut parts);
        Self { parts, adapt_case: false }
    }

    /// Expand for one match (also the preview hook: `expand(match) -> String`).
    pub fn expand(&self, m: &MatchCtx) -> String {
        let mut out = String::new();
        let mut stack: Vec<Case> = Vec::new();
        let push = |out: &mut String, stack: &[Case], s: &str| match stack.last() {
            Some(Case::Upper) => out.push_str(&s.to_uppercase()),
            Some(Case::Lower) => out.push_str(&s.to_lowercase()),
            None => out.push_str(s),
        };
        for part in &self.parts {
            match part {
                Part::Lit(s) => push(&mut out, &stack, s),
                Part::Group(n) => push(&mut out, &stack, (m.group)(*n).unwrap_or("")),
                Part::Line { offset, pad } => {
                    let v = m.line as i64 + offset;
                    let mut s = v.to_string();
                    if let Some((ch, w)) = pad {
                        while s.chars().count() < *w {
                            s.insert(0, *ch);
                        }
                    }
                    push(&mut out, &stack, &s);
                }
                Part::Begin(c) => stack.push(*c),
                Part::End => {
                    stack.pop();
                }
            }
        }
        if self.adapt_case {
            return adapt_case((m.group)(0).unwrap_or(""), &out);
        }
        out
    }
}

fn parse_placeholder(body: &str) -> Option<Part> {
    if body == "MATCH" {
        return Some(Part::Group(0));
    }
    if let Some(n) = body.strip_prefix("GROUP") {
        return n.parse().ok().map(Part::Group);
    }
    let rest = body.strip_prefix("LINE")?;
    let (arith, pad) = match rest.split_once(':') {
        Some((a, p)) => (a, Some(p)),
        None => (rest, None),
    };
    let offset = if arith.is_empty() { 0 } else { arith.parse::<i64>().ok()? };
    let pad = match pad {
        None => None,
        Some(p) => {
            let mut it = p.chars();
            let ch = match it.next()? {
                'Z' => '0',
                'S' => ' ',
                _ => return None,
            };
            Some((ch, it.as_str().parse::<usize>().ok()?))
        }
    };
    Some(Part::Line { offset, pad })
}

/// Map the case pattern of `matched` onto `replacement`:
/// ALL UPPER -> upper, all lower -> lower, Capitalised -> capitalised, else as is.
pub fn adapt_case(matched: &str, replacement: &str) -> String {
    let letters: Vec<char> = matched.chars().filter(|c| c.is_alphabetic()).collect();
    if letters.is_empty() {
        return replacement.to_owned();
    }
    let upper = |c: &char| c.is_uppercase();
    if letters.iter().all(|c| !c.is_lowercase()) && letters.len() > 1 {
        replacement.to_uppercase()
    } else if letters.iter().all(|c| !c.is_uppercase()) {
        replacement.to_lowercase()
    } else if upper(&letters[0]) && letters[1..].iter().all(|c| !c.is_uppercase()) {
        let mut cs = replacement.chars();
        match cs.next() {
            Some(f) => f.to_uppercase().collect::<String>() + &cs.as_str().to_lowercase(),
            None => String::new(),
        }
    } else {
        replacement.to_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(t: &Template, groups: &[&str], line: usize) -> String {
        let g = |n: usize| groups.get(n).copied();
        t.expand(&MatchCtx { group: &g, line })
    }

    #[test]
    fn upper_swap_example() {
        let t = Template::parse(r"\U\2\E \1");
        assert_eq!(run(&t, &["ab cd", "ab", "cd"], 1), "CD ab");
    }

    #[test]
    fn stray_end_is_noop_and_dollar_zero_is_match() {
        assert_eq!(run(&Template::parse(r"a\Eb"), &["m"], 1), "ab");
        assert_eq!(run(&Template::parse("[$0|$&|\\0]"), &["hit"], 1), "[hit|hit|hit]");
    }

    #[test]
    fn nested_case_scopes() {
        let t = Template::parse(r"\Ua\Lb\Ec\Ed");
        assert_eq!(run(&t, &["x"], 1), "AbCd");
    }

    #[test]
    fn escapes_and_literals() {
        let t = Template::parse(r"a\tb\nc\\d\$e$$f\q");
        assert_eq!(run(&t, &["x"], 1), "a\tb\nc\\d$e$f\\q");
        assert_eq!(run(&Template::parse("${1}0"), &["x", "9"], 1), "90");
        assert_eq!(run(&Template::parse("$x$"), &["m"], 1), "$x$");
    }

    #[test]
    fn missing_group_is_empty() {
        assert_eq!(run(&Template::parse(r"<\3>"), &["m"], 1), "<>");
    }

    #[test]
    fn placeholders_line_padding() {
        let t = Template::parse_with_placeholders("%LINE:Z4%|%LINE+1%|%MATCH%|%GROUP1%|100%");
        assert_eq!(run(&t, &["m", "g"], 7), "0007|8|m|g|100%");
        assert!(t.needs_line());
        assert!(!Template::parse("x").needs_line());
        // placeholders are inert unless enabled
        assert_eq!(run(&Template::parse("%MATCH%"), &["m"], 1), "%MATCH%");
    }

    #[test]
    fn adapt_case_patterns() {
        assert_eq!(adapt_case("Foo", "bar"), "Bar");
        assert_eq!(adapt_case("FOO", "bar"), "BAR");
        assert_eq!(adapt_case("foo", "BAR"), "bar");
        assert_eq!(adapt_case("fOo", "Bar"), "Bar");
        let t = Template::parse("bar").with_adapt_case(true);
        assert_eq!(run(&t, &["Foo"], 1), "Bar");
    }
}
