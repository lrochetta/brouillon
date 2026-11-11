//! Embedded public keys (list, for rotation). Source: `tools/release/trusted-keys.json`.

use crate::UpdateError;
use serde::Deserialize;
use sha2::{Digest, Sha256};

const EMBEDDED_JSON: &str = include_str!("../../../../../../tools/release/trusted-keys.json");

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrustedKey {
    pub key_id: String,
    pub public: [u8; 32],
}

#[derive(Deserialize)]
struct File {
    keys: Vec<Entry>,
}
#[derive(Deserialize)]
struct Entry {
    key_id: String,
    public_key_hex: String,
}

pub fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

pub fn unhex(s: &str) -> Option<Vec<u8>> {
    let s = s.trim();
    if s.len() % 2 != 0 || !s.is_ascii() {
        return None;
    }
    (0..s.len()).step_by(2).map(|i| u8::from_str_radix(&s[i..i + 2], 16).ok()).collect()
}

pub fn key_id_of(public: &[u8; 32]) -> String {
    hex(&Sha256::digest(public)[..8])
}

pub fn key_from_hex(h: &str) -> Option<TrustedKey> {
    let public: [u8; 32] = unhex(h)?.try_into().ok()?;
    Some(TrustedKey { key_id: key_id_of(&public), public })
}

/// Parse a key list. An entry whose declared `key_id` does not match its key is dropped.
pub fn parse_keys(json: &str) -> Result<Vec<TrustedKey>, UpdateError> {
    let f: File = serde_json::from_str(json).map_err(|e| UpdateError::BadManifest(format!("keys: {e}")))?;
    Ok(f.keys.iter().filter_map(|e| key_from_hex(&e.public_key_hex).filter(|k| k.key_id == e.key_id)).collect())
}

/// Keys compiled into the executable.
pub fn embedded() -> Vec<TrustedKey> {
    parse_keys(EMBEDDED_JSON).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn embedded_list_is_not_empty_and_ids_are_consistent() {
        let k = embedded();
        assert!(!k.is_empty(), "no usable embedded key");
        for key in &k {
            assert_eq!(key.key_id, key_id_of(&key.public));
        }
    }

    #[test]
    fn mismatching_key_id_is_dropped() {
        let j = r#"{"keys":[{"key_id":"0000000000000000","public_key_hex":"9c707ad63c7c04574218973e87b644c44fd8a25770eb8b585d1fae4706fd442d"}]}"#;
        assert!(parse_keys(j).unwrap().is_empty());
    }
}
