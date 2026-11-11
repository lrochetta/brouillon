//! Faithful file reading / writing (BK-007, BK-009, BK-010).
//!
//! Rules: the buffer text keeps its original line endings and the BOM flag is
//! remembered separately, so open + save with no edit is byte-identical.
//! Nothing here changes BOM or EOL unless the caller asks for it.

use std::fmt;
use std::io;
use std::path::Path;

use brouillon_core::codepages::{self, CodepageError};
use brouillon_core::contract::{CmdCtx, CmdOutcome, CommandSpec, EditSet, PanelId, TextEdit};
use brouillon_core::encoding;
use brouillon_core::eol::{self, EolKind, EolScan};

/// A decoded file plus everything needed to write it back unchanged.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoadedDoc {
    /// Text with its ORIGINAL line endings and no BOM.
    pub text: String,
    /// Codepage id (see `codepages::TABLE`).
    pub codepage: &'static str,
    pub bom: bool,
    pub eol: EolScan,
    /// Decoding replaced invalid bytes; saving would not reproduce the file.
    pub lossy: bool,
}

impl LoadedDoc {
    pub fn eol_kind(&self) -> Option<EolKind> {
        self.eol.dominant()
    }
    pub fn eol_mixed(&self) -> bool {
        self.eol.is_mixed()
    }
}

#[derive(Debug)]
pub enum FileIoError {
    Codepage(CodepageError),
    /// Saving would replace `unmappable` characters; caller must confirm.
    Lossy { unmappable: usize, codepage: &'static str },
    Io(io::Error),
}

impl fmt::Display for FileIoError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Codepage(e) => e.fmt(f),
            Self::Lossy { unmappable, codepage } => f.write_str(&lossy_warning(*unmappable, codepage)),
            Self::Io(e) => e.fmt(f),
        }
    }
}
impl std::error::Error for FileIoError {}
impl From<io::Error> for FileIoError {
    fn from(e: io::Error) -> Self {
        Self::Io(e)
    }
}
impl From<CodepageError> for FileIoError {
    fn from(e: CodepageError) -> Self {
        Self::Codepage(e)
    }
}

/// Pre-save warning text for a lossy conversion.
pub fn lossy_warning(unmappable: usize, codepage: &str) -> String {
    format!("{unmappable} char{} unmappable in {codepage}", if unmappable == 1 { "" } else { "s" })
}

fn bom_bytes(id: &str) -> &'static [u8] {
    match id {
        "utf-8" => &[0xEF, 0xBB, 0xBF],
        "utf-16le" => &[0xFF, 0xFE],
        "utf-16be" => &[0xFE, 0xFF],
        "utf-32le" => &[0xFF, 0xFE, 0, 0],
        "utf-32be" => &[0, 0, 0xFE, 0xFF],
        _ => &[],
    }
}

fn finish(bytes: &[u8], id: &'static str, bom_len: usize) -> Result<LoadedDoc, CodepageError> {
    let cp = codepages::supported(id)?;
    let text = codepages::decode(cp, &bytes[bom_len..])?;
    let bom = bom_len > 0;
    let rebuilt = prepare_save(&text, &SaveSpec { codepage: cp.id, bom, eol: None })?;
    Ok(LoadedDoc { eol: eol::scan(&text), text, codepage: cp.id, bom, lossy: rebuilt.bytes != bytes })
}

/// Decode with automatic detection (BOM > UTF-8 > guess > `fallback_id`).
/// If the detected codepage cannot reproduce the bytes, fall back to the
/// byte-faithful ISO-8859-1 rather than silently corrupting the file.
pub fn decode_bytes(bytes: &[u8], fallback_id: &str) -> LoadedDoc {
    let d = encoding::detect(bytes, fallback_id);
    match finish(bytes, d.id, d.bom_len) {
        Ok(doc) if !doc.lossy => doc,
        first => finish(bytes, "iso-8859-1", 0).ok().filter(|doc| !doc.lossy).or(first.ok()).expect("iso-8859-1 always decodes"),
    }
}

