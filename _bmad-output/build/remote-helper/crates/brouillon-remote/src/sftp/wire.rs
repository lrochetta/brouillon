//! Primitive encoders/decoders of the SFTP wire format (big-endian, length-prefixed strings).

use super::SftpError;

pub fn put_u8(o: &mut Vec<u8>, v: u8) {
    o.push(v);
}
pub fn put_u32(o: &mut Vec<u8>, v: u32) {
    o.extend_from_slice(&v.to_be_bytes());
}
pub fn put_u64(o: &mut Vec<u8>, v: u64) {
    o.extend_from_slice(&v.to_be_bytes());
}
pub fn put_str(o: &mut Vec<u8>, s: &[u8]) {
    put_u32(o, s.len() as u32);
    o.extend_from_slice(s);
}

/// Bounds-checked reader. Every method fails with `Protocol` instead of panicking or over-allocating.
pub struct Rd<'a> {
    b: &'a [u8],
    pos: usize,
}

fn short() -> SftpError {
    SftpError::Protocol("packet shorter than its fields".into())
}

impl<'a> Rd<'a> {
    pub fn new(b: &'a [u8]) -> Self {
        Rd { b, pos: 0 }
    }
    pub fn remaining(&self) -> usize {
        self.b.len() - self.pos
    }
    pub fn u8(&mut self) -> Result<u8, SftpError> {
        let v = *self.b.get(self.pos).ok_or_else(short)?;
        self.pos += 1;
        Ok(v)
    }
    pub fn u32(&mut self) -> Result<u32, SftpError> {
        let s = self.b.get(self.pos..self.pos + 4).ok_or_else(short)?;
        self.pos += 4;
        Ok(u32::from_be_bytes(s.try_into().unwrap()))
    }
    pub fn u64(&mut self) -> Result<u64, SftpError> {
        let s = self.b.get(self.pos..self.pos + 8).ok_or_else(short)?;
        self.pos += 8;
        Ok(u64::from_be_bytes(s.try_into().unwrap()))
    }
    /// A length-prefixed string; the declared length is checked against what is left *before* slicing.
    pub fn string(&mut self) -> Result<&'a [u8], SftpError> {
        let n = self.u32()? as usize;
        if n > self.remaining() {
            return Err(SftpError::Protocol(format!("string of {n} bytes overruns the packet")));
        }
        let s = &self.b[self.pos..self.pos + n];
        self.pos += n;
        Ok(s)
    }
    pub fn rest(&mut self) -> &'a [u8] {
        let s = &self.b[self.pos..];
        self.pos = self.b.len();
        s
    }
    pub fn finish(&self) -> Result<(), SftpError> {
        if self.remaining() == 0 {
            Ok(())
        } else {
            Err(SftpError::Protocol(format!("{} trailing bytes", self.remaining())))
        }
    }
}
