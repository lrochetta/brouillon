//! Goto-offset expressions: a bounded integer expression parser, no `eval`, no variables beyond a
//! fixed set of names.
//!
//! Grammar: `expr := term (('+'|'-') term)*`, `term := factor ('*' factor)*`,
//! `factor := number | 'end' | 'cur' | 'sel.start' | 'sel.end' | '(' expr ')'`. Numbers are decimal
//! (`1k`, `2m`, `1g` suffixes, 1024-based) or `0x` hex; `_` separators are allowed. A leading `+` or
//! `-` on the whole input means "relative to the cursor". Arithmetic is checked and the result must
//! lie in `0..=len`. Input length and nesting depth are capped.

use std::ops::Range;

#[derive(Clone, Debug, Default)]
pub struct ExprCtx {
    pub cursor: u64,
    pub sel: Option<Range<u64>>,
    /// Document length: `end`, and the upper bound of the result.
    pub len: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ExprError {
    Empty,
    TooLong,
    TooDeep,
    Syntax(String),
    Overflow,
    Negative,
    PastEnd(u64),
    NoSelection,
}

impl std::fmt::Display for ExprError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ExprError::Empty => write!(f, "Type an offset"),
            ExprError::TooLong => write!(f, "Expression too long"),
            ExprError::TooDeep => write!(f, "Too many nested parentheses"),
            ExprError::Syntax(s) => write!(f, "Cannot read: {s}"),
            ExprError::Overflow => write!(f, "Number too large"),
            ExprError::Negative => write!(f, "Before the start of the file"),
            ExprError::PastEnd(len) => write!(f, "Past the end of the file ({len} bytes)"),
            ExprError::NoSelection => write!(f, "No selection"),
        }
    }
}

pub const MAX_INPUT: usize = 256;
const MAX_DEPTH: usize = 16;

struct P<'a> {
    s: &'a [u8],
    i: usize,
    depth: usize,
    ctx: &'a ExprCtx,
}

impl P<'_> {
    fn ws(&mut self) {
        while self.s.get(self.i).is_some_and(|c| c.is_ascii_whitespace()) {
            self.i += 1;
        }
    }
    fn peek(&mut self) -> Option<u8> {
        self.ws();
        self.s.get(self.i).copied()
    }
    fn expr(&mut self) -> Result<i128, ExprError> {
        let mut v = self.term()?;
        while let Some(c @ (b'+' | b'-')) = self.peek() {
            self.i += 1;
            let r = self.term()?;
            v = if c == b'+' { v.checked_add(r) } else { v.checked_sub(r) }.ok_or(ExprError::Overflow)?;
        }
        Ok(v)
    }
    fn term(&mut self) -> Result<i128, ExprError> {
        let mut v = self.factor()?;
        while self.peek() == Some(b'*') {
            self.i += 1;
            let r = self.factor()?;
            v = v.checked_mul(r).ok_or(ExprError::Overflow)?;
            if v > u64::MAX as i128 * 4 || v < -(u64::MAX as i128) * 4 {
                return Err(ExprError::Overflow);
            }
        }
        Ok(v)
    }
    fn factor(&mut self) -> Result<i128, ExprError> {
        match self.peek() {
            None => Err(ExprError::Syntax("unexpected end".into())),
            Some(b'(') => {
                self.i += 1;
                self.depth += 1;
                if self.depth > MAX_DEPTH {
                    return Err(ExprError::TooDeep);
                }
                let v = self.expr()?;
                if self.peek() != Some(b')') {
                    return Err(ExprError::Syntax("missing )".into()));
                }
                self.i += 1;
                self.depth -= 1;
                Ok(v)
            }
            Some(c) if c.is_ascii_digit() => self.number(),
            Some(c) if c.is_ascii_alphabetic() => self.name(),
            Some(c) => Err(ExprError::Syntax(format!("unexpected '{}'", if c.is_ascii() { c as char } else { '?' }))),
        }
    }
    fn number(&mut self) -> Result<i128, ExprError> {
        let hex = self.s[self.i..].starts_with(b"0x") || self.s[self.i..].starts_with(b"0X");
        if hex {
            self.i += 2;
        }
        let ds = self.i;
        while self.s.get(self.i).is_some_and(|c| (if hex { c.is_ascii_hexdigit() } else { c.is_ascii_digit() }) || *c == b'_') {
            self.i += 1;
        }
        let digits: String = self.s[ds..self.i].iter().filter(|c| **c != b'_').map(|c| *c as char).collect();
        if digits.is_empty() {
            return Err(ExprError::Syntax("number".into()));
        }
        let mut v = i128::from_str_radix(&digits, if hex { 16 } else { 10 }).map_err(|_| ExprError::Overflow)?;
        if !hex {
            let mult: i128 = match self.s.get(self.i).map(|c| c.to_ascii_lowercase()) {
                Some(b'k') => 1 << 10,
                Some(b'm') => 1 << 20,
                Some(b'g') => 1 << 30,
                _ => 1,
            };
            if mult != 1 {
                self.i += 1;
                v = v.checked_mul(mult).ok_or(ExprError::Overflow)?;
            }
        }
        if v > u64::MAX as i128 {
            return Err(ExprError::Overflow);
        }
        if self.s.get(self.i).is_some_and(|c| c.is_ascii_alphanumeric()) {
            return Err(ExprError::Syntax("bad number".into()));
        }
        Ok(v)
    }
    fn name(&mut self) -> Result<i128, ExprError> {
        let st = self.i;
        while self.s.get(self.i).is_some_and(|c| c.is_ascii_alphabetic() || *c == b'.') {
            self.i += 1;
        }
        let w = std::str::from_utf8(&self.s[st..self.i]).unwrap_or("").to_ascii_lowercase();
        Ok(match w.as_str() {
            "end" => self.ctx.len as i128,
            "cur" | "cursor" => self.ctx.cursor as i128,
            "sel.start" => self.ctx.sel.as_ref().ok_or(ExprError::NoSelection)?.start as i128,
            "sel.end" => self.ctx.sel.as_ref().ok_or(ExprError::NoSelection)?.end as i128,
            _ => return Err(ExprError::Syntax(format!("unknown name '{w}'"))),
        })
    }
}

