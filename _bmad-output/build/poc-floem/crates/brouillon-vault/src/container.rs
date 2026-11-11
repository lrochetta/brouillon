//! `vault.bsv` v1 on-disk layout (little-endian):
//!
//! ```text
//! header : magic "BSVAULT\0"(8) version u16, flags u16, vault_id[16], write_counter u64,
//!          aead_id u8, kdf_id u8, slot_count u16
//! slots  : slot_count x { kind u8, slot_id u16, data_len u32, data }
//! index  : nonce[24], ct_len u32, ct          (AEAD under HKDF(MK,"index"), AAD = header+slots)
//! records: count u32, count x { record_id u128, version u32, dk_nonce[24], wrapped_dk[48],
//!                               nonce[24], ct_len u32, ct }
//! footer : SHA-256 of everything before (corruption/truncation only, NOT security)
//! ```
//!
//! Header+slot table = the AAD of the index. Each record's AAD binds magic, version, vault id,
//! aead id, record id and record version; the index lists the expected version of each record.

use sha2::{Digest, Sha256};

use crate::codec::{R, W};
use crate::crypto::{NONCE_LEN, TAG_LEN};
use crate::error::VaultError;
use crate::store::{SecretId, SecretKind, SecretMeta, MAX_STR};
use crate::FORMAT_VERSION;

pub(crate) const MAGIC: &[u8; 8] = b"BSVAULT\0";
pub(crate) const AEAD_XCHACHA20POLY1305: u8 = 1;
pub(crate) const KDF_ARGON2ID: u8 = 1;

pub(crate) const MAX_FILE: usize = 4 * 1024 * 1024;
pub(crate) const MAX_RECORD_CT: usize = 64 * 1024 + 64;
pub(crate) const MAX_SECRET: usize = 60 * 1024;
const MAX_INDEX_CT: usize = 1024 * 1024;
const MAX_SLOTS: usize = 8;
pub(crate) const MAX_RECORDS: usize = 4096;
const MAX_SLOT_DATA: usize = 4096;
pub(crate) const WRAPPED_DK_LEN: usize = 32 + TAG_LEN;

#[derive(Clone, Debug)]
pub(crate) struct Slot {
    pub kind: u8,
    pub id: u16,
    pub data: Vec<u8>,
}

#[derive(Clone)]
pub(crate) struct RawRecord {
    pub id: u128,
    pub version: u32,
    pub dk_nonce: [u8; NONCE_LEN],
    pub wrapped_dk: [u8; WRAPPED_DK_LEN],
    pub nonce: [u8; NONCE_LEN],
    pub ct: Vec<u8>,
}

#[derive(Clone)]
pub(crate) struct Container {
    pub flags: u16,
    pub vault_id: [u8; 16],
    pub counter: u64,
    pub aead_id: u8,
    pub kdf_id: u8,
    pub slots: Vec<Slot>,
    pub index_nonce: [u8; NONCE_LEN],
    pub index_ct: Vec<u8>,
    pub records: Vec<RawRecord>,
}

impl Container {
    pub fn empty(vault_id: [u8; 16], slots: Vec<Slot>) -> Self {
        Container {
            flags: 0,
            vault_id,
            counter: 0,
            aead_id: AEAD_XCHACHA20POLY1305,
            kdf_id: KDF_ARGON2ID,
            slots,
            index_nonce: [0; NONCE_LEN],
            index_ct: Vec::new(),
            records: Vec::new(),
        }
    }

    /// Header + slot table: the AAD of the index.
    pub fn header_bytes(&self) -> Vec<u8> {
        let mut w = W::default();
        w.raw(MAGIC);
        w.u16(FORMAT_VERSION);
        w.u16(self.flags);
        w.raw(&self.vault_id);
        w.u64(self.counter);
        w.u8(self.aead_id);
        w.u8(self.kdf_id);
        w.u16(self.slots.len() as u16);
        for s in &self.slots {
            w.u8(s.kind);
            w.u16(s.id);
            w.u32(s.data.len() as u32);
            w.raw(&s.data);
        }
        w.0
    }

