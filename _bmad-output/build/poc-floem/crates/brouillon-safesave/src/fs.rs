//! The minimal blocking provider contract the saver talks to.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Kind {
    File,
    Dir,
    Symlink,
    Other,
}

/// What `stat` (lstat semantics: a symlink is reported as `Symlink`) returns.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Stat {
    pub kind: Kind,
    pub size: u64,
    /// Seconds since epoch (SFTP resolution).
    pub mtime: i64,
    /// Unix permission bits (0o644 ...).
    pub mode: u32,
}

/// Capabilities probed once at connect time.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Caps {
    /// `rename_replace` is atomic (SFTP `posix-rename@openssh.com`, local fs).
    pub posix_rename: bool,
    /// `hash` returns a server-computed SHA-256.
    pub hash: bool,
    /// `set_mode` is supported.
    pub set_mode: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FsError {
    NotFound,
    /// Rename target already exists (plain SFTP/FTP rename semantics).
    Exists,
    PermissionDenied,
    NoSpace,
    /// Network class: timeout, reset, disconnect. The saver queues instead of failing.
    Transport(String),
    Other(String),
    /// Test hook: simulated process death. The saver stops dead (no cleanup,
    /// no journal write), exactly like a killed process.
    Crash,
}

pub type FsResult<T> = Result<T, FsError>;

/// Blocking remote file provider. SFTP, FTPS and cloud adapters implement this.
/// Paths are `/`-separated strings.
pub trait RemoteFs {
    fn caps(&self) -> Caps;
    /// `None` when the path does not exist.
    fn stat(&mut self, path: &str) -> FsResult<Option<Stat>>;
    /// Resolve symlinks to the real path. `NotFound` for a dangling link.
    fn realpath(&mut self, path: &str) -> FsResult<String>;
    fn read(&mut self, path: &str) -> FsResult<Vec<u8>>;
    /// Create a new file exclusively (`Exists` if present) with optional creation mode.
    fn write_new(&mut self, path: &str, data: &[u8], mode: Option<u32>) -> FsResult<()>;
    /// Overwrite an existing file in place (truncate + write). Only for forced in-place saves.
    fn write_truncate(&mut self, path: &str, data: &[u8]) -> FsResult<()>;
    fn set_mode(&mut self, path: &str, mode: u32) -> FsResult<()>;
    /// Plain rename; `Exists` when `to` exists.
    fn rename(&mut self, from: &str, to: &str) -> FsResult<()>;
    /// Atomic rename over an existing target. Only called when `caps().posix_rename`.
    fn rename_replace(&mut self, from: &str, to: &str) -> FsResult<()>;
    fn remove(&mut self, path: &str) -> FsResult<()>;
    /// Server-side SHA-256 (lowercase hex). Only called when `caps().hash`.
    fn hash(&mut self, _path: &str) -> FsResult<Option<String>> {
        Ok(None)
    }
}

pub fn sha256_hex(data: &[u8]) -> String {
    let d = Sha256::digest(data);
    d.iter().map(|b| format!("{b:02x}")).collect()
}

pub(crate) fn parent(path: &str) -> &str {
    match path.rfind('/') {
        Some(0) => "/",
        Some(i) => &path[..i],
        None => "",
    }
}

pub(crate) fn file_name(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

pub(crate) fn sibling(path: &str, name: &str) -> String {
    let p = parent(path);
    if p.is_empty() {
        name.to_string()
    } else if p == "/" {
        format!("/{name}")
    } else {
        format!("{p}/{name}")
    }
}

/// Hex nonce, unique enough for temp names (std RandomState, no extra dependency).
pub(crate) fn nonce() -> String {
    use std::hash::{BuildHasher, Hasher};
    let v = std::collections::hash_map::RandomState::new().build_hasher().finish();
    format!("{:012x}", v & 0xffff_ffff_ffff)
}

/// Only names we generated may ever be deleted as orphans.
pub(crate) fn is_our_temp(path: &str, target: &str) -> bool {
    let n = file_name(path);
    parent(path) == parent(target)
        && n.starts_with('.')
        && n.contains(".brouillon-")
        && (n.ends_with(".tmp") || n.ends_with(".old"))
}