/// Open As / Reload As: decode `bytes` with an explicit codepage. A matching
/// BOM is stripped and remembered.
pub fn decode_with(bytes: &[u8], codepage_id: &str) -> Result<LoadedDoc, CodepageError> {
    let cp = codepages::supported(codepage_id)?;
    let bom = bom_bytes(cp.id);
    let bom_len = if !bom.is_empty() && bytes.starts_with(bom) { bom.len() } else { 0 };
    finish(bytes, cp.id, bom_len)
}

pub fn read_file(path: &Path, fallback_id: &str) -> io::Result<LoadedDoc> {
    Ok(decode_owned(std::fs::read(path)?, fallback_id))
}

/// Like `decode_bytes`, but takes the bytes by value: BOM-less valid UTF-8 (the
/// common case) becomes the text with no copy and no re-encode round trip, which
/// used to hold up to four full copies of the file at once (about 600 MB peak for
/// a 100 MB file). Everything else goes through the general path unchanged.
pub fn decode_owned(bytes: Vec<u8>, fallback_id: &str) -> LoadedDoc {
    let d = encoding::detect(&bytes, fallback_id);
    if d.id == "utf-8" && d.bom_len == 0 {
        return match String::from_utf8(bytes) {
            Ok(text) => LoadedDoc { eol: eol::scan(&text), text, codepage: "utf-8", bom: false, lossy: false },
            Err(e) => decode_bytes(e.as_bytes(), fallback_id),
        };
    }
    decode_bytes(&bytes, fallback_id)
}

/// A BOM-less UTF-8 file read straight into a rope, with the same EOL scan `decode_owned` would give.
pub struct StreamedRope {
    pub rope: crop::Rope,
    pub eol: EolScan,
}

const STREAM_CHUNK: usize = 4 << 20;

/// Read a huge BOM-less UTF-8 file in 4 MB chunks into a rope: the heap never holds the file twice
/// (`read_file` + `Rope::from_str` peaks at 2x the file, 204 MB for a 100 MB file; this peaks at 1x
/// plus one chunk). `Ok(None)` = not that kind of file (BOM, UTF-16, invalid UTF-8): the caller uses
/// the general path, which decodes and detects exactly as before.
pub fn read_utf8_rope(path: &Path) -> io::Result<Option<StreamedRope>> {
    use std::io::Read;
    let mut file = std::fs::File::open(path)?;
    let mut builder = crop::RopeBuilder::new();
    let mut eol = EolScan::default();
    let mut buf = vec![0u8; STREAM_CHUNK];
    let mut carry = 0usize;
    let mut first = true;
    loop {
        let mut filled = carry;
        let mut eof = false;
        while filled < buf.len() {
            let n = file.read(&mut buf[filled..])?;
            if n == 0 {
                eof = true;
                break;
            }
            filled += n;
        }
        if first {
            first = false;
            let head = &buf[..filled.min(4)];
            if head.starts_with(&[0xEF, 0xBB, 0xBF]) || head.starts_with(&[0xFF, 0xFE]) || head.starts_with(&[0xFE, 0xFF]) || head.starts_with(&[0, 0, 0xFE, 0xFF]) {
                return Ok(None);
            }
        }
        let valid = match std::str::from_utf8(&buf[..filled]) {
            Ok(_) => filled,
            Err(e) if e.error_len().is_none() && !eof => e.valid_up_to(), // a character cut by the chunk end
            Err(_) => return Ok(None),
        };
        let mut end = valid;
        // A "\r\n" must not be split between two chunks (the EOL scan and the rope both see it whole).
        if !eof && end > 0 && buf[end - 1] == b'\r' {
            end -= 1;
        }
        if end > 0 {
            let text = std::str::from_utf8(&buf[..end]).expect("validated above");
            let s = eol::scan(text);
            eol.lf += s.lf;
            eol.crlf += s.crlf;
            eol.cr += s.cr;
            eol.nul += s.nul;
            builder.append(text);
        }
        buf.copy_within(end..filled, 0);
        carry = filled - end;
        if eof {
            return Ok(Some(StreamedRope { rope: builder.build(), eol }));
        }
    }
}

