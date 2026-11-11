use std::collections::HashMap;
use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, Weak};
use std::thread::Thread;
use std::time::{Duration, Instant};

use zeroize::Zeroizing;

use crate::container::*;
use crate::crypto::*;
use crate::error::VaultError;
use crate::fsutil;
use crate::os::{default_os_wrap, OsWrap};
use crate::secret::Secret;
use crate::store::*;

type Listener = Arc<dyn Fn() + Send + Sync>;

pub(crate) struct Inner {
    path: PathBuf,
    policy: VaultPolicy,
    os: Arc<dyn OsWrap>,
    /// Master key, RAM only. `None` = locked. Dropping zeroizes.
    pub(crate) mk: Mutex<Option<Key32>>,
    sidecar: Mutex<Option<PathBuf>>,
    /// Newest (vault_id, counter) seen by this process or the sidecar.
    seen: Mutex<Option<([u8; 16], u64)>>,
    accept_rollback: AtomicBool,
    /// last_used / used_by updates, flushed with the next write (a read never rewrites the file).
    usage: Mutex<HashMap<u128, (u64, String)>>,
    start: Instant,
    last_used_ms: AtomicU64,
    idle_ms: AtomicU64,
    idle_thread: Mutex<Option<Thread>>,
    listener: Mutex<Option<Listener>>,
    fault: AtomicBool,
    write_lock: Mutex<()>,
}

/// Production vault: `vault.bsv` rewritten whole and atomically on every change.
///
/// Nothing is read or decrypted until the first [`SecretStore::unlock`] (lazy unlock). Every
/// operation re-reads the file, so a second editor instance never gets overwritten with stale
/// state (single-writer lock file is a documented TODO).
pub struct FileVault {
    pub(crate) inner: Arc<Inner>,
}

struct Loaded {
    c: Container,
    idx: Index,
    mk: Key32,
}

fn os_entropy(vault_id: &[u8; 16]) -> Vec<u8> {
    let mut v = b"brouillon-vault/os-slot/v1".to_vec();
    v.extend_from_slice(vault_id);
    v
}

impl FileVault {
    /// Platform default key wrap (DPAPI on Windows). Touches nothing yet.
    pub fn new(path: impl Into<PathBuf>, policy: VaultPolicy) -> Self {
        Self::with_os(path, policy, default_os_wrap())
    }

    pub fn with_os(path: impl Into<PathBuf>, policy: VaultPolicy, os: Arc<dyn OsWrap>) -> Self {
        FileVault {
            inner: Arc::new(Inner {
                path: path.into(),
                policy,
                os,
                mk: Mutex::new(None),
                sidecar: Mutex::new(None),
                seen: Mutex::new(None),
                accept_rollback: AtomicBool::new(false),
                usage: Mutex::new(HashMap::new()),
                start: Instant::now(),
                last_used_ms: AtomicU64::new(0),
                idle_ms: AtomicU64::new(0),
                idle_thread: Mutex::new(None),
                listener: Mutex::new(None),
                fault: AtomicBool::new(false),
                write_lock: Mutex::new(()),
            }),
        }
    }

    /// Remember the newest write counter outside the vault directory (use a per-user location,
    /// never the portable data dir) so that restarts can detect a rolled-back file.
    pub fn with_counter_sidecar(self, path: impl Into<PathBuf>) -> Self {
        *self.inner.sidecar.lock().unwrap() = Some(path.into());
        self
    }

    pub fn path(&self) -> &Path {
        &self.inner.path
    }

    pub fn exists(&self) -> bool {
        self.inner.path.is_file()
    }

    /// Called from `lock()` (idle timer included). It must only flip a signal: never touch UI.
    pub fn set_lock_listener(&self, f: Option<Box<dyn Fn() + Send + Sync>>) {
        *self.inner.listener.lock().unwrap() = f.map(|b| Arc::from(b) as Listener);
    }

