//! `ListingCache`: lazy, per-directory listings of remote folders, read by the tree on the UI
//! thread. It never calls a provider. A folder that is not cached yet becomes `Loading` plus ONE
//! request to the hub; a refresh of a known folder keeps showing the old rows meanwhile
//! (stale-while-revalidate); a failure keeps the old rows and remembers the error.

use std::collections::HashMap;
use std::sync::Arc;

use super::error::{RemoteError, RemoteErrorKind};
use super::hub::{JobId, OpKind, RemoteEvent, RemoteHub, RemoteOp, RemoteReply};
use super::path::RemotePath;
use super::provider::RemoteEntry;
use super::sites::SiteId;

pub type Entries = Arc<Vec<RemoteEntry>>;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Listing {
    /// First listing in flight, nothing to show yet.
    Loading,
    /// `refreshing`: a newer listing is in flight, the rows shown are the previous ones.
    Ready { entries: Entries, refreshing: bool },
    /// The last attempt failed; `stale` are the rows from the last success, if any.
    Failed { error: RemoteError, stale: Option<Entries> },
}

impl Listing {
    /// Rows worth drawing, if any (also while refreshing or after a failure).
    pub fn entries(&self) -> Option<&Entries> {
        match self {
            Listing::Ready { entries, .. } => Some(entries),
            Listing::Failed { stale, .. } => stale.as_ref(),
            Listing::Loading => None,
        }
    }
}

struct Slot {
    listing: Listing,
    /// Latest job that may fill this slot; older answers are ignored.
    job: JobId,
}

#[derive(Default)]
pub struct ListingCache {
    map: HashMap<(SiteId, RemotePath), Slot>,
}

impl ListingCache {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn get(&self, site: &str, dir: &RemotePath) -> Option<&Listing> {
        self.map.get(&(site.to_string(), dir.clone())).map(|s| &s.listing)
    }

    pub fn len(&self) -> usize {
        self.map.len()
    }

    pub fn is_empty(&self) -> bool {
        self.map.is_empty()
    }

    fn submit(&mut self, hub: &RemoteHub, site: &str, dir: &RemotePath) {
        let job = hub.submit(site, RemoteOp::List(dir.clone()));
        let k = (site.to_string(), dir.clone());
        let listing = match self.map.get(&k).map(|s| &s.listing) {
            Some(Listing::Ready { entries, .. }) => Listing::Ready { entries: entries.clone(), refreshing: true },
            Some(Listing::Failed { stale: Some(e), .. }) => Listing::Ready { entries: e.clone(), refreshing: true },
            _ => Listing::Loading,
        };
        self.map.insert(k, Slot { listing, job });
    }

    /// First sight of `dir`: queue its listing. Returns `true` if a request was made.
    pub fn ensure(&mut self, hub: &RemoteHub, site: &str, dir: &RemotePath) -> bool {
        if self.get(site, dir).is_some() {
            return false;
        }
        self.submit(hub, site, dir);
        true
    }

    /// User asked for a refresh (F5, or our own mutation under `dir`).
    pub fn refresh(&mut self, hub: &RemoteHub, site: &str, dir: &RemotePath) {
        self.submit(hub, site, dir);
    }

    /// Re-list every folder of `site` that is currently cached (focus regained, reconnect).
    pub fn refresh_site(&mut self, hub: &RemoteHub, site: &str) {
        let dirs: Vec<RemotePath> = self.map.keys().filter(|(s, _)| s == site).map(|(_, d)| d.clone()).collect();
        for d in dirs {
            self.submit(hub, site, &d);
        }
    }

    /// Forget one folder (it will be listed again when next shown).
    pub fn invalidate(&mut self, site: &str, dir: &RemotePath) {
        self.map.remove(&(site.to_string(), dir.clone()));
    }

    /// Forget a folder and everything cached below it (after a rename or delete of a folder).
    pub fn invalidate_tree(&mut self, site: &str, dir: &RemotePath) {
        self.map.retain(|(s, d), _| !(s == site && d.starts_with(dir)));
    }

    pub fn drop_site(&mut self, site: &str) {
        self.map.retain(|(s, _), _| s != site);
    }

