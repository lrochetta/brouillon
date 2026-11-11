//! Search engine over a `crop::Rope` (Lot 3: BK-015..BK-018).
//!
//! Design:
//! - Streaming (BK-015). The regex crate needs a contiguous haystack, so the
//!   rope is never turned into one big `String` when it can be avoided:
//!   a pattern that cannot match a line break (`Matcher::spans_lines() ==
//!   false`) is run line by line, borrowing each line straight from the rope
//!   chunk (a copy only when a line straddles two chunks). Patterns that can
//!   cross lines (`\n`, `\R`, `\s`, `[^x]`, `(?s)`, `\A`, ...) fall back to ONE
//!   allocation of the searched range (never of the whole file when scoped).
//!   Cancellation is checked between lines; in the fallback path it is only
//!   checked before the (single) regex run.
//! - Options (BK-016). Normal / Extended / Regex modes, whole word, case,
//!   dot-matches-newline, free-spacing, multi-line `^`/`$` (default on, CRLF
//!   aware). Literal newlines and `\R` match CRLF, LF or CR.
//! - Replacement never mutates: it returns an `EditSet` (original offsets).
//!
//! `commands()` / `run()` are the registry entry points (see bottom).

use std::borrow::Cow;
use std::ops::Range;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use brouillon_core::contract::{CmdCtx, CmdOutcome, CommandSpec, EditSet, PanelId, Region, TextEdit};
use crop::{Rope, RopeSlice};
use regex::{Regex, RegexBuilder};

use crate::buffer::Buffer;

pub mod history;
pub mod live;
pub mod replace_tpl;
pub mod scope;

use replace_tpl::{MatchCtx, Template};

/// A single match in the buffer. Line and column are 1-based to match the
/// classic status-bar convention ("Ln 42, Col 7"). `col` is a *byte* offset
/// inside the line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Match {
    pub start_byte: usize,
    pub end_byte: usize,
    pub line: usize,
    pub col: usize,
}

/// Pattern could not be compiled.
#[derive(Debug)]
pub enum SearchError {
    /// `column` is 1-based, relative to the user's pattern when it can be
    /// determined (approximate if a `\R` / newline precedes the error).
    InvalidRegex { message: String, column: Option<usize> },
}

impl std::fmt::Display for SearchError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidRegex { message, column: Some(c) } => write!(f, "invalid regex at column {c}: {message}"),
            Self::InvalidRegex { message, column: None } => write!(f, "invalid regex: {message}"),
        }
    }
}

impl std::error::Error for SearchError {}

/// Returned when a [`CancelToken`] fired mid-search.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Cancelled;

/// Shared cancellation flag (clone it to the UI thread, call `cancel()`).
#[derive(Debug, Clone, Default)]
pub struct CancelToken(Arc<AtomicBool>);

impl CancelToken {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn cancel(&self) {
        self.0.store(true, Ordering::Relaxed);
    }
    pub fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::Relaxed)
    }
}

/// How the pattern text is interpreted.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SearchMode {
    /// Literal text.
    #[default]
    Normal,
    /// Literal text with `\t \n \r \0 \xHH \\` escapes.
    Extended,
    /// Regular expression.
    Regex,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SearchOptions {
    pub mode: SearchMode,
    pub match_case: bool,
    pub whole_word: bool,
    /// `.` also matches a line break (regex mode).
    pub dot_all: bool,
    /// Ignore pattern whitespace and `#` comments (regex mode).
    pub free_spacing: bool,
    /// `^` / `$` match at every line boundary (default on).
    pub multi_line: bool,
}

impl Default for SearchOptions {
    fn default() -> Self {
        Self {
            mode: SearchMode::Normal,
            match_case: true,
            whole_word: false,
            dot_all: false,
            free_spacing: false,
            multi_line: true,
        }
    }
}

/// A found range; `wrapped` is true when the search restarted from the other end.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Found {
    pub range: Range<usize>,
    pub wrapped: bool,
}

/// Result of a replace pass: edits in ORIGINAL offsets plus the match count.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Replacement {
    pub edits: EditSet,
    pub count: usize,
}

const NL_ALT: &str = r"(?:\r\n|\n|\r)";

/// A compiled search pattern.
#[derive(Debug)]
pub struct Matcher {
    re: Regex,
    spans_lines: bool,
}

impl Matcher {
    pub fn new(pattern: &str, opts: &SearchOptions) -> Result<Self, SearchError> {
        let (core, prefix_len) = match opts.mode {
            SearchMode::Normal => (literal_pattern(pattern), 0),
            SearchMode::Extended => (literal_pattern(&unescape_extended(pattern)), 0),
            SearchMode::Regex => (translate_regex(pattern, opts.free_spacing), 0),
        };
        let mut prefix_len: usize = prefix_len;
        let mut full = core.clone();
        if opts.whole_word {
            if opts.mode == SearchMode::Regex {
                full = format!(r"\b(?:{core})\b");
                prefix_len = 5;
            } else {
                let w = |c: Option<char>| c.is_some_and(|c| c.is_alphanumeric() || c == '_');
                let raw = if opts.mode == SearchMode::Extended { unescape_extended(pattern) } else { pattern.to_owned() };
                let (lead, trail) = (w(raw.chars().next()), w(raw.chars().next_back()));
                full = format!("{}{}{}", if lead { r"\b" } else { "" }, core, if trail { r"\b" } else { "" });
                prefix_len = if lead { 2 } else { 0 };
            }
        }
        let spans_lines = opts.dot_all || pattern_spans_lines(&full);
        let re = RegexBuilder::new(&full)
            .case_insensitive(!opts.match_case)
            .multi_line(opts.multi_line)
            .dot_matches_new_line(opts.dot_all)
            .ignore_whitespace(opts.free_spacing && opts.mode == SearchMode::Regex)
            .crlf(true)
            .build()
            .map_err(|e| invalid_pattern(&e, prefix_len))?;
        Ok(Self { re, spans_lines })
    }

