//! Persisted find / replace history (BK-020). TOML, most recent first,
//! deduplicated, capped.

use std::io;
use std::path::Path;

use serde::{Deserialize, Serialize};

pub const DEFAULT_CAP: usize = 32;

fn default_cap() -> usize {
    DEFAULT_CAP
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SearchHistory {
    #[serde(default)]
    pub find: Vec<String>,
    #[serde(default)]
    pub replace: Vec<String>,
    #[serde(skip, default = "default_cap")]
    cap: usize,
}

impl Default for SearchHistory {
    fn default() -> Self {
        Self { find: Vec::new(), replace: Vec::new(), cap: DEFAULT_CAP }
    }
}

fn push(list: &mut Vec<String>, entry: &str, cap: usize) {
    if entry.is_empty() {
        return;
    }
    list.retain(|e| e != entry);
    list.insert(0, entry.to_owned());
    list.truncate(cap);
}

impl SearchHistory {
    /// History keeping at most `cap` entries per list (minimum 1).
    pub fn with_cap(cap: usize) -> Self {
        Self { cap: cap.max(1), ..Self::default() }
    }

    pub fn push_find(&mut self, entry: &str) {
        push(&mut self.find, entry, self.cap);
    }

    pub fn push_replace(&mut self, entry: &str) {
        push(&mut self.replace, entry, self.cap);
    }

    pub fn to_toml(&self) -> String {
        toml::to_string(self).unwrap_or_default()
    }

    /// Parse; lists longer than `cap` are truncated.
    pub fn from_toml(src: &str, cap: usize) -> Result<Self, toml::de::Error> {
        let mut h: Self = toml::from_str(src)?;
        h.cap = cap.max(1);
        h.find.truncate(h.cap);
        h.replace.truncate(h.cap);
        Ok(h)
    }

    /// Load from `path`; a missing or unreadable file yields an empty history.
    pub fn load(path: &Path, cap: usize) -> Self {
        std::fs::read_to_string(path)
            .ok()
            .and_then(|s| Self::from_toml(&s, cap).ok())
            .unwrap_or_else(|| Self::with_cap(cap))
    }

    pub fn save(&self, path: &Path) -> io::Result<()> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        std::fs::write(path, self.to_toml())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dedupes_moves_to_front_and_caps() {
        let mut h = SearchHistory::with_cap(3);
        for s in ["a", "b", "c", "a", "d", ""] {
            h.push_find(s);
        }
        assert_eq!(h.find, vec!["d", "a", "c"]);
        h.push_replace("x");
        h.push_replace("x");
        assert_eq!(h.replace, vec!["x"]);
    }

    #[test]
    fn survives_reload() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nested").join("search_history.toml");
        let mut h = SearchHistory::with_cap(16);
        h.push_find("foo\\d+");
        h.push_find("line\nbreak \"quoted\"");
        h.push_replace("\\U$1");
        h.save(&path).unwrap();
        let back = SearchHistory::load(&path, 16);
        assert_eq!(back, h);
        // smaller cap on reload truncates; missing file is empty
        assert_eq!(SearchHistory::load(&path, 1).find.len(), 1);
        assert!(SearchHistory::load(&dir.path().join("none.toml"), 16).find.is_empty());
    }
}
