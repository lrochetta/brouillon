//! Find / Replace over huge documents (lazy-store task 3): literal and regex, next/previous,
//! count, find-all (capped), Replace All (streamed), over an immutable [`ByteSource`] snapshot,
//! with no whole-text `String`.
//!
//! The regex is the SAME compiled [`Matcher`] the in-memory engine uses (options, translation of
//! the pattern, whole word, CRLF handling), so results are equal to `Matcher::{count, find_all,
//! find_next, find_prev, replace}` on the same text. This is tested against the whole-text
//! implementation on random inputs at tiny window sizes.
//!
//! # How a window is chosen
//!
//! * Pattern that cannot match a line break (`!Matcher::spans_lines()`, the common case): windows
//!   of `window` bytes (1 MiB) are cut AFTER the last `\n`, so every line is searched whole and the
//!   per-line semantics of the in-memory engine hold exactly. A line longer than the window grows
//!   the window (doubling) up to `line_cap` (64 MiB); beyond that the window is cut anywhere and
//!   handled like the multi-line case below (matches on such a line that straddle the cut within
//!   `margin` bytes are found; `^` right after such a cut can match wrongly).
//! * Pattern that can span lines: windows overlap. A match is accepted only if it ends at least
//!   `margin` bytes (128 KiB) before the window end; the next window restarts at the first match
//!   that was not accepted (a true match boundary, with 8 bytes of left context so `\b`, `^`
//!   and friends see the real previous character). **Limit:** a match (or an earlier-starting
//!   candidate that fails to complete) longer than `margin` that straddles a window end can be
//!   missed or reported differently; the window doubles (up to `line_cap`) when one match does not
//!   fit, so a single match up to 64 MiB is still found, with the rule applied at that size.
//! * Previous: line-mode patterns scan windows backwards (cost proportional to the distance to the
//!   previous match); patterns that span lines scan forward from the start and keep the last match
//!   (O(file), streaming).
//! * Invalid UTF-8 inside the document: the bad bytes are skipped as barriers (no match crosses
//!   them) and offsets stay exact. The lazy store only exposes valid text for editable documents.
//!
//! Memory: one window buffer (1 MiB, up to 64 MiB for pathological lines) plus the result list
//! (capped by the caller). Cancel and progress through [`Ctl`], checked once per window.

use std::io;
use std::ops::Range;

use regex::{Captures, Regex};

use crate::byte_source::{read_blocks, ByteSource, Ctl};
use crate::search::replace_tpl::{MatchCtx, Template};
use crate::search::{normalize_ranges, Cancelled, Found, Matcher};

#[derive(Debug, Clone)]
pub struct WindowCfg {
    pub window: usize,
    pub margin: usize,
    pub line_cap: usize,
}
impl Default for WindowCfg {
    fn default() -> Self {
        Self { window: 1 << 20, margin: 128 << 10, line_cap: 64 << 20 }
    }
}
impl WindowCfg {
    /// Tiny windows for tests (every window edge is crossed constantly).
    pub fn tiny(window: usize, margin: usize, line_cap: usize) -> Self {
        Self { window, margin, line_cap }
    }
    fn sane(&self) -> Self {
        let margin = self.margin.max(1);
        let window = self.window.max(2 * margin + 16);
        Self { window, margin, line_cap: self.line_cap.max(window) }
    }
}

/// One match handed to the callback of [`drive`].
pub struct Hit<'a> {
    /// Absolute byte range in the source.
    pub range: Range<u64>,
    pub caps: Option<&'a Captures<'a>>,
    /// 0-based line of the match start (only computed when asked for).
    pub line: u64,
}

fn next_boundary(s: &str, i: usize) -> usize {
    let mut j = i + 1;
    while j < s.len() && !s.is_char_boundary(j) {
        j += 1;
    }
    j
}
fn floor_boundary(s: &str, mut i: usize) -> usize {
    i = i.min(s.len());
    while i > 0 && !s.is_char_boundary(i) {
        i -= 1;
    }
    i
}

/// The match iteration of `Regex::{find_iter, captures_iter}` started at `at` of `text` (look-behind
/// context before `at` is visible), including the rule that an empty match touching the previous
/// match end is skipped. `last_end` is an index into `text`.
fn next_match<'t>(re: &Regex, text: &'t str, pos: &mut usize, last_end: &mut Option<usize>, want_caps: bool) -> Option<(Range<usize>, Option<Captures<'t>>)> {
    loop {
        if *pos > text.len() {
            return None;
        }
        let (range, caps) = if want_caps {
            let c = re.captures_at(text, *pos)?;
            let m = c.get(0).expect("group 0");
            (m.range(), Some(c))
        } else {
            (re.find_at(text, *pos)?.range(), None)
        };
        if range.is_empty() && Some(range.end) == *last_end {
            *pos = if range.end >= text.len() { text.len() + 1 } else { next_boundary(text, range.end) };
            continue;
        }
        *pos = range.end;
        *last_end = Some(range.end);
        return Some((range, caps));
    }
}

