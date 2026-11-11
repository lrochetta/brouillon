//! Language registry: shipped definitions (`include_str!`) plus user
//! definitions from the config folder, with detection by file name,
//! extension and first line.

use super::def::*;
use brouillon_core::filetype::glob_match;
use std::collections::HashSet;
use std::path::Path;
use std::sync::{Arc, Mutex, OnceLock, RwLock};

macro_rules! builtin {
    ($($f:literal),* $(,)?) => {
        &[$(($f, include_str!(concat!("langs/", $f)))),*]
    };
}

/// Shipped definitions, written for Brouillon. Order is stable: it fixes the
/// `LanguageId::Defined` indices of the shipped languages.
const BUILTIN: &[(&str, &str)] = builtin![
    "batch.toml",
    "c.toml",
    "cpp.toml",
    "csharp.toml",
    "css.toml",
    "diff.toml",
    "dockerfile.toml",
    "go.toml",
    "html.toml",
    "ini.toml",
    "java.toml",
    "javascript.toml",
    "json.toml",
    "log.toml",
    "lua.toml",
    "makefile.toml",
    "markdown.toml",
    "perl.toml",
    "php.toml",
    "powershell.toml",
    "python.toml",
    "ruby.toml",
    "shell.toml",
    "sql.toml",
    "toml.toml",
    "typescript.toml",
    "vb.toml",
    "xml.toml",
    "yaml.toml",
];

fn intern(s: &str) -> &'static str {
    static POOL: OnceLock<Mutex<HashSet<&'static str>>> = OnceLock::new();
    let mut pool = POOL.get_or_init(Default::default).lock().unwrap_or_else(|e| e.into_inner());
    if let Some(x) = pool.get(s) {
        return x;
    }
    let leaked: &'static str = Box::leak(s.to_string().into_boxed_str());
    pool.insert(leaked);
    leaked
}

struct Entry {
    raw: RawDef,
    file: String,
    text: String,
    origin: Origin,
}

fn line_of(text: &str, byte: usize) -> usize {
    text[..byte.min(text.len())].bytes().filter(|b| *b == b'\n').count() + 1
}

fn parse_entry(file: &str, text: &str, origin: Origin, errors: &mut Vec<SyntaxError>) -> Option<Entry> {
    match toml::from_str::<RawDef>(text) {
        Ok(raw) => Some(Entry { raw, file: file.to_string(), text: text.to_string(), origin }),
        Err(e) => {
            errors.push(SyntaxError {
                source: file.to_string(),
                line: e.span().map(|s| line_of(text, s.start)),
                message: e.message().to_string(),
            });
            None
        }
    }
}

fn resolve(entries: &[Entry], i: usize, depth: u8, errors: &mut Vec<SyntaxError>) -> RawDef {
    let e = &entries[i];
    let Some(base_name) = e.raw.extends.clone() else { return e.raw.clone() };
    let fail = |msg: String, errors: &mut Vec<SyntaxError>| {
        errors.push(SyntaxError { source: e.file.clone(), line: e.text.lines().position(|l| l.contains("extends")).map(|n| n + 1), message: msg });
    };
    if depth > 8 {
        fail("`extends` chain too deep (cycle?)".into(), errors);
        return RawDef { extends: None, ..e.raw.clone() };
    }
    let same = e.raw.name.eq_ignore_ascii_case(&base_name);
    let find = |origin: Origin| {
        entries
            .iter()
            .enumerate()
            .position(|(j, x)| j != i && x.origin == origin && x.raw.name.eq_ignore_ascii_case(&base_name))
    };
    // A user definition extending its own name means "start from the shipped one".
    let order = if same || e.origin == Origin::Builtin { [Origin::Builtin, Origin::User] } else { [Origin::User, Origin::Builtin] };
    match find(order[0]).or_else(|| find(order[1])) {
        Some(j) => {
            let base = resolve(entries, j, depth + 1, errors);
            e.raw.clone().overlay_on(&base)
        }
        None => {
            fail(format!("`extends` refers to unknown language \"{base_name}\""), errors);
            RawDef { extends: None, ..e.raw.clone() }
        }
    }
}

