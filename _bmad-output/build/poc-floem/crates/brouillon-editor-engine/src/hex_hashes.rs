//! Checksums of a byte range, streamed in 1 MiB windows with progress and cancel.
//!
//! CRC-32 (IEEE) and SHA-1 are hand-written here (about 80 lines, tested against published vectors)
//! so no crate is added; MD5 and SHA-256 come from the `md-5` and `sha2` crates already in the
//! workspace. One pass over the range feeds every requested algorithm; memory is one window.
//! MD5 and SHA-1 are offered for comparing against published checksums, not for security.

use std::ops::Range;

use crate::byte_source::{ByteSource, Ctl};
use crate::search::Cancelled;

/// Window size of the streaming pass.
pub const WINDOW: usize = 1 << 20;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HashAlgo {
    Crc32,
    Md5,
    Sha1,
    Sha256,
}

impl HashAlgo {
    pub const ALL: [HashAlgo; 4] = [HashAlgo::Crc32, HashAlgo::Md5, HashAlgo::Sha1, HashAlgo::Sha256];
    pub fn label(self) -> &'static str {
        match self {
            HashAlgo::Crc32 => "CRC-32",
            HashAlgo::Md5 => "MD5",
            HashAlgo::Sha1 => "SHA-1",
            HashAlgo::Sha256 => "SHA-256",
        }
    }
}

// ----------------------------------------------------------------------------- CRC-32 (IEEE 802.3)

const fn crc_table() -> [u32; 256] {
    let mut t = [0u32; 256];
    let mut i = 0;
    while i < 256 {
        let mut c = i as u32;
        let mut k = 0;
        while k < 8 {
            c = if c & 1 != 0 { 0xEDB8_8320 ^ (c >> 1) } else { c >> 1 };
            k += 1;
        }
        t[i] = c;
        i += 1;
    }
    t
}
static CRC_TABLE: [u32; 256] = crc_table();

#[derive(Clone)]
pub struct Crc32(u32);
impl Default for Crc32 {
    fn default() -> Self {
        Crc32(!0)
    }
}
impl Crc32 {
    pub fn update(&mut self, data: &[u8]) {
        let mut c = self.0;
        for b in data {
            c = CRC_TABLE[((c ^ *b as u32) & 0xFF) as usize] ^ (c >> 8);
        }
        self.0 = c;
    }
    pub fn finish(&self) -> u32 {
        !self.0
    }
}

// ----------------------------------------------------------------------------- SHA-1 (FIPS 180-4)

#[derive(Clone)]
pub struct Sha1 {
    h: [u32; 5],
    buf: [u8; 64],
    fill: usize,
    total: u64,
}
impl Default for Sha1 {
    fn default() -> Self {
        Sha1 { h: [0x6745_2301, 0xEFCD_AB89, 0x98BA_DCFE, 0x1032_5476, 0xC3D2_E1F0], buf: [0; 64], fill: 0, total: 0 }
    }
}
impl Sha1 {
    fn block(h: &mut [u32; 5], b: &[u8]) {
        let mut w = [0u32; 80];
        for i in 0..16 {
            w[i] = u32::from_be_bytes([b[4 * i], b[4 * i + 1], b[4 * i + 2], b[4 * i + 3]]);
        }
        for i in 16..80 {
            w[i] = (w[i - 3] ^ w[i - 8] ^ w[i - 14] ^ w[i - 16]).rotate_left(1);
        }
        let [mut a, mut bb, mut c, mut d, mut e] = *h;
        for (i, wi) in w.iter().enumerate() {
            let (f, k) = match i {
                0..=19 => ((bb & c) | (!bb & d), 0x5A82_7999),
                20..=39 => (bb ^ c ^ d, 0x6ED9_EBA1),
                40..=59 => ((bb & c) | (bb & d) | (c & d), 0x8F1B_BCDC),
                _ => (bb ^ c ^ d, 0xCA62_C1D6u32),
            };
            let t = a.rotate_left(5).wrapping_add(f).wrapping_add(e).wrapping_add(k).wrapping_add(*wi);
            e = d;
            d = c;
            c = bb.rotate_left(30);
            bb = a;
            a = t;
        }
        for (x, y) in h.iter_mut().zip([a, bb, c, d, e]) {
            *x = x.wrapping_add(y);
        }
    }
    pub fn update(&mut self, mut data: &[u8]) {
        self.total += data.len() as u64;
        if self.fill > 0 {
            let n = (64 - self.fill).min(data.len());
            self.buf[self.fill..self.fill + n].copy_from_slice(&data[..n]);
            self.fill += n;
            data = &data[n..];
            if self.fill < 64 {
                return;
            }
            let b = self.buf;
            Self::block(&mut self.h, &b);
            self.fill = 0;
        }
        let mut chunks = data.chunks_exact(64);
        for c in &mut chunks {
            Self::block(&mut self.h, c);
        }
        let rest = chunks.remainder();
        self.buf[..rest.len()].copy_from_slice(rest);
        self.fill = rest.len();
    }
    pub fn finish(mut self) -> [u8; 20] {
        let bits = self.total.wrapping_mul(8);
        let mut pad = vec![0x80u8];
        let zeros = (119 - self.fill) % 64; // so that fill + 1 + zeros + 8 == 0 mod 64
        pad.extend(std::iter::repeat(0).take(zeros));
        pad.extend_from_slice(&bits.to_be_bytes());
        let total = self.total;
        self.update(&pad);
        self.total = total;
        debug_assert_eq!(self.fill, 0);
        let mut out = [0u8; 20];
        for (i, v) in self.h.iter().enumerate() {
            out[4 * i..4 * i + 4].copy_from_slice(&v.to_be_bytes());
        }
        out
    }
}

