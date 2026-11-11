//! `Explorer`: the whole explorer model behind the left pane, with no UI in it.
//!
//! One `Explorer` lives on the UI thread. It owns the [`RemoteHub`], the [`ListingCache`] and one
//! [`OpenSite`] per connected site. The view calls `rows()` (cache only) and the action methods;
//! after the hub woke the UI, the view calls `pump()` and shows the returned notices.
//! Nothing in here waits on the network.

use std::collections::HashMap;

use super::cache::ListingCache;
use super::error::{RemoteError, RemoteErrorKind};
use super::hub::{JobId, OpKind, RemoteEvent, RemoteHub, RemoteOp, RemoteReply};
use super::path::{valid_entry_name, RemotePath};
use super::local::{LocalProvider, LOCAL_SITE_ID};
use super::sites::{AuthMethod, Protocol, SecretRef, SiteId, SiteProfile};
use crate::browser::{NavHistory, SortMode};
use std::sync::Arc;
use super::state::{ConnState, RemoteOrigin};
use super::tree::{RemoteRow, RemoteTree};

pub struct OpenSite {
    pub profile: SiteProfile,
    pub conn: ConnState,
    pub tree: RemoteTree,
    /// Back / forward of this source only.
    pub history: NavHistory,
}

/// Something the view should tell the user, or act on.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ExplorerNotice {
    Connected { site: SiteId },
    ConnectFailed { site: SiteId, error: RemoteError },
    /// A file asked for with `open_file` arrived (bytes as the server sent them).
    FileLoaded { origin: RemoteOrigin, bytes: Vec<u8> },
    /// `open_file` failed.
    OpenFailed { origin: RemoteOrigin, error: RemoteError },
    /// A `save` job finished. The save ENGINE (SafeSaver) will replace this naive path.
    Written { job: JobId, origin: RemoteOrigin, result: Result<(), RemoteError> },
    /// A create / rename / delete finished.
    Changed { site: SiteId, what: String },
    ChangeFailed { site: SiteId, what: String, error: RemoteError },
    /// The connection to a site was lost (a request failed as unreachable / dropped).
    WentOffline { site: SiteId, error: RemoteError },
}

pub struct Explorer {
    pub hub: RemoteHub,
    pub cache: ListingCache,
    pub sites: Vec<OpenSite>,
    pub active: Option<usize>,
    /// What an in-flight job of ours is for (so the answer can be reported with its origin).
    reads: HashMap<JobId, RemoteOrigin>,
}

impl Explorer {
    pub fn new(hub: RemoteHub) -> Self {
        Self { hub, cache: ListingCache::new(), sites: Vec::new(), active: None, reads: HashMap::new() }
    }

    pub fn site_index(&self, id: &str) -> Option<usize> {
        self.sites.iter().position(|s| s.profile.id == id)
    }

    pub fn active_site(&self) -> Option<&OpenSite> {
        self.active.and_then(|i| self.sites.get(i))
    }

    pub fn active_site_mut(&mut self) -> Option<&mut OpenSite> {
        self.active.and_then(|i| self.sites.get_mut(i))
    }

    /// Start (or switch to) a connection. The tree appears at once in the "Connecting" state.
    pub fn connect(&mut self, profile: SiteProfile) {
        if let Some(i) = self.site_index(&profile.id) {
            self.active = Some(i);
            if matches!(self.sites[i].conn, ConnState::Connected | ConnState::Connecting) {
                return;
            }
            self.sites[i].profile = profile.clone();
            self.sites[i].conn = ConnState::Connecting;
        } else {
            let tree = RemoteTree::new(RemotePath::new(&profile.root_path));
            self.sites.push(OpenSite { profile: profile.clone(), conn: ConnState::Connecting, tree, history: NavHistory::default() });
            self.active = Some(self.sites.len() - 1);
        }
        let id = profile.id.clone();
        self.hub.submit(&id, RemoteOp::Connect(profile));
    }

    /// The local disk as a source: connected at once (no handshake), listed on the hub's worker like any
    /// site. Nothing runs until its first folder is shown. Idempotent; returns its index.
    pub fn add_local(&mut self, start: RemotePath) -> usize {
        if let Some(i) = self.site_index(LOCAL_SITE_ID) {
            return i;
        }
        let profile = SiteProfile {
            id: LOCAL_SITE_ID.into(),
            name: "Local".into(),
            protocol: Protocol::Sftp,
            host: String::new(),
            port: 0,
            user: String::new(),
            auth: AuthMethod::Agent,
            root_path: start.to_string(),
            production: false,
            read_only: false,
            secret_ref: SecretRef::default(),
            keep_asked: false,
        };
        self.hub.register_provider(LOCAL_SITE_ID, Arc::new(LocalProvider));
        self.sites.push(OpenSite { profile, conn: ConnState::Connected, tree: RemoteTree::new(start), history: NavHistory::default() });
        self.sites.len() - 1
    }

