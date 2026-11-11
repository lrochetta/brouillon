//! Test-only stand-in for the OS keyslot. NOT secure: the wrapping key lives in the caller's
//! memory next to the vault. Compiled only for tests or with the `test-util` feature.

use crate::crypto::*;
use crate::error::VaultError;
use crate::os::{OsWrap, SLOT_DPAPI};

pub struct SoftWrap {
    key: [u8; KEY_LEN],
}

impl SoftWrap {
    pub fn new() -> Self {
        SoftWrap { key: random::<KEY_LEN>().expect("rng") }
    }
}

impl Default for SoftWrap {
    fn default() -> Self {
        Self::new()
    }
}

impl OsWrap for SoftWrap {
    fn slot_kind(&self) -> u8 {
        SLOT_DPAPI
    }
    fn machine_bound(&self) -> bool {
        true
    }
    fn wrap(&self, mk: &[u8; KEY_LEN], entropy: &[u8]) -> Result<Vec<u8>, VaultError> {
        let n = random::<NONCE_LEN>()?;
        let mut out = n.to_vec();
        out.extend(seal(&self.key, &n, mk, entropy));
        Ok(out)
    }
    fn unwrap(&self, blob: &[u8], entropy: &[u8]) -> Result<Key32, VaultError> {
        if blob.len() < NONCE_LEN {
            return Err(VaultError::Corrupt);
        }
        let n: [u8; NONCE_LEN] = blob[..NONCE_LEN].try_into().unwrap();
        let p = open(&self.key, &n, &blob[NONCE_LEN..], entropy)
            .map_err(|_| VaultError::Unavailable("soft unwrap failed"))?;
        if p.len() != KEY_LEN {
            return Err(VaultError::Corrupt);
        }
        let mut k = zeroize::Zeroizing::new([0u8; KEY_LEN]);
        k.copy_from_slice(&p);
        Ok(k)
    }
}

/// A key wrap that always fails, to prove there is no plaintext fallback.
pub struct FailingWrap;

impl OsWrap for FailingWrap {
    fn slot_kind(&self) -> u8 {
        SLOT_DPAPI
    }
    fn machine_bound(&self) -> bool {
        true
    }
    fn wrap(&self, _: &[u8; KEY_LEN], _: &[u8]) -> Result<Vec<u8>, VaultError> {
        Err(VaultError::Unavailable("simulated DPAPI failure"))
    }
    fn unwrap(&self, _: &[u8], _: &[u8]) -> Result<Key32, VaultError> {
        Err(VaultError::Unavailable("simulated DPAPI failure"))
    }
}
