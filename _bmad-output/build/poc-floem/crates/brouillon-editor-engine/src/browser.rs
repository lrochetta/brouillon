//! File-browser model, no UI and no I/O beyond one small settings file: sources, per-source history,
//! breadcrumbs, sort modes, kind glyphs, typed-path parsing, favorites / recent folders / layout
//! (`browser.toml`, never a secret) and detection of the cloud-drive folders the OS already syncs.
//!
//! The listing itself is the existing remote machinery (`remote::Explorer`); the local disk is just
//! one more source of it (`remote::local::LocalProvider`).

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::remote::provider::RemoteEntry;
use crate::remote::RemotePath;

// ---------------------------------------------------------------------------------------- sources

/// A cloud drive offered in the source switcher. Only the folder the vendor's own app syncs is used
/// (no API, no account, no network); without it the entry is a labelled placeholder.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CloudKind {
    GoogleDrive,
    Dropbox,
    Box,
}

impl CloudKind {
    pub const ALL: [CloudKind; 3] = [CloudKind::GoogleDrive, CloudKind::Dropbox, CloudKind::Box];

    pub fn label(self) -> &'static str {
        match self {
            CloudKind::GoogleDrive => "Google Drive",
            CloudKind::Dropbox => "Dropbox",
            CloudKind::Box => "Box",
        }
    }

    /// The vendor app that makes the folder exist ("synced by ...").
    pub fn sync_app(self) -> &'static str {
        match self {
            CloudKind::GoogleDrive => "Google Drive for desktop",
            CloudKind::Dropbox => "the Dropbox app",
            CloudKind::Box => "Box Drive",
        }
    }
}

/// A cloud folder found on this computer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SyncedFolder {
    pub kind: CloudKind,
    pub path: PathBuf,
}

/// Where the browser reads from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Source {
    Local,
    Site(String),
}

impl Source {
    /// Stable key stored in favorites (`local` / `site:<id>`), never a host or a secret.
    pub fn key(&self) -> String {
        match self {
            Source::Local => "local".into(),
            Source::Site(id) => format!("site:{id}"),
        }
    }

    pub fn from_key(k: &str) -> Option<Self> {
        match k {
            "local" => Some(Source::Local),
            _ => k.strip_prefix("site:").filter(|s| !s.is_empty()).map(|s| Source::Site(s.to_string())),
        }
    }
}

/// Look for the folders the cloud apps sync. Pure: every file-system question goes through the two
/// closures so the rules are testable. Order = display order; a folder is listed once.
/// `dropbox_info` is the text of Dropbox's own `info.json` (its real location when it is not the default).
pub fn detect_synced(home: &Path, windows: bool, dropbox_info: Option<&str>, exists: &dyn Fn(&Path) -> bool, children: &dyn Fn(&Path) -> Vec<PathBuf>) -> Vec<SyncedFolder> {
    let mut out: Vec<SyncedFolder> = Vec::new();
    let mut add = |kind: CloudKind, p: PathBuf| {
        if !out.iter().any(|f| f.path == p) {
            out.push(SyncedFolder { kind, path: p });
        }
    };
    if let Some(text) = dropbox_info {
        for p in dropbox_paths(text) {
            if exists(&p) {
                add(CloudKind::Dropbox, p);
            }
        }
    }
    for (kind, names) in [
        (CloudKind::GoogleDrive, &["Google Drive", "My Drive", "Mon Drive", "GoogleDrive"][..]),
        (CloudKind::Dropbox, &["Dropbox"][..]),
        (CloudKind::Box, &["Box", "Box Sync"][..]),
    ] {
        for n in names {
            let p = home.join(n);
            if exists(&p) {
                add(kind, p);
            }
        }
    }
    // macOS: Drive / Box mount under ~/Library/CloudStorage/<Vendor>-<account>.
    let cs = home.join("Library").join("CloudStorage");
    for c in children(&cs) {
        let n = c.file_name().map(|n| n.to_string_lossy().to_lowercase()).unwrap_or_default();
        if n.starts_with("googledrive") {
            add(CloudKind::GoogleDrive, c);
        } else if n.starts_with("dropbox") {
            add(CloudKind::Dropbox, c);
        } else if n.starts_with("box") {
            add(CloudKind::Box, c);
        }
    }
    // Windows: Google Drive for desktop mounts a virtual drive (often G:) holding "My Drive".
    if windows {
        for l in 'D'..='Z' {
            for n in ["My Drive", "Mon Drive"] {
                let p = PathBuf::from(format!("{l}:\\{n}"));
                if exists(&p) {
                    add(CloudKind::GoogleDrive, p);
                }
            }
        }
    }
    out
}

