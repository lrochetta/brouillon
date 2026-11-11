//! Table-driven syntax colouring.
//!
//! * `def`      — TOML language definition schema + compiled `Syntax`.
//! * `registry` — shipped (`langs/*.toml`) + user definitions, detection.
//! * `lexer`    — line lexer with carried state (comments, strings, heredocs,
//!                markup tags, nested languages such as PHP in HTML).
//! * `SyntaxDoc` (here) — text + per-line start states, lexed lazily and
//!                invalidated from the edited line.
//!
//! Definition format: see `langs/README.md`.

pub mod def;
pub mod lexer;
pub mod registry;

pub use def::{Origin, Syntax, SyntaxError};
pub use registry::{registry, reload_user_dir, Registry};

use crate::highlight::HighlightSpan;
use lexer::State;
use std::borrow::Cow;
use std::cell::RefCell;
use std::ops::Range;
use std::sync::Arc;

/// The text of a [`SyntaxDoc`]: an owned `String`, or a rope shared with the editor's buffer (no copy of
/// the document at all; the rope only diverges from the buffer's by the chunks that were edited since).
enum Source {
    Owned(String),
    Rope(crop::Rope),
}

impl Source {
    fn len(&self) -> usize {
        match self {
            Self::Owned(t) => t.len(),
            Self::Rope(r) => r.byte_len(),
        }
    }

    /// `s..e` (both on character boundaries): borrowed when it lies in one chunk, copied otherwise.
    fn slice(&self, s: usize, e: usize) -> Cow<'_, str> {
        match self {
            Self::Owned(t) => Cow::Borrowed(&t[s..e]),
            Self::Rope(r) => {
                let mut chunks = r.byte_slice(s..e).chunks();
                match (chunks.next(), chunks.next()) {
                    (None, _) => Cow::Borrowed(""),
                    (Some(a), None) => Cow::Borrowed(a),
                    (Some(a), Some(b)) => {
                        let mut out = String::with_capacity(e - s);
                        out.push_str(a);
                        out.push_str(b);
                        out.extend(chunks);
                        Cow::Owned(out)
                    }
                }
            }
        }
    }

    fn is_char_boundary(&self, i: usize) -> bool {
        match self {
            Self::Owned(t) => t.is_char_boundary(i),
            Self::Rope(r) => r.is_char_boundary(i),
        }
    }

    /// The text as byte pieces (one piece for a `String`, the rope's chunks otherwise).
    fn pieces(&self) -> Vec<&[u8]> {
        match self {
            Self::Owned(t) => vec![t.as_bytes()],
            Self::Rope(r) => r.chunks().map(str::as_bytes).collect(),
        }
    }

    /// Line starts created by the newlines in bytes `a..b` (the offset just after each `\n`), without copying.
    fn newline_starts_in(&self, a: usize, b: usize) -> Vec<usize> {
        let mut v = Vec::new();
        if a >= b {
            return v;
        }
        match self {
            Self::Owned(t) => v.extend(t[a..b].match_indices('\n').map(|(i, _)| a + i + 1)),
            Self::Rope(r) => {
                let mut off = a;
                for c in r.byte_slice(a..b).chunks() {
                    v.extend(c.match_indices('\n').map(|(i, _)| off + i + 1));
                    off += c.len();
                }
            }
        }
        v
    }

    /// Byte offset of each line start (always at least `[0]`).
    fn line_starts(&self) -> Vec<usize> {
        match self {
            Self::Owned(t) => line_starts(t),
            Self::Rope(r) => {
                let mut v = Vec::with_capacity(r.byte_len() / 32 + 2);
                v.push(0);
                let mut off = 0;
                for c in r.chunks() {
                    v.extend(c.match_indices('\n').map(|(i, _)| off + i + 1));
                    off += c.len();
                }
                v
            }
        }
    }
}

/// Lexer start states of the lines, run-length encoded: a document is mostly "code, nothing open", so one
/// entry per line (30+ bytes each, as large as the source for short lines) collapses to a handful.
#[derive(Clone, Default)]
struct StateRuns {
    /// `(first line, state)`; first lines strictly increase and consecutive runs differ.
    runs: Vec<(usize, State)>,
    len: usize,
}

impl StateRuns {
    fn single(s: State) -> Self {
        Self { runs: vec![(0, s)], len: 1 }
    }

    fn len(&self) -> usize {
        self.len
    }