fn count_newlines(src: &dyn ByteSource, r: Range<u64>) -> u64 {
    let mut n = 0u64;
    read_blocks(src, r, 1 << 20, &mut |_, b| {
        n += memchr::memchr_iter(b'\n', b).count() as u64;
        true
    });
    n
}

/// Run the matcher over `range` of `src` window by window and call `on_hit` for every match in
/// order; it returns false to stop. `Ok(true)` = ran to the end, `Ok(false)` = stopped by `on_hit`.
pub fn drive(
    m: &Matcher,
    src: &dyn ByteSource,
    range: Range<u64>,
    cfg: &WindowCfg,
    ctl: &mut Ctl,
    want_caps: bool,
    want_line: bool,
    on_hit: &mut dyn FnMut(&Hit) -> bool,
) -> Result<bool, Cancelled> {
    let cfg = cfg.sane();
    let re = m.regex();
    let spans = m.spans_lines();
    let end = range.end.min(src.len());
    let start = range.start.min(end);
    if ctl.cancel.is_cancelled() {
        return Err(Cancelled);
    }
    if start == end {
        // The in-memory engine runs the regex once over the empty text.
        let mut pos = 0;
        let mut le = None;
        while let Some((r, caps)) = next_match(re, "", &mut pos, &mut le, want_caps) {
            if !on_hit(&Hit { range: start..start + r.len() as u64, caps: caps.as_ref(), line: 0 }) {
                return Ok(false);
            }
        }
        return Ok(true);
    }
    let total = end - start;
    let mut pos = start;
    let mut nl_before = if want_line { count_newlines(src, 0..start) } else { 0 };
    let mut buf: Vec<u8> = Vec::new();
    let mut w = cfg.window;
    let mut last_end_abs: Option<u64> = None;
    let mut force = false;

    'win: loop {
        if ctl.cancel.is_cancelled() {
            return Err(Cancelled);
        }
        let rd = read_window(src, &mut buf, pos, start, end, w);
        let (cstart, at, final_win, barrier_skip) = (rd.cstart, rd.at, rd.final_win, rd.barrier_skip);
        let mut text: &str = std::str::from_utf8(&buf[rd.lead..rd.lead + rd.text_len]).expect("validated");
        let barrier = barrier_skip > 0;
        let complete = final_win || barrier;
        let mut line_aligned = false;
        // Line mode: cut after the last newline so every line is whole.
        if !spans && !complete && !force {
            match memchr::memrchr(b'\n', &text.as_bytes()[at..]) {
                Some(k) => {
                    text = &text[..at + k + 1];
                    line_aligned = true;
                }
                None => {
                    if w < cfg.line_cap {
                        w = (w * 2).min(cfg.line_cap);
                        continue 'win;
                    }
                    // a line beyond the cap: handled like a spanning window below
                }
            }
        }
        let partial = !(complete || line_aligned || force);
        let mut accept = text.len();
        if partial {
            accept = floor_boundary(text, text.len().saturating_sub(cfg.margin));
            if accept <= at {
                if w < cfg.line_cap {
                    w = (w * 2).min(cfg.line_cap);
                    continue 'win;
                }
                accept = text.len();
            }
        }
        // ---- iterate the matches of this window
        let mut restart: Option<usize> = None;
        let mut last_end: Option<usize> = last_end_abs.and_then(|a| a.checked_sub(cstart)).map(|x| x as usize).filter(|&x| x <= text.len());
        let mut line_idx = at; // text[at..line_idx] already counted into line_cnt
        let mut line_cnt = nl_before;
        let mut stopped = false;
        let mut emit = |r: Range<usize>, caps: Option<&Captures>, line_idx: &mut usize, line_cnt: &mut u64, text: &str| -> bool {
            if want_line {
                *line_cnt += memchr::memchr_iter(b'\n', &text.as_bytes()[*line_idx..r.start]).count() as u64;
                *line_idx = r.start;
            }
            on_hit(&Hit { range: cstart + r.start as u64..cstart + r.end as u64, caps, line: *line_cnt })
        };
        let at_line_start = at == 0 || text.as_bytes()[at - 1] == b'\n';
        if !spans && (complete || line_aligned) && at_line_start {
            // per-line, exactly like the in-memory walk
            let tb = text.as_bytes();
            let mut i = at;
            'lines: while i < text.len() {
                // Prefilter: the first match anywhere in the rest of the window, so lines without a
                // match are skipped at the regex's own speed. Any per-line match is also a window
                // match, hence no line holding one is skipped; the per-line pass below keeps the
                // exact semantics of the in-memory engine.
                let Some(m0) = re.find_at(text, i) else { break };
                let ls = memchr::memrchr(b'\n', &tb[i..m0.start()]).map_or(i, |k| i + k + 1);
                if ls >= text.len() {
                    break; // the match is the empty one after the final newline: no such line
                }
                let e = memchr::memchr(b'\n', &tb[ls..]).map_or(text.len(), |k| ls + k + 1);
                let line = &text[ls..e];
                let body = line.strip_suffix("\r\n").or_else(|| line.strip_suffix('\n')).unwrap_or(line);
                let (mut p, mut le) = (0usize, None);
                while let Some((r, caps)) = next_match(re, body, &mut p, &mut le, want_caps) {
                    if !emit(ls + r.start..ls + r.end, caps.as_ref(), &mut line_idx, &mut line_cnt, text) {
                        stopped = true;
                        break 'lines;
                    }
                }
                i = e;
            }
        } else {
            let mut p = at;
            loop {
                let saved = last_end;
                let Some((r, caps)) = next_match(re, text, &mut p, &mut last_end, want_caps) else { break };
                if !complete && r.start >= text.len() {
                    // starts at the very end of a window that continues: decided by the next window
                    last_end = saved;
                    break;
                }
                if partial && r.end > accept {
                    last_end = saved;
                    restart = Some(r.start);
                    break;
                }
                if !emit(r, caps.as_ref(), &mut line_idx, &mut line_cnt, text) {
                    stopped = true;
                    break;
                }
            }
            if partial && restart.is_none() && !stopped {
                restart = Some(accept);
            }
        }
        if stopped {
            return Ok(false);
        }
        // ---- advance
        let consumed_idx = if partial { restart.expect("partial window sets a restart") } else { text.len() };
        let next_pos = cstart + consumed_idx as u64 + barrier_skip as u64;
        if final_win || next_pos >= end {
            return Ok(true);
        }
        if next_pos <= pos {
            // no progress: a single match does not fit; grow, or accept everything at the cap
            if w < cfg.line_cap {
                w = (w * 2).min(cfg.line_cap);
            } else {
                force = true;
            }
            continue 'win;
        }
        if want_line {
            let from = line_idx.min(consumed_idx);
            nl_before = line_cnt + memchr::memchr_iter(b'\n', &text.as_bytes()[from..consumed_idx]).count() as u64;
        }
        // carry the empty-match rule across the restart
        last_end_abs = if partial { last_end.map(|x| cstart + x as u64).filter(|&a| a <= next_pos) } else { None };
        pos = next_pos;
        w = cfg.window;
        force = false;
        if !ctl.tick(pos - start, total) {
            return Err(Cancelled);
        }
    }
}

