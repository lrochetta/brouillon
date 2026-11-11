//! `RemotePath` and `RemoteName`: strings that came from (or go to) a remote server.
//!
//! They deliberately implement none of `AsRef<Path>`, `AsRef<OsStr>`, `Deref<Target = str>` or
//! `Into<PathBuf>`, so a server-supplied string cannot reach `std::fs` by accident:
//!
//! ```compile_fail
//! use brouillon_remote_proto::RemotePath;
//! let p = RemotePath::parse("/etc/passwd").unwrap();
//! let _local: &std::path::Path = p.as_ref();
//! ```
//!
//! ```compile_fail
//! use brouillon_remote_proto::RemotePath;
//! let p = RemotePath::parse("/etc/passwd").unwrap();
//! let _ = std::fs::read(&p);
//! ```
//!
//! The local cache key is derived with [`RemotePath::cache_key`] (a hash), never from the text.

use crate::limits::{MAX_NAME_BYTES, MAX_PATH_BYTES};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PathError {
    #[error("path is not absolute")]
    NotAbsolute,
    #[error("path is longer than {MAX_PATH_BYTES} bytes")]
    PathTooLong,
    #[error("name is empty")]
    EmptyName,
    #[error("name is '.' or '..'")]
    DotName,
    #[error("name is longer than {MAX_NAME_BYTES} bytes")]
    NameTooLong,
    #[error("name contains '/'")]
    Slash,
    #[error("name or path contains NUL or a control character")]
    Control,
    #[error("name contains an invisible or bidirectional-override character")]
    Invisible,
    #[error("name is not valid UTF-8")]
    NotUtf8,
}

fn is_invisible(c: char) -> bool {
    matches!(c as u32,
        0x200B..=0x200F | 0x202A..=0x202E | 0x2060..=0x2064 | 0x2066..=0x2069 | 0xFEFF | 0xE0000..=0xE007F)
}

/// One path component received from a server (a directory entry name).
/// Hard rules (the entry must be hidden): empty, `.`/`..`, `/`, NUL, C0/DEL/C1 control characters,
/// bidi/zero-width/tag characters, more than 255 bytes.
/// Soft rule: [`RemoteName::is_local_unsafe`] flags names that are legal remotely but would be
/// dangerous as a Windows file name (reserved device names, `:`, `\`, trailing dot/space). The editor
/// never uses a remote name as a local file name, so this is a display hint only.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize)]
pub struct RemoteName(String);

impl RemoteName {
    pub fn new(s: &str) -> Result<Self, PathError> {
        if s.is_empty() {
            return Err(PathError::EmptyName);
        }
        if s == "." || s == ".." {
            return Err(PathError::DotName);
        }
        if s.len() > MAX_NAME_BYTES {
            return Err(PathError::NameTooLong);
        }
        for c in s.chars() {
            if c == '/' {
                return Err(PathError::Slash);
            }
            if c.is_control() {
                return Err(PathError::Control);
            }
            if is_invisible(c) {
                return Err(PathError::Invisible);
            }
        }
        Ok(RemoteName(s.to_string()))
    }

    pub fn from_bytes(b: &[u8]) -> Result<Self, PathError> {
        Self::new(std::str::from_utf8(b).map_err(|_| PathError::NotUtf8)?)
    }

    /// The name as sent on the wire / shown in the UI. Not a local file name.
    pub fn as_wire_str(&self) -> &str {
        &self.0
    }

    pub fn is_local_unsafe(&self) -> bool {
        let s = &self.0;
        if s.contains(':') || s.contains('\\') || s.ends_with('.') || s.ends_with(' ') {
            return true;
        }
        let stem = s.split('.').next().unwrap_or("").to_ascii_uppercase();
        matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL")
            || ((stem.starts_with("COM") || stem.starts_with("LPT"))
                && stem.len() == 4
                && stem.as_bytes()[3].is_ascii_digit()
                && stem.as_bytes()[3] != b'0')
    }
}

impl<'de> Deserialize<'de> for RemoteName {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let s = String::deserialize(d)?;
        RemoteName::new(&s).map_err(serde::de::Error::custom)
    }
}

impl std::fmt::Display for RemoteName {
    /// Escaped for display: never raw control characters.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// An absolute, lexically normalised path on the remote server (always `/`-separated).
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize)]
pub struct RemotePath(String);

impl RemotePath {
    pub fn root() -> Self {
        RemotePath("/".to_string())
    }

    /// Parse and normalise: `//` and `.` collapse, `..` is resolved lexically (and never climbs above `/`),
    /// every component must be a valid [`RemoteName`], total length <= 4096 bytes.
    pub fn parse(s: &str) -> Result<Self, PathError> {
        if !s.starts_with('/') {
            return Err(PathError::NotAbsolute);
        }
        if s.len() > MAX_PATH_BYTES {
            return Err(PathError::PathTooLong);
        }
        let mut parts: Vec<&str> = Vec::new();
        for comp in s.split('/') {
            match comp {
                "" | "." => {}
                ".." => {
                    parts.pop();
                }
                c => {
                    RemoteName::new(c)?;
                    parts.push(c);
                }
            }
        }
        let out = if parts.is_empty() { "/".to_string() } else { format!("/{}", parts.join("/")) };
        if out.len() > MAX_PATH_BYTES {
            return Err(PathError::PathTooLong);
        }
        Ok(RemotePath(out))
    }

    pub fn join(&self, name: &RemoteName) -> Result<RemotePath, PathError> {
        let s = if self.0 == "/" { format!("/{}", name.0) } else { format!("{}/{}", self.0, name.0) };
        if s.len() > MAX_PATH_BYTES {
            return Err(PathError::PathTooLong);
        }
        Ok(RemotePath(s))
    }

