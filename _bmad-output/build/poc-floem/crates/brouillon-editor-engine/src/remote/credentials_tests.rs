//! `Credentials` against the prompt broker: the keep-after-connect cycle, the wrong-saved-password
//! fallback, "ask every time" degradations, and the canary sweep (real DPAPI, in a temp dir).

use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use std::time::Duration;

use brouillon_vault::{LockState, MemVault, NewSecret, Purpose, Secret, SecretId, SecretMeta, SecretStore, Unlock, VaultError};

use super::credentials::{scope_for, Credentials, SavedEntry};
use super::helper::{AskKind, PromptBroker, PromptVault};

fn mem() -> Arc<Credentials> {
    Credentials::with_store(Arc::new(MemVault::new()), true)
}

/// Block a helper-side ask on a thread, answer it from the "UI", return what the helper received.
fn ask_and_answer(b: &Arc<PromptBroker>, site: &str, answer: &str) -> Option<String> {
    let (b2, s) = (b.clone(), site.to_string());
    let h = std::thread::spawn(move || b2.ask(&s, "vps", AskKind::Password, "pw:", &AtomicBool::new(false), Duration::from_secs(5)));
    let id = loop {
        if let Some(v) = b.current() {
            break v.id;
        }
        std::thread::sleep(Duration::from_millis(5));
    };
    assert!(b.answer(id, Some(answer.into())));
    h.join().unwrap()
}

#[test]
fn use_this_time_only_leaves_nothing_behind() {
    let c = mem();
    let b = PromptBroker::new();
    b.set_vault(c.clone());
    assert_eq!(ask_and_answer(&b, "s", "pw1").as_deref(), Some("pw1"));
    let offer = c.connected("s").expect("a proven typed password is offered");
    assert!(!offer.replace);
    c.discard("s");
    assert!(c.connected("s").is_none());
    assert!(!c.is_saved("s"));
    assert!(c.entries().unwrap().is_empty());
    assert!(c.keep("s", "vps").is_err(), "nothing pending: nothing to keep");
}

#[test]
fn a_failed_connect_never_offers_to_keep_a_typo() {
    let c = mem();
    c.typed("s", AskKind::Password, "typo");
    assert!(!c.failed("s"), "it was not a saved password");
    assert!(c.connected("s").is_none());
}

#[test]
fn kept_password_answers_the_next_connect_without_a_dialog() {
    let c = mem();
    let b = PromptBroker::new();
    b.set_vault(c.clone());
    ask_and_answer(&b, "s", "pw1");
    assert!(c.connected("s").is_some());
    c.keep("s", "vps").unwrap();
    assert!(c.is_saved("s"));
    // Next connect: the broker answers by itself, nothing is queued for the UI.
    let got = b.ask("s", "vps", AskKind::Password, "pw:", &AtomicBool::new(false), Duration::from_secs(1));
    assert_eq!(got.as_deref(), Some("pw1"));
    assert!(b.current().is_none(), "no dialog was shown");
    assert!(c.connected("s").is_none(), "no new offer for a saved password that worked");
    // Host-key questions are never answered from the vault.
    assert!(b.ask("s", "vps", AskKind::HostKey, "trust?", &AtomicBool::new(true), Duration::from_millis(10)).is_none());
}

#[test]
fn a_wrong_saved_password_falls_back_to_asking_and_offers_to_replace() {
    let c = mem();
    c.typed("s", AskKind::Password, "old");
    c.connected("s");
    c.keep("s", "vps").unwrap();
    assert_eq!(c.resolve("s", AskKind::Password).as_deref(), Some("old"));
    // The helper asks again in the same connect: the saved one was rejected, so ask the user.
    assert!(c.resolve("s", AskKind::Password).is_none());
    c.typed("s", AskKind::Password, "new");
    let offer = c.connected("s").unwrap();
    assert!(offer.replace);
    c.keep("s", "vps").unwrap();
    assert_eq!(c.entries().unwrap().len(), 1, "replaced, not duplicated");
    assert_eq!(c.resolve("s", AskKind::Password).as_deref(), Some("new"));
}

