//! `RemotePath`: a normalised, always-absolute, slash-separated path on a server.
//!
//! It is deliberately NOT a `std::path::Path`: the local side never gets to interpret a name that
//! came from a server (path traversal, drive letters, backslashes). `..` can never climb above `/`.

use std::path::PathBuf;

#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct RemotePath(String);

/// A listing entry name is safe to join: not empty, no separator, not `.` / `..`, no NUL.
pub fn valid_entry_name(name: &str) -> bool {
    !name.is_empty() && name != "." && name != ".." && !name.contains(['/', '\\', '\0'])
}

impl RemotePath {
    pub fn root() -> Self {
        Self("/".into())
    }

    /// Normalise `s`: backslashes become `/`, `//` and `.` vanish, `..` pops (never above `/`).
    pub fn new(s: &str) -> Self {
        let mut parts: Vec<&str> = Vec::new();
        for seg in s.split(['/', '\\']) {
            match seg {
                "" | "." => {}
                ".." => {
                    parts.pop();
                }
                other => parts.push(other),
            }
        }
        if parts.is_empty() {
            Self::root()
        } else {
            Self(format!("/{}", parts.join("/")))
        }
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn is_root(&self) -> bool {
        self.0 == "/"
    }

    /// Child `name`; `None` if `name` is not a plain file name.
    pub fn join(&self, name: &str) -> Option<Self> {
        if !valid_entry_name(name) {
            return None;
        }
        Some(if self.is_root() { Self(format!("/{name}")) } else { Self(format!("{}/{name}", self.0)) })
    }

    pub fn parent(&self) -> Option<Self> {
        if self.is_root() {
            return None;
        }
        let i = self.0.rfind('/')?;
        Some(if i == 0 { Self::root() } else { Self(self.0[..i].to_string()) })
    }

    pub fn file_name(&self) -> Option<&str> {
        if self.is_root() {
            None
        } else {
            self.0.rsplit('/').next()
        }
    }

    /// `self` is `other` or lives below it (component aware: `/ab` is not under `/a`).
    pub fn starts_with(&self, other: &RemotePath) -> bool {
        other.is_root() || self.0 == other.0 || self.0.starts_with(&format!("{}/", other.0))
    }

    /// Slash path handed to a blocking `FsProvider` (its `Path` is just a carrier here).
    pub fn to_path(&self) -> PathBuf {
        PathBuf::from(&self.0)
    }
}

impl std::fmt::Display for RemotePath {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalises_and_never_climbs_above_root() {
        assert_eq!(RemotePath::new("a//b/./c/").as_str(), "/a/b/c");
        assert_eq!(RemotePath::new("/../../etc").as_str(), "/etc");
        assert_eq!(RemotePath::new("C:\\x\\y").as_str(), "/C:/x/y");
        assert_eq!(RemotePath::new("").as_str(), "/");
        assert_eq!(RemotePath::new("/a/b/../..").as_str(), "/");
    }

    #[test]
    fn join_parent_and_name() {
        let r = RemotePath::root();
        let a = r.join("var").unwrap().join("www").unwrap();
        assert_eq!(a.as_str(), "/var/www");
        assert_eq!(a.parent().unwrap().as_str(), "/var");
        assert_eq!(a.parent().unwrap().parent().unwrap(), r);
        assert!(r.parent().is_none());
        assert_eq!(a.file_name(), Some("www"));
    }

    #[test]
    fn hostile_names_cannot_be_joined() {
        let a = RemotePath::new("/srv");
        for bad in ["", ".", "..", "a/b", "..\\x", "a\0b", "/etc/passwd"] {
            assert!(a.join(bad).is_none(), "{bad:?}");
        }
    }

    #[test]
    fn starts_with_is_component_aware() {
        assert!(RemotePath::new("/a/b").starts_with(&RemotePath::new("/a")));
        assert!(!RemotePath::new("/ab").starts_with(&RemotePath::new("/a")));
        assert!(RemotePath::new("/x").starts_with(&RemotePath::root()));
    }
}
