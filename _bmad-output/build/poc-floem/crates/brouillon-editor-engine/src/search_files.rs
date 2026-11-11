//! Lot 4 — Find in Files engine (BK-023, BK-026).
//!
//! Parallel, `.gitignore`-aware walk (`ignore`), include/exclude globs
//! (`globset`), encoding-aware reads, streaming per-file results and a
//! cancel token. Matching is delegated to the [`TextMatcher`] trait so the
//! Lot 3 engine can plug in its own option parity (whole word, extended
//! escapes, ...); [`RegexMatcher`] is the minimal built-in implementation.

use std::collections::HashMap;
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;

use brouillon_core::contract::{CmdCtx, CmdOutcome, CommandSpec, PanelId};
use encoding_rs::Encoding;
use globset::{GlobBuilder, GlobSet, GlobSetBuilder};
use ignore::{WalkBuilder, WalkState};
use regex::{Regex, RegexBuilder};

/// Panel ids the shell maps to the Find in Files dialog / results panel.
pub const PANEL_FIND_IN_FILES: &str = "find_in_files";
pub const PANEL_REPLACE_IN_FILES: &str = "replace_in_files";
pub const PANEL_RESULTS: &str = "search_results";

fn spec(id: &str, caption: &str, chord: Option<&str>) -> CommandSpec {
    CommandSpec {
        id: id.into(),
        caption: caption.into(),
        category: "Search".into(),
        default_chord: chord.map(String::from),
        when: None, ..Default::default()
    }
}

/// Commands this module contributes to the registry.
pub fn commands() -> Vec<CommandSpec> {
    vec![
        spec("find_in_files", "Search: Find in Files...", Some("ctrl+shift+f")),
        spec("replace_in_files", "Search: Replace in Files...", Some("ctrl+shift+h")),
        spec("results_next_hit", "Search: Next Result", Some("f4")),
        spec("results_prev_hit", "Search: Previous Result", Some("shift+f4")),
    ]
}

/// Run command `id`; `None` = not mine. These commands only ask the shell to
/// open/focus a panel; the shell drives `ResultsModel` for next/prev.
pub fn run(id: &str, ctx: &CmdCtx) -> Option<CmdOutcome> {
    let _ = ctx;
    let panel = match id {
        "find_in_files" => PANEL_FIND_IN_FILES,
        "replace_in_files" => PANEL_REPLACE_IN_FILES,
        "results_next_hit" | "results_prev_hit" => PANEL_RESULTS,
        _ => return None,
    };
    Some(CmdOutcome { open_panel: Some(PanelId(panel.into())), ..CmdOutcome::default() })
}

// ---------------------------------------------------------------- matching

/// Matching interface used by Find/Replace in Files. Lot 3 may implement it
/// on top of its streaming engine; the walker only needs these two methods.
pub trait TextMatcher: Send + Sync {
    /// All non-overlapping match ranges (byte offsets) in `text`, ascending.
    fn find_all(&self, text: &str) -> Vec<Range<usize>>;
    /// Replacement text for the match `m` previously returned by `find_all`.
    fn replacement(&self, text: &str, m: &Range<usize>) -> String;
}

/// Built-in matcher: literal or regex, optional case/whole-word, and a
/// replacement using regex `$1` / `${name}` syntax (or literal text).
#[derive(Debug, Clone)]
pub struct RegexMatcher {
    re: Regex,
    replace_with: String,
    literal_replacement: bool,
}

impl RegexMatcher {
    pub fn new(pattern: &str, regex: bool, case_sensitive: bool, whole_word: bool) -> Result<Self, String> {
        if pattern.is_empty() {
            return Err("empty search pattern".into());
        }
        let mut p = if regex { pattern.to_string() } else { regex::escape(pattern) };
        if whole_word {
            p = format!(r"\b(?:{p})\b");
        }
        let re = RegexBuilder::new(&p)
            .case_insensitive(!case_sensitive)
            .multi_line(true)
            .build()
            .map_err(|e| e.to_string())?;
        Ok(Self { re, replace_with: String::new(), literal_replacement: !regex })
    }

