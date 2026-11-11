use std::collections::HashMap;
use std::fmt;
use std::sync::Mutex;

use crate::crypto::random;
use crate::error::VaultError;
use crate::secret::Secret;
use crate::store::*;

struct Entry {
    meta: SecretMeta,
    value: Secret,
}

/// In-memory store: tests and the "ask every time" mode (secrets live until `lock()`/exit).
/// Starts unlocked. Nothing ever touches the disk.
pub struct MemVault {
    inner: Mutex<Option<HashMap<u128, Entry>>>,
}

impl MemVault {
    pub fn new() -> Self {
        MemVault { inner: Mutex::new(Some(HashMap::new())) }
    }
}

impl Default for MemVault {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Debug for MemVault {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("MemVault(***)")
    }
}

impl SecretStore for MemVault {
    fn state(&self) -> LockState {
        if self.inner.lock().unwrap().is_some() {
            LockState::Unlocked { until: None }
        } else {
            LockState::Locked
        }
    }

    fn unlock(&self, how: Unlock<'_>) -> Result<(), VaultError> {
        match how {
            Unlock::Os => {
                let mut g = self.inner.lock().unwrap();
                if g.is_none() {
                    *g = Some(HashMap::new());
                }
                Ok(())
            }
            _ => Err(VaultError::NotImplemented("passphrase and recovery slots")),
        }
    }

    fn lock(&self) {
        // Dropping each `Secret` zeroizes it.
        *self.inner.lock().unwrap() = None;
    }

    fn put(&self, meta: NewSecret, value: Secret) -> Result<SecretId, VaultError> {
        meta.check()?;
        let mut g = self.inner.lock().unwrap();
        let m = g.as_mut().ok_or(VaultError::Locked)?;
        let mut id = u128::from_le_bytes(random::<16>()?);
        while id == 0 || m.contains_key(&id) {
            id = u128::from_le_bytes(random::<16>()?);
        }
        let sm = SecretMeta {
            id: SecretId(id),
            kind: meta.kind,
            label: meta.label,
            scope: meta.scope,
            created: now(),
            last_used: 0,
            used_by: String::new(),
        };
        m.insert(id, Entry { meta: sm, value });
        Ok(SecretId(id))
    }

    fn replace(&self, id: SecretId, value: Secret) -> Result<(), VaultError> {
        let mut g = self.inner.lock().unwrap();
        let e = g.as_mut().ok_or(VaultError::Locked)?.get_mut(&id.0).ok_or(VaultError::NotFound)?;
        e.value = value;
        Ok(())
    }

    fn delete(&self, id: SecretId) -> Result<(), VaultError> {
        let mut g = self.inner.lock().unwrap();
        g.as_mut().ok_or(VaultError::Locked)?.remove(&id.0).map(|_| ()).ok_or(VaultError::NotFound)
    }

    fn list(&self) -> Result<Vec<SecretMeta>, VaultError> {
        let g = self.inner.lock().unwrap();
        let mut v: Vec<_> = g.as_ref().ok_or(VaultError::Locked)?.values().map(|e| e.meta.clone()).collect();
        v.sort_by_key(|m| (m.created, m.id.0));
        Ok(v)
    }

    fn with_secret_dyn(
        &self,
        id: SecretId,
        purpose: Purpose,
        f: &mut dyn FnMut(&Secret),
    ) -> Result<(), VaultError> {
        let mut g = self.inner.lock().unwrap();
        let e = g.as_mut().ok_or(VaultError::Locked)?.get_mut(&id.0).ok_or(VaultError::NotFound)?;
        purpose_allowed(e.meta.kind, purpose)?;
        e.meta.last_used = now();
        e.meta.used_by = purpose.tag().to_string();
        f(&e.value);
        Ok(())
    }
}
