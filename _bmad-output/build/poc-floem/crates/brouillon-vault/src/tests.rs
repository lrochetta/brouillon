use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use crate::container::{Container, Slot};
use crate::os::SLOT_PASSPHRASE;
use crate::testing::{FailingWrap, SoftWrap};
use crate::*;

const CANARY: &str = "CANARY-7f3aQ9xK2mLp4VbN8sTe1WzR";

struct Env {
    _dir: tempfile::TempDir,
    path: PathBuf,
    os: Arc<SoftWrap>,
}

impl Env {
    fn new() -> Env {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("vault.bsv");
        Env { _dir: dir, path, os: Arc::new(SoftWrap::new()) }
    }
    fn vault(&self) -> FileVault {
        FileVault::with_os(&self.path, VaultPolicy::default(), self.os.clone())
    }
    fn unlocked(&self) -> FileVault {
        let v = self.vault();
        v.unlock(Unlock::Os).unwrap();
        v
    }
    fn dir(&self) -> PathBuf {
        self.path.parent().unwrap().to_path_buf()
    }
}

fn ns(kind: SecretKind, label: &str) -> NewSecret {
    NewSecret { kind, label: label.into(), scope: "remote/site-1".into() }
}

fn sec(s: &str) -> Secret {
    Secret::from_bytes(s.as_bytes())
}

fn read(v: &FileVault, id: SecretId) -> Result<Vec<u8>, VaultError> {
    v.with_secret(id, Purpose::RemoteLogin, |s| s.expose_bytes().to_vec())
}

/// Opens a fresh vault on the (possibly doctored) file and exercises everything.
/// Returns the first error, or Ok if every secret came back intact.
fn full_check(env: &Env, ids: &[(SecretId, &str)]) -> Result<(), VaultError> {
    let v = env.vault();
    v.unlock(Unlock::Os)?;
    let list = v.list()?;
    if list.len() != ids.len() {
        return Err(VaultError::Tampered);
    }
    for (id, want) in ids {
        if read(&v, *id)? != want.as_bytes() {
            return Err(VaultError::Tampered);
        }
    }
    Ok(())
}

