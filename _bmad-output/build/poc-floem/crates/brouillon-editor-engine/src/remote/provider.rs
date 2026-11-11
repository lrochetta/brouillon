//! The surface a real (helper-backed) SFTP client has to implement.

use std::io;
use std::sync::Arc;
use std::time::SystemTime;

use super::path::{valid_entry_name, RemotePath};
use brouillon_safesave::RemoteFs;
use crate::fs_provider::{FsEntry, FsProvider};

/// One sanitised listing line. Built from a provider's `FsEntry`, never from a server-supplied path.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RemoteEntry {
    pub name: String,
    pub is_dir: bool,
    pub size: u64,
    pub mtime: Option<SystemTime>,
    pub mode: Option<u32>,
    pub is_symlink: bool,
}

impl RemoteEntry {
    /// `None` when the name is not a plain file name (a hostile server cannot inject `..` or `a/b`).
    pub fn from_fs(e: &FsEntry) -> Option<Self> {
        valid_entry_name(&e.name).then(|| Self { name: e.name.clone(), is_dir: e.is_dir, size: e.size, mtime: e.mtime, mode: e.mode, is_symlink: e.is_symlink })
    }

    /// The owner cannot write it (mode known and no owner-write bit).
    pub fn is_locked(&self) -> bool {
        self.mode.is_some_and(|m| m & 0o200 == 0)
    }
}

/// Blocking server access, always called from a hub worker thread, never from the UI thread.
/// `FsProvider::list_dir/stat/read/write` carry slash paths (see [`RemotePath::to_path`]).
pub trait RemoteProvider: FsProvider {
    fn mkdir(&self, path: &RemotePath) -> io::Result<()>;
    /// Rename or move; must fail with `AlreadyExists` instead of replacing an existing target.
    fn rename(&self, from: &RemotePath, to: &RemotePath) -> io::Result<()>;
    fn remove(&self, path: &RemotePath, is_dir: bool) -> io::Result<()>;

    /// The blocking file contract of the safe-save engine, for providers that can honour it
    /// (exclusive create, atomic replace, verified size). `None` = this provider only offers the
    /// plain upload of [`FsProvider::write`] (the in-memory demo server).
    fn safe_fs(&self) -> Option<Box<dyn RemoteFs + Send>> {
        None
    }

    /// The server stamp (size, mtime, mode) of `path` as of the last [`FsProvider::read`] of it,
    /// taken as stat / read / stat that agreed. It is the "base" a later safe save compares against.
    /// Consumed by the call.
    fn take_open_stamp(&self, _path: &RemotePath) -> Option<OpenStamp> {
        None
    }
}

/// What the server said about a file at the moment we read it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct OpenStamp {
    pub size: u64,
    pub mtime: i64,
    pub mode: u32,
}

pub type SharedProvider = Arc<dyn RemoteProvider>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hostile_entry_names_are_dropped() {
        let ok = FsEntry::new("index.php", "/x/index.php", false, 3);
        let evil = FsEntry::new("../../etc/passwd", "/x/y", false, 3);
        let slash = FsEntry::new("a/b", "/x/y", false, 3);
        assert!(RemoteEntry::from_fs(&ok).is_some());
        assert!(RemoteEntry::from_fs(&evil).is_none());
        assert!(RemoteEntry::from_fs(&slash).is_none());
    }

    #[test]
    fn locked_means_no_owner_write_bit() {
        let mut e = RemoteEntry::from_fs(&FsEntry::new("a", "/a", false, 1)).unwrap();
        assert!(!e.is_locked());
        e.mode = Some(0o444);
        assert!(e.is_locked());
        e.mode = Some(0o644);
        assert!(!e.is_locked());
    }
}
