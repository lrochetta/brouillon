//! Live (non-blocking) search for the Find bar.
//!
//! The per-line walk of [`Matcher`] is exact but slow on huge files (about
//! 10 MB/s measured on a 105 MB text) and used to run on the UI thread, once
//! for the count and once for the first match, at every key press. This module
//! scans by ~1 MiB slices cut at line starts, so the regex engine and its
//! literal prefilters see big haystacks, and it reports progress through a
//! callback so a worker thread can publish "N matches so far" while the UI
//! stays free. Every loop checks the [`CancelToken`] between slices.
//!
//! Slicing is only used when a match cannot span lines and the pattern cannot
//! match the empty string; otherwise the exact per-line walk of [`Matcher`] is
//! used (same results, slower), still off the UI thread.

use std::borrow::Cow;
use std::ops::Range;
use std::sync::Arc;
use std::time::{Duration, Instant};

pub use crop::Rope;

use super::{slice_text, snap_range, CancelToken, Cancelled, Matcher};
use crate::buffer::BufferSnapshot;

/// What the scan needs from the text: lines and slices, nothing that implies "the whole text". Implemented by
/// the rope (small files) and by `BufferSnapshot` (rope or lazy store), so the same worker serves both.
pub trait Hay {
    fn byte_len(&self) -> usize;
    /// Terminated-line count (the rope convention).
    fn line_len(&self) -> usize;
    fn byte_of_line(&self, line: usize) -> usize;
    fn line_of_byte(&self, byte: usize) -> usize;
    fn is_char_boundary(&self, byte: usize) -> bool;
    fn text(&self, r: Range<usize>) -> Cow<'_, str>;
    /// The rope, when there is one (the exact per-line `Matcher` walk needs it; the sliced scan does not).
    fn as_rope(&self) -> Option<&Rope>;
    /// Windowed byte access, when the text has no rope: the multi-line / empty-matching patterns then run
    /// through `lazy_search` (1 MiB windows) instead of being refused.
    fn byte_source(&self) -> Option<&dyn crate::byte_source::ByteSource> {
        None
    }
}

impl Hay for Rope {
    fn byte_len(&self) -> usize {
        Rope::byte_len(self)
    }
    fn line_len(&self) -> usize {
        Rope::line_len(self)
    }
    fn byte_of_line(&self, line: usize) -> usize {
        Rope::byte_of_line(self, line)
    }
    fn line_of_byte(&self, byte: usize) -> usize {
        Rope::line_of_byte(self, byte)
    }
    fn is_char_boundary(&self, byte: usize) -> bool {
        Rope::is_char_boundary(self, byte)
    }
    fn text(&self, r: Range<usize>) -> Cow<'_, str> {
        slice_text(self.byte_slice(r))
    }
    fn as_rope(&self) -> Option<&Rope> {
        Some(self)
    }
}

impl Hay for BufferSnapshot {
    fn byte_len(&self) -> usize {
        BufferSnapshot::byte_len(self)
    }
    fn line_len(&self) -> usize {
        BufferSnapshot::line_count(self)
    }
    fn byte_of_line(&self, line: usize) -> usize {
        BufferSnapshot::byte_of_line(self, line)
    }
    fn line_of_byte(&self, byte: usize) -> usize {
        BufferSnapshot::line_of_byte(self, byte)
    }
    fn is_char_boundary(&self, byte: usize) -> bool {
        BufferSnapshot::is_char_boundary(self, byte)
    }
    fn text(&self, r: Range<usize>) -> Cow<'_, str> {
        match self {
            BufferSnapshot::Rope(rope) => slice_text(rope.byte_slice(r)),
            BufferSnapshot::Lazy(_) => Cow::Owned(self.slice(r)),
        }
    }
    fn as_rope(&self) -> Option<&Rope> {
        BufferSnapshot::as_rope(self)
    }
    fn byte_source(&self) -> Option<&dyn crate::byte_source::ByteSource> {
        Some(self)
    }
}

