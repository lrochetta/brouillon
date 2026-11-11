//! `FileKind`: what are these bytes? Pure functions, no UI, no I/O beyond one bounded head read.
//!
//! Detection runs only when a file is opened (never at launch) and reads at most [`HEAD_BYTES`].
//! Magic bytes beat the extension for binary-backed kinds; the extension only refines plain text.
//! Design: internal design notes (not published) section 1.

use std::collections::HashSet;
use std::path::Path;
use std::sync::{Mutex, OnceLock};

/// Bytes of a file head that detection may look at.
pub const HEAD_BYTES: usize = 16 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ImageFmt {
    Png,
    Jpeg,
    Gif,
    Webp,
    Bmp,
    Ico,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ArchiveFmt {
    Zip,
    Gzip,
    SevenZip,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FileKind {
    /// Any decodable text; the language is a separate axis.
    Text,
    Markdown,
    Html,
    Csv,
    Tsv,
    Json,
    Yaml,
    Toml,
    Xml,
    Log,
    Rtf,
    Doc,
    Docx,
    Odt,
    Ods,
    Odp,
    Xlsx,
    Pptx,
    Pdf,
    Epub,
    Image(ImageFmt),
    Audio,
    Video,
    Archive(ArchiveFmt),
    Sqlite,
    /// Not text and not recognised: hex.
    Binary,
}

impl FileKind {
    /// Short label for the status chip and the offer bar.
    pub fn label(self) -> &'static str {
        use FileKind::*;
        match self {
            Text => "Text",
            Markdown => "Markdown",
            Html => "HTML",
            Csv => "CSV",
            Tsv => "TSV",
            Json => "JSON",
            Yaml => "YAML",
            Toml => "TOML",
            Xml => "XML",
            Log => "Log",
            Rtf => "RTF document",
            Doc => "Word document (legacy)",
            Docx => "Word document",
            Odt => "OpenDocument text",
            Ods => "OpenDocument spreadsheet",
            Odp => "OpenDocument presentation",
            Xlsx => "Excel workbook",
            Pptx => "PowerPoint presentation",
            Pdf => "PDF document",
            Epub => "EPUB book",
            Image(_) => "Image",
            Audio => "Audio",
            Video => "Video",
            Archive(_) => "Archive",
            Sqlite => "SQLite database",
            Binary => "Binary file",
        }
    }

    /// The optional module that serves this kind, `None` = nothing to install (in the exe, or no viewer yet).
    pub fn module(self) -> Option<&'static str> {
        use FileKind::*;
        match self {
            Pdf | Doc | Docx | Odt | Rtf => Some("docs"),
            _ => None,
        }
    }

    /// Bytes that are not text: opening them in the text editor shows garbage.
    pub fn is_binary_backed(self) -> bool {
        use FileKind::*;
        matches!(self, Pdf | Doc | Docx | Odt | Ods | Odp | Xlsx | Pptx | Epub | Image(_) | Audio | Video | Archive(_) | Sqlite | Binary)
    }

    /// Kinds this build routes away from the text editor while no viewer is available (hex fallback).
    /// Other binary kinds keep their previous behaviour until their own viewer exists.
    pub fn routed_to_viewer(self) -> bool {
        matches!(self, FileKind::Pdf | FileKind::Doc | FileKind::Docx | FileKind::Odt)
    }

    /// Stable key for settings and the module state file.
    pub fn key(self) -> &'static str {
        use FileKind::*;
        match self {
            Pdf => "pdf",
            Doc => "doc",
            Docx => "docx",
            Odt => "odt",
            Rtf => "rtf",
            _ => "other",
        }
    }
}

/// Result of detection plus the one-line reason ("Why was this opened as X?").
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Detection {
    pub kind: FileKind,
    /// The content says one thing and the extension another (e.g. `report.txt` that is a PDF).
    pub mislabelled: bool,
    pub evidence: &'static str,
}