    /// Apply a finished job. Returns `true` when something visible changed.
    pub fn apply(&mut self, ev: &RemoteEvent) -> bool {
        if ev.kind != OpKind::List {
            return false;
        }
        let k = (ev.site.clone(), ev.path.clone());
        let Some(slot) = self.map.get_mut(&k) else { return false };
        if slot.job != ev.job {
            return false; // a newer request owns this slot
        }
        let prev = slot.listing.entries().cloned();
        match &ev.result {
            Ok(RemoteReply::Listed(v)) => slot.listing = Listing::Ready { entries: Arc::new(v.clone()), refreshing: false },
            Err(e) if e.kind == RemoteErrorKind::Cancelled => match prev {
                Some(entries) => slot.listing = Listing::Ready { entries, refreshing: false },
                None => {
                    self.map.remove(&k);
                }
            },
            Err(e) => slot.listing = Listing::Failed { error: e.clone(), stale: prev },
            Ok(_) => return false,
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::remote::fake::FakeRemoteProvider;
    use crate::remote::hub::ProviderFactory;
    use crate::remote::provider::SharedProvider;
    use crate::remote::sites::SiteProfile;
    use std::time::{Duration, Instant};

    struct NoFactory;
    impl ProviderFactory for NoFactory {
        fn connect(&self, _: &SiteProfile) -> Result<SharedProvider, RemoteError> {
            Err(RemoteError::new(RemoteErrorKind::Unreachable, "none"))
        }
    }

    fn setup() -> (RemoteHub, Arc<FakeRemoteProvider>) {
        let fake = FakeRemoteProvider::demo();
        let hub = RemoteHub::new(|| {}, Arc::new(NoFactory));
        hub.register_provider("s", fake.clone());
        (hub, fake)
    }

    fn pump(hub: &RemoteHub, cache: &mut ListingCache, until: impl Fn(&ListingCache) -> bool) {
        let t = Instant::now();
        while !until(cache) && t.elapsed() < Duration::from_secs(10) {
            for ev in hub.drain() {
                cache.apply(&ev);
            }
            std::thread::sleep(Duration::from_millis(2));
        }
        assert!(until(cache), "timed out");
    }

    #[test]
    fn first_sight_is_loading_then_ready_and_only_one_request_is_made() {
        let (hub, fake) = setup();
        let mut c = ListingCache::new();
        let d = RemotePath::new("/var/www");
        assert!(c.ensure(&hub, "s", &d));
        assert!(!c.ensure(&hub, "s", &d), "no second request while one is pending");
        assert_eq!(c.get("s", &d), Some(&Listing::Loading));
        pump(&hub, &mut c, |c| matches!(c.get("s", &d), Some(Listing::Ready { .. })));
        assert_eq!(fake.calls(), 1);
        assert!(!c.ensure(&hub, "s", &d), "cached");
        assert!(c.get("s", &d).unwrap().entries().unwrap().iter().any(|e| e.name == "index.php"));
    }

    #[test]
    fn refresh_keeps_the_old_rows_visible() {
        let (hub, fake) = setup();
        let mut c = ListingCache::new();
        let d = RemotePath::new("/var/www");
        c.ensure(&hub, "s", &d);
        pump(&hub, &mut c, |c| matches!(c.get("s", &d), Some(Listing::Ready { refreshing: false, .. })));
        fake.set_latency(Duration::from_millis(80));
        fake.put_file("/var/www/new.txt", b"n");
        c.refresh(&hub, "s", &d);
        match c.get("s", &d).unwrap() {
            Listing::Ready { entries, refreshing: true } => assert!(!entries.iter().any(|e| e.name == "new.txt")),
            other => panic!("{other:?}"),
        }
        pump(&hub, &mut c, |c| matches!(c.get("s", &d), Some(Listing::Ready { refreshing: false, .. })));
        assert!(c.get("s", &d).unwrap().entries().unwrap().iter().any(|e| e.name == "new.txt"));
    }

    #[test]
    fn a_failed_listing_is_an_error_state_that_keeps_the_stale_rows() {
        let (hub, fake) = setup();
        let mut c = ListingCache::new();
        let d = RemotePath::new("/var/www");
        c.ensure(&hub, "s", &d);
        pump(&hub, &mut c, |c| matches!(c.get("s", &d), Some(Listing::Ready { refreshing: false, .. })));
        fake.fail_path("/var/www", std::io::ErrorKind::TimedOut);
        c.refresh(&hub, "s", &d);
        pump(&hub, &mut c, |c| matches!(c.get("s", &d), Some(Listing::Failed { .. })));
        match c.get("s", &d).unwrap() {
            Listing::Failed { error, stale } => {
                assert_eq!(error.kind, RemoteErrorKind::Unreachable);
                assert!(stale.is_some());
            }
            _ => unreachable!(),
        }
        // retry after the server is back
        fake.clear_failures();
        c.refresh(&hub, "s", &d);
        pump(&hub, &mut c, |c| matches!(c.get("s", &d), Some(Listing::Ready { refreshing: false, .. })));
    }

    #[test]
    fn a_first_failure_without_stale_rows_is_a_plain_error() {
        let (hub, _fake) = setup();
        let mut c = ListingCache::new();
        let d = RemotePath::new("/missing");
        c.ensure(&hub, "s", &d);
        pump(&hub, &mut c, |c| matches!(c.get("s", &d), Some(Listing::Failed { .. })));
        let Some(Listing::Failed { error, stale: None }) = c.get("s", &d) else { panic!() };
        assert_eq!(error.kind, RemoteErrorKind::NotFound);
    }

    #[test]
    fn a_late_answer_for_a_superseded_request_is_ignored() {
        let (hub, fake) = setup();
        let mut c = ListingCache::new();
        let d = RemotePath::new("/var/www");
        c.ensure(&hub, "s", &d);
        let stale_ev = RemoteEvent { job: 0, site: "s".into(), kind: OpKind::List, path: d.clone(), to: None, result: Ok(RemoteReply::Listed(vec![])) };
        assert!(!c.apply(&stale_ev));
        assert_eq!(c.get("s", &d), Some(&Listing::Loading));
        pump(&hub, &mut c, |c| matches!(c.get("s", &d), Some(Listing::Ready { .. })));
        assert!(fake.calls() >= 1);
    }

    #[test]
    fn cancelled_first_listing_leaves_no_zombie_loading_row() {
        let (hub, fake) = setup();
        fake.set_latency(Duration::from_millis(60));
        let mut c = ListingCache::new();
        let d = RemotePath::new("/var/www");
        c.ensure(&hub, "s", &d);
        hub.disconnect("s");
        pump(&hub, &mut c, |c| c.get("s", &d).is_none());
    }

    #[test]
    fn invalidation_drops_a_folder_and_what_is_below() {
        let (hub, _f) = setup();
        let mut c = ListingCache::new();
        for d in ["/var", "/var/www", "/etc"] {
            c.ensure(&hub, "s", &RemotePath::new(d));
        }
        c.invalidate_tree("s", &RemotePath::new("/var"));
        assert_eq!(c.len(), 1);
        c.drop_site("s");
        assert!(c.is_empty());
    }
}