    pub fn is_local(&self, site: &str) -> bool {
        site == LOCAL_SITE_ID
    }

    /// Folder the active source is showing (the tree's root).
    pub fn current_root(&self) -> Option<RemotePath> {
        self.active_site().map(|s| s.tree.root.clone())
    }

    /// Show `path` as the root of the active source, remembering where we were.
    pub fn navigate(&mut self, path: RemotePath) {
        let Some(s) = self.active_site_mut() else { return };
        if s.tree.root == path {
            return;
        }
        let old = s.tree.root.clone();
        s.history.visit(&old);
        s.tree.go_to(path);
    }

    pub fn back(&mut self) -> bool {
        let Some(s) = self.active_site_mut() else { return false };
        let cur = s.tree.root.clone();
        match s.history.back(&cur) {
            Some(p) => {
                s.tree.go_to(p);
                true
            }
            None => false,
        }
    }

    pub fn forward(&mut self) -> bool {
        let Some(s) = self.active_site_mut() else { return false };
        let cur = s.tree.root.clone();
        match s.history.forward(&cur) {
            Some(p) => {
                s.tree.go_to(p);
                true
            }
            None => false,
        }
    }

    /// The parent of the shown folder becomes the shown folder; the folder we left stays selected.
    pub fn up(&mut self) -> bool {
        let left = self.current_root();
        let Some(parent) = left.as_ref().and_then(|r| r.parent()) else { return false };
        self.navigate(parent);
        if let (Some(s), Some(l)) = (self.active_site_mut(), left) {
            s.tree.selected = Some(l);
        }
        true
    }

    pub fn can_back(&self) -> bool {
        self.active_site().is_some_and(|s| s.history.can_back())
    }

    pub fn can_forward(&self) -> bool {
        self.active_site().is_some_and(|s| s.history.can_forward())
    }

    /// Show `file` of `site` in the browser: switch to the source, show its folder, select it.
    /// Returns false when the site is not open.
    pub fn reveal(&mut self, site: &str, file: &RemotePath) -> bool {
        let Some(i) = self.site_index(site) else { return false };
        self.active = Some(i);
        if let Some(parent) = file.parent() {
            let shown = self.sites[i].tree.root.clone();
            if file.starts_with(&shown) && file != &shown {
                // Already under the shown folder: keep the view, expand down to the file.
                let mut cur = RemotePath::root();
                for seg in file.as_str().trim_start_matches('/').split('/') {
                    let Some(next) = cur.join(seg) else { break };
                    if &next != file && next.starts_with(&shown) {
                        self.sites[i].tree.expand(&next);
                    }
                    cur = next;
                }
            } else {
                self.navigate(parent);
            }
        }
        self.sites[i].tree.selected = Some(file.clone());
        true
    }

    pub fn set_sort(&mut self, mode: SortMode) {
        for s in &mut self.sites {
            s.tree.sort = mode;
        }
    }

    /// Reconnect a site that went offline / failed.
    pub fn reconnect(&mut self, id: &str) {
        let Some(i) = self.site_index(id) else { return };
        if matches!(self.sites[i].conn, ConnState::Connected | ConnState::Connecting) {
            return;
        }
        let profile = self.sites[i].profile.clone();
        self.sites[i].conn = ConnState::Connecting;
        self.cache.drop_site(id);
        self.hub.submit(id, RemoteOp::Connect(profile));
    }

    /// Close a site tab: pending work is cancelled, cached rows dropped. Open documents are the
    /// caller's business (they stay open and become "detached").
    pub fn close_site(&mut self, id: &str) {
        let Some(i) = self.site_index(id) else { return };
        self.hub.disconnect(id);
        self.cache.drop_site(id);
        self.sites.remove(i);
        let n = self.sites.len();
        self.active = match self.active {
            _ if n == 0 => None,
            Some(a) if a > i => Some(a - 1),
            Some(a) if a == i => Some(a.min(n - 1)),
            other => other,
        };
    }

    /// Select the local tab (`None`) or a site tab.
    pub fn activate(&mut self, id: Option<&str>) {
        self.active = id.and_then(|id| self.site_index(id));
    }

    /// Rows of the active site, from the cache. Folders that are shown but not cached are requested
    /// (once each) as a side effect, which is why this takes `&mut self`.
    pub fn rows(&mut self) -> Vec<RemoteRow> {
        let Some(i) = self.active else { return Vec::new() };
        if self.sites[i].conn != ConnState::Connected {
            return Vec::new();
        }
        let id = self.sites[i].profile.id.clone();
        let (rows, missing) = self.sites[i].tree.rows(&self.cache, &id);
        for d in missing {
            self.cache.ensure(&self.hub, &id, &d);
        }
        rows
    }