fn ext_of(name: &str) -> String {
    Path::new(name).extension().map(|e| e.to_string_lossy().to_ascii_lowercase()).unwrap_or_default()
}

fn kind_from_ext(ext: &str) -> Option<FileKind> {
    Some(match ext {
        "md" | "markdown" | "mdown" => FileKind::Markdown,
        "html" | "htm" => FileKind::Html,
        "csv" => FileKind::Csv,
        "tsv" => FileKind::Tsv,
        "json" | "jsonl" => FileKind::Json,
        "yaml" | "yml" => FileKind::Yaml,
        "toml" => FileKind::Toml,
        "xml" => FileKind::Xml,
        "log" => FileKind::Log,
        "rtf" => FileKind::Rtf,
        "pdf" => FileKind::Pdf,
        "doc" => FileKind::Doc,
        "docx" => FileKind::Docx,
        "odt" => FileKind::Odt,
        _ => return None,
    })
}

/// Kind from the head bytes alone, when they are unambiguous.
fn kind_from_magic(head: &[u8]) -> Option<(FileKind, &'static str)> {
    let starts = |p: &[u8]| head.starts_with(p);
    // `%PDF-` at the start (a few junk bytes tolerated). Not "anywhere in the first KiB": a text file that merely
    // quotes the marker must stay text.
    if head[..head.len().min(21)].windows(5).any(|w| w == b"%PDF-") {
        return Some((FileKind::Pdf, "magic %PDF-"));
    }
    if starts(b"{\\rtf") {
        return Some((FileKind::Rtf, "magic {\\rtf"));
    }
    if starts(&[0xD0, 0xCF, 0x11, 0xE0, 0xA1, 0xB1, 0x1A, 0xE1]) {
        // OLE2: legacy Word/Excel/PowerPoint. Without parsing the directory, call it Doc (the viewer decides).
        return Some((FileKind::Doc, "magic OLE2"));
    }
    if starts(&[0x89, b'P', b'N', b'G']) {
        return Some((FileKind::Image(ImageFmt::Png), "magic PNG"));
    }
    if starts(&[0xFF, 0xD8, 0xFF]) {
        return Some((FileKind::Image(ImageFmt::Jpeg), "magic JPEG"));
    }
    if starts(b"GIF8") {
        return Some((FileKind::Image(ImageFmt::Gif), "magic GIF"));
    }
    if starts(b"RIFF") && head.len() >= 12 {
        match &head[8..12] {
            b"WEBP" => return Some((FileKind::Image(ImageFmt::Webp), "magic WEBP")),
            b"WAVE" => return Some((FileKind::Audio, "magic WAVE")),
            _ => {}
        }
    }
    if starts(&[0, 0, 1, 0]) {
        return Some((FileKind::Image(ImageFmt::Ico), "magic ICO"));
    }
    if starts(b"SQLite format 3\0") {
        return Some((FileKind::Sqlite, "magic SQLite"));
    }
    if starts(&[0x1F, 0x8B]) {
        return Some((FileKind::Archive(ArchiveFmt::Gzip), "magic gzip"));
    }
    if starts(&[0x37, 0x7A, 0xBC, 0xAF]) {
        return Some((FileKind::Archive(ArchiveFmt::SevenZip), "magic 7z"));
    }
    if starts(b"ID3") || starts(b"OggS") || starts(b"fLaC") {
        return Some((FileKind::Audio, "magic audio"));
    }
    if head.len() >= 12 && &head[4..8] == b"ftyp" {
        return Some((FileKind::Video, "magic ftyp"));
    }
    if starts(&[0x1A, 0x45, 0xDF, 0xA3]) {
        return Some((FileKind::Video, "magic Matroska"));
    }
    if starts(b"PK\x03\x04") {
        return Some(zip_kind(head));
    }
    None
}