fn snap(h: &dyn Hay, mut b: usize) -> usize {
    b = b.min(h.byte_len());
    while !h.is_char_boundary(b) {
        b -= 1;
    }
    b
}

/// Slice size of the scan (bytes, rounded up to the next line start).
pub const SLICE: usize = 1 << 20;
/// Minimum delay between two progress reports.
const REPORT_EVERY: Duration = Duration::from_millis(50);

/// Which match to look for first.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    /// First match starting at or after the byte, wrapping to the top.
    Next(usize),
    /// Last match lying entirely before the byte, wrapping to the bottom.
    Prev(usize),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Job {
    pub dir: Direction,
    /// Also count every match of the whole text (progressively).
    pub count: bool,
    /// Pause (cancellable) between publishing the target and starting the count, so a burst of
    /// keystrokes only pays for the first-match search of each query.
    pub count_delay: Duration,
}

/// What a job knows so far.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Progress {
    /// Matches counted so far (final once `done`); only meaningful when the job counts.
    pub count: usize,
    pub scanned: usize,
    pub total: usize,
    /// The match asked for by [`Direction`] (`None` = none in the whole text).
    pub target: Option<Range<usize>>,
    /// The target is known (it may still be `None`).
    pub target_known: bool,
    pub done: bool,
    /// The pattern needs the whole text (it can span lines or match the empty string) and the text is a lazy
    /// large file: nothing was searched. The UI says "not available in large file mode for this pattern".
    pub unsupported: bool,
}

/// Can slices be used for this matcher (see the module docs)?
fn sliceable(m: &Matcher) -> bool {
    !m.spans_lines() && !m.regex().is_match("")
}

/// End of the slice starting at `start`: `SLICE` further, moved to a line start.
fn slice_end(rope: &dyn Hay, start: usize) -> usize {
    let len = rope.byte_len();
    let want = start.saturating_add(SLICE);
    if want >= len {
        return len;
    }
    let next = rope.line_of_byte(want) + 1;
    if next >= rope.line_len() {
        len
    } else {
        rope.byte_of_line(next)
    }
}

/// Start of the slice ending at `end`: `SLICE` earlier, moved back to a line start.
fn slice_start(rope: &dyn Hay, end: usize) -> usize {
    if end <= SLICE {
        return 0;
    }
    rope.byte_of_line(rope.line_of_byte(end - SLICE))
}

/// First match starting at or after `from` (no wrap).
fn first_from(rope: &dyn Hay, m: &Matcher, from: usize, cancel: &CancelToken) -> Result<Option<Range<usize>>, Cancelled> {
    let len = rope.byte_len();
    let from = snap(rope, from);
    if !sliceable(m) {
        if let Some(bs) = rope.byte_source().filter(|_| rope.as_rope().is_none()) {
            let mut ctl = crate::byte_source::Ctl::new(cancel);
            return Ok(crate::lazy_search::find_next(m, bs, from, false, &Default::default(), &mut ctl)?.map(|f| f.range));
        }
        let Some(r) = rope.as_rope() else { return Ok(None) };
        return Ok(m.find_next(r, from, false, cancel)?.map(|f| f.range));
    }
    let mut start = from;
    while start < len {
        if cancel.is_cancelled() {
            return Err(Cancelled);
        }
        let end = slice_end(rope, start);
        let text = rope.text(start..end);
        if let Some(h) = m.regex().find(&text) {
            return Ok(Some(start + h.start()..start + h.end()));
        }
        start = end;
    }
    Ok(None)
}

