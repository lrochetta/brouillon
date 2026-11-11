//! `SaveService`: runs the SafeSaver (crate `brouillon-safesave`) for remote documents, off the UI thread.
//!
//! * The UI thread only calls [`SaveService::save`] / `retry` / `resolve` / `recover` (they spawn a
//!   thread and return at once) and [`SaveService::drain`]. It never touches a provider.
//! * One [`RemoteDoc`] (the "base" the server copy is compared with) is kept per open remote file.
//!   While a job runs the doc is checked out, so a second save of the same file answers `Busy`.
//! * Journal and backups live under the settings folder, one sub-folder per site:
//!   `<journal_root>/<site>/<id>.json|.bin`, `<backup_root>/...` (the BackupStore names its own
//!   per-profile folders). Nothing in there is a credential.
//! * "Saved" is reported only for `SaveOutcome::Saved` (the bytes were uploaded to a temp name,
//!   verified, and swapped in). Everything else keeps the buffer dirty.

use std::collections::{HashMap, VecDeque};
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use brouillon_safesave::{doc_id, sha256_hex, Base, BackupStore, ConflictChoice, FileMeta, Journal, RemoteDoc, Resolution, SafeSaver, SaveOptions, SaveOutcome, SyncState};

use super::provider::{OpenStamp, SharedProvider};
use super::state::RemoteSaveState;

/// `(site id, remote path)`.
pub type DocKey = (String, String);

/// Result of asking for a save.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Submit {
    Started,
    /// A job for this file is still running.
    Busy,
    /// No base is known for this file (it was not opened through a safe provider).
    NotTracked,
}

#[derive(Debug)]
pub enum ResolvedView {
    Cancelled,
    Reloaded(Vec<u8>),
    Rebased(Vec<u8>),
    Diff { base: Vec<u8>, local: Vec<u8>, remote: Vec<u8> },
    Saved(SaveOutcome),
    SavedAs { path: String, outcome: SaveOutcome },
}

#[derive(Debug)]
pub enum SaveEvent {
    /// A `save` / `retry` finished. `sent` is exactly what was uploaded.
    Outcome { key: DocKey, sent: Vec<u8>, outcome: SaveOutcome },
    /// A conflict choice was carried out (or failed with a plain message).
    Resolved { key: DocKey, local: Vec<u8>, result: Result<ResolvedView, String> },
    /// Crash recovery ran for a site (after it connected). Entries that need the user are listed by `recoverable`.
    Recovery { site: String, cleaned: usize, repaired: Vec<String>, failed: Vec<String> },
}

/// One journaled buffer that never reached the server.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RecoverItem {
    pub site: String,
    pub id: String,
    pub path: String,
    pub bytes: u64,
    pub state: SyncState,
    pub updated_ms: u64,
}

struct Slot {
    /// `None` while a job holds it.
    doc: Option<RemoteDoc>,
}

struct Inner {
    journal_root: PathBuf,
    backup_root: PathBuf,
    docs: Mutex<HashMap<DocKey, Slot>>,
    events: Mutex<VecDeque<SaveEvent>>,
    wake: Mutex<Option<Arc<dyn Fn() + Send + Sync>>>,
}

pub struct SaveService {
    inner: Arc<Inner>,
}