    /// State of line `i` (`i < len`).
    fn get(&self, i: usize) -> &State {
        let k = self.runs.partition_point(|(s, _)| *s <= i);
        &self.runs[k - 1].1
    }

    fn push_n(&mut self, st: State, n: usize) {
        if n == 0 {
            return;
        }
        if self.runs.last().map_or(true, |(_, last)| *last != st) {
            self.runs.push((self.len, st));
        }
        self.len += n;
    }

    fn push(&mut self, st: State) {
        self.push_n(st, 1);
    }

    fn truncate(&mut self, n: usize) {
        if n >= self.len {
            return;
        }
        while self.runs.last().map_or(false, |(s, _)| *s >= n) {
            self.runs.pop();
        }
        self.len = n;
    }

    /// The first `n` states.
    fn prefix(&self, n: usize) -> Self {
        let keep = self.runs.partition_point(|(s, _)| *s < n);
        Self { runs: self.runs[..keep].to_vec(), len: n.min(self.len) }
    }

    /// Append the states of `other` from line `from` on.
    fn append_from(&mut self, other: &StateRuns, from: usize) {
        let mut idx = other.runs.partition_point(|(s, _)| *s <= from).saturating_sub(1);
        while idx < other.runs.len() {
            let (s, st) = &other.runs[idx];
            let end = other.runs.get(idx + 1).map_or(other.len, |r| r.0);
            let a = (*s).max(from);
            if end > a {
                self.push_n(st.clone(), end - a);
            }
            idx += 1;
        }
    }
}

/// A document being coloured with one language definition.
///
/// `states.get(i)` is the lexer state at the start of line `i`; only a prefix of
/// the lines has been lexed (`states.len() - 1` lines). Lines are lexed on
/// demand when spans are requested and the prefix is cut at the edited line by
/// [`SyntaxDoc::apply_edit`].
pub struct SyntaxDoc {
    reg: Arc<Registry>,
    root: u16,
    text: Source,
    /// Byte offset of each line start (always at least `[0]`).
    starts: Vec<usize>,
    states: RefCell<StateRuns>,
}

fn line_starts(text: &str) -> Vec<usize> {
    let mut v = Vec::with_capacity(text.len() / 32 + 2);
    v.push(0);
    v.extend(text.match_indices('\n').map(|(i, _)| i + 1));
    v
}

impl SyntaxDoc {
    pub fn new(reg: Arc<Registry>, root: u16, text: &str) -> Self {
        let init = State::initial(&reg, reg.def(root));
        Self { reg, root, text: Source::Owned(text.to_string()), starts: line_starts(text), states: RefCell::new(StateRuns::single(init)) }
    }

    /// A document that reads its text from `rope` (a cheap clone of the buffer's rope): no copy of the text.
    pub fn new_rope(reg: Arc<Registry>, root: u16, rope: crop::Rope) -> Self {
        let init = State::initial(&reg, reg.def(root));
        let text = Source::Rope(rope);
        let starts = text.line_starts();
        Self { reg, root, text, starts, states: RefCell::new(StateRuns::single(init)) }
    }

    fn reset_states(&mut self) {
        let init = State::initial(&self.reg, self.reg.def(self.root));
        *self.states.borrow_mut() = StateRuns::single(init);
    }

    /// Replace the whole text (drops all cached state).
    pub fn set_text(&mut self, text: &str) {
        match &mut self.text {
            Source::Owned(t) => {
                t.clear();
                t.push_str(text);
            }
            other => *other = Source::Owned(text.to_string()),
        }
        self.starts = line_starts(text);
        self.reset_states();
    }

    /// Replace the whole text by a rope (drops all cached state).
    pub fn set_rope(&mut self, rope: crop::Rope) {
        self.text = Source::Rope(rope);
        self.starts = self.text.line_starts();
        self.reset_states();
    }