    /// Per-vault constant part of every record AAD.
    pub fn base_aad(&self) -> Vec<u8> {
        let mut w = W::default();
        w.raw(MAGIC);
        w.u16(FORMAT_VERSION);
        w.raw(&self.vault_id);
        w.u8(self.aead_id);
        w.0
    }

    pub fn encode(&self) -> Vec<u8> {
        let mut w = W(self.header_bytes());
        w.raw(&self.index_nonce);
        w.u32(self.index_ct.len() as u32);
        w.raw(&self.index_ct);
        w.u32(self.records.len() as u32);
        for r in &self.records {
            w.u128(r.id);
            w.u32(r.version);
            w.raw(&r.dk_nonce);
            w.raw(&r.wrapped_dk);
            w.raw(&r.nonce);
            w.u32(r.ct.len() as u32);
            w.raw(&r.ct);
        }
        let digest = Sha256::digest(&w.0);
        w.raw(&digest);
        w.0
    }

    pub fn decode(bytes: &[u8]) -> Result<Container, VaultError> {
        if bytes.len() > MAX_FILE {
            return Err(VaultError::TooLarge);
        }
        if bytes.len() < MAGIC.len() + 32 {
            return Err(VaultError::Corrupt);
        }
        if &bytes[..8] != MAGIC {
            return Err(VaultError::Corrupt);
        }
        let (body, footer) = bytes.split_at(bytes.len() - 32);
        if Sha256::digest(body).as_slice() != footer {
            return Err(VaultError::Corrupt);
        }
        let mut r = R::new(body);
        r.take(8)?;
        let version = r.u16()?;
        if version == 0 {
            return Err(VaultError::Corrupt);
        }
        if version > FORMAT_VERSION {
            return Err(VaultError::UnsupportedVersion);
        }
        let flags = r.u16()?;
        let vault_id = r.arr::<16>()?;
        let counter = r.u64()?;
        let aead_id = r.u8()?;
        let kdf_id = r.u8()?;
        if aead_id != AEAD_XCHACHA20POLY1305 || kdf_id != KDF_ARGON2ID {
            return Err(VaultError::UnsupportedAlgorithm);
        }
        let slot_count = r.u16()? as usize;
        if slot_count > MAX_SLOTS {
            return Err(VaultError::Corrupt);
        }
        let mut slots: Vec<Slot> = Vec::with_capacity(slot_count);
        for _ in 0..slot_count {
            let kind = r.u8()?;
            let id = r.u16()?;
            let n = r.u32()? as usize;
            if n > MAX_SLOT_DATA {
                return Err(VaultError::Corrupt);
            }
            if slots.iter().any(|s| s.id == id) {
                return Err(VaultError::Corrupt);
            }
            slots.push(Slot { kind, id, data: r.take(n)?.to_vec() });
        }
        let index_nonce = r.arr::<NONCE_LEN>()?;
        let n = r.u32()? as usize;
        if n > MAX_INDEX_CT {
            return Err(VaultError::Corrupt);
        }
        let index_ct = r.take(n)?.to_vec();
        let count = r.u32()? as usize;
        if count > MAX_RECORDS {
            return Err(VaultError::Corrupt);
        }
        let mut records = Vec::with_capacity(count);
        for _ in 0..count {
            let id = r.u128()?;
            let version = r.u32()?;
            let dk_nonce = r.arr::<NONCE_LEN>()?;
            let wrapped_dk = r.arr::<WRAPPED_DK_LEN>()?;
            let nonce = r.arr::<NONCE_LEN>()?;
            let n = r.u32()? as usize;
            if n > MAX_RECORD_CT {
                return Err(VaultError::Corrupt);
            }
            records.push(RawRecord { id, version, dk_nonce, wrapped_dk, nonce, ct: r.take(n)?.to_vec() });
        }
        if r.remaining() != 0 {
            return Err(VaultError::Corrupt);
        }
        Ok(Container { flags, vault_id, counter, aead_id, kdf_id, slots, index_nonce, index_ct, records })
    }
}