    /// The compiled regex (for slice-based scanning in [`live`]).
    pub fn regex(&self) -> &Regex {
        &self.re
    }

    /// True when a match may cross line boundaries (full-range fallback path).
    pub fn spans_lines(&self) -> bool {
        self.spans_lines
    }

    /// Feed `f(text, base)` the searchable units of `range`: lines (streaming)
    /// or the whole range (fallback). `f` returns false to stop early.
    /// `Ok(true)` = exhausted, `Ok(false)` = stopped by `f`.
    fn walk(
        &self,
        rope: &Rope,
        range: Range<usize>,
        cancel: &CancelToken,
        f: &mut dyn FnMut(&str, usize) -> bool,
    ) -> Result<bool, Cancelled> {
        let range = snap_range(rope, range);
        if cancel.is_cancelled() {
            return Err(Cancelled);
        }
        if range.is_empty() {
            return Ok(f("", range.start));
        }
        let slice = rope.byte_slice(range.clone());
        if self.spans_lines {
            return Ok(f(&slice_text(slice), range.start));
        }
        let mut base = range.start;
        for raw in slice.raw_lines() {
            if cancel.is_cancelled() {
                return Err(Cancelled);
            }
            let text = slice_text(raw);
            let body = text.strip_suffix("\r\n").or_else(|| text.strip_suffix('\n')).unwrap_or(&text);
            if !f(body, base) {
                return Ok(false);
            }
            base += text.len();
        }
        Ok(true)
    }

    /// Number of matches inside `ranges` (BK-015 count).
    pub fn count(&self, rope: &Rope, ranges: &[Range<usize>], cancel: &CancelToken) -> Result<usize, Cancelled> {
        let mut n = 0;
        for r in normalize_ranges(ranges) {
            self.walk(rope, r, cancel, &mut |t, _| {
                n += self.re.find_iter(t).count();
                true
            })?;
        }
        Ok(n)
    }

    /// All match ranges (absolute byte offsets, ascending) inside `ranges`.
    pub fn find_all(&self, rope: &Rope, ranges: &[Range<usize>], cancel: &CancelToken) -> Result<Vec<Range<usize>>, Cancelled> {
        let mut out = Vec::new();
        for r in normalize_ranges(ranges) {
            self.walk(rope, r, cancel, &mut |t, base| {
                out.extend(self.re.find_iter(t).map(|m| base + m.start()..base + m.end()));
                true
            })?;
        }
        Ok(out)
    }

    fn first_in(&self, rope: &Rope, range: Range<usize>, cancel: &CancelToken) -> Result<Option<Range<usize>>, Cancelled> {
        let mut hit = None;
        self.walk(rope, range, cancel, &mut |t, base| {
            hit = self.re.find(t).map(|m| base + m.start()..base + m.end());
            hit.is_none()
        })?;
        Ok(hit)
    }

    /// First match starting at or after `from`; with `wrap`, restarts at 0.
    pub fn find_next(&self, rope: &Rope, from: usize, wrap: bool, cancel: &CancelToken) -> Result<Option<Found>, Cancelled> {
        let len = rope.byte_len();
        if let Some(range) = self.first_in(rope, from.min(len)..len, cancel)? {
            return Ok(Some(Found { range, wrapped: false }));
        }
        if !wrap {
            return Ok(None);
        }
        Ok(self.first_in(rope, 0..len, cancel)?.map(|range| Found { range, wrapped: true }))
    }

    fn last_in(&self, rope: &Rope, range: Range<usize>, cancel: &CancelToken) -> Result<Option<Range<usize>>, Cancelled> {
        let range = snap_range(rope, range);
        if self.spans_lines || range.is_empty() {
            let mut last = None;
            self.walk(rope, range, cancel, &mut |t, base| {
                last = self.re.find_iter(t).last().map(|m| base + m.start()..base + m.end());
                true
            })?;
            return Ok(last);
        }
        let mut end = range.end;
        for raw in rope.byte_slice(range).raw_lines().rev() {
            if cancel.is_cancelled() {
                return Err(Cancelled);
            }
            let text = slice_text(raw);
            let base = end - text.len();
            end = base;
            let body = text.strip_suffix("\r\n").or_else(|| text.strip_suffix('\n')).unwrap_or(&text);
            if let Some(m) = self.re.find_iter(body).last() {
                return Ok(Some(base + m.start()..base + m.end()));
            }
        }
        Ok(None)
    }

    /// Last match lying entirely before `before`; with `wrap`, restarts at the end.
    pub fn find_prev(&self, rope: &Rope, before: usize, wrap: bool, cancel: &CancelToken) -> Result<Option<Found>, Cancelled> {
        let len = rope.byte_len();
        if let Some(range) = self.last_in(rope, 0..before.min(len), cancel)? {
            return Ok(Some(Found { range, wrapped: false }));
        }
        if !wrap {
            return Ok(None);
        }
        Ok(self.last_in(rope, 0..len, cancel)?.map(|range| Found { range, wrapped: true }))
    }