/// Last match lying entirely before `before` (no wrap).
fn last_before(rope: &dyn Hay, m: &Matcher, before: usize, cancel: &CancelToken) -> Result<Option<Range<usize>>, Cancelled> {
    let before = snap(rope, before);
    if !sliceable(m) {
        if let Some(bs) = rope.byte_source().filter(|_| rope.as_rope().is_none()) {
            let mut ctl = crate::byte_source::Ctl::new(cancel);
            return Ok(crate::lazy_search::find_prev(m, bs, before, false, &Default::default(), &mut ctl)?.map(|f| f.range));
        }
        let Some(r) = rope.as_rope() else { return Ok(None) };
        return Ok(m.find_prev(r, before, false, cancel)?.map(|f| f.range));
    }
    let mut end = before;
    while end > 0 {
        if cancel.is_cancelled() {
            return Err(Cancelled);
        }
        let start = slice_start(rope, end);
        let text = rope.text(start..end);
        if let Some(h) = m.regex().find_iter(&text).last() {
            return Ok(Some(start + h.start()..start + h.end()));
        }
        end = start;
    }
    Ok(None)
}

/// Count matches over the whole text, reporting `(count, scanned)` regularly.
fn count_all(rope: &dyn Hay, m: &Matcher, cancel: &CancelToken, report: &mut dyn FnMut(usize, usize)) -> Result<usize, Cancelled> {
    let len = rope.byte_len();
    if !sliceable(m) {
        if let Some(bs) = rope.byte_source().filter(|_| rope.as_rope().is_none()) {
            let n = {
                let mut cb = |done: u64, _t: u64| report(0, done as usize);
                let mut ctl = crate::byte_source::Ctl::new(cancel).with_progress(&mut cb);
                crate::lazy_search::count(m, bs, &[0..len], &Default::default(), &mut ctl)? as usize
            };
            report(n, len);
            return Ok(n);
        }
        let Some(r) = rope.as_rope() else { return Ok(0) };
        let n = m.count(r, &[0..len], cancel)?;
        report(n, len);
        return Ok(n);
    }
    let (mut n, mut start, mut last) = (0, 0, Instant::now());
    while start < len {
        if cancel.is_cancelled() {
            return Err(Cancelled);
        }
        let end = slice_end(rope, start);
        n += m.regex().find_iter(&rope.text(start..end)).count();
        start = end;
        if last.elapsed() >= REPORT_EVERY && start < len {
            report(n, start);
            last = Instant::now();
        }
    }
    Ok(n)
}

/// Run `job` on the calling thread. `sink` receives the target as soon as it is
/// known, then the running count, then a final `done` report. Returns
/// `Err(Cancelled)` (nothing more is reported) when `cancel` fired.
pub fn run(rope: &dyn Hay, m: &Matcher, job: &Job, cancel: &CancelToken, sink: &mut dyn FnMut(&Progress)) -> Result<(), Cancelled> {
    let total = rope.byte_len();
    let mut p = Progress { total, ..Progress::default() };
    if rope.as_rope().is_none() && rope.byte_source().is_none() && !sliceable(m) {
        // A lazy large file cannot run the exact whole-text walk that multi-line / empty-matching patterns need.
        p.unsupported = true;
        p.target_known = true;
        p.done = true;
        sink(&p);
        return Ok(());
    }
    let target = match job.dir {
        Direction::Next(from) => match first_from(rope, m, from, cancel)? {
            Some(r) => Some(r),
            None => first_from(rope, m, 0, cancel)?,
        },
        Direction::Prev(before) => match last_before(rope, m, before, cancel)? {
            Some(r) => Some(r),
            None => last_before(rope, m, total, cancel)?,
        },
    };
    p.target = target;
    p.target_known = true;
    p.done = !job.count;
    sink(&p);
    if !job.count {
        return Ok(());
    }
    let waited = Instant::now();
    while waited.elapsed() < job.count_delay {
        if cancel.is_cancelled() {
            return Err(Cancelled);
        }
        std::thread::sleep(Duration::from_millis(10).min(job.count_delay));
    }
    let count = count_all(rope, m, cancel, &mut |count, scanned| {
        sink(&Progress { count, scanned, ..p.clone() });
    })?;
    p.count = count;
    p.scanned = total;
    p.done = true;
    sink(&p);
    Ok(())
}

