//! Brouillon secret vault (MVP, "Standard" mode).
//!
//! One file, `vault.bsv`, holding a random 256-bit master key (MK) wrapped by an OS keyslot
//! (Windows DPAPI, user scope), a per-secret random data key (DK) wrapped by MK, and the
//! secret bytes sealed with XChaCha20-Poly1305. The header and the slot table are bound as
//! AAD to the index, and the index/record versions are bound to each record, so slot swaps,
//! downgrades and record shuffles are detected. A monotonic write counter detects rollback.
//!
//! Design: internal design notes (not published) and `00-decisions.md`.
//!
//! Hard rules honoured here:
//! * no plaintext or home-made obfuscation fallback: if the OS wrap fails the vault reports
//!   [`VaultError::Unavailable`] and the caller falls back to "ask every time" (decision Q1);
//! * portable mode never silently machine-binds the vault ([`VaultPolicy`]);
//! * secrets are only readable inside a closure ([`SecretStore::with_secret`]) and the
//!   [`Secret`] type prints `***` everywhere;
//! * this crate has no UI, no network and no logging; it must not be a dependency of
//!   `brouillon-mcp`.
//!
//! Not implemented yet (format-reserved, parsed): passphrase (Argon2id), recovery key, FIDO2,
//! TPM slots; cross-process write lock file; Keychain / Secret Service.

#![deny(unsafe_code)]

mod codec;
mod container;
mod crypto;
mod error;
mod file;
mod fsutil;
mod mem;
mod os;
mod secret;
mod store;

#[cfg(any(test, feature = "test-util"))]
pub mod testing;

#[cfg(test)]
mod tests;

pub use error::VaultError;
pub use file::FileVault;
pub use mem::MemVault;
pub use os::{default_os_wrap, OsWrap, SLOT_DPAPI, SLOT_FIDO2, SLOT_PASSPHRASE, SLOT_RECOVERY, SLOT_TPM};
pub use secret::Secret;
pub use store::{
    LockState, NewSecret, Purpose, SecretId, SecretKind, SecretMeta, SecretStore, SecretStoreExt, Timestamp, Unlock,
    VaultPolicy,
};

/// Newest `vault.bsv` format version this build reads and writes.
pub const FORMAT_VERSION: u16 = 1;
/// Default file name inside the data directory.
pub const VAULT_FILE_NAME: &str = "vault.bsv";
