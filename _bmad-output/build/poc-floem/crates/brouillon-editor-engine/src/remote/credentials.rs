//! `Credentials`: the one service between the remote prompts and the secret vault.
//!
//! Rules (decisions Q1, Q2, Q15 in internal design notes (not published)):
//! * "Use it this time only" is the default. A typed password is held here ONLY between the moment the
//!   user typed it and the first connect result; it is kept in the vault only if the user chose to save it
//!   AFTER a successful connect. On a failed connect, a cancel or a lock it is zeroized and dropped.
//! * No plaintext fallback, ever. If the vault or DPAPI is unavailable (or portable mode refuses a
//!   machine-bound key) nothing is stored and the user is asked every time.
//! * Lazy unlock: the vault is opened on the first real use (a saved password to read, a password to keep,
//!   the credentials view), never at startup and never for a site that has no saved password.
//! * Reads go through `with_secret` only; the value is copied into the answer the helper receives once.
//! * Nothing here logs, prints or `Debug`-formats a value, a label or a path.
//!
//! The vault lives in `brouillon-vault`; this crate-level module is the only place the editor touches it.
//! `brouillon-mcp` must never depend on either.

use std::collections::{HashMap, HashSet};
use std::fmt;
use std::path::Path;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use brouillon_vault::{
    FileVault, LockState, NewSecret, Purpose, Secret, SecretKind, SecretMeta, SecretStore, SecretStoreExt, Unlock, VaultError, VaultPolicy,
    VAULT_FILE_NAME,
};

pub use brouillon_vault::SecretId;
use super::helper::{AskKind, PromptVault};

/// Saved passwords re-lock after this much idle time (the vault's own one-shot thread does it).
pub const IDLE_LOCK: Duration = Duration::from_secs(15 * 60);

/// What the "Keep this password?" step needs to know. Carries no secret.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct KeepOffer {
    pub site_id: String,
    pub kind: AskKind,
    /// The previously saved password was rejected: the step is "replace the saved password".
    pub replace: bool,
}

/// Metadata of one saved secret, for the read-only view. There is no value field by construction.
#[derive(Clone, PartialEq, Eq)]
pub struct SavedEntry {
    pub id: SecretId,
    pub label: String,
    pub kind: &'static str,
    pub scope: String,
    /// The site this belongs to (`remote/<site>/<purpose>` scopes).
    pub site_id: Option<String>,
    pub created: u64,
    /// 0 = never used.
    pub last_used: u64,
}

impl fmt::Debug for SavedEntry {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SavedEntry").field("kind", &self.kind).field("label_len", &self.label.len()).field("created", &self.created).finish()
    }
}

#[derive(Default)]
struct State {
    /// Typed this session, not yet proven by a connect. Zeroized on drop.
    pending: HashMap<String, (AskKind, Secret)>,
    /// Sites with a saved password (from `sites.toml`'s secret ref): the only ones the vault is opened for.
    known: HashSet<String>,
    /// A saved password was handed to the helper for the connect in flight.
    used_saved: HashSet<String>,
    /// The saved password was rejected this session: ask instead of trying it again.
    stale: HashSet<String>,
    /// The vault cannot be used on this machine (Q1): ask every time.
    unavailable: Option<&'static str>,
}

pub struct Credentials {
    store: Arc<dyn SecretStore>,
    /// False in portable mode: nothing is ever kept (no machine-bound key there).
    can_keep: bool,
    st: Mutex<State>,
}

impl fmt::Debug for Credentials {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let st = self.st();
        f.debug_struct("Credentials").field("can_keep", &self.can_keep).field("pending", &st.pending.len()).field("sites", &st.known.len()).finish()
    }
}

pub fn kind_of(k: AskKind) -> SecretKind {
    if k == AskKind::Passphrase {
        SecretKind::KeyPassphrase
    } else {
        SecretKind::Password
    }
}

fn purpose_tag(k: AskKind) -> &'static str {
    if k == AskKind::Passphrase {
        "passphrase"
    } else {
        "password"
    }
}

/// `remote/<site-id>/<password|passphrase>`
pub fn scope_for(site_id: &str, kind: AskKind) -> String {
    format!("remote/{site_id}/{}", purpose_tag(kind))
}

fn site_of_scope(scope: &str) -> Option<String> {
    let rest = scope.strip_prefix("remote/")?;
    rest.rsplit_once('/').map(|(site, _)| site.to_string())
}