struct Read {
    /// Absolute offset of the first byte of the text (context included).
    cstart: u64,
    /// Offset of that byte inside the buffer.
    lead: usize,
    text_len: usize,
    /// Index in the text where matches may start (text[..at] is look-behind context).
    at: usize,
    /// The text reaches the end of the range (or the source ended early).
    final_win: bool,
    /// Invalid bytes right after the text that no match may cross; the next window starts after them.
    barrier_skip: usize,
}

/// Read the window that starts at `pos` (plus up to 8 bytes of left context, never before `start`).
fn read_window(src: &dyn ByteSource, buf: &mut Vec<u8>, pos: u64, start: u64, end: u64, w: usize) -> Read {
    let mut ctx = if pos > start { 8.min(pos - start) } else { 0 } as usize;
    loop {
        let cs = pos - ctx as u64;
        let want_end = (pos + w as u64).min(end);
        let want = (want_end - cs) as usize;
        if buf.len() < want {
            buf.resize(want, 0);
        }
        let got = src.read_at(cs, &mut buf[..want]);
        let is_final = cs + got as u64 >= end || got < want;
        // skip the continuation bytes of a character that starts before the context
        let mut lead = 0;
        while lead < ctx.min(got) && (buf[lead] & 0xC0) == 0x80 {
            lead += 1;
        }
        let at = ctx - lead;
        let slice = &buf[lead..got];
        let base = Read { cstart: cs + lead as u64, lead, text_len: slice.len(), at, final_win: is_final, barrier_skip: 0 };
        match std::str::from_utf8(slice) {
            Ok(_) => return base,
            Err(e) => {
                let v = e.valid_up_to();
                if ctx > 0 && v < at && e.error_len().is_some() {
                    ctx = 0; // invalid bytes inside the context: search without it
                    continue;
                }
                return match e.error_len() {
                    Some(l) => Read { text_len: v, final_win: false, barrier_skip: l, ..base },
                    None if is_final => Read { text_len: v, final_win: false, barrier_skip: slice.len() - v, ..base },
                    None => Read { text_len: v, final_win: false, ..base }, // a character cut by the window end
                };
            }
        }
    }
}