// ----------------------------------------------------------------------------- the streaming pass

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

enum Hasher {
    Crc(Crc32),
    Md5(md5::Md5),
    Sha1(Sha1),
    Sha256(sha2::Sha256),
}

impl Hasher {
    fn new(a: HashAlgo) -> Self {
        use md5::Digest;
        match a {
            HashAlgo::Crc32 => Hasher::Crc(Crc32::default()),
            HashAlgo::Md5 => Hasher::Md5(md5::Md5::new()),
            HashAlgo::Sha1 => Hasher::Sha1(Sha1::default()),
            HashAlgo::Sha256 => Hasher::Sha256(sha2::Sha256::new()),
        }
    }
    fn update(&mut self, d: &[u8]) {
        use md5::Digest;
        match self {
            Hasher::Crc(h) => h.update(d),
            Hasher::Md5(h) => h.update(d),
            Hasher::Sha1(h) => h.update(d),
            Hasher::Sha256(h) => h.update(d),
        }
    }
    fn finish(self) -> String {
        use md5::Digest;
        match self {
            Hasher::Crc(h) => format!("{:08x}", h.finish()),
            Hasher::Md5(h) => hex(&h.finalize()),
            Hasher::Sha1(h) => hex(&h.finish()),
            Hasher::Sha256(h) => hex(&h.finalize()),
        }
    }
}

/// Hash `range` of `src` (clamped to the source) with every algorithm in `algos`, in one pass.
/// Progress is reported per window; a cancelled pass returns `Err(Cancelled)`.
pub fn hash_range(src: &dyn ByteSource, range: Range<u64>, algos: &[HashAlgo], ctl: &mut Ctl) -> Result<Vec<(HashAlgo, String)>, Cancelled> {
    hash_range_windowed(src, range, algos, ctl, WINDOW)
}

