//! "Copy as": pure formatters from bytes to text (hex, C / Rust / Python arrays, Base64, ASCII).
//!
//! `copy_as` formats a slice; `copy_range_as` reads a range of a `ByteSource` and refuses ranges
//! above [`MAX_COPY_BYTES`] (the clipboard is not the place for a 1 GB selection).

use std::ops::Range;

use crate::byte_source::ByteSource;

/// Largest selection `copy_range_as` accepts.
pub const MAX_COPY_BYTES: u64 = 16 << 20;
/// Values per line in the array formats.
const PER_LINE: usize = 12;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CopyFormat {
    /// `48656C6C6F`
    HexPlain,
    /// `48 65 6C 6C 6F`
    HexSpaced,
    /// `unsigned char data[5] = { 0x48, ... };`
    CArray,
    /// `const DATA: [u8; 5] = [0x48, ...];`
    RustArray,
    /// `data = bytes([0x48, ...])`
    PythonBytes,
    Base64,
    /// Printable ASCII as is, every other byte as `.`
    Ascii,
}

impl CopyFormat {
    pub const ALL: [CopyFormat; 7] = [
        CopyFormat::HexPlain,
        CopyFormat::HexSpaced,
        CopyFormat::CArray,
        CopyFormat::RustArray,
        CopyFormat::PythonBytes,
        CopyFormat::Base64,
        CopyFormat::Ascii,
    ];
    /// Palette / menu label (without the "Hex: Copy as" prefix).
    pub fn label(self) -> &'static str {
        match self {
            CopyFormat::HexPlain => "hex string",
            CopyFormat::HexSpaced => "spaced hex",
            CopyFormat::CArray => "C array",
            CopyFormat::RustArray => "Rust array",
            CopyFormat::PythonBytes => "Python bytes",
            CopyFormat::Base64 => "Base64",
            CopyFormat::Ascii => "ASCII text",
        }
    }
    /// Stable command id, e.g. `hex_copy_as_c_array`.
    pub fn command_id(self) -> &'static str {
        match self {
            CopyFormat::HexPlain => "hex_copy_as_hex",
            CopyFormat::HexSpaced => "hex_copy_as_spaced_hex",
            CopyFormat::CArray => "hex_copy_as_c_array",
            CopyFormat::RustArray => "hex_copy_as_rust_array",
            CopyFormat::PythonBytes => "hex_copy_as_python_bytes",
            CopyFormat::Base64 => "hex_copy_as_base64",
            CopyFormat::Ascii => "hex_copy_as_ascii",
        }
    }
    pub fn from_command_id(id: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|f| f.command_id() == id)
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum CopyError {
    TooLarge(u64),
    OutOfRange,
}

impl std::fmt::Display for CopyError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CopyError::TooLarge(n) => write!(f, "Selection of {n} bytes is too large to copy (limit {} MiB)", MAX_COPY_BYTES >> 20),
            CopyError::OutOfRange => write!(f, "Selection is outside the file"),
        }
    }
}

fn hex_list(bytes: &[u8], head: &str, tail: &str) -> String {
    let mut s = String::with_capacity(head.len() + tail.len() + bytes.len() * 6 + 8);
    s.push_str(head);
    for (i, line) in bytes.chunks(PER_LINE).enumerate() {
        if i > 0 {
            s.push('\n');
        }
        s.push_str("    ");
        for (j, b) in line.iter().enumerate() {
            if j > 0 {
                s.push(' ');
            }
            s.push_str(&format!("0x{b:02X},"));
        }
    }
    if !bytes.is_empty() {
        s.push('\n');
    }
    s.push_str(tail);
    s
}

