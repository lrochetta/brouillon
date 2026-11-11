//! Config/data locations and portable mode (BK-073).
//!
//! Every persistent path goes through `Paths`. With the portable marker file
//! next to the executable (or an explicit `--settings-dir`), the whole tree is
//! redirected and nothing is written under the user profile.

use std::path::{Path, PathBuf};

/// File whose presence next to the executable enables portable mode.
pub const PORTABLE_MARKER: &str = "brouillon.portable";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Paths {
    root: PathBuf,
    portable: bool,
}

impl Paths {
    /// Pure resolver: `override_dir` (--settings-dir) > marker in `exe_dir`
    /// (data lives in `exe_dir/data`) > `user_root` (e.g. `~/.brouillon`).
    pub fn resolve(exe_dir: &Path, override_dir: Option<&Path>, user_root: PathBuf) -> Self {
        if let Some(dir) = override_dir {
            return Self { root: dir.to_path_buf(), portable: true };
        }
        if exe_dir.join(PORTABLE_MARKER).is_file() {
            return Self { root: exe_dir.join("data"), portable: true };
        }
        Self { root: user_root, portable: false }
    }

    /// Resolve from the running process (`~/.brouillon` unless portable).
    pub fn system(override_dir: Option<&Path>) -> Option<Self> {
        let exe_dir = std::env::current_exe().ok()?.parent()?.to_path_buf();
        let user_root = dirs::home_dir()?.join(".brouillon");
        Some(Self::resolve(&exe_dir, override_dir, user_root))
    }

    pub fn is_portable(&self) -> bool {
        self.portable
    }
    pub fn root(&self) -> &Path {
        &self.root
    }
    pub fn settings_file(&self) -> PathBuf {
        self.root.join("settings.toml")
    }
    pub fn keymap_file(&self) -> PathBuf {
        self.root.join("keymap.toml")
    }
    pub fn session_file(&self) -> PathBuf {
        self.root.join("session.toml")
    }
    pub fn tools_file(&self) -> PathBuf {
        self.root.join("tools.toml")
    }
    pub fn journal_dir(&self) -> PathBuf {
        self.root.join("journal")
    }
    pub fn backup_dir(&self) -> PathBuf {
        self.root.join("backup")
    }
    pub fn filetypes_dir(&self) -> PathBuf {
        self.root.join("filetypes")
    }
    /// Cliptext libraries (`*.toml`, EditPlus `*.ctl`); `default.toml` is created on first use.
    pub fn cliptext_dir(&self) -> PathBuf {
        self.root.join("cliptext")
    }
    /// User language definitions (`*.toml`), loaded by the syntax registry.
    pub fn syntax_dir(&self) -> PathBuf {
        self.root.join("syntax")
    }
    /// Hex-mode regions (names and offsets only, one small file per document path hash). Never
    /// written next to the user's file.
    pub fn hex_regions_dir(&self) -> PathBuf {
        self.root.join("hex-regions")
    }
    /// Optional modules (absent by default). Never read before the first file of a module's kind is opened.
    pub fn modules_dir(&self) -> PathBuf {
        self.root.join("modules")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn all(p: &Paths) -> Vec<PathBuf> {
        vec![
            p.settings_file(), p.keymap_file(), p.session_file(), p.tools_file(),
            p.journal_dir(), p.backup_dir(), p.filetypes_dir(), p.cliptext_dir(), p.hex_regions_dir(),
        ]
    }

    #[test]
    fn default_uses_user_root() {
        let exe = std::env::temp_dir().join("brouillon-paths-none");
        let p = Paths::resolve(&exe, None, PathBuf::from("/home/u/.brouillon"));
        assert!(!p.is_portable());
        assert!(p.keymap_file().starts_with("/home/u/.brouillon"));
    }

    #[test]
    fn marker_redirects_everything_away_from_profile() {
        let dir = std::env::temp_dir().join(format!("brouillon-portable-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join(PORTABLE_MARKER), "").unwrap();
        let user = PathBuf::from("/home/u/.brouillon");
        let p = Paths::resolve(&dir, None, user.clone());
        assert!(p.is_portable());
        assert!(all(&p).iter().all(|x| x.starts_with(&dir) && !x.starts_with(&user)));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn settings_dir_flag_beats_marker() {
        let p = Paths::resolve(Path::new("."), Some(Path::new("/custom")), PathBuf::from("/home/u/.brouillon"));
        assert!(p.is_portable());
        assert!(all(&p).iter().all(|x| x.starts_with("/custom")));
    }
}
