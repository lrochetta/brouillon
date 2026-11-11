//! File-type registry (BK-057): file name / extension patterns (or a first-line
//! test) resolve to a profile of editing settings. Own TOML format, one profile
//! per `[[profile]]`; a single profile exports/imports on its own.

use serde::{Deserialize, Serialize};
use std::path::Path;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct FileProfile {
    pub name: String,
    /// File name globs (`*.html`, `Makefile`); each entry may hold several
    /// separated by `;` (semicolon-separated, as in classic editors). Case-insensitive.
    pub patterns: Vec<String>,
    /// First line must start with this (e.g. `#!`) ...
    pub first_line_starts: Option<String>,
    /// ... and contain this (e.g. `python`). Both, when set, must hold.
    pub first_line_contains: Option<String>,
    pub tab_size: usize,
    pub use_spaces: bool,
    pub auto_indent_chars: String,
    pub wrap: bool,
    pub function_regex: Option<String>,
    pub comment_line: Option<String>,
    pub comment_block_open: Option<String>,
    pub comment_block_close: Option<String>,
    pub encoding: Option<String>,
    pub eol: Option<String>,
    pub cliptext_library: Option<String>,
    pub trim_on_save: bool,
    pub tool_group: Option<String>,
}

impl Default for FileProfile {
    fn default() -> Self {
        Self {
            name: "Plain text".into(),
            patterns: Vec::new(),
            first_line_starts: None,
            first_line_contains: None,
            tab_size: 4,
            use_spaces: true,
            auto_indent_chars: String::new(),
            wrap: false,
            function_regex: None,
            comment_line: None,
            comment_block_open: None,
            comment_block_close: None,
            encoding: None,
            eol: None,
            cliptext_library: None,
            trim_on_save: false,
            tool_group: None,
        }
    }
}

impl FileProfile {
    fn matches_name(&self, file_name: &str) -> bool {
        self.patterns.iter().flat_map(|p| p.split(';')).map(str::trim).filter(|p| !p.is_empty()).any(|p| glob_match(p, file_name))
    }

    fn matches_first_line(&self, line: &str) -> bool {
        if self.first_line_starts.is_none() && self.first_line_contains.is_none() {
            return false;
        }
        self.first_line_starts.as_deref().map_or(true, |s| line.starts_with(s))
            && self.first_line_contains.as_deref().map_or(true, |s| line.contains(s))
    }

    pub fn to_toml(&self) -> String {
        toml::to_string(self).unwrap_or_default()
    }