    /// The whole text (borrowed for a `String` document, copied for a rope document).
    pub fn text(&self) -> Cow<'_, str> {
        self.text.slice(0, self.text.len())
    }

    /// Length of the text in bytes.
    pub fn text_len(&self) -> usize {
        self.text.len()
    }

    pub fn line_count(&self) -> usize {
        self.starts.len()
    }

    /// Number of lines whose start state is currently cached.
    pub fn lexed_lines(&self) -> usize {
        self.states.borrow().len()
    }

    fn line_text(&self, i: usize) -> (usize, Cow<'_, str>) {
        let s = self.starts[i];
        let e = self.starts.get(i + 1).copied().unwrap_or(self.text.len());
        let mut line = self.text.slice(s, e);
        if line.ends_with('\n') {
            line = trim_suffix(line, 1);
        }
        if line.ends_with('\r') {
            line = trim_suffix(line, 1);
        }
        (s, line)
    }

    fn line_of(&self, byte: usize) -> usize {
        self.starts.partition_point(|s| *s <= byte).saturating_sub(1)
    }

    /// Lex line `i` from `st`, appending spans to `out`; returns end state.
    fn lex(&self, i: usize, mut st: State, out: &mut Vec<HighlightSpan>) -> State {
        let (base, line) = self.line_text(i);
        lexer::lex_line(&self.reg, self.reg.def(self.root), &line, base, &mut st, out);
        st
    }

    /// Make sure `states.get(line)` exists.
    fn ensure(&self, line: usize) {
        let mut states = self.states.borrow_mut();
        let mut scratch = Vec::new();
        while states.len() <= line && states.len() <= self.starts.len() {
            let i = states.len() - 1;
            scratch.clear();
            let next = self.lex(i, states.get(i).clone(), &mut scratch);
            states.push(next);
        }
    }

    /// Spans for every line overlapping `range` (absolute byte offsets).
    /// Spans may extend past the range on the first/last line.
    pub fn spans_in_range(&self, range: Range<usize>) -> Vec<HighlightSpan> {
        let mut out = Vec::new();
        if range.is_empty() || self.text.len() == 0 {
            return out;
        }
        let first = self.line_of(range.start);
        self.ensure(first);
        let mut i = first;
        while i < self.starts.len() && self.starts[i] < range.end {
            self.ensure(i);
            let st = self.states.borrow().get(i).clone();
            let next = self.lex(i, st, &mut out);
            {
                let mut states = self.states.borrow_mut();
                if states.len() == i + 1 {
                    states.push(next);
                }
            }
            i += 1;
        }
        out
    }

    /// Replace `old_len` bytes at `start` by `new_len` bytes; `new_text` is the
    /// full text after the edit. Re-lexes from the edited line and stops as soon
    /// as the lexer state converges with the previously cached one.
    pub fn apply_edit(&mut self, new_text: &str, start: usize, old_len: usize, new_len: usize) {
        self.relex_after(start, old_len, new_len, |text| match text {
            Source::Owned(t) => {
                t.clear();
                t.push_str(new_text);
            }
            other => *other = Source::Owned(new_text.to_string()),
        });
    }

    /// [`Self::apply_edit`] with the full text after the edit given as a rope (no copy of the text).
    pub fn apply_edit_rope(&mut self, new_text: crop::Rope, start: usize, old_len: usize, new_len: usize) {
        self.relex_after(start, old_len, new_len, move |text| *text = Source::Rope(new_text));
    }

    /// Update to `new_text` without knowing the edit: the changed span is found by trimming the common
    /// prefix and suffix against the current text (compared piece by piece, nothing is copied), then
    /// [`Self::apply_edit_rope`] runs on it.
    pub fn apply_rope(&mut self, new_text: crop::Rope) {
        let newer = Source::Rope(new_text);
        let (start, old_len, new_len) = {
            let (old_p, new_p) = (self.text.pieces(), newer.pieces());
            let (a_len, b_len) = (self.text.len(), newer.len());
            let max = a_len.min(b_len);
            let mut start = common_prefix(&old_p, &new_p).min(max);
            while start > 0 && !(self.text.is_char_boundary(start) && newer.is_char_boundary(start)) {
                start -= 1;
            }
            let mut suffix = common_suffix(&old_p, &new_p, max - start);
            while suffix > 0 && !(self.text.is_char_boundary(a_len - suffix) && newer.is_char_boundary(b_len - suffix)) {
                suffix -= 1;
            }
            (start, a_len - start - suffix, b_len - start - suffix)
        };
        let Source::Rope(rope) = newer else { unreachable!() };
        self.apply_edit_rope(rope, start, old_len, new_len);
    }

    /// Line index after an edit that replaced `old_len` bytes at `start` by `new_len` bytes, in place: the
    /// starts inside the replaced span are swapped for those of the new span and every later start shifts
    /// (no second index is built, so an edit allocates nothing proportional to the document).
    fn splice_starts(&mut self, start: usize, old_len: usize, new_len: usize, old_total: usize) {
        let start = start.min(old_total);
        let old_end = (start + old_len).min(old_total);
        let lo = self.starts.partition_point(|&s| s <= start);
        let hi = self.starts.partition_point(|&s| s <= old_end);
        let fresh = self.text.newline_starts_in(start, (start + new_len).min(self.text.len()));
        let delta = new_len as isize - (old_end - start) as isize;
        if delta != 0 {
            for s in &mut self.starts[hi..] {
                *s = (*s as isize + delta) as usize;
            }
        }
        self.starts.splice(lo..hi, fresh);
    }

    #[cfg(test)]
    fn index_is_consistent(&self) -> bool {
        self.starts == self.text.line_starts()
    }

    fn relex_after(&mut self, start: usize, old_len: usize, new_len: usize, replace: impl FnOnce(&mut Source)) {
        let d = self.line_of(start.min(self.text.len()));
        let old_states = std::mem::take(&mut *self.states.borrow_mut());
        let old_line_count = self.starts.len() as isize;
        let old_total = self.text.len();
        replace(&mut self.text);
        self.splice_starts(start, old_len, new_len, old_total);
        let new_line_count = self.starts.len() as isize;
        let delta = new_line_count - old_line_count;
        let i_end = self.line_of((start + new_len).min(self.text.len()));

        if old_states.len() <= d {
            // Nothing cached at or after the edited line: just keep the prefix.
            *self.states.borrow_mut() = old_states;
            return;
        }
        let mut states = old_states.prefix(d + 1);
        let mut scratch = Vec::new();
        let mut k = d;
        while k < self.starts.len() {
            scratch.clear();
            let next = self.lex(k, states.get(k).clone(), &mut scratch);
            let n = k + 1;
            if n > i_end {
                let o = n as isize - delta; // matching old line start
                if o > d as isize {
                    let o = o as usize;
                    if o < old_states.len() {
                        if *old_states.get(o) == next {
                            states.push(next);
                            states.append_from(&old_states, o + 1);
                            break;
                        }
                    } else {
                        states.push(next);
                        break; // past what was cached: stay lazy
                    }
                }
            }
            states.push(next);
            k += 1;
        }
        states.truncate(self.starts.len() + 1);
        *self.states.borrow_mut() = states;
    }
}