fn setup_two(env: &Env) -> [(SecretId, &'static str); 2] {
    let v = env.unlocked();
    let a = v.put(ns(SecretKind::Password, "alpha"), sec("pw-alpha-1")).unwrap();
    let b = v.put(ns(SecretKind::ApiKey, "beta"), sec("key-beta-2")).unwrap();
    [(a, "pw-alpha-1"), (b, "key-beta-2")]
}

fn rewrite(env: &Env, f: impl FnOnce(&mut Container)) {
    let mut c = Container::decode(&std::fs::read(&env.path).unwrap()).unwrap();
    f(&mut c);
    std::fs::write(&env.path, c.encode()).unwrap(); // encode() recomputes the footer
}

// ---------------------------------------------------------------- round trip

#[test]
fn round_trip_metadata_and_values() {
    let env = Env::new();
    let v = env.vault();
    assert_eq!(v.state(), LockState::Locked);
    assert!(!env.path.exists(), "nothing touched before the first unlock");
    v.unlock(Unlock::Os).unwrap();
    assert!(matches!(v.state(), LockState::Unlocked { until: None }));

    let a = v.put(ns(SecretKind::Password, "ftp prod"), sec("hunter2")).unwrap();
    let _oauth = v.put(NewSecret { kind: SecretKind::OAuthRefresh, label: "drive".into(), scope: "oauth/gdrive/me".into() }, sec("tok")).unwrap();
    assert_eq!(read(&v, a).unwrap(), b"hunter2");
    let b = v.put(ns(SecretKind::Other, "unused"), sec("z")).unwrap();

    let list = v.list().unwrap();
    assert_eq!(list.len(), 3);
    let ma = list.iter().find(|m| m.id == a).unwrap();
    assert_eq!(ma.label, "ftp prod");
    assert_eq!(ma.scope, "remote/site-1");
    assert_eq!(ma.kind, SecretKind::Password);
    assert!(ma.created > 0);
    assert_eq!(ma.last_used, ctx_nonzero(ma.last_used), "last_used set after a read");
    assert_eq!(ma.used_by, "remote-login");
    let mb = list.iter().find(|m| m.id == b).unwrap();
    assert_eq!(mb.last_used, 0);
    assert_eq!(mb.used_by, "");

    // New instance, same file: lazy unlock, same data, metadata persisted.
    drop(v);
    let v2 = env.unlocked();
    assert_eq!(read(&v2, a).unwrap(), b"hunter2");
    // replace keeps id, changes value.
    v2.replace(a, sec("hunter3")).unwrap();
    assert_eq!(read(&v2, a).unwrap(), b"hunter3");
    // delete removes it.
    v2.delete(b).unwrap();
    assert_eq!(read(&v2, b), Err(VaultError::NotFound));
    assert_eq!(v2.list().unwrap().len(), 2);
    assert_eq!(v2.delete(b), Err(VaultError::NotFound));
}

fn ctx_nonzero(v: u64) -> u64 {
    assert!(v > 0);
    v
}

#[test]
fn last_used_is_flushed_with_the_next_write() {
    let env = Env::new();
    let v = env.unlocked();
    let a = v.put(ns(SecretKind::Password, "a"), sec("x")).unwrap();
    read(&v, a).unwrap();
    v.put(ns(SecretKind::Password, "b"), sec("y")).unwrap(); // flushes usage
    drop(v);
    let v2 = env.unlocked();
    let m = v2.list().unwrap().into_iter().find(|m| m.id == a).unwrap();
    assert!(m.last_used > 0);
    assert_eq!(m.used_by, "remote-login");
}

#[test]
fn empty_and_large_values() {
    let env = Env::new();
    let v = env.unlocked();
    let e = v.put(ns(SecretKind::Other, "empty"), sec("")).unwrap();
    assert_eq!(read(&v, e).unwrap(), b"");
    let big = vec![0xABu8; 50_000];
    let b = v.put(ns(SecretKind::Other, "big"), Secret::from_bytes(&big)).unwrap();
    assert_eq!(read(&v, b).unwrap(), big);
    let too = vec![1u8; 70_000];
    assert_eq!(v.put(ns(SecretKind::Other, "x"), Secret::from_bytes(&too)), Err(VaultError::TooLarge));
}

#[test]
fn trait_object_usage() {
    let env = Env::new();
    let v: Box<dyn SecretStore> = Box::new(env.unlocked());
    let id = v.put(ns(SecretKind::Password, "dyn"), sec("p")).unwrap();
    let len = v.with_secret(id, Purpose::RemoteLogin, |s| s.len()).unwrap();
    assert_eq!(len, 1);
    let m: Box<dyn SecretStore> = Box::new(MemVault::new());
    let id = m.put(ns(SecretKind::Password, "dyn"), sec("pp")).unwrap();
    assert_eq!(m.with_secret(id, Purpose::RemoteLogin, |s| s.len()).unwrap(), 2);
}

#[cfg(windows)]
#[test]
fn real_dpapi_round_trip() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("vault.bsv");
    let v = FileVault::new(&path, VaultPolicy::default());
    v.unlock(Unlock::Os).unwrap();
    let id = v.put(ns(SecretKind::Password, "dpapi"), sec(CANARY)).unwrap();
    drop(v);
    let v2 = FileVault::new(&path, VaultPolicy::default());
    v2.unlock(Unlock::Os).unwrap();
    assert_eq!(read(&v2, id).unwrap(), CANARY.as_bytes());
    // A DPAPI slot taken from another vault does not open this one (entropy binds vault_id).
    let other = tempfile::tempdir().unwrap();
    let p2 = other.path().join("vault.bsv");
    let w = FileVault::new(&p2, VaultPolicy::default());
    w.unlock(Unlock::Os).unwrap();
    w.put(ns(SecretKind::Password, "x"), sec("y")).unwrap();
    drop(w);
    let mut a = Container::decode(&std::fs::read(&path).unwrap()).unwrap();
    let b = Container::decode(&std::fs::read(&p2).unwrap()).unwrap();
    a.slots = b.slots.clone();
    std::fs::write(&path, a.encode()).unwrap();
    let v3 = FileVault::new(&path, VaultPolicy::default());
    assert!(v3.unlock(Unlock::Os).is_err());
    // Corrupted blob: DPAPI itself rejects it, no fallback.
    let mut a = Container::decode(&std::fs::read(&p2).unwrap()).unwrap();
    let n = a.slots[0].data.len();
    a.slots[0].data[n / 2] ^= 0x01;
    std::fs::write(&p2, a.encode()).unwrap();
    let v4 = FileVault::new(&p2, VaultPolicy::default());
    assert!(v4.unlock(Unlock::Os).is_err());
}