    pub fn toggle(&mut self, path: &RemotePath) {
        if let Some(s) = self.active_site_mut() {
            s.tree.toggle(path);
        }
    }

    pub fn select(&mut self, path: &RemotePath) {
        if let Some(s) = self.active_site_mut() {
            s.tree.selected = Some(path.clone());
        }
    }

    pub fn set_filter(&mut self, f: &str) {
        if let Some(s) = self.active_site_mut() {
            s.tree.filter = f.to_string();
        }
    }

    pub fn refresh_dir(&mut self, dir: &RemotePath) {
        if let Some(i) = self.active {
            let id = self.sites[i].profile.id.clone();
            self.cache.refresh(&self.hub, &id, dir);
        }
    }

    /// F5: re-list the selected folder (or the selected file's folder, or the root).
    pub fn refresh_selected(&mut self, rows: &[RemoteRow]) {
        let Some(s) = self.active_site() else { return };
        let dir = match &s.tree.selected {
            Some(p) => match rows.iter().find(|r| &r.path == p) {
                Some(r) if r.is_dir => p.clone(),
                _ => p.parent().unwrap_or_else(|| s.tree.root.clone()),
            },
            None => s.tree.root.clone(),
        };
        self.refresh_dir(&dir);
    }

    pub fn origin(&self, site: &str, path: &RemotePath) -> Option<RemoteOrigin> {
        let s = &self.sites[self.site_index(site)?];
        Some(RemoteOrigin { site: s.profile.id.clone(), site_label: s.profile.name.clone(), path: path.to_string() })
    }

    /// Ask for a file's content; the answer comes back from `pump` as `FileLoaded` / `OpenFailed`.
    pub fn open_file(&mut self, path: &RemotePath) -> Option<JobId> {
        let i = self.active?;
        let id = self.sites[i].profile.id.clone();
        let origin = self.origin(&id, path)?;
        let job = self.hub.submit(&id, RemoteOp::Read(path.clone()));
        self.reads.insert(job, origin);
        Some(job)
    }

    fn writable(&self) -> Result<(String, usize), String> {
        let i = self.active.ok_or("No server is selected.")?;
        let s = &self.sites[i];
        if s.conn != ConnState::Connected {
            return Err("This server is not connected.".into());
        }
        if s.profile.read_only {
            return Err("This site is read-only.".into());
        }
        Ok((s.profile.id.clone(), i))
    }

    fn checked_child(dir: &RemotePath, name: &str) -> Result<RemotePath, String> {
        let name = name.trim();
        if !valid_entry_name(name) {
            return Err("A name cannot be empty or contain / or \\.".into());
        }
        dir.join(name).ok_or_else(|| "That name is not allowed.".to_string())
    }

    pub fn create_file(&mut self, dir: &RemotePath, name: &str) -> Result<JobId, String> {
        let (id, _) = self.writable()?;
        let p = Self::checked_child(dir, name)?;
        Ok(self.hub.submit(&id, RemoteOp::CreateFile(p)))
    }

    pub fn create_folder(&mut self, dir: &RemotePath, name: &str) -> Result<JobId, String> {
        let (id, _) = self.writable()?;
        let p = Self::checked_child(dir, name)?;
        Ok(self.hub.submit(&id, RemoteOp::Mkdir(p)))
    }

    pub fn rename(&mut self, path: &RemotePath, new_name: &str) -> Result<JobId, String> {
        let (id, _) = self.writable()?;
        let parent = path.parent().ok_or("The root cannot be renamed.")?;
        let to = Self::checked_child(&parent, new_name)?;
        if &to == path {
            return Err("That is already its name.".into());
        }
        Ok(self.hub.submit(&id, RemoteOp::Rename { from: path.clone(), to }))
    }

    pub fn delete(&mut self, path: &RemotePath, is_dir: bool) -> Result<JobId, String> {
        let (id, i) = self.writable()?;
        if path == &self.sites[i].tree.root || path.is_root() {
            return Err("The start folder cannot be deleted from here.".into());
        }
        Ok(self.hub.submit(&id, RemoteOp::Remove { path: path.clone(), is_dir }))
    }

    /// Plain upload of `data` to `path` (placeholder for the SafeSaver pipeline: no temp file, no
    /// verification, no conflict check). Honours the read-only site flag.
    pub fn save(&mut self, site: &str, path: &RemotePath, data: Vec<u8>) -> Result<JobId, RemoteError> {
        let i = self.site_index(site).ok_or_else(|| RemoteError::new(RemoteErrorKind::Unreachable, "site is not open"))?;
        if self.sites[i].profile.read_only {
            return Err(RemoteError::new(RemoteErrorKind::PermissionDenied, "site is read-only"));
        }
        let origin = self.origin(site, path).expect("site exists");
        let job = self.hub.submit(site, RemoteOp::Write { path: path.clone(), data });
        self.reads.insert(job, origin);
        Ok(job)
    }