/// All loaded languages.
#[derive(Debug)]
pub struct Registry {
    defs: Vec<Syntax>,
    errors: Vec<SyntaxError>,
}

impl Registry {
    /// Build from the shipped definitions plus `(file name, text)` user files.
    pub fn build(user: &[(String, String)]) -> Registry {
        let mut errors = Vec::new();
        let mut entries: Vec<Entry> = Vec::new();
        for (f, t) in BUILTIN {
            if let Some(e) = parse_entry(f, t, Origin::Builtin, &mut errors) {
                entries.push(e);
            }
        }
        for (f, t) in user {
            if let Some(e) = parse_entry(f, t, Origin::User, &mut errors) {
                entries.push(e);
            }
        }
        let mut defs: Vec<Syntax> = Vec::new();
        for i in 0..entries.len() {
            let raw = resolve(&entries, i, 0, &mut errors);
            let e = &entries[i];
            let name = intern(raw.name.trim());
            match Syntax::compile(raw, e.origin, &e.file, &e.text, name) {
                Ok(s) => match defs.iter().position(|d| d.name.eq_ignore_ascii_case(s.name)) {
                    // A user definition replaces the shipped one of the same name in place.
                    Some(k) if s.origin == Origin::User => defs[k] = s,
                    Some(_) => errors.push(SyntaxError {
                        source: e.file.clone(),
                        line: None,
                        message: format!("duplicate language name \"{}\" ignored", s.name),
                    }),
                    None => defs.push(s),
                },
                Err(err) => errors.push(err),
            }
        }
        // Resolve host / embedded language names to indices.
        let names: Vec<&'static str> = defs.iter().map(|d| d.name).collect();
        let lookup = |n: &str| names.iter().position(|x| x.eq_ignore_ascii_case(n)).map(|i| i as u16);
        for d in defs.iter_mut() {
            if let Some(h) = d.host_name.clone() {
                d.host = lookup(&h);
                if d.host.is_none() {
                    errors.push(SyntaxError { source: d.source.clone(), line: None, message: format!("unknown host language \"{h}\"") });
                }
            }
            let src = d.source.clone();
            if let Some(m) = d.markup.as_mut() {
                for r in m.raw_text.iter_mut() {
                    r.def = lookup(&r.lang);
                    if r.def.is_none() {
                        errors.push(SyntaxError { source: src.clone(), line: None, message: format!("unknown embedded language \"{}\"", r.lang) });
                    }
                }
            }
        }
        Registry { defs, errors }
    }

    /// Shipped definitions plus every `*.toml` in `dir` (missing dir is fine).
    pub fn build_with_dir(dir: Option<&Path>) -> Registry {
        let mut user = Vec::new();
        let mut io_errors = Vec::new();
        if let Some(dir) = dir {
            if let Ok(rd) = std::fs::read_dir(dir) {
                let mut files: Vec<_> = rd.flatten().map(|e| e.path()).filter(|p| p.extension().map_or(false, |x| x == "toml")).collect();
                files.sort();
                for p in files {
                    let name = p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                    match std::fs::read_to_string(&p) {
                        Ok(t) => user.push((name, t)),
                        Err(e) => io_errors.push(SyntaxError { source: name, line: None, message: e.to_string() }),
                    }
                }
            }
        }
        let mut r = Registry::build(&user);
        r.errors.extend(io_errors);
        r
    }

    pub fn def(&self, i: u16) -> &Syntax {
        &self.defs[i as usize]
    }

    pub fn len(&self) -> usize {
        self.defs.len()
    }

    pub fn is_empty(&self) -> bool {
        self.defs.is_empty()
    }

