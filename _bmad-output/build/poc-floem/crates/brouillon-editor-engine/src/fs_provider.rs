//! File-system abstraction behind the folder tree and (later) remote drives.
//!
//! The tree and the workspace only talk to `FsProvider`. `LocalFs` is the only
//! implementation today; an SFTP/FTPS/cloud provider plugs in by implementing the
//! same four calls (see `docs/REMOTE_FS.md`). Paths are plain `Path`s: a remote
//! provider uses its own slash-separated root as the `Path`.

use std::io;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FsEntry {
    pub name: String,
    pub path: PathBuf,
    pub is_dir: bool,
    pub size: u64,
    /// Last modification time when the provider knows it (needed for sorting and conflict checks).
    pub mtime: Option<SystemTime>,
    /// Unix permission bits when known (`None` on Windows and on servers that do not say).
    pub mode: Option<u32>,
    /// The entry is a symbolic link (never followed by the tree).
    pub is_symlink: bool,
}

impl FsEntry {
    /// Entry with only the basic facts; `mtime`, `mode` and `is_symlink` are unknown / false.
    pub fn new(name: impl Into<String>, path: impl Into<PathBuf>, is_dir: bool, size: u64) -> Self {
        Self { name: name.into(), path: path.into(), is_dir, size, mtime: None, mode: None, is_symlink: false }
    }
}

pub trait FsProvider: Send + Sync {
    /// Short stable id ("local", "sftp", ...), used in workspace keys.
    fn id(&self) -> &str;
    /// Direct children of `dir`: directories first, then files, each group sorted
    /// case-insensitively. Providers must not recurse.
    fn list_dir(&self, dir: &Path) -> io::Result<Vec<FsEntry>>;
    fn stat(&self, path: &Path) -> io::Result<FsEntry>;
    fn read(&self, path: &Path) -> io::Result<Vec<u8>>;
    fn write(&self, path: &Path, data: &[u8]) -> io::Result<()>;
}

/// Names never listed (VCS internals: huge, never edited by hand).
const HIDDEN: &[&str] = &[".git", ".hg", ".svn"];

/// Sort rule shared by every provider: folders first, then case-insensitive name.
pub fn sort_entries(v: &mut [FsEntry]) {
    v.sort_by(|a, b| b.is_dir.cmp(&a.is_dir).then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase())).then_with(|| a.name.cmp(&b.name)));
}

#[cfg(unix)]
fn mode_of(md: &std::fs::Metadata) -> Option<u32> {
    use std::os::unix::fs::PermissionsExt;
    Some(md.permissions().mode())
}

#[cfg(not(unix))]
fn mode_of(_: &std::fs::Metadata) -> Option<u32> {
    None
}

#[derive(Clone, Copy, Debug, Default)]
pub struct LocalFs;

impl FsProvider for LocalFs {
    fn id(&self) -> &str {
        "local"
    }

    fn list_dir(&self, dir: &Path) -> io::Result<Vec<FsEntry>> {
        let mut out = Vec::new();
        for e in std::fs::read_dir(dir)? {
            let Ok(e) = e else { continue };
            let name = e.file_name().to_string_lossy().into_owned();
            if HIDDEN.contains(&name.as_str()) {
                continue;
            }
            // A broken symlink / permission error on one entry must not hide its siblings.
            let Ok(md) = e.metadata() else { continue };
            let is_symlink = e.file_type().map(|t| t.is_symlink()).unwrap_or(false);
            out.push(FsEntry { mtime: md.modified().ok(), mode: mode_of(&md), is_symlink, ..FsEntry::new(name, e.path(), md.is_dir(), if md.is_dir() { 0 } else { md.len() }) });
        }
        sort_entries(&mut out);
        Ok(out)
    }

    fn stat(&self, path: &Path) -> io::Result<FsEntry> {
        let md = std::fs::metadata(path)?;
        let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| path.display().to_string());
        let is_symlink = std::fs::symlink_metadata(path).map(|m| m.file_type().is_symlink()).unwrap_or(false);
        Ok(FsEntry { mtime: md.modified().ok(), mode: mode_of(&md), is_symlink, ..FsEntry::new(name, path, md.is_dir(), md.len()) })
    }

    fn read(&self, path: &Path) -> io::Result<Vec<u8>> {
        std::fs::read(path)
    }

    fn write(&self, path: &Path, data: &[u8]) -> io::Result<()> {
        std::fs::write(path, data)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn local_lists_folders_first_sorted_and_hides_vcs() {
        let d = tempfile::tempdir().unwrap();
        for n in ["b.txt", "A.txt", "zeta", "Alpha"] {
            if n.contains('.') {
                std::fs::write(d.path().join(n), "x").unwrap();
            } else {
                std::fs::create_dir(d.path().join(n)).unwrap();
            }
        }
        std::fs::create_dir(d.path().join(".git")).unwrap();
        let names: Vec<String> = LocalFs.list_dir(d.path()).unwrap().into_iter().map(|e| e.name).collect();
        assert_eq!(names, ["Alpha", "zeta", "A.txt", "b.txt"]);
    }

    #[test]
    fn local_read_write_stat_roundtrip_through_the_trait() {
        let d = tempfile::tempdir().unwrap();
        let fs: &dyn FsProvider = &LocalFs;
        let p = d.path().join("f.txt");
        fs.write(&p, b"hello").unwrap();
        assert_eq!(fs.read(&p).unwrap(), b"hello");
        let st = fs.stat(&p).unwrap();
        assert!(!st.is_dir && st.size == 5 && st.name == "f.txt");
        assert!(fs.list_dir(&d.path().join("nope")).is_err());
    }
}