// ---------------------------------------------------------------- tamper

#[test]
fn flip_any_byte_is_detected_even_with_footer_recomputed() {
    let env = Env::new();
    let ids = setup_two(&env);
    let good = std::fs::read(&env.path).unwrap();
    full_check(&env, &ids).unwrap();
    let body = good.len() - 32;
    for i in 0..good.len() {
        for bit in [0x01u8, 0x80u8] {
            let mut bad = good.clone();
            bad[i] ^= bit;
            if i < body {
                // An attacker who also fixes the corruption footer.
                use sha2::{Digest, Sha256};
                let d = Sha256::digest(&bad[..body]);
                bad[body..].copy_from_slice(&d);
            }
            std::fs::write(&env.path, &bad).unwrap();
            assert!(full_check(&env, &ids).is_err(), "flip of byte {i} (mask {bit:#x}) went unnoticed");
        }
    }
    std::fs::write(&env.path, &good).unwrap();
    full_check(&env, &ids).unwrap();
}

#[test]
fn truncation_and_extension_are_detected() {
    let env = Env::new();
    let ids = setup_two(&env);
    let good = std::fs::read(&env.path).unwrap();
    for n in (0..good.len()).step_by(7) {
        std::fs::write(&env.path, &good[..n]).unwrap();
        assert!(full_check(&env, &ids).is_err(), "truncation to {n}");
    }
    let mut ext = good.clone();
    ext.extend_from_slice(b"trailing");
    std::fs::write(&env.path, &ext).unwrap();
    assert!(full_check(&env, &ids).is_err());
}

#[test]
fn downgrade_and_slot_attacks() {
    let env = Env::new();
    let ids = setup_two(&env);
    let good = std::fs::read(&env.path).unwrap();

    // 1. strip the keyslot table
    rewrite(&env, |c| c.slots.clear());
    assert!(full_check(&env, &ids).is_err());
    std::fs::write(&env.path, &good).unwrap();

    // 2. add an extra (passphrase) slot: header changes, index AAD no longer matches
    rewrite(&env, |c| c.slots.push(Slot { kind: SLOT_PASSPHRASE, id: 9, data: vec![0; 64] }));
    assert_eq!(full_check(&env, &ids), Err(VaultError::Tampered));
    std::fs::write(&env.path, &good).unwrap();

    // 3. swap in the slot table of another vault
    let env2 = Env::new();
    let _ = setup_two(&env2);
    let other = Container::decode(&std::fs::read(&env2.path).unwrap()).unwrap();
    rewrite(&env, |c| c.slots = other.slots.clone());
    assert!(full_check(&env, &ids).is_err());
    std::fs::write(&env.path, &good).unwrap();

    // 4. algorithm downgrade / unknown algorithm / future version
    rewrite(&env, |c| c.aead_id = 2);
    assert_eq!(full_check(&env, &ids), Err(VaultError::UnsupportedAlgorithm));
    std::fs::write(&env.path, &good).unwrap();
    let mut v = good.clone();
    v[8] = 2; // format_version = 2
    {
        use sha2::{Digest, Sha256};
        let n = v.len() - 32;
        let d = Sha256::digest(&v[..n]);
        v[n..].copy_from_slice(&d);
    }
    std::fs::write(&env.path, &v).unwrap();
    assert_eq!(full_check(&env, &ids), Err(VaultError::UnsupportedVersion));
    std::fs::write(&env.path, &good).unwrap();

    // 5. flags bit (e.g. pretend "portable-origin")
    rewrite(&env, |c| c.flags ^= 0b10);
    assert_eq!(full_check(&env, &ids), Err(VaultError::Tampered));
    std::fs::write(&env.path, &good).unwrap();
    full_check(&env, &ids).unwrap();
}