/// Format `bytes`. Uppercase hex digits throughout.
pub fn copy_as(bytes: &[u8], f: CopyFormat) -> String {
    use std::fmt::Write;
    match f {
        CopyFormat::HexPlain => {
            let mut s = String::with_capacity(bytes.len() * 2);
            bytes.iter().for_each(|b| {
                let _ = write!(s, "{b:02X}");
            });
            s
        }
        CopyFormat::HexSpaced => {
            let mut s = String::with_capacity(bytes.len() * 3);
            for (i, b) in bytes.iter().enumerate() {
                if i > 0 {
                    s.push(' ');
                }
                let _ = write!(s, "{b:02X}");
            }
            s
        }
        CopyFormat::CArray => hex_list(bytes, &format!("unsigned char data[{}] = {{\n", bytes.len()), "};"),
        CopyFormat::RustArray => hex_list(bytes, &format!("const DATA: [u8; {}] = [\n", bytes.len()), "];"),
        CopyFormat::PythonBytes => hex_list(bytes, "data = bytes([\n", "])"),
        CopyFormat::Base64 => {
            use base64::Engine;
            base64::engine::general_purpose::STANDARD.encode(bytes)
        }
        CopyFormat::Ascii => bytes.iter().map(|b| if (0x20..0x7F).contains(b) { *b as char } else { '.' }).collect(),
    }
}

/// Read `range` of `src` and format it; ranges above [`MAX_COPY_BYTES`] are refused.
pub fn copy_range_as(src: &dyn ByteSource, range: Range<u64>, f: CopyFormat) -> Result<String, CopyError> {
    if range.start > range.end || range.end > src.len() {
        return Err(CopyError::OutOfRange);
    }
    let n = range.end - range.start;
    if n > MAX_COPY_BYTES {
        return Err(CopyError::TooLarge(n));
    }
    let mut buf = vec![0u8; n as usize];
    let got = src.read_at(range.start, &mut buf);
    buf.truncate(got);
    Ok(copy_as(&buf, f))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hex_and_text() {
        let b = b"Hello";
        assert_eq!(copy_as(b, CopyFormat::HexPlain), "48656C6C6F");
        assert_eq!(copy_as(b, CopyFormat::HexSpaced), "48 65 6C 6C 6F");
        assert_eq!(copy_as(b, CopyFormat::Base64), "SGVsbG8=");
    }

    #[test]
    fn ascii_dots() {
        assert_eq!(copy_as(&[0x41, 0x00, 0xFF, 0x7F, 0x20], CopyFormat::Ascii), "A... ");
    }

    #[test]
    fn arrays() {
        let b = [1u8, 2, 255];
        assert_eq!(copy_as(&b, CopyFormat::CArray), "unsigned char data[3] = {\n    0x01, 0x02, 0xFF,\n};");
        assert_eq!(copy_as(&b, CopyFormat::RustArray), "const DATA: [u8; 3] = [\n    0x01, 0x02, 0xFF,\n];");
        assert_eq!(copy_as(&b, CopyFormat::PythonBytes), "data = bytes([\n    0x01, 0x02, 0xFF,\n])");
        let long: Vec<u8> = (0..13).collect();
        let s = copy_as(&long, CopyFormat::RustArray);
        assert_eq!(s.lines().count(), 4, "header + 12 + 1 + footer: {s}");
    }

    #[test]
    fn empty_input() {
        for f in CopyFormat::ALL {
            let _ = copy_as(&[], f);
        }
        assert_eq!(copy_as(&[], CopyFormat::HexPlain), "");
        assert_eq!(copy_as(&[], CopyFormat::RustArray), "const DATA: [u8; 0] = [\n];");
    }

    #[test]
    fn range_and_limits() {
        let v: Vec<u8> = (0..100u8).collect();
        assert_eq!(copy_range_as(&v, 2..5, CopyFormat::HexSpaced).unwrap(), "02 03 04");
        assert_eq!(copy_range_as(&v, 5..101, CopyFormat::HexPlain), Err(CopyError::OutOfRange));
        assert_eq!(copy_range_as(&v, 6..5, CopyFormat::HexPlain), Err(CopyError::OutOfRange));
        struct Big;
        impl ByteSource for Big {
            fn len(&self) -> u64 {
                1 << 40
            }
            fn read_at(&self, _: u64, _: &mut [u8]) -> usize {
                0
            }
        }
        assert_eq!(copy_range_as(&Big, 0..MAX_COPY_BYTES + 1, CopyFormat::HexPlain), Err(CopyError::TooLarge(MAX_COPY_BYTES + 1)));
    }

    #[test]
    fn command_ids_round_trip() {
        for f in CopyFormat::ALL {
            assert_eq!(CopyFormat::from_command_id(f.command_id()), Some(f));
        }
        assert_eq!(CopyFormat::from_command_id("nope"), None);
    }
}
