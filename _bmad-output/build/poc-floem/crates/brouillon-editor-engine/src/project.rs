//! Project file: the folders, open files (with their carets) and active file of a piece of work, saved as one small
//! TOML file. It lives under the settings directory (`<settings>/projects/`), never inside the project folder
//! unless the user picks that place in the dialog. Pure data + atomic I/O; the shell applies it.

use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

pub const EXTENSION: &str = "brouillon-project.toml";

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectFile {
    pub path: String,
    #[serde(default)]
    pub line: Option<usize>,
    #[serde(default)]
    pub col: Option<usize>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Project {
    pub name: String,
    /// Root folders. The folder tree shows the first one; the others are kept for a later multi-root view.
    #[serde(default)]
    pub folders: Vec<String>,
    #[serde(default)]
    pub files: Vec<ProjectFile>,
    /// Path of the file that was the active tab.
    #[serde(default)]
    pub active: Option<String>,
}

impl Project {
    pub fn to_toml(&self) -> String {
        toml::to_string_pretty(self).unwrap_or_default()
    }

    pub fn from_toml(text: &str) -> Result<Self, String> {
        toml::from_str(text).map_err(|e| format!("not a project file: {e}"))
    }

    /// Atomic write (temp file next to the target, then rename); creates the parent folder.
    pub fn save(&self, path: &Path) -> io::Result<()> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let tmp = path.with_extension("tmp");
        std::fs::write(&tmp, self.to_toml())?;
        std::fs::rename(&tmp, path)
    }

    pub fn load(path: &Path) -> Result<Self, String> {
        let text = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
        Self::from_toml(&text)
    }

    /// Folder to show in the tree: the first one that still exists.
    pub fn first_folder(&self) -> Option<PathBuf> {
        self.folders.iter().map(PathBuf::from).find(|p| p.is_dir())
    }

    /// `path[:line[:col]]` targets of the files that still exist, in saved order except that the active file comes
    /// last (the last file opened is the one shown), in the command-line form.
    pub fn targets(&self) -> Vec<String> {
        let live: Vec<&ProjectFile> = self.files.iter().filter(|f| Path::new(&f.path).is_file()).collect();
        let (act, rest): (Vec<&ProjectFile>, Vec<&ProjectFile>) = live.into_iter().partition(|f| self.active.as_deref() == Some(f.path.as_str()));
        rest.into_iter()
            .chain(act)
            .map(|f| match (f.line, f.col) {
                (Some(l), Some(c)) => format!("{}:{l}:{c}", f.path),
                (Some(l), None) => format!("{}:{l}", f.path),
                _ => f.path.clone(),
            })
            .collect()
    }
}

/// `<settings>/projects`.
pub fn projects_dir(settings_root: &Path) -> PathBuf {
    settings_root.join("projects")
}

/// Default file for a project called `name`: `<settings>/projects/<name>.brouillon-project.toml`, `name` cleaned of
/// characters a file name cannot hold.
pub fn default_file(settings_root: &Path, name: &str) -> PathBuf {
    let clean: String = name.chars().map(|c| if c.is_alphanumeric() || matches!(c, '-' | '_' | ' ' | '.') { c } else { '_' }).collect();
    let clean = clean.trim().trim_matches('.');
    projects_dir(settings_root).join(format!("{}.{EXTENSION}", if clean.is_empty() { "project" } else { clean }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample(dir: &Path) -> Project {
        let f = dir.join("a.txt");
        std::fs::write(&f, "x").unwrap();
        Project {
            name: "demo".into(),
            folders: vec![dir.to_string_lossy().into_owned(), "Z:-gone".into()],
            files: vec![
                ProjectFile { path: f.to_string_lossy().into_owned(), line: Some(3), col: Some(2) },
                ProjectFile { path: dir.join("missing.txt").to_string_lossy().into_owned(), line: None, col: None },
            ],
            active: Some(f.to_string_lossy().into_owned()),
        }
    }

    #[test]
    fn round_trips_through_a_file_and_skips_what_no_longer_exists() {
        let d = tempfile::tempdir().unwrap();
        let p = sample(d.path());
        let file = default_file(d.path(), "demo");
        p.save(&file).unwrap();
        assert!(file.to_string_lossy().ends_with("demo.brouillon-project.toml") && file.starts_with(d.path().join("projects")));
        assert_eq!(Project::load(&file).unwrap(), p);
        assert_eq!(p.first_folder(), Some(d.path().to_path_buf()));
        let t = p.targets();
        assert_eq!(t.len(), 1, "the missing file is skipped");
        assert!(t[0].ends_with("a.txt:3:2"));
    }


    #[test]
    fn the_active_file_is_opened_last_so_it_ends_up_shown() {
        let d = tempfile::tempdir().unwrap();
        let (a, b) = (d.path().join("a.txt"), d.path().join("b.txt"));
        std::fs::write(&a, "x").unwrap();
        std::fs::write(&b, "x").unwrap();
        let pf = |p: &Path| ProjectFile { path: p.to_string_lossy().into_owned(), line: None, col: None };
        let p = Project { name: "n".into(), folders: vec![], files: vec![pf(&a), pf(&b)], active: Some(a.to_string_lossy().into_owned()) };
        let t = p.targets();
        assert!(t[0].ends_with("b.txt") && t[1].ends_with("a.txt"));
    }
    #[test]
    fn a_damaged_file_is_an_error_not_a_panic_and_unknown_keys_are_tolerated() {
        assert!(Project::from_toml("this is = = not toml").is_err());
        assert!(Project::from_toml("name = \"x\"\nfuture = 1\n").is_ok());
        assert!(Project::from_toml("folders = []").is_err(), "name is required");
    }

    #[test]
    fn default_file_name_is_clean() {
        let f = default_file(Path::new("S"), "a/b:c*?");
        assert_eq!(f.file_name().unwrap().to_string_lossy(), "a_b_c__.brouillon-project.toml");
        assert_eq!(default_file(Path::new("S"), "  ").file_name().unwrap().to_string_lossy(), "project.brouillon-project.toml");
    }
}