fn kind_label(k: SecretKind) -> &'static str {
    match k {
        SecretKind::Password => "password",
        SecretKind::KeyPassphrase => "key passphrase",
        SecretKind::OAuthRefresh => "sign-in",
        SecretKind::ApiKey => "AI key",
        SecretKind::LinkToken => "link",
        SecretKind::Other => "other",
    }
}

impl Credentials {
    /// Any store (tests use `MemVault`). `can_keep = false` means "ask every time".
    pub fn with_store(store: Arc<dyn SecretStore>, can_keep: bool) -> Arc<Self> {
        Arc::new(Self { store, can_keep, st: Mutex::new(State::default()) })
    }

    /// The app wiring: `vault.bsv` under `root`, Standard mode (DPAPI), idle re-lock armed. In portable
    /// mode the vault policy refuses a machine-bound key and nothing is kept (ask every time).
    pub fn for_app(root: &Path, portable: bool) -> Arc<Self> {
        let vault = FileVault::new(root.join(VAULT_FILE_NAME), VaultPolicy::for_portable(portable));
        vault.set_idle_lock(Some(IDLE_LOCK));
        Self::with_store(Arc::new(vault), !portable)
    }

    fn st(&self) -> MutexGuard<'_, State> {
        self.st.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Whether a password could be kept at all right now (portable mode and a failed vault say no).
    pub fn keep_possible(&self) -> bool {
        self.can_keep && self.st().unavailable.is_none()
    }

