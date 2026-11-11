//! Quick open overlay model (BK-071): fuzzy file open over MRU + folder files,
//! with `:line[:col]`, `@symbol` and `#text` prefixes. Pure data; the view and
//! the shell turn a `Hit` into an action. Available in Moldu and Geek (D11).
//! The `quick_open` command itself is registered by `core::command_registry`.

use std::path::PathBuf;

use brouillon_core::contract::{CmdCtx, CmdOutcome, CommandSpec};

use crate::palette::fuzzy_score;

/// What the user typed, classified by prefix.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Query {
    File(String),
    /// 1-based line and optional column.
    Line(usize, Option<usize>),
    Symbol(String),
    Text(String),
}

pub fn parse_query(input: &str) -> Query {
    let t = input.trim_start();
    if let Some(rest) = t.strip_prefix(':') {
        let mut it = rest.trim().splitn(2, [':', ',']);
        if let Some(line) = it.next().and_then(|l| l.trim().parse::<usize>().ok()) {
            let col = it.next().and_then(|c| c.trim().parse::<usize>().ok());
            return Query::Line(line, col);
        }
    }
    if let Some(rest) = t.strip_prefix('@') {
        return Query::Symbol(rest.trim().to_string());
    }
    if let Some(rest) = t.strip_prefix('#') {
        return Query::Text(rest.to_string());
    }
    Query::File(t.trim_end().to_string())
}

/// A symbol from the outline (empty list when no outline is available).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Symbol {
    pub name: String,
    pub line: usize,
}

/// A selectable result row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Hit {
    File(PathBuf),
    Goto { line: usize, col: Option<usize> },
    Symbol(Symbol),
    /// Search the current document for this text.
    Text(String),
}

pub struct QuickOpen {
    /// Most recently used first.
    pub mru: Vec<PathBuf>,
    /// Files of the open folder.
    pub files: Vec<PathBuf>,
    pub symbols: Vec<Symbol>,
}

fn file_score(query: &str, path: &PathBuf) -> Option<i32> {
    let full = path.to_string_lossy().replace('\\', "/");
    let name = full.rsplit('/').next().unwrap_or(&full);
    let by_name = fuzzy_score(query, name).map(|s| s * 2 + 10);
    let by_path = fuzzy_score(query, &full);
    // shorter paths win ties
    by_name.max(by_path).map(|s| s * 4 - (full.len() as i32).min(3))
}

impl QuickOpen {
    pub fn new(mru: Vec<PathBuf>, files: Vec<PathBuf>, symbols: Vec<Symbol>) -> Self {
        Self { mru, files, symbols }
    }

    /// Ranked results for the raw input, at most `limit`.
    pub fn search(&self, input: &str, limit: usize) -> Vec<Hit> {
        match parse_query(input) {
            Query::Line(line, col) => vec![Hit::Goto { line, col }],
            Query::Text(t) if !t.is_empty() => vec![Hit::Text(t)],
            Query::Text(_) => Vec::new(),
            Query::Symbol(q) => {
                let mut v: Vec<_> = self
                    .symbols
                    .iter()
                    .filter_map(|s| fuzzy_score(&q, &s.name).map(|sc| (sc, s)))
                    .collect();
                v.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.line.cmp(&b.1.line)));
                v.into_iter().take(limit).map(|(_, s)| Hit::Symbol(s.clone())).collect()
            }
            Query::File(q) => {
                if q.is_empty() {
                    return self.mru.iter().take(limit).cloned().map(Hit::File).collect();
                }
                let mut seen: Vec<&PathBuf> = Vec::new();
                let mut v: Vec<(i32, &PathBuf)> = Vec::new();
                for (rank, p) in self.mru.iter().chain(self.files.iter()).enumerate() {
                    if seen.contains(&p) {
                        continue;
                    }
                    seen.push(p);
                    if let Some(s) = file_score(&q, p) {
                        // MRU entries (first in the chain) get a small bonus
                        let bonus = if rank < self.mru.len() { 6 } else { 0 };
                        v.push((s + bonus, p));
                    }
                }
                v.sort_by(|a, b| b.0.cmp(&a.0));
                v.into_iter().take(limit).map(|(_, p)| Hit::File(p.clone())).collect()
            }
        }
    }
}

/// UI lot entry points kept for the wiring contract; commands live in core.
pub fn commands() -> Vec<CommandSpec> {
    Vec::new()
}

pub fn run(id: &str, ctx: &CmdCtx) -> Option<CmdOutcome> {
    let _ = (id, ctx);
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn qo() -> QuickOpen {
        let p = |s: &str| PathBuf::from(s);
        QuickOpen::new(
            vec![p("src/recent.rs")],
            vec![p("src/main.rs"), p("src/editor/mod.rs"), p("README.md"), p("src/recent.rs"), p("src/commands/registry.rs")],
            vec![Symbol { name: "parse_query".into(), line: 10 }, Symbol { name: "run".into(), line: 3 }],
        )
    }

    #[test]
    fn prefixes_are_classified() {
        assert_eq!(parse_query(":12"), Query::Line(12, None));
        assert_eq!(parse_query(":12:5"), Query::Line(12, Some(5)));
        assert_eq!(parse_query("@run"), Query::Symbol("run".into()));
        assert_eq!(parse_query("#todo"), Query::Text("todo".into()));
        assert_eq!(parse_query(":x"), Query::File(":x".into()));
    }

    #[test]
    fn colon_line_returns_goto_action() {
        assert_eq!(qo().search(":12", 5), vec![Hit::Goto { line: 12, col: None }]);
    }

    #[test]
    fn fuzzy_ranking_prefers_filename_and_mru() {
        let hits = qo().search("main", 5);
        assert_eq!(hits[0], Hit::File("src/main.rs".into()));
        let hits = qo().search("rrs", 5);
        assert_eq!(hits[0], Hit::File("src/recent.rs".into()), "mru entry first");
        assert!(hits.iter().filter(|h| **h == Hit::File("src/recent.rs".into())).count() == 1, "deduplicated");
        assert!(qo().search("zzz", 5).is_empty());
    }

    #[test]
    fn empty_query_lists_mru_and_symbols_filter() {
        assert_eq!(qo().search("", 5), vec![Hit::File("src/recent.rs".into())]);
        assert_eq!(qo().search("@pq", 5), vec![Hit::Symbol(Symbol { name: "parse_query".into(), line: 10 })]);
        assert_eq!(qo().search("#hello", 5), vec![Hit::Text("hello".into())]);
    }
}