    /// Arms (or disarms) idle re-lock. One lazily spawned parked thread sleeps exactly until the
    /// deadline, re-arms if the vault was used meanwhile, then calls `lock()`; no polling, no UI
    /// timer. Exits when the vault is dropped.
    pub fn set_idle_lock(&self, after: Option<Duration>) {
        let ms = after.map(|d| d.as_millis().max(1) as u64).unwrap_or(0);
        self.inner.idle_ms.store(ms, Ordering::SeqCst);
        if ms == 0 {
            return;
        }
        let mut t = self.inner.idle_thread.lock().unwrap();
        if t.is_none() {
            let weak: Weak<Inner> = Arc::downgrade(&self.inner);
            let h = std::thread::Builder::new()
                .name("vault-idle".into())
                .spawn(move || idle_loop(weak))
                .expect("spawn idle thread");
            *t = Some(h.thread().clone());
        }
        t.as_ref().unwrap().unpark();
    }

    /// After a `Rollback` error: the user confirms that the older file is legitimate.
    pub fn accept_rollback(&self) {
        self.inner.accept_rollback.store(true, Ordering::SeqCst);
    }

    /// Replace a corrupt `vault.bsv` with the `.bak` generation, if the backup parses. Explicit
    /// user action only (a recent secret may be lost). Returns `Corrupt` if the backup is unusable.
    pub fn restore_from_backup(&self) -> Result<(), VaultError> {
        let _w = self.inner.write_lock.lock().unwrap();
        let bak = fsutil::bak_path(&self.inner.path);
        let bytes = std::fs::read(&bak)?;
        let c = Container::decode(&bytes)?;
        fsutil::atomic_write(&self.inner.path, &bytes, false)?;
        self.inner.note_counter(c.vault_id, c.counter, true);
        Ok(())
    }

    /// Test hook: fail between the temp-file write and the atomic replace.
    #[doc(hidden)]
    pub fn inject_fault_before_replace(&self, on: bool) {
        self.inner.fault.store(on, Ordering::SeqCst);
    }
}

impl Drop for FileVault {
    fn drop(&mut self) {
        self.inner.lock();
        if let Some(t) = self.inner.idle_thread.lock().unwrap().as_ref() {
            t.unpark();
        }
    }
}

impl fmt::Debug for FileVault {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "FileVault({:?})", self.state())
    }
}

fn idle_loop(weak: Weak<Inner>) {
    loop {
        let wait = {
            let Some(inner) = weak.upgrade() else { return };
            let idle = inner.idle_ms.load(Ordering::SeqCst);
            if idle == 0 || inner.mk.lock().unwrap().is_none() {
                None
            } else {
                let last = inner.last_used_ms.load(Ordering::SeqCst);
                let now = inner.start.elapsed().as_millis() as u64;
                if now >= last + idle {
                    inner.lock();
                    continue;
                }
                Some(Duration::from_millis(last + idle - now))
            }
        };
        match wait {
            Some(d) => std::thread::park_timeout(d),
            None => std::thread::park(),
        }
        if weak.strong_count() == 0 {
            return;
        }
    }
}

impl Inner {
    fn touch(&self) {
        self.last_used_ms.store(self.start.elapsed().as_millis() as u64, Ordering::SeqCst);
    }

    fn lock(&self) {
        let was = self.mk.lock().unwrap().take();
        // `take()` moved the key out; dropping a `Zeroizing` wipes it.
        let was_unlocked = was.is_some();
        drop(was);
        if was_unlocked {
            let l = self.listener.lock().unwrap().clone();
            if let Some(l) = l {
                l();
            }
        }
    }

    fn set_mk(&self, mk: Key32) {
        *self.mk.lock().unwrap() = Some(mk);
        self.touch();
        if let Some(t) = self.idle_thread.lock().unwrap().as_ref() {
            t.unpark();
        }
    }

    fn mk_copy(&self) -> Result<Key32, VaultError> {
        let g = self.mk.lock().unwrap();
        let k = g.as_ref().ok_or(VaultError::Locked)?;
        Ok(Zeroizing::new(**k))
    }

