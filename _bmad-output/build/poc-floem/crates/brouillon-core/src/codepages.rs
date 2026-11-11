//! Codepage table (BK-008): picker list (id, label, script group) mapped to
//! `encoding_rs`, plus a few trivially bijective native codecs. Legacy
//! codepages with no table (EBCDIC, some DOS) are listed but unsupported.

use std::fmt;

/// How a codepage is implemented.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Backend {
    /// `encoding_rs` label.
    Rs(&'static str),
    Utf8,
    Utf16Le,
    Utf16Be,
    Utf32Le,
    Utf32Be,
    /// Native ISO-8859-1: every byte maps to the same code point.
    Latin1,
    /// Native 7-bit US-ASCII.
    Ascii,
    Unsupported,
}

/// One row of the picker table.
#[derive(Debug, Clone, Copy)]
pub struct Codepage {
    pub id: &'static str,
    pub label: &'static str,
    pub group: &'static str,
    backend: Backend,
}

impl Codepage {
    pub fn is_supported(&self) -> bool {
        self.backend != Backend::Unsupported
    }
    /// True when encoding a text piece by piece gives the same bytes as encoding it whole (every
    /// backend but the escape-sequence ISO-2022-JP, whose output depends on the previous piece).
    pub fn encodes_piecewise(&self) -> bool {
        !matches!(self.backend, Backend::Rs("iso-2022-jp"))
    }
    /// Unicode transformation formats carry (or may carry) a BOM.
    pub fn is_unicode(&self) -> bool {
        matches!(self.backend, Backend::Utf8 | Backend::Utf16Le | Backend::Utf16Be | Backend::Utf32Le | Backend::Utf32Be)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CodepageError {
    /// Id not in the table.
    Unknown(String),
    /// Listed, but no conversion table is available.
    Unsupported(String),
}

impl fmt::Display for CodepageError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unknown(id) => write!(f, "Unknown encoding \"{id}\""),
            Self::Unsupported(id) => write!(f, "Encoding \"{id}\" is listed but not supported in this build"),
        }
    }
}
impl std::error::Error for CodepageError {}

const fn cp(id: &'static str, label: &'static str, group: &'static str, backend: Backend) -> Codepage {
    Codepage { id, label, group, backend }
}
const fn rs(id: &'static str, label: &'static str, group: &'static str) -> Codepage {
    cp(id, label, group, Backend::Rs(id))
}

pub const TABLE: &[Codepage] = &[
    cp("utf-8", "Unicode (UTF-8)", "Unicode", Backend::Utf8),
    cp("utf-16le", "Unicode (UTF-16 LE)", "Unicode", Backend::Utf16Le),
    cp("utf-16be", "Unicode (UTF-16 BE)", "Unicode", Backend::Utf16Be),
    cp("utf-32le", "Unicode (UTF-32 LE)", "Unicode", Backend::Utf32Le),
    cp("utf-32be", "Unicode (UTF-32 BE)", "Unicode", Backend::Utf32Be),
    cp("us-ascii", "US-ASCII", "Western European", Backend::Ascii),
    cp("iso-8859-1", "Western European (ISO-8859-1)", "Western European", Backend::Latin1),
    rs("windows-1252", "Western European (Windows-1252)", "Western European"),
    rs("iso-8859-15", "Western European (ISO-8859-15)", "Western European"),
    rs("macintosh", "Western European (Mac Roman)", "Western European"),
    rs("iso-8859-2", "Central European (ISO-8859-2)", "Central European"),
    rs("windows-1250", "Central European (Windows-1250)", "Central European"),
    rs("iso-8859-16", "South-Eastern European (ISO-8859-16)", "Central European"),
    rs("iso-8859-3", "Latin 3 (ISO-8859-3)", "South European"),
    rs("iso-8859-4", "Baltic (ISO-8859-4)", "Baltic"),
    rs("iso-8859-10", "Nordic (ISO-8859-10)", "Nordic"),
    rs("iso-8859-13", "Baltic (ISO-8859-13)", "Baltic"),
    rs("windows-1257", "Baltic (Windows-1257)", "Baltic"),
    rs("iso-8859-14", "Celtic (ISO-8859-14)", "Celtic"),
    rs("iso-8859-5", "Cyrillic (ISO-8859-5)", "Cyrillic"),
    rs("windows-1251", "Cyrillic (Windows-1251)", "Cyrillic"),
    rs("koi8-r", "Cyrillic (KOI8-R)", "Cyrillic"),
    rs("koi8-u", "Cyrillic (KOI8-U)", "Cyrillic"),
    rs("ibm866", "Cyrillic (DOS 866)", "Cyrillic"),
    rs("x-mac-cyrillic", "Cyrillic (Mac)", "Cyrillic"),
    rs("iso-8859-7", "Greek (ISO-8859-7)", "Greek"),
    rs("windows-1253", "Greek (Windows-1253)", "Greek"),
    rs("windows-1254", "Turkish (Windows-1254)", "Turkish"),
    rs("iso-8859-6", "Arabic (ISO-8859-6)", "Arabic"),
    rs("windows-1256", "Arabic (Windows-1256)", "Arabic"),
    rs("iso-8859-8", "Hebrew (ISO-8859-8 visual)", "Hebrew"),
    rs("iso-8859-8-i", "Hebrew (ISO-8859-8-I logical)", "Hebrew"),
    rs("windows-1255", "Hebrew (Windows-1255)", "Hebrew"),
    rs("windows-874", "Thai (Windows-874)", "Thai"),
    rs("windows-1258", "Vietnamese (Windows-1258)", "Vietnamese"),
    rs("shift_jis", "Japanese (Shift_JIS)", "East Asian"),
    rs("euc-jp", "Japanese (EUC-JP)", "East Asian"),
    rs("iso-2022-jp", "Japanese (ISO-2022-JP)", "East Asian"),
    rs("euc-kr", "Korean (EUC-KR)", "East Asian"),
    rs("gbk", "Chinese Simplified (GBK)", "East Asian"),
    rs("gb18030", "Chinese Simplified (GB18030)", "East Asian"),
    rs("big5", "Chinese Traditional (Big5)", "East Asian"),
    // Listed for completeness; no conversion table available.
    cp("ibm037", "EBCDIC US/Canada (IBM037)", "EBCDIC", Backend::Unsupported),
    cp("ibm500", "EBCDIC International (IBM500)", "EBCDIC", Backend::Unsupported),
    cp("ibm1026", "EBCDIC Turkish (IBM1026)", "EBCDIC", Backend::Unsupported),
    cp("ibm437", "DOS US (CP437)", "DOS", Backend::Unsupported),
    cp("ibm850", "DOS Western European (CP850)", "DOS", Backend::Unsupported),
    cp("ibm852", "DOS Central European (CP852)", "DOS", Backend::Unsupported),
];