/// Classify a zip by walking local file headers inside the head (no extraction, no central directory read).
fn zip_kind(head: &[u8]) -> (FileKind, &'static str) {
    let mut names: Vec<&[u8]> = Vec::new();
    let mut mimetype: Option<&[u8]> = None;
    let mut pos = 0usize;
    while pos + 30 <= head.len() && head[pos..].starts_with(b"PK\x03\x04") && names.len() < 64 {
        let u16at = |o: usize| u16::from_le_bytes([head[pos + o], head[pos + o + 1]]) as usize;
        let u32at = |o: usize| u32::from_le_bytes([head[pos + o], head[pos + o + 1], head[pos + o + 2], head[pos + o + 3]]) as usize;
        let (flags, method, csize, nlen, xlen) = (u16at(6), u16at(8), u32at(18), u16at(26), u16at(28));
        let name_at = pos + 30;
        let data_at = name_at + nlen + xlen;
        if data_at > head.len() {
            if name_at + nlen <= head.len() {
                names.push(&head[name_at..name_at + nlen]);
            }
            break;
        }
        let name = &head[name_at..name_at + nlen];
        names.push(name);
        if name == b"mimetype" && method == 0 && data_at + csize <= head.len() {
            mimetype = Some(&head[data_at..data_at + csize]);
        }
        if flags & 8 != 0 && csize == 0 {
            break; // sizes in a trailing descriptor: cannot skip safely
        }
        pos = data_at + csize;
    }
    if let Some(m) = mimetype {
        return match m {
            b"application/epub+zip" => (FileKind::Epub, "zip mimetype epub"),
            b"application/vnd.oasis.opendocument.text" => (FileKind::Odt, "zip mimetype odt"),
            b"application/vnd.oasis.opendocument.spreadsheet" => (FileKind::Ods, "zip mimetype ods"),
            b"application/vnd.oasis.opendocument.presentation" => (FileKind::Odp, "zip mimetype odp"),
            _ => (FileKind::Archive(ArchiveFmt::Zip), "zip"),
        };
    }
    let has = |p: &[u8]| names.iter().any(|n| n.starts_with(p));
    if has(b"[Content_Types].xml") || has(b"_rels/") {
        if has(b"word/") {
            return (FileKind::Docx, "zip word/ parts");
        }
        if has(b"xl/") {
            return (FileKind::Xlsx, "zip xl/ parts");
        }
        if has(b"ppt/") {
            return (FileKind::Pptx, "zip ppt/ parts");
        }
    }
    (FileKind::Archive(ArchiveFmt::Zip), "zip")
}

/// Text or not: NUL bytes mean binary, except UTF-16/32 (BOM, or NULs alternating in the sampled head).
fn looks_binary(head: &[u8]) -> bool {
    if head.starts_with(&[0xFF, 0xFE]) || head.starts_with(&[0xFE, 0xFF]) || head.starts_with(&[0xEF, 0xBB, 0xBF]) {
        return false;
    }
    let sample = &head[..head.len().min(8192)];
    let nuls = sample.iter().filter(|&&b| b == 0).count();
    if nuls == 0 {
        return false;
    }
    // UTF-16 without BOM: about half the bytes are NUL at one parity.
    let even = sample.iter().step_by(2).filter(|&&b| b == 0).count();
    let odd = sample.iter().skip(1).step_by(2).filter(|&&b| b == 0).count();
    let half = sample.len() / 2;
    let utf16 = half > 0 && (even * 10 > half * 3 || odd * 10 > half * 3) && (even == 0 || odd == 0 || even.min(odd) * 8 < even.max(odd));
    !utf16
}