pub fn hash_range_windowed(
    src: &dyn ByteSource,
    range: Range<u64>,
    algos: &[HashAlgo],
    ctl: &mut Ctl,
    window: usize,
) -> Result<Vec<(HashAlgo, String)>, Cancelled> {
    let end = range.end.min(src.len());
    let start = range.start.min(end);
    let total = end - start;
    let mut hashers: Vec<(HashAlgo, Hasher)> = algos.iter().map(|a| (*a, Hasher::new(*a))).collect();
    let mut buf = vec![0u8; window.max(1)];
    let mut pos = start;
    if !ctl.tick(0, total) {
        return Err(Cancelled);
    }
    while pos < end {
        let want = ((end - pos) as usize).min(buf.len());
        let n = src.read_at(pos, &mut buf[..want]);
        if n == 0 {
            break;
        }
        for (_, h) in hashers.iter_mut() {
            h.update(&buf[..n]);
        }
        pos += n as u64;
        if !ctl.tick(pos - start, total) {
            return Err(Cancelled);
        }
    }
    Ok(hashers.into_iter().map(|(a, h)| (a, h.finish())).collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::search::CancelToken;

    fn all(data: &[u8]) -> Vec<(HashAlgo, String)> {
        let cancel = CancelToken::new();
        hash_range(&data.to_vec(), 0..data.len() as u64, &HashAlgo::ALL, &mut Ctl::new(&cancel)).unwrap()
    }
    fn get(r: &[(HashAlgo, String)], a: HashAlgo) -> &str {
        &r.iter().find(|(x, _)| *x == a).unwrap().1
    }

    #[test]
    fn known_vectors() {
        let r = all(b"abc");
        assert_eq!(get(&r, HashAlgo::Md5), "900150983cd24fb0d6963f7d28e17f72");
        assert_eq!(get(&r, HashAlgo::Sha1), "a9993e364706816aba3e25717850c26c9cd0d89d");
        assert_eq!(get(&r, HashAlgo::Sha256), "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad");
        assert_eq!(get(&all(b"123456789"), HashAlgo::Crc32), "cbf43926");
        let e = all(b"");
        assert_eq!(get(&e, HashAlgo::Crc32), "00000000");
        assert_eq!(get(&e, HashAlgo::Md5), "d41d8cd98f00b204e9800998ecf8427e");
        assert_eq!(get(&e, HashAlgo::Sha1), "da39a3ee5e6b4b0d3255bfef95601890afd80709");
        assert_eq!(get(&e, HashAlgo::Sha256), "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855");
    }

    #[test]
    fn sha1_long_message_vector() {
        // FIPS 180 test message of 448 bits (padding spills into a second block).
        let r = all(b"abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq");
        assert_eq!(get(&r, HashAlgo::Sha1), "84983e441c3bd26ebaae4aa1f95129e5e54670f1");
        // one million 'a'
        let m = vec![b'a'; 1_000_000];
        assert_eq!(get(&all(&m), HashAlgo::Sha1), "34aa973cd4c4daa4f61eeb2bdbad27316534016f");
    }

    #[test]
    fn sha1_every_length_matches_chunked_updates() {
        // Chunking must not change the digest: feed 1 byte at a time vs all at once.
        for n in 0..200usize {
            let d: Vec<u8> = (0..n as u32).map(|i| (i * 7 + 3) as u8).collect();
            let mut a = Sha1::default();
            a.update(&d);
            let mut b = Sha1::default();
            d.iter().for_each(|x| b.update(&[*x]));
            assert_eq!(a.finish(), b.finish(), "len {n}");
        }
    }

    #[test]
    fn crc_matches_bitwise_reference() {
        fn bitwise(d: &[u8]) -> u32 {
            let mut c = !0u32;
            for b in d {
                c ^= *b as u32;
                for _ in 0..8 {
                    c = if c & 1 != 0 { 0xEDB8_8320 ^ (c >> 1) } else { c >> 1 };
                }
            }
            !c
        }
        let d: Vec<u8> = (0..5000u32).map(|i| (i.wrapping_mul(2654435761) >> 13) as u8).collect();
        let mut c = Crc32::default();
        c.update(&d);
        assert_eq!(c.finish(), bitwise(&d));
    }

    #[test]
    fn window_size_does_not_change_the_digest_and_range_is_respected() {
        let d: Vec<u8> = (0..10_000u32).map(|i| (i % 251) as u8).collect();
        let cancel = CancelToken::new();
        let a = hash_range_windowed(&d, 100..9000, &HashAlgo::ALL, &mut Ctl::new(&cancel), 7).unwrap();
        let b = hash_range_windowed(&d, 100..9000, &HashAlgo::ALL, &mut Ctl::new(&cancel), 1 << 20).unwrap();
        let c = all(&d[100..9000]);
        assert_eq!(a, b);
        assert_eq!(a, c);
        // out-of-range end is clamped
        let e = hash_range(&d, 9990..20_000, &[HashAlgo::Crc32], &mut Ctl::new(&cancel)).unwrap();
        assert_eq!(e, all(&d[9990..]).into_iter().filter(|(a, _)| *a == HashAlgo::Crc32).collect::<Vec<_>>());
    }

    #[test]
    fn progress_and_cancel() {
        let d = vec![0u8; 100_000];
        let cancel = CancelToken::new();
        let mut seen = Vec::new();
        {
            let mut cb = |done: u64, total: u64| seen.push((done, total));
            let mut ctl = Ctl::new(&cancel).with_progress(&mut cb);
            hash_range_windowed(&d, 0..100_000, &[HashAlgo::Crc32], &mut ctl, 30_000).unwrap();
        }
        assert_eq!(seen.first(), Some(&(0, 100_000)));
        assert_eq!(seen.last(), Some(&(100_000, 100_000)));
        assert!(seen.windows(2).all(|w| w[0].0 <= w[1].0));
        // cancel after the first window
        let cancel = CancelToken::new();
        let c2 = cancel.clone();
        let mut cb = move |done: u64, _t: u64| {
            if done > 0 {
                c2.cancel();
            }
        };
        let mut ctl = Ctl::new(&cancel).with_progress(&mut cb);
        assert!(hash_range_windowed(&d, 0..100_000, &[HashAlgo::Sha256], &mut ctl, 30_000).is_err());
    }
}