#[test]
fn a_saved_password_rejected_by_the_server_is_reported_and_not_retried() {
    let c = mem();
    c.typed("s", AskKind::Password, "old");
    c.connected("s");
    c.keep("s", "vps").unwrap();
    assert!(c.resolve("s", AskKind::Password).is_some());
    assert!(c.failed("s"), "the UI says the saved password was not accepted");
    assert!(c.resolve("s", AskKind::Password).is_none(), "the next connect asks");
}

#[test]
fn forget_removes_the_value_and_the_saved_state() {
    let c = mem();
    c.typed("s", AskKind::Password, "pw");
    c.connected("s");
    c.keep("s", "vps").unwrap();
    assert_eq!(c.forget("s").unwrap(), 1);
    assert!(!c.is_saved("s"));
    assert!(c.resolve("s", AskKind::Password).is_none());
    assert_eq!(c.forget("s").unwrap(), 0, "idempotent");
}

#[test]
fn the_view_lists_metadata_and_delete_clears_the_site() {
    let c = mem();
    c.typed("s", AskKind::Passphrase, "pp");
    c.connected("s");
    c.keep("s", "vps").unwrap();
    let e: Vec<SavedEntry> = c.entries().unwrap();
    assert_eq!((e.len(), e[0].kind, e[0].site_id.as_deref()), (1, "key passphrase", Some("s")));
    assert_eq!(e[0].scope, scope_for("s", AskKind::Passphrase));
    assert_eq!(c.delete_entry(e[0].id).unwrap().as_deref(), Some("s"));
    assert!(!c.is_saved("s"));
}

#[test]
fn portable_mode_keeps_nothing_and_says_so() {
    let c = Credentials::with_store(Arc::new(MemVault::new()), false);
    c.typed("s", AskKind::Password, "pw");
    assert!(c.connected("s").is_none(), "the typed password was not even retained");
    assert!(!c.keep_possible());
    assert!(c.unavailable_note().unwrap().contains("ask each time"));
    assert!(c.keep("s", "vps").is_err());
}

/// A store whose OS key wrap does not work here.
struct Broken;
impl SecretStore for Broken {
    fn state(&self) -> LockState {
        LockState::Locked
    }
    fn unlock(&self, _: Unlock<'_>) -> Result<(), VaultError> {
        Err(VaultError::Unavailable("no key store"))
    }
    fn lock(&self) {}
    fn put(&self, _: NewSecret, _: Secret) -> Result<SecretId, VaultError> {
        Err(VaultError::Locked)
    }
    fn replace(&self, _: SecretId, _: Secret) -> Result<(), VaultError> {
        Err(VaultError::Locked)
    }
    fn delete(&self, _: SecretId) -> Result<(), VaultError> {
        Err(VaultError::Locked)
    }
    fn list(&self) -> Result<Vec<SecretMeta>, VaultError> {
        Err(VaultError::Locked)
    }
    fn with_secret_dyn(&self, _: SecretId, _: Purpose, _: &mut dyn FnMut(&Secret)) -> Result<(), VaultError> {
        Err(VaultError::Locked)
    }
}

#[test]
fn an_unavailable_vault_degrades_to_ask_every_time_without_any_fallback() {
    let c = Credentials::with_store(Arc::new(Broken), true);
    c.typed("s", AskKind::Password, "pw");
    assert!(c.connected("s").is_some());
    let e = c.keep("s", "vps").unwrap_err();
    assert!(e.means_ask_every_time());
    assert!(!c.keep_possible());
    assert!(c.unavailable_note().is_some());
    c.typed("s", AskKind::Password, "again");
    assert!(c.connected("s").is_none(), "after a failed vault nothing is retained anymore");
    assert!(!c.is_saved("s"));
}

#[test]
fn lock_drops_what_was_typed_and_not_yet_proven() {
    let c = mem();
    c.typed("s", AskKind::Password, "pw");
    c.lock();
    assert!(c.connected("s").is_none());
    assert!(c.is_locked());
}

// ---------------------------------------------------------------- canary

