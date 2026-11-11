//! Error patterns and jump-to-error (BK-065).
//!
//! A pattern is a regex with named groups `file`, `line` and optional `col`.
//! Built-ins cover rustc, gcc/clang, MSVC, python tracebacks, node; users add
//! their own. `OutputBuffer` is the pure model behind the output panel.

use regex::Regex;
use std::path::{Path, PathBuf};

/// One compiled pattern.
#[derive(Debug, Clone)]
pub struct ErrorPattern {
    pub name: String,
    re: Regex,
}

impl ErrorPattern {
    /// Compile `regex`; it must define named groups `file` and `line`.
    pub fn new(name: &str, regex: &str) -> Result<Self, String> {
        let re = Regex::new(regex).map_err(|e| format!("pattern '{name}': {e}"))?;
        let names: Vec<&str> = re.capture_names().flatten().collect();
        for need in ["file", "line"] {
            if !names.contains(&need) {
                return Err(format!("pattern '{name}': missing named group '{need}'"));
            }
        }
        Ok(Self { name: name.to_string(), re })
    }
}

/// A located error: `path` as written in the output (not yet resolved).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ErrorHit {
    pub path: PathBuf,
    /// 1-based.
    pub line: usize,
    /// 1-based, 0 when the pattern has no column.
    pub col: usize,
}

impl ErrorHit {
    /// Relative paths resolve against the tool's working directory.
    pub fn resolved(&self, cwd: &Path) -> PathBuf {
        if self.path.is_absolute() { self.path.clone() } else { cwd.join(&self.path) }
    }
}