    /// Set the replacement (used only by Replace in Files).
    pub fn with_replacement(mut self, replace_with: &str) -> Self {
        self.replace_with = replace_with.to_string();
        self
    }
}

impl TextMatcher for RegexMatcher {
    fn find_all(&self, text: &str) -> Vec<Range<usize>> {
        self.re.find_iter(text).filter(|m| !m.is_empty()).map(|m| m.range()).collect()
    }

    fn replacement(&self, text: &str, m: &Range<usize>) -> String {
        if self.literal_replacement {
            return self.replace_with.clone();
        }
        let mut out = String::new();
        if let Some(caps) = self.re.captures_at(text, m.start) {
            caps.expand(&self.replace_with, &mut out);
        }
        out
    }
}

// ---------------------------------------------------------------- decoding

/// A file decoded to text, remembering how to write it back.
#[derive(Debug, Clone)]
pub struct Decoded {
    pub text: String,
    pub encoding: &'static Encoding,
    pub bom: bool,
}

/// BOM > valid UTF-8 > chardetng guess. `None` = binary or undecodable.
pub fn decode_bytes(bytes: &[u8]) -> Option<Decoded> {
    if let Some((enc, bom_len)) = Encoding::for_bom(bytes) {
        let (text, had_errors) = enc.decode_without_bom_handling(&bytes[bom_len..]);
        return (!had_errors).then(|| Decoded { text: text.into_owned(), encoding: enc, bom: true });
    }
    if bytes[..bytes.len().min(8192)].contains(&0) {
        return None;
    }
    if let Ok(s) = std::str::from_utf8(bytes) {
        return Some(Decoded { text: s.to_string(), encoding: encoding_rs::UTF_8, bom: false });
    }
    let mut det = chardetng::EncodingDetector::new();
    det.feed(bytes, true);
    let enc = det.guess(None, true);
    let (text, had_errors) = enc.decode_without_bom_handling(bytes);
    (!had_errors).then(|| Decoded { text: text.into_owned(), encoding: enc, bom: false })
}

impl Decoded {
    /// Re-encode `text` with the original encoding and BOM.
    pub fn encode(&self, text: &str) -> Result<Vec<u8>, String> {
        let enc = self.encoding;
        let mut out = Vec::with_capacity(text.len() + 3);
        if enc == encoding_rs::UTF_16LE || enc == encoding_rs::UTF_16BE {
            let le = enc == encoding_rs::UTF_16LE;
            if self.bom {
                out.extend(if le { [0xFF, 0xFE] } else { [0xFE, 0xFF] });
            }
            for u in text.encode_utf16() {
                out.extend(if le { u.to_le_bytes() } else { u.to_be_bytes() });
            }
            return Ok(out);
        }
        if enc == encoding_rs::UTF_8 {
            if self.bom {
                out.extend([0xEF, 0xBB, 0xBF]);
            }
            out.extend(text.as_bytes());
            return Ok(out);
        }
        let (bytes, _, unmappable) = enc.encode(text);
        if unmappable {
            return Err(format!("text not representable in {}", enc.name()));
        }
        Ok(bytes.into_owned())
    }
}

// ----------------------------------------------------------------- options

/// Cooperative cancel flag shared with the UI thread.
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

/// Where to search (BK-026). The project folder list is injected via
/// [`ProjectFolders`] so this module knows nothing about workspaces.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ScopePreset {
    Folder(PathBuf),
    OpenFiles(Vec<PathBuf>),
    CurrentFileDir(PathBuf),
    ProjectFolders,
}

pub trait ProjectFolders {
    fn folders(&self) -> Vec<PathBuf>;
}

impl ScopePreset {
    /// Resolve to `(roots, recurse)`. `CurrentFileDir` takes a file path and
    /// searches its directory only (no subfolders).
    pub fn resolve(&self, project: &dyn ProjectFolders) -> (Vec<PathBuf>, bool) {
        match self {
            Self::Folder(p) => (vec![p.clone()], true),
            Self::OpenFiles(files) => (files.clone(), false),
            Self::CurrentFileDir(file) => {
                (vec![file.parent().map(Path::to_path_buf).unwrap_or_else(|| PathBuf::from("."))], false)
            }
            Self::ProjectFolders => (project.folders(), true),
        }
    }
}