    pub fn names(&self) -> Vec<&'static str> {
        self.defs.iter().map(|d| d.name).collect()
    }

    /// Non-fatal load problems (bad TOML, unknown kinds, unknown embeds...).
    pub fn errors(&self) -> &[SyntaxError] {
        &self.errors
    }

    pub fn by_name(&self, name: &str) -> Option<u16> {
        self.defs.iter().position(|d| d.name.eq_ignore_ascii_case(name.trim())).map(|i| i as u16)
    }

    /// Pick a definition: file name, then extension, then first line.
    /// User definitions win over shipped ones at each step.
    pub fn detect(&self, path: &Path, first_line: Option<&str>) -> Option<u16> {
        let origins = [Origin::User, Origin::Builtin];
        let fname = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
        if !fname.is_empty() {
            for o in origins {
                for (i, d) in self.defs.iter().enumerate().filter(|(_, d)| d.origin == o) {
                    if d.filenames.iter().any(|p| glob_match(p, fname)) {
                        return Some(i as u16);
                    }
                }
            }
        }
        if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
            let ext = ext.to_ascii_lowercase();
            for o in origins {
                for (i, d) in self.defs.iter().enumerate().filter(|(_, d)| d.origin == o) {
                    if d.extensions.iter().any(|e| *e == ext) {
                        return Some(i as u16);
                    }
                }
            }
        }
        let line = first_line?;
        let lower = line.trim_start().to_ascii_lowercase();
        let interp = shebang_interpreter(&lower);
        for o in origins {
            for (i, d) in self.defs.iter().enumerate().filter(|(_, d)| d.origin == o) {
                if let Some(it) = &interp {
                    if d.shebang.iter().any(|s| {
                        let s = s.to_ascii_lowercase();
                        it == &s || (it.starts_with(&s) && it[s.len()..].bytes().all(|c| c.is_ascii_digit() || c == b'.'))
                    }) {
                        return Some(i as u16);
                    }
                }
                if d.first_line.iter().any(|p| lower.starts_with(p.as_str())) {
                    return Some(i as u16);
                }
            }
        }
        None
    }
}

/// `#!/usr/bin/env python3 -u` -> `python3`; `#!/bin/bash` -> `bash`.
fn shebang_interpreter(lower_line: &str) -> Option<String> {
    let rest = lower_line.strip_prefix("#!")?;
    let mut toks = rest.split_whitespace();
    let first = toks.next()?;
    let base = |t: &str| t.rsplit(['/', '\\']).next().unwrap_or(t).to_string();
    let b = base(first);
    if b == "env" {
        let t = toks.find(|t| !t.starts_with('-') && !t.contains('='))?;
        return Some(base(t));
    }
    Some(b)
}

static GLOBAL: OnceLock<RwLock<Arc<Registry>>> = OnceLock::new();

fn global() -> &'static RwLock<Arc<Registry>> {
    GLOBAL.get_or_init(|| {
        #[cfg(not(test))]
        let dir = brouillon_core::paths::Paths::system(None).map(|p| p.syntax_dir());
        #[cfg(test)]
        let dir: Option<std::path::PathBuf> = None; // tests stay hermetic
        let reg = Registry::build_with_dir(dir.as_deref());
        #[cfg(not(test))]
        for e in reg.errors() {
            eprintln!("[syntax] {e}");
        }
        RwLock::new(Arc::new(reg))
    })
}

/// The process-wide registry (shipped + user definitions).
pub fn registry() -> Arc<Registry> {
    global().read().unwrap_or_else(|e| e.into_inner()).clone()
}

/// (Re)load user definitions from `dir` (e.g. `Paths::syntax_dir()`, or after
/// the user edits a file). Returns the non-fatal load errors.
pub fn reload_user_dir(dir: &Path) -> Vec<SyntaxError> {
    let reg = Registry::build_with_dir(Some(dir));
    let errs = reg.errors().to_vec();
    *global().write().unwrap_or_else(|e| e.into_inner()) = Arc::new(reg);
    errs
}
