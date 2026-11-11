use std::time::Instant;

use crate::error::VaultError;
use crate::secret::Secret;

/// Seconds since the Unix epoch.
pub type Timestamp = u64;

pub(crate) fn now() -> Timestamp {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Opaque 128-bit random id. Loggable; never derived from the label.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub struct SecretId(pub(crate) u128);

impl SecretId {
    pub fn as_u128(self) -> u128 {
        self.0
    }
    pub fn from_u128(v: u128) -> Self {
        SecretId(v)
    }
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum SecretKind {
    Password,
    KeyPassphrase,
    OAuthRefresh,
    ApiKey,
    LinkToken,
    Other,
}

impl SecretKind {
    pub(crate) fn to_u8(self) -> u8 {
        match self {
            SecretKind::Password => 1,
            SecretKind::KeyPassphrase => 2,
            SecretKind::OAuthRefresh => 3,
            SecretKind::ApiKey => 4,
            SecretKind::LinkToken => 5,
            SecretKind::Other => 6,
        }
    }
    pub(crate) fn from_u8(v: u8) -> Result<Self, VaultError> {
        Ok(match v {
            1 => SecretKind::Password,
            2 => SecretKind::KeyPassphrase,
            3 => SecretKind::OAuthRefresh,
            4 => SecretKind::ApiKey,
            5 => SecretKind::LinkToken,
            6 => SecretKind::Other,
            _ => return Err(VaultError::Corrupt),
        })
    }
}

/// Why a secret is being read. Drives policy and (later) the audit log.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Purpose {
    RemoteLogin,
    OAuthRefresh,
    AiRequest,
    LinkHandshake,
    Export,
}

impl Purpose {
    pub(crate) fn tag(self) -> &'static str {
        match self {
            Purpose::RemoteLogin => "remote-login",
            Purpose::OAuthRefresh => "oauth-refresh",
            Purpose::AiRequest => "ai-request",
            Purpose::LinkHandshake => "link-handshake",
            Purpose::Export => "export",
        }
    }
}

/// `LinkToken` secrets may only be read for the Link handshake.
pub(crate) fn purpose_allowed(kind: SecretKind, purpose: Purpose) -> Result<(), VaultError> {
    if kind == SecretKind::LinkToken && purpose != Purpose::LinkHandshake {
        return Err(VaultError::Policy("link tokens are only readable for the link handshake"));
    }
    Ok(())
}

/// Non-secret descriptor; readable without decrypting any value (it does need the vault unlocked,
/// because labels are mildly sensitive and stored encrypted).
#[derive(Clone, PartialEq, Eq)]
pub struct SecretMeta {
    pub id: SecretId,
    pub kind: SecretKind,
    pub label: String,
    /// Namespace such as `remote/<site-uuid>`, `oauth/gdrive/<account>`, `ai/<provider>`.
    pub scope: String,
    pub created: Timestamp,
    /// 0 = never used.
    pub last_used: Timestamp,
    /// Purpose tag of the last use (e.g. `remote-login`), empty if never used.
    pub used_by: String,
}

#[derive(Clone, PartialEq, Eq)]
pub struct NewSecret {
    pub kind: SecretKind,
    pub label: String,
    pub scope: String,
}

// Labels name sites and accounts: mildly sensitive, so `Debug` (logs, panics) shows lengths only.
impl std::fmt::Debug for SecretMeta {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SecretMeta")
            .field("id", &self.id)
            .field("kind", &self.kind)
            .field("label_len", &self.label.len())
            .field("scope_len", &self.scope.len())
            .field("created", &self.created)
            .field("last_used", &self.last_used)
            .field("used_by", &self.used_by)
            .finish()
    }
}

impl std::fmt::Debug for NewSecret {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NewSecret").field("kind", &self.kind).field("label_len", &self.label.len()).finish()
    }
}

pub(crate) const MAX_STR: usize = 1024;

impl NewSecret {
    pub(crate) fn check(&self) -> Result<(), VaultError> {
        if self.label.len() > MAX_STR || self.scope.len() > MAX_STR {
            return Err(VaultError::TooLarge);
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LockState {
    Locked,
    /// `until` is the idle deadline when idle re-lock is armed.
    Unlocked { until: Option<Instant> },
    Unavailable(&'static str),
}

pub enum Unlock<'a> {
    /// Unwrap through the OS keyslot (DPAPI). Creates the vault file on first use.
    Os,
    /// Reserved: Argon2id passphrase slot.
    Passphrase(&'a Secret),
    /// Reserved: recovery key slot.
    Recovery(&'a Secret),
}

/// Machine-binding policy. Portable mode must never silently bind to the machine (rule V8).
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct VaultPolicy {
    pub allow_machine_bound: bool,
}

impl VaultPolicy {
    /// Pass `Paths::is_portable()`.
    pub fn for_portable(portable: bool) -> Self {
        VaultPolicy { allow_machine_bound: !portable }
    }
}

impl Default for VaultPolicy {
    fn default() -> Self {
        VaultPolicy { allow_machine_bound: true }
    }
}

pub trait SecretStore: Send + Sync {
    fn state(&self) -> LockState;
    fn unlock(&self, how: Unlock<'_>) -> Result<(), VaultError>;
    /// Zeroizes key material. Idempotent.
    fn lock(&self);
    fn put(&self, meta: NewSecret, value: Secret) -> Result<SecretId, VaultError>;
    /// New data key and nonce; keeps id and metadata.
    fn replace(&self, id: SecretId, value: Secret) -> Result<(), VaultError>;
    /// Removes the record and its wrapped data key.
    fn delete(&self, id: SecretId) -> Result<(), VaultError>;
    fn list(&self) -> Result<Vec<SecretMeta>, VaultError>;
    /// Object-safe form of [`SecretStore::with_secret`]: the ONLY way to read a value.
    fn with_secret_dyn(
        &self,
        id: SecretId,
        purpose: Purpose,
        f: &mut dyn FnMut(&Secret),
    ) -> Result<(), VaultError>;
}

/// Closure-scoped read: the plaintext lives only for the call and is zeroized right after.
/// Blanket-implemented for every store, including `dyn SecretStore`.
pub trait SecretStoreExt: SecretStore {
    fn with_secret<R>(
        &self,
        id: SecretId,
        purpose: Purpose,
        f: impl FnOnce(&Secret) -> R,
    ) -> Result<R, VaultError> {
        let mut f = Some(f);
        let mut out = None;
        self.with_secret_dyn(id, purpose, &mut |s| {
            if let Some(f) = f.take() {
                out = Some(f(s));
            }
        })?;
        out.ok_or(VaultError::NotFound)
    }
}

impl<T: SecretStore + ?Sized> SecretStoreExt for T {}