/// Evaluate a goto expression to an absolute offset in `0..=ctx.len`.
pub fn eval_offset(src: &str, ctx: &ExprCtx) -> Result<u64, ExprError> {
    let t = src.trim();
    if t.is_empty() {
        return Err(ExprError::Empty);
    }
    if t.len() > MAX_INPUT {
        return Err(ExprError::TooLong);
    }
    let (rel, body) = match t.as_bytes()[0] {
        b'+' => (Some(1i128), &t[1..]),
        b'-' => (Some(-1i128), &t[1..]),
        _ => (None, t),
    };
    let mut p = P { s: body.as_bytes(), i: 0, depth: 0, ctx };
    let v = p.expr()?;
    if p.peek().is_some() {
        return Err(ExprError::Syntax("unexpected character".into()));
    }
    let abs = match rel {
        Some(sign) => (ctx.cursor as i128).checked_add(sign * v).ok_or(ExprError::Overflow)?,
        None => v,
    };
    if abs < 0 {
        return Err(ExprError::Negative);
    }
    if abs > ctx.len as i128 {
        return Err(ExprError::PastEnd(ctx.len));
    }
    Ok(abs as u64)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx() -> ExprCtx {
        ExprCtx { cursor: 0x100, sel: Some(0x20..0x28), len: 0x10000 }
    }
    fn ev(s: &str) -> Result<u64, ExprError> {
        eval_offset(s, &ctx())
    }

    #[test]
    fn plain_numbers() {
        assert_eq!(ev("0"), Ok(0));
        assert_eq!(ev("1234"), Ok(1234));
        assert_eq!(ev("0x1F40"), Ok(0x1F40));
        assert_eq!(ev("0XfF"), Ok(255));
        assert_eq!(ev("1_000"), Ok(1000));
        assert_eq!(ev("2k"), Ok(2048));
        assert_eq!(ev("1K"), Ok(1024));
    }

    #[test]
    fn relative_to_cursor() {
        assert_eq!(ev("+0x20"), Ok(0x120));
        assert_eq!(ev("-16"), Ok(0xF0));
        assert_eq!(ev("-0x100"), Ok(0));
        assert_eq!(ev("-0x101"), Err(ExprError::Negative));
        assert_eq!(ev("+ 1k"), Ok(0x100 + 1024));
    }

    #[test]
    fn arithmetic_and_names() {
        assert_eq!(ev("0x10 + 4 * 2"), Ok(0x18));
        assert_eq!(ev("(0x10 + 4) * 2"), Ok(0x28));
        assert_eq!(ev("end - 8"), Ok(0xFFF8));
        assert_eq!(ev("sel.start + 4"), Ok(0x24));
        assert_eq!(ev("sel.end"), Ok(0x28));
        assert_eq!(ev("cur*2"), Ok(0x200));
        assert_eq!(ev("END"), Ok(0x10000));
        assert_eq!(eval_offset("sel.end", &ExprCtx { sel: None, ..ctx() }), Err(ExprError::NoSelection));
    }

    #[test]
    fn errors() {
        assert_eq!(ev(""), Err(ExprError::Empty));
        assert_eq!(ev("   "), Err(ExprError::Empty));
        assert!(matches!(ev("abc"), Err(ExprError::Syntax(_))));
        assert!(matches!(ev("1 +"), Err(ExprError::Syntax(_))));
        assert!(matches!(ev("(1"), Err(ExprError::Syntax(_))));
        assert!(matches!(ev("1 2"), Err(ExprError::Syntax(_))));
        assert!(matches!(ev("0x"), Err(ExprError::Syntax(_))));
        assert!(matches!(ev("12abc"), Err(ExprError::Syntax(_))));
        assert_eq!(ev("0x10001"), Err(ExprError::PastEnd(0x10000)));
        assert_eq!(ev("1 - 2"), Err(ExprError::Negative));
        assert_eq!(ev("0xFFFFFFFFFFFFFFFFF"), Err(ExprError::Overflow));
        assert_eq!(ev("0xFFFFFFFFFFFFFFFF * 0xFFFFFFFFFFFFFFFF * 0xFFFFFFFFFFFFFFFF"), Err(ExprError::Overflow));
        assert_eq!(ev(&"1".repeat(300)), Err(ExprError::TooLong));
        let deep = format!("{}1{}", "(".repeat(40), ")".repeat(40));
        assert_eq!(ev(&deep), Err(ExprError::TooDeep));
    }

    #[test]
    fn hostile_input_never_panics() {
        let alphabet: Vec<char> = "0123456789xXabcdefkmg+-*() .endcurslt_\u{e9}".chars().collect();
        let mut seed = 0x1234_5678_9abc_def0u64;
        for _ in 0..20_000 {
            seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            let n = (seed >> 60) as usize + 1;
            let mut s = String::new();
            let mut x = seed;
            for _ in 0..n {
                x = x.wrapping_mul(6364136223846793005).wrapping_add(1);
                s.push(alphabet[(x >> 33) as usize % alphabet.len()]);
            }
            let _ = eval_offset(&s, &ctx());
        }
    }
}