/// Reload the file at `path` with another codepage (changes text, not bytes).
pub fn reopen_file(path: &Path, codepage_id: &str) -> Result<LoadedDoc, FileIoError> {
    Ok(decode_with(&std::fs::read(path)?, codepage_id)?)
}

/// How to write a document.
#[derive(Debug, Clone, Copy)]
pub struct SaveSpec<'a> {
    pub codepage: &'a str,
    pub bom: bool,
    /// Convert line endings while saving. `None` = write the text as is.
    pub eol: Option<EolKind>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Prepared {
    pub bytes: Vec<u8>,
    pub unmappable: usize,
}

/// Encode for saving (no I/O). BOM is only written for Unicode codepages.
pub fn prepare_save(text: &str, spec: &SaveSpec) -> Result<Prepared, CodepageError> {
    let cp = codepages::supported(spec.codepage)?;
    let converted;
    let text = match spec.eol {
        Some(k) => {
            converted = eol::convert(text, k);
            &converted
        }
        None => text,
    };
    let enc = codepages::encode(cp, text)?;
    let mut bytes = Vec::with_capacity(enc.bytes.len() + 4);
    if spec.bom && cp.is_unicode() {
        bytes.extend_from_slice(bom_bytes(cp.id));
    }
    bytes.extend_from_slice(&enc.bytes);
    Ok(Prepared { bytes, unmappable: enc.unmappable })
}

/// Chunk size of the streamed encoder (text gathered from rope pieces before one encode call).
const STREAM_BATCH: usize = 256 << 10;

/// Encode `pieces` (consecutive parts of one text, e.g. rope chunks) and hand the bytes to `sink` in
/// batches, BOM first when the spec asks for it. Same bytes as [`prepare_save`] on the joined text,
/// without ever holding the whole text or the whole encoding: memory stays at one batch. Returns the
/// number of unmappable characters. Escape-sequence encodings and an EOL conversion need the whole
/// text, so they take the materialising path.
pub fn encode_stream<'a>(
    pieces: impl Iterator<Item = &'a str>,
    spec: &SaveSpec,
    sink: &mut dyn FnMut(&[u8]) -> io::Result<()>,
) -> Result<usize, FileIoError> {
    let cp = codepages::supported(spec.codepage)?;
    if spec.bom && cp.is_unicode() {
        sink(bom_bytes(cp.id))?;
    }
    if spec.eol.is_some() || !cp.encodes_piecewise() {
        let joined: String = pieces.collect();
        let p = prepare_save(&joined, &SaveSpec { bom: false, ..*spec })?;
        sink(&p.bytes)?;
        return Ok(p.unmappable);
    }
    let mut unmappable = 0;
    let mut batch = String::new();
    let mut flush = |batch: &mut String, unmappable: &mut usize| -> Result<(), FileIoError> {
        if batch.is_empty() {
            return Ok(());
        }
        let enc = codepages::encode(cp, batch)?;
        *unmappable += enc.unmappable;
        sink(&enc.bytes)?;
        batch.clear();
        Ok(())
    };
    for piece in pieces {
        batch.push_str(piece);
        if batch.len() >= STREAM_BATCH {
            flush(&mut batch, &mut unmappable)?;
        }
    }
    flush(&mut batch, &mut unmappable)?;
    Ok(unmappable)
}

/// Unmappable characters a save of `pieces` would replace (the bytes are discarded as they are made).
pub fn count_unmappable<'a>(pieces: impl Iterator<Item = &'a str>, spec: &SaveSpec) -> Result<usize, FileIoError> {
    let cp = codepages::supported(spec.codepage)?;
    if cp.is_unicode() {
        return Ok(0); // every character has a Unicode encoding
    }
    encode_stream(pieces, &SaveSpec { bom: false, ..*spec }, &mut |_| Ok(()))
}

/// Pre-save check: `Some(warning)` when saving would lose characters.
pub fn save_warning(text: &str, spec: &SaveSpec) -> Result<Option<String>, CodepageError> {
    let p = prepare_save(text, spec)?;
    Ok((p.unmappable > 0).then(|| lossy_warning(p.unmappable, spec.codepage)))
}