    /// Build the edits replacing every match inside `ranges` with `tpl`.
    /// Matches never leave `ranges` (selection scope is safe).
    pub fn replace(&self, rope: &Rope, ranges: &[Range<usize>], tpl: &Template, cancel: &CancelToken) -> Result<Replacement, Cancelled> {
        let mut edits = Vec::new();
        let mut count = 0;
        for r in normalize_ranges(ranges) {
            self.walk(rope, r, cancel, &mut |t, base| {
                for caps in self.re.captures_iter(t) {
                    let m0 = caps.get(0).expect("group 0 always present");
                    let g = |n: usize| caps.get(n).map(|m| m.as_str());
                    let line = if tpl.needs_line() { rope.line_of_byte(base + m0.start()) + 1 } else { 0 };
                    let text = tpl.expand(&MatchCtx { group: &g, line });
                    count += 1;
                    edits.push(TextEdit::new(base + m0.start(), base + m0.end(), text));
                }
                true
            })?;
        }
        Ok(Replacement { edits: EditSet(edits), count })
    }
}

/// Convert a byte range into a 1-based line/col [`Match`].
pub fn to_match(rope: &Rope, range: Range<usize>) -> Match {
    let line_idx = rope.line_of_byte(range.start);
    Match {
        start_byte: range.start,
        end_byte: range.end,
        line: line_idx + 1,
        col: range.start - rope.byte_of_line(line_idx) + 1,
    }
}

/// [`to_match`] for any text source (rope or lazy snapshot).
pub fn to_match_hay(h: &dyn live::Hay, range: Range<usize>) -> Match {
    let line_idx = h.line_of_byte(range.start);
    Match { start_byte: range.start, end_byte: range.end, line: line_idx + 1, col: range.start - h.byte_of_line(line_idx) + 1 }
}

/// Whole identifier around `pos` (letters, digits, `_`), if any (BK-020).
pub fn word_range_at(rope: &Rope, pos: usize) -> Option<Range<usize>> {
    let pos = snap_range(rope, pos..pos).start;
    let line = rope.line_of_byte(pos);
    let line_start = rope.byte_of_line(line);
    let text = slice_text(rope.line(line));
    let rel = pos - line_start;
    let is_w = |c: char| c.is_alphanumeric() || c == '_';
    let start = text[..rel].char_indices().rev().take_while(|&(_, c)| is_w(c)).last().map_or(rel, |(i, _)| i);
    let end = rel + text[rel..].char_indices().find(|&(_, c)| !is_w(c)).map_or(text.len() - rel, |(i, _)| i);
    (start < end).then_some(line_start + start..line_start + end)
}

// ---------------------------------------------------------------- helpers