/// Case-insensitive lookup by id.
pub fn find(id: &str) -> Result<&'static Codepage, CodepageError> {
    TABLE
        .iter()
        .find(|c| c.id.eq_ignore_ascii_case(id))
        .ok_or_else(|| CodepageError::Unknown(id.to_string()))
}

/// Lookup that also fails for unsupported rows.
pub fn supported(id: &str) -> Result<&'static Codepage, CodepageError> {
    let c = find(id)?;
    if c.is_supported() {
        Ok(c)
    } else {
        Err(CodepageError::Unsupported(c.id.to_string()))
    }
}

/// Picker filter: case-insensitive substring over id, label and group.
pub fn search(query: &str) -> Vec<&'static Codepage> {
    let q = query.to_lowercase();
    TABLE
        .iter()
        .filter(|c| q.is_empty() || c.id.contains(&q) || c.label.to_lowercase().contains(&q) || c.group.to_lowercase().contains(&q))
        .collect()
}

/// Table id for an `encoding_rs` detector guess (falls back to windows-1252).
pub fn id_for_rs(enc: &'static encoding_rs::Encoding) -> &'static str {
    let name = enc.name().to_ascii_lowercase();
    TABLE.iter().find(|c| c.id == name).map(|c| c.id).unwrap_or("windows-1252")
}

/// Decode `bytes` (BOM already removed). Lossy: invalid input becomes U+FFFD
/// (callers wanting fidelity re-encode and compare).
pub fn decode(cp: &Codepage, bytes: &[u8]) -> Result<String, CodepageError> {
    Ok(match cp.backend {
        Backend::Unsupported => return Err(CodepageError::Unsupported(cp.id.to_string())),
        Backend::Utf8 => String::from_utf8_lossy(bytes).into_owned(),
        Backend::Utf16Le => encoding_rs::UTF_16LE.decode_without_bom_handling(bytes).0.into_owned(),
        Backend::Utf16Be => encoding_rs::UTF_16BE.decode_without_bom_handling(bytes).0.into_owned(),
        Backend::Utf32Le => decode_utf32(bytes, u32::from_le_bytes),
        Backend::Utf32Be => decode_utf32(bytes, u32::from_be_bytes),
        Backend::Latin1 => bytes.iter().map(|&b| b as char).collect(),
        Backend::Ascii => bytes.iter().map(|&b| if b < 0x80 { b as char } else { '\u{FFFD}' }).collect(),
        Backend::Rs(label) => rs_encoding(label).decode_without_bom_handling(bytes).0.into_owned(),
    })
}

fn decode_utf32(bytes: &[u8], conv: fn([u8; 4]) -> u32) -> String {
    bytes
        .chunks(4)
        .map(|c| match <[u8; 4]>::try_from(c) {
            Ok(a) => char::from_u32(conv(a)).unwrap_or('\u{FFFD}'),
            Err(_) => '\u{FFFD}',
        })
        .collect()
}

/// Result of an encode: bytes plus the number of unmappable characters
/// (each written as a `?` replacement).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Encoded {
    pub bytes: Vec<u8>,
    pub unmappable: usize,
}