fn dropbox_paths(info_json: &str) -> Vec<PathBuf> {
    let Ok(v) = serde_json::from_str::<serde_json::Value>(info_json) else { return Vec::new() };
    ["personal", "business"].iter().filter_map(|k| v.get(*k)?.get("path")?.as_str().map(PathBuf::from)).collect()
}

/// [`detect_synced`] against the real disk. Touches drive letters, so call it off the UI thread.
pub fn detect_synced_system() -> Vec<SyncedFolder> {
    let Some(home) = dirs::home_dir() else { return Vec::new() };
    let info = [dirs::data_local_dir(), dirs::config_dir()].into_iter().flatten().map(|d| d.join("Dropbox").join("info.json")).find_map(|p| std::fs::read_to_string(p).ok());
    detect_synced(
        &home,
        cfg!(windows),
        info.as_deref(),
        &|p| p.is_dir(),
        &|p| std::fs::read_dir(p).map(|r| r.filter_map(|e| e.ok().map(|e| e.path())).collect()).unwrap_or_default(),
    )
}

// --------------------------------------------------------------------------------------- history

const HISTORY_CAP: usize = 100;

/// Back / forward stacks of one source. `visit` is called with the folder being LEFT.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct NavHistory {
    back: Vec<RemotePath>,
    fwd: Vec<RemotePath>,
}

impl NavHistory {
    pub fn visit(&mut self, leaving: &RemotePath) {
        if self.back.last() != Some(leaving) {
            self.back.push(leaving.clone());
            if self.back.len() > HISTORY_CAP {
                self.back.remove(0);
            }
        }
        self.fwd.clear();
    }

    pub fn can_back(&self) -> bool {
        !self.back.is_empty()
    }

    pub fn can_forward(&self) -> bool {
        !self.fwd.is_empty()
    }

    /// The folder to go back to; `current` becomes the forward target.
    pub fn back(&mut self, current: &RemotePath) -> Option<RemotePath> {
        let p = self.back.pop()?;
        self.fwd.push(current.clone());
        Some(p)
    }

    pub fn forward(&mut self, current: &RemotePath) -> Option<RemotePath> {
        let p = self.fwd.pop()?;
        self.back.push(current.clone());
        Some(p)
    }
}

// ------------------------------------------------------------------------------------ breadcrumbs

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Crumb {
    pub label: String,
    pub path: RemotePath,
}

/// One clickable segment per folder from the root down to `path`. `windows_local`: the root is the
/// drives list ("This PC") and the first segment is a drive.
pub fn breadcrumbs(path: &RemotePath, windows_local: bool) -> Vec<Crumb> {
    let mut out = vec![Crumb { label: if windows_local { "This PC".into() } else { "/".into() }, path: RemotePath::root() }];
    let mut cur = RemotePath::root();
    for seg in path.as_str().split('/').filter(|s| !s.is_empty()) {
        let Some(next) = cur.join(seg) else { break };
        out.push(Crumb { label: seg.to_string(), path: next.clone() });
        cur = next;
    }
    out
}

/// Keep the path bar short: the last `max` crumbs, the earlier ones folded into a leading "..." crumb
/// that goes to the nearest hidden folder.
pub fn fold_crumbs(mut crumbs: Vec<Crumb>, max: usize) -> Vec<Crumb> {
    if max < 2 || crumbs.len() <= max {
        return crumbs;
    }
    let cut = crumbs.len() - (max - 1);
    let target = crumbs[cut - 1].path.clone();
    let mut tail = crumbs.split_off(cut);
    tail.insert(0, Crumb { label: "...".into(), path: target });
    tail
}