fn slice_text(s: RopeSlice<'_>) -> Cow<'_, str> {
    let mut chunks = s.chunks();
    match (chunks.next(), chunks.next()) {
        (None, _) => Cow::Borrowed(""),
        (Some(a), None) => Cow::Borrowed(a),
        (Some(a), Some(b)) => {
            let mut o = String::with_capacity(s.byte_len());
            o.push_str(a);
            o.push_str(b);
            chunks.for_each(|c| o.push_str(c));
            Cow::Owned(o)
        }
    }
}

fn snap_range(rope: &Rope, r: Range<usize>) -> Range<usize> {
    let len = rope.byte_len();
    let snap = |mut b: usize| {
        b = b.min(len);
        while !rope.is_char_boundary(b) {
            b -= 1;
        }
        b
    };
    let (s, e) = (snap(r.start), snap(r.end));
    s..e.max(s)
}

/// Sort and merge overlapping/touching ranges.
pub(crate) fn normalize_ranges(ranges: &[Range<usize>]) -> Vec<Range<usize>> {
    let mut v: Vec<Range<usize>> = ranges.to_vec();
    v.sort_by_key(|r| (r.start, r.end));
    let mut out: Vec<Range<usize>> = Vec::with_capacity(v.len());
    for r in v {
        match out.last_mut() {
            Some(last) if r.start <= last.end => last.end = last.end.max(r.end),
            _ => out.push(r),
        }
    }
    out
}

/// Literal text -> regex; every newline form becomes the CRLF/LF/CR alternation.
fn literal_pattern(text: &str) -> String {
    let mut out = String::new();
    let mut it = text.chars().peekable();
    while let Some(c) = it.next() {
        match c {
            '\r' => {
                if it.peek() == Some(&'\n') {
                    it.next();
                }
                out.push_str(NL_ALT);
            }
            '\n' => out.push_str(NL_ALT),
            c => out.push_str(&regex::escape(c.encode_utf8(&mut [0; 4]))),
        }
    }
    out
}

/// Extended mode: `\t \n \r \0 \xHH \\`; unknown escapes stay literal.
fn unescape_extended(p: &str) -> String {
    let c: Vec<char> = p.chars().collect();
    let mut out = String::new();
    let mut i = 0;
    while i < c.len() {
        if c[i] != '\\' || i + 1 >= c.len() {
            out.push(c[i]);
            i += 1;
            continue;
        }
        i += 1;
        match c[i] {
            't' => out.push('\t'),
            'n' => out.push('\n'),
            'r' => out.push('\r'),
            '0' => out.push('\0'),
            '\\' => out.push('\\'),
            'x' if i + 2 < c.len() + 0 && c[i + 1].is_ascii_hexdigit() && c[i + 2].is_ascii_hexdigit() => {
                let v = u8::from_str_radix(&c[i + 1..=i + 2].iter().collect::<String>(), 16).unwrap_or(0);
                out.push(v as char);
                i += 2;
            }
            other => {
                out.push('\\');
                out.push(other);
            }
        }
        i += 1;
    }
    out
}

/// Regex mode: `\R` and literal newline characters match CRLF/LF/CR.
fn translate_regex(p: &str, free_spacing: bool) -> String {
    let c: Vec<char> = p.chars().collect();
    let mut out = String::new();
    let (mut i, mut in_class) = (0, false);
    while i < c.len() {
        match c[i] {
            '\\' if i + 1 < c.len() => {
                if c[i + 1] == 'R' && !in_class {
                    out.push_str(NL_ALT);
                } else {
                    out.push('\\');
                    out.push(c[i + 1]);
                }
                i += 2;
                continue;
            }
            '[' => in_class = true,
            ']' => in_class = false,
            '\r' | '\n' if !in_class && !free_spacing => {
                if c[i] == '\r' && c.get(i + 1) == Some(&'\n') {
                    i += 1;
                }
                out.push_str(NL_ALT);
                i += 1;
                continue;
            }
            _ => {}
        }
        out.push(c[i]);
        i += 1;
    }
    out
}

/// Conservative: can the (translated) pattern match a line break or anchor
/// on whole-text boundaries? If unsure, say yes (the fallback is always correct).
fn pattern_spans_lines(p: &str) -> bool {
    let c: Vec<char> = p.chars().collect();
    let mut i = 0;
    while i < c.len() {
        match c[i] {
            '\\' if i + 1 < c.len() => {
                if "nrsWDAzZxuUpPv0RhHVN".contains(c[i + 1]) {
                    return true;
                }
                i += 2;
                continue;
            }
            '\n' | '\r' => return true,
            '[' if c.get(i + 1) == Some(&'^') => return true,
            '(' if c.get(i + 1) == Some(&'?') => {
                let flags: String = c[i + 2..].iter().take_while(|ch| ch.is_ascii_alphabetic() || **ch == '-').collect();
                if flags.split('-').next().is_some_and(|on| on.contains('s')) {
                    return true;
                }
            }
            _ => {}
        }
        i += 1;
    }
    false
}

fn invalid_pattern(e: &regex::Error, prefix_len: usize) -> SearchError {
    let text = e.to_string();
    let message = text
        .lines()
        .rev()
        .find_map(|l| l.strip_prefix("error: "))
        .map_or_else(|| text.clone(), str::to_owned);
    let column = text
        .lines()
        .find(|l| l.trim().starts_with('^') && l.trim().chars().all(|c| c == '^'))
        .and_then(|l| l.find('^'))
        .map(|pos| pos.saturating_sub(4).saturating_sub(prefix_len) + 1);
    SearchError::InvalidRegex { message, column }
}

// ----------------------------------------------------- legacy Search API

/// Back-compat facade (regex mode, case-sensitive unless `(?i)`), used by the
/// current find/replace dialog. New code should use [`Matcher`] directly.
#[derive(Debug)]
pub struct Search {
    matcher: Matcher,
    pattern_source: String,
    match_case: bool,
}

impl Search {
    pub fn new(pattern: &str) -> Result<Self, SearchError> {
        Self::with_options(pattern, &SearchOptions { mode: SearchMode::Regex, ..SearchOptions::default() })
    }

    pub fn with_options(pattern: &str, opts: &SearchOptions) -> Result<Self, SearchError> {
        Ok(Self {
            matcher: Matcher::new(pattern, opts)?,
            pattern_source: pattern.to_owned(),
            match_case: opts.match_case,
        })
    }

    pub fn matcher(&self) -> &Matcher {
        &self.matcher
    }

    /// Case sensitivity is whatever the options AND the pattern's own `(?i)` say.
    pub fn is_case_sensitive(&self) -> bool {
        self.match_case && !contains_case_insensitive_flag(&self.pattern_source)
    }

    /// First match at or after `from_byte` (no wrap-around).
    pub fn find_next(&self, buffer: &Buffer, from_byte: usize) -> Option<Match> {
        let rope = buffer.rope();
        if from_byte > rope.byte_len() {
            return None;
        }
        let found = self.matcher.find_next(rope, from_byte, false, &CancelToken::new()).ok()??;
        Some(to_match(rope, found.range))
    }

    /// All non-overlapping matches in left-to-right order.
    pub fn find_all<'a>(&'a self, buffer: &'a Buffer) -> impl Iterator<Item = Match> + 'a {
        let rope = buffer.rope();
        let all = self.matcher.find_all(rope, &[0..rope.byte_len()], &CancelToken::new()).unwrap_or_default();
        all.into_iter().map(move |r| to_match(rope, r))
    }

    pub fn count_matches(&self, buffer: &Buffer) -> usize {
        let rope = buffer.rope();
        self.matcher.count(rope, &[0..rope.byte_len()], &CancelToken::new()).unwrap_or(0)
    }

    /// `(new_text, count)`; does not mutate the buffer. Template syntax: see
    /// [`replace_tpl`].
    pub fn replace_all(&self, buffer: &Buffer, replacement: &str) -> (String, usize) {
        let rope = buffer.rope();
        let r = self
            .matcher
            .replace(rope, &[0..rope.byte_len()], &Template::parse(replacement), &CancelToken::new())
            .unwrap_or_default();
        let text = r.edits.apply_to_str(&rope.to_string()).unwrap_or_else(|_| rope.to_string());
        (text, r.count)
    }
}

// ------------------------------------------------------- registry wiring

/// Commands this module contributes to the registry.
pub fn commands() -> Vec<CommandSpec> {
    let spec = |id: &str, caption: &str, chord: Option<&str>| CommandSpec {
        id: id.into(),
        caption: caption.into(),
        category: "Search".into(),
        default_chord: chord.map(Into::into),
        when: None, ..Default::default()
    };
    vec![
        spec("search_open_find", "Search: Find / Replace", Some("ctrl+f")),
        spec("search_find_next", "Search: Find Next", Some("f3")),
        spec("search_find_prev", "Search: Find Previous", Some("shift+f3")),
        spec("search_count", "Search: Count Matches", None),
        spec("search_replace_all", "Search: Replace All", None),
    ]
}

/// Flags string: `c` match case, `w` whole word, `e` extended, `r` regex,
/// `s` dot-all, `x` free-spacing, `S` in selection, `n` no wrap.
fn parse_flags(f: &str) -> (SearchOptions, bool, bool) {
    let opts = SearchOptions {
        mode: if f.contains('r') {
            SearchMode::Regex
        } else if f.contains('e') {
            SearchMode::Extended
        } else {
            SearchMode::Normal
        },
        match_case: f.contains('c'),
        whole_word: f.contains('w'),
        dot_all: f.contains('s'),
        free_spacing: f.contains('x'),
        multi_line: true,
    };
    (opts, f.contains('S'), !f.contains('n'))
}

/// Run command `id`; `None` = not mine. Args: `[pattern, flags, replacement]`.
/// Failures (bad pattern, no match) come back as a message-only outcome.
pub fn run(id: &str, ctx: &CmdCtx) -> Option<CmdOutcome> {
    if id == "search_open_find" {
        return Some(CmdOutcome { open_panel: Some(PanelId("find".into())), ..CmdOutcome::default() });
    }
    if !matches!(id, "search_find_next" | "search_find_prev" | "search_count" | "search_replace_all") {
        return None;
    }
    let msg = |m: String| Some(CmdOutcome { message: Some(m), ..CmdOutcome::default() });
    let pattern = ctx.args.first().filter(|p| !p.is_empty());
    let Some(pattern) = pattern else { return msg("Search: empty pattern".into()) };
    let (opts, in_sel, wrap) = parse_flags(ctx.args.get(1).map_or("c", String::as_str));
    let matcher = match Matcher::new(pattern, &opts) {
        Ok(m) => m,
        Err(e) => return msg(e.to_string()),
    };
    let rope = Rope::from(ctx.text);
    let token = CancelToken::new();
    let sel: Vec<Range<usize>> = ctx.selection.iter().filter(|r| !r.is_caret()).map(|r| r.start()..r.end()).collect();
    let ranges = if in_sel && !sel.is_empty() { sel } else { vec![0..rope.byte_len()] };
    match id {
        "search_count" => {
            let n = matcher.count(&rope, &ranges, &token).unwrap_or(0);
            msg(format!("{n} match(es)"))
        }
        "search_replace_all" => {
            let tpl = Template::parse(ctx.args.get(2).map_or("", String::as_str));
            let r = matcher.replace(&rope, &ranges, &tpl, &token).unwrap_or_default();
            Some(CmdOutcome { edits: r.edits, message: Some(format!("{} replaced", r.count)), ..CmdOutcome::default() })
        }
        _ => {
            let prev = id == "search_find_prev";
            let caret = ctx.selection.last().map_or(0, |r| if prev { r.start() } else { r.end() });
            let found = if prev { matcher.find_prev(&rope, caret, wrap, &token) } else { matcher.find_next(&rope, caret, wrap, &token) };
            match found {
                Ok(Some(f)) => Some(CmdOutcome {
                    selection: Some(vec![Region { anchor: f.range.start, head: f.range.end }]),
                    message: f.wrapped.then(|| "Search wrapped".to_owned()),
                    ..CmdOutcome::default()
                }),
                _ => msg("Not found".into()),
            }
        }
    }
}

/// Heuristic: does the pattern source contain a `(?i)` inline flag that would
/// turn case-insensitivity on at the top level? Walks the source skipping
/// character classes (`[...]`) and escaped sequences (`\(`). Detects:
///   * `(?i)` standalone
///   * `(?i...)` combined flags (`i` anywhere in the flag chunk)
///   * `(?i:pat)` scoped — yes, that's also case-insensitive for the scope;
///     we conservatively report the whole search as case-insensitive even
///     though strictly only `pat` is. Cheap-but-correct enough for the
///     dialog "Match case" checkbox.
fn contains_case_insensitive_flag(pattern: &str) -> bool {
    let bytes = pattern.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        let b = bytes[i];
        if b == b'\\' {
            // Skip escaped char.
            i += 2;
            continue;
        }
        if b == b'[' {
            // Skip character class. Inside a class `\]` is literal.
            i += 1;
            while i < bytes.len() {
                if bytes[i] == b'\\' {
                    i += 2;
                    continue;
                }
                if bytes[i] == b']' {
                    i += 1;
                    break;
                }
                i += 1;
            }
            continue;
        }
        if b == b'(' && i + 1 < bytes.len() && bytes[i + 1] == b'?' {
            // Read flag chunk until `)` or `:` or `-`.
            let mut j = i + 2;
            while j < bytes.len() {
                let c = bytes[j];
                if c == b')' || c == b':' || c == b'-' {
                    return pattern[i + 2..j].contains('i');
                }
                if !c.is_ascii_alphabetic() {
                    break;
                }
                j += 1;
            }
        }
        i += 1;
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    const CRLF: &str = "Foo bar\r\nfoo-baz\r\nBar\r\n";

    fn rope(s: &str) -> Rope {
        Rope::from(s)
    }

    fn opts(mode: SearchMode) -> SearchOptions {
        SearchOptions { mode, ..SearchOptions::default() }
    }

    fn spans(pat: &str, o: &SearchOptions, text: &str) -> Vec<(usize, usize)> {
        let m = Matcher::new(pat, o).unwrap();
        let r = rope(text);
        m.find_all(&r, &[0..r.byte_len()], &CancelToken::new()).unwrap().into_iter().map(|x| (x.start, x.end)).collect()
    }

    #[test]
    fn option_table_over_crlf_fixture() {
        let base = opts(SearchMode::Normal);
        // case on / off
        assert_eq!(spans("foo", &base, CRLF), vec![(9, 12)]);
        assert_eq!(spans("foo", &SearchOptions { match_case: false, ..base }, CRLF), vec![(0, 3), (9, 12)]);
        // whole word: "foo" in "foo-baz" is a word, "ba" is not
        let ww = SearchOptions { whole_word: true, ..base };
        assert_eq!(spans("foo", &ww, CRLF), vec![(9, 12)]);
        assert!(spans("ba", &ww, CRLF).is_empty());
        assert_eq!(spans("ba", &base, CRLF).len(), 2);
        // regex multi-line anchors, CRLF aware
        let re = opts(SearchMode::Regex);
        assert_eq!(spans("^foo", &re, CRLF), vec![(9, 12)]);
        assert_eq!(spans("bar$", &re, CRLF), vec![(4, 7)]);
        // \A and \z are whole-text anchors
        assert_eq!(spans(r"\AFoo", &re, CRLF), vec![(0, 3)]);
        assert!(spans(r"\Afoo", &re, CRLF).is_empty());
        assert_eq!(spans(r"\r\n\z", &re, CRLF), vec![(21, 23)]);
        // dot-matches-newline
        assert_eq!(spans("bar.+?foo", &re, CRLF).len(), 0);
        assert_eq!(spans("bar.+?foo", &SearchOptions { dot_all: true, ..re }, CRLF).len(), 1);
        // free-spacing
        assert_eq!(spans("F o o  # comment", &SearchOptions { free_spacing: true, ..re }, CRLF), vec![(0, 3)]);
    }

    #[test]
    fn newline_forms_match_crlf_lf_cr() {
        let re = opts(SearchMode::Regex);
        for text in ["a\r\nb", "a\nb", "a\rb"] {
            assert_eq!(spans(r"a\Rb", &re, text).len(), 1, "{text:?}");
            assert_eq!(spans("a\nb", &opts(SearchMode::Normal), text).len(), 1, "{text:?}");
            assert_eq!(spans(r"a\nb", &opts(SearchMode::Extended), text).len(), 1, "{text:?}");
            assert_eq!(spans("a\nb", &re, text).len(), 1, "{text:?}");
        }
        // CRLF consumed as ONE newline
        assert_eq!(spans(r"a\Rb", &re, "a\r\nb"), vec![(0, 4)]);
    }

    #[test]
    fn modes_normal_extended_regex() {
        assert_eq!(spans("a.b", &opts(SearchMode::Normal), "a.b axb"), vec![(0, 3)]);
        assert_eq!(spans("a.b", &opts(SearchMode::Regex), "a.b axb").len(), 2);
        assert_eq!(spans(r"a\tb", &opts(SearchMode::Extended), "a\tb"), vec![(0, 3)]);
        assert_eq!(spans(r"a\x41\\", &opts(SearchMode::Extended), "aA\\"), vec![(0, 3)]);
        assert_eq!(spans(r"a\0b", &opts(SearchMode::Extended), "a\0b"), vec![(0, 3)]);
        // Normal mode keeps backslashes literal
        assert_eq!(spans(r"a\tb", &opts(SearchMode::Normal), r"a\tb"), vec![(0, 4)]);
    }

    #[test]
    fn invalid_pattern_reports_column() {
        let err = Matcher::new("ab[c", &opts(SearchMode::Regex)).unwrap_err();
        let SearchError::InvalidRegex { message, column } = &err;
        assert!(message.contains("unclosed"), "{message}");
        assert_eq!(*column, Some(3));
        assert!(err.to_string().contains("column 3"));
        // whole-word prefix does not shift the reported column
        let o = SearchOptions { whole_word: true, ..opts(SearchMode::Regex) };
        let SearchError::InvalidRegex { column, .. } = Matcher::new("ab[c", &o).unwrap_err();
        assert_eq!(column, Some(3));
    }

    #[test]
    fn line_mode_equals_full_mode() {
        let text = "alpha beta\ngamma\r\ndelta alpha\n\nalpha";
        let r = rope(text);
        let line = Matcher::new("alpha", &opts(SearchMode::Normal)).unwrap();
        let full = Matcher::new(r"(?s)alpha", &opts(SearchMode::Regex)).unwrap();
        assert!(!line.spans_lines() && full.spans_lines());
        let t = CancelToken::new();
        let all = [0..r.byte_len()];
        assert_eq!(line.find_all(&r, &all, &t).unwrap(), full.find_all(&r, &all, &t).unwrap());
        assert_eq!(line.count(&r, &all, &t).unwrap(), 3);
    }

    #[test]
    fn matches_split_across_rope_chunks() {
        // Large enough to span several crop chunks; one line crosses boundaries.
        let line = "x".repeat(5000);
        let text = format!("{line}needle{line}\n{line}needle\n");
        let r = rope(&text);
        let m = Matcher::new("needle", &opts(SearchMode::Normal)).unwrap();
        let got = m.find_all(&r, &[0..r.byte_len()], &CancelToken::new()).unwrap();
        assert_eq!(got, vec![5000..5006, 10007 + 5000..10007 + 5006]);
    }

    #[test]
    fn find_next_wrap_and_prev() {
        let r = rope("one two one two one");
        let m = Matcher::new("one", &opts(SearchMode::Normal)).unwrap();
        let t = CancelToken::new();
        assert_eq!(m.find_next(&r, 1, false, &t).unwrap().unwrap().range, 8..11);
        assert!(m.find_next(&r, 17, false, &t).unwrap().is_none());
        let w = m.find_next(&r, 17, true, &t).unwrap().unwrap();
        assert_eq!((w.range, w.wrapped), (0..3, true));
        // backward: previous match strictly before the caret
        assert_eq!(m.find_prev(&r, 16, false, &t).unwrap().unwrap().range, 8..11);
        assert_eq!(m.find_prev(&r, 8, false, &t).unwrap().unwrap().range, 0..3);
        assert!(m.find_prev(&r, 2, false, &t).unwrap().is_none());
        let w = m.find_prev(&r, 2, true, &t).unwrap().unwrap();
        assert_eq!((w.range, w.wrapped), (16..19, true));
        // multi-line (fallback) backward
        let ml = Matcher::new(r"b\Rc", &opts(SearchMode::Regex)).unwrap();
        let r2 = rope("b\nc x b\r\nc y");
        assert_eq!(ml.find_prev(&r2, 13, false, &t).unwrap().unwrap().range, 6..10);
    }

    #[test]
    fn cancel_aborts() {
        let r = rope(&"hello world\n".repeat(10_000));
        let m = Matcher::new("world", &opts(SearchMode::Normal)).unwrap();
        let t = CancelToken::new();
        t.cancel();
        assert_eq!(m.count(&r, &[0..r.byte_len()], &t), Err(Cancelled));
        assert_eq!(m.find_next(&r, 0, true, &t), Err(Cancelled));
    }

    #[test]
    fn replace_builds_editset_in_original_offsets() {
        let r = rope("ab cd\r\nef gh");
        let m = Matcher::new(r"(\w+) (\w+)", &opts(SearchMode::Regex)).unwrap();
        let rep = m
            .replace(&r, &[0..r.byte_len()], &Template::parse(r"\U\2\E \1"), &CancelToken::new())
            .unwrap();
        assert_eq!(rep.count, 2);
        assert_eq!(rep.edits.apply_to_str("ab cd\r\nef gh").unwrap(), "CD ab\r\nGH ef");
    }

    #[test]
    fn selection_scope_never_edits_outside() {
        let text = "aa aa aa aa";
        let r = rope(text);
        let m = Matcher::new("aa", &opts(SearchMode::Normal)).unwrap();
        let rep = m.replace(&r, &[3..8], &Template::parse("X"), &CancelToken::new()).unwrap();
        assert!(rep.edits.0.iter().all(|e| e.start_byte >= 3 && e.end_byte <= 8));
        assert_eq!(rep.edits.apply_to_str(text).unwrap(), "aa X X aa");
        // a match that would straddle the selection edge is not taken
        let rep = m.replace(&r, &[1..5], &Template::parse("X"), &CancelToken::new()).unwrap();
        assert_eq!(rep.count, 1);
        // overlapping ranges are merged, no double edit
        let rep = m.replace(&r, &[0..4, 2..6], &Template::parse("X"), &CancelToken::new()).unwrap();
        assert_eq!(rep.edits.apply_to_str(text).unwrap(), "X X aa aa");
    }

    #[test]
    fn placeholders_line_via_replace() {
        let text = "a\nb\nc\nd\ne\nf\nTODO\n";
        let r = rope(text);
        let m = Matcher::new("TODO", &opts(SearchMode::Normal)).unwrap();
        let tpl = Template::parse_with_placeholders("%LINE:Z4%");
        let rep = m.replace(&r, &[0..r.byte_len()], &tpl, &CancelToken::new()).unwrap();
        assert_eq!(rep.edits.apply_to_str(text).unwrap(), "a\nb\nc\nd\ne\nf\n0007\n");
    }

    #[test]
    fn word_under_caret() {
        let r = rope("let foo_bar = 1;");
        assert_eq!(word_range_at(&r, 6), Some(4..11));
        assert_eq!(word_range_at(&r, 4), Some(4..11));
        assert_eq!(word_range_at(&r, 11), Some(4..11));
        assert_eq!(word_range_at(&r, 12), None);
    }

    #[test]
    fn commands_and_run() {
        let cmds = commands();
        assert!(cmds.iter().all(|c| c.id.starts_with("search_") && c.caption.starts_with("Search: ")));
        let sel = [Region::caret(0)];
        let args = vec!["o".to_string(), "c".to_string(), "0".to_string()];
        let ctx = CmdCtx { text: "foo boo", selection: &sel, path: None, args: &args };
        let out = run("search_replace_all", &ctx).unwrap();
        assert_eq!(out.edits.apply_to_str("foo boo").unwrap(), "f00 b00");
        let out = run("search_find_next", &ctx).unwrap();
        assert_eq!(out.selection, Some(vec![Region { anchor: 1, head: 2 }]));
        assert!(run("nope", &ctx).is_none());
        let bad = vec!["[".to_string(), "r".to_string()];
        let ctx = CmdCtx { args: &bad, ..ctx };
        assert!(run("search_count", &ctx).unwrap().message.unwrap().contains("invalid regex"));
    }

    /// BK-015 acceptance (timing only; run with `--ignored`, ideally --release).
    #[test]
    #[ignore]
    fn perf_count_50mb() {
        let r = rope(&"lorem ipsum dolor sit amet, consectetur adipiscing\n".repeat(1_000_000));
        let m = Matcher::new("dolor", &opts(SearchMode::Normal)).unwrap();
        let t0 = std::time::Instant::now();
        let n = m.count(&r, &[0..r.byte_len()], &CancelToken::new()).unwrap();
        eprintln!("count={n} in {:?} over {} bytes", t0.elapsed(), r.byte_len());
        assert_eq!(n, 1_000_000);
    }
}
#[cfg(test)]
mod legacy_tests {
    use super::*;
    use crate::buffer::Buffer;

    fn small_buf() -> Buffer {
        Buffer::from_str("alpha\nbeta\ngamma")
    }

    #[test]
    fn find_first_in_small_buffer() {
        let buf = small_buf();
        let s = Search::new("beta").unwrap();
        let m = s.find_next(&buf, 0).expect("should find beta");
        assert_eq!(m.line, 2);
        assert_eq!(m.col, 1);
        // "alpha\n" is 6 bytes, so beta starts at 6.
        assert_eq!(m.start_byte, 6);
        assert_eq!(m.end_byte, 10);
    }

    #[test]
    fn find_all_count() {
        let buf = small_buf();
        let s = Search::new("[a-z]+").unwrap();
        let matches: Vec<_> = s.find_all(&buf).collect();
        // alpha, beta, gamma — three runs of lowercase letters.
        assert_eq!(matches.len(), 3);
        assert_eq!(s.count_matches(&buf), 3);
        assert_eq!(matches[0].line, 1);
        assert_eq!(matches[1].line, 2);
        assert_eq!(matches[2].line, 3);
    }

    #[test]
    fn replace_all_basic() {
        let buf = small_buf();
        let s = Search::new("beta").unwrap();
        let (new_text, n) = s.replace_all(&buf, "BETA");
        assert_eq!(n, 1);
        assert_eq!(new_text, "alpha\nBETA\ngamma");
    }

    #[test]
    fn case_sensitivity_via_flag() {
        let buf = small_buf();
        let sensitive = Search::new("BETA").unwrap();
        assert!(sensitive.is_case_sensitive());
        assert!(sensitive.find_next(&buf, 0).is_none());

        let insensitive = Search::new("(?i)BETA").unwrap();
        assert!(!insensitive.is_case_sensitive());
        let m = insensitive.find_next(&buf, 0).expect("(?i)BETA should match beta");
        assert_eq!(m.line, 2);
        assert_eq!(m.col, 1);
    }

    #[test]
    fn regex_metacharacters() {
        let buf = Buffer::from_str("line 1\nfoo42bar\nbaz 7 qux 13");
        let s = Search::new(r"\d+").unwrap();
        let matches: Vec<_> = s.find_all(&buf).collect();
        // "1", "42", "7", "13" — four digit runs.
        assert_eq!(matches.len(), 4);
        assert_eq!(matches[0].line, 1);
        assert_eq!(matches[1].line, 2);
        assert_eq!(matches[2].line, 3);
        assert_eq!(matches[3].line, 3);
    }

    #[test]
    fn invalid_pattern_returns_error() {
        let err = Search::new("[").unwrap_err();
        let SearchError::InvalidRegex { message, .. } = err;
        assert!(!message.is_empty(), "error message should not be empty");
    }

    #[test]
    fn find_next_with_offset() {
        // "alpha alpha alpha" — three matches starting at 0, 6, 12.
        let buf = Buffer::from_str("alpha alpha alpha");
        let s = Search::new("alpha").unwrap();
        // from_byte=10 lands inside the second "alpha" (which starts at 6,
        // ends at 11), so find_next should skip it and return the third.
        let m = s.find_next(&buf, 10).expect("should find third alpha");
        assert_eq!(m.start_byte, 12);
        assert_eq!(m.end_byte, 17);
        assert_eq!(m.line, 1);
        assert_eq!(m.col, 13);
    }

    #[test]
    fn case_insensitive_flag_detection() {
        assert!(contains_case_insensitive_flag("(?i)foo"));
        assert!(contains_case_insensitive_flag("(?im)foo"));
        assert!(contains_case_insensitive_flag("(?i:foo)"));
        assert!(contains_case_insensitive_flag("foo(?i)bar"));
        assert!(!contains_case_insensitive_flag("foo"));
        assert!(!contains_case_insensitive_flag("[abc(?i)]"));
        assert!(!contains_case_insensitive_flag(r"\(?i\)"));
    }

    #[test]
    fn find_next_beyond_end_returns_none() {
        let buf = small_buf();
        let s = Search::new("alpha").unwrap();
        assert!(s.find_next(&buf, 99999).is_none());
    }

    #[test]
    fn count_matches_zero_for_no_match() {
        let buf = small_buf();
        let s = Search::new("zzzzz").unwrap();
        assert_eq!(s.count_matches(&buf), 0);
    }

    #[test]
    fn replace_all_no_match_returns_original() {
        let buf = small_buf();
        let s = Search::new("zzzzz").unwrap();
        let (text, n) = s.replace_all(&buf, "X");
        assert_eq!(n, 0);
        assert_eq!(text, "alpha\nbeta\ngamma");
    }

    #[test]
    fn search_error_display() {
        let err = SearchError::InvalidRegex { message: "test error".to_string(), column: None };
        let msg = format!("{}", err);
        assert!(msg.contains("test error"));
    }

    #[test]
    fn match_struct_clone_eq() {
        let m = Match { start_byte: 0, end_byte: 5, line: 1, col: 1 };
        let m2 = m.clone();
        assert_eq!(m, m2);
    }

    #[test]
    fn find_all_empty_buffer() {
        let buf = Buffer::from_str("");
        let s = Search::new("x").unwrap();
        let matches: Vec<_> = s.find_all(&buf).collect();
        assert!(matches.is_empty());
    }

    #[test]
    fn multiline_pattern() {
        let buf = Buffer::from_str("foo\nbar\nbaz");
        let s = Search::new(r"bar\nbaz").unwrap();
        let matches: Vec<_> = s.find_all(&buf).collect();
        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0].line, 2);
    }
}
