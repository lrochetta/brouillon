use chacha20poly1305::aead::{Aead, KeyInit, Payload};
use chacha20poly1305::{XChaCha20Poly1305, XNonce};
use hkdf::Hkdf;
use sha2::Sha256;
use zeroize::Zeroizing;

use crate::error::VaultError;

pub(crate) const KEY_LEN: usize = 32;
pub(crate) const NONCE_LEN: usize = 24;
pub(crate) const TAG_LEN: usize = 16;

pub(crate) type Key32 = Zeroizing<[u8; KEY_LEN]>;

pub(crate) fn random<const N: usize>() -> Result<[u8; N], VaultError> {
    let mut b = [0u8; N];
    getrandom::fill(&mut b).map_err(|_| VaultError::Unavailable("random source"))?;
    Ok(b)
}

pub(crate) fn random_key() -> Result<Key32, VaultError> {
    Ok(Zeroizing::new(random::<KEY_LEN>()?))
}

pub(crate) fn seal(key: &[u8; KEY_LEN], nonce: &[u8; NONCE_LEN], plaintext: &[u8], aad: &[u8]) -> Vec<u8> {
    let c = XChaCha20Poly1305::new(key.into());
    c.encrypt(XNonce::from_slice(nonce), Payload { msg: plaintext, aad })
        .expect("XChaCha20-Poly1305 encryption cannot fail for in-memory buffers")
}

pub(crate) fn open(
    key: &[u8; KEY_LEN],
    nonce: &[u8; NONCE_LEN],
    ct: &[u8],
    aad: &[u8],
) -> Result<Zeroizing<Vec<u8>>, VaultError> {
    let c = XChaCha20Poly1305::new(key.into());
    c.decrypt(XNonce::from_slice(nonce), Payload { msg: ct, aad })
        .map(Zeroizing::new)
        .map_err(|_| VaultError::Tampered)
}

/// HKDF-SHA256 sub-key, domain separated by `info`.
pub(crate) fn subkey(mk: &[u8; KEY_LEN], info: &[u8]) -> Key32 {
    let hk = Hkdf::<Sha256>::new(None, mk);
    let mut okm = Zeroizing::new([0u8; KEY_LEN]);
    hk.expand(info, okm.as_mut()).expect("32 bytes is a valid HKDF-SHA256 output length");
    okm
}

pub(crate) const INFO_INDEX: &[u8] = b"brouillon-vault/v1/index";
pub(crate) const INFO_DK_WRAP: &[u8] = b"brouillon-vault/v1/dk-wrap";