    /// UI thread, after the hub woke it: apply finished jobs, return what the user should see.
    pub fn pump(&mut self) -> Vec<ExplorerNotice> {
        let mut notices = Vec::new();
        for ev in self.hub.drain() {
            self.apply(ev, &mut notices);
        }
        notices
    }

    fn apply(&mut self, ev: RemoteEvent, out: &mut Vec<ExplorerNotice>) {
        let Some(si) = self.site_index(&ev.site) else {
            self.reads.remove(&ev.job);
            return; // the site tab was closed meanwhile
        };
        let cancelled = matches!(&ev.result, Err(e) if e.kind == RemoteErrorKind::Cancelled);
        // A request that failed because the link is gone marks the site offline (once).
        if let Err(e) = &ev.result {
            if matches!(e.kind, RemoteErrorKind::Unreachable | RemoteErrorKind::Dropped) && ev.kind != OpKind::Connect && self.sites[si].conn == ConnState::Connected {
                self.sites[si].conn = ConnState::Failed(e.clone());
                self.hub.disconnect(&ev.site);
                out.push(ExplorerNotice::WentOffline { site: ev.site.clone(), error: e.clone() });
            }
        }
        match ev.kind {
            OpKind::Connect => match ev.result {
                Ok(_) => {
                    self.sites[si].conn = ConnState::Connected;
                    out.push(ExplorerNotice::Connected { site: ev.site });
                }
                Err(_) if cancelled => {}
                Err(e) => {
                    self.sites[si].conn = ConnState::Failed(e.clone());
                    out.push(ExplorerNotice::ConnectFailed { site: ev.site, error: e });
                }
            },
            OpKind::List => {
                self.cache.apply(&ev);
            }
            OpKind::Read => {
                if let Some(origin) = self.reads.remove(&ev.job) {
                    match ev.result {
                        Ok(RemoteReply::Read(bytes)) => out.push(ExplorerNotice::FileLoaded { origin, bytes }),
                        Err(error) if !cancelled => out.push(ExplorerNotice::OpenFailed { origin, error }),
                        _ => {}
                    }
                }
            }
            OpKind::Write => {
                if let Some(origin) = self.reads.remove(&ev.job) {
                    let result = ev.result.map(|_| ());
                    if let (Ok(()), Some(dir)) = (&result, ev.path.parent()) {
                        self.cache.invalidate(&ev.site, &dir);
                    }
                    out.push(ExplorerNotice::Written { job: ev.job, origin, result });
                }
            }
            OpKind::CreateFile | OpKind::Mkdir | OpKind::Rename | OpKind::Remove => {
                let what = match ev.kind {
                    OpKind::CreateFile => format!("Created {}", ev.path.file_name().unwrap_or("")),
                    OpKind::Mkdir => format!("Created folder {}", ev.path.file_name().unwrap_or("")),
                    OpKind::Rename => format!("Renamed {}", ev.path.file_name().unwrap_or("")),
                    _ => format!("Deleted {}", ev.path.file_name().unwrap_or("")),
                };
                match ev.result {
                    Ok(_) => {
                        // What changed is no longer what we cached: re-list the folder(s) involved.
                        if matches!(ev.kind, OpKind::Rename | OpKind::Remove) {
                            self.cache.invalidate_tree(&ev.site, &ev.path);
                        }
                        let mut parents = vec![ev.path.parent()];
                        parents.push(ev.to.as_ref().and_then(|t| t.parent()));
                        parents.sort();
                        parents.dedup();
                        for p in parents.into_iter().flatten() {
                            if self.cache.get(&ev.site, &p).is_some() {
                                self.cache.refresh(&self.hub, &ev.site, &p);
                            }
                        }
                        if let Some(to) = &ev.to {
                            self.sites[si].tree.selected = Some(to.clone());
                        }
                        out.push(ExplorerNotice::Changed { site: ev.site, what });
                    }
                    Err(error) if !cancelled => out.push(ExplorerNotice::ChangeFailed { site: ev.site, what, error }),
                    Err(_) => {}
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::remote::tree::RemoteRowKind;
    use crate::remote::fake::FakeRemoteProvider;
    use crate::remote::hub::ProviderFactory;
    use crate::remote::provider::SharedProvider;
    use crate::remote::sites::AuthMethod;
    use std::sync::Arc;
    use std::time::{Duration, Instant};

    struct Factory(Arc<FakeRemoteProvider>);
    impl ProviderFactory for Factory {
        fn connect(&self, p: &SiteProfile) -> Result<SharedProvider, RemoteError> {
            if p.host == "down" {
                return Err(RemoteError::new(RemoteErrorKind::Unreachable, "refused"));
            }
            Ok(self.0.clone())
        }
    }

    fn profile(user_host: &str, root: &str) -> SiteProfile {
        SiteProfile::from_quick(user_host, AuthMethod::Agent, root).unwrap()
    }

    fn explorer() -> (Explorer, Arc<FakeRemoteProvider>) {
        let fake = FakeRemoteProvider::demo();
        (Explorer::new(RemoteHub::new(|| {}, Arc::new(Factory(fake.clone())))), fake)
    }

    fn run_until(ex: &mut Explorer, notices: &mut Vec<ExplorerNotice>, done: impl Fn(&mut Explorer, &[ExplorerNotice]) -> bool) {
        let t = Instant::now();
        loop {
            notices.extend(ex.pump());
            ex.rows();
            if done(ex, notices) {
                return;
            }
            assert!(t.elapsed() < Duration::from_secs(10), "timed out; notices: {notices:?}");
            std::thread::sleep(Duration::from_millis(2));
        }
    }

    fn connected(ex: &mut Explorer, host: &str) -> Vec<ExplorerNotice> {
        ex.connect(profile(host, "/var/www"));
        let mut n = Vec::new();
        run_until(ex, &mut n, |ex, _| ex.rows().iter().any(|r| r.name == "index.php"));
        n
    }

    #[test]
    fn nothing_runs_until_a_site_is_used_and_connect_lists_the_start_folder() {
        let (mut ex, _f) = explorer();
        assert_eq!(ex.hub.worker_count(), 0);
        assert!(ex.rows().is_empty());
        let n = connected(&mut ex, "me@box");
        assert!(n.contains(&ExplorerNotice::Connected { site: "me-box".into() }));
        assert!(ex.hub.worker_count() >= 1);
        assert_eq!(ex.active_site().unwrap().conn, ConnState::Connected);
    }

    #[test]
    fn a_failed_connect_is_reported_in_plain_language_and_can_be_retried() {
        let (mut ex, _f) = explorer();
        ex.connect(profile("me@down", "/"));
        let mut n = Vec::new();
        run_until(&mut ex, &mut n, |_, n| !n.is_empty());
        let ExplorerNotice::ConnectFailed { error, .. } = &n[0] else { panic!("{n:?}") };
        assert!(error.plain().starts_with("Can't reach the server"));
        assert!(matches!(ex.active_site().unwrap().conn, ConnState::Failed(_)));
        assert!(ex.rows().is_empty());
        ex.reconnect("me-down");
        assert_eq!(ex.active_site().unwrap().conn, ConnState::Connecting);
    }

    #[test]
    fn open_save_external_change_save_again_is_a_conflict_through_the_explorer_open_path() {
        use crate::remote::saving::{SaveEvent, SaveService, Submit};
        use brouillon_safesave::SaveOutcome;
        let dir = std::env::temp_dir().join(format!("brouillon-ex-flow-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let trig = dir.join("trigger");
        std::fs::create_dir_all(&trig).unwrap();
        let (mut ex, fake) = explorer();
        fake.enable_safe(Some(trig.clone()));
        fake.set_latency(Duration::from_millis(5));
        connected(&mut ex, "me@box");
        let svc = SaveService::new(dir.join("journal"), dir.join("backups"));
        let shared: SharedProvider = fake.clone();
        let path = RemotePath::new("/var/www/index.php");
        ex.open_file(&path).unwrap();
        let mut n = Vec::new();
        run_until(&mut ex, &mut n, |_, n| n.iter().any(|x| matches!(x, ExplorerNotice::FileLoaded { .. })));
        let Some(ExplorerNotice::FileLoaded { origin, bytes }) = n.into_iter().find(|x| matches!(x, ExplorerNotice::FileLoaded { .. })) else { panic!() };
        // Exactly what RemoteUi does on FileLoaded.
        let key = (origin.site.clone(), origin.path.clone());
        let stamp = shared.take_open_stamp(&path).expect("stamp");
        assert!(shared.safe_fs().is_some());
        svc.attach(key.clone(), &bytes, stamp);
        let wait = || {
            let t = Instant::now();
            loop {
                if let Some(e) = svc.drain().into_iter().next() {
                    return e;
                }
                assert!(t.elapsed() < Duration::from_secs(10));
                std::thread::sleep(Duration::from_millis(5));
            }
        };
        let mut first = b"A-".to_vec();
        first.extend_from_slice(&bytes);
        assert_eq!(svc.save(&key, first.clone(), shared.clone()), Submit::Started);
        match wait() {
            SaveEvent::Outcome { outcome, .. } => assert!(matches!(outcome, SaveOutcome::Saved { .. }), "{outcome:?}"),
            o => panic!("{o:?}"),
        }
        // The explorer refreshes the folder after a save, like the UI does.
        ex.cache.invalidate(&origin.site, &RemotePath::new("/var/www"));
        let _ = ex.rows();
        std::thread::sleep(Duration::from_millis(100));
        let _ = ex.pump();
        std::fs::write(trig.join("conflict-once.txt"), "/var/www/index.php
CHANGED BY SOMEONE ELSE
").unwrap();
        let mut second = b"B-".to_vec();
        second.extend_from_slice(&first);
        assert_eq!(svc.save(&key, second, shared), Submit::Started);
        match wait() {
            SaveEvent::Outcome { outcome, .. } => assert!(matches!(outcome, SaveOutcome::Conflict { .. }), "must be a conflict: {outcome:?}"),
            o => panic!("{o:?}"),
        }
        assert_eq!(fake.file_text("/var/www/index.php").unwrap(), "CHANGED BY SOMEONE ELSE
");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn open_file_returns_the_bytes_with_their_origin() {
        let (mut ex, _f) = explorer();
        connected(&mut ex, "me@box");
        ex.open_file(&RemotePath::new("/var/www/index.php")).unwrap();
        let mut n = Vec::new();
        run_until(&mut ex, &mut n, |_, n| !n.is_empty());
        let ExplorerNotice::FileLoaded { origin, bytes } = &n[0] else { panic!("{n:?}") };
        assert_eq!((origin.site.as_str(), origin.path.as_str(), origin.chip().as_str()), ("me-box", "/var/www/index.php", "~box"));
        assert!(bytes.starts_with(b"<?php"));
    }

    #[test]
    fn create_rename_delete_update_the_listing_without_a_manual_refresh() {
        let (mut ex, fake) = explorer();
        connected(&mut ex, "me@box");
        let dir = RemotePath::new("/var/www");
        ex.create_file(&dir, "new.txt").unwrap();
        let mut n = Vec::new();
        run_until(&mut ex, &mut n, |ex, _| ex.rows().iter().any(|r| r.name == "new.txt"));
        assert!(fake.exists("/var/www/new.txt"));
        ex.rename(&RemotePath::new("/var/www/new.txt"), "renamed.txt").unwrap();
        run_until(&mut ex, &mut n, |ex, _| { let r = ex.rows(); r.iter().any(|r| r.name == "renamed.txt") && !r.iter().any(|r| r.name == "new.txt") });
        assert_eq!(ex.active_site().unwrap().tree.selected, Some(RemotePath::new("/var/www/renamed.txt")));
        ex.delete(&RemotePath::new("/var/www/renamed.txt"), false).unwrap();
        run_until(&mut ex, &mut n, |ex, _| !ex.rows().iter().any(|r| r.name == "renamed.txt"));
        assert!(!fake.exists("/var/www/renamed.txt"));
    }

    #[test]
    fn bad_names_and_read_only_sites_are_refused_before_any_request() {
        let (mut ex, fake) = explorer();
        connected(&mut ex, "me@box");
        let calls = fake.calls();
        let dir = RemotePath::new("/var/www");
        for bad in ["", "  ", "a/b", "..", "x\\y"] {
            assert!(ex.create_file(&dir, bad).is_err(), "{bad:?}");
        }
        assert!(ex.delete(&RemotePath::new("/var/www"), true).is_err(), "start folder is protected");
        assert!(ex.rename(&RemotePath::new("/var/www/robots.txt"), "robots.txt").is_err());
        ex.active_site_mut().unwrap().profile.read_only = true;
        assert_eq!(ex.create_file(&dir, "x").unwrap_err(), "This site is read-only.");
        assert!(ex.save("me-box", &RemotePath::new("/var/www/robots.txt"), vec![]).is_err());
        assert_eq!(fake.calls(), calls);
    }

    #[test]
    fn a_failed_change_names_what_failed_and_leaves_the_tree_alone() {
        let (mut ex, fake) = explorer();
        connected(&mut ex, "me@box");
        fake.fail_path("/var/www/robots.txt", std::io::ErrorKind::PermissionDenied);
        ex.delete(&RemotePath::new("/var/www/robots.txt"), false).unwrap();
        let mut n = Vec::new();
        run_until(&mut ex, &mut n, |_, n| !n.is_empty());
        let ExplorerNotice::ChangeFailed { what, error, .. } = &n[0] else { panic!("{n:?}") };
        assert_eq!(what, "Deleted robots.txt");
        assert_eq!(error.kind, RemoteErrorKind::PermissionDenied);
        assert!(ex.rows().iter().any(|r| r.name == "robots.txt"));
    }

    #[test]
    fn a_dropped_link_marks_the_site_offline_once_and_keeps_the_cached_rows_out_of_the_way() {
        let (mut ex, fake) = explorer();
        connected(&mut ex, "me@box");
        fake.fail_next(2, std::io::ErrorKind::ConnectionReset);
        ex.refresh_dir(&RemotePath::new("/var/www"));
        ex.refresh_dir(&RemotePath::new("/var/www"));
        let mut n = Vec::new();
        run_until(&mut ex, &mut n, |ex, _| matches!(ex.active_site().unwrap().conn, ConnState::Failed(_)));
        std::thread::sleep(Duration::from_millis(50));
        n.extend(ex.pump());
        assert_eq!(n.iter().filter(|x| matches!(x, ExplorerNotice::WentOffline { .. })).count(), 1);
        ex.reconnect("me-box");
        run_until(&mut ex, &mut n, |ex, _| ex.active_site().unwrap().conn == ConnState::Connected && ex.rows().iter().any(|r| r.name == "index.php"));
    }

    #[test]
    fn closing_a_site_cancels_its_work_and_forgets_its_rows() {
        let (mut ex, fake) = explorer();
        connected(&mut ex, "me@box");
        fake.set_latency(Duration::from_millis(60));
        ex.refresh_dir(&RemotePath::new("/var/www"));
        ex.close_site("me-box");
        assert!(ex.sites.is_empty() && ex.active.is_none() && ex.cache.is_empty());
        assert!(ex.rows().is_empty());
        std::thread::sleep(Duration::from_millis(150));
        assert!(ex.pump().is_empty(), "late answers of a closed site are dropped");
    }

    #[test]
    fn two_sites_have_independent_trees_and_switching_is_a_signal_swap() {
        let (mut ex, _f) = explorer();
        connected(&mut ex, "me@box");
        ex.connect(profile("me@other", "/etc"));
        let mut n = Vec::new();
        run_until(&mut ex, &mut n, |ex, _| ex.rows().iter().any(|r| r.name == "hostname"));
        assert_eq!(ex.sites.len(), 2);
        ex.activate(Some("me-box"));
        assert!(ex.rows().iter().any(|r| r.name == "index.php"));
        ex.activate(None);
        assert!(ex.active.is_none() && ex.rows().is_empty());
    }

    #[test]
    fn save_is_a_plain_write_and_reports_with_the_origin() {
        let (mut ex, fake) = explorer();
        connected(&mut ex, "me@box");
        let job = ex.save("me-box", &RemotePath::new("/var/www/robots.txt"), b"changed".to_vec()).unwrap();
        let mut n = Vec::new();
        run_until(&mut ex, &mut n, |_, n| !n.is_empty());
        let ExplorerNotice::Written { job: j, origin, result } = &n[0] else { panic!("{n:?}") };
        assert_eq!((*j, origin.path.as_str(), result.is_ok()), (job, "/var/www/robots.txt", true));
        assert_eq!(fake.file_text("/var/www/robots.txt").unwrap(), "changed");
    }

    // ------------------------------------------------------------ the local source and navigation

    fn local_explorer(dir: &std::path::Path) -> Explorer {
        let (mut ex, _f) = explorer();
        for n in ["b.txt", "a.md"] {
            std::fs::write(dir.join(n), "x").unwrap();
        }
        std::fs::create_dir_all(dir.join("sub").join("deep")).unwrap();
        std::fs::write(dir.join("sub").join("inner.rs"), "y").unwrap();
        ex.add_local(crate::remote::local::from_native(dir));
        ex.activate(Some(LOCAL_SITE_ID));
        ex
    }

    #[test]
    fn the_local_source_lists_off_the_ui_thread_and_starts_nothing_before_it_is_shown() {
        let d = tempfile::tempdir().unwrap();
        let mut ex = local_explorer(d.path());
        assert_eq!(ex.hub.worker_count(), 0, "adding the source starts no thread");
        let first = ex.rows();
        assert!(first.iter().any(|r| r.kind == RemoteRowKind::Loading), "the first frame is a loading row, not a blocking read");
        let mut n = Vec::new();
        run_until(&mut ex, &mut n, |ex, _| ex.rows().iter().any(|r| r.name == "b.txt"));
        let names: Vec<String> = ex.rows().iter().filter(|r| r.is_entry()).map(|r| r.name.clone()).collect();
        assert_eq!(names, ["sub", "a.md", "b.txt"]);
        assert!(ex.hub.worker_count() >= 1);
    }

    #[test]
    fn navigating_keeps_a_history_per_source_and_up_selects_the_folder_we_left() {
        let d = tempfile::tempdir().unwrap();
        let mut ex = local_explorer(d.path());
        let root = ex.current_root().unwrap();
        ex.navigate(root.join("sub").unwrap());
        assert_eq!(ex.current_root().unwrap(), root.join("sub").unwrap());
        assert!(ex.can_back() && !ex.can_forward());
        let mut n = Vec::new();
        run_until(&mut ex, &mut n, |ex, _| ex.rows().iter().any(|r| r.name == "inner.rs"));
        assert!(ex.up());
        assert_eq!(ex.current_root().unwrap(), root);
        assert_eq!(ex.active_site().unwrap().tree.selected, Some(root.join("sub").unwrap()));
        assert!(ex.back());
        assert_eq!(ex.current_root().unwrap(), root.join("sub").unwrap());
        assert!(ex.forward());
        assert_eq!(ex.current_root().unwrap(), root);
        assert!(!ex.forward());
        // a server source has its own, untouched history
        connected(&mut ex, "me@box");
        assert!(!ex.can_back());
        ex.activate(Some("me-box"));
        ex.navigate(RemotePath::root());
        assert!(!ex.up(), "nothing above the root");
    }

    #[test]
    fn reveal_expands_in_place_or_moves_to_the_folder_and_selects_the_file() {
        let d = tempfile::tempdir().unwrap();
        let mut ex = local_explorer(d.path());
        let root = ex.current_root().unwrap();
        let inner = root.join("sub").unwrap().join("inner.rs").unwrap();
        assert!(ex.reveal(LOCAL_SITE_ID, &inner));
        assert_eq!(ex.current_root().unwrap(), root, "already under the shown folder: the view does not move");
        assert!(ex.active_site().unwrap().tree.is_expanded(&root.join("sub").unwrap()));
        assert_eq!(ex.active_site().unwrap().tree.selected, Some(inner));
        let outside = RemotePath::new("/elsewhere/file.txt");
        assert!(ex.reveal(LOCAL_SITE_ID, &outside));
        assert_eq!(ex.current_root().unwrap(), RemotePath::new("/elsewhere"));
        assert!(!ex.reveal("nope", &outside));
    }

    #[test]
    fn the_sort_mode_reorders_every_tree_and_keeps_folders_first() {
        let d = tempfile::tempdir().unwrap();
        let mut ex = local_explorer(d.path());
        std::fs::write(d.path().join("big.bin"), vec![0u8; 5000]).unwrap();
        let mut n = Vec::new();
        run_until(&mut ex, &mut n, |ex, _| ex.rows().iter().any(|r| r.name == "big.bin"));
        ex.set_sort(SortMode { key: crate::browser::SortKey::Size, descending: true });
        let names: Vec<String> = ex.rows().iter().filter(|r| r.is_entry()).map(|r| r.name.clone()).collect();
        assert_eq!(names, ["sub", "big.bin", "b.txt", "a.md"]);
    }

    #[test]
    fn local_create_rename_delete_go_through_the_same_path_as_a_server() {
        let d = tempfile::tempdir().unwrap();
        let mut ex = local_explorer(d.path());
        let root = ex.current_root().unwrap();
        let mut n = Vec::new();
        run_until(&mut ex, &mut n, |ex, _| ex.rows().iter().any(|r| r.name == "b.txt"));
        ex.create_file(&root, "new.txt").unwrap();
        run_until(&mut ex, &mut n, |ex, _| ex.rows().iter().any(|r| r.name == "new.txt"));
        assert!(d.path().join("new.txt").exists());
        ex.rename(&root.join("new.txt").unwrap(), "n2.txt").unwrap();
        run_until(&mut ex, &mut n, |ex, _| ex.rows().iter().any(|r| r.name == "n2.txt"));
        assert!(!d.path().join("new.txt").exists());
        ex.delete(&root.join("n2.txt").unwrap(), false).unwrap();
        run_until(&mut ex, &mut n, |ex, _| !ex.rows().iter().any(|r| r.name == "n2.txt"));
        assert!(!d.path().join("n2.txt").exists());
        // the start folder itself is never deleted from here
        assert!(ex.delete(&root, true).is_err());
    }

    #[test]
    fn a_huge_local_folder_is_capped_with_a_more_row() {
        let d = tempfile::tempdir().unwrap();
        let mut ex = local_explorer(d.path());
        let extra = crate::remote::tree::MAX_ROWS_PER_DIR + 7;
        for i in 0..extra {
            std::fs::write(d.path().join(format!("f{i:05}.dat")), "").unwrap();
        }
        let mut n = Vec::new();
        run_until(&mut ex, &mut n, |ex, _| ex.rows().iter().any(|r| matches!(r.kind, RemoteRowKind::More(_))));
        let rows = ex.rows();
        assert!(rows.len() <= crate::remote::tree::MAX_ROWS_PER_DIR + 2);
        let more = rows.iter().find_map(|r| if let RemoteRowKind::More(n) = r.kind { Some(n) } else { None }).unwrap();
        assert!(more >= 7);
    }
}