#[test]
fn record_swap_replay_and_resurrection_are_detected() {
    let env = Env::new();
    let ids = setup_two(&env);
    let good = std::fs::read(&env.path).unwrap();

    // swap the record bodies of the two ids
    rewrite(&env, |c| {
        let (a, b) = (c.records[0].id, c.records[1].id);
        c.records[0].id = b;
        c.records[1].id = a;
    });
    assert!(full_check(&env, &ids).is_err());
    std::fs::write(&env.path, &good).unwrap();

    // duplicate a record under a fresh id (record from "another vault" shape)
    rewrite(&env, |c| {
        let mut r = c.records[0].clone();
        r.id = 0xdead_beef;
        c.records.push(r);
    });
    assert_eq!(full_check(&env, &ids), Err(VaultError::Tampered));
    std::fs::write(&env.path, &good).unwrap();

    // replay an old version of a record after a replace
    let v = env.unlocked();
    v.replace(ids[0].0, sec("new-value")).unwrap();
    drop(v);
    let old = Container::decode(&good).unwrap();
    rewrite(&env, |c| {
        let slot = c.records.iter_mut().find(|r| r.id == ids[0].0.as_u128()).unwrap();
        *slot = old.records.iter().find(|r| r.id == ids[0].0.as_u128()).unwrap().clone();
    });
    assert_eq!(full_check(&env, &[(ids[0].0, "new-value"), ids[1]]), Err(VaultError::Tampered));

    // resurrect a deleted record
    let env3 = Env::new();
    let ids3 = setup_two(&env3);
    let before = Container::decode(&std::fs::read(&env3.path).unwrap()).unwrap();
    let v3 = env3.unlocked();
    v3.delete(ids3[1].0).unwrap();
    drop(v3);
    rewrite(&env3, |c| c.records.push(before.records.iter().find(|r| r.id == ids3[1].0.as_u128()).unwrap().clone()));
    assert_eq!(full_check(&env3, &ids3[..1]), Err(VaultError::Tampered));
}

#[test]
fn parser_limits_and_garbage() {
    let env = Env::new();
    std::fs::write(&env.path, b"not a vault at all").unwrap();
    let v = env.vault();
    assert_eq!(v.unlock(Unlock::Os), Err(VaultError::Corrupt));
    std::fs::write(&env.path, vec![0u8; 5 * 1024 * 1024]).unwrap();
    assert_eq!(env.vault().unlock(Unlock::Os), Err(VaultError::TooLarge));
}

// ---------------------------------------------------------------- rollback

#[test]
fn rollback_is_detected_in_process() {
    let env = Env::new();
    let v = env.unlocked();
    let a = v.put(ns(SecretKind::Password, "a"), sec("one")).unwrap();
    let old = std::fs::read(&env.path).unwrap();
    v.put(ns(SecretKind::Password, "b"), sec("two")).unwrap();
    std::fs::write(&env.path, &old).unwrap(); // attacker restores an older valid file
    assert!(matches!(v.list(), Err(VaultError::Rollback { file_counter: 2, seen_counter: 3 })));
    assert!(matches!(read(&v, a), Err(VaultError::Rollback { .. })));
    // The user confirms a legitimate restore.
    v.accept_rollback();
    assert_eq!(v.list().unwrap().len(), 1);
    assert_eq!(read(&v, a).unwrap(), b"one");
}

#[test]
fn rollback_is_detected_across_restarts_with_sidecar() {
    let env = Env::new();
    let side = env.dir().join("ctr").join("vault.ctr");
    {
        let v = env.vault().with_counter_sidecar(&side);
        v.unlock(Unlock::Os).unwrap();
        v.put(ns(SecretKind::Password, "a"), sec("one")).unwrap();
    }
    let old = std::fs::read(&env.path).unwrap();
    {
        let v = env.vault().with_counter_sidecar(&side);
        v.unlock(Unlock::Os).unwrap();
        v.put(ns(SecretKind::Password, "b"), sec("two")).unwrap();
    }
    std::fs::write(&env.path, &old).unwrap();
    let v = env.vault().with_counter_sidecar(&side);
    assert!(matches!(v.unlock(Unlock::Os), Err(VaultError::Rollback { .. })));
    assert_eq!(v.state(), LockState::Locked);
    v.accept_rollback();
    v.unlock(Unlock::Os).unwrap();
    assert_eq!(v.list().unwrap().len(), 1);
    // Without the sidecar the same restart cannot know (documented limitation).
    let fresh = env.vault();
    fresh.unlock(Unlock::Os).unwrap();
}