/// Write durably (`durable::durable_write`: temp file, fsync, atomic rename). A lossy
/// conversion is refused with `FileIoError::Lossy` unless `accept_lossy`.
pub fn save_file(path: &Path, text: &str, spec: &SaveSpec, accept_lossy: bool) -> Result<(), FileIoError> {
    let p = prepare_save(text, spec)?;
    if p.unmappable > 0 && !accept_lossy {
        return Err(FileIoError::Lossy { unmappable: p.unmappable, codepage: codepages::find(spec.codepage)?.id });
    }
    crate::durable::durable_write(path, &p.bytes)?;
    Ok(())
}

/// Commands this module contributes to the registry.
pub fn commands() -> Vec<CommandSpec> {
    let spec = |id: &str, caption: &str| CommandSpec {
        id: id.into(),
        caption: caption.into(),
        category: "Encoding".into(),
        ..Default::default()
    };
    vec![
        spec("eol_to_lf", "Document: Convert Line Endings to LF"),
        spec("eol_to_crlf", "Document: Convert Line Endings to CRLF"),
        spec("eol_to_cr", "Document: Convert Line Endings to CR"),
        spec("eol_cycle", "Document: Cycle Line Endings (LF, CRLF, CR)"),
        spec("encoding_reopen_as", "Encoding: Reopen with Encoding..."),
        spec("encoding_convert_to", "Encoding: Convert to Encoding..."),
        spec("encoding_pick", "Encoding: Choose Encoding..."),
    ]
}

/// Run command `id`; `None` = not mine. `args[0]` is the codepage id for
/// `encoding_reopen_as` / `encoding_convert_to`.
pub fn run(id: &str, ctx: &CmdCtx) -> Option<CmdOutcome> {
    let msg = |m: String| CmdOutcome { message: Some(m), ..Default::default() };
    let kind = match id {
        "eol_to_lf" => Some(EolKind::Lf),
        "eol_to_crlf" => Some(EolKind::Crlf),
        "eol_to_cr" => Some(EolKind::Cr),
        // Status-bar click: next kind after the dominant one (mixed -> LF).
        "eol_cycle" => Some(match eol::scan(ctx.text).dominant() {
            Some(EolKind::Lf) if !eol::scan(ctx.text).is_mixed() => EolKind::Crlf,
            Some(EolKind::Crlf) if !eol::scan(ctx.text).is_mixed() => EolKind::Cr,
            _ => EolKind::Lf,
        }),
        _ => None,
    };
    if let Some(k) = kind {
        let edits = eol::convert_edits(ctx.text, k);
        let message = Some(format!("Line endings: {}", k.name()));
        return Some(CmdOutcome { edits, message, ..Default::default() });
    }
    match id {
        "encoding_pick" => Some(CmdOutcome { open_panel: Some(PanelId("encoding_picker".into())), ..Default::default() }),
        "encoding_reopen_as" => Some(reopen_outcome(ctx).unwrap_or_else(|e| msg(e.to_string()))),
        "encoding_convert_to" => {
            let Some(target) = ctx.args.first() else { return Some(msg("No encoding given".into())) };
            let spec = SaveSpec { codepage: target, bom: false, eol: None };
            Some(msg(match save_warning(ctx.text, &spec) {
                Ok(Some(w)) => format!("{w}; saving as {target} will replace them with '?'"),
                Ok(None) => format!("Will save as {target}"),
                Err(e) => e.to_string(),
            }))
        }
        _ => None,
    }
}

fn reopen_outcome(ctx: &CmdCtx) -> Result<CmdOutcome, FileIoError> {
    let target = ctx.args.first().ok_or_else(|| CodepageError::Unknown(String::new()))?;
    let path = ctx.path.ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "Document has no file on disk"))?;
    let doc = reopen_file(path, target)?;
    let edits = if doc.text == ctx.text { EditSet::new() } else { EditSet(vec![TextEdit::new(0, ctx.text.len(), doc.text)]) };
    let note = if doc.lossy { " (some bytes could not be decoded)" } else { "" };
    Ok(CmdOutcome { edits, message: Some(format!("Reopened as {}{note}", doc.codepage)), ..Default::default() })
}