/// Run `job` on a worker thread. The sink runs on that thread: keep it cheap
/// (store, then wake the UI).
pub fn spawn<H: Hay + Send + 'static>(
    rope: H,
    matcher: Arc<Matcher>,
    job: Job,
    cancel: CancelToken,
    mut sink: impl FnMut(&Progress) + Send + 'static,
) -> std::thread::JoinHandle<()> {
    std::thread::spawn(move || {
        let _ = run(&rope as &dyn Hay, &matcher, &job, &cancel, &mut sink);
    })
}

/// Matches intersecting `range` (the visible part of the text), at most `limit`:
/// what a highlight pass needs, without a scan of the whole file.
pub fn matches_in(rope: &Rope, m: &Matcher, range: Range<usize>, limit: usize) -> Vec<Range<usize>> {
    let range = snap_range(rope, range);
    m.find_all(rope, &[range], &CancelToken::new())
        .unwrap_or_default()
        .into_iter()
        .take(limit)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::search::{SearchMode, SearchOptions};

    fn opts(mode: SearchMode, match_case: bool, whole_word: bool) -> SearchOptions {
        SearchOptions { mode, match_case, whole_word, ..SearchOptions::default() }
    }

    fn matcher(p: &str, o: &SearchOptions) -> Matcher {
        Matcher::new(p, o).unwrap()
    }

    /// Text made of many slices (SLICE is 1 MiB, lines are short).
    fn big_text() -> String {
        let mut s = String::new();
        let mut i = 0usize;
        while s.len() < 2 * SLICE + 12345 {
            s.push_str(&format!("line {i} Foo bar foobar baz\n"));
            i += 1;
        }
        s.push_str("tail without newline FOO");
        s
    }

    fn collect(rope: &Rope, m: &Matcher, job: Job) -> (Progress, usize) {
        let mut last = Progress::default();
        let mut reports = 0;
        run(rope, m, &job, &CancelToken::new(), &mut |p| {
            last = p.clone();
            reports += 1;
        })
        .unwrap();
        (last, reports)
    }

    #[test]
    fn count_matches_reference_on_many_slices() {
        let text = big_text();
        let rope = Rope::from(text.as_str());
        let cases = [
            ("foo", opts(SearchMode::Normal, false, false)),
            ("foo", opts(SearchMode::Normal, true, false)),
            ("Foo", opts(SearchMode::Normal, true, false)),
            ("foo", opts(SearchMode::Normal, false, true)),
            (r"fo+\w*", opts(SearchMode::Regex, true, false)),
            (r"b[a-z]+z$", opts(SearchMode::Regex, true, false)),
            ("zzz-nothing", opts(SearchMode::Normal, true, false)),
        ];
        for (pat, o) in cases {
            let m = matcher(pat, &o);
            let reference = m.count(&rope, &[0..rope.byte_len()], &CancelToken::new()).unwrap();
            let (p, _) = collect(&rope, &m, Job { dir: Direction::Next(0), count: true, count_delay: Duration::ZERO });
            assert_eq!(p.count, reference, "{pat:?} {o:?}");
            assert!(p.done);
            assert_eq!(p.scanned, rope.byte_len());
        }
    }

    #[test]
    fn whole_word_and_case_are_honoured() {
        let rope = Rope::from("foo Foo foobar FOO\nfoo");
        let ci = matcher("foo", &opts(SearchMode::Normal, false, false));
        let cs = matcher("foo", &opts(SearchMode::Normal, true, false));
        let ww = matcher("foo", &opts(SearchMode::Normal, false, true));
        let n = |m: &Matcher| collect(&rope, m, Job { dir: Direction::Next(0), count: true, count_delay: Duration::ZERO }).0.count;
        assert_eq!((n(&ci), n(&cs), n(&ww)), (5, 3, 4));
    }

    #[test]
    fn next_and_prev_match_the_reference_and_wrap() {
        let text = big_text();
        let rope = Rope::from(text.as_str());
        let m = matcher("foobar", &opts(SearchMode::Normal, true, false));
        let cancel = CancelToken::new();
        let len = rope.byte_len();
        for from in [0, 1, SLICE - 3, SLICE + 7, 2 * SLICE, len - 30, len] {
            let want = m
                .find_next(&rope, from, true, &cancel)
                .unwrap()
                .map(|f| f.range);
            let (p, _) = collect(&rope, &m, Job { dir: Direction::Next(from), count: false, count_delay: Duration::ZERO });
            assert!(p.target_known && p.done);
            assert_eq!(p.target, want, "next from {from}");

            let want = m.find_prev(&rope, from, true, &cancel).unwrap().map(|f| f.range);
            let (p, _) = collect(&rope, &m, Job { dir: Direction::Prev(from), count: false, count_delay: Duration::ZERO });
            assert_eq!(p.target, want, "prev before {from}");
        }
    }

    #[test]
    fn no_match_gives_none_and_known() {
        let rope = Rope::from("abc\ndef\n");
        let m = matcher("zzz", &opts(SearchMode::Normal, true, false));
        let (p, _) = collect(&rope, &m, Job { dir: Direction::Next(0), count: true, count_delay: Duration::ZERO });
        assert!(p.target_known && p.target.is_none() && p.count == 0 && p.done);
    }

    #[test]
    fn target_is_reported_before_the_count() {
        let text = big_text();
        let rope = Rope::from(text.as_str());
        let m = matcher("foo", &opts(SearchMode::Normal, false, false));
        let mut first = None;
        run(&rope, &m, &Job { dir: Direction::Next(0), count: true, count_delay: Duration::ZERO }, &CancelToken::new(), &mut |p| {
            if first.is_none() {
                first = Some(p.clone());
            }
        })
        .unwrap();
        let f = first.unwrap();
        assert!(f.target_known && !f.done && f.count == 0);
        assert_eq!(f.target, Some(7..10));
    }

    #[test]
    fn cancelled_job_reports_nothing_after_cancel_and_returns_err() {
        let text = big_text();
        let rope = Rope::from(text.as_str());
        let m = matcher("foo", &opts(SearchMode::Normal, false, false));
        let cancel = CancelToken::new();
        let mut seen_done = false;
        let c2 = cancel.clone();
        let r = run(&rope, &m, &Job { dir: Direction::Next(0), count: true, count_delay: Duration::ZERO }, &cancel, &mut |p| {
            if p.target_known {
                c2.cancel();
            }
            seen_done |= p.done;
        });
        assert_eq!(r, Err(Cancelled));
        assert!(!seen_done);
    }

    #[test]
    fn spawned_job_honours_a_cancel_token() {
        let text = "x".repeat(200) + "\n";
        let rope = Rope::from(text.repeat(50_000).as_str());
        let m = Arc::new(matcher("x+y", &opts(SearchMode::Regex, true, false)));
        let cancel = CancelToken::new();
        let (tx, rx) = std::sync::mpsc::channel();
        cancel.cancel();
        let h = spawn(rope, m, Job { dir: Direction::Next(0), count: true, count_delay: Duration::ZERO }, cancel, move |p| {
            let _ = tx.send(p.clone());
        });
        h.join().unwrap();
        assert!(rx.try_iter().all(|p| !p.done), "a cancelled job never reports done");
    }

    #[test]
    fn multiline_and_empty_matching_patterns_use_the_exact_path() {
        let rope = Rope::from("a\nb\na\nb\n");
        let m = matcher(r"a\nb", &opts(SearchMode::Regex, true, false));
        assert!(!sliceable(&m));
        assert_eq!(collect(&rope, &m, Job { dir: Direction::Next(0), count: true, count_delay: Duration::ZERO }).0.count, 2);
        let e = matcher(r"^", &opts(SearchMode::Regex, true, false));
        assert!(!sliceable(&e));
        let reference = e.count(&rope, &[0..rope.byte_len()], &CancelToken::new()).unwrap();
        assert_eq!(collect(&rope, &e, Job { dir: Direction::Next(0), count: true, count_delay: Duration::ZERO }).0.count, reference);
    }

    #[test]
    fn crlf_text_counts_like_the_reference() {
        let text = "foo bar\r\nbaz foo\r\nfoo\r\n".repeat(60_000);
        let rope = Rope::from(text.as_str());
        let m = matcher(r"foo$", &opts(SearchMode::Regex, true, false));
        let reference = m.count(&rope, &[0..rope.byte_len()], &CancelToken::new()).unwrap();
        assert_eq!(reference, 120_000);
        assert_eq!(collect(&rope, &m, Job { dir: Direction::Next(0), count: true, count_delay: Duration::ZERO }).0.count, reference);
    }

    #[test]
    fn matches_in_returns_only_the_requested_window() {
        let rope = Rope::from("foo\nfoo\nfoo\nfoo\n");
        let m = matcher("foo", &opts(SearchMode::Normal, true, false));
        assert_eq!(matches_in(&rope, &m, 4..12, 10), vec![4..7, 8..11]);
        assert_eq!(matches_in(&rope, &m, 0..16, 3).len(), 3);
    }

    /// Reproducible benchmark: `BROUILLON_PERF_FILE=<path> cargo test -p brouillon-editor-engine
    /// --release live::tests::bench_big_file -- --ignored --nocapture`.
    /// Without the variable a 100 MB file is generated in the temp dir and removed afterwards.
    #[test]
    #[ignore]
    fn bench_big_file() {
        let (path, generated) = match std::env::var("BROUILLON_PERF_FILE") {
            Ok(p) => (std::path::PathBuf::from(p), false),
            Err(_) => {
                let p = std::env::temp_dir().join("brouillon-live-bench.txt");
                let mut f = std::io::BufWriter::new(std::fs::File::create(&p).unwrap());
                use std::io::Write;
                for i in 0..1_050_000u32 {
                    writeln!(f, "{i:07} the quick brown fox jumps over the lazy dog, lorem ipsum dolor sit amet consectetur adipiscing elit, sed do").unwrap();
                }
                writeln!(f, "ZZUNIQUEZZ last line").unwrap();
                (p, true)
            }
        };
        let text = std::fs::read_to_string(&path).unwrap();
        let rope = Rope::from(text.as_str());
        println!("file: {} MB", text.len() / (1 << 20));
        for (pat, o) in [
            ("lazy dog", opts(SearchMode::Normal, true, false)),
            ("LAZY DOG", opts(SearchMode::Normal, false, false)),
            ("ZZUNIQUEZZ", opts(SearchMode::Normal, false, false)),
            ("zzabsentzz", opts(SearchMode::Normal, false, false)),
            (r"fox\s+jumps", opts(SearchMode::Regex, true, false)),
        ] {
            let m = matcher(pat, &o);
            let t = Instant::now();
            let mut first_ms = None;
            let mut count = 0;
            run(&rope, &m, &Job { dir: Direction::Next(0), count: true, count_delay: Duration::ZERO }, &CancelToken::new(), &mut |p| {
                if p.target_known && first_ms.is_none() {
                    first_ms = Some(t.elapsed().as_millis());
                }
                count = p.count;
            })
            .unwrap();
            let t2 = Instant::now();
            let p = collect(&rope, &m, Job { dir: Direction::Prev(text.len() / 2), count: false, count_delay: Duration::ZERO }).0;
            println!(
                "{pat:?}: first match {} ms, full count {count} in {} ms, prev from middle {} ms ({:?})",
                first_ms.unwrap(),
                t.elapsed().as_millis(),
                t2.elapsed().as_millis(),
                p.target.map(|r| r.start)
            );
        }
        if generated {
            let _ = std::fs::remove_file(&path);
        }
    }
}
