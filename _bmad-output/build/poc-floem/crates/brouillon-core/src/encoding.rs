//! Document encoding & line endings (EOL) helper (Round 14).
//! Exposes UTF-8, UTF-16 LE/BE, and Windows-1252 encodings,
//! with BOM auto-detection and EOL normalization.

use encoding_rs;

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum DocumentEncoding {
    Utf8,
    Utf16Le,
    Utf16Be,
    Windows1252,
}

impl DocumentEncoding {
    pub fn name(&self) -> &'static str {
        match self {
            DocumentEncoding::Utf8 => "UTF-8",
            DocumentEncoding::Utf16Le => "UTF-16 LE",
            DocumentEncoding::Utf16Be => "UTF-16 BE",
            DocumentEncoding::Windows1252 => "Windows-1252",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum EolMode {
    Lf,
    Crlf,
}

impl EolMode {
    pub fn name(&self) -> &'static str {
        match self {
            EolMode::Lf => "LF",
            EolMode::Crlf => "CRLF",
        }
    }

    pub fn separator(&self) -> &'static str {
        match self {
            EolMode::Lf => "\n",
            EolMode::Crlf => "\r\n",
        }
    }
}

/// Detect encoding from BOM or heuristic scan.
pub fn detect_encoding(bytes: &[u8]) -> (DocumentEncoding, usize) {
    if bytes.starts_with(&[0xEF, 0xBB, 0xBF]) {
        (DocumentEncoding::Utf8, 3)
    } else if bytes.starts_with(&[0xFF, 0xFE]) {
        (DocumentEncoding::Utf16Le, 2)
    } else if bytes.starts_with(&[0xFE, 0xFF]) {
        (DocumentEncoding::Utf16Be, 2)
    } else {
        // Heuristic: If valid UTF-8, use Utf8; otherwise assume Windows-1252
        if std::str::from_utf8(bytes).is_ok() {
            (DocumentEncoding::Utf8, 0)
        } else {
            (DocumentEncoding::Windows1252, 0)
        }
    }
}

/// Decode raw bytes to standard UTF-8 String based on target encoding.
pub fn decode_to_string(bytes: &[u8], encoding: DocumentEncoding) -> String {
    match encoding {
        DocumentEncoding::Utf8 => {
            let content = if bytes.starts_with(&[0xEF, 0xBB, 0xBF]) {
                &bytes[3..]
            } else {
                bytes
            };
            String::from_utf8_lossy(content).into_owned()
        }
        DocumentEncoding::Utf16Le => {
            let content = if bytes.starts_with(&[0xFF, 0xFE]) {
                &bytes[2..]
            } else {
                bytes
            };
            let (cow, _, _) = encoding_rs::UTF_16LE.decode(content);
            let result: String = cow.into_owned();
            result
        }
        DocumentEncoding::Utf16Be => {
            let content = if bytes.starts_with(&[0xFE, 0xFF]) {
                &bytes[2..]
            } else {
                bytes
            };
            let (cow, _, _) = encoding_rs::UTF_16BE.decode(content);
            let result: String = cow.into_owned();
            result
        }
        DocumentEncoding::Windows1252 => {
            let (cow, _, _) = encoding_rs::WINDOWS_1252.decode(bytes);
            let result: String = cow.into_owned();
            result
        }
    }
}

/// Encode standard UTF-8 String back into raw bytes based on target encoding.
pub fn encode_from_string(text: &str, encoding: DocumentEncoding) -> Vec<u8> {
    match encoding {
        DocumentEncoding::Utf8 => {
            // Write standard UTF-8 without BOM
            text.as_bytes().to_vec()
        }
        DocumentEncoding::Utf16Le => {
            let mut out = vec![0xFF, 0xFE]; // prepend BOM
            for code_unit in text.encode_utf16() {
                out.extend_from_slice(&code_unit.to_le_bytes());
            }
            out
        }
        DocumentEncoding::Utf16Be => {
            let mut out = vec![0xFE, 0xFF]; // prepend BOM
            for code_unit in text.encode_utf16() {
                out.extend_from_slice(&code_unit.to_be_bytes());
            }
            out
        }
        DocumentEncoding::Windows1252 => {
            let (cow, _, _) = encoding_rs::WINDOWS_1252.encode(text);
            let result: Vec<u8> = cow.into_owned();
            result
        }
    }
}

/// Detect EOL style based on raw text contents.
pub fn detect_eol(text: &str) -> EolMode {
    if text.contains("\r\n") {
        EolMode::Crlf
    } else {
        EolMode::Lf
    }
}

/// Result of full detection: codepage id (see `codepages::TABLE`) and BOM length.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Detected {
    pub id: &'static str,
    pub bom_len: usize,
}