/// Encode `text` (no BOM added). Unmappable chars are replaced by `?` and counted.
pub fn encode(cp: &Codepage, text: &str) -> Result<Encoded, CodepageError> {
    let ok = |bytes: Vec<u8>| Encoded { bytes, unmappable: 0 };
    Ok(match cp.backend {
        Backend::Unsupported => return Err(CodepageError::Unsupported(cp.id.to_string())),
        Backend::Utf8 => ok(text.as_bytes().to_vec()),
        Backend::Utf16Le => ok(text.encode_utf16().flat_map(u16::to_le_bytes).collect()),
        Backend::Utf16Be => ok(text.encode_utf16().flat_map(u16::to_be_bytes).collect()),
        Backend::Utf32Le => ok(text.chars().flat_map(|c| (c as u32).to_le_bytes()).collect()),
        Backend::Utf32Be => ok(text.chars().flat_map(|c| (c as u32).to_be_bytes()).collect()),
        Backend::Latin1 => encode_single(text, |c| u8::try_from(c as u32).ok()),
        Backend::Ascii => encode_single(text, |c| c.is_ascii().then_some(c as u8)),
        Backend::Rs(label) => {
            let enc = rs_encoding(label);
            let (bytes, _, had_errors) = enc.encode(text);
            if !had_errors {
                ok(bytes.into_owned())
            } else {
                // Replace each unmappable char by '?' (counted), then re-encode.
                let mut n = 0;
                let mut buf = [0u8; 4];
                let fixed: String = text
                    .chars()
                    .map(|c| {
                        if enc.encode(c.encode_utf8(&mut buf)).2 {
                            n += 1;
                            '?'
                        } else {
                            c
                        }
                    })
                    .collect();
                Encoded { bytes: enc.encode(&fixed).0.into_owned(), unmappable: n }
            }
        }
    })
}

fn encode_single(text: &str, map: impl Fn(char) -> Option<u8>) -> Encoded {
    let mut unmappable = 0;
    let bytes = text
        .chars()
        .map(|c| {
            map(c).unwrap_or_else(|| {
                unmappable += 1;
                b'?'
            })
        })
        .collect();
    Encoded { bytes, unmappable }
}

fn rs_encoding(label: &str) -> &'static encoding_rs::Encoding {
    encoding_rs::Encoding::for_label(label.as_bytes()).unwrap_or(encoding_rs::WINDOWS_1252)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn forty_plus_supported_round_trip_ascii() {
        let sample = "Hello, world 123 {}[]\n";
        let mut n = 0;
        for c in TABLE.iter().filter(|c| c.is_supported()) {
            let e = encode(c, sample).unwrap();
            assert_eq!(e.unmappable, 0, "{}", c.id);
            assert_eq!(decode(c, &e.bytes).unwrap(), sample, "{}", c.id);
            n += 1;
        }
        assert!(n >= 40, "only {n} supported");
    }

    #[test]
    fn accents_round_trip_where_representable() {
        for id in ["utf-8", "utf-16le", "utf-16be", "utf-32le", "iso-8859-1", "windows-1252", "iso-8859-15", "iso-8859-2", "windows-1250", "macintosh", "gb18030"] {
            let c = supported(id).unwrap();
            let e = encode(c, "caf\u{e9}").unwrap();
            assert_eq!(e.unmappable, 0, "{id}");
            assert_eq!(decode(c, &e.bytes).unwrap(), "caf\u{e9}", "{id}");
        }
    }

    #[test]
    fn unsupported_is_listed_with_readable_error() {
        assert!(find("ibm037").is_ok());
        let err = supported("ibm037").unwrap_err();
        assert!(matches!(err, CodepageError::Unsupported(_)));
        assert!(err.to_string().contains("not supported"));
        assert!(matches!(decode(find("ibm037").unwrap(), b"a"), Err(CodepageError::Unsupported(_))));
        assert!(matches!(find("nope"), Err(CodepageError::Unknown(_))));
    }

    #[test]
    fn counts_unmappable() {
        let c = supported("windows-1252").unwrap();
        // The euro sign exists in Windows-1252 (0x80), unlike ISO-8859-1.
        assert_eq!(encode(c, "\u{20ac}\u{20ac}\u{20ac}").unwrap().unmappable, 0);
        let e = encode(c, "a\u{4e2d}b\u{4e2d}").unwrap();
        assert_eq!(e.unmappable, 2);
        assert_eq!(e.bytes, b"a?b?");
        let l1 = supported("iso-8859-1").unwrap();
        assert_eq!(encode(l1, "\u{20ac}\u{20ac}\u{20ac}").unwrap().unmappable, 3);
    }

    #[test]
    fn latin1_is_byte_faithful() {
        let all: Vec<u8> = (0..=255).collect();
        let c = supported("iso-8859-1").unwrap();
        assert_eq!(encode(c, &decode(c, &all).unwrap()).unwrap().bytes, all);
    }

    #[test]
    fn search_filters() {
        assert!(search("cyr").len() >= 5);
        assert_eq!(search("").len(), TABLE.len());
    }
}