// ---------------------------------------------------------------- crash safety

#[test]
fn crash_between_temp_write_and_replace_keeps_old_vault() {
    let env = Env::new();
    let ids = setup_two(&env);
    let before = std::fs::read(&env.path).unwrap();
    let v = env.unlocked();
    v.inject_fault_before_replace(true);
    assert!(v.put(ns(SecretKind::Password, "lost"), sec("never")).is_err());
    assert!(v.replace(ids[0].0, sec("never")).is_err());
    assert!(v.delete(ids[1].0).is_err());
    // The old file is byte-identical and still opens with the old content.
    assert_eq!(std::fs::read(&env.path).unwrap(), before);
    // An orphan temp file was left behind (already-encrypted bytes) ...
    let orphans = |dir: &PathBuf| {
        std::fs::read_dir(dir)
            .unwrap()
            .flatten()
            .filter(|e| e.file_name().to_string_lossy().contains(".tmp."))
            .count()
    };
    assert!(orphans(&env.dir()) >= 1);
    // ... and the next unlock sweeps it; the old content is intact.
    v.inject_fault_before_replace(false);
    full_check(&env, &ids).unwrap();
    assert_eq!(orphans(&env.dir()), 0);
    // After the fault is cleared the vault works again.
    v.put(ns(SecretKind::Password, "ok"), sec("fine")).unwrap();
}

#[test]
fn crash_while_creating_leaves_no_vault() {
    let env = Env::new();
    let v = env.vault();
    v.inject_fault_before_replace(true);
    assert!(v.unlock(Unlock::Os).is_err());
    assert!(!env.path.exists());
    assert_eq!(v.state(), LockState::Locked);
}

#[test]
fn bak_generation_is_a_valid_vault_and_can_restore() {
    let env = Env::new();
    let v = env.unlocked();
    let a = v.put(ns(SecretKind::Password, "a"), sec("one")).unwrap();
    // creation wrote generation 1 (empty); the first put replaced it and kept it as .bak
    let bak = fsutil_bak(&env);
    let b0 = Container::decode(&std::fs::read(&bak).unwrap()).unwrap();
    assert_eq!((b0.counter, b0.records.len()), (1, 0));
    v.put(ns(SecretKind::Password, "b"), sec("two")).unwrap();
    let b = Container::decode(&std::fs::read(&bak).unwrap()).unwrap();
    assert_eq!((b.counter, b.records.len()), (2, 1));
    // Corrupt the main file; restore is an explicit action.
    std::fs::write(&env.path, b"garbage").unwrap();
    assert_eq!(env.vault().unlock(Unlock::Os), Err(VaultError::Corrupt));
    v.restore_from_backup().unwrap();
    let v2 = env.unlocked();
    assert_eq!(v2.list().unwrap().len(), 1);
    assert_eq!(read(&v2, a).unwrap(), b"one");
}

fn fsutil_bak(env: &Env) -> PathBuf {
    crate::fsutil::bak_path(&env.path)
}

// ---------------------------------------------------------------- lock / zeroize / idle

#[test]
fn lock_wipes_keys_and_blocks_access() {
    let env = Env::new();
    let v = env.unlocked();
    let a = v.put(ns(SecretKind::Password, "a"), sec("one")).unwrap();
    let hits = Arc::new(AtomicUsize::new(0));
    let h = hits.clone();
    v.set_lock_listener(Some(Box::new(move || {
        h.fetch_add(1, Ordering::SeqCst);
    })));
    v.lock();
    v.lock(); // idempotent
    assert_eq!(hits.load(Ordering::SeqCst), 1);
    assert_eq!(v.state(), LockState::Locked);
    assert!(v.inner.mk.lock().unwrap().is_none(), "master key dropped (zeroized) on lock");
    assert_eq!(read(&v, a), Err(VaultError::Locked));
    assert_eq!(v.list(), Err(VaultError::Locked));
    assert_eq!(v.put(ns(SecretKind::Other, "x"), sec("y")), Err(VaultError::Locked));
    v.unlock(Unlock::Os).unwrap();
    assert_eq!(read(&v, a).unwrap(), b"one");
}