// ------------------------------------------------------------------------------------------ queries

fn ranges_u64(ranges: &[Range<usize>], len: u64) -> Vec<Range<u64>> {
    normalize_ranges(ranges).into_iter().map(|r| (r.start as u64).min(len)..(r.end as u64).min(len)).collect()
}

/// Number of matches in `ranges`.
pub fn count(m: &Matcher, src: &dyn ByteSource, ranges: &[Range<usize>], cfg: &WindowCfg, ctl: &mut Ctl) -> Result<u64, Cancelled> {
    let mut n = 0u64;
    for r in ranges_u64(ranges, src.len()) {
        drive(m, src, r, cfg, ctl, false, false, &mut |_| {
            n += 1;
            true
        })?;
    }
    Ok(n)
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct FoundAll {
    pub ranges: Vec<Range<usize>>,
    /// More matches exist than `max_results`.
    pub truncated: bool,
}

/// Match ranges in order, at most `max_results` (the list is what the results panel can show).
pub fn find_all(m: &Matcher, src: &dyn ByteSource, ranges: &[Range<usize>], max_results: usize, cfg: &WindowCfg, ctl: &mut Ctl) -> Result<FoundAll, Cancelled> {
    let mut out = FoundAll::default();
    for r in ranges_u64(ranges, src.len()) {
        let done = drive(m, src, r, cfg, ctl, false, false, &mut |h| {
            if out.ranges.len() >= max_results {
                out.truncated = true;
                return false;
            }
            out.ranges.push(h.range.start as usize..h.range.end as usize);
            true
        })?;
        if !done {
            break;
        }
    }
    Ok(out)
}

fn first_in(m: &Matcher, src: &dyn ByteSource, r: Range<u64>, cfg: &WindowCfg, ctl: &mut Ctl) -> Result<Option<Range<usize>>, Cancelled> {
    let mut hit = None;
    drive(m, src, r, cfg, ctl, false, false, &mut |h| {
        hit = Some(h.range.start as usize..h.range.end as usize);
        false
    })?;
    Ok(hit)
}

/// First match starting at or after `from`; with `wrap`, restarts at 0.
pub fn find_next(m: &Matcher, src: &dyn ByteSource, from: usize, wrap: bool, cfg: &WindowCfg, ctl: &mut Ctl) -> Result<Option<Found>, Cancelled> {
    let len = src.len();
    if let Some(range) = first_in(m, src, (from as u64).min(len)..len, cfg, ctl)? {
        return Ok(Some(Found { range, wrapped: false }));
    }
    if !wrap {
        return Ok(None);
    }
    Ok(first_in(m, src, 0..len, cfg, ctl)?.map(|range| Found { range, wrapped: true }))
}

fn last_forward(m: &Matcher, src: &dyn ByteSource, r: Range<u64>, cfg: &WindowCfg, ctl: &mut Ctl) -> Result<Option<Range<usize>>, Cancelled> {
    let mut last = None;
    drive(m, src, r, cfg, ctl, false, false, &mut |h| {
        last = Some(h.range.start as usize..h.range.end as usize);
        true
    })?;
    Ok(last)
}

/// Last match inside `r`, scanning windows from the end (line-mode patterns only).
fn last_backward(m: &Matcher, src: &dyn ByteSource, r: Range<u64>, cfg: &WindowCfg, ctl: &mut Ctl) -> Result<Option<Option<Range<usize>>>, Cancelled> {
    let cfg = cfg.sane();
    let re = m.regex();
    let mut end = r.end.min(src.len());
    let mut buf: Vec<u8> = Vec::new();
    let mut w = cfg.window;
    while end > r.start {
        if ctl.cancel.is_cancelled() {
            return Err(Cancelled);
        }
        let start0 = end.saturating_sub(w as u64).max(r.start);
        let want = (end - start0) as usize;
        if buf.len() < want {
            buf.resize(want, 0);
        }
        let got = src.read_at(start0, &mut buf[..want]);
        if got < want {
            return Ok(None); // the source shrank: let the caller use the forward scan
        }
        let mut lo = 0usize;
        if start0 > r.start {
            match memchr::memchr(b'\n', &buf[..want]) {
                Some(k) if k + 1 >= want && w < cfg.line_cap => {
                    // the only newline is the last byte: no whole line in this window yet
                    w = (w * 2).min(cfg.line_cap);
                    continue;
                }
                Some(k) if k + 1 >= want => {}
                Some(k) => lo = k + 1,
                None if w < cfg.line_cap => {
                    w = (w * 2).min(cfg.line_cap);
                    continue;
                }
                None => {
                    while lo < want && (buf[lo] & 0xC0) == 0x80 {
                        lo += 1;
                    }
                }
            }
        }
        let Ok(text) = std::str::from_utf8(&buf[lo..want]) else { return Ok(None) };
        let base = start0 + lo as u64;
        let tb = text.as_bytes();
        let mut e = text.len();
        while e > 0 {
            let s = memchr::memrchr(b'\n', &tb[..e - 1]).map_or(0, |k| k + 1);
            let line = &text[s..e];
            let body = line.strip_suffix("\r\n").or_else(|| line.strip_suffix('\n')).unwrap_or(line);
            if let Some(mt) = re.find_iter(body).last() {
                return Ok(Some(Some((base as usize + s + mt.start())..(base as usize + s + mt.end()))));
            }
            e = s;
        }
        if lo == 0 && start0 == r.start {
            break;
        }
        end = base;
        w = cfg.window;
        if !ctl.tick(r.end - end, r.end - r.start) {
            return Err(Cancelled);
        }
    }
    Ok(Some(None))
}

fn last_in(m: &Matcher, src: &dyn ByteSource, r: Range<u64>, cfg: &WindowCfg, ctl: &mut Ctl) -> Result<Option<Range<usize>>, Cancelled> {
    if !m.spans_lines() && r.start < r.end {
        if let Some(found) = last_backward(m, src, r.clone(), cfg, ctl)? {
            return Ok(found);
        }
    }
    last_forward(m, src, r, cfg, ctl)
}

/// Last match lying entirely before `before`; with `wrap`, restarts at the end.
pub fn find_prev(m: &Matcher, src: &dyn ByteSource, before: usize, wrap: bool, cfg: &WindowCfg, ctl: &mut Ctl) -> Result<Option<Found>, Cancelled> {
    let len = src.len();
    if let Some(range) = last_in(m, src, 0..(before as u64).min(len), cfg, ctl)? {
        return Ok(Some(Found { range, wrapped: false }));
    }
    if !wrap {
        return Ok(None);
    }
    Ok(last_in(m, src, 0..len, cfg, ctl)?.map(|range| Found { range, wrapped: true }))
}

// ------------------------------------------------------------------------------------------ replace

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ReplaceStats {
    pub count: u64,
    /// `on_edit` asked to stop.
    pub stopped: bool,
}

/// Stream the replacements of every match in `ranges` to `on_edit(range, text)` in ascending order and
/// ORIGINAL offsets. Nothing is accumulated (no `EditSet` of millions of edits): the caller decides
/// whether to apply (in-place on a store, bounded by [`MAX_INPLACE_EDITS`]) or to write a new file
/// ([`replace_to_sink`], [`replace_to_file`]).
pub fn replace_stream(
    m: &Matcher,
    tpl: &Template,
    src: &dyn ByteSource,
    ranges: &[Range<usize>],
    cfg: &WindowCfg,
    ctl: &mut Ctl,
    on_edit: &mut dyn FnMut(Range<u64>, String) -> bool,
) -> Result<ReplaceStats, Cancelled> {
    let mut st = ReplaceStats::default();
    let need_line = tpl.needs_line();
    for r in ranges_u64(ranges, src.len()) {
        let done = drive(m, src, r, cfg, ctl, true, need_line, &mut |h| {
            let caps = h.caps.expect("captures requested");
            let g = |n: usize| caps.get(n).map(|x| x.as_str());
            let text = tpl.expand(&MatchCtx { group: &g, line: h.line as usize + 1 });
            st.count += 1;
            if on_edit(h.range.clone(), text) {
                true
            } else {
                st.stopped = true;
                false
            }
        })?;
        if !done {
            break;
        }
    }
    Ok(st)
}

/// Applying a Replace All to a lazy store in place makes one piece per match (plus an undo record):
/// above this many matches use [`replace_to_file`] and reopen instead.
pub const MAX_INPLACE_EDITS: u64 = 100_000;

/// Write the whole source with every match replaced into `sink` (copy-through of untouched regions in
/// 1 MiB blocks). `io::ErrorKind::Interrupted` = cancelled.
pub fn replace_to_sink(
    m: &Matcher,
    tpl: &Template,
    src: &dyn ByteSource,
    ranges: &[Range<usize>],
    cfg: &WindowCfg,
    ctl: &mut Ctl,
    sink: &mut dyn FnMut(&[u8]) -> io::Result<()>,
) -> io::Result<ReplaceStats> {
    let mut copied = 0u64;
    let mut err: Option<io::Error> = None;
    let mut copy = |from: u64, to: u64, sink: &mut dyn FnMut(&[u8]) -> io::Result<()>| -> io::Result<()> {
        let mut e = None;
        read_blocks(src, from..to, 1 << 20, &mut |_, b| match sink(b) {
            Ok(()) => true,
            Err(x) => {
                e = Some(x);
                false
            }
        });
        e.map_or(Ok(()), Err)
    };
    let st = replace_stream(m, tpl, src, ranges, cfg, ctl, &mut |r, text| {
        let step = copy(copied, r.start, sink).and_then(|_| sink(text.as_bytes()));
        match step {
            Ok(()) => {
                copied = r.end;
                true
            }
            Err(e) => {
                err = Some(e);
                false
            }
        }
    })
    .map_err(|_| io::Error::new(io::ErrorKind::Interrupted, "cancelled"))?;
    if let Some(e) = err {
        return Err(e);
    }
    copy(copied, src.len(), sink)?;
    Ok(st)
}

/// Replace All into a new file atomically (`durable_write_with`); the target is untouched on cancel
/// or error. Peak memory: one search window + a 1 MiB write buffer.
pub fn replace_to_file(
    path: &std::path::Path,
    m: &Matcher,
    tpl: &Template,
    src: &dyn ByteSource,
    ranges: &[Range<usize>],
    cfg: &WindowCfg,
    ctl: &mut Ctl,
) -> io::Result<ReplaceStats> {
    use std::io::Write;
    let mut stats = ReplaceStats::default();
    crate::durable::durable_write_with(path, |f| {
        let mut w = io::BufWriter::with_capacity(1 << 20, &mut *f);
        stats = replace_to_sink(m, tpl, src, ranges, cfg, ctl, &mut |b| w.write_all(b))?;
        w.flush()
    })?;
    Ok(stats)
}

// ------------------------------------------------------------------------------------------ tests

#[cfg(test)]
mod tests {
    use super::*;
    use crate::search::{CancelToken, SearchMode, SearchOptions};
    use crop::Rope;
    use rand::{Rng, SeedableRng};
    use rand_chacha::ChaCha8Rng;

    const POOL: &[&str] = &["a", "b", "ab", "abc", "foo", "Foo", "bar", " ", "  ", "\n", "\r\n", "\r", "\u{e9}", "\u{4e2d}", "x1", "_", ".", "z", "\t", "end"];

    fn rand_text(rng: &mut ChaCha8Rng, n: usize) -> String {
        (0..n).map(|_| POOL[rng.gen_range(0..POOL.len())]).collect()
    }

    fn patterns() -> Vec<(&'static str, SearchOptions)> {
        let n = SearchOptions::default();
        let re = SearchOptions { mode: SearchMode::Regex, ..n };
        let ci = SearchOptions { match_case: false, ..n };
        let ww = SearchOptions { whole_word: true, ..n };
        let reww = SearchOptions { mode: SearchMode::Regex, whole_word: true, ..n };
        let dot = SearchOptions { mode: SearchMode::Regex, dot_all: true, ..n };
        let ext = SearchOptions { mode: SearchMode::Extended, ..n };
        vec![
            ("ab", n),
            ("abc", n),
            ("foo", ci),
            ("foo", ww),
            ("a\nb", n),
            ("\r\n", n),
            ("b\n", n),
            ("ab\\n", ext),
            ("a+b?", re),
            ("^a", re),
            ("a$", re),
            ("^$", re),
            ("o\\b", re),
            ("\\bfoo\\b", re),
            ("x1|ab|end", re),
            ("a.*?z", re),
            ("a.*?z", dot),
            ("b\\s+a", re),
            ("[^x]\\n", re),
            ("\\s+", re),
            ("a*", re),
            ("\\b", re),
            ("(?:a|\\u{e9})+", re),
            ("\u{4e2d}", n),
            ("foo", reww),
        ]
    }

    #[test]
    fn equivalent_to_the_whole_text_engine_on_random_inputs_and_tiny_windows() {
        let cancel = CancelToken::new();
        let mut rng = ChaCha8Rng::seed_from_u64(7);
        let cfgs = [WindowCfg::tiny(8, 2, 1 << 20), WindowCfg::tiny(24, 4, 1 << 20), WindowCfg::tiny(37, 9, 1 << 20), WindowCfg::default()];
        let mut checked = 0usize;
        for round in 0..40 {
            let n = if round % 10 == 0 { 0 } else { rng.gen_range(1..120) };
            let text = rand_text(&mut rng, n);
            let rope = Rope::from(text.as_str());
            let bytes = text.clone().into_bytes();
            for (pat, opts) in patterns() {
                let m = Matcher::new(pat, &opts).unwrap();
                let all = [0..rope.byte_len()];
                let want_all = m.find_all(&rope, &all, &cancel).unwrap();
                let want_count = m.count(&rope, &all, &cancel).unwrap();
                for cfg in &cfgs {
                    // Window cuts are only exact for matches within `margin`: for span patterns keep to that limit.
                    let limited = m.spans_lines() && cfg.margin < 100;
                    let ctl = &mut Ctl::new(&cancel);
                    let got = find_all(&m, &bytes, &all, usize::MAX, cfg, ctl).unwrap();
                    let cnt = count(&m, &bytes, &all, cfg, ctl).unwrap();
                    if limited && got.ranges != want_all {
                        // allowed to differ only when some expected match is longer than the margin
                        assert!(want_all.iter().any(|r| r.len() > cfg.margin / 2), "pattern {pat:?} cfg {cfg:?} text {text:?}: got {:?} want {:?}", got.ranges, want_all);
                        continue;
                    }
                    assert_eq!(got.ranges, want_all, "find_all {pat:?} {cfg:?} text {text:?}");
                    assert_eq!(cnt, want_count as u64, "count {pat:?} {cfg:?}");
                    for _ in 0..4 {
                        let from = (0..=bytes.len()).filter(|&i| text.is_char_boundary(i)).nth(rng.gen_range(0..=text.chars().count())).unwrap_or(0);
                        for wrap in [false, true] {
                            let a = m.find_next(&rope, from, wrap, &cancel).unwrap();
                            let b = find_next(&m, &bytes, from, wrap, cfg, ctl).unwrap();
                            if limited && a != b && want_all.iter().any(|r| r.len() > cfg.margin / 2) {
                                continue;
                            }
                            assert_eq!(b, a, "find_next {pat:?} from {from} wrap {wrap} {cfg:?} text {text:?}");
                            let a = m.find_prev(&rope, from, wrap, &cancel).unwrap();
                            let b = find_prev(&m, &bytes, from, wrap, cfg, ctl).unwrap();
                            if limited && a != b && want_all.iter().any(|r| r.len() > cfg.margin / 2) {
                                continue;
                            }
                            assert_eq!(b, a, "find_prev {pat:?} before {from} wrap {wrap} {cfg:?} text {text:?}");
                        }
                    }
                    checked += 1;
                }
            }
        }
        assert!(checked > 2000, "{checked}");
    }

    #[test]
    fn replace_is_equivalent_including_line_templates_and_captures() {
        let cancel = CancelToken::new();
        let mut rng = ChaCha8Rng::seed_from_u64(11);
        let cases: Vec<(&str, SearchOptions, &str)> = vec![
            ("foo", SearchOptions::default(), "<$0>"),
            ("(a)(b)?", SearchOptions { mode: SearchMode::Regex, ..Default::default() }, "[\\1|\\2]"),
            ("ab", SearchOptions::default(), "line\\L"),
            ("^", SearchOptions { mode: SearchMode::Regex, ..Default::default() }, "> "),
            ("\\s+", SearchOptions { mode: SearchMode::Regex, ..Default::default() }, "_"),
            ("a*", SearchOptions { mode: SearchMode::Regex, ..Default::default() }, "X"),
        ];
        for round in 0..30 {
            let text = rand_text(&mut rng, 5 + round * 3);
            let rope = Rope::from(text.as_str());
            for (pat, opts, rep) in &cases {
                let m = Matcher::new(pat, opts).unwrap();
                let tpl = Template::parse(rep);
                let want = m.replace(&rope, &[0..text.len()], &tpl, &cancel).unwrap();
                for cfg in [WindowCfg::tiny(8, 2, 1 << 20), WindowCfg::tiny(31, 5, 1 << 20)] {
                    let mut edits = Vec::new();
                    let st = replace_stream(&m, &tpl, &text.as_bytes(), &[0..text.len()], &cfg, &mut Ctl::new(&cancel), &mut |r, t| {
                        edits.push((r.start as usize, r.end as usize, t));
                        true
                    })
                    .unwrap();
                    let want_edits: Vec<(usize, usize, String)> = want.edits.0.iter().map(|e| (e.start_byte, e.end_byte, e.text.clone())).collect();
                    assert_eq!(edits, want_edits, "{pat:?} -> {rep:?} cfg {cfg:?} text {text:?}");
                    assert_eq!(st.count as usize, want.count);
                    // streamed whole-output equals applying the EditSet
                    let mut out = Vec::new();
                    replace_to_sink(&m, &tpl, &text.as_bytes(), &[0..text.len()], &cfg, &mut Ctl::new(&cancel), &mut |b| {
                        out.extend_from_slice(b);
                        Ok(())
                    })
                    .unwrap();
                    assert_eq!(String::from_utf8(out).unwrap(), want.edits.apply_to_str(&text).unwrap(), "{pat:?} output");
                }
            }
        }
    }

    #[test]
    fn matches_straddling_every_window_boundary_are_found() {
        let cancel = CancelToken::new();
        // A needle at every possible offset relative to windows of 16 bytes.
        for shift in 0..40 {
            let mut text = "x".repeat(shift);
            text.push_str("NEEDLE");
            text.push_str(&"y".repeat(50));
            text.push_str("NEEDLE\nNEEDLE");
            for (pat, regex) in [("NEEDLE", false), ("NEE.LE", true), ("LE\\nNE", true), ("NEEDLE\\nNEE", true)] {
                let opts = SearchOptions { mode: if regex { SearchMode::Regex } else { SearchMode::Normal }, ..Default::default() };
                let m = Matcher::new(pat, &opts).unwrap();
                let rope = Rope::from(text.as_str());
                let want = m.find_all(&rope, &[0..text.len()], &cancel).unwrap();
                for cfg in [WindowCfg::tiny(16, 12, 16), WindowCfg::tiny(20, 13, 64)] {
                    let got = find_all(&m, &text.as_bytes(), &[0..text.len()], 100, &cfg, &mut Ctl::new(&cancel)).unwrap();
                    assert_eq!(got.ranges, want, "shift {shift} pattern {pat:?} cfg {cfg:?}");
                }
            }
        }
    }

    #[test]
    fn long_line_beyond_the_cap_still_finds_matches_away_from_the_cut() {
        let cancel = CancelToken::new();
        let text = format!("{}needle{}needle\nshort needle\n", "a".repeat(300), "b".repeat(300));
        let m = Matcher::new("needle", &SearchOptions::default()).unwrap();
        let rope = Rope::from(text.as_str());
        let want = m.find_all(&rope, &[0..text.len()], &cancel).unwrap();
        let got = find_all(&m, &text.as_bytes(), &[0..text.len()], 10, &WindowCfg::tiny(64, 16, 128), &mut Ctl::new(&cancel)).unwrap();
        assert_eq!(got.ranges, want);
    }

    #[test]
    fn results_are_capped_cancel_and_progress_work() {
        let cancel = CancelToken::new();
        let text = "hit\n".repeat(1000);
        let m = Matcher::new("hit", &SearchOptions::default()).unwrap();
        let r = find_all(&m, &text.as_bytes(), &[0..text.len()], 10, &WindowCfg::tiny(64, 8, 128), &mut Ctl::new(&cancel)).unwrap();
        assert_eq!((r.ranges.len(), r.truncated), (10, true));
        let mut ticks = 0;
        let mut cb = |_d: u64, t: u64| {
            ticks += 1;
            assert_eq!(t, 4000);
            if ticks == 5 {
                cancel.cancel();
            }
        };
        let mut ctl = Ctl::new(&cancel).with_progress(&mut cb);
        let r = count(&m, &text.as_bytes(), &[0..text.len()], &WindowCfg::tiny(64, 8, 128), &mut ctl);
        assert_eq!(r, Err(Cancelled));
    }

    #[test]
    fn invalid_utf8_is_a_barrier_and_offsets_stay_exact() {
        let cancel = CancelToken::new();
        let mut bytes = b"one needle\n".to_vec();
        bytes.extend_from_slice(&[0xFF, 0xFE]);
        bytes.extend_from_slice(b"two needle\nne");
        bytes.push(0xC3); // dangling lead byte near the end
        let m = Matcher::new("needle", &SearchOptions::default()).unwrap();
        for cfg in [WindowCfg::tiny(8, 2, 8), WindowCfg::default()] {
            let r = find_all(&m, &bytes, &[0..bytes.len()], 10, &cfg, &mut Ctl::new(&cancel)).unwrap();
            assert_eq!(r.ranges, vec![4..10, 17..23], "{cfg:?}");
        }
    }

    #[test]
    fn replace_to_file_is_atomic_and_cancel_leaves_target_alone() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("t.txt");
        std::fs::write(&target, "keep me").unwrap();
        let text = "foo bar foo\n".repeat(200);
        let m = Matcher::new("foo", &SearchOptions::default()).unwrap();
        let tpl = Template::parse("quux");
        let cancel = CancelToken::new();
        let mut n = 0;
        let mut cb = |_: u64, _: u64| {
            n += 1;
            if n == 3 {
                cancel.cancel();
            }
        };
        let r = replace_to_file(&target, &m, &tpl, &text.as_bytes(), &[0..text.len()], &WindowCfg::tiny(64, 8, 128), &mut Ctl::new(&cancel).with_progress(&mut cb));
        assert!(matches!(r, Err(ref e) if e.kind() == io::ErrorKind::Interrupted));
        assert_eq!(std::fs::read_to_string(&target).unwrap(), "keep me");
        let cancel2 = CancelToken::new();
        let st = replace_to_file(&target, &m, &tpl, &text.as_bytes(), &[0..text.len()], &WindowCfg::tiny(64, 8, 128), &mut Ctl::new(&cancel2)).unwrap();
        assert_eq!(st.count, 400);
        assert_eq!(std::fs::read_to_string(&target).unwrap(), text.replace("foo", "quux"));
    }
}