#[cfg(windows)]
mod canary {
    use super::*;
    use crate::remote::helper::AskView;
    use crate::remote::sites::{AuthMethod, SecretRef, SiteProfile, SiteStore};

    const CANARY: &str = "Canary-Pw-9917-wXyZqLm";

    fn forms(c: &str) -> Vec<Vec<u8>> {
        use base64::Engine;
        let raw = c.as_bytes().to_vec();
        let mut v = vec![raw.clone(), c.encode_utf16().flat_map(|u| u.to_le_bytes()).collect()];
        v.push(raw.iter().map(|b| format!("{b:02x}")).collect::<String>().into_bytes());
        v.push(raw.iter().map(|b| format!("{b:02X}")).collect::<String>().into_bytes());
        for pad in 0..3 {
            let mut s = vec![b'x'; pad];
            s.extend_from_slice(&raw);
            let enc = base64::engine::general_purpose::STANDARD.encode(&s);
            v.push(enc.as_bytes()[4..enc.len() - 4].to_vec());
        }
        v
    }

    fn assert_clean(where_: &str, hay: &[u8]) {
        for f in forms(CANARY) {
            assert!(!hay.windows(f.len()).any(|w| w == f.as_slice()), "canary found in {where_}");
        }
    }

    /// The canary goes through the whole cycle (typed, kept in the REAL DPAPI vault, answered to the helper once)
    /// and is then looked for in every place that persists or prints: the data directory (vault, `sites.toml`, and
    /// any settings or journal file written next to them) and the Debug text of every type on the path.
    #[test]
    fn canary_never_reaches_disk_sites_toml_or_any_debug_text() {
        let dir = tempfile::tempdir().unwrap();
        let c = Credentials::for_app(dir.path(), false);
        let b = PromptBroker::new();
        b.set_vault(c.clone());
        let mut texts: Vec<String> = Vec::new();

        let b2 = b.clone();
        let h = std::thread::spawn(move || b2.ask("deploy-vps", "vps", AskKind::Password, "pw:", &AtomicBool::new(false), Duration::from_secs(5)));
        let view: AskView = loop {
            if let Some(v) = b.current() {
                break v;
            }
            std::thread::sleep(Duration::from_millis(5));
        };
        texts.push(format!("{view:?}"));
        assert!(b.answer(view.id, Some(CANARY.into())));
        assert_eq!(h.join().unwrap().as_deref(), Some(CANARY), "the helper receives it once");

        let offer = c.connected("deploy-vps").unwrap();
        texts.push(format!("{offer:?}"));
        let id = c.keep("deploy-vps", "vps").unwrap();
        texts.push(format!("{id:?} {c:?}"));
        for e in c.entries().unwrap() {
            texts.push(format!("{e:?}"));
            assert!(!e.label.contains(CANARY) && !e.scope.contains(CANARY));
        }
        // A second connect is answered from the vault (still real DPAPI).
        assert_eq!(c.resolve("deploy-vps", AskKind::Password).as_deref(), Some(CANARY));
        let err = c.keep("nothing", "x").unwrap_err();
        texts.push(format!("{err} {err:?}"));

        // sites.toml carries only the secret ref.
        let mut store = SiteStore::default();
        let mut p = SiteProfile::from_quick("deploy@vps.example.org", AuthMethod::Password, "/").unwrap();
        p.secret_ref = SecretRef(Some(format!("{:032x}", id.as_u128())));
        p.keep_asked = true;
        texts.push(format!("{p:?}"));
        store.upsert(p);
        let sites = dir.path().join("sites.toml");
        store.save(&sites).unwrap();
        assert!(std::fs::read_to_string(&sites).unwrap().contains("secret_ref"));

        c.lock();
        let mut files = 0;
        for e in std::fs::read_dir(dir.path()).unwrap().flatten() {
            assert_clean(&e.file_name().to_string_lossy(), &std::fs::read(e.path()).unwrap());
            files += 1;
        }
        assert!(files >= 2, "vault.bsv and sites.toml were written");
        for t in &texts {
            assert_clean("debug text", t.as_bytes());
        }
        assert!(c.is_locked());
    }
}