#[test]
fn secret_wipe_zeroes_the_buffer() {
    let mut s = sec("super-secret");
    let ptr = s.expose_bytes().as_ptr();
    let len = s.len();
    s.wipe();
    assert!(s.is_empty());
    // The Vec was zeroized in place (capacity retained): read the old bytes through the pointer.
    // SAFETY: the allocation is still owned by `s`; we only read `len` bytes within its capacity.
    #[allow(unsafe_code)]
    let after = unsafe { std::slice::from_raw_parts(ptr, len) };
    assert!(after.iter().all(|b| *b == 0), "bytes were zeroed in place");
}

#[test]
fn idle_relock_fires_once_and_is_rearmed_by_use() {
    let env = Env::new();
    let v = env.unlocked();
    let a = v.put(ns(SecretKind::Password, "a"), sec("one")).unwrap();
    let hits = Arc::new(AtomicUsize::new(0));
    let h = hits.clone();
    v.set_lock_listener(Some(Box::new(move || {
        h.fetch_add(1, Ordering::SeqCst);
    })));
    v.set_idle_lock(Some(Duration::from_millis(2000)));
    assert!(matches!(v.state(), LockState::Unlocked { until: Some(_) }));
    // Use keeps it alive past the first deadline.
    for _ in 0..3 {
        std::thread::sleep(Duration::from_millis(400));
        assert_eq!(read(&v, a).unwrap(), b"one");
    }
    assert_eq!(hits.load(Ordering::SeqCst), 0);
    // Idle: locks within a generous bound.
    let t0 = std::time::Instant::now();
    while v.state() != LockState::Locked && t0.elapsed() < Duration::from_secs(30) {
        std::thread::sleep(Duration::from_millis(20));
    }
    assert_eq!(v.state(), LockState::Locked);
    assert_eq!(hits.load(Ordering::SeqCst), 1);
    // Re-unlocking re-arms the same thread.
    v.unlock(Unlock::Os).unwrap();
    let t0 = std::time::Instant::now();
    while v.state() != LockState::Locked && t0.elapsed() < Duration::from_secs(30) {
        std::thread::sleep(Duration::from_millis(20));
    }
    assert_eq!(v.state(), LockState::Locked);
    assert_eq!(hits.load(Ordering::SeqCst), 2);
}

// ---------------------------------------------------------------- policy

#[test]
fn portable_mode_refuses_machine_bound_vault() {
    let env = Env::new();
    let portable = VaultPolicy::for_portable(true);
    assert!(!portable.allow_machine_bound);
    let v = FileVault::with_os(&env.path, portable, env.os.clone());
    let e = v.unlock(Unlock::Os).unwrap_err();
    assert!(matches!(e, VaultError::Policy(_)));
    assert!(e.means_ask_every_time());
    assert!(!env.path.exists(), "no vault, and certainly no plaintext, is created");
    // A vault made on a normal install and carried to a portable stick is refused too.
    env.unlocked().put(ns(SecretKind::Password, "a"), sec("b")).unwrap();
    let v = FileVault::with_os(&env.path, portable, env.os.clone());
    assert!(matches!(v.unlock(Unlock::Os), Err(VaultError::Policy(_))));
    assert_eq!(VaultPolicy::for_portable(false), VaultPolicy::default());
}

#[test]
fn os_wrap_failure_means_no_storage_at_all() {
    let env = Env::new();
    let v = FileVault::with_os(&env.path, VaultPolicy::default(), Arc::new(FailingWrap));
    let e = v.unlock(Unlock::Os).unwrap_err();
    assert!(matches!(e, VaultError::Unavailable(_)));
    assert!(e.means_ask_every_time());
    assert!(!env.path.exists(), "no plaintext or obfuscated fallback file");
    assert_eq!(v.put(ns(SecretKind::Password, "a"), sec(CANARY)), Err(VaultError::Locked));
    assert!(std::fs::read_dir(env.dir()).unwrap().next().is_none());
}