/// Length of the common prefix of two texts given as byte pieces.
fn common_prefix(a: &[&[u8]], b: &[&[u8]]) -> usize {
    let (mut ia, mut ib, mut oa, mut ob) = (0, 0, 0, 0);
    let mut n = 0;
    loop {
        while ia < a.len() && oa == a[ia].len() {
            ia += 1;
            oa = 0;
        }
        while ib < b.len() && ob == b[ib].len() {
            ib += 1;
            ob = 0;
        }
        if ia >= a.len() || ib >= b.len() {
            return n;
        }
        let (x, y) = (&a[ia][oa..], &b[ib][ob..]);
        let m = x.len().min(y.len());
        let same = x[..m].iter().zip(&y[..m]).take_while(|(p, q)| p == q).count();
        n += same;
        if same < m {
            return n;
        }
        oa += m;
        ob += m;
    }
}

/// Length of the common suffix of two texts given as byte pieces, at most `cap`.
fn common_suffix(a: &[&[u8]], b: &[&[u8]], cap: usize) -> usize {
    let (mut ia, mut ib) = (a.len(), b.len());
    let (mut ra, mut rb) = (0usize, 0usize); // bytes left in the current piece, counted from its start
    let mut n = 0;
    loop {
        while ra == 0 && ia > 0 {
            ia -= 1;
            ra = a[ia].len();
        }
        while rb == 0 && ib > 0 {
            ib -= 1;
            rb = b[ib].len();
        }
        if ra == 0 || rb == 0 || n >= cap {
            return n.min(cap);
        }
        let m = ra.min(rb);
        let (x, y) = (&a[ia][ra - m..ra], &b[ib][rb - m..rb]);
        let same = x.iter().rev().zip(y.iter().rev()).take_while(|(p, q)| p == q).count();
        n += same;
        if same < m {
            return n.min(cap);
        }
        ra -= m;
        rb -= m;
    }
}

/// `s` without its last `n` bytes, keeping the borrow when there is one.
fn trim_suffix(s: Cow<'_, str>, n: usize) -> Cow<'_, str> {
    match s {
        Cow::Borrowed(b) => Cow::Borrowed(&b[..b.len() - n]),
        Cow::Owned(mut o) => {
            o.truncate(o.len() - n);
            Cow::Owned(o)
        }
    }
}

#[cfg(test)]
mod tests;