#[derive(Debug, Clone)]
pub struct FindOptions {
    /// Directories to walk and/or individual files.
    pub roots: Vec<PathBuf>,
    /// `*.rs;*.toml`; empty = everything. Tokens starting with `!` exclude.
    pub include: String,
    /// `target;node_modules` (a leading `!` is accepted and ignored).
    pub exclude: String,
    pub recurse: bool,
    pub hidden: bool,
    pub respect_gitignore: bool,
    pub max_file_size: u64,
    /// Unsaved open buffers, keyed by the exact path the walker yields.
    pub buffers: HashMap<PathBuf, String>,
}

impl FindOptions {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self {
            roots: vec![root.into()],
            include: String::new(),
            exclude: String::new(),
            recurse: true,
            hidden: false,
            respect_gitignore: true,
            max_file_size: 10 * 1024 * 1024,
            buffers: HashMap::new(),
        }
    }
}

/// One match inside a file. `line`/`col` are 0-based; `col` is a byte column.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hit {
    pub line: usize,
    pub col: usize,
    pub start_byte: usize,
    pub end_byte: usize,
    /// The full line containing the match start, without EOL.
    pub line_text: String,
    pub matched: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileResult {
    pub path: PathBuf,
    pub hits: Vec<Hit>,
    /// Hits come from an unsaved open buffer, not from the disk content.
    pub from_buffer: bool,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SearchStats {
    pub files_scanned: usize,
    pub files_with_hits: usize,
    pub hits: usize,
    pub cancelled: bool,
}

fn split_tokens(s: &str) -> impl Iterator<Item = &str> {
    s.split(';').map(str::trim).filter(|t| !t.is_empty())
}

fn globset(patterns: &[String]) -> Result<GlobSet, String> {
    let mut b = GlobSetBuilder::new();
    for p in patterns {
        let g = GlobBuilder::new(p)
            .case_insensitive(cfg!(windows))
            .build()
            .map_err(|e| format!("bad glob '{p}': {e}"))?;
        b.add(g);
    }
    b.build().map_err(|e| e.to_string())
}

/// `(include, exclude)` glob sets from the option strings.
fn compile_filters(opts: &FindOptions) -> Result<(Option<GlobSet>, GlobSet), String> {
    let mut inc = Vec::new();
    let mut exc = Vec::new();
    for t in split_tokens(&opts.include) {
        match t.strip_prefix('!') {
            Some(x) => exc.push(x.trim().to_string()),
            None => inc.push(t.to_string()),
        }
    }
    for t in split_tokens(&opts.exclude) {
        exc.push(t.trim_start_matches('!').trim().to_string());
    }
    let inc_set = if inc.is_empty() {
        None
    } else {
        // Patterns without a separator match the file name at any depth.
        let pats: Vec<String> = inc
            .into_iter()
            .flat_map(|p| if p.contains('/') { vec![p] } else { vec![p.clone(), format!("**/{p}")] })
            .collect();
        Some(globset(&pats)?)
    };
    let exc_pats: Vec<String> = exc
        .into_iter()
        .filter(|p| !p.is_empty())
        .flat_map(|p| {
            let p = p.trim_end_matches('/').to_string();
            vec![p.clone(), format!("**/{p}"), format!("{p}/**"), format!("**/{p}/**")]
        })
        .collect();
    Ok((inc_set, globset(&exc_pats)?))
}

fn rel_path(path: &Path, roots: &[PathBuf]) -> String {
    let rel = roots.iter().find_map(|r| path.strip_prefix(r).ok()).unwrap_or(path);
    rel.to_string_lossy().replace('\\', "/")
}

fn hits_in(text: &str, matcher: &dyn TextMatcher) -> Vec<Hit> {
    let mut hits = Vec::new();
    let (mut line, mut line_start, mut scanned) = (0usize, 0usize, 0usize);
    for m in matcher.find_all(text) {
        let newlines = text.as_bytes()[scanned..m.start].iter().filter(|&&b| b == b'\n').count();
        if newlines > 0 {
            line += newlines;
            line_start = text[..m.start].rfind('\n').map_or(0, |i| i + 1);
        }
        scanned = m.start;
        let line_end = text[line_start..].find('\n').map_or(text.len(), |i| line_start + i);
        hits.push(Hit {
            line,
            col: m.start - line_start,
            start_byte: m.start,
            end_byte: m.end,
            line_text: text[line_start..line_end].trim_end_matches('\r').to_string(),
            matched: text[m.clone()].to_string(),
        });
    }
    hits
}

/// Chunk size for streamed (big) files; each chunk is extended to end on a newline.
const STREAM_CHUNK: usize = 4 * 1024 * 1024;
/// Per-file hit cap for streamed files (keeps the results panel and memory bounded).
const STREAM_MAX_HITS: usize = 10_000;

/// Search a big file chunk by chunk (memory stays ~one chunk). Line-oriented:
/// a match cannot span a chunk boundary. UTF-8 only (UTF-16 / binary: `None`).
fn stream_hits(path: &Path, matcher: &dyn TextMatcher, cancel: &CancelToken) -> Option<Vec<Hit>> {
    use std::io::{BufRead, BufReader, Read};
    let mut rd = BufReader::with_capacity(1 << 20, std::fs::File::open(path).ok()?);
    let mut hits: Vec<Hit> = Vec::new();
    let (mut base, mut base_line, mut first) = (0usize, 0usize, true);
    let mut buf: Vec<u8> = Vec::with_capacity(STREAM_CHUNK + 4096);
    loop {
        if cancel.is_cancelled() || hits.len() >= STREAM_MAX_HITS {
            break;
        }
        buf.clear();
        let n = (&mut rd).take(STREAM_CHUNK as u64).read_to_end(&mut buf).ok()?;
        if n == 0 {
            break;
        }
        if buf.last() != Some(&b'\n') {
            rd.read_until(b'\n', &mut buf).ok()?;
        }
        let mut slice = &buf[..];
        if first {
            first = false;
            if slice.starts_with(&[0xFF, 0xFE]) || slice.starts_with(&[0xFE, 0xFF]) {
                return stream_hits_decoded(path, matcher, cancel); // UTF-16 / UTF-32: decoded window by window
            }
            if slice[..slice.len().min(8192)].contains(&0) {
                return None;
            }
            if matches!(std::str::from_utf8(slice), Err(e) if e.error_len().is_some()) {
                // not UTF-8: a legacy codepage (Shift_JIS, GBK, windows-125x, ...) decoded window by window
                if matches!(crate::stream_codec::plan_open(path, "windows-1252"), Ok(crate::stream_codec::OpenPlan::Transcode { .. })) {
                    return stream_hits_decoded(path, matcher, cancel);
                }
            }
            if slice.starts_with(&[0xEF, 0xBB, 0xBF]) {
                slice = &slice[3..];
                base += 3;
            }
        }
        let text = String::from_utf8_lossy(slice);
        for mut h in hits_in(&text, matcher) {
            h.line += base_line;
            h.start_byte += base;
            h.end_byte += base;
            hits.push(h);
        }
        base += slice.len();
        base_line += slice.iter().filter(|&&b| b == b'\n').count();
    }
    hits.truncate(STREAM_MAX_HITS);
    Some(hits)
}

/// Big file in UTF-16/32 or a legacy codepage: decode it in line-aligned UTF-8 windows (memory stays
/// one window) and search each. Hit offsets are in the DECODED text, like the in-memory path.
fn stream_hits_decoded(path: &Path, matcher: &dyn TextMatcher, cancel: &CancelToken) -> Option<Vec<Hit>> {
    let crate::stream_codec::OpenPlan::Transcode { codepage } = crate::stream_codec::plan_open(path, "windows-1252").ok()? else { return None };
    let mut hits: Vec<Hit> = Vec::new();
    let (mut base, mut base_line) = (0usize, 0usize);
    crate::stream_codec::for_each_decoded_window(path, codepage, STREAM_CHUNK, &|| cancel.is_cancelled(), &mut |text| {
        for mut h in hits_in(text, matcher) {
            h.line += base_line;
            h.start_byte += base;
            h.end_byte += base;
            hits.push(h);
        }
        base += text.len();
        base_line += text.bytes().filter(|&b| b == b'\n').count();
        hits.len() < STREAM_MAX_HITS
    })
    .ok()?;
    hits.truncate(STREAM_MAX_HITS);
    Some(hits)
}

/// Search `opts.roots` in parallel, calling `on_file` (from worker threads)
/// for each file with at least one hit as soon as it is scanned.
pub fn search_files(
    opts: &FindOptions,
    matcher: &dyn TextMatcher,
    cancel: &CancelToken,
    on_file: &(dyn Fn(FileResult) + Sync),
) -> Result<SearchStats, String> {
    let (include, exclude) = compile_filters(opts)?;
    let (include, exclude) = (include.map(Arc::new), Arc::new(exclude));
    let roots = Arc::new(opts.roots.clone());
    let first = opts.roots.first().ok_or("no search root")?;

    let mut wb = WalkBuilder::new(first);
    for r in &opts.roots[1..] {
        wb.add(r);
    }
    wb.hidden(!opts.hidden)
        .git_ignore(opts.respect_gitignore)
        .git_global(opts.respect_gitignore)
        .git_exclude(opts.respect_gitignore)
        .ignore(opts.respect_gitignore)
        .parents(opts.respect_gitignore)
        .require_git(false);
    if !opts.recurse {
        wb.max_depth(Some(1));
    }
    {
        let (exclude, roots) = (exclude.clone(), roots.clone());
        wb.filter_entry(move |e| e.depth() == 0 || !exclude.is_match(rel_path(e.path(), &roots)));
    }

    let scanned = AtomicUsize::new(0);
    let with_hits = AtomicUsize::new(0);
    let total = AtomicUsize::new(0);
    wb.build_parallel().run(|| {
        let (include, exclude, roots) = (include.clone(), exclude.clone(), roots.clone());
        let (scanned, with_hits, total) = (&scanned, &with_hits, &total);
        Box::new(move |entry| {
            if cancel.is_cancelled() {
                return WalkState::Quit;
            }
            let Ok(entry) = entry else { return WalkState::Continue };
            let path = entry.path();
            if !entry.file_type().is_some_and(|t| t.is_file()) {
                return WalkState::Continue;
            }
            let rel = rel_path(path, &roots);
            if exclude.is_match(&rel) || include.as_ref().is_some_and(|i| !i.is_match(&rel)) {
                return WalkState::Continue;
            }
            let (hits, from_buffer) = match opts.buffers.get(path) {
                Some(t) => (hits_in(t, matcher), true),
                None => {
                    let Ok(len) = entry.metadata().map(|m| m.len()) else { return WalkState::Continue };
                    if len > opts.max_file_size {
                        // Big files are streamed in line-aligned chunks, never loaded whole.
                        match stream_hits(path, matcher, cancel) {
                            Some(h) => (h, false),
                            None => return WalkState::Continue,
                        }
                    } else {
                        match std::fs::read(path).ok().as_deref().and_then(decode_bytes) {
                            Some(d) => (hits_in(&d.text, matcher), false),
                            None => return WalkState::Continue,
                        }
                    }
                }
            };
            scanned.fetch_add(1, Ordering::Relaxed);
            if !hits.is_empty() {
                with_hits.fetch_add(1, Ordering::Relaxed);
                total.fetch_add(hits.len(), Ordering::Relaxed);
                on_file(FileResult { path: path.to_path_buf(), hits, from_buffer });
            }
            WalkState::Continue
        })
    });

    Ok(SearchStats {
        files_scanned: scanned.into_inner(),
        files_with_hits: with_hits.into_inner(),
        hits: total.into_inner(),
        cancelled: cancel.is_cancelled(),
    })
}

/// Convenience: collect all results, sorted by path.
pub fn search_files_collect(
    opts: &FindOptions,
    matcher: &dyn TextMatcher,
    cancel: &CancelToken,
) -> Result<(Vec<FileResult>, SearchStats), String> {
    let out = std::sync::Mutex::new(Vec::new());
    let stats = search_files(opts, matcher, cancel, &|r| out.lock().unwrap().push(r))?;
    let mut v = out.into_inner().unwrap();
    v.sort_by(|a, b| a.path.cmp(&b.path));
    Ok((v, stats))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn write(root: &Path, rel: &str, content: &[u8]) {
        let p = root.join(rel);
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(p, content).unwrap();
    }

    fn names(res: &[FileResult], root: &Path) -> Vec<String> {
        res.iter().map(|r| rel_path(&r.path, &[root.to_path_buf()])).collect()
    }

    fn fixture() -> tempfile::TempDir {
        let d = tempfile::tempdir().unwrap();
        let r = d.path();
        write(r, "a.rs", b"fn needle() {}\nlet x = needle;\n");
        write(r, "b.toml", b"needle = 1\n");
        write(r, "c.txt", b"needle\n");
        write(r, "target/out.rs", b"needle\n");
        write(r, "sub/d.rs", b"// NEEDLE\r\n");
        write(r, "ignored.log", b"needle\n");
        write(r, ".gitignore", b"*.log\n");
        write(r, ".hidden.rs", b"needle\n");
        write(r, "bin.dat", b"needle\0\0\0");
        d
    }

    fn run_search(opts: &FindOptions) -> Vec<String> {
        let m = RegexMatcher::new("needle", false, false, false).unwrap();
        let (res, _) = search_files_collect(opts, &m, &CancelToken::new()).unwrap();
        names(&res, &opts.roots[0])
    }

    #[test]
    fn include_exclude_gitignore_hidden_binary() {
        let d = fixture();
        let mut o = FindOptions::new(d.path());
        assert_eq!(run_search(&o), ["a.rs", "b.toml", "c.txt", "sub/d.rs", "target/out.rs"]);
        o.include = "*.rs;*.toml".into();
        o.exclude = "!target".into();
        assert_eq!(run_search(&o), ["a.rs", "b.toml", "sub/d.rs"]);
        o.include = "*.rs;!sub".into();
        o.exclude = String::new();
        assert_eq!(run_search(&o), ["a.rs", "target/out.rs"]);
        let mut o = FindOptions::new(d.path());
        o.respect_gitignore = false;
        assert!(run_search(&o).contains(&"ignored.log".to_string()));
        o.hidden = true;
        assert!(run_search(&o).contains(&".hidden.rs".to_string()));
        let mut o = FindOptions::new(d.path());
        o.recurse = false;
        assert_eq!(run_search(&o), ["a.rs", "b.toml", "c.txt"]);
    }

    #[test]
    fn hit_positions_case_and_crlf() {
        let d = fixture();
        let m = RegexMatcher::new("needle", false, false, false).unwrap();
        let (res, stats) = search_files_collect(&FindOptions::new(d.path()), &m, &CancelToken::new()).unwrap();
        let a = &res[0];
        assert_eq!((a.hits[0].line, a.hits[0].col), (0, 3));
        assert_eq!((a.hits[1].line, a.hits[1].col, a.hits[1].line_text.as_str()), (1, 8, "let x = needle;"));
        let sub = res.iter().find(|r| r.path.ends_with("d.rs") && r.path.parent().unwrap().ends_with("sub")).unwrap();
        assert_eq!(sub.hits[0].line_text, "// NEEDLE");
        assert_eq!(stats.hits, 6);
        let cs = RegexMatcher::new("needle", false, true, false).unwrap();
        let (res, _) = search_files_collect(&FindOptions::new(d.path()), &cs, &CancelToken::new()).unwrap();
        assert!(res.iter().all(|r| !r.path.ends_with("d.rs")));
    }

    #[test]
    fn unsaved_buffer_preferred_and_whole_word() {
        let d = fixture();
        let mut o = FindOptions::new(d.path());
        o.include = "c.txt".into();
        o.buffers.insert(d.path().join("c.txt"), "no hit here\nthe needle_x and needle\n".into());
        let m = RegexMatcher::new("needle", false, true, true).unwrap();
        let (res, _) = search_files_collect(&o, &m, &CancelToken::new()).unwrap();
        assert!(res[0].from_buffer);
        assert_eq!((res[0].hits.len(), res[0].hits[0].line, res[0].hits[0].col), (1, 1, 17));
    }

    #[test]
    fn encodings_utf16_and_cp1252() {
        let d = tempfile::tempdir().unwrap();
        let mut u16 = vec![0xFF, 0xFE];
        u16.extend("needle é\n".encode_utf16().flat_map(|u| u.to_le_bytes()));
        write(d.path(), "u16.txt", &u16);
        write(d.path(), "l1.txt", b"caf\xe9 needle\n");
        let r = run_search(&FindOptions::new(d.path()));
        assert_eq!(r, ["l1.txt", "u16.txt"]);
    }

    #[test]
    fn cancel_stops_and_invalid_pattern() {
        let d = fixture();
        let c = CancelToken::new();
        c.cancel();
        let m = RegexMatcher::new("needle", false, false, false).unwrap();
        let (res, stats) = search_files_collect(&FindOptions::new(d.path()), &m, &c).unwrap();
        assert!(res.is_empty() && stats.cancelled);
        assert!(RegexMatcher::new("(", true, true, false).is_err());
    }

    #[test]
    fn large_tree_streams_fast() {
        let d = tempfile::tempdir().unwrap();
        for i in 0..2000 {
            write(d.path(), &format!("d{}/f{i}.txt", i % 50), format!("line {i}\nneedle {i}\n").as_bytes());
        }
        let m = RegexMatcher::new("needle", false, true, false).unwrap();
        let t = std::time::Instant::now();
        let first = std::sync::Mutex::new(None);
        search_files(&FindOptions::new(d.path()), &m, &CancelToken::new(), &|_| {
            first.lock().unwrap().get_or_insert(t.elapsed());
        })
        .unwrap();
        assert!(first.into_inner().unwrap().unwrap().as_millis() < 300);
    }

    #[test]
    fn big_file_is_streamed_not_skipped() {
        let d = tempfile::tempdir().unwrap();
        let mut s = String::new();
        for i in 0..400_000 {
            s.push_str(&format!("line number {i} filler filler filler\n"));
        }
        s.push_str("the needle is here\n");
        write(d.path(), "big.txt", s.as_bytes());
        let mut o = FindOptions::new(d.path());
        o.max_file_size = 1024 * 1024; // force the streaming path (file is ~14 MB)
        assert!(s.len() > 8 * 1024 * 1024);
        let m = RegexMatcher::new("needle", false, true, false).unwrap();
        let (res, stats) = search_files_collect(&o, &m, &CancelToken::new()).unwrap();
        assert_eq!((stats.files_scanned, stats.hits), (1, 1));
        let h = &res[0].hits[0];
        assert_eq!((h.line, h.col, h.line_text.as_str()), (400_000, 4, "the needle is here"));
        assert_eq!(&s[h.start_byte..h.end_byte], "needle");
    }

    struct Proj;
    impl ProjectFolders for Proj {
        fn folders(&self) -> Vec<PathBuf> {
            vec![PathBuf::from("p1"), PathBuf::from("p2")]
        }
    }

    #[test]
    fn scope_presets_resolve() {
        let f = vec![PathBuf::from("a.rs")];
        assert_eq!(ScopePreset::OpenFiles(f.clone()).resolve(&Proj), (f, false));
        assert_eq!(ScopePreset::CurrentFileDir(PathBuf::from("x/y.rs")).resolve(&Proj), (vec![PathBuf::from("x")], false));
        assert_eq!(ScopePreset::ProjectFolders.resolve(&Proj).0.len(), 2);
        assert_eq!(ScopePreset::Folder("z".into()).resolve(&Proj), (vec![PathBuf::from("z")], true));
        let d = fixture();
        let (roots, recurse) = ScopePreset::CurrentFileDir(d.path().join("a.rs")).resolve(&Proj);
        let mut o = FindOptions::new(roots[0].clone());
        o.recurse = recurse;
        assert_eq!(run_search(&o), ["a.rs", "b.toml", "c.txt"]);
    }

    #[test]
    fn commands_run() {
        assert_eq!(commands().len(), 4);
        let out = run("find_in_files", &CmdCtx::default()).unwrap();
        assert_eq!(out.open_panel, Some(PanelId(PANEL_FIND_IN_FILES.into())));
        assert!(run("nope", &CmdCtx::default()).is_none());
    }
}
