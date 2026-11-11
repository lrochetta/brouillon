//! Workspace = one opened folder + a lazily expanded tree over an `FsProvider`.
//!
//! Only expanded folders are ever listed, so opening a 200 000-file repository costs
//! one `read_dir`. Expansion state, the selected row and the last opened file are
//! remembered per workspace root in `<settings dir>/workspaces/<id>.toml`, never in
//! the project folder itself.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::fs_provider::{FsEntry, FsProvider, LocalFs};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TreeRow {
    pub path: PathBuf,
    pub name: String,
    pub depth: usize,
    pub is_dir: bool,
    pub expanded: bool,
}

/// What is remembered between sessions for one root.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspaceState {
    pub root: String,
    #[serde(default)]
    pub expanded: Vec<String>,
    #[serde(default)]
    pub selected: Option<String>,
    #[serde(default)]
    pub last_file: Option<String>,
}

#[derive(Clone)]
pub struct Workspace {
    pub root: PathBuf,
    fs: Arc<dyn FsProvider>,
    expanded: BTreeSet<PathBuf>,
    pub selected: Option<PathBuf>,
    pub last_file: Option<PathBuf>,
}

/// File name of the remembered state of `root` (stable across runs, no path characters).
pub fn state_file_name(provider_id: &str, root: &Path) -> String {
    let mut h = Sha256::new();
    h.update(provider_id.as_bytes());
    h.update([0]);
    h.update(root.to_string_lossy().to_lowercase().as_bytes());
    let d = h.finalize();
    let hex: String = d.iter().take(8).map(|b| format!("{b:02x}")).collect();
    format!("{hex}.toml")
}

impl Workspace {
    pub fn open(root: impl Into<PathBuf>, fs: Arc<dyn FsProvider>) -> Self {
        let mut w = Self { root: root.into(), fs, expanded: BTreeSet::new(), selected: None, last_file: None };
        w.expanded.insert(w.root.clone());
        w
    }

    pub fn open_local(root: impl Into<PathBuf>) -> Self {
        Self::open(root, Arc::new(LocalFs))
    }

    pub fn provider(&self) -> &Arc<dyn FsProvider> {
        &self.fs
    }