#[test]
fn reserved_slots_parse_but_are_not_implemented() {
    let env = Env::new();
    let _ = setup_two(&env);
    let v = env.vault();
    let pw = sec("pw");
    assert_eq!(v.unlock(Unlock::Passphrase(&pw)), Err(VaultError::NotImplemented("passphrase slot")));
    assert_eq!(v.unlock(Unlock::Recovery(&pw)), Err(VaultError::NotImplemented("recovery key slot")));
    // A file whose only slot is a passphrase slot (authentic header is not needed to see the error kind).
    rewrite(&env, |c| c.slots = vec![Slot { kind: SLOT_PASSPHRASE, id: 1, data: vec![0; 80] }]);
    assert_eq!(env.vault().unlock(Unlock::Os), Err(VaultError::NotImplemented("passphrase slot")));
}

#[test]
fn link_tokens_are_only_readable_for_the_handshake() {
    let env = Env::new();
    for store in [Box::new(env.unlocked()) as Box<dyn SecretStore>, Box::new(MemVault::new())] {
        let t = store.put(ns(SecretKind::LinkToken, "link"), sec("tok")).unwrap();
        for p in [Purpose::RemoteLogin, Purpose::AiRequest, Purpose::Export, Purpose::OAuthRefresh] {
            assert!(matches!(store.with_secret(t, p, |_| ()), Err(VaultError::Policy(_))));
        }
        assert_eq!(store.with_secret(t, Purpose::LinkHandshake, |s| s.len()).unwrap(), 3);
    }
}

#[test]
fn mem_vault_basics_and_lock() {
    let m = MemVault::new();
    let id = m.put(ns(SecretKind::Password, "m"), sec(CANARY)).unwrap();
    assert_eq!(m.with_secret(id, Purpose::RemoteLogin, |s| s.expose_str().unwrap().to_string()).unwrap(), CANARY);
    let meta = m.list().unwrap();
    assert_eq!(meta[0].used_by, "remote-login");
    m.lock();
    assert_eq!(m.state(), LockState::Locked);
    assert_eq!(m.list(), Err(VaultError::Locked));
    m.unlock(Unlock::Os).unwrap();
    assert!(m.list().unwrap().is_empty(), "lock() forgets session secrets");
}

// ---------------------------------------------------------------- canary

fn b64(data: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut s = String::new();
    for ch in data.chunks(3) {
        let n = (ch[0] as u32) << 16 | (*ch.get(1).unwrap_or(&0) as u32) << 8 | *ch.get(2).unwrap_or(&0) as u32;
        for i in 0..4 {
            if i <= ch.len() {
                s.push(T[(n >> (18 - 6 * i) & 63) as usize] as char);
            }
        }
    }
    s
}

fn hex(data: &[u8], upper: bool) -> String {
    data.iter().map(|b| if upper { format!("{b:02X}") } else { format!("{b:02x}") }).collect()
}

fn contains(hay: &[u8], needle: &[u8]) -> bool {
    hay.windows(needle.len()).any(|w| w == needle)
}

/// Every encoding of the canary we scan for.
fn canary_forms(c: &str) -> Vec<Vec<u8>> {
    let raw = c.as_bytes().to_vec();
    let utf16: Vec<u8> = c.encode_utf16().flat_map(|u| u.to_le_bytes()).collect();
    let mut v = vec![raw.clone(), utf16, hex(&raw, false).into_bytes(), hex(&raw, true).into_bytes()];
    // base64 at the three alignments
    for pad in 0..3 {
        let mut shifted = vec![b'x'; pad];
        shifted.extend_from_slice(&raw);
        let enc = b64(&shifted);
        let core = &enc[4..enc.len() - 4];
        v.push(core.as_bytes().to_vec());
    }
    v
}

fn assert_no_canary(where_: &str, hay: &[u8], c: &str) {
    for f in canary_forms(c) {
        assert!(f.len() >= 8);
        assert!(!contains(hay, &f), "canary found in {where_} as {:?}", String::from_utf8_lossy(&f));
    }
}