/// Record AAD for the secret payload (`tag = b"rec"`) or the wrapped data key (`tag = b"dk"`).
pub(crate) fn record_aad(base: &[u8], tag: &[u8], id: u128, version: u32) -> Vec<u8> {
    let mut w = W::default();
    w.raw(tag);
    w.raw(base);
    w.u128(id);
    w.u32(version);
    w.0
}

#[derive(Clone, Debug)]
pub(crate) struct IndexEntry {
    pub id: u128,
    pub version: u32,
    pub kind: SecretKind,
    pub created: u64,
    pub last_used: u64,
    pub label: String,
    pub scope: String,
    pub used_by: String,
}

impl IndexEntry {
    pub fn meta(&self) -> SecretMeta {
        SecretMeta {
            id: SecretId(self.id),
            kind: self.kind,
            label: self.label.clone(),
            scope: self.scope.clone(),
            created: self.created,
            last_used: self.last_used,
            used_by: self.used_by.clone(),
        }
    }
}

pub(crate) struct Index {
    /// Mirror of the header counter, authenticated by the AEAD.
    pub counter: u64,
    pub entries: Vec<IndexEntry>,
}

impl Index {
    pub fn encode(&self) -> Vec<u8> {
        let mut w = W::default();
        w.u64(self.counter);
        w.u32(self.entries.len() as u32);
        for e in &self.entries {
            w.u128(e.id);
            w.u32(e.version);
            w.u8(e.kind.to_u8());
            w.u64(e.created);
            w.u64(e.last_used);
            w.str(&e.label);
            w.str(&e.scope);
            w.str(&e.used_by);
        }
        w.0
    }

    pub fn decode(b: &[u8]) -> Result<Index, VaultError> {
        let mut r = R::new(b);
        let counter = r.u64()?;
        let n = r.u32()? as usize;
        if n > MAX_RECORDS {
            return Err(VaultError::Corrupt);
        }
        let mut entries = Vec::with_capacity(n);
        for _ in 0..n {
            let id = r.u128()?;
            let version = r.u32()?;
            let kind = SecretKind::from_u8(r.u8()?)?;
            let created = r.u64()?;
            let last_used = r.u64()?;
            let label = r.str()?;
            let scope = r.str()?;
            let used_by = r.str()?;
            if label.len() > MAX_STR || scope.len() > MAX_STR {
                return Err(VaultError::Corrupt);
            }
            entries.push(IndexEntry { id, version, kind, created, last_used, label, scope, used_by });
        }
        if r.remaining() != 0 {
            return Err(VaultError::Corrupt);
        }
        Ok(Index { counter, entries })
    }
}

/// Payload padding: u32 length + data, zero padded to a multiple of 64 bytes (hides exact length).
pub(crate) fn pad(secret: &[u8]) -> zeroize::Zeroizing<Vec<u8>> {
    let mut v = zeroize::Zeroizing::new(Vec::with_capacity(4 + secret.len() + 64));
    v.extend_from_slice(&(secret.len() as u32).to_le_bytes());
    v.extend_from_slice(secret);
    let padded = v.len().div_ceil(64) * 64;
    v.resize(padded, 0);
    v
}

pub(crate) fn unpad(p: &[u8]) -> Result<zeroize::Zeroizing<Vec<u8>>, VaultError> {
    if p.len() < 4 {
        return Err(VaultError::Corrupt);
    }
    let n = u32::from_le_bytes([p[0], p[1], p[2], p[3]]) as usize;
    if n > p.len() - 4 {
        return Err(VaultError::Corrupt);
    }
    Ok(zeroize::Zeroizing::new(p[4..4 + n].to_vec()))
}