#[cfg(test)]
mod tests {
    fn whole(text: &str, spec: &SaveSpec) -> (Vec<u8>, usize) {
        let p = prepare_save(text, spec).unwrap();
        (p.bytes, p.unmappable)
    }

    fn streamed(text: &str, spec: &SaveSpec) -> (Vec<u8>, usize) {
        // Odd, tiny pieces (cut on char boundaries) stand for the rope chunks.
        let mut pieces = Vec::new();
        let (mut start, mut step) = (0, 1);
        while start < text.len() {
            let mut end = (start + step).min(text.len());
            while !text.is_char_boundary(end) {
                end += 1;
            }
            pieces.push(&text[start..end]);
            start = end;
            step = step % 7 + 1;
        }
        let mut out = Vec::new();
        let n = encode_stream(pieces.into_iter(), spec, &mut |b| {
            out.extend_from_slice(b);
            Ok(())
        })
        .unwrap();
        (out, n)
    }

    #[test]
    fn streamed_encoding_equals_prepare_save_for_every_family() {
        let text = "h\u{e9}llo \u{20ac} \u{4e2d}\u{6587} \u{1F600}\r\nline two\n";
        for cp in ["utf-8", "utf-16le", "utf-16be", "utf-32le", "utf-32be", "windows-1252", "iso-8859-1", "us-ascii", "shift_jis", "gbk", "iso-2022-jp"] {
            for bom in [false, true] {
                let spec = SaveSpec { codepage: cp, bom, eol: None };
                assert_eq!(streamed(text, &spec), whole(text, &spec), "{cp} bom={bom}");
                let n = count_unmappable(std::iter::once(text), &spec).unwrap();
                assert_eq!(n, whole(text, &spec).1, "{cp} unmappable count");
            }
        }
        let crlf = SaveSpec { codepage: "utf-8", bom: false, eol: Some(EolKind::Crlf) };
        assert_eq!(streamed(text, &crlf), whole(text, &crlf), "an EOL conversion takes the whole-text path");
    }

    #[test]
    fn streamed_encoding_crosses_the_batch_size() {
        let line = "abc d\u{e9}f \u{20ac}\n";
        let text = line.repeat(STREAM_BATCH * 2 / line.len() + 10);
        let spec = SaveSpec { codepage: "windows-1252", bom: false, eol: None };
        let (expected, expected_unmappable) = whole(&text, &spec);
        let mut out = Vec::new();
        let mut calls = 0;
        let u = encode_stream(text.split_inclusive('\n'), &spec, &mut |b| {
            calls += 1;
            out.extend_from_slice(b);
            Ok(())
        })
        .unwrap();
        assert!(calls > 1, "more than one batch was produced");
        assert_eq!((out, u), (expected, expected_unmappable));
    }

    use super::*;

    fn roundtrip(bytes: &[u8]) -> LoadedDoc {
        let doc = decode_bytes(bytes, "windows-1252");
        let spec = SaveSpec { codepage: doc.codepage, bom: doc.bom, eol: None };
        assert_eq!(prepare_save(&doc.text, &spec).unwrap().bytes, bytes, "{}", doc.codepage);
        doc
    }

