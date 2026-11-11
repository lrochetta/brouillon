//! `RemoteTree`: expansion / selection / filter state of one site's tree and the pure function that
//! turns it plus the [`ListingCache`] into a flat list of rows. It reads the cache only, so it is
//! safe on the UI thread; the folders it needs but does not have are returned for the caller to
//! request.

use std::collections::BTreeSet;

use super::cache::{Entries, Listing, ListingCache};
use super::path::RemotePath;
use super::provider::RemoteEntry;
use crate::browser::{sort_remote, SortMode};

/// Rows drawn for one folder at most (spec 02 5.1); the rest collapses into a "more" row.
pub const MAX_ROWS_PER_DIR: usize = 5_000;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RemoteRowKind {
    Entry,
    /// "Loading..." placeholder under an expanded folder.
    Loading,
    /// The folder could not be listed; the message is plain language.
    Error(String),
    Empty,
    /// `n` entries are not drawn.
    More(usize),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RemoteRow {
    pub kind: RemoteRowKind,
    pub path: RemotePath,
    pub name: String,
    pub depth: usize,
    pub is_dir: bool,
    pub expanded: bool,
    pub is_symlink: bool,
    pub locked: bool,
    pub size: u64,
}

impl RemoteRow {
    fn info(kind: RemoteRowKind, parent: &RemotePath, depth: usize, name: String) -> Self {
        Self { kind, path: parent.clone(), name, depth, is_dir: false, expanded: false, is_symlink: false, locked: false, size: 0 }
    }

    pub fn is_entry(&self) -> bool {
        self.kind == RemoteRowKind::Entry
    }
}

#[derive(Clone, Debug)]
pub struct RemoteTree {
    pub root: RemotePath,
    expanded: BTreeSet<RemotePath>,
    pub selected: Option<RemotePath>,
    pub filter: String,
    /// Order of the entries inside each folder (folders always first).
    pub sort: SortMode,
}

impl RemoteTree {
    pub fn new(root: RemotePath) -> Self {
        let mut expanded = BTreeSet::new();
        expanded.insert(root.clone());
        Self { root, expanded, selected: None, filter: String::new(), sort: SortMode::default() }
    }

    /// Make `p` the root (navigation: breadcrumb, up, back). Expansion of folders below is kept.
    pub fn go_to(&mut self, p: RemotePath) {
        self.expanded.insert(p.clone());
        self.root = p;
        self.selected = None;
        self.filter.clear();
    }

    pub fn is_expanded(&self, p: &RemotePath) -> bool {
        self.expanded.contains(p)
    }

    pub fn toggle(&mut self, p: &RemotePath) -> bool {
        if !self.expanded.remove(p) {
            self.expanded.insert(p.clone());
            true
        } else {
            false
        }
    }

    pub fn expand(&mut self, p: &RemotePath) {
        self.expanded.insert(p.clone());
    }

    pub fn collapse(&mut self, p: &RemotePath) {
        if p != &self.root {
            self.expanded.remove(p);
        }
    }

    pub fn expanded_dirs(&self) -> impl Iterator<Item = &RemotePath> {
        self.expanded.iter()
    }

    /// Visible rows and the expanded folders that have no cache entry yet (to be requested).
    /// The root is not a row (only its loading / error placeholder). With a filter, only loaded folders are searched and no placeholder
    /// rows are produced (client-side filter: no network).
    pub fn rows(&self, cache: &ListingCache, site: &str) -> (Vec<RemoteRow>, Vec<RemotePath>) {
        let mut out = Vec::new();
        let mut missing = Vec::new();
        let needle = self.filter.trim().to_lowercase();
        if needle.is_empty() {
            self.walk(cache, site, &self.root, 0, &mut out, &mut missing);
        } else {
            self.walk_filtered(cache, site, &self.root, 0, &needle, &mut out);
        }
        (out, missing)
    }

    /// Entries of a folder in the chosen order (the hub's name order is already the default one).
    fn ordered<'a>(&self, entries: &'a Entries) -> std::borrow::Cow<'a, [RemoteEntry]> {
        if self.sort == SortMode::default() {
            std::borrow::Cow::Borrowed(entries.as_slice())
        } else {
            let mut v = entries.to_vec();
            sort_remote(&mut v, self.sort);
            std::borrow::Cow::Owned(v)
        }
    }

    fn entry_row(&self, dir: &RemotePath, e: &RemoteEntry, depth: usize) -> Option<RemoteRow> {
        let path = dir.join(&e.name)?;
        Some(RemoteRow { kind: RemoteRowKind::Entry, expanded: false, name: e.name.clone(), depth, is_dir: e.is_dir, is_symlink: e.is_symlink, locked: e.is_locked(), size: e.size, path })
    }

    fn walk(&self, cache: &ListingCache, site: &str, dir: &RemotePath, depth: usize, out: &mut Vec<RemoteRow>, missing: &mut Vec<RemotePath>) {
        match cache.get(site, dir) {
            None => {
                missing.push(dir.clone());
                out.push(RemoteRow::info(RemoteRowKind::Loading, dir, depth, "Loading...".into()));
            }
            Some(Listing::Loading) => out.push(RemoteRow::info(RemoteRowKind::Loading, dir, depth, "Loading...".into())),
            Some(l) => {
                if let Listing::Failed { error, .. } = l {
                    out.push(RemoteRow::info(RemoteRowKind::Error(error.plain()), dir, depth, error.plain()));
                }
                let Some(entries) = l.entries().map(|e| self.ordered(e)) else { return };
                if entries.is_empty() && matches!(l, Listing::Ready { .. }) {
                    out.push(RemoteRow::info(RemoteRowKind::Empty, dir, depth, "(empty)".into()));
                }
                for e in entries.iter().take(MAX_ROWS_PER_DIR) {
                    let Some(mut row) = self.entry_row(dir, e, depth) else { continue };
                    // A symlinked folder is shown but never expanded (loops, escapes).
                    row.expanded = e.is_dir && !e.is_symlink && self.expanded.contains(&row.path);
                    let (expanded, path) = (row.expanded, row.path.clone());
                    out.push(row);
                    if expanded {
                        self.walk(cache, site, &path, depth + 1, out, missing);
                    }
                }
                if entries.len() > MAX_ROWS_PER_DIR {
                    let n = entries.len() - MAX_ROWS_PER_DIR;
                    out.push(RemoteRow::info(RemoteRowKind::More(n), dir, depth, format!("{n} more...")));
                }
            }
        }
    }

    /// Returns true when something under `dir` matched.
    fn walk_filtered(&self, cache: &ListingCache, site: &str, dir: &RemotePath, depth: usize, needle: &str, out: &mut Vec<RemoteRow>) -> bool {
        let Some(entries) = cache.get(site, dir).and_then(|l| l.entries()).map(|e| self.ordered(e)) else { return false };
        let mut any = false;
        for e in entries.iter().take(MAX_ROWS_PER_DIR) {
            let Some(mut row) = self.entry_row(dir, e, depth) else { continue };
            let name_hit = e.name.to_lowercase().contains(needle);
            let mut sub = Vec::new();
            let sub_hit = e.is_dir && !e.is_symlink && self.walk_filtered(cache, site, &row.path, depth + 1, needle, &mut sub);
            if name_hit || sub_hit {
                row.expanded = sub_hit;
                out.push(row);
                out.extend(sub);
                any = true;
            }
        }
        any
    }

    /// Move the selection `delta` entry rows (clamped) within `rows`.
    pub fn move_selection(&mut self, rows: &[RemoteRow], delta: isize) -> Option<RemotePath> {
        let entries: Vec<&RemoteRow> = rows.iter().filter(|r| r.is_entry()).collect();
        if entries.is_empty() {
            return None;
        }
        let cur = self.selected.as_ref().and_then(|s| entries.iter().position(|r| &r.path == s));
        let next = match cur {
            None => 0,
            Some(i) => (i as isize + delta).clamp(0, entries.len() as isize - 1) as usize,
        };
        self.selected = Some(entries[next].path.clone());
        self.selected.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::remote::error::{RemoteError, RemoteErrorKind};
    use crate::remote::fake::FakeRemoteProvider;
    use crate::remote::hub::{ProviderFactory, RemoteHub};
    use crate::remote::provider::SharedProvider;
    use crate::remote::sites::SiteProfile;
    use std::sync::Arc;
    use std::time::{Duration, Instant};

    struct NoFactory;
    impl ProviderFactory for NoFactory {
        fn connect(&self, _: &SiteProfile) -> Result<SharedProvider, RemoteError> {
            Err(RemoteError::new(RemoteErrorKind::Unreachable, "none"))
        }
    }

    /// Settle the tree: keep requesting what is missing until nothing is.
    fn settle(hub: &RemoteHub, cache: &mut ListingCache, tree: &RemoteTree) -> Vec<RemoteRow> {
        let t = Instant::now();
        loop {
            let (rows, missing) = tree.rows(cache, "s");
            for d in &missing {
                cache.ensure(hub, "s", d);
            }
            let loading = rows.iter().any(|r| r.kind == RemoteRowKind::Loading) || !missing.is_empty();
            if !loading || t.elapsed() > Duration::from_secs(10) {
                return rows;
            }
            for ev in hub.drain() {
                cache.apply(&ev);
            }
            std::thread::sleep(Duration::from_millis(2));
        }
    }

    fn names(rows: &[RemoteRow]) -> Vec<String> {
        rows.iter().map(|r| format!("{}{}", " ".repeat(r.depth), r.name)).collect()
    }

    fn env() -> (RemoteHub, Arc<FakeRemoteProvider>, ListingCache) {
        let fake = FakeRemoteProvider::demo();
        let hub = RemoteHub::new(|| {}, Arc::new(NoFactory));
        hub.register_provider("s", fake.clone());
        (hub, fake, ListingCache::new())
    }

    #[test]
    fn rows_show_loading_first_then_the_listing_and_expand_lazily() {
        let (hub, fake, mut cache) = env();
        let mut tree = RemoteTree::new(RemotePath::new("/var/www"));
        let (rows, missing) = tree.rows(&cache, "s");
        assert_eq!(rows.len(), 1, "the root itself is not a row, only its loading placeholder");
        assert_eq!(rows[0].kind, RemoteRowKind::Loading);
        assert_eq!(missing, [RemotePath::new("/var/www")]);
        cache.ensure(&hub, "s", &missing[0]);
        let rows = settle(&hub, &mut cache, &tree);
        assert_eq!(names(&rows), ["css", "img", ".htaccess", "index.php", "robots.txt"]);
        let calls = fake.calls();
        // expanding a folder shows one Loading row at once, then its children
        tree.toggle(&RemotePath::new("/var/www/css"));
        let (rows, missing) = tree.rows(&cache, "s");
        assert!(rows.iter().any(|r| r.kind == RemoteRowKind::Loading && r.depth == 1));
        assert_eq!(missing, [RemotePath::new("/var/www/css")]);
        let rows = settle(&hub, &mut cache, &tree);
        assert_eq!(names(&rows)[..3], ["css", " style.css", "img"]);
        assert_eq!(fake.calls(), calls + 1, "only the expanded folder was listed");
    }

    #[test]
    fn a_failed_folder_shows_a_plain_error_row_in_place() {
        let (hub, fake, mut cache) = env();
        fake.fail_path("/var/www/css", std::io::ErrorKind::PermissionDenied);
        let mut tree = RemoteTree::new(RemotePath::new("/var/www"));
        tree.toggle(&RemotePath::new("/var/www/css"));
        let rows = settle(&hub, &mut cache, &tree);
        let err = rows.iter().find(|r| matches!(r.kind, RemoteRowKind::Error(_))).expect("error row");
        assert_eq!(err.depth, 1);
        assert_eq!(err.name, "You don't have permission to do that here.");
    }

    #[test]
    fn locked_symlink_and_empty_are_surfaced_and_symlinked_dirs_do_not_expand() {
        let (hub, fake, mut cache) = env();
        fake.put_symlink("/var/www/current");
        fake.put_dir("/var/www/emptydir");
        let mut tree = RemoteTree::new(RemotePath::new("/var/www"));
        tree.toggle(&RemotePath::new("/var/www/emptydir"));
        tree.toggle(&RemotePath::new("/var/www/current"));
        let rows = settle(&hub, &mut cache, &tree);
        assert!(rows.iter().find(|r| r.name == ".htaccess").unwrap().locked);
        assert!(rows.iter().find(|r| r.name == "current").unwrap().is_symlink);
        assert!(rows.iter().any(|r| r.kind == RemoteRowKind::Empty && r.depth == 1));
        assert!(!rows.iter().find(|r| r.name == "current").unwrap().expanded);
    }

    #[test]
    fn filter_is_client_side_and_keeps_the_ancestors_of_matches() {
        let (hub, fake, mut cache) = env();
        let mut tree = RemoteTree::new(RemotePath::new("/var/www"));
        tree.expand(&RemotePath::new("/var/www/css"));
        settle(&hub, &mut cache, &tree);
        let calls = fake.calls();
        tree.filter = "STYLE".into();
        let (rows, missing) = tree.rows(&cache, "s");
        assert!(missing.is_empty());
        assert_eq!(names(&rows), ["css", " style.css"]);
        tree.filter = "zzz".into();
        assert!(tree.rows(&cache, "s").0.is_empty());
        tree.filter.clear();
        assert!(!tree.rows(&cache, "s").0.is_empty());
        assert_eq!(fake.calls(), calls, "filtering never touches the server");
    }

    #[test]
    fn a_huge_folder_is_capped_with_a_more_row() {
        let (hub, fake, mut cache) = env();
        for i in 0..(MAX_ROWS_PER_DIR + 7) {
            fake.put_file(&format!("/var/log/f{i:05}.log"), b"");
        }
        let tree = RemoteTree::new(RemotePath::new("/var/log"));
        let rows = settle(&hub, &mut cache, &tree);
        assert_eq!(rows.len(), MAX_ROWS_PER_DIR + 1);
        assert!(matches!(rows.last().unwrap().kind, RemoteRowKind::More(n) if n == 8));
    }

    #[test]
    fn selection_moves_over_entries_only_and_clamps() {
        let (hub, _f, mut cache) = env();
        let mut tree = RemoteTree::new(RemotePath::new("/var/www"));
        tree.expand(&RemotePath::new("/var/www/css"));
        let rows = settle(&hub, &mut cache, &tree);
        assert_eq!(tree.move_selection(&rows, 1).unwrap().as_str(), "/var/www/css");
        assert_eq!(tree.move_selection(&rows, 1).unwrap().as_str(), "/var/www/css/style.css");
        assert_eq!(tree.move_selection(&rows, 99).unwrap().as_str(), "/var/www/robots.txt");
        assert_eq!(tree.move_selection(&rows, -99).unwrap().as_str(), "/var/www/css");
    }
}