    /// Plain words for the "asked every time" state, if it applies.
    pub fn unavailable_note(&self) -> Option<&'static str> {
        if !self.can_keep {
            return Some("Portable mode keeps no saved passwords. Brouillon will ask each time.");
        }
        self.st().unavailable.map(|_| "This computer has no safe place for passwords. Brouillon will ask each time and keep nothing.")
    }

    /// Feed the sites that have a saved password (the `secret_ref` of each profile). Replaces the set.
    pub fn set_saved_sites<'a>(&self, sites: impl IntoIterator<Item = &'a str>) {
        self.st().known = sites.into_iter().map(String::from).collect();
    }

    pub fn is_saved(&self, site_id: &str) -> bool {
        self.st().known.contains(site_id)
    }

    /// Lazy unlock. A vault that cannot work here turns the service into "ask every time" for good.
    fn open(&self) -> Result<(), VaultError> {
        if matches!(self.store.state(), LockState::Unlocked { .. }) {
            return Ok(());
        }
        match self.store.unlock(Unlock::Os) {
            Ok(()) => Ok(()),
            Err(e) => {
                if e.means_ask_every_time() {
                    self.st().unavailable = Some("vault unavailable");
                }
                Err(e)
            }
        }
    }

    fn find(&self, site_id: &str, kind: AskKind) -> Result<Option<SecretMeta>, VaultError> {
        self.open()?;
        let scope = scope_for(site_id, kind);
        Ok(self.store.list()?.into_iter().find(|m| m.scope == scope))
    }

    // ---------------------------------------------------------------- the connect cycle

    /// After the connect succeeded: the offer to keep what the user just typed, if there is one.
    /// The caller must answer with `keep` or `discard` (the typed password stays in memory until then).
    pub fn connected(&self, site_id: &str) -> Option<KeepOffer> {
        let mut st = self.st();
        st.used_saved.remove(site_id);
        let replace = st.stale.contains(site_id);
        let (kind, _) = st.pending.get(site_id)?;
        Some(KeepOffer { site_id: site_id.into(), kind: *kind, replace })
    }

    /// After the connect failed: a typed password is dropped (never offer to keep a typo). Returns true
    /// when a SAVED password had just been tried: the caller says so and the next connect asks.
    pub fn failed(&self, site_id: &str) -> bool {
        let mut st = self.st();
        st.pending.remove(site_id);
        let was_saved = st.used_saved.remove(site_id);
        if was_saved {
            st.stale.insert(site_id.into());
        }
        was_saved
    }

    /// The user chose "use this time only" (or Esc): forget what was typed.
    pub fn discard(&self, site_id: &str) {
        self.st().pending.remove(site_id);
    }

    /// The user chose "save on this computer". Needs the vault (lazy unlock). On `Err` nothing was stored;
    /// `VaultError::means_ask_every_time` tells the caller to say so in plain words.
    pub fn keep(&self, site_id: &str, label: &str) -> Result<SecretId, VaultError> {
        let (kind, secret) = self.st().pending.remove(site_id).ok_or(VaultError::NotFound)?;
        if !self.can_keep {
            return Err(VaultError::Policy("portable mode keeps no saved passwords"));
        }
        let scope = scope_for(site_id, kind);
        let id = match self.find(site_id, kind)? {
            Some(m) => {
                self.store.replace(m.id, secret)?;
                m.id
            }
            None => self.store.put(NewSecret { kind: kind_of(kind), label: label.into(), scope }, secret)?,
        };
        let mut st = self.st();
        st.known.insert(site_id.into());
        st.stale.remove(site_id);
        Ok(id)
    }

    /// "Forget this password": every secret of this site. Idempotent.
    pub fn forget(&self, site_id: &str) -> Result<usize, VaultError> {
        let prefix = format!("remote/{site_id}/");
        let had = self.st().known.contains(site_id);
        {
            let mut st = self.st();
            st.pending.remove(site_id);
            st.stale.remove(site_id);
            st.used_saved.remove(site_id);
        }
        if !had {
            return Ok(0);
        }
        self.open()?;
        let mut n = 0;
        for m in self.store.list()?.into_iter().filter(|m| m.scope.starts_with(&prefix)) {
            self.store.delete(m.id)?;
            n += 1;
        }
        self.st().known.remove(site_id);
        Ok(n)
    }

    // ---------------------------------------------------------------- the read-only view

    /// Metadata only. Empty (and the vault untouched) while no site has a saved password.
    pub fn entries(&self) -> Result<Vec<SavedEntry>, VaultError> {
        if self.st().known.is_empty() {
            return Ok(Vec::new());
        }
        self.open()?;
        let mut v: Vec<SavedEntry> = self
            .store
            .list()?
            .into_iter()
            .map(|m| SavedEntry {
                site_id: site_of_scope(&m.scope),
                id: m.id,
                label: m.label,
                kind: kind_label(m.kind),
                scope: m.scope,
                created: m.created,
                last_used: m.last_used,
            })
            .collect();
        v.sort_by(|a, b| a.label.to_lowercase().cmp(&b.label.to_lowercase()));
        Ok(v)
    }

    /// Delete one entry; returns the site it belonged to so the caller can clear that site's saved state.
    pub fn delete_entry(&self, id: SecretId) -> Result<Option<String>, VaultError> {
        self.open()?;
        let site = self.store.list()?.into_iter().find(|m| m.id == id).and_then(|m| site_of_scope(&m.scope));
        self.store.delete(id)?;
        if let Some(s) = &site {
            // Another entry of the same site (password and passphrase) may remain.
            let prefix = format!("remote/{s}/");
            let left = self.store.list()?.iter().any(|m| m.scope.starts_with(&prefix));
            if !left {
                self.st().known.remove(s);
            }
        }
        Ok(site)
    }

    // ---------------------------------------------------------------- locking

    /// Zeroize the vault key and drop everything typed and not yet proven. Idempotent. Called by the kill
    /// switch and by the Panic command.
    pub fn lock(&self) {
        self.store.lock();
        let mut st = self.st();
        st.pending.clear();
        st.used_saved.clear();
    }

    pub fn is_locked(&self) -> bool {
        !matches!(self.store.state(), LockState::Unlocked { .. })
    }
}

impl PromptVault for Credentials {
    /// Helper thread: answer a password/passphrase question from the vault, once per connect.
    fn resolve(&self, site_id: &str, kind: AskKind) -> Option<String> {
        {
            let mut st = self.st();
            if !st.known.contains(site_id) || st.stale.contains(site_id) {
                return None;
            }
            // Asked again in the same connect: the saved one was wrong. Never retry it.
            if st.used_saved.remove(site_id) {
                st.stale.insert(site_id.into());
                return None;
            }
        }
        let meta = self.find(site_id, kind).ok().flatten()?;
        let answer = self.store.with_secret(meta.id, Purpose::RemoteLogin, |s| s.expose_str().ok().map(String::from)).ok().flatten()?;
        self.st().used_saved.insert(site_id.into());
        Some(answer)
    }

    /// UI thread, at answer time: remember what was typed until the connect result decides.
    fn typed(&self, site_id: &str, kind: AskKind, answer: &str) {
        if answer.is_empty() || !self.keep_possible() {
            return;
        }
        self.st().pending.insert(site_id.into(), (kind, Secret::from_bytes(answer.as_bytes())));
    }
}