fn dir_name(site: &str) -> String {
    let s: String = site.chars().map(|c| if c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.') { c } else { '_' }).collect();
    if s.is_empty() || s.starts_with('.') {
        format!("_{s}")
    } else {
        s
    }
}

impl SaveService {
    pub fn new(journal_root: PathBuf, backup_root: PathBuf) -> Self {
        Self { inner: Arc::new(Inner { journal_root, backup_root, docs: Mutex::new(HashMap::new()), events: Mutex::new(VecDeque::new()), wake: Mutex::new(None) }) }
    }

    pub fn set_waker(&self, f: impl Fn() + Send + Sync + 'static) {
        if let Ok(mut w) = self.inner.wake.lock() {
            *w = Some(Arc::new(f));
        }
    }

    pub fn journal_dir(&self, site: &str) -> PathBuf {
        self.inner.journal_root.join(dir_name(site))
    }

    /// UI thread: everything that finished since the last call.
    pub fn drain(&self) -> Vec<SaveEvent> {
        self.inner.events.lock().map(|mut e| e.drain(..).collect()).unwrap_or_default()
    }

    pub fn is_tracked(&self, key: &DocKey) -> bool {
        self.inner.docs.lock().map(|d| d.contains_key(key)).unwrap_or(false)
    }

    pub fn forget(&self, key: &DocKey) {
        if let Ok(mut d) = self.inner.docs.lock() {
            d.remove(key);
        }
    }

    /// Register a file that was just read: `bytes` as received, `stamp` as the server described it then.
    /// No I/O.
    pub fn attach(&self, key: DocKey, bytes: &[u8], stamp: OpenStamp) {
        let doc = RemoteDoc {
            id: doc_id(&key.0, &key.1),
            profile: key.0.clone(),
            path: key.1.clone(),
            base: Some(Base { size: stamp.size, mtime: stamp.mtime, mode: stamp.mode, sha256: sha256_hex(bytes) }),
            shadow: bytes.to_vec(),
            meta: FileMeta::detect(bytes),
            state: SyncState::Clean,
        };
        if let Ok(mut d) = self.inner.docs.lock() {
            d.insert(key, Slot { doc: Some(doc) });
        }
    }

    /// Register a file that was read from a safe provider but whose open stamp is unknown. The base gets
    /// the right content hash and an mtime that can never match, so the first save compares the CONTENT:
    /// an unchanged server file is accepted (the base is refreshed), a changed one is a conflict. This
    /// keeps such a file on the safe path instead of silently falling back to a plain overwrite.
    pub fn attach_unstamped(&self, key: DocKey, bytes: &[u8]) {
        self.attach(key, bytes, OpenStamp { size: bytes.len() as u64, mtime: i64::MIN, mode: 0o644 });
    }

    // ------------------------------------------------------------------ jobs

    pub fn save(&self, key: &DocKey, bytes: Vec<u8>, provider: SharedProvider) -> Submit {
        self.run(key, provider, Job::Save(bytes))
    }

    /// Re-save the journaled snapshot (after Queued / Failed).
    pub fn retry(&self, key: &DocKey, provider: SharedProvider) -> Submit {
        self.run(key, provider, Job::Retry)
    }

    pub fn resolve(&self, key: &DocKey, local: Vec<u8>, choice: ConflictChoice, provider: SharedProvider) -> Submit {
        self.run(key, provider, Job::Resolve(local, choice))
    }

    fn run(&self, key: &DocKey, provider: SharedProvider, job: Job) -> Submit {
        if provider.safe_fs().is_none() {
            return Submit::NotTracked;
        }
        let doc = {
            let Ok(mut d) = self.inner.docs.lock() else { return Submit::NotTracked };
            match d.get_mut(key) {
                None => return Submit::NotTracked,
                Some(s) => match s.doc.take() {
                    None => return Submit::Busy,
                    Some(doc) => doc,
                },
            }
        };
        let inner = self.inner.clone();
        let k = key.clone();
        let spare = doc.clone();
        let spawned = std::thread::Builder::new().name("brouillon-remote-save".into()).spawn({
            let inner = inner.clone();
            let k = k.clone();
            move || execute(inner, k, doc, provider, job)
        });
        if spawned.is_err() {
            if let Ok(mut d) = inner.docs.lock() {
                if let Some(s) = d.get_mut(&k) {
                    s.doc = Some(spare);
                }
            }
            return Submit::Busy;
        }
        Submit::Started
    }

    /// After a site connected: run crash recovery over its journal (cleans our orphan temp files,
    /// puts a swapped-out file back; NEVER uploads a buffer). Returns at once; see `SaveEvent::Recovery`.
    pub fn recover(&self, site: &str, provider: SharedProvider) {
        if provider.safe_fs().is_none() || !self.journal_dir(site).is_dir() {
            return;
        }
        let inner = self.inner.clone();
        let site = site.to_string();
        let _ = std::thread::Builder::new().name("brouillon-remote-recover".into()).spawn(move || {
            let Some(mut fs) = provider.safe_fs() else { return };
            let journal = Journal::new(inner.journal_root.join(dir_name(&site)));
            let backups = BackupStore::new(&inner.backup_root);
            let results = catch_unwind(AssertUnwindSafe(|| SafeSaver::new(&mut *fs, &journal, &backups).recover_all())).unwrap_or_default();
            let (mut cleaned, mut repaired, mut failed) = (0usize, Vec::new(), Vec::new());
            for (id, r) in results {
                match r {
                    Ok(Some(rep)) if rep.committed => cleaned += 1,
                    Ok(Some(rep)) if rep.restored_old || !rep.removed_orphans.is_empty() => repaired.push(id),
                    Ok(_) => {}
                    Err(e) => failed.push(format!("{id}: {e:?}")),
                }
            }
            inner.push(SaveEvent::Recovery { site, cleaned, repaired, failed });
        });
    }

    // ------------------------------------------------------------------ recovery listing (no server)

    /// Journaled buffers that were never confirmed on a server, across all sites (disk only).
    pub fn recoverable(&self) -> Vec<RecoverItem> {
        let mut out = Vec::new();
        let Ok(rd) = std::fs::read_dir(&self.inner.journal_root) else { return out };
        for e in rd.flatten() {
            if !e.path().is_dir() {
                continue;
            }
            let site = e.file_name().to_string_lossy().into_owned();
            if site == "discarded" {
                continue;
            }
            for en in Journal::new(e.path()).list() {
                if en.state == SyncState::Clean {
                    continue;
                }
                out.push(RecoverItem { site: site.clone(), id: en.id, path: en.path, bytes: en.snapshot_len, state: en.state, updated_ms: en.updated_ms });
            }
        }
        out.sort_by(|a, b| b.updated_ms.cmp(&a.updated_ms));
        out
    }

    /// Take a journaled buffer back: registers its document (so a save compares with the right base)
    /// and returns the path and the bytes to open as a modified tab. The journal entry stays until a save succeeds.
    pub fn adopt(&self, site: &str, id: &str) -> Option<(String, Vec<u8>)> {
        let j = Journal::new(self.journal_dir(site));
        let entry = j.get(id).ok().flatten()?;
        let bytes = j.snapshot(id).ok()?;
        let mut doc = entry.to_doc();
        doc.state = SyncState::Recovered;
        let key = (site.to_string(), entry.path.clone());
        if let Ok(mut d) = self.inner.docs.lock() {
            d.insert(key, Slot { doc: Some(doc) });
        }
        Some((entry.path, bytes))
    }

    /// Put a journaled buffer aside (kept 7 days by the journal), without opening it.
    pub fn discard(&self, site: &str, id: &str) -> bool {
        Journal::new(self.journal_dir(site)).discard(id).is_ok()
    }
}

enum Job {
    Save(Vec<u8>),
    Retry,
    Resolve(Vec<u8>, ConflictChoice),
}

impl Inner {
    fn push(&self, e: SaveEvent) {
        if let Ok(mut q) = self.events.lock() {
            q.push_back(e);
        }
        let f = self.wake.lock().ok().and_then(|w| w.clone());
        if let Some(f) = f {
            f();
        }
    }
}

fn execute(inner: Arc<Inner>, key: DocKey, mut doc: RemoteDoc, provider: SharedProvider, job: Job) {
    let spare = doc.clone();
    let journal = Journal::new(inner.journal_root.join(dir_name(&key.0)));
    let backups = BackupStore::new(&inner.backup_root);
    let result = catch_unwind(AssertUnwindSafe(|| {
        let mut fs = provider.safe_fs();
        let fs: &mut (dyn brouillon_safesave::RemoteFs + Send) = match fs.as_mut() {
            Some(f) => &mut **f,
            None => return Err("this server connection cannot save safely".to_string()),
        };
        let mut saver = SafeSaver::new(fs, &journal, &backups);
        Ok(match job {
            Job::Save(bytes) => {
                let outcome = saver.save(&mut doc, &bytes, SaveOptions::default());
                SaveEvent::Outcome { key: key.clone(), sent: bytes, outcome }
            }
            Job::Retry => {
                let sent = journal.snapshot(&doc.id).unwrap_or_default();
                let outcome = saver.retry(&mut doc, SaveOptions::default());
                SaveEvent::Outcome { key: key.clone(), sent, outcome }
            }
            Job::Resolve(local, choice) => {
                let r = saver.resolve_conflict(&mut doc, &local, choice).map_err(|e| format!("{e:?}")).map(|r| match r {
                    Resolution::Diff { base, local, remote } => ResolvedView::Diff { base, local, remote },
                    Resolution::Rebased { buffer } => ResolvedView::Rebased(buffer),
                    Resolution::Saved(o) => ResolvedView::Saved(o),
                    Resolution::SavedAs(nd, o) => ResolvedView::SavedAs { path: nd.path, outcome: o },
                    Resolution::Reloaded { bytes } => ResolvedView::Reloaded(bytes),
                    Resolution::Cancelled => ResolvedView::Cancelled,
                });
                SaveEvent::Resolved { key: key.clone(), local, result: r }
            }
        })
    }));
    let (ev, keep) = match result {
        Ok(Ok(ev)) => (ev, doc),
        Ok(Err(msg)) => (SaveEvent::Resolved { key: key.clone(), local: Vec::new(), result: Err(msg) }, spare),
        Err(_) => (SaveEvent::Resolved { key: key.clone(), local: Vec::new(), result: Err("the save worker stopped unexpectedly".into()) }, spare),
    };
    if let Ok(mut d) = inner.docs.lock() {
        if let Some(s) = d.get_mut(&key) {
            s.doc = Some(keep);
        }
    }
    inner.push(ev);
}

// ---------------------------------------------------------------- outcome -> what the UI shows

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConflictKind {
    /// The server file changed and the buffer is modified too.
    Changed,
    /// The server file is newer but the buffer equals what was opened.
    Newer,
    /// The server file was deleted.
    Gone,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OutcomeView {
    pub state: RemoteSaveState,
    /// Chip tooltip / status line.
    pub note: Option<String>,
    /// The buffer now equals the server copy: clear the modified marker.
    pub saved: bool,
    pub conflict: Option<ConflictKind>,
    /// A Retry action makes sense.
    pub retry: bool,
}

/// Pure mapping used by the UI (and unit tested): the chip state, the dialog to open, and whether the
/// buffer may be called saved. Only `Saved` ever sets `saved`.
pub fn view_of(o: &SaveOutcome) -> OutcomeView {
    use brouillon_safesave::Fail;
    let base = OutcomeView { state: RemoteSaveState::Pending, note: None, saved: false, conflict: None, retry: false };
    match o {
        SaveOutcome::Saved { non_atomic, .. } => OutcomeView {
            state: RemoteSaveState::Synced,
            saved: true,
            note: non_atomic.then(|| "Saved, but this server cannot replace a file in one step, so the swap was not atomic.".to_string()),
            ..base
        },
        SaveOutcome::Conflict { .. } => OutcomeView { state: RemoteSaveState::Conflict, conflict: Some(ConflictKind::Changed), note: Some("The file changed on the server since you opened it. Nothing was overwritten.".into()), ..base },
        SaveOutcome::Stale { .. } => OutcomeView { state: RemoteSaveState::Conflict, conflict: Some(ConflictKind::Newer), note: Some("The server has a newer version and you made no changes.".into()), ..base },
        SaveOutcome::RemoteGone => OutcomeView { state: RemoteSaveState::Conflict, conflict: Some(ConflictKind::Gone), note: Some("The file no longer exists on the server. Nothing was written.".into()), ..base },
        SaveOutcome::Queued { .. } => OutcomeView { state: RemoteSaveState::Pending, retry: true, note: Some("The server could not be reached. Your text is safe here; it will be saved when you retry.".into()), ..base },
        SaveOutcome::Failed(f) => {
            let note = match f {
                Fail::Fs(brouillon_safesave::FsError::PermissionDenied) => "The server refused the write (permission). Your text is safe here.".to_string(),
                Fail::Fs(brouillon_safesave::FsError::NoSpace) => "The server has no space left. Your text is safe here.".to_string(),
                Fail::VerifyFailed => "The uploaded copy did not match, so it was not used. The server file is unchanged.".to_string(),
                Fail::PermissionsChanged => "The file permissions changed on the server. Nothing was written.".to_string(),
                Fail::NotAFile | Fail::DanglingLink => "That path is no longer a regular file on the server. Nothing was written.".to_string(),
                Fail::Backup(_) | Fail::Journal(_) => "Brouillon could not keep a local safety copy, so it did not upload.".to_string(),
                Fail::Crashed | Fail::Fs(_) => "The save failed. Your text is safe here.".to_string(),
            };
            OutcomeView { retry: true, note: Some(note), ..base }
        }
    }
}

/// Default name offered by "Save my version as...": next to the file, never replacing anything.
pub fn save_as_suggestion(path: &str) -> String {
    let (dir, name) = match path.rsplit_once('/') {
        Some((d, n)) => (d, n),
        None => ("", path),
    };
    let mine = match name.rsplit_once('.') {
        Some((stem, ext)) if !stem.is_empty() => format!("{stem}.mine.{ext}"),
        _ => format!("{name}.mine"),
    };
    format!("{dir}/{mine}")
}