/// Text typed in the path bar to a folder. `~` is the home folder (local only); a relative text is
/// taken from `current`. `None` for an empty text.
pub fn parse_typed_path(input: &str, current: &RemotePath, local: bool, home: Option<&Path>) -> Option<RemotePath> {
    let t = input.trim().trim_matches('"');
    if t.is_empty() {
        return None;
    }
    if local {
        if let Some(rest) = t.strip_prefix('~').filter(|r| r.is_empty() || r.starts_with(['/', '\\'])) {
            let home = home?;
            return Some(RemotePath::new(&format!("{}/{}", home.to_string_lossy(), rest)));
        }
    }
    let b = t.as_bytes();
    let drive = local && b.len() >= 2 && b[0].is_ascii_alphabetic() && b[1] == b':';
    if drive || t.starts_with(['/', '\\']) {
        Some(RemotePath::new(t))
    } else {
        Some(RemotePath::new(&format!("{current}/{t}")))
    }
}

// ----------------------------------------------------------------------------------------- sorting

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SortKey {
    #[default]
    Name,
    Size,
    Modified,
    Kind,
}

impl SortKey {
    pub fn label(self) -> &'static str {
        match self {
            SortKey::Name => "name",
            SortKey::Size => "size",
            SortKey::Modified => "modified",
            SortKey::Kind => "type",
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SortMode {
    pub key: SortKey,
    pub descending: bool,
}

impl SortMode {
    /// Toggle button: same key flips the direction, another key starts ascending.
    pub fn pick(self, key: SortKey) -> Self {
        if self.key == key {
            Self { key, descending: !self.descending }
        } else {
            Self { key, descending: false }
        }
    }

    /// "name ^" / "size v".
    pub fn caption(self) -> String {
        format!("{} {}", self.key.label(), if self.descending { "v" } else { "^" })
    }
}

fn ext_of(name: &str) -> String {
    match name.rsplit_once('.') {
        Some((stem, e)) if !stem.is_empty() => e.to_lowercase(),
        _ => String::new(),
    }
}

/// Folders first in every mode; then the chosen key (ties: name, case-insensitive).
pub fn sort_remote(v: &mut [RemoteEntry], mode: SortMode) {
    use std::cmp::Ordering;
    v.sort_by(|a, b| {
        let by_name = || a.name.to_lowercase().cmp(&b.name.to_lowercase()).then_with(|| a.name.cmp(&b.name));
        let key = match mode.key {
            SortKey::Name => by_name(),
            SortKey::Size => a.size.cmp(&b.size).then_with(by_name),
            SortKey::Modified => a.mtime.cmp(&b.mtime).then_with(by_name),
            SortKey::Kind => ext_of(&a.name).cmp(&ext_of(&b.name)).then_with(by_name),
        };
        let key = if mode.descending { key.reverse() } else { key };
        match (a.is_dir, b.is_dir) {
            (true, false) => Ordering::Less,
            (false, true) => Ordering::Greater,
            _ => key,
        }
    });
}

// ------------------------------------------------------------------------------------------ glyphs

/// A short plain-ASCII tag for what a file is (renders with any font; folders use the chevron instead).
pub fn kind_glyph(name: &str, is_dir: bool, is_symlink: bool) -> &'static str {
    if is_symlink {
        return "->";
    }
    if is_dir {
        return "";
    }
    match ext_of(name).as_str() {
        "rs" | "c" | "h" | "cpp" | "hpp" | "cs" | "go" | "java" | "kt" | "py" | "js" | "ts" | "tsx" | "jsx" | "php" | "rb" | "lua" | "sh" | "ps1" | "bat" | "sql" | "swift" => "{}",
        "html" | "htm" | "xml" | "svg" | "css" | "scss" | "vue" | "xhtml" => "<>",
        "md" | "markdown" | "txt" | "rst" | "log" | "tex" | "doc" | "docx" | "odt" | "rtf" => "T",
        "json" | "toml" | "yaml" | "yml" | "ini" | "cfg" | "conf" | "env" | "csv" | "tsv" | "lock" => "=",
        "png" | "jpg" | "jpeg" | "gif" | "webp" | "bmp" | "ico" | "tif" | "tiff" | "xcf" | "psd" => "*",
        "zip" | "gz" | "tar" | "7z" | "rar" | "xz" | "bz2" | "tgz" => "z",
        "exe" | "dll" | "so" | "dylib" | "bin" | "o" | "wasm" | "msi" => "o",
        _ => "-",
    }
}

// ------------------------------------------------------------------------------------------ prefs

/// A folder on a source. `source` is a [`Source::key`], `path` a [`RemotePath`] string.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Place {
    pub source: String,
    pub path: String,
}