    #[test]
    fn streamed_rope_matches_decode_owned() {
        // Text longer than one chunk with multibyte characters and CR / CRLF / LF straddling chunk ends.
        let mut text = String::new();
        let mut i = 0usize;
        while text.len() < 2 * STREAM_CHUNK + 100 {
            text.push_str(match i % 5 {
                0 => "line\r\n",
                1 => "caf\u{e9}\n",
                2 => "\u{4e2d}\u{6587}\r",
                3 => "x\n",
                _ => "tail",
            });
            i += 1;
        }
        let dir = std::env::temp_dir().join(format!("brouillon-stream-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        // Shift the content one byte at a time so every kind of boundary meets the chunk end.
        for pad in 0..6 {
            let body = format!("{}{}", "p".repeat(pad), text);
            let p = dir.join("t.txt");
            std::fs::write(&p, &body).unwrap();
            let s = read_utf8_rope(&p).unwrap().expect("valid utf-8");
            let d = decode_owned(body.as_bytes().to_vec(), "windows-1252");
            assert_eq!(s.rope.to_string(), d.text, "pad {pad}");
            assert_eq!(s.eol, d.eol, "pad {pad}");
        }
        // Not applicable: BOM, invalid UTF-8.
        let p = dir.join("b.txt");
        std::fs::write(&p, [&[0xEF, 0xBB, 0xBF][..], b"bom"].concat()).unwrap();
        assert!(read_utf8_rope(&p).unwrap().is_none());
        std::fs::write(&p, b"caf\xe9").unwrap();
        assert!(read_utf8_rope(&p).unwrap().is_none());
        std::fs::write(&p, b"").unwrap();
        assert_eq!(read_utf8_rope(&p).unwrap().unwrap().rope.byte_len(), 0);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn decode_owned_matches_decode_bytes() {
        let samples: Vec<Vec<u8>> = vec![
            b"plain ascii\nline two\r\n".to_vec(),
            "h\u{e9}llo w\u{f6}rld\n".as_bytes().to_vec(),
            [&[0xEF, 0xBB, 0xBF][..], b"bom\n"].concat(),
            b"caf\xe9 na\xefve\n".to_vec(),
            [&[0xFF, 0xFE][..], &"hi".encode_utf16().flat_map(u16::to_le_bytes).collect::<Vec<_>>()].concat(),
            Vec::new(),
        ];
        for s in samples {
            let a = decode_bytes(&s, "windows-1252");
            let b = decode_owned(s.clone(), "windows-1252");
            assert_eq!((a.text, a.codepage, a.bom, a.lossy), (b.text, b.codepage, b.bom, b.lossy), "{s:?}");
        }
    }

    #[test]
    fn fixtures_are_byte_identical() {
        let utf8_bom = [&[0xEF, 0xBB, 0xBF][..], b"h\xc3\xa9llo\r\nw\n"].concat();
        let d = roundtrip(&utf8_bom);
        assert!(d.bom && d.codepage == "utf-8" && d.text.starts_with('h'));
        roundtrip("h\u{e9}llo\nw".as_bytes());
        roundtrip(b"plain ascii\n");
        let le: Vec<u8> = [&[0xFF, 0xFE][..], &"hi\r\n\u{e9}".encode_utf16().flat_map(u16::to_le_bytes).collect::<Vec<_>>()].concat();
        assert_eq!(roundtrip(&le).codepage, "utf-16le");
        let be: Vec<u8> = [&[0xFE, 0xFF][..], &"hi\r\n\u{e9}".encode_utf16().flat_map(u16::to_be_bytes).collect::<Vec<_>>()].concat();
        assert_eq!(roundtrip(&be).codepage, "utf-16be");
        // Legacy single-byte (invalid UTF-8) incl. undefined 1252 bytes.
        roundtrip(b"caf\xe9 \x81\x8d na\xefve\r\n");
        // Mixed EOL, untouched.
        let m = roundtrip(b"a\nb\r\nc\rd\r\n");
        assert!(m.eol_mixed() && m.eol_kind() == Some(EolKind::Crlf));
        assert!(m.text.contains('\r'));
    }

    #[test]
    fn no_bom_stays_no_bom_and_utf16_without_bom_is_reproducible() {
        let d = roundtrip("abc".as_bytes());
        assert!(!d.bom);
        let doc = decode_with(&"hi".encode_utf16().flat_map(u16::to_le_bytes).collect::<Vec<_>>(), "utf-16le").unwrap();
        assert!(!doc.bom && !doc.lossy && doc.text == "hi");
    }

    #[test]
    fn reopen_changes_text_not_bytes() {
        let bytes = b"caf\xc3\xa9";
        let as_utf8 = decode_with(bytes, "utf-8").unwrap();
        let as_1252 = decode_with(bytes, "windows-1252").unwrap();
        assert_eq!(as_utf8.text, "caf\u{e9}");
        assert_eq!(as_1252.text, "caf\u{c3}\u{a9}");
        assert!(!as_1252.lossy);
        assert!(matches!(decode_with(bytes, "ibm037"), Err(CodepageError::Unsupported(_))));
    }

    #[test]
    fn lossy_conversion_warns_before_writing() {
        let text = "\u{20ac}\u{20ac}\u{20ac}";
        let spec = SaveSpec { codepage: "iso-8859-1", bom: false, eol: None };
        assert_eq!(save_warning(text, &spec).unwrap().unwrap(), "3 chars unmappable in iso-8859-1");
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("f.txt");
        std::fs::write(&p, b"orig").unwrap();
        let err = save_file(&p, text, &spec, false).unwrap_err();
        assert!(matches!(err, FileIoError::Lossy { unmappable: 3, .. }));
        assert_eq!(std::fs::read(&p).unwrap(), b"orig", "refused save must not touch the file");
        save_file(&p, text, &spec, true).unwrap();
        assert_eq!(std::fs::read(&p).unwrap(), b"???");
        // The euro IS representable in Windows-1252: no warning.
        let w = SaveSpec { codepage: "windows-1252", bom: false, eol: None };
        assert_eq!(save_warning(text, &w).unwrap(), None);
    }

    #[test]
    fn save_is_faithful_and_eol_only_on_request() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("a.txt");
        let src = [&[0xEF, 0xBB, 0xBF][..], b"a\nb\r\n"].concat();
        std::fs::write(&p, &src).unwrap();
        let d = read_file(&p, "windows-1252").unwrap();
        let spec = SaveSpec { codepage: d.codepage, bom: d.bom, eol: None };
        save_file(&p, &d.text, &spec, false).unwrap();
        assert_eq!(std::fs::read(&p).unwrap(), src);
        save_file(&p, &d.text, &SaveSpec { eol: Some(EolKind::Lf), ..spec }, false).unwrap();
        assert_eq!(std::fs::read(&p).unwrap(), [&[0xEF, 0xBB, 0xBF][..], b"a\nb\n"].concat());
    }

    #[test]
    fn commands_eol_and_reopen() {
        let ctx = CmdCtx { text: "a\nb\r\n", ..Default::default() };
        let out = run("eol_to_crlf", &ctx).unwrap();
        assert_eq!(out.edits.apply_to_str(ctx.text).unwrap(), "a\r\nb\r\n");
        assert!(run("nope", &ctx).is_none());
        let cyc = |t: &str| run("eol_cycle", &CmdCtx { text: t, ..Default::default() }).unwrap().edits.apply_to_str(t).unwrap();
        assert_eq!(cyc("a\nb"), "a\r\nb");
        assert_eq!(cyc("a\r\nb"), "a\rb");
        assert_eq!(cyc("a\rb\n"), "a\nb\n");
        assert!(commands().iter().all(|c| run(&c.id, &CmdCtx::default()).is_some()));

        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("r.txt");
        std::fs::write(&p, b"caf\xc3\xa9").unwrap();
        let args = ["windows-1252".to_string()];
        let ctx = CmdCtx { text: "caf\u{e9}", path: Some(&p), args: &args, ..Default::default() };
        let out = run("encoding_reopen_as", &ctx).unwrap();
        assert_eq!(out.edits.apply_to_str(ctx.text).unwrap(), "caf\u{c3}\u{a9}");

        let args = ["iso-8859-1".to_string()];
        let ctx = CmdCtx { text: "\u{20ac}\u{20ac}\u{20ac}", args: &args, ..Default::default() };
        assert!(run("encoding_convert_to", &ctx).unwrap().message.unwrap().starts_with("3 chars unmappable"));
        assert_eq!(run("encoding_pick", &ctx).unwrap().open_panel, Some(PanelId("encoding_picker".into())));
    }
}