    pub fn name(&self) -> String {
        self.root.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| self.root.display().to_string())
    }

    pub fn is_expanded(&self, dir: &Path) -> bool {
        self.expanded.contains(dir)
    }

    /// Expand or collapse a folder; returns the new state.
    pub fn toggle(&mut self, dir: &Path) -> bool {
        if self.expanded.remove(dir) {
            false
        } else {
            self.expanded.insert(dir.to_path_buf());
            true
        }
    }

    pub fn expand(&mut self, dir: &Path) {
        self.expanded.insert(dir.to_path_buf());
    }

    pub fn collapse(&mut self, dir: &Path) {
        if dir != self.root {
            self.expanded.remove(dir);
        }
    }

    /// Visible rows, depth-first, listing only expanded folders (the root itself is not a row).
    pub fn rows(&self) -> Vec<TreeRow> {
        let mut out = Vec::new();
        self.walk(&self.root, 0, &mut out);
        out
    }

    fn walk(&self, dir: &Path, depth: usize, out: &mut Vec<TreeRow>) {
        let Ok(children) = self.fs.list_dir(dir) else { return };
        for FsEntry { name, path, is_dir, .. } in children {
            let expanded = is_dir && self.expanded.contains(&path);
            out.push(TreeRow { path: path.clone(), name, depth, is_dir, expanded });
            if expanded {
                self.walk(&path, depth + 1, out);
            }
        }
    }

    /// Move the selection by `delta` rows (clamped); returns the new selected path.
    pub fn move_selection(&mut self, delta: isize) -> Option<PathBuf> {
        let rows = self.rows();
        if rows.is_empty() {
            return None;
        }
        let cur = self.selected.as_ref().and_then(|s| rows.iter().position(|r| &r.path == s));
        let next = match cur {
            None => 0,
            Some(i) => (i as isize + delta).clamp(0, rows.len() as isize - 1) as usize,
        };
        self.selected = Some(rows[next].path.clone());
        self.selected.clone()
    }

    /// Enter / double-click on `path`: a folder toggles, a file is returned to be opened.
    pub fn activate(&mut self, path: &Path) -> Option<PathBuf> {
        self.selected = Some(path.to_path_buf());
        match self.fs.stat(path) {
            Ok(e) if e.is_dir => {
                self.toggle(path);
                None
            }
            Ok(_) => {
                self.last_file = Some(path.to_path_buf());
                Some(path.to_path_buf())
            }
            Err(_) => None,
        }
    }

    pub fn state(&self) -> WorkspaceState {
        let s = |p: &Path| p.to_string_lossy().into_owned();
        WorkspaceState {
            root: s(&self.root),
            expanded: self.expanded.iter().map(|p| s(p)).collect(),
            selected: self.selected.as_deref().map(s),
            last_file: self.last_file.as_deref().map(s),
        }
    }

    pub fn apply_state(&mut self, st: &WorkspaceState) {
        // Only folders that still exist and live under the root are re-expanded.
        for e in &st.expanded {
            let p = PathBuf::from(e);
            if p.starts_with(&self.root) && self.fs.stat(&p).map(|s| s.is_dir).unwrap_or(false) {
                self.expanded.insert(p);
            }
        }
        self.selected = st.selected.as_ref().map(PathBuf::from).filter(|p| self.fs.stat(p).is_ok());
        self.last_file = st.last_file.as_ref().map(PathBuf::from).filter(|p| self.fs.stat(p).is_ok());
    }

    /// Save into `dir` (created if missing). Best-effort: errors are returned, never fatal.
    pub fn save(&self, dir: &Path) -> std::io::Result<()> {
        std::fs::create_dir_all(dir)?;
        let text = toml::to_string(&self.state()).map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e.to_string()))?;
        std::fs::write(dir.join(state_file_name(self.fs.id(), &self.root)), text)
    }

    /// Restore the remembered state of this root from `dir`; a missing/corrupt file is ignored.
    pub fn restore(&mut self, dir: &Path) -> bool {
        let f = dir.join(state_file_name(self.fs.id(), &self.root));
        match std::fs::read_to_string(f).ok().and_then(|t| toml::from_str::<WorkspaceState>(&t).ok()) {
            Some(st) => {
                self.apply_state(&st);
                true
            }
            None => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> tempfile::TempDir {
        let d = tempfile::tempdir().unwrap();
        let r = d.path();
        std::fs::create_dir_all(r.join("src/inner")).unwrap();
        std::fs::create_dir_all(r.join("docs")).unwrap();
        std::fs::write(r.join("README.md"), "r").unwrap();
        std::fs::write(r.join("src/main.rs"), "m").unwrap();
        std::fs::write(r.join("src/inner/deep.txt"), "d").unwrap();
        d
    }

    #[test]
    fn tree_is_lazy_and_expands_on_toggle() {
        let d = fixture();
        let mut w = Workspace::open_local(d.path());
        let names = |w: &Workspace| w.rows().iter().map(|r| format!("{}{}", " ".repeat(r.depth), r.name)).collect::<Vec<_>>();
        assert_eq!(names(&w), ["docs", "src", "README.md"]);
        assert!(w.toggle(&d.path().join("src")));
        assert_eq!(names(&w), ["docs", "src", " inner", " main.rs", "README.md"]);
        w.toggle(&d.path().join("src/inner"));
        assert!(names(&w).contains(&"  deep.txt".to_string()));
        assert!(!w.toggle(&d.path().join("src")));
        assert_eq!(names(&w), ["docs", "src", "README.md"]);
    }

    #[test]
    fn activate_opens_files_and_toggles_folders() {
        let d = fixture();
        let mut w = Workspace::open_local(d.path());
        assert_eq!(w.activate(&d.path().join("src")), None);
        assert!(w.is_expanded(&d.path().join("src")));
        let f = d.path().join("src/main.rs");
        assert_eq!(w.activate(&f), Some(f.clone()));
        assert_eq!(w.last_file, Some(f));
    }

    #[test]
    fn selection_moves_and_clamps() {
        let d = fixture();
        let mut w = Workspace::open_local(d.path());
        assert_eq!(w.move_selection(1), Some(d.path().join("docs")));
        assert_eq!(w.move_selection(1), Some(d.path().join("src")));
        assert_eq!(w.move_selection(99), Some(d.path().join("README.md")));
        assert_eq!(w.move_selection(-99), Some(d.path().join("docs")));
    }

    #[test]
    fn state_is_remembered_per_workspace_and_stale_entries_dropped() {
        let d = fixture();
        let store = tempfile::tempdir().unwrap();
        let mut w = Workspace::open_local(d.path());
        w.toggle(&d.path().join("src"));
        w.toggle(&d.path().join("src/inner"));
        w.activate(&d.path().join("src/main.rs"));
        w.save(store.path()).unwrap();

        let mut again = Workspace::open_local(d.path());
        assert!(again.restore(store.path()));
        assert!(again.is_expanded(&d.path().join("src/inner")));
        assert_eq!(again.last_file, Some(d.path().join("src/main.rs")));

        // another root has its own state
        let other = fixture();
        let mut o = Workspace::open_local(other.path());
        assert!(!o.restore(store.path()));
        assert_ne!(state_file_name("local", d.path()), state_file_name("local", other.path()));
        assert_ne!(state_file_name("local", d.path()), state_file_name("sftp", d.path()));

        // a folder deleted since is not re-expanded; a foreign path is ignored
        std::fs::remove_dir_all(d.path().join("src/inner")).unwrap();
        let mut third = Workspace::open_local(d.path());
        third.apply_state(&WorkspaceState { root: String::new(), expanded: vec![d.path().join("src/inner").to_string_lossy().into(), "Z:/elsewhere".into()], ..Default::default() });
        assert!(!third.is_expanded(&d.path().join("src/inner")));
        assert!(!third.is_expanded(Path::new("Z:/elsewhere")));
    }

    #[test]
    fn corrupt_state_file_is_ignored() {
        let d = fixture();
        let store = tempfile::tempdir().unwrap();
        std::fs::write(store.path().join(state_file_name("local", d.path())), "not = [toml").unwrap();
        let mut w = Workspace::open_local(d.path());
        assert!(!w.restore(store.path()));
    }

    #[test]
    fn a_provider_other_than_local_plugs_in() {
        // The tree knows nothing about the disk: a fake provider is enough.
        struct Mem;
        impl FsProvider for Mem {
            fn id(&self) -> &str {
                "mem"
            }
            fn list_dir(&self, dir: &Path) -> std::io::Result<Vec<FsEntry>> {
                let e = |n: &str, is_dir| FsEntry::new(n, dir.join(n), is_dir, 0);
                Ok(if dir == Path::new("/r") { vec![e("d", true), e("a.txt", false)] } else { vec![e("x.txt", false)] })
            }
            fn stat(&self, p: &Path) -> std::io::Result<FsEntry> {
                Ok(FsEntry::new("n", p, p == Path::new("/r/d"), 0))
            }
            fn read(&self, _: &Path) -> std::io::Result<Vec<u8>> {
                Ok(b"remote".to_vec())
            }
            fn write(&self, _: &Path, _: &[u8]) -> std::io::Result<()> {
                Ok(())
            }
        }
        let mut w = Workspace::open("/r", Arc::new(Mem));
        w.activate(Path::new("/r/d"));
        let names: Vec<_> = w.rows().into_iter().map(|r| r.name).collect();
        assert_eq!(names, ["d", "x.txt", "a.txt"]);
    }
}