    pub fn parent(&self) -> Option<RemotePath> {
        if self.0 == "/" {
            return None;
        }
        let i = self.0.rfind('/')?;
        Some(RemotePath(if i == 0 { "/".to_string() } else { self.0[..i].to_string() }))
    }

    pub fn file_name(&self) -> Option<RemoteName> {
        if self.0 == "/" {
            return None;
        }
        RemoteName::new(&self.0[self.0.rfind('/')? + 1..]).ok()
    }

    /// Component-wise prefix test (`/a/b` is under `/a`, `/ab` is not).
    pub fn starts_with(&self, base: &RemotePath) -> bool {
        if base.0 == "/" {
            return true;
        }
        self.0 == base.0 || (self.0.starts_with(&base.0) && self.0.as_bytes().get(base.0.len()) == Some(&b'/'))
    }

    /// The text to put on the wire to the server. Not a local path.
    pub fn as_wire_str(&self) -> &str {
        &self.0
    }

    /// Stable, filesystem-safe key (16 hex chars, 64-bit FNV-1a of site + path) for the local cache.
    /// The remote text never appears in a local file name.
    pub fn cache_key(&self, site_id: &str) -> String {
        let mut h: u64 = 0xcbf29ce484222325;
        for b in site_id.bytes().chain(std::iter::once(0)).chain(self.0.bytes()) {
            h ^= b as u64;
            h = h.wrapping_mul(0x100000001b3);
        }
        format!("{h:016x}")
    }
}

impl<'de> Deserialize<'de> for RemotePath {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let s = String::deserialize(d)?;
        RemotePath::parse(&s).map_err(serde::de::Error::custom)
    }
}

impl std::fmt::Display for RemotePath {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalises() {
        assert_eq!(RemotePath::parse("/a//b/./c/../d").unwrap().as_wire_str(), "/a/b/d");
        assert_eq!(RemotePath::parse("/../../etc").unwrap().as_wire_str(), "/etc");
        assert_eq!(RemotePath::parse("/").unwrap(), RemotePath::root());
        assert_eq!(RemotePath::parse("/a/..").unwrap(), RemotePath::root());
    }

    #[test]
    fn rejects_bad_paths() {
        assert_eq!(RemotePath::parse("a/b"), Err(PathError::NotAbsolute));
        assert_eq!(RemotePath::parse(""), Err(PathError::NotAbsolute));
        assert_eq!(RemotePath::parse("/a\0b"), Err(PathError::Control));
        assert_eq!(RemotePath::parse("/a\nb"), Err(PathError::Control));
        assert_eq!(RemotePath::parse("/a/\u{202e}b"), Err(PathError::Invisible));
        assert_eq!(RemotePath::parse(&format!("/{}", "x".repeat(5000))), Err(PathError::PathTooLong));
        assert_eq!(RemotePath::parse(&format!("/{}", "x".repeat(256))), Err(PathError::NameTooLong));
    }

    #[test]
    fn names() {
        for bad in ["", ".", "..", "a/b", "a\0", "a\u{7f}", "a\u{85}", "a\u{200b}", "a\u{feff}"] {
            assert!(RemoteName::new(bad).is_err(), "{bad:?}");
        }
        assert!(RemoteName::new("..hidden").is_ok());
        assert!(RemoteName::new("é").is_ok());
        assert!(RemoteName::new(&"é".repeat(128)).is_err()); // 256 bytes
        assert!(RemoteName::new("CON").unwrap().is_local_unsafe());
        assert!(RemoteName::new("com1.txt").unwrap().is_local_unsafe());
        assert!(RemoteName::new("a:b").unwrap().is_local_unsafe());
        assert!(RemoteName::new("trail.").unwrap().is_local_unsafe());
        assert!(!RemoteName::new("COM0").unwrap().is_local_unsafe());
        assert!(!RemoteName::new("readme.md").unwrap().is_local_unsafe());
        assert!(RemoteName::from_bytes(&[0xff, 0xfe]).is_err());
    }

    #[test]
    fn join_parent_prefix() {
        let p = RemotePath::parse("/srv/www").unwrap();
        let c = p.join(&RemoteName::new("index.php").unwrap()).unwrap();
        assert_eq!(c.as_wire_str(), "/srv/www/index.php");
        assert_eq!(c.parent().unwrap(), p);
        assert_eq!(RemotePath::root().parent(), None);
        assert!(c.starts_with(&p));
        assert!(!RemotePath::parse("/srv/wwwx").unwrap().starts_with(&p));
        assert!(c.starts_with(&RemotePath::root()));
        assert_eq!(c.file_name().unwrap().as_wire_str(), "index.php");
    }

    #[test]
    fn cache_key_is_safe_and_distinct() {
        let a = RemotePath::parse("/a/B").unwrap().cache_key("s1");
        let b = RemotePath::parse("/a/b").unwrap().cache_key("s1");
        let c = RemotePath::parse("/a/B").unwrap().cache_key("s2");
        assert_eq!(a.len(), 16);
        assert!(a.chars().all(|c| c.is_ascii_hexdigit()));
        assert!(a != b && a != c);
    }

    #[test]
    fn serde_validates() {
        assert!(serde_json::from_str::<RemotePath>("\"/ok\"").is_ok());
        assert!(serde_json::from_str::<RemotePath>("\"relative\"").is_err());
        assert!(serde_json::from_str::<RemoteName>("\"a/b\"").is_err());
        assert_eq!(serde_json::to_string(&RemotePath::root()).unwrap(), "\"/\"");
    }
}