/// Detect the kind of a file from its name and head bytes.
pub fn detect_kind(name: &str, head: &[u8]) -> Detection {
    let head = &head[..head.len().min(HEAD_BYTES)];
    let ext_kind = kind_from_ext(&ext_of(name));
    if let Some((kind, evidence)) = kind_from_magic(head) {
        let mislabelled = ext_kind.map_or(false, |e| e != kind && e.is_binary_backed()) || (ext_kind.is_none() && !ext_of(name).is_empty() && matches!(ext_of(name).as_str(), "txt"));
        return Detection { kind, mislabelled, evidence };
    }
    if looks_binary(head) {
        return Detection { kind: FileKind::Binary, mislabelled: false, evidence: "NUL bytes" };
    }
    match ext_kind {
        // A binary-backed extension on bytes that are plain text (a stub, a renamed note): text wins.
        Some(k) if k.is_binary_backed() => Detection { kind: FileKind::Text, mislabelled: true, evidence: "text content under a binary extension" },
        Some(k) => Detection { kind: k, mislabelled: false, evidence: "extension" },
        None => Detection { kind: FileKind::Text, mislabelled: false, evidence: "text" },
    }
}

/// Read at most [`HEAD_BYTES`] of `path` and detect. An unreadable file is `Text` (the normal open reports the error).
pub fn detect_path(path: &Path) -> Detection {
    use std::io::Read;
    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let mut head = Vec::with_capacity(HEAD_BYTES);
    match std::fs::File::open(path).and_then(|f| f.take(HEAD_BYTES as u64).read_to_end(&mut head)) {
        Ok(_) => detect_kind(&name, &head),
        Err(_) => Detection { kind: FileKind::Text, mislabelled: false, evidence: "unreadable head" },
    }
}

// ---- "Always open these files as text" (per kind, set from the offer bar) ----

fn text_override() -> &'static Mutex<HashSet<&'static str>> {
    static S: OnceLock<Mutex<HashSet<&'static str>>> = OnceLock::new();
    S.get_or_init(|| Mutex::new(HashSet::new()))
}

static LOADER: OnceLock<fn() -> Vec<String>> = OnceLock::new();
static LOADED: std::sync::Once = std::sync::Once::new();

/// Register where the saved preference comes from. Only stores a function pointer (no I/O at startup);
/// it is called once, the first time a routed kind is opened.
pub fn set_always_text_loader(f: fn() -> Vec<String>) {
    let _ = LOADER.set(f);
}

/// Replace the set of kinds (by [`FileKind::key`]) that always open as text. Empty by default.
pub fn set_always_text(keys: &[&str]) {
    let all = [FileKind::Pdf, FileKind::Doc, FileKind::Docx, FileKind::Odt, FileKind::Rtf];
    let mut s = text_override().lock().unwrap_or_else(|e| e.into_inner());
    s.clear();
    for k in all.iter().filter(|k| keys.contains(&k.key())) {
        s.insert(k.key());
    }
}

