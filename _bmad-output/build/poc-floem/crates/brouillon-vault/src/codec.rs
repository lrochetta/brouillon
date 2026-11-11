//! Tiny bounds-checked binary codec (little-endian, length-prefixed).

use crate::error::VaultError;

#[derive(Default)]
pub(crate) struct W(pub Vec<u8>);

impl W {
    pub fn u8(&mut self, v: u8) {
        self.0.push(v);
    }
    pub fn u16(&mut self, v: u16) {
        self.0.extend_from_slice(&v.to_le_bytes());
    }
    pub fn u32(&mut self, v: u32) {
        self.0.extend_from_slice(&v.to_le_bytes());
    }
    pub fn u64(&mut self, v: u64) {
        self.0.extend_from_slice(&v.to_le_bytes());
    }
    pub fn u128(&mut self, v: u128) {
        self.0.extend_from_slice(&v.to_le_bytes());
    }
    pub fn raw(&mut self, b: &[u8]) {
        self.0.extend_from_slice(b);
    }
    pub fn str(&mut self, s: &str) {
        self.u16(s.len() as u16);
        self.raw(s.as_bytes());
    }
}

pub(crate) struct R<'a> {
    b: &'a [u8],
    pos: usize,
}

impl<'a> R<'a> {
    pub fn new(b: &'a [u8]) -> Self {
        R { b, pos: 0 }
    }
    pub fn remaining(&self) -> usize {
        self.b.len() - self.pos
    }
    pub fn take(&mut self, n: usize) -> Result<&'a [u8], VaultError> {
        if n > self.remaining() {
            return Err(VaultError::Corrupt);
        }
        let s = &self.b[self.pos..self.pos + n];
        self.pos += n;
        Ok(s)
    }
    pub fn arr<const N: usize>(&mut self) -> Result<[u8; N], VaultError> {
        let mut a = [0u8; N];
        a.copy_from_slice(self.take(N)?);
        Ok(a)
    }
    pub fn u8(&mut self) -> Result<u8, VaultError> {
        Ok(self.take(1)?[0])
    }
    pub fn u16(&mut self) -> Result<u16, VaultError> {
        Ok(u16::from_le_bytes(self.arr()?))
    }
    pub fn u32(&mut self) -> Result<u32, VaultError> {
        Ok(u32::from_le_bytes(self.arr()?))
    }
    pub fn u64(&mut self) -> Result<u64, VaultError> {
        Ok(u64::from_le_bytes(self.arr()?))
    }
    pub fn u128(&mut self) -> Result<u128, VaultError> {
        Ok(u128::from_le_bytes(self.arr()?))
    }
    pub fn str(&mut self) -> Result<String, VaultError> {
        let n = self.u16()? as usize;
        String::from_utf8(self.take(n)?.to_vec()).map_err(|_| VaultError::Corrupt)
    }
}
