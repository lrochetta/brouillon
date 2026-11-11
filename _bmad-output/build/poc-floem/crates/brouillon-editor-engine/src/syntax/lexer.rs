//! Line-by-line, table-driven lexer. Each line is lexed from a start `State`
//! and yields spans plus the state for the next line (multi-line comments,
//! strings, heredocs, markup tags and nested languages all live in `State`).

use super::def::*;
use super::registry::Registry;
use crate::highlight::{HighlightSpan, SyntaxKind};
use std::sync::Arc;

/// Which rule set is active on the current line.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Mode {
    /// The document's own rules (languages without markup).
    Code,
    /// Markup text/tags (HTML/XML, or the `host` of the document language).
    Markup,
    /// Raw-text element body (`<script>`, `<style>`) lexed with another language.
    Embedded { def: u16, raw: u8 },
    /// Script region of a hosted language (PHP between `<?php` and `?>`);
    /// `ret` is the embedded region to return to, `None` = markup.
    Script { ret: Option<(u16, u8)> },
}

/// Multi-line construct in progress at a line boundary.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Inner {
    None,
    Block { rule: u8, depth: u16 },
    Str { rule: u8 },
    Heredoc { id: Arc<str>, indented: bool },
    MComment,
    MCdata,
    MPi,
    MTag { raw: u8, closing: bool },
    MAttrStr { q: u8, raw: u8, closing: bool },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct State {
    pub mode: Mode,
    pub inner: Inner,
}

impl State {
    /// State at the first line of a document of language `root`.
    pub fn initial(reg: &Registry, root: &Syntax) -> State {
        let markup = root.markup.is_some() || root.host.map_or(false, |h| reg.def(h).markup.is_some());
        State { mode: if markup { Mode::Markup } else { Mode::Code }, inner: Inner::None }
    }
}

struct Out<'a> {
    v: &'a mut Vec<HighlightSpan>,
    base: usize,
}

impl Out<'_> {
    #[inline]
    fn push(&mut self, s: usize, e: usize, kind: SyntaxKind) {
        if s >= e || kind == SyntaxKind::Default {
            return;
        }
        let (a, b) = (self.base + s, self.base + e);
        if let Some(l) = self.v.last_mut() {
            if l.kind == kind && l.byte_range.end == a {
                l.byte_range.end = b;
                return;
            }
        }
        self.v.push(HighlightSpan { byte_range: a..b, kind });
    }
}

#[inline]
fn at(b: &[u8], pos: usize, tok: &[u8]) -> bool {
    b.len() >= pos + tok.len() && &b[pos..pos + tok.len()] == tok
}

#[inline]
fn at_ci(b: &[u8], pos: usize, tok: &[u8]) -> bool {
    b.len() >= pos + tok.len() && b[pos..pos + tok.len()].eq_ignore_ascii_case(tok)
}

fn find(b: &[u8], from: usize, tok: &[u8]) -> Option<usize> {
    if tok.is_empty() || from > b.len() {
        return None;
    }
    let first = tok[0];
    let mut i = from;
    while i + tok.len() <= b.len() {
        if b[i] == first && &b[i..i + tok.len()] == tok {
            return Some(i);
        }
        i += 1;
    }
    None
}

fn find_ci(b: &[u8], from: usize, tok: &[u8]) -> Option<usize> {
    if tok.is_empty() || from > b.len() {
        return None;
    }
    let mut i = from;
    while i + tok.len() <= b.len() {
        if b[i..i + tok.len()].eq_ignore_ascii_case(tok) {
            return Some(i);
        }
        i += 1;
    }
    None
}

