//! The save state machine, conflict resolution and crash recovery.

use crate::backup::{BackupSource, BackupStore};
use crate::doc::{resolve_file, Base, RemoteDoc, SyncState};
use crate::fs::{file_name, is_our_temp, nonce, sha256_hex, sibling, FsError, FsResult, Kind, RemoteFs, Stat};
use crate::journal::{now_ms, Entry, Journal};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verification {
    /// Size matched (server offers no hash).
    Size,
    /// Server-side SHA-256 matched.
    Hash,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Fail {
    /// Simulated process death (test hook); nothing was cleaned up.
    Crashed,
    Fs(FsError),
    VerifyFailed,
    /// Mode changed on the server since open; explicit decision needed.
    PermissionsChanged,
    NotAFile,
    DanglingLink,
    Backup(String),
    Journal(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SaveOutcome {
    /// Verified and committed; `doc` is Clean with a fresh base.
    Saved { verified: Verification, non_atomic: bool },
    /// Server changed since open and the buffer is dirty. Drive `resolve_conflict`.
    Conflict { remote: Option<Stat> },
    /// Server is newer but the buffer equals the base: offer reload.
    Stale { remote: Stat },
    /// File deleted on the server. Re-save with `allow_create`, save-as, or cancel.
    RemoteGone,
    /// Network failure; buffer journaled, retry later with `retry`.
    Queued { reason: String },
    Failed(Fail),
}

#[derive(Debug, Clone, Copy, Default)]
pub struct SaveOptions {
    /// Re-create the file when it vanished from the server.
    pub allow_create: bool,
    /// Write into the live file (no temp+rename). Forces a backup of the remote bytes first.
    pub force_in_place: bool,
}

/// What the UI can pick on a conflict.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConflictChoice {
    /// Return base / local / remote bytes for a diff view.
    Diff,
    /// User-supplied merged text: becomes the new dirty buffer on top of the fresh remote base.
    Merge { merged: Vec<u8> },
    /// Overwrite the server copy; its real current bytes are backed up first (forced).
    Overwrite,
    SaveAs { path: String },
    /// Discard local edits (journaled to `discarded/` for 7 days) and take the server copy.
    Reload,
    Cancel,
}

#[derive(Debug)]
pub enum Resolution {
    Diff { base: Vec<u8>, local: Vec<u8>, remote: Vec<u8> },
    Rebased { buffer: Vec<u8> },
    Saved(SaveOutcome),
    SavedAs(RemoteDoc, SaveOutcome),
    Reloaded { bytes: Vec<u8> },
    Cancelled,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RecoveryReport {
    /// Server already had the buffer content: journal entry cleared.
    pub committed: bool,
    /// Target was missing mid-swap; the old file was put back.
    pub restored_old: bool,
    pub removed_orphans: Vec<String>,
    /// Old file left in place because the target is not ours to judge (tell the user).
    pub left_behind: Vec<String>,
    /// In-place write was interrupted: target may be truncated; restore from local backup.
    pub in_place_unverified: bool,
}

enum StepErr {
    Fs(FsError),
    Journal(std::io::Error),
    Verify,
}
impl From<FsError> for StepErr {
    fn from(e: FsError) -> Self {
        StepErr::Fs(e)
    }
}
impl From<std::io::Error> for StepErr {
    fn from(e: std::io::Error) -> Self {
        StepErr::Journal(e)
    }
}

pub struct SafeSaver<'a> {
    fs: &'a mut dyn RemoteFs,
    journal: &'a Journal,
    backups: &'a BackupStore,
}

fn content_sha(fs: &mut dyn RemoteFs, path: &str) -> FsResult<String> {
    if fs.caps().hash {
        if let Some(h) = fs.hash(path)? {
            return Ok(h);
        }
    }
    Ok(sha256_hex(&fs.read(path)?))
}

fn same_stamp(a: Option<Stat>, b: Option<Stat>) -> bool {
    match (a, b) {
        (None, None) => true,
        (Some(a), Some(b)) => a.size == b.size && a.mtime == b.mtime,
        _ => false,
    }
}

fn in_flight(e: &Entry) -> bool {
    e.temp.is_some()
        || e.old.is_some()
        || !e.orphans.is_empty()
        || e.in_place
        || matches!(e.state, SyncState::Preflight | SyncState::Uploading | SyncState::Verify | SyncState::Swap)
}

/// Server-side repair, shared by live failures and crash recovery. Never uploads
/// buffer content: it only removes our own temp files and restores a swapped-out file.
fn repair(fs: &mut dyn RemoteFs, e: &mut Entry) -> FsResult<RecoveryReport> {
    let mut r = RecoveryReport::default();
    let mut keep_old = false;
    let target = e.target.clone();
    let committed = |fs: &mut dyn RemoteFs, e: &Entry| -> FsResult<bool> {
        match fs.stat(&e.target)? {
            Some(s) if s.kind == Kind::File && s.size == e.snapshot_len => Ok(content_sha(fs, &e.target)? == e.snapshot_sha256),
            _ => Ok(false),
        }
    };
    // Leftovers of attempts that already completed: safe to delete unconditionally.
    while let Some(o) = e.orphans.last().cloned() {
        if is_our_temp(&o, &target) && fs.stat(&o)?.is_some() {
            fs.remove(&o)?;
            r.removed_orphans.push(o);
        }
        e.orphans.pop();
    }
    if let Some(old) = e.old.clone() {
        if is_our_temp(&old, &target) {
            if fs.stat(&old)?.is_some() {
                if fs.stat(&target)?.is_none() {
                    fs.rename(&old, &target)?;
                    r.restored_old = true;
                } else if committed(fs, e)? {
                    fs.remove(&old)?;
                    r.removed_orphans.push(old);
                } else {
                    // Not ours to judge: keep it recorded and tell the user.
                    r.left_behind.push(old);
                    keep_old = true;
                }
            }
        }
        if !keep_old {
            e.old = None;
        }
    }
    if let Some(t) = e.temp.clone() {
        if is_our_temp(&t, &target) && fs.stat(&t)?.is_some() {
            fs.remove(&t)?;
            r.removed_orphans.push(t);
        }
        e.temp = None;
    }
    r.committed = committed(fs, e)?;
    r.in_place_unverified = e.in_place && !r.committed;
    Ok(r)
}

impl<'a> SafeSaver<'a> {
    pub fn new(fs: &'a mut dyn RemoteFs, journal: &'a Journal, backups: &'a BackupStore) -> Self {
        SafeSaver { fs, journal, backups }
    }

    /// Save `bytes` (written verbatim: encoding, BOM, EOL, final newline untouched).
    pub fn save(&mut self, doc: &mut RemoteDoc, bytes: &[u8], opts: SaveOptions) -> SaveOutcome {
        self.save_inner(doc, bytes, opts, false)
    }

    /// Retry a Queued/Failed save from the journaled snapshot.
    pub fn retry(&mut self, doc: &mut RemoteDoc, opts: SaveOptions) -> SaveOutcome {
        match self.journal.snapshot(&doc.id) {
            Ok(b) => self.save_inner(doc, &b, opts, false),
            Err(e) => SaveOutcome::Failed(Fail::Journal(e.to_string())),
        }
    }

    pub fn resolve_conflict(&mut self, doc: &mut RemoteDoc, local: &[u8], choice: ConflictChoice) -> FsResult<Resolution> {
        match choice {
            ConflictChoice::Cancel => {
                doc.state = SyncState::Dirty;
                Ok(Resolution::Cancelled)
            }
            ConflictChoice::Diff => {
                let (real, _) = resolve_file(self.fs, &doc.path)?;
                let remote = self.fs.read(&real)?;
                Ok(Resolution::Diff { base: doc.shadow.clone(), local: local.to_vec(), remote })
            }
            ConflictChoice::Merge { merged } => {
                let (_, remote) = RemoteDoc::open(self.fs, &doc.profile, &doc.path)?;
                doc.base = remote.base;
                doc.shadow = remote.shadow;
                doc.state = SyncState::Dirty;
                Ok(Resolution::Rebased { buffer: merged })
            }
            ConflictChoice::Overwrite => Ok(Resolution::Saved(self.save_inner(doc, local, SaveOptions { allow_create: true, ..Default::default() }, true))),
            ConflictChoice::SaveAs { path } => {
                let mut nd = RemoteDoc::new_file(&doc.profile, &path);
                nd.meta = doc.meta.clone();
                let out = self.save_inner(&mut nd, local, SaveOptions::default(), false);
                Ok(Resolution::SavedAs(nd, out))
            }
            ConflictChoice::Reload => {
                // The local buffer is kept 7 days in discarded/ first.
                let en = Self::entry_for(doc, local, SyncState::Dirty);
                self.journal.put(&en, local).map_err(|x| FsError::Other(x.to_string()))?;
                self.journal.discard(&doc.id).map_err(|x| FsError::Other(x.to_string()))?;
                let (bytes, nd) = RemoteDoc::open(self.fs, &doc.profile, &doc.path)?;
                *doc = nd;
                Ok(Resolution::Reloaded { bytes })
            }
        }
    }

    /// Crash recovery for one journal entry. NEVER uploads the buffer. Cleans our
    /// orphan temp files, restores a swapped-out file, and clears the entry if the
    /// server already holds the buffer. Otherwise the entry becomes `Recovered`.
    pub fn recover(&mut self, id: &str) -> FsResult<Option<RecoveryReport>> {
        let jerr = |e: std::io::Error| FsError::Other(e.to_string());
        let Some(mut e) = self.journal.get(id).map_err(jerr)? else { return Ok(None) };
        let mut report = RecoveryReport::default();
        if in_flight(&e) {
            report = repair(self.fs, &mut e)?;
            if report.committed || e.state == SyncState::Clean {
                self.journal.clear(id).map_err(jerr)?;
                return Ok(Some(report));
            }
        }
        e.state = SyncState::Recovered;
        self.journal.update(&e).map_err(jerr)?;
        Ok(Some(report))
    }

    pub fn recover_all(&mut self) -> Vec<(String, FsResult<Option<RecoveryReport>>)> {
        let ids: Vec<String> = self.journal.list().into_iter().map(|e| e.id).collect();
        ids.into_iter().map(|id| (id.clone(), self.recover(&id))).collect()
    }

    fn entry_for(doc: &RemoteDoc, bytes: &[u8], state: SyncState) -> Entry {
        Entry {
            id: doc.id.clone(),
            profile: doc.profile.clone(),
            path: doc.path.clone(),
            target: doc.path.clone(),
            base: doc.base.clone(),
            meta: doc.meta.clone(),
            snapshot_len: bytes.len() as u64,
            snapshot_sha256: sha256_hex(bytes),
            state,
            temp: None,
            old: None,
            orphans: Vec::new(),
            in_place: false,
            attempts: 0,
            updated_ms: now_ms(),
        }
    }

    fn save_inner(&mut self, doc: &mut RemoteDoc, bytes: &[u8], opts: SaveOptions, overwrite: bool) -> SaveOutcome {
        let mut e = Self::entry_for(doc, bytes, SyncState::Queued);
        if let Ok(Some(mut prev)) = self.journal.get(&doc.id) {
            e.attempts = prev.attempts + 1;
            if prev.temp.is_some() || prev.old.is_some() || !prev.orphans.is_empty() {
                match repair(self.fs, &mut prev) {
                    Err(FsError::Crash) => return SaveOutcome::Failed(Fail::Crashed),
                    Ok(_) => {}
                    Err(_) => {
                        e.temp = prev.temp;
                        e.old = prev.old;
                        e.orphans = prev.orphans;
                    }
                }
            }
        }
        if let Err(x) = self.journal.put(&e, bytes) {
            return SaveOutcome::Failed(Fail::Journal(x.to_string()));
        }
        let res = self.steps(doc, bytes, &mut e, opts, overwrite);
        let mut out = match res {
            Ok(o) => o,
            Err(StepErr::Fs(FsError::Crash)) => return SaveOutcome::Failed(Fail::Crashed),
            Err(StepErr::Fs(FsError::Transport(m))) => SaveOutcome::Queued { reason: m },
            Err(StepErr::Fs(x)) => SaveOutcome::Failed(Fail::Fs(x)),
            Err(StepErr::Verify) => SaveOutcome::Failed(Fail::VerifyFailed),
            Err(StepErr::Journal(x)) => SaveOutcome::Failed(Fail::Journal(x.to_string())),
        };
        if matches!(out, SaveOutcome::Saved { .. }) {
            return out;
        }
        if e.temp.is_some() || e.old.is_some() {
            let non_atomic = e.old.is_some();
            match repair(self.fs, &mut e) {
                Err(FsError::Crash) => return SaveOutcome::Failed(Fail::Crashed),
                Ok(r) if r.committed => {
                    // The commit had actually landed (e.g. ack lost): this is a success.
                    if let Ok(Some(st)) = self.fs.stat(&e.target) {
                        self.finish_ok(doc, bytes, &mut e, st);
                        return SaveOutcome::Saved { verified: Verification::Size, non_atomic };
                    }
                }
                _ => {} // leftovers stay in the entry; the next save / recovery sweeps them
            }
        }
        e.state = match &out {
            SaveOutcome::Queued { .. } => SyncState::Queued,
            SaveOutcome::Conflict { .. } => SyncState::Conflict,
            SaveOutcome::Stale { .. } => SyncState::Stale,
            SaveOutcome::RemoteGone => SyncState::RemoteGone,
            _ => SyncState::Failed,
        };
        doc.state = e.state;
        e.updated_ms = now_ms();
        if let Err(x) = self.journal.update(&e) {
            if matches!(out, SaveOutcome::Failed(_)) {
                out = SaveOutcome::Failed(Fail::Journal(x.to_string()));
            }
        }
        out
    }

    fn phase(&mut self, doc: &mut RemoteDoc, e: &mut Entry, s: SyncState) -> Result<(), StepErr> {
        doc.state = s;
        e.state = s;
        e.updated_ms = now_ms();
        self.journal.update(e)?;
        Ok(())
    }

    fn finish_ok(&mut self, doc: &mut RemoteDoc, bytes: &[u8], e: &mut Entry, st: Stat) {
        doc.base = Some(Base::from(&st, sha256_hex(bytes)));
        doc.shadow = bytes.to_vec();
        doc.state = SyncState::Clean;
        e.state = SyncState::Clean;
        // A failed cleanup only leaves orphans to sweep later.
        e.orphans.extend(e.temp.take());
        e.orphans.extend(e.old.take());
        if e.orphans.is_empty() {
            let _ = self.journal.clear(&e.id);
        } else {
            let _ = self.journal.update(e);
        }
    }

    fn steps(&mut self, doc: &mut RemoteDoc, bytes: &[u8], e: &mut Entry, opts: SaveOptions, overwrite: bool) -> Result<SaveOutcome, StepErr> {
        let caps = self.fs.caps();
        let want_sha = sha256_hex(bytes);

        // 1. Preflight: resolve symlink to the real file, compare with the base.
        self.phase(doc, e, SyncState::Preflight)?;
        let lst = self.fs.stat(&doc.path)?;
        let (real, cur): (String, Option<Stat>) = match lst {
            None => (doc.path.clone(), None),
            Some(s) if s.kind == Kind::Symlink => match self.fs.realpath(&doc.path) {
                Err(FsError::NotFound) => return Ok(SaveOutcome::Failed(Fail::DanglingLink)),
                r => {
                    let real = r?;
                    match self.fs.stat(&real)? {
                        None => return Ok(SaveOutcome::Failed(Fail::DanglingLink)),
                        Some(rs) => (real, Some(rs)),
                    }
                }
            },
            Some(s) => (doc.path.clone(), Some(s)),
        };
        e.target = real.clone();
        if let Some(c) = cur {
            if c.kind != Kind::File {
                return Ok(SaveOutcome::Failed(Fail::NotAFile));
            }
        }
        match (cur, doc.base.clone()) {
            (None, Some(_)) if !opts.allow_create => return Ok(SaveOutcome::RemoteGone),
            (None, _) => {}
            (Some(c), None) => {
                if !overwrite {
                    return Ok(SaveOutcome::Conflict { remote: Some(c) });
                }
            }
            (Some(c), Some(b)) if !overwrite => {
                if (c.size, c.mtime) == (b.size, b.mtime) {
                    if c.mode != b.mode {
                        return Ok(SaveOutcome::Failed(Fail::PermissionsChanged));
                    }
                } else if content_sha(self.fs, &real)? == b.sha256 {
                    // touch-only change: refresh the base silently
                    doc.base = Some(Base::from(&c, b.sha256));
                } else if !doc.shadow.is_empty() && doc.shadow == bytes {
                    return Ok(SaveOutcome::Stale { remote: c });
                } else {
                    return Ok(SaveOutcome::Conflict { remote: Some(c) });
                }
            }
            _ => {}
        }

        // 2. Local backup of what we are about to replace (never skipped silently).
        if cur.is_some() {
            let forced = overwrite || opts.force_in_place || doc.shadow.is_empty();
            let (data, src) = if forced {
                let src = if overwrite || opts.force_in_place { BackupSource::BeforeOverwrite } else { BackupSource::BeforeSave };
                (self.fs.read(&real)?, src)
            } else {
                (doc.shadow.clone(), BackupSource::BeforeSave)
            };
            if let Err(x) = self.backups.put(&doc.profile, &real, &data, src) {
                return Ok(SaveOutcome::Failed(Fail::Backup(x.to_string())));
            }
        }
        let mode = cur.map(|c| c.mode).or(doc.base.as_ref().map(|b| b.mode));

        // 3a. Forced in-place write (not atomic; backup above is mandatory).
        if opts.force_in_place && cur.is_some() {
            e.in_place = true;
            self.phase(doc, e, SyncState::Uploading)?;
            self.fs.write_truncate(&real, bytes)?;
            self.phase(doc, e, SyncState::Verify)?;
            let verified = self.verify(&real, bytes, &want_sha)?;
            let st = self.fs.stat(&real)?.ok_or(StepErr::Verify)?;
            e.in_place = false;
            self.finish_ok(doc, bytes, e, st);
            return Ok(SaveOutcome::Saved { verified, non_atomic: true });
        }

        // 3b. Upload to a hidden unique temp in the same directory.
        let temp = sibling(&real, &format!(".{}.brouillon-{}.tmp", file_name(&real), nonce()));
        e.temp = Some(temp.clone());
        self.phase(doc, e, SyncState::Uploading)?;
        self.fs.write_new(&temp, bytes, mode)?;
        if let Some(m) = mode {
            if caps.set_mode {
                self.fs.set_mode(&temp, m)?;
            }
        }

        // 4. Verify the temp.
        self.phase(doc, e, SyncState::Verify)?;
        let verified = self.verify(&temp, bytes, &want_sha)?;

        // 5. Re-check the target right before the swap (narrows the race).
        let now = self.fs.stat(&real)?;
        if !same_stamp(cur, now) {
            return Ok(SaveOutcome::Conflict { remote: now });
        }

        // 6. Swap.
        self.phase(doc, e, SyncState::Swap)?;
        let mut non_atomic = false;
        if cur.is_none() {
            match self.fs.rename(&temp, &real) {
                Err(FsError::Exists) => return Ok(SaveOutcome::Conflict { remote: self.fs.stat(&real)? }),
                r => r?,
            }
            e.temp = None;
        } else if caps.posix_rename {
            self.fs.rename_replace(&temp, &real)?;
            e.temp = None;
        } else {
            non_atomic = true;
            let old = sibling(&real, &format!(".{}.brouillon-old-{}.old", file_name(&real), nonce()));
            e.old = Some(old.clone());
            self.journal.update(e)?;
            self.fs.rename(&real, &old)?;
            if let Err(x) = self.fs.rename(&temp, &real) {
                // Undo immediately; if that fails too the entry keeps `old` for repair.
                if self.fs.rename(&old, &real).is_ok() {
                    e.old = None;
                }
                return Err(x.into());
            }
            e.temp = None;
        }

        // 7. Post-verify the live name.
        let post = self.verify(&real, bytes, &want_sha);
        if matches!(post, Err(StepErr::Verify)) && non_atomic {
            // Put the old file back: new content out of the way, old in.
            if let Some(old) = e.old.clone() {
                if self.fs.rename(&real, &temp).is_ok() && self.fs.rename(&old, &real).is_ok() {
                    e.old = None;
                    e.temp = Some(temp);
                }
            }
        }
        post?;
        let st = self.fs.stat(&real)?.ok_or(StepErr::Verify)?;

        // 8. Drop the old file (a failure here only leaves a swept orphan).
        if let Some(old) = e.old.clone() {
            if self.fs.remove(&old).is_ok() {
                e.old = None;
            }
        }
        self.finish_ok(doc, bytes, e, st);
        Ok(SaveOutcome::Saved { verified, non_atomic })
    }

    fn verify(&mut self, path: &str, bytes: &[u8], want_sha: &str) -> Result<Verification, StepErr> {
        let st = self.fs.stat(path)?.ok_or(StepErr::Verify)?;
        if st.size != bytes.len() as u64 {
            return Err(StepErr::Verify);
        }
        if self.fs.caps().hash {
            if let Some(h) = self.fs.hash(path)? {
                return if h == want_sha { Ok(Verification::Hash) } else { Err(StepErr::Verify) };
            }
        }
        Ok(Verification::Size)
    }
}