const RECENT_CAP: usize = 12;
pub const SPLIT_MIN: f32 = 0.15;
pub const SPLIT_MAX: f32 = 0.85;

/// Everything the browser remembers, in `browser.toml`. Paths and site ids only: no password, no key,
/// no host name.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct BrowserPrefs {
    /// Shown by default (collapsible); View > Toggle Folder Tree (Ctrl+Shift+E) hides it and the choice is remembered.
    pub visible: bool,
    pub open_files_collapsed: bool,
    pub browser_collapsed: bool,
    /// Share of the sidebar height given to Open Files when both sections are open.
    pub split: f32,
    pub sort: SortMode,
    pub last_local: String,
    pub favorites: Vec<Place>,
    pub recents: Vec<Place>,
}

impl Default for BrowserPrefs {
    fn default() -> Self {
        Self { visible: true, open_files_collapsed: false, browser_collapsed: false, split: 0.3, sort: SortMode::default(), last_local: String::new(), favorites: Vec::new(), recents: Vec::new() }
    }
}

impl BrowserPrefs {
    /// A missing or damaged file is the defaults (the browser must always open).
    pub fn load(path: &Path) -> Self {
        let mut p: Self = std::fs::read_to_string(path).ok().and_then(|t| toml::from_str(&t).ok()).unwrap_or_default();
        p.split = clamp_split(p.split);
        p
    }

    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        let text = toml::to_string_pretty(self).map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let tmp = path.with_extension("toml.tmp");
        std::fs::write(&tmp, text)?;
        std::fs::rename(&tmp, path)
    }

    pub fn is_pinned(&self, p: &Place) -> bool {
        self.favorites.contains(p)
    }

    /// Pin or unpin; returns true when it is pinned afterwards.
    pub fn toggle_pin(&mut self, p: Place) -> bool {
        if let Some(i) = self.favorites.iter().position(|f| f == &p) {
            self.favorites.remove(i);
            false
        } else {
            self.favorites.push(p);
            true
        }
    }

    /// Most recent first, no duplicates, at most 12.
    pub fn push_recent(&mut self, p: Place) {
        self.recents.retain(|r| r != &p);
        self.recents.insert(0, p);
        self.recents.truncate(RECENT_CAP);
    }

    /// A site was forgotten: drop what pointed to it.
    pub fn forget_source(&mut self, key: &str) {
        self.favorites.retain(|f| f.source != key);
        self.recents.retain(|f| f.source != key);
    }
}

pub fn clamp_split(s: f32) -> f32 {
    if s.is_finite() {
        s.clamp(SPLIT_MIN, SPLIT_MAX)
    } else {
        0.3
    }
}