/// Built-in patterns, most specific first.
pub fn builtin_patterns() -> Vec<ErrorPattern> {
    let defs: [(&str, &str); 6] = [
        ("quoted", r#"^"(?P<file>[^"]+)"\((?P<line>[0-9]+)(?:,(?P<col>[0-9]+))?\)"#),
        ("rustc", r"^\s*--> (?P<file>.+?):(?P<line>[0-9]+):(?P<col>[0-9]+)"),
        ("python", r#"File "(?P<file>[^"]+)", line (?P<line>[0-9]+)"#),
        ("msvc", r"^\s*(?P<file>[^(]+?)\((?P<line>[0-9]+)(?:,(?P<col>[0-9]+))?\)\s*:"),
        ("node", r"^\s*at .*?\(?(?P<file>(?:[A-Za-z]:)?[^():\s]+):(?P<line>[0-9]+):(?P<col>[0-9]+)\)?\s*$"),
        // gcc / clang / generic `file:line[:col]`
        ("generic", r"^\s*(?P<file>(?:[A-Za-z]:)?[^:\s][^:]*?):(?P<line>[0-9]+)(?::(?P<col>[0-9]+))?(?:[: ]|$)"),
    ];
    defs.iter().map(|(n, r)| ErrorPattern::new(n, r).expect("builtin pattern")).collect()
}

/// Built-ins plus user regexes; user patterns win (checked first). Invalid
/// user patterns are returned as messages instead of being dropped silently.
pub fn with_user_patterns(user: &[(String, String)]) -> (Vec<ErrorPattern>, Vec<String>) {
    let mut pats = Vec::new();
    let mut errs = Vec::new();
    for (name, re) in user {
        match ErrorPattern::new(name, re) {
            Ok(p) => pats.push(p),
            Err(e) => errs.push(e),
        }
    }
    pats.extend(builtin_patterns());
    (pats, errs)
}

/// First pattern matching `text` wins. A bare `file:line` needs a path-like
/// file part (contains '.', '/' or '\') to avoid matching prose.
pub fn locate(text: &str, patterns: &[ErrorPattern]) -> Option<ErrorHit> {
    for p in patterns {
        let Some(c) = p.re.captures(text) else { continue };
        let file = c.name("file")?.as_str().trim();
        let Ok(line) = c["line"].parse::<usize>() else { continue };
        if !(file.contains('.') || file.contains('/') || file.contains('\\')) {
            continue;
        }
        let col = c.name("col").and_then(|m| m.as_str().parse().ok()).unwrap_or(0);
        return Some(ErrorHit { path: PathBuf::from(file), line, col });
    }
    None
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stream {
    Stdout,
    Stderr,
    /// Headers and footers written by the runner.
    Info,
}

#[derive(Debug, Clone)]
pub struct OutputLine {
    pub text: String,
    pub stream: Stream,
    pub hit: Option<ErrorHit>,
}

/// Lines shown in the output panel, with their jump targets.
#[derive(Debug, Clone)]
pub struct OutputBuffer {
    pub lines: Vec<OutputLine>,
    /// Working directory relative error paths resolve against.
    pub cwd: PathBuf,
    patterns: Vec<ErrorPattern>,
    /// Line last jumped to (F4 / Shift+F4 start from here).
    pub current: Option<usize>,
}

impl OutputBuffer {
    pub fn new(cwd: PathBuf, patterns: Vec<ErrorPattern>) -> Self {
        Self { lines: Vec::new(), cwd, patterns, current: None }
    }

    pub fn push(&mut self, text: String, stream: Stream) {
        let hit = if stream == Stream::Info { None } else { locate(&text, &self.patterns) };
        self.lines.push(OutputLine { text, stream, hit });
    }

    /// Click / Enter on line `idx`: the resolved target, if the line is an error.
    pub fn activate(&mut self, idx: usize) -> Option<(PathBuf, usize, usize)> {
        let hit = self.lines.get(idx)?.hit.as_ref()?;
        let target = (hit.resolved(&self.cwd), hit.line, hit.col);
        self.current = Some(idx);
        Some(target)
    }

    /// F4 (`forward`) / Shift+F4: jump to the next/previous error line,
    /// wrapping around.
    pub fn step(&mut self, forward: bool) -> Option<(PathBuf, usize, usize)> {
        let n = self.lines.len();
        if n == 0 {
            return None;
        }
        let start = self.current.unwrap_or(if forward { n - 1 } else { 0 });
        for k in 1..=n {
            let idx = if forward { (start + k) % n } else { (start + n - k % n) % n };
            if self.lines[idx].hit.is_some() {
                return self.activate(idx);
            }
        }
        None
    }
}

/// "Go to file at selection": interpret selected text (`path`, `path:12`,
/// `path:12:3`) as a location, resolving relative paths against `base`.
pub fn goto_file_at(selection: &str, base: &Path, patterns: &[ErrorPattern]) -> Option<(PathBuf, usize, usize)> {
    let s = selection.trim();
    if let Some(h) = locate(s, patterns) {
        return Some((h.resolved(base), h.line, h.col));
    }
    if s.is_empty() || s.contains('\n') {
        return None;
    }
    let p = PathBuf::from(s);
    let full = if p.is_absolute() { p } else { base.join(p) };
    full.is_file().then_some((full, 1, 0))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hit(text: &str) -> Option<(String, usize, usize)> {
        locate(text, &builtin_patterns()).map(|h| (h.path.to_string_lossy().into_owned(), h.line, h.col))
    }

    #[test]
    fn builtin_fixtures() {
        assert_eq!(hit("  --> src/main.rs:42:5"), Some(("src/main.rs".into(), 42, 5)));
        assert_eq!(hit("src/a.c:10:3: error: x undeclared"), Some(("src/a.c".into(), 10, 3)));
        assert_eq!(hit("src/a.c:10: warning: unused"), Some(("src/a.c".into(), 10, 0)));
        assert_eq!(hit("C:\\w\\a.c:7:1: error: y"), Some(("C:\\w\\a.c".into(), 7, 1)));
        assert_eq!(hit("foo\\bar.cpp(15,9): error C2065: 'x'"), Some(("foo\\bar.cpp".into(), 15, 9)));
        assert_eq!(hit("bar.cpp(15): error C2065"), Some(("bar.cpp".into(), 15, 0)));
        assert_eq!(hit("  File \"script.py\", line 25, in main"), Some(("script.py".into(), 25, 0)));
        assert_eq!(hit("    at Object.<anonymous> (/app/x.js:3:9)"), Some(("/app/x.js".into(), 3, 9)));
        assert_eq!(hit("\"/p/f.txt\"(8,2): text"), Some(("/p/f.txt".into(), 8, 2)));
    }

    #[test]
    fn prose_is_not_an_error() {
        assert_eq!(hit("Build finished: 3 warnings"), None);
        assert_eq!(hit("note: see item 5: here"), None);
        assert_eq!(hit("Compiling foo v0.1.0"), None);
    }

    #[test]
    fn user_regex_with_named_groups_wins() {
        let user = vec![("mine".to_string(), r"^ERR@(?P<file>\S+)#(?P<line>[0-9]+)".to_string())];
        let (pats, errs) = with_user_patterns(&user);
        assert!(errs.is_empty());
        let h = locate("ERR@x.txt#9 boom", &pats).unwrap();
        assert_eq!((h.path, h.line, h.col), (PathBuf::from("x.txt"), 9, 0));
    }

    #[test]
    fn invalid_user_patterns_reported() {
        let user = vec![
            ("bad".to_string(), "(unclosed".to_string()),
            ("nogroups".to_string(), r"[0-9]+".to_string()),
        ];
        let (pats, errs) = with_user_patterns(&user);
        assert_eq!(errs.len(), 2);
        assert_eq!(pats.len(), builtin_patterns().len());
    }

    #[test]
    fn relative_paths_resolve_against_cwd() {
        let mut b = OutputBuffer::new(PathBuf::from("/work/proj"), builtin_patterns());
        b.push("src/x.rs:3:1: error: z".into(), Stream::Stderr);
        let (p, l, c) = b.activate(0).unwrap();
        assert_eq!((p, l, c), (PathBuf::from("/work/proj").join("src/x.rs"), 3, 1));
        b.push("/abs/y.rs:4:2: error".into(), Stream::Stderr);
        assert_eq!(b.activate(1).unwrap().0, PathBuf::from("/abs/y.rs"));
    }

    #[test]
    fn f4_steps_forward_backward_and_wraps() {
        let mut b = OutputBuffer::new(PathBuf::from("/w"), builtin_patterns());
        for (t, s) in [("start", Stream::Info), ("a.rs:1:1: e", Stream::Stderr), ("noise", Stream::Stdout), ("b.rs:2:2: e", Stream::Stderr)] {
            b.push(t.into(), s);
        }
        let line = |b: &mut OutputBuffer, fwd| b.step(fwd).unwrap().1;
        assert_eq!(line(&mut b, true), 1); // a.rs
        assert_eq!(line(&mut b, true), 2); // b.rs
        assert_eq!(line(&mut b, true), 1); // wraps to a.rs
        assert_eq!(line(&mut b, false), 2); // wraps back to b.rs
        assert_eq!(line(&mut b, false), 1);
        assert_eq!(OutputBuffer::new(PathBuf::new(), vec![]).step(true), None);
    }

    #[test]
    fn go_to_file_at_selection() {
        let pats = builtin_patterns();
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("t.txt"), "x").unwrap();
        assert_eq!(goto_file_at("t.txt", dir.path(), &pats), Some((dir.path().join("t.txt"), 1, 0)));
        assert_eq!(goto_file_at("t.txt:7:2", dir.path(), &pats), Some((dir.path().join("t.txt"), 7, 2)));
        assert_eq!(goto_file_at("nope.txt", dir.path(), &pats), None);
    }
}