pub fn is_always_text(kind: FileKind) -> bool {
    LOADED.call_once(|| {
        if let Some(load) = LOADER.get() {
            let keys = load();
            set_always_text(&keys.iter().map(|s| s.as_str()).collect::<Vec<_>>());
        }
    });
    text_override().lock().map(|s| s.contains(kind.key())).unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn zip_with(entries: &[(&str, &[u8])]) -> Vec<u8> {
        let mut v = Vec::new();
        for (n, d) in entries {
            v.extend_from_slice(b"PK\x03\x04");
            v.extend_from_slice(&[20, 0, 0, 0, 0, 0, 0, 0, 0, 0]); // ver, flags, method 0, time, date
            v.extend_from_slice(&0u32.to_le_bytes()); // crc
            v.extend_from_slice(&(d.len() as u32).to_le_bytes());
            v.extend_from_slice(&(d.len() as u32).to_le_bytes());
            v.extend_from_slice(&(n.len() as u16).to_le_bytes());
            v.extend_from_slice(&0u16.to_le_bytes());
            v.extend_from_slice(n.as_bytes());
            v.extend_from_slice(d);
        }
        v
    }

    #[test]
    fn magic_kinds() {
        assert_eq!(detect_kind("a.bin", b"%PDF-1.7\n...").kind, FileKind::Pdf);
        assert_eq!(detect_kind("a", b"{\\rtf1\\ansi").kind, FileKind::Rtf);
        assert_eq!(detect_kind("a", &[0xD0, 0xCF, 0x11, 0xE0, 0xA1, 0xB1, 0x1A, 0xE1, 0, 0]).kind, FileKind::Doc);
        assert_eq!(detect_kind("a", &[0x89, b'P', b'N', b'G', 13, 10]).kind, FileKind::Image(ImageFmt::Png));
        assert_eq!(detect_kind("a", b"SQLite format 3\0xxxx").kind, FileKind::Sqlite);
    }

    #[test]
    fn zip_flavours() {
        let docx = zip_with(&[("[Content_Types].xml", b"<x/>"), ("word/document.xml", b"<d/>")]);
        assert_eq!(detect_kind("a.zip", &docx).kind, FileKind::Docx);
        let odt = zip_with(&[("mimetype", b"application/vnd.oasis.opendocument.text"), ("content.xml", b"<c/>")]);
        assert_eq!(detect_kind("a.dat", &odt).kind, FileKind::Odt);
        let plain = zip_with(&[("readme.txt", b"hi")]);
        assert_eq!(detect_kind("a.zip", &plain).kind, FileKind::Archive(ArchiveFmt::Zip));
        let xlsx = zip_with(&[("[Content_Types].xml", b"<x/>"), ("xl/workbook.xml", b"<w/>")]);
        assert_eq!(detect_kind("a", &xlsx).kind, FileKind::Xlsx);
    }

    #[test]
    fn magic_beats_extension_and_flags_it() {
        let d = detect_kind("report.txt", b"%PDF-1.4\n");
        assert_eq!(d.kind, FileKind::Pdf);
        assert!(d.mislabelled);
        let d = detect_kind("a.pdf", b"just some notes\n");
        assert_eq!(d.kind, FileKind::Text);
        assert!(d.mislabelled);
    }

    #[test]
    fn text_lens_kinds_by_extension_and_binary_by_nul() {
        assert_eq!(detect_kind("README.md", b"# hi\n").kind, FileKind::Markdown);
        assert_eq!(detect_kind("x.csv", b"a,b\n1,2\n").kind, FileKind::Csv);
        assert_eq!(detect_kind("x.rtf", b"plain text under rtf ext").kind, FileKind::Rtf);
        assert_eq!(detect_kind("x.dat", &[1, 2, 0, 0, 0, 3, 4, 0]).kind, FileKind::Binary);
        assert_eq!(detect_kind("empty", b"").kind, FileKind::Text);
        // UTF-16LE without BOM is text, not binary
        let u16: Vec<u8> = "hello world, this is text".encode_utf16().flat_map(|c| c.to_le_bytes()).collect();
        assert_eq!(detect_kind("u.txt", &u16).kind, FileKind::Text);
    }

    #[test]
    fn never_panics_on_truncated_or_hostile_heads() {
        let mut z = zip_with(&[("[Content_Types].xml", b"<x/>"), ("word/document.xml", b"<d/>")]);
        for n in 0..z.len() {
            let _ = detect_kind("a.docx", &z[..n]);
        }
        z[26] = 0xFF; // absurd name length
        z[27] = 0xFF;
        let _ = detect_kind("a", &z);
        let big = vec![b'A'; HEAD_BYTES * 3];
        let _ = detect_kind("a", &big);
    }

    #[test]
    fn module_and_routing_table() {
        assert_eq!(FileKind::Pdf.module(), Some("docs"));
        assert_eq!(FileKind::Rtf.module(), Some("docs"));
        assert_eq!(FileKind::Markdown.module(), None);
        assert!(FileKind::Docx.routed_to_viewer() && !FileKind::Rtf.routed_to_viewer() && !FileKind::Xlsx.routed_to_viewer());
    }
}
