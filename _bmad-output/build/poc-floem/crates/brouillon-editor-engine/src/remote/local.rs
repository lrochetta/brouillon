//! `LocalProvider`: this computer's own folders, served through the same blocking provider contract
//! as a server, so the browser lists them on the hub's worker thread (never on the UI thread) and
//! shows them through the same `ListingCache` / `RemoteTree` (loading rows, "N more" cap, filter).
//!
//! Paths are carried as [`RemotePath`]s: `/home/me/x` on Unix, `/C:/Users/me` on Windows, where the
//! root `/` lists the drives. Nothing here opens a socket or starts a thread by itself.

use std::io;
use std::path::{Path, PathBuf};

use super::path::RemotePath;
use super::provider::RemoteProvider;
use crate::fs_provider::{FsEntry, FsProvider, LocalFs};

/// Id of the always-available local source (never collides with a saved site id, which is a slug).
pub const LOCAL_SITE_ID: &str = "@local";

/// `RemotePath` to a native path. `None` = the drives list (Windows root). `Err` = not a local path.
pub fn to_native(p: &RemotePath, windows: bool) -> Result<Option<PathBuf>, String> {
    if !windows {
        return Ok(Some(PathBuf::from(p.as_str())));
    }
    if p.is_root() {
        return Ok(None);
    }
    let mut parts = p.as_str().trim_start_matches('/').split('/');
    let drive = parts.next().unwrap_or("");
    let b = drive.as_bytes();
    if b.len() != 2 || !b[0].is_ascii_alphabetic() || b[1] != b':' {
        return Err(format!("{p} is not a drive path"));
    }
    let mut out = format!("{}\\", drive.to_ascii_uppercase());
    let rest: Vec<&str> = parts.collect();
    out.push_str(&rest.join("\\"));
    Ok(Some(PathBuf::from(out)))
}

/// Native path to `RemotePath` (`C:\a\b` becomes `/C:/a/b`).
pub fn from_native(p: &Path) -> RemotePath {
    RemotePath::new(&p.to_string_lossy())
}

/// What the user reads: `C:\Users\me` on Windows (the drives list is "This PC"), the path elsewhere.
pub fn display_path(p: &RemotePath, windows: bool) -> String {
    if !windows {
        return p.to_string();
    }
    match to_native(p, true) {
        Ok(Some(n)) => n.to_string_lossy().into_owned(),
        Ok(None) => "This PC".into(),
        Err(_) => p.to_string(),
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub struct LocalProvider;

fn native(dir: &Path) -> io::Result<Option<PathBuf>> {
    to_native(&RemotePath::new(&dir.to_string_lossy()), cfg!(windows)).map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e))
}

fn needs_real_path(dir: &Path) -> io::Result<PathBuf> {
    native(dir)?.ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "Pick a drive first."))
}

impl FsProvider for LocalProvider {
    fn id(&self) -> &str {
        "local"
    }

    fn list_dir(&self, dir: &Path) -> io::Result<Vec<FsEntry>> {
        match native(dir)? {
            Some(real) => LocalFs.list_dir(&real),
            None => Ok(('A'..='Z')
                .filter(|c| Path::new(&format!("{c}:\\")).exists())
                .map(|c| FsEntry::new(format!("{c}:"), format!("/{c}:"), true, 0))
                .collect()),
        }
    }

    fn stat(&self, path: &Path) -> io::Result<FsEntry> {
        match native(path)? {
            Some(real) => LocalFs.stat(&real),
            None => Ok(FsEntry::new("This PC", "/", true, 0)),
        }
    }

    fn read(&self, path: &Path) -> io::Result<Vec<u8>> {
        LocalFs.read(&needs_real_path(path)?)
    }

    fn write(&self, path: &Path, data: &[u8]) -> io::Result<()> {
        LocalFs.write(&needs_real_path(path)?, data)
    }
}

impl RemoteProvider for LocalProvider {
    fn mkdir(&self, path: &RemotePath) -> io::Result<()> {
        std::fs::create_dir(needs_real_path(&path.to_path())?)
    }