    pub fn from_toml(text: &str) -> Result<Self, String> {
        toml::from_str(text).map_err(|e| e.to_string())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
struct RegistryFile {
    #[serde(default)]
    default: Option<FileProfile>,
    #[serde(default)]
    profile: Vec<FileProfile>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct FileTypeRegistry {
    profiles: Vec<FileProfile>,
    default: FileProfile,
}

impl FileTypeRegistry {
    pub fn new(profiles: Vec<FileProfile>) -> Self {
        Self { profiles, default: FileProfile::default() }
    }

    pub fn from_toml(text: &str) -> Result<Self, String> {
        let f: RegistryFile = toml::from_str(text).map_err(|e| e.to_string())?;
        Ok(Self { profiles: f.profile, default: f.default.unwrap_or_default() })
    }

    pub fn to_toml(&self) -> String {
        let f = RegistryFile { default: Some(self.default.clone()), profile: self.profiles.clone() };
        toml::to_string(&f).unwrap_or_default()
    }

    pub fn profiles(&self) -> &[FileProfile] {
        &self.profiles
    }

    /// Put `profile` first so it overrides an earlier one with overlapping patterns
    /// (a profile with the same name is replaced).
    pub fn add_override(&mut self, profile: FileProfile) {
        self.profiles.retain(|p| p.name != profile.name);
        self.profiles.insert(0, profile);
    }

    /// Resolve by file name first (registry order), then by first line, else the default.
    pub fn resolve(&self, path: &Path, first_line: Option<&str>) -> &FileProfile {
        let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
        self.profiles
            .iter()
            .find(|p| p.matches_name(name))
            .or_else(|| first_line.and_then(|l| self.profiles.iter().find(|p| p.matches_first_line(l))))
            .unwrap_or(&self.default)
    }

    /// Small built-in set; users extend it through TOML.
    pub fn builtin() -> Self {
        let p = |name: &str, pats: &[&str]| FileProfile {
            name: name.into(),
            patterns: pats.iter().map(|s| s.to_string()).collect(),
            ..Default::default()
        };
        let mut html = p("HTML", &["*.html;*.htm"]);
        html.tab_size = 2;
        html.comment_block_open = Some("<!--".into());
        html.comment_block_close = Some("-->".into());
        let mut make = p("Makefile", &["Makefile", "makefile", "*.mk"]);
        make.use_spaces = false;
        make.comment_line = Some("#".into());
        let mut rust = p("Rust", &["*.rs"]);
        rust.comment_line = Some("//".into());
        rust.function_regex = Some(r"^\s*(pub\s+)?fn\s+(\w+)".into());
        let mut sh = p("Shell", &["*.sh"]);
        sh.first_line_starts = Some("#!".into());
        sh.first_line_contains = Some("sh".into());
        sh.comment_line = Some("#".into());
        sh.eol = Some("lf".into());
        Self::new(vec![html, make, rust, sh])
    }
}

/// `*` (any run) and `?` (one char) glob, ASCII-case-insensitive, whole-string.
pub fn glob_match(pattern: &str, name: &str) -> bool {
    let p: Vec<char> = pattern.to_lowercase().chars().collect();
    let n: Vec<char> = name.to_lowercase().chars().collect();
    let (mut pi, mut ni) = (0, 0);
    let (mut star, mut mark) = (None, 0);
    while ni < n.len() {
        if pi < p.len() && (p[pi] == '?' || p[pi] == n[ni]) && p[pi] != '*' {
            pi += 1;
            ni += 1;
        } else if pi < p.len() && p[pi] == '*' {
            star = Some(pi);
            mark = ni;
            pi += 1;
        } else if let Some(s) = star {
            pi = s + 1;
            mark += 1;
            ni = mark;
        } else {
            return false;
        }
    }
    p[pi..].iter().all(|&c| c == '*')
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn reg() -> FileTypeRegistry {
        FileTypeRegistry::builtin()
    }

    #[test]
    fn extension_list_resolves() {
        let r = reg();
        assert_eq!(r.resolve(&PathBuf::from("site/index.html"), None).name, "HTML");
        assert_eq!(r.resolve(&PathBuf::from("INDEX.HTM"), None).name, "HTML");
        assert_eq!(r.resolve(&PathBuf::from("a.rs"), None).name, "Rust");
    }

    #[test]
    fn exact_name_makefile() {
        let r = reg();
        let p = r.resolve(&PathBuf::from("proj/Makefile"), None);
        assert_eq!((p.name.as_str(), p.use_spaces), ("Makefile", false));
    }

    #[test]
    fn shebang_first_line() {
        let r = reg();
        assert_eq!(r.resolve(&PathBuf::from("deploy"), Some("#!/bin/bash")).name, "Shell");
        // starts with #! but not a shell
        assert_eq!(r.resolve(&PathBuf::from("deploy"), Some("#!/usr/bin/perl")).name, "Plain text");
    }

    #[test]
    fn fallback_default_profile() {
        let r = reg();
        let p = r.resolve(&PathBuf::from("notes.zzz"), Some("hello"));
        assert_eq!((p.name.as_str(), p.tab_size, p.use_spaces), ("Plain text", 4, true));
    }

    #[test]
    fn toml_roundtrip_and_single_profile_export() {
        let r = reg();
        assert_eq!(FileTypeRegistry::from_toml(&r.to_toml()).unwrap(), r);
        let one = r.profiles()[0].clone();
        assert_eq!(FileProfile::from_toml(&one.to_toml()).unwrap(), one);
        // partial TOML keeps defaults
        let p = FileProfile::from_toml("name = \"X\"\npatterns = [\"*.x\"]\n").unwrap();
        assert_eq!(p.tab_size, 4);
    }

    #[test]
    fn override_wins_and_glob_edges() {
        let mut r = reg();
        let mut h = FileProfile { name: "My HTML".into(), patterns: vec!["*.html".into()], tab_size: 8, ..Default::default() };
        h.trim_on_save = true;
        r.add_override(h);
        assert_eq!(r.resolve(&PathBuf::from("a.html"), None).tab_size, 8);
        assert!(glob_match("*", "x") && glob_match("a?c", "ABC") && !glob_match("*.rs", "a.rs.bak") && glob_match("*.*", "a.b"));
    }
}