/// New split after the divider was dragged by `dy` pixels in a sidebar `total` pixels high.
pub fn drag_split(split: f32, dy: f64, total: f64) -> f32 {
    if total < 1.0 {
        return clamp_split(split);
    }
    clamp_split(split + (dy / total) as f32)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, SystemTime};

    fn e(name: &str, dir: bool, size: u64, secs: u64) -> RemoteEntry {
        RemoteEntry { name: name.into(), is_dir: dir, size, mtime: Some(SystemTime::UNIX_EPOCH + Duration::from_secs(secs)), mode: None, is_symlink: false }
    }

    fn names(v: &[RemoteEntry]) -> Vec<&str> {
        v.iter().map(|x| x.name.as_str()).collect()
    }

    #[test]
    fn history_goes_back_and_forward_and_a_new_visit_forgets_forward() {
        let (a, b, c, d) = (RemotePath::new("/a"), RemotePath::new("/b"), RemotePath::new("/c"), RemotePath::new("/d"));
        let mut h = NavHistory::default();
        assert!(!h.can_back() && !h.can_forward());
        h.visit(&a); // a -> b
        h.visit(&b); // b -> c
        assert_eq!(h.back(&c), Some(b.clone()));
        assert_eq!(h.back(&b), Some(a.clone()));
        assert_eq!(h.back(&a), None);
        assert_eq!(h.forward(&a), Some(b.clone()));
        assert!(h.can_forward());
        h.visit(&b); // b -> d, from the middle of the history
        assert!(!h.can_forward());
        assert_eq!(h.back(&d), Some(b));
    }

    #[test]
    fn history_is_capped() {
        let mut h = NavHistory::default();
        for i in 0..(HISTORY_CAP + 20) {
            h.visit(&RemotePath::new(&format!("/d{i}")));
        }
        assert_eq!(h.back.len(), HISTORY_CAP);
    }

    #[test]
    fn breadcrumbs_have_one_clickable_segment_per_folder() {
        let c = breadcrumbs(&RemotePath::new("/var/www/html"), false);
        let labels: Vec<_> = c.iter().map(|c| c.label.as_str()).collect();
        assert_eq!(labels, ["/", "var", "www", "html"]);
        assert_eq!(c[2].path, RemotePath::new("/var/www"));
        let w = breadcrumbs(&RemotePath::new("C:\\Users\\me"), true);
        assert_eq!(w[0].label, "This PC");
        assert_eq!(w[1].label, "C:");
        assert_eq!(w[1].path, RemotePath::new("/C:"));
        assert_eq!(breadcrumbs(&RemotePath::root(), false).len(), 1);
    }

    #[test]
    fn a_long_path_folds_its_head() {
        let c = breadcrumbs(&RemotePath::new("/a/b/c/d/e/f"), false);
        let f = fold_crumbs(c.clone(), 4);
        let labels: Vec<_> = f.iter().map(|c| c.label.as_str()).collect();
        assert_eq!(labels, ["...", "d", "e", "f"]);
        assert_eq!(f[0].path, RemotePath::new("/a/b/c"));
        assert_eq!(fold_crumbs(c.clone(), 20), c);
    }

    #[test]
    fn typed_paths_resolve_home_absolute_drive_and_relative() {
        let cur = RemotePath::new("/C:/work");
        let home = Path::new("C:\\Users\\me");
        assert_eq!(parse_typed_path("~", &cur, true, Some(home)), Some(RemotePath::new("/C:/Users/me")));
        assert_eq!(parse_typed_path("~/docs", &cur, true, Some(home)), Some(RemotePath::new("/C:/Users/me/docs")));
        assert_eq!(parse_typed_path("D:\\data", &cur, true, None), Some(RemotePath::new("/D:/data")));
        assert_eq!(parse_typed_path("\"C:\\a b\"", &cur, true, None), Some(RemotePath::new("/C:/a b")));
        assert_eq!(parse_typed_path("sub/dir", &cur, true, None), Some(RemotePath::new("/C:/work/sub/dir")));
        assert_eq!(parse_typed_path("..", &cur, true, None), Some(RemotePath::new("/C:")));
        assert_eq!(parse_typed_path("/etc", &RemotePath::new("/var"), false, None), Some(RemotePath::new("/etc")));
        assert_eq!(parse_typed_path("   ", &cur, true, None), None);
        // on a server `~` is just a name, and a drive letter is not special
        assert_eq!(parse_typed_path("~x", &RemotePath::new("/srv"), false, None), Some(RemotePath::new("/srv/~x")));
        assert_eq!(parse_typed_path("c:x", &RemotePath::new("/srv"), false, None), Some(RemotePath::new("/srv/c:x")));
    }

    #[test]
    fn sorting_keeps_folders_first_in_every_mode() {
        let mut v = vec![e("b.txt", false, 5, 30), e("A.rs", false, 100, 10), e("zdir", true, 0, 50), e("adir", true, 0, 5), e("c.md", false, 1, 20)];
        sort_remote(&mut v, SortMode::default());
        assert_eq!(names(&v), ["adir", "zdir", "A.rs", "b.txt", "c.md"]);
        sort_remote(&mut v, SortMode { key: SortKey::Size, descending: true });
        assert_eq!(names(&v), ["zdir", "adir", "A.rs", "b.txt", "c.md"]);
        sort_remote(&mut v, SortMode { key: SortKey::Size, descending: false });
        assert_eq!(names(&v), ["adir", "zdir", "c.md", "b.txt", "A.rs"]);
        sort_remote(&mut v, SortMode { key: SortKey::Modified, descending: true });
        assert_eq!(names(&v), ["zdir", "adir", "b.txt", "c.md", "A.rs"]);
        sort_remote(&mut v, SortMode { key: SortKey::Kind, descending: false });
        assert_eq!(names(&v), ["adir", "zdir", "c.md", "A.rs", "b.txt"]);
    }

    #[test]
    fn the_sort_toggle_flips_then_restarts() {
        let m = SortMode::default();
        assert_eq!(m.caption(), "name ^");
        let m = m.pick(SortKey::Name);
        assert_eq!(m.caption(), "name v");
        let m = m.pick(SortKey::Size);
        assert_eq!(m.caption(), "size ^");
    }

    #[test]
    fn glyphs_are_plain_ascii() {
        assert_eq!(kind_glyph("main.rs", false, false), "{}");
        assert_eq!(kind_glyph("index.HTML", false, false), "<>");
        assert_eq!(kind_glyph("notes.md", false, false), "T");
        assert_eq!(kind_glyph("Cargo.toml", false, false), "=");
        assert_eq!(kind_glyph("logo.png", false, false), "*");
        assert_eq!(kind_glyph("a.zip", false, false), "z");
        assert_eq!(kind_glyph("noext", false, false), "-");
        assert_eq!(kind_glyph(".gitignore", false, false), "-");
        assert_eq!(kind_glyph("src", true, false), "");
        assert_eq!(kind_glyph("link", false, true), "->");
        for n in ["a.rs", "a.html", "a.md", "a.json", "a.png", "a.zip", "a.exe", "a"] {
            assert!(kind_glyph(n, false, false).is_ascii());
        }
    }

    #[test]
    fn prefs_round_trip_pin_recent_and_survive_damage() {
        let d = tempfile::tempdir().unwrap();
        let f = d.path().join("browser.toml");
        assert_eq!(BrowserPrefs::load(&f), BrowserPrefs::default());
        let mut p = BrowserPrefs::default();
        let a = Place { source: "local".into(), path: "/C:/work".into() };
        assert!(p.toggle_pin(a.clone()));
        assert!(p.is_pinned(&a));
        for i in 0..20 {
            p.push_recent(Place { source: "site:vps".into(), path: format!("/d{i}") });
        }
        p.push_recent(Place { source: "site:vps".into(), path: "/d15".into() });
        assert_eq!(p.recents.len(), RECENT_CAP);
        assert_eq!(p.recents[0].path, "/d15");
        assert_eq!(p.recents.iter().filter(|r| r.path == "/d15").count(), 1);
        p.split = 0.5;
        p.browser_collapsed = true;
        p.sort = SortMode { key: SortKey::Modified, descending: true };
        p.save(&f).unwrap();
        assert_eq!(BrowserPrefs::load(&f), p);
        assert!(!p.clone().toggle_pin(a.clone()));
        std::fs::write(&f, "split = [broken").unwrap();
        assert_eq!(BrowserPrefs::load(&f), BrowserPrefs::default());
        std::fs::write(&f, "split = 9.0\nvisible = true\n").unwrap();
        let l = BrowserPrefs::load(&f);
        assert_eq!((l.split, l.visible), (SPLIT_MAX, true));
        let mut p2 = BrowserPrefs::default();
        p2.toggle_pin(Place { source: "site:x".into(), path: "/".into() });
        p2.push_recent(Place { source: "site:x".into(), path: "/".into() });
        p2.forget_source("site:x");
        assert!(p2.favorites.is_empty() && p2.recents.is_empty());
    }

    #[test]
    fn the_browser_is_shown_by_default_and_a_hidden_choice_is_remembered() {
        let dir = tempfile::tempdir().unwrap();
        let f = dir.path().join("browser.toml");
        assert!(BrowserPrefs::load(&f).visible, "no file: shown");
        std::fs::write(&f, "split = 0.4
").unwrap();
        assert!(BrowserPrefs::load(&f).visible, "a file without the key: shown");
        let mut p = BrowserPrefs::default();
        p.visible = false;
        p.save(&f).unwrap();
        assert!(!BrowserPrefs::load(&f).visible, "hidden by the user: stays hidden");
    }

    #[test]
    fn the_saved_file_holds_no_secret_field() {
        let mut p = BrowserPrefs::default();
        p.toggle_pin(Place { source: Source::Site("me-box".into()).key(), path: "/var/www".into() });
        let text = toml::to_string_pretty(&p).unwrap();
        for bad in ["password", "passphrase", "secret", "token", "host"] {
            assert!(!text.to_lowercase().contains(bad), "{bad} in {text}");
        }
    }

    #[test]
    fn the_divider_drag_stays_in_range() {
        assert!((drag_split(0.3, 100.0, 1000.0) - 0.4).abs() < 1e-6);
        assert_eq!(drag_split(0.3, 5000.0, 1000.0), SPLIT_MAX);
        assert_eq!(drag_split(0.3, -5000.0, 1000.0), SPLIT_MIN);
        assert_eq!(drag_split(0.3, 10.0, 0.0), 0.3);
        assert_eq!(clamp_split(f32::NAN), 0.3);
    }

    #[test]
    fn source_keys_round_trip() {
        for s in [Source::Local, Source::Site("me-box".into())] {
            assert_eq!(Source::from_key(&s.key()), Some(s));
        }
        assert_eq!(Source::from_key("site:"), None);
        assert_eq!(Source::from_key("cloud"), None);
    }

    fn probe(present: &'static [&'static str]) -> impl Fn(&Path) -> bool {
        move |p: &Path| present.iter().any(|x| Path::new(x) == p)
    }

    #[test]
    fn synced_folders_are_found_in_their_usual_places_and_only_once() {
        let home = Path::new("/h");
        let none = |_: &Path| Vec::new();
        let found = detect_synced(home, false, None, &probe(&["/h/Dropbox", "/h/Box", "/h/Google Drive"]), &none);
        let pairs: Vec<_> = found.iter().map(|f| (f.kind, f.path.to_string_lossy().replace('\\', "/"))).collect();
        assert_eq!(pairs, [(CloudKind::GoogleDrive, "/h/Google Drive".to_string()), (CloudKind::Dropbox, "/h/Dropbox".to_string()), (CloudKind::Box, "/h/Box".to_string())]);
        assert!(detect_synced(home, false, None, &probe(&[]), &none).is_empty());
        // Dropbox's own info.json wins and is not listed twice
        let info = r#"{"personal":{"path":"/h/Dropbox"},"business":{"path":"/data/Dropbox (Work)"}}"#;
        let found = detect_synced(home, false, Some(info), &probe(&["/h/Dropbox", "/data/Dropbox (Work)"]), &none);
        assert_eq!(found.len(), 2);
        assert!(found.iter().all(|f| f.kind == CloudKind::Dropbox));
        assert!(detect_synced(home, false, Some("not json"), &probe(&[]), &none).is_empty());
    }

    #[test]
    fn macos_cloud_storage_and_windows_virtual_drives_are_recognised() {
        let kids = |p: &Path| {
            if p.ends_with("CloudStorage") {
                vec![p.join("GoogleDrive-me@example.org"), p.join("Box-Box"), p.join("Other")]
            } else {
                Vec::new()
            }
        };
        let found = detect_synced(Path::new("/Users/me"), false, None, &probe(&[]), &kids);
        let kinds: Vec<_> = found.iter().map(|f| f.kind).collect();
        assert_eq!(kinds, [CloudKind::GoogleDrive, CloudKind::Box]);
        let found = detect_synced(Path::new("C:\\Users\\me"), true, None, &probe(&["G:\\My Drive", "H:\\Mon Drive"]), &|_| Vec::new());
        assert_eq!(found.len(), 2);
        assert!(found.iter().all(|f| f.kind == CloudKind::GoogleDrive));
        // off Windows the drive letters are never probed
        assert!(detect_synced(Path::new("/h"), false, None, &probe(&["G:\\My Drive"]), &|_| Vec::new()).is_empty());
    }
}