    fn read_container(&self) -> Result<Container, VaultError> {
        let md = std::fs::metadata(&self.path)?;
        if md.len() as usize > MAX_FILE {
            return Err(VaultError::TooLarge);
        }
        Container::decode(&std::fs::read(&self.path)?)
    }

    fn sidecar_counter(&self, vault_id: &[u8; 16]) -> Option<u64> {
        let p = self.sidecar.lock().unwrap().clone()?;
        let b = std::fs::read(p).ok()?;
        if b.len() != 24 || &b[8..24] != vault_id {
            return None;
        }
        Some(u64::from_le_bytes(b[..8].try_into().ok()?))
    }

    fn note_counter(&self, vault_id: [u8; 16], counter: u64, force: bool) {
        {
            let mut s = self.seen.lock().unwrap();
            let next = match *s {
                Some((id, c)) if id == vault_id && !force => c.max(counter),
                _ => counter,
            };
            *s = Some((vault_id, next));
        }
        let p = self.sidecar.lock().unwrap().clone();
        if let Some(p) = p {
            let stored = if force { None } else { self.sidecar_counter(&vault_id) };
            let v = stored.map_or(counter, |c| c.max(counter));
            let mut b = v.to_le_bytes().to_vec();
            b.extend_from_slice(&vault_id);
            if let Some(dir) = p.parent().filter(|d| !d.as_os_str().is_empty()) {
                let _ = std::fs::create_dir_all(dir);
            }
            let _ = std::fs::write(p, b);
        }
    }

    fn check_rollback(&self, c: &Container) -> Result<(), VaultError> {
        let mem = match *self.seen.lock().unwrap() {
            Some((id, n)) if id == c.vault_id => n,
            _ => 0,
        };
        let seen = mem.max(self.sidecar_counter(&c.vault_id).unwrap_or(0));
        if c.counter < seen {
            if self.accept_rollback.swap(false, Ordering::SeqCst) {
                self.note_counter(c.vault_id, c.counter, true);
                return Ok(());
            }
            return Err(VaultError::Rollback { file_counter: c.counter, seen_counter: seen });
        }
        Ok(())
    }

    fn decrypt_index(c: &Container, mk: &[u8; KEY_LEN]) -> Result<Index, VaultError> {
        let k = subkey(mk, INFO_INDEX);
        let plain = open(&k, &c.index_nonce, &c.index_ct, &c.header_bytes())?;
        let idx = Index::decode(&plain)?;
        if idx.counter != c.counter {
            return Err(VaultError::Tampered);
        }
        // Records and index must describe each other exactly.
        if idx.entries.len() != c.records.len() {
            return Err(VaultError::Tampered);
        }
        for e in &idx.entries {
            let n = c.records.iter().filter(|r| r.id == e.id && r.version == e.version).count();
            if n != 1 {
                return Err(VaultError::Tampered);
            }
        }
        Ok(idx)
    }

    fn load(&self) -> Result<Loaded, VaultError> {
        let mk = self.mk_copy()?;
        let c = self.read_container()?;
        self.check_rollback(&c)?;
        let idx = Self::decrypt_index(&c, &mk)?;
        self.touch();
        Ok(Loaded { c, idx, mk })
    }

    fn commit(&self, mut c: Container, mut idx: Index, mk: &Key32) -> Result<(), VaultError> {
        {
            let mut u = self.usage.lock().unwrap();
            for e in &mut idx.entries {
                if let Some((t, by)) = u.get(&e.id) {
                    if *t >= e.last_used {
                        e.last_used = *t;
                        e.used_by = by.clone();
                    }
                }
            }
            u.clear();
        }
        c.counter += 1;
        idx.counter = c.counter;
        c.index_nonce = random::<NONCE_LEN>()?;
        let k = subkey(mk, INFO_INDEX);
        c.index_ct = seal(&k, &c.index_nonce, &idx.encode(), &c.header_bytes());
        let bytes = c.encode();
        fsutil::atomic_write(&self.path, &bytes, self.fault.load(Ordering::SeqCst))?;
        self.note_counter(c.vault_id, c.counter, false);
        Ok(())
    }