    fn rename(&self, from: &RemotePath, to: &RemotePath) -> io::Result<()> {
        let (a, b) = (needs_real_path(&from.to_path())?, needs_real_path(&to.to_path())?);
        if b.exists() {
            return Err(io::Error::new(io::ErrorKind::AlreadyExists, "that name is taken"));
        }
        std::fs::rename(a, b)
    }

    /// A folder is only removed when it is empty (`remove_dir`): the browser never deletes a tree.
    fn remove(&self, path: &RemotePath, is_dir: bool) -> io::Result<()> {
        let real = needs_real_path(&path.to_path())?;
        if is_dir {
            std::fs::remove_dir(real)
        } else {
            std::fs::remove_file(real)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn windows_paths_round_trip_and_the_root_is_the_drive_list() {
        let p = RemotePath::new("C:\\Users\\me\\notes");
        assert_eq!(p.as_str(), "/C:/Users/me/notes");
        assert_eq!(to_native(&p, true).unwrap().unwrap(), PathBuf::from("C:\\Users\\me\\notes"));
        assert_eq!(to_native(&RemotePath::new("/c:"), true).unwrap().unwrap(), PathBuf::from("C:\\"));
        assert_eq!(to_native(&RemotePath::root(), true).unwrap(), None);
        assert!(to_native(&RemotePath::new("/etc/passwd"), true).is_err());
        assert_eq!(display_path(&RemotePath::root(), true), "This PC");
        assert_eq!(display_path(&RemotePath::new("/D:/x"), true), "D:\\x");
    }

    #[test]
    fn unix_paths_are_taken_as_they_are() {
        assert_eq!(to_native(&RemotePath::new("/home/me"), false).unwrap().unwrap(), PathBuf::from("/home/me"));
        assert_eq!(display_path(&RemotePath::new("/home/me"), false), "/home/me");
    }

    #[test]
    fn lists_creates_renames_and_removes_real_files_without_replacing() {
        let d = tempfile::tempdir().unwrap();
        let base = from_native(d.path());
        let lp = LocalProvider;
        let a = base.join("a.txt").unwrap();
        lp.write(&a.to_path(), b"hi").unwrap();
        lp.mkdir(&base.join("sub").unwrap()).unwrap();
        let names: Vec<String> = lp.list_dir(&base.to_path()).unwrap().into_iter().map(|e| e.name).collect();
        assert_eq!(names, ["sub", "a.txt"]);
        let b = base.join("b.txt").unwrap();
        lp.write(&b.to_path(), b"x").unwrap();
        assert_eq!(lp.rename(&a, &b).unwrap_err().kind(), io::ErrorKind::AlreadyExists);
        assert_eq!(std::fs::read(d.path().join("a.txt")).unwrap(), b"hi");
        lp.rename(&a, &base.join("c.txt").unwrap()).unwrap();
        assert!(d.path().join("c.txt").exists() && !d.path().join("a.txt").exists());
        lp.remove(&base.join("c.txt").unwrap(), false).unwrap();
        lp.remove(&base.join("sub").unwrap(), true).unwrap();
        assert!(!d.path().join("sub").exists());
    }

    #[test]
    fn a_folder_with_content_is_not_deleted() {
        let d = tempfile::tempdir().unwrap();
        std::fs::create_dir(d.path().join("full")).unwrap();
        std::fs::write(d.path().join("full").join("f.txt"), "x").unwrap();
        let p = from_native(&d.path().join("full"));
        assert!(LocalProvider.remove(&p, true).is_err());
        assert!(d.path().join("full").join("f.txt").exists());
    }

    #[cfg(windows)]
    #[test]
    fn the_root_lists_drives_on_windows() {
        let v = LocalProvider.list_dir(Path::new("/")).unwrap();
        assert!(v.iter().all(|e| e.is_dir && e.name.ends_with(':')));
        assert!(!v.is_empty());
    }
}
