use std::fmt;
use std::str::Utf8Error;
use zeroize::{Zeroize, Zeroizing};

/// Secret bytes. Zeroized on drop, not `Clone`, not `Serialize`, prints `***` in both
/// `Debug` and `Display`.
pub struct Secret(Zeroizing<Vec<u8>>);

impl Secret {
    pub fn from_bytes(b: &[u8]) -> Self {
        Secret(Zeroizing::new(b.to_vec()))
    }
    pub(crate) fn from_zeroizing(v: Zeroizing<Vec<u8>>) -> Self {
        Secret(v)
    }
    /// Takes ownership of the buffer (no extra copy is made).
    pub fn from_vec(v: Vec<u8>) -> Self {
        Secret(Zeroizing::new(v))
    }
    /// Copies the string; the caller should drop/zeroize its own copy.
    pub fn from_string(s: String) -> Self {
        Secret(Zeroizing::new(s.into_bytes()))
    }
    pub fn expose_bytes(&self) -> &[u8] {
        &self.0
    }
    pub fn expose_str(&self) -> Result<&str, Utf8Error> {
        std::str::from_utf8(&self.0)
    }
    pub fn len(&self) -> usize {
        self.0.len()
    }
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
    /// Deliberate copy, for the rare case where two owners are really needed.
    pub fn duplicate(&self) -> Secret {
        Secret::from_bytes(&self.0)
    }
    /// Zero the bytes now and empty the buffer.
    pub fn wipe(&mut self) {
        self.0.zeroize();
    }
}

impl fmt::Debug for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Secret(***)")
    }
}

impl fmt::Display for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("***")
    }
}