    fn mutate<T>(
        &self,
        f: impl FnOnce(&mut Container, &mut Index, &Key32) -> Result<T, VaultError>,
    ) -> Result<T, VaultError> {
        let _w = self.write_lock.lock().unwrap();
        let Loaded { mut c, mut idx, mk } = self.load()?;
        let out = f(&mut c, &mut idx, &mk)?;
        self.commit(c, idx, &mk)?;
        Ok(out)
    }

    fn create(&self) -> Result<(), VaultError> {
        if self.os.machine_bound() && !self.policy.allow_machine_bound {
            return Err(VaultError::Policy(
                "portable mode: a machine-bound vault is refused (use a passphrase or ask every time)",
            ));
        }
        let mk = random_key()?;
        let vault_id = random::<16>()?;
        let blob = self.os.wrap(&mk, &os_entropy(&vault_id))?;
        let c = Container::empty(vault_id, vec![Slot { kind: self.os.slot_kind(), id: 1, data: blob }]);
        if let Some(dir) = self.path.parent().filter(|d| !d.as_os_str().is_empty()) {
            std::fs::create_dir_all(dir)?;
        }
        self.commit(c, Index { counter: 0, entries: Vec::new() }, &mk)?;
        self.set_mk(mk);
        Ok(())
    }

    fn unlock_os(&self) -> Result<(), VaultError> {
        let _w = self.write_lock.lock().unwrap();
        if !self.path.exists() {
            return self.create();
        }
        fsutil::clean_orphans(&self.path);
        let c = self.read_container()?;
        let slot = match c.slots.iter().find(|s| s.kind == self.os.slot_kind()) {
            Some(s) => s,
            None if c.slots.iter().any(|s| s.kind == crate::os::SLOT_PASSPHRASE) => {
                return Err(VaultError::NotImplemented("passphrase slot"))
            }
            None => return Err(VaultError::Unavailable("no usable keyslot")),
        };
        if self.os.machine_bound() && !self.policy.allow_machine_bound {
            return Err(VaultError::Policy("portable mode: machine-bound keyslot refused"));
        }
        let mk = self.os.unwrap(&slot.data, &os_entropy(&c.vault_id))?;
        Self::decrypt_index(&c, &mk)?; // authenticates header + slot table + index
        self.check_rollback(&c)?;
        self.note_counter(c.vault_id, c.counter, false);
        self.set_mk(mk);
        Ok(())
    }
}

fn new_record(
    c: &Container,
    mk: &Key32,
    id: u128,
    version: u32,
    value: &Secret,
) -> Result<RawRecord, VaultError> {
    if value.len() > MAX_SECRET {
        return Err(VaultError::TooLarge);
    }
    let base = c.base_aad();
    let dk = random_key()?;
    let dk_nonce = random::<NONCE_LEN>()?;
    let nonce = random::<NONCE_LEN>()?;
    let kwrap = subkey(mk, INFO_DK_WRAP);
    let wrapped = seal(&kwrap, &dk_nonce, &dk[..], &record_aad(&base, b"dk", id, version));
    let padded = pad(value.expose_bytes());
    let ct = seal(&dk, &nonce, &padded, &record_aad(&base, b"rec", id, version));
    let mut wrapped_dk = [0u8; WRAPPED_DK_LEN];
    wrapped_dk.copy_from_slice(&wrapped);
    Ok(RawRecord { id, version, dk_nonce, wrapped_dk, nonce, ct })
}

fn read_record(c: &Container, mk: &Key32, r: &RawRecord) -> Result<Secret, VaultError> {
    let base = c.base_aad();
    let kwrap = subkey(mk, INFO_DK_WRAP);
    let dk_plain = open(&kwrap, &r.dk_nonce, &r.wrapped_dk, &record_aad(&base, b"dk", r.id, r.version))?;
    let mut dk = Zeroizing::new([0u8; KEY_LEN]);
    if dk_plain.len() != KEY_LEN {
        return Err(VaultError::Corrupt);
    }
    dk.copy_from_slice(&dk_plain);
    let padded = open(&dk, &r.nonce, &r.ct, &record_aad(&base, b"rec", r.id, r.version))?;
    Ok(Secret::from_zeroizing(unpad(&padded)?))
}