/// BK-007 detection order: BOM > valid UTF-8 > chardetng guess (only when it
/// is confident) > `fallback_id` (must be a table id; else windows-1252).
pub fn detect(bytes: &[u8], fallback_id: &str) -> Detected {
    let by_bom: [(&[u8], &'static str); 5] = [
        (&[0xEF, 0xBB, 0xBF], "utf-8"),
        (&[0xFF, 0xFE, 0x00, 0x00], "utf-32le"),
        (&[0x00, 0x00, 0xFE, 0xFF], "utf-32be"),
        (&[0xFF, 0xFE], "utf-16le"),
        (&[0xFE, 0xFF], "utf-16be"),
    ];
    for (bom, id) in by_bom {
        if bytes.starts_with(bom) {
            return Detected { id, bom_len: bom.len() };
        }
    }
    if std::str::from_utf8(bytes).is_ok() {
        return Detected { id: "utf-8", bom_len: 0 };
    }
    let mut det = chardetng::EncodingDetector::new();
    det.feed(&bytes[..bytes.len().min(1 << 20)], bytes.len() <= 1 << 20);
    let (guess, confident) = det.guess_assess(None, false);
    let id = if confident {
        crate::codepages::id_for_rs(guess)
    } else {
        crate::codepages::supported(fallback_id).map(|c| c.id).unwrap_or("windows-1252")
    };
    Detected { id, bom_len: 0 }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detect_order() {
        assert_eq!(detect(&[0xEF, 0xBB, 0xBF, b'a'], "windows-1252"), Detected { id: "utf-8", bom_len: 3 });
        assert_eq!(detect(&[0xFF, 0xFE, b'a', 0], "x").id, "utf-16le");
        assert_eq!(detect(&[0xFF, 0xFE, 0, 0, b'a', 0, 0, 0], "x").id, "utf-32le");
        assert_eq!(detect(&[0xFE, 0xFF, 0, b'a'], "x").id, "utf-16be");
        assert_eq!(detect("h\u{e9}llo".as_bytes(), "windows-1252"), Detected { id: "utf-8", bom_len: 0 });
        // Invalid UTF-8: a legacy single-byte codepage from the table.
        let d = detect(&[b'a', 0xE9, b'b'], "bogus");
        assert!(crate::codepages::supported(d.id).is_ok() && d.id != "utf-8");
    }

    #[test]
    fn test_bom_detection() {
        assert_eq!(detect_encoding(&[0xEF, 0xBB, 0xBF, 0x61, 0x62]).0, DocumentEncoding::Utf8);
        assert_eq!(detect_encoding(&[0xFF, 0xFE, 0x61, 0x00]).0, DocumentEncoding::Utf16Le);
        assert_eq!(detect_encoding(&[0xFE, 0xFF, 0x00, 0x61]).0, DocumentEncoding::Utf16Be);
        // Valid UTF-8 ASCII
        assert_eq!(detect_encoding(&[0x61, 0x62, 0x63]).0, DocumentEncoding::Utf8);
        // Invalid UTF-8 bytes -> Windows-1252 fallback
        assert_eq!(detect_encoding(&[0xE9, 0xE0, 0xE2]).0, DocumentEncoding::Windows1252);
    }

    #[test]
    fn test_windows_1252_decoding() {
        let raw = vec![0xE9, 0x20, 0xE0, 0x20, 0xEB]; // "é à ë" in Win-1252
        let decoded = decode_to_string(&raw, DocumentEncoding::Windows1252);
        assert_eq!(decoded, "é à ë");
    }

    #[test]
    fn test_windows_1252_encoding() {
        let text = "é à ë";
        let encoded = encode_from_string(text, DocumentEncoding::Windows1252);
        assert_eq!(encoded, vec![0xE9, 0x20, 0xE0, 0x20, 0xEB]);
    }

    #[test]
    fn test_eol_detection() {
        assert_eq!(detect_eol("hello\r\nworld"), EolMode::Crlf);
        assert_eq!(detect_eol("hello\nworld"), EolMode::Lf);
    }

    #[test]
    fn test_utf16_le_roundtrip() {
        let text = "ABC";
        let encoded = encode_from_string(text, DocumentEncoding::Utf16Le);
        // Should start with UTF-16 LE BOM: FF FE
        assert_eq!(&encoded[0..2], &[0xFF, 0xFE], "UTF-16 LE BOM");
        // Encoding produces bytes — decode them back
        let decoded = decode_to_string(&encoded, DocumentEncoding::Utf16Le);
        assert_eq!(decoded, text);
    }

    #[test]
    fn test_utf8_bom_stripping() {
        let raw = vec![0xEF, 0xBB, 0xBF, b'A', b'B', b'C'];
        let decoded = decode_to_string(&raw, DocumentEncoding::Utf8);
        assert_eq!(decoded, "ABC", "BOM should be stripped from decoded text");
    }

    #[test]
    fn test_encoding_names() {
        assert_eq!(DocumentEncoding::Utf8.name(), "UTF-8");
        assert_eq!(DocumentEncoding::Utf16Le.name(), "UTF-16 LE");
        assert_eq!(DocumentEncoding::Utf16Be.name(), "UTF-16 BE");
        assert_eq!(DocumentEncoding::Windows1252.name(), "Windows-1252");
    }

    #[test]
    fn test_eol_separators() {
        assert_eq!(EolMode::Lf.separator(), "\n");
        assert_eq!(EolMode::Crlf.separator(), "\r\n");
        assert_eq!(EolMode::Lf.name(), "LF");
        assert_eq!(EolMode::Crlf.name(), "CRLF");
    }
}