fn canary_never_leaks(store_for: impl Fn(&Env) -> FileVault, env: &Env) {
    let v = store_for(env);
    v.unlock(Unlock::Os).unwrap();
    let label_canary = format!("label-{CANARY}");
    let id = v
        .put(NewSecret { kind: SecretKind::Password, label: label_canary.clone(), scope: "remote/x".into() }, sec(CANARY))
        .unwrap();
    v.put(ns(SecretKind::ApiKey, "other"), sec("x")).unwrap(); // second write: .bak exists
    v.replace(id, sec(CANARY)).unwrap();
    v.inject_fault_before_replace(true);
    let _ = v.put(ns(SecretKind::Password, "z"), sec(CANARY)); // leaves an orphan tmp
    v.inject_fault_before_replace(false);

    // 1. every file in the directory: vault, .bak, orphan tmp, nothing else.
    let mut seen = 0;
    for e in std::fs::read_dir(env.dir()).unwrap().flatten() {
        let bytes = std::fs::read(e.path()).unwrap();
        assert_no_canary(&e.file_name().to_string_lossy(), &bytes, CANARY);
        assert_no_canary("file (label canary)", &bytes, &label_canary);
        seen += 1;
    }
    assert!(seen >= 2);

    // 2. Debug / Display of everything the API exposes.
    let mut texts: Vec<String> = Vec::new();
    v.with_secret(id, Purpose::RemoteLogin, |s| {
        texts.push(format!("{s:?}"));
        texts.push(format!("{s}"));
        texts.push(format!("{s:#?}"));
        assert_eq!(s.expose_str().unwrap(), CANARY); // sanity: it IS the secret
    })
    .unwrap();
    texts.push(format!("{v:?}"));
    texts.push(format!("{:?}", v.state()));
    texts.push(format!("{:?}", MemVault::new()));
    let mem = MemVault::new();
    let mid = mem.put(ns(SecretKind::Password, "m"), sec(CANARY)).unwrap();
    texts.push(format!("{mem:?}"));
    texts.push(format!("{mid:?}"));
    // metadata holds labels (not secrets) but never values
    for m in v.list().unwrap() {
        texts.push(format!("{m:?}"));
        assert!(!m.used_by.contains(CANARY));
    }

    // 3. error strings from every failure path
    let errors = [
        v.with_secret(SecretId::from_u128(42), Purpose::RemoteLogin, |_| ()).unwrap_err(),
        v.put(ns(SecretKind::Other, "big"), Secret::from_bytes(&vec![b'C'; 70_000])).unwrap_err(),
        v.delete(SecretId::from_u128(7)).unwrap_err(),
        {
            let t = v.put(ns(SecretKind::LinkToken, "t"), sec(CANARY)).unwrap();
            v.with_secret(t, Purpose::AiRequest, |_| ()).unwrap_err()
        },
        VaultError::Rollback { file_counter: 1, seen_counter: 2 },
        VaultError::Unavailable("DPAPI protect failed"),
        VaultError::Corrupt,
        VaultError::Tampered,
        VaultError::Io(std::io::ErrorKind::NotFound),
    ];
    v.lock();
    for e in errors.iter().chain([&v.list().unwrap_err(), &read(&v, id).unwrap_err()]) {
        texts.push(format!("{e}"));
        texts.push(format!("{e:?}"));
    }
    for t in &texts {
        assert_no_canary("debug/display/error text", t.as_bytes(), CANARY);
    }
}

#[test]
fn canary_never_in_file_debug_display_or_errors() {
    let env = Env::new();
    canary_never_leaks(|e| e.vault(), &env);
}

#[cfg(windows)]
#[test]
fn canary_never_leaks_with_real_dpapi() {
    // Same checks, real DPAPI keyslot.
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("vault.bsv");
    let env = Env { _dir: dir, path, os: Arc::new(SoftWrap::new()) };
    canary_never_leaks(|e| FileVault::new(&e.path, VaultPolicy::default()), &env);
}

#[test]
fn canary_scanner_self_check() {
    // The scanner must be able to find the canary if it were there.
    for f in canary_forms(CANARY) {
        let mut hay = b"xxxxxxxxxxxx".to_vec();
        hay.extend_from_slice(&f);
        hay.extend_from_slice(b"yyyy");
        assert!(contains(&hay, &f));
    }
    assert_eq!(format!("{:?}", sec(CANARY)), "Secret(***)");
    assert_eq!(format!("{}", sec(CANARY)), "***");
}