/// Lex one line (without its line terminator). `base` is the absolute byte
/// offset of the line start; spans are appended to `out` in order.
pub fn lex_line(reg: &Registry, root: &Syntax, line: &str, base: usize, st: &mut State, out: &mut Vec<HighlightSpan>) {
    let len = line.len();
    if len == 0 {
        return;
    }
    let mut o = Out { v: out, base };
    let md: Option<&Syntax> = if root.markup.is_some() { Some(root) } else { root.host.map(|h| reg.def(h)) };
    let mut pos = 0usize;
    let mut guard = 0usize;
    while pos < len {
        guard += 1;
        if guard > len * 4 + 16 {
            break; // defensive: never loop forever on a malformed definition
        }
        match st.mode.clone() {
            Mode::Code => {
                lex_code(root, line, pos, len, &mut st.inner, None, &mut o);
                pos = len;
            }
            Mode::Markup => match md {
                Some(m) => pos = lex_markup(root, m, line, pos, st, &mut o),
                None => {
                    st.mode = Mode::Code;
                }
            },
            Mode::Embedded { def, raw } => {
                let ed = reg.def(def);
                let tag = md
                    .and_then(|m| m.markup.as_ref())
                    .and_then(|mk| mk.raw_text.get(raw as usize))
                    .map(|r| r.tag.as_str())
                    .unwrap_or("script");
                let lb = line.as_bytes();
                let end_tag = format!("</{tag}");
                let mut seg_end = find_ci(lb, pos, end_tag.as_bytes()).unwrap_or(len);
                let mut php: Option<usize> = None;
                if let Some(sc) = &root.script {
                    for bt in &sc.begin {
                        if let Some(p) = find(&lb[..seg_end], pos, bt) {
                            if p < seg_end {
                                seg_end = p;
                                php = Some(bt.len());
                            }
                        }
                    }
                }
                lex_code(ed, line, pos, seg_end, &mut st.inner, None, &mut o);
                if seg_end < len {
                    st.inner = Inner::None;
                    match php {
                        Some(n) => {
                            o.push(seg_end, seg_end + n, SyntaxKind::Preprocessor);
                            st.mode = Mode::Script { ret: Some((def, raw)) };
                            pos = seg_end + n;
                        }
                        None => {
                            st.mode = Mode::Markup;
                            pos = seg_end;
                        }
                    }
                } else {
                    pos = len;
                }
            }
            Mode::Script { ret } => {
                let end = root.script.as_ref().map(|s| s.end.clone());
                let stop = lex_code(root, line, pos, len, &mut st.inner, end.as_deref(), &mut o);
                if stop < len {
                    let n = end.as_ref().map_or(0, |e| e.len());
                    o.push(stop, stop + n, SyntaxKind::Preprocessor);
                    st.inner = Inner::None;
                    st.mode = match ret {
                        Some((def, raw)) => Mode::Embedded { def, raw },
                        None => Mode::Markup,
                    };
                    pos = stop + n;
                } else {
                    pos = len;
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Code lexer
// ---------------------------------------------------------------------------

enum StrEnd {
    Closed(usize),
    /// Reached end of line; `cont` = ended on a lone escape char (line continuation).
    Open { cont: bool },
}

fn utf8_len(b: u8) -> usize {
    match b {
        0..=0x7f => 1,
        0xc0..=0xdf => 2,
        0xe0..=0xef => 3,
        _ => 4,
    }
}

/// Length in bytes of the escape sequence starting at `p` (the escape char).
fn escape_len(b: &[u8], p: usize) -> usize {
    if p + 1 >= b.len() {
        return 1;
    }
    let next = b[p + 1];
    let mut n = 1 + utf8_len(next).min(b.len() - p - 1);
    let hex = |c: u8| c.is_ascii_hexdigit();
    match next {
        b'x' => {
            let mut k = 0;
            while k < 2 && p + n < b.len() && hex(b[p + n]) {
                n += 1;
                k += 1;
            }
        }
        b'u' | b'U' => {
            if p + n < b.len() && b[p + n] == b'{' {
                while p + n < b.len() && b[p + n] != b'}' {
                    n += 1;
                }
                if p + n < b.len() {
                    n += 1;
                }
            } else {
                let mut k = 0;
                while k < 8 && p + n < b.len() && hex(b[p + n]) {
                    n += 1;
                    k += 1;
                }
            }
        }
        b'0'..=b'7' => {
            let mut k = 0;
            while k < 2 && p + n < b.len() && (b'0'..=b'7').contains(&b[p + n]) {
                n += 1;
                k += 1;
            }
        }
        _ => {}
    }
    n
}

/// Scan a string body from `from`; emits String/Escape spans.
fn scan_string(b: &[u8], from: usize, seg_start: usize, r: &StrRule, kind: SyntaxKind, o: &mut Out) -> StrEnd {
    let mut p = from;
    let mut seg = seg_start;
    let close = &r.close[..];
    while p < b.len() {
        let c = b[p];
        if Some(c) == r.escape {
            let e = escape_len(b, p);
            if r.hl_escapes {
                o.push(seg, p, kind);
                o.push(p, p + e, SyntaxKind::Escape);
                seg = p + e;
            }
            if e == 1 && p + 1 == b.len() {
                o.push(seg, b.len(), kind);
                return StrEnd::Open { cont: true };
            }
            p += e;
            continue;
        }
        if c == close[0] && at(b, p, close) {
            if r.doubled && at(b, p + close.len(), close) {
                p += close.len() * 2;
                continue;
            }
            p += close.len();
            o.push(seg, p, kind);
            return StrEnd::Closed(p);
        }
        p += 1;
    }
    o.push(seg, b.len(), kind);
    StrEnd::Open { cont: false }
}

/// Position just after the closing delimiter, without emitting anything.
fn find_string_close(b: &[u8], from: usize, r: &StrRule) -> Option<usize> {
    let close = &r.close[..];
    let mut p = from;
    while p < b.len() {
        let c = b[p];
        if Some(c) == r.escape {
            p += escape_len(b, p);
            continue;
        }
        if c == close[0] && at(b, p, close) {
            if r.doubled && at(b, p + close.len(), close) {
                p += close.len() * 2;
                continue;
            }
            return Some(p + close.len());
        }
        p += 1;
    }
    None
}

/// Scan block-comment text from `from`. Returns (end position, closed).
fn scan_block(b: &[u8], from: usize, r: &BlockRule, depth: &mut u16) -> (usize, bool) {
    let mut p = from;
    loop {
        let c = find(b, p, &r.close);
        let o = if r.nested { find(b, p, &r.open) } else { None };
        match (o, c) {
            (Some(o), Some(c)) if o < c => {
                *depth += 1;
                p = o + r.open.len();
            }
            (Some(o), None) => {
                *depth += 1;
                p = o + r.open.len();
            }
            (_, Some(c)) => {
                p = c + r.close.len();
                if *depth == 0 {
                    return (p, true);
                }
                *depth -= 1;
            }
            (None, None) => return (b.len(), false),
        }
    }
}

fn scan_number(b: &[u8], start: usize, style: NumStyle) -> usize {
    let mut p = start;
    let n = b.len();
    let digit = |c: u8| c.is_ascii_digit() || c == b'_';
    if b[p] == b'0' && p + 1 < n && matches!(b[p + 1], b'x' | b'X') {
        p += 2;
        while p < n && (b[p].is_ascii_hexdigit() || b[p] == b'_') {
            p += 1;
        }
    } else if b[p] == b'0' && p + 1 < n && matches!(b[p + 1], b'b' | b'B' | b'o' | b'O') {
        p += 2;
        while p < n && digit(b[p]) {
            p += 1;
        }
    } else {
        while p < n && digit(b[p]) {
            p += 1;
        }
        if p + 1 < n && b[p] == b'.' && b[p + 1].is_ascii_digit() {
            p += 1;
            while p < n && digit(b[p]) {
                p += 1;
            }
        } else if b[start] == b'.' {
            // ".5": the dot was the start; digits follow
            p = start + 1;
            while p < n && digit(b[p]) {
                p += 1;
            }
        }
        if p < n && matches!(b[p], b'e' | b'E') {
            let mut q = p + 1;
            if q < n && matches!(b[q], b'+' | b'-') {
                q += 1;
            }
            if q < n && b[q].is_ascii_digit() {
                while q < n && b[q].is_ascii_digit() {
                    q += 1;
                }
                p = q;
            }
        }
    }
    match style {
        NumStyle::Css => {
            while p < n && (b[p].is_ascii_alphabetic() || b[p] == b'%') {
                p += 1;
            }
        }
        _ => {
            while p < n && matches!(b[p], b'u' | b'U' | b'l' | b'L' | b'f' | b'F' | b'd' | b'D' | b'n' | b'm' | b'M') {
                p += 1;
            }
        }
    }
    p
}

fn next_nonspace(b: &[u8], from: usize) -> Option<usize> {
    let mut i = from;
    while i < b.len() && (b[i] == b' ' || b[i] == b'\t') {
        i += 1;
    }
    if i < b.len() {
        Some(i)
    } else {
        None
    }
}

fn regex_allowed(b: &[u8], pos: usize) -> bool {
    let mut i = pos;
    while i > 0 && (b[i - 1] == b' ' || b[i - 1] == b'\t') {
        i -= 1;
    }
    if i == 0 {
        return true;
    }
    let c = b[i - 1];
    if b"(,=:[!&|?{};+-*%<>~^".contains(&c) {
        return true;
    }
    // identifier before: allow after a few keywords
    let mut s = i;
    while s > 0 && (b[s - 1].is_ascii_alphanumeric() || b[s - 1] == b'_') {
        s -= 1;
    }
    matches!(&b[s..i], b"return" | b"typeof" | b"case" | b"in" | b"of" | b"delete" | b"void" | b"throw" | b"else")
}

/// Scan `/.../flags`; returns end or None if not closed on this line.
fn scan_regex(b: &[u8], start: usize) -> Option<usize> {
    let mut p = start + 1;
    let mut class = false;
    while p < b.len() {
        match b[p] {
            b'\\' => p += 1,
            b'[' => class = true,
            b']' => class = false,
            b'/' if !class => {
                p += 1;
                while p < b.len() && b[p].is_ascii_alphabetic() {
                    p += 1;
                }
                return Some(p);
            }
            _ => {}
        }
        p += 1;
    }
    None
}

fn heredoc_terminates(line: &[u8], id: &str, indented: bool, word_cont: &[bool; 256]) -> Option<(usize, usize)> {
    let mut s = 0;
    if indented {
        while s < line.len() && (line[s] == b' ' || line[s] == b'\t') {
            s += 1;
        }
    }
    let idb = id.as_bytes();
    if !at(line, s, idb) {
        return None;
    }
    let e = s + idb.len();
    if indented {
        if e < line.len() && word_cont[line[e] as usize] {
            return None;
        }
    } else if e != line.len() {
        return None;
    }
    Some((s, e))
}

/// Lex `line[start..end]` with language `d`. Returns the position where it
/// stopped: `end`, or the index of `script_end` when that token was found.
fn lex_code(
    d: &Syntax,
    line: &str,
    start: usize,
    end: usize,
    inner: &mut Inner,
    script_end: Option<&[u8]>,
    o: &mut Out,
) -> usize {
    let b = &line.as_bytes()[..end];
    let mut pos = start;
    let mut at_line_start = b[..pos].iter().all(|c| *c == b' ' || *c == b'\t');
    let mut first_token = at_line_start;
    let mut pending_heredoc: Option<(Arc<str>, bool)> = None;

    // Resume a multi-line construct.
    match inner.clone() {
        Inner::Block { rule, mut depth } => {
            let r = &d.blocks[rule as usize];
            let (np, closed) = scan_block(b, pos, r, &mut depth);
            o.push(pos, np, SyntaxKind::Comment);
            pos = np;
            at_line_start = false;
            first_token = false;
            if closed {
                *inner = Inner::None;
            } else {
                *inner = Inner::Block { rule, depth };
                return end;
            }
        }
        Inner::Str { rule } => {
            let r = &d.strings[rule as usize];
            match scan_string(b, pos, pos, r, r.kind, o) {
                StrEnd::Closed(np) => {
                    pos = np;
                    *inner = Inner::None;
                    at_line_start = false;
                    first_token = false;
                }
                StrEnd::Open { cont } => {
                    if !(r.multiline || cont) {
                        *inner = Inner::None;
                    }
                    return end;
                }
            }
        }
        Inner::Heredoc { id, indented } => {
            match heredoc_terminates(b, &id, indented, &d.word_cont) {
                Some((s, e)) => {
                    o.push(s, e, SyntaxKind::Preprocessor);
                    *inner = Inner::None;
                    pos = e;
                    at_line_start = false;
                    first_token = false;
                }
                None => {
                    o.push(0, end, SyntaxKind::String);
                    return end;
                }
            }
        }
        _ => *inner = Inner::None,
    }

    while pos < end {
        let c = b[pos];
        if c == b' ' || c == b'\t' || c == b'\r' {
            pos += 1;
            continue;
        }
        if let Some(se) = script_end {
            if at(b, pos, se) {
                finish(inner, pending_heredoc);
                return pos;
            }
        }
        if pos == 0 && !d.line_start.is_empty() {
            if let Some((_, k)) = d.line_start.iter().find(|(p, _)| at(b, 0, p)) {
                o.push(0, end, *k);
                return end;
            }
        }
        let f = d.flags[c as usize];

        if f & F_BLOCK != 0 {
            if let Some(ri) = d.blocks.iter().position(|r| at(b, pos, &r.open)) {
                let r = &d.blocks[ri];
                let mut depth = 0u16;
                let (np, closed) = scan_block(b, pos + r.open.len(), r, &mut depth);
                o.push(pos, np, SyntaxKind::Comment);
                pos = np;
                at_line_start = false;
                first_token = false;
                if !closed {
                    *inner = Inner::Block { rule: ri as u8, depth };
                    return end;
                }
                continue;
            }
        }
        if f & F_LINE != 0 {
            let ci = !d.case_sensitive;
            if d.line_comments.iter().any(|r| {
                (!r.col0 || pos == 0) && (if ci { at_ci(b, pos, &r.tok) } else { at(b, pos, &r.tok) })
            }) {
                let ce = match script_end {
                    Some(se) => find(b, pos, se).unwrap_or(end),
                    None => end,
                };
                o.push(pos, ce, SyntaxKind::Comment);
                pos = ce;
                continue;
            }
        }
        if f & F_PRE != 0 && at_line_start {
            if let Some(pre) = &d.pre {
                if at(b, pos, &pre.start) {
                    let mut p = pos + pre.start.len();
                    while p < end && (b[p] == b' ' || b[p] == b'\t') {
                        p += 1;
                    }
                    let ws = p;
                    while p < end && d.word_cont[b[p] as usize] {
                        p += 1;
                    }
                    if p > ws {
                        o.push(pos, p, SyntaxKind::Preprocessor);
                        let word = &line[ws..p];
                        pos = p;
                        if pre.angle_include.iter().any(|w| w == word) {
                            if let Some(q) = next_nonspace(b, pos) {
                                if b[q] == b'<' {
                                    if let Some(e) = find(b, q, b">") {
                                        o.push(q, e + 1, SyntaxKind::String);
                                        pos = e + 1;
                                    }
                                }
                            }
                        }
                    } else {
                        o.push(pos, pos + pre.start.len(), SyntaxKind::Preprocessor);
                        pos += pre.start.len();
                    }
                    at_line_start = false;
                    first_token = false;
                    continue;
                }
            }
        }
        if f & F_HEREDOC != 0 && pending_heredoc.is_none() {
            if let Some(ldr) = d.heredoc.iter().find(|l| at(b, pos, l)) {
                let mut p = pos + ldr.len();
                let q = if p < end && (b[p] == b'\'' || b[p] == b'"') {
                    let q = b[p];
                    p += 1;
                    Some(q)
                } else {
                    None
                };
                let s = p;
                while p < end && d.word_cont[b[p] as usize] && b[p] < 0x80 {
                    p += 1;
                }
                let plain_shift = ldr.as_ref() == b"<<";
                let ok = p > s
                    && (b[s].is_ascii_alphabetic() || b[s] == b'_')
                    && (!plain_shift || q.is_some() || b[s].is_ascii_uppercase());
                if ok {
                    let id: Arc<str> = Arc::from(&line[s..p]);
                    if let Some(qc) = q {
                        if p < end && b[p] == qc {
                            p += 1;
                        }
                    }
                    let indented = ldr.last().map_or(false, |l| matches!(l, b'-' | b'~')) || ldr.as_ref() == b"<<<";
                    o.push(pos, p, SyntaxKind::Preprocessor);
                    pending_heredoc = Some((id, indented));
                    pos = p;
                    at_line_start = false;
                    first_token = false;
                    continue;
                }
            }
        }
        if f & F_STR != 0 {
            if let Some(ri) = d.strings.iter().position(|r| at(b, pos, &r.open)) {
                let r = &d.strings[ri];
                let body = pos + r.open.len();
                let closed_at = find_string_close(b, body, r);
                if !(r.check_closing && closed_at.is_none()) {
                    let mut kind = r.kind;
                    if let (Some(k), Some(ce)) = (&d.keys, closed_at) {
                        if k.strings && (!k.line_start_only || first_token) && key_follows(k, b, ce) {
                            kind = k.kind;
                        }
                    }
                    match scan_string(b, body, pos, r, kind, o) {
                        StrEnd::Closed(np) => {
                            pos = np;
                            at_line_start = false;
                            first_token = false;
                            continue;
                        }
                        StrEnd::Open { cont } => {
                            if r.multiline || cont {
                                *inner = Inner::Str { rule: ri as u8 };
                            }
                            finish(inner, pending_heredoc);
                            return end;
                        }
                    }
                }
            }
        }
        if c == b'/' && d.regex_literals && regex_allowed(b, pos) {
            if let Some(e) = scan_regex(b, pos) {
                o.push(pos, e, SyntaxKind::String);
                pos = e;
                at_line_start = false;
                first_token = false;
                continue;
            }
        }
        if c == b'-' && d.number == NumStyle::Css && pos + 1 < end && (b[pos + 1].is_ascii_digit() || (b[pos + 1] == b'.' && pos + 2 < end && b[pos + 2].is_ascii_digit())) && (pos == 0 || !d.word_cont[b[pos - 1] as usize]) {
            let e = scan_number(b, pos + 1, d.number);
            o.push(pos, e, SyntaxKind::Number);
            pos = e;
            at_line_start = false;
            first_token = false;
            continue;
        }
        if d.number != NumStyle::None && (c.is_ascii_digit() || (c == b'.' && pos + 1 < end && b[pos + 1].is_ascii_digit())) {
            let e = scan_number(b, pos, d.number);
            o.push(pos, e, SyntaxKind::Number);
            pos = e;
            at_line_start = false;
            first_token = false;
            continue;
        }
        if f & F_PREFIX != 0 {
            if let Some(rule) = d.prefixes.iter().find(|r| r.ch == c) {
                let prev_ok = pos == 0 || (!d.word_cont[b[pos - 1] as usize] && !(c == b':' && b[pos - 1] == b':'));
                // `$$var`, `@@var`: skip repeated sigils
                let mut sig = pos + 1;
                if matches!(c, b'$' | b'@') {
                    while sig < end && b[sig] == c {
                        sig += 1;
                    }
                }
                if prev_ok && sig < end {
                    let n = b[sig];
                    if d.word_cont[n as usize] {
                        let mut p = sig;
                        while p < end && d.word_cont[b[p] as usize] {
                            p += 1;
                        }
                        o.push(pos, p, rule.kind);
                        pos = p;
                        at_line_start = false;
                        first_token = false;
                        continue;
                    } else {
                        let close = match n {
                            b'{' if rule.braced => Some(b"}"),
                            b'(' if rule.parens => Some(b")"),
                            _ => None,
                        };
                        let end_at = match close {
                            Some(cl) => find(b, pos + 2, cl).map(|e| e + 1),
                            None if rule.parens && b"@<^?*%+".contains(&n) => Some(pos + 2),
                            None => None,
                        };
                        if let Some(e) = end_at {
                            o.push(pos, e, rule.kind);
                            pos = e;
                            at_line_start = false;
                            first_token = false;
                            continue;
                        }
                    }
                }
            }
        }
        if d.word_start[c as usize] {
            let mut p = pos + 1;
            while p < end && d.word_cont[b[p] as usize] {
                p += 1;
            }
            let word = line.get(pos..p).unwrap_or("");
            let mut kind = None;
            if let Some(k) = &d.keys {
                if (!k.line_start_only || first_token) && key_follows(k, b, p) {
                    kind = Some(k.kind);
                }
            }
            if kind.is_none() {
                kind = d.word_kind(word);
            }
            if kind.is_none() && d.functions {
                if let Some(q) = next_nonspace(b, p) {
                    if b[q] == b'(' {
                        kind = Some(SyntaxKind::Function);
                    }
                }
            }
            if let Some(k) = kind {
                o.push(pos, p, k);
            }
            pos = p;
            at_line_start = false;
            first_token = false;
            continue;
        }
        if f & F_OP != 0 {
            o.push(pos, pos + 1, SyntaxKind::Operator);
        }
        pos += 1;
        at_line_start = false;
        first_token = false;
    }
    finish(inner, pending_heredoc);
    end
}

#[inline]
fn finish(inner: &mut Inner, pending: Option<(Arc<str>, bool)>) {
    if let Some((id, indented)) = pending {
        if *inner == Inner::None {
            *inner = Inner::Heredoc { id, indented };
        }
    }
}

fn key_follows(k: &KeysRule, b: &[u8], after: usize) -> bool {
    match next_nonspace(b, after) {
        Some(q) => k.seps.contains(&b[q]) && !(b[q] == b':' && b.get(q + 1) == Some(&b':')),
        None => false,
    }
}

// ---------------------------------------------------------------------------
// Markup lexer
// ---------------------------------------------------------------------------

fn tag_char(c: u8) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, b'-' | b'_' | b':' | b'.')
}

fn lex_markup(root: &Syntax, md: &Syntax, line: &str, mut pos: usize, st: &mut State, o: &mut Out) -> usize {
    let m = md.markup.as_ref().expect("markup def");
    let b = line.as_bytes();
    let len = b.len();
    while pos < len {
        match st.inner.clone() {
            Inner::MComment => {
                match find(b, pos, b"-->") {
                    Some(e) => {
                        o.push(pos, e + 3, SyntaxKind::Comment);
                        pos = e + 3;
                        st.inner = Inner::None;
                    }
                    None => {
                        o.push(pos, len, SyntaxKind::Comment);
                        return len;
                    }
                }
            }
            Inner::MCdata => match find(b, pos, b"]]>") {
                Some(e) => {
                    o.push(pos, e + 3, SyntaxKind::String);
                    pos = e + 3;
                    st.inner = Inner::None;
                }
                None => {
                    o.push(pos, len, SyntaxKind::String);
                    return len;
                }
            },
            Inner::MPi => match find(b, pos, b"?>") {
                Some(e) => {
                    o.push(pos, e + 2, SyntaxKind::Preprocessor);
                    pos = e + 2;
                    st.inner = Inner::None;
                }
                None => {
                    o.push(pos, len, SyntaxKind::Preprocessor);
                    return len;
                }
            },
            Inner::MAttrStr { q, raw, closing } => match find(b, pos, &[q]) {
                Some(e) => {
                    o.push(pos, e + 1, SyntaxKind::String);
                    pos = e + 1;
                    st.inner = Inner::MTag { raw, closing };
                }
                None => {
                    o.push(pos, len, SyntaxKind::String);
                    return len;
                }
            },
            Inner::MTag { raw, closing } => {
                let c = b[pos];
                if c == b' ' || c == b'\t' || c == b'\r' {
                    pos += 1;
                } else if c == b'>' || (c == b'/' && pos + 1 < len && b[pos + 1] == b'>') {
                    let n = if c == b'>' { 1 } else { 2 };
                    o.push(pos, pos + n, SyntaxKind::Tag);
                    pos += n;
                    st.inner = Inner::None;
                    if raw > 0 && !closing && c == b'>' {
                        let r = &m.raw_text[raw as usize - 1];
                        if let Some(def) = r.def {
                            st.mode = Mode::Embedded { def, raw: raw - 1 };
                            return pos;
                        }
                    }
                } else if c == b'"' || c == b'\'' {
                    match find(b, pos + 1, &[c]) {
                        Some(e) => {
                            o.push(pos, e + 1, SyntaxKind::String);
                            pos = e + 1;
                        }
                        None => {
                            o.push(pos, len, SyntaxKind::String);
                            st.inner = Inner::MAttrStr { q: c, raw, closing };
                            return len;
                        }
                    }
                } else if let Some(n) = script_begin(root, b, pos) {
                    o.push(pos, pos + n, SyntaxKind::Preprocessor);
                    st.mode = Mode::Script { ret: None };
                    st.inner = Inner::None;
                    return pos + n;
                } else if tag_char(c) {
                    let mut p = pos + 1;
                    while p < len && tag_char(b[p]) {
                        p += 1;
                    }
                    o.push(pos, p, SyntaxKind::Attribute);
                    pos = p;
                } else if c == b'=' {
                    // unquoted attribute value: name=value
                    pos += 1;
                    let mut p = pos;
                    while p < len && (b[p] == b' ' || b[p] == b'\t') {
                        p += 1;
                    }
                    let vs = p;
                    while p < len && !matches!(b[p], b' ' | b'\t' | b'>' | b'"' | b'\'' | b'<') {
                        p += 1;
                    }
                    if p > vs && !matches!(b[vs], b'"' | b'\'') {
                        o.push(vs, p, SyntaxKind::String);
                        pos = p;
                    }
                } else {
                    pos += 1;
                }
            }
            Inner::None => {
                if let Some(n) = script_begin(root, b, pos) {
                    o.push(pos, pos + n, SyntaxKind::Preprocessor);
                    st.mode = Mode::Script { ret: None };
                    return pos + n;
                }
                let c = b[pos];
                if c == b'<' {
                    if at(b, pos, b"<!--") {
                        o.push(pos, pos + 4, SyntaxKind::Comment);
                        pos += 4;
                        st.inner = Inner::MComment;
                    } else if m.cdata && at(b, pos, b"<![CDATA[") {
                        o.push(pos, pos + 9, SyntaxKind::String);
                        pos += 9;
                        st.inner = Inner::MCdata;
                    } else if m.pi && at(b, pos, b"<?") {
                        o.push(pos, pos + 2, SyntaxKind::Preprocessor);
                        pos += 2;
                        st.inner = Inner::MPi;
                    } else if at(b, pos, b"<!") {
                        let mut p = pos + 2;
                        while p < len && b[p].is_ascii_alphabetic() {
                            p += 1;
                        }
                        o.push(pos, p, SyntaxKind::Preprocessor);
                        pos = p;
                        st.inner = Inner::MTag { raw: 0, closing: false };
                    } else {
                        let closing = pos + 1 < len && b[pos + 1] == b'/';
                        let ns = pos + if closing { 2 } else { 1 };
                        if ns < len && (b[ns].is_ascii_alphabetic() || b[ns] == b'_') {
                            let mut p = ns + 1;
                            while p < len && tag_char(b[p]) {
                                p += 1;
                            }
                            o.push(pos, p, SyntaxKind::Tag);
                            let name = line[ns..p].to_ascii_lowercase();
                            let raw = if closing {
                                0
                            } else {
                                m.raw_text.iter().position(|r| r.tag == name).map_or(0, |i| i as u8 + 1)
                            };
                            st.inner = Inner::MTag { raw, closing };
                            pos = p;
                        } else {
                            pos += 1;
                        }
                    }
                } else if c == b'&' && m.entities {
                    let mut p = pos + 1;
                    if p < len && b[p] == b'#' {
                        p += 1;
                    }
                    let s = p;
                    while p < len && b[p].is_ascii_alphanumeric() {
                        p += 1;
                    }
                    if p > s && p < len && b[p] == b';' {
                        o.push(pos, p + 1, SyntaxKind::Escape);
                        pos = p + 1;
                    } else {
                        pos += 1;
                    }
                } else {
                    pos += 1;
                }
            }
            _ => st.inner = Inner::None,
        }
    }
    pos
}

fn script_begin(root: &Syntax, b: &[u8], pos: usize) -> Option<usize> {
    let sc = root.script.as_ref()?;
    if b[pos] != b'<' {
        return None;
    }
    sc.begin.iter().find(|t| at_ci(b, pos, t)).map(|t| t.len())
}