impl SecretStore for FileVault {
    fn state(&self) -> LockState {
        if self.inner.mk.lock().unwrap().is_none() {
            return LockState::Locked;
        }
        let idle = self.inner.idle_ms.load(Ordering::SeqCst);
        let until = (idle > 0).then(|| {
            self.inner.start + Duration::from_millis(self.inner.last_used_ms.load(Ordering::SeqCst) + idle)
        });
        LockState::Unlocked { until }
    }

    fn unlock(&self, how: Unlock<'_>) -> Result<(), VaultError> {
        match how {
            Unlock::Os => self.inner.unlock_os(),
            Unlock::Passphrase(_) => Err(VaultError::NotImplemented("passphrase slot")),
            Unlock::Recovery(_) => Err(VaultError::NotImplemented("recovery key slot")),
        }
    }

    fn lock(&self) {
        self.inner.lock();
    }

    fn put(&self, meta: NewSecret, value: Secret) -> Result<SecretId, VaultError> {
        meta.check()?;
        self.inner.mutate(|c, idx, mk| {
            if c.records.len() >= MAX_RECORDS {
                return Err(VaultError::TooLarge);
            }
            let mut id = u128::from_le_bytes(random::<16>()?);
            while id == 0 || idx.entries.iter().any(|e| e.id == id) {
                id = u128::from_le_bytes(random::<16>()?);
            }
            let rec = new_record(c, mk, id, 1, &value)?;
            c.records.push(rec);
            idx.entries.push(IndexEntry {
                id,
                version: 1,
                kind: meta.kind,
                created: now(),
                last_used: 0,
                label: meta.label,
                scope: meta.scope,
                used_by: String::new(),
            });
            Ok(SecretId(id))
        })
    }

    fn replace(&self, id: SecretId, value: Secret) -> Result<(), VaultError> {
        self.inner.mutate(|c, idx, mk| {
            let e = idx.entries.iter_mut().find(|e| e.id == id.0).ok_or(VaultError::NotFound)?;
            e.version = e.version.checked_add(1).ok_or(VaultError::TooLarge)?;
            let rec = new_record(c, mk, id.0, e.version, &value)?;
            let slot = c.records.iter_mut().find(|r| r.id == id.0).ok_or(VaultError::Tampered)?;
            *slot = rec;
            Ok(())
        })
    }

    fn delete(&self, id: SecretId) -> Result<(), VaultError> {
        self.inner.mutate(|c, idx, _| {
            let n = idx.entries.len();
            idx.entries.retain(|e| e.id != id.0);
            if idx.entries.len() == n {
                return Err(VaultError::NotFound);
            }
            c.records.retain(|r| r.id != id.0);
            Ok(())
        })
    }

    fn list(&self) -> Result<Vec<SecretMeta>, VaultError> {
        let l = self.inner.load()?;
        let u = self.inner.usage.lock().unwrap();
        Ok(l.idx
            .entries
            .iter()
            .map(|e| {
                let mut m = e.meta();
                if let Some((t, by)) = u.get(&e.id) {
                    if *t >= m.last_used {
                        m.last_used = *t;
                        m.used_by = by.clone();
                    }
                }
                m
            })
            .collect())
    }

    fn with_secret_dyn(
        &self,
        id: SecretId,
        purpose: Purpose,
        f: &mut dyn FnMut(&Secret),
    ) -> Result<(), VaultError> {
        let l = self.inner.load()?;
        let e = l.idx.entries.iter().find(|e| e.id == id.0).ok_or(VaultError::NotFound)?;
        purpose_allowed(e.kind, purpose)?;
        let r = l.c.records.iter().find(|r| r.id == id.0 && r.version == e.version).ok_or(VaultError::Tampered)?;
        let secret = read_record(&l.c, &l.mk, r)?;
        self.inner.usage.lock().unwrap().insert(id.0, (now(), purpose.tag().to_string()));
        f(&secret);
        Ok(())
    }
}
