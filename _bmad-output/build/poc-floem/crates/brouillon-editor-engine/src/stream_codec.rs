//! Streaming transcoding for huge files (lazy-store task 1): UTF-16/32 and legacy codepages.
//!
//! # Design (the smallest correct one)
//!
//! The lazy store reads UTF-8 bytes through a piece table over a read-only base FILE, so a file in
//! another encoding needs a UTF-8 file to sit on. Instead of an offset map between two byte worlds
//! (every position, every line index entry, every edit would need translating), the file is
//! transcoded ONCE, in streaming chunks, into a temporary UTF-8 *sidecar* file, and the lazy store
//! opens the sidecar. Memory is O(chunk) (256 KiB default); disk is one extra UTF-8 copy.
//!
//! * Open = one sequential pass (decode + sidecar write + EOL count + a round-trip check). Not O(1)
//!   like a UTF-8 file: about the speed of the disk/decoder (hundreds of MB/s). `Ctl` gives
//!   progress and cancel.
//! * Save = the document bytes (UTF-8) are streamed through an encoder into the original encoding
//!   with the original BOM, atomically (`durable_write_with`), optionally converting line endings
//!   on the fly. Untouched regions of an unedited, lossless file come back byte-exact.
//! * Byte-exactness is *proved at open*: the decoded text is re-encoded in the same pass and
//!   compared with the source bytes. `Transcoded::lossy` is true when anything would not survive
//!   (invalid sequences, lone surrogates, bytes with several mappings, ...), with the first
//!   differing source offset. Policy for a lossy file: [`LossyPolicy::ReadOnlyView`] (view only,
//!   saving back in that encoding must not be offered) or [`LossyPolicy::FallbackLatin1`] (what the
//!   in-memory path does: re-read as ISO-8859-1, which is byte-faithful but shows mojibake).
//!
//! # Limits
//!
//! * A multi-byte sequence or a surrogate pair cut by a chunk boundary is handled by the stateful
//!   `encoding_rs` decoders (and by the pending buffers of the native UTF-32 decoder): tested at
//!   1..=7 byte chunks against a full decode.
//! * Detection looks at the first MiB only (`detect_codepage`); a file whose first MiB is valid
//!   UTF-8 but whose tail is not is opened as UTF-8 and the lazy store reports it invalid.
//! * Disk: the sidecar is at most 3x the source for CJK/ASCII-heavy UTF-16 (usually 0.5-1.5x).
//! * ISO-2022-JP is stateful: the encoder keeps its state across chunks (unlike the piecewise
//!   in-memory path), so it streams too.
//! * Unmappable characters on save become `?` (counted), exactly like `codepages::encode`.

use std::fs::File;
use std::io::{self, BufWriter, Write};
use std::ops::Range;
use std::path::{Path, PathBuf};

use brouillon_core::codepages;
use brouillon_core::eol::{EolKind, EolScan};
use encoding_rs::Encoding;

use crate::byte_source::{read_blocks, ByteSource, Ctl, FileSource, StoreSource};
use crate::file_io::FileIoError;

/// Chunk sizes of the streaming passes.
#[derive(Debug, Clone)]
pub struct StreamCfg {
    /// Bytes read from the source per step.
    pub chunk: usize,
    /// Where the sidecar goes (default: the OS temp dir).
    pub sidecar_dir: Option<PathBuf>,
}
impl Default for StreamCfg {
    fn default() -> Self {
        Self { chunk: 256 << 10, sidecar_dir: None }
    }
}

// ------------------------------------------------------------------------------------------ codecs

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Codec {
    /// UTF-8, UTF-16 LE/BE and every legacy table (decoding and, but for UTF-16, encoding).
    Rs(&'static Encoding),
    Utf32 { be: bool },
    Latin1,
    Ascii,
}

impl Codec {
    /// Codec for a picker id (`codepages::TABLE`); unsupported/unknown ids are an error.
    pub fn for_id(id: &str) -> Result<Codec, codepages::CodepageError> {
        let cp = codepages::supported(id)?;
        Ok(match cp.id {
            "utf-8" => Codec::Rs(encoding_rs::UTF_8),
            "utf-16le" => Codec::Rs(encoding_rs::UTF_16LE),
            "utf-16be" => Codec::Rs(encoding_rs::UTF_16BE),
            "utf-32le" => Codec::Utf32 { be: false },
            "utf-32be" => Codec::Utf32 { be: true },
            "iso-8859-1" => Codec::Latin1,
            "us-ascii" => Codec::Ascii,
            other => Codec::Rs(Encoding::for_label(other.as_bytes()).unwrap_or(encoding_rs::WINDOWS_1252)),
        })
    }
}

/// BOM bytes of a Unicode codepage id (empty for the others).
pub fn bom_of(id: &str) -> &'static [u8] {
    match id {
        "utf-8" => &[0xEF, 0xBB, 0xBF],
        "utf-16le" => &[0xFF, 0xFE],
        "utf-16be" => &[0xFE, 0xFF],
        "utf-32le" => &[0xFF, 0xFE, 0, 0],
        "utf-32be" => &[0, 0, 0xFE, 0xFF],
        _ => &[],
    }
}

const SUB: usize = 64 << 10;

/// Incremental decoder: bytes in (any chunking), UTF-8 text out.
pub struct StreamDecoder {
    kind: DecKind,
    scratch: Vec<u8>,
    /// Some input could not be decoded and was replaced by U+FFFD.
    pub replaced: bool,
}
enum DecKind {
    Rs(encoding_rs::Decoder),
    Utf32 { be: bool, pending: Vec<u8> },
    Latin1,
    Ascii,
}

impl StreamDecoder {
    pub fn new(codec: Codec) -> Self {
        let kind = match codec {
            Codec::Rs(e) => DecKind::Rs(e.new_decoder_without_bom_handling()),
            Codec::Utf32 { be } => DecKind::Utf32 { be, pending: Vec::with_capacity(4) },
            Codec::Latin1 => DecKind::Latin1,
            Codec::Ascii => DecKind::Ascii,
        };
        Self { kind, scratch: Vec::new(), replaced: false }
    }

    /// Feed a chunk; `emit` receives zero or more UTF-8 pieces. Pass `last` on the final call (it may
    /// be empty) so a dangling partial sequence is flushed as U+FFFD.
    pub fn push(&mut self, src: &[u8], last: bool, emit: &mut dyn FnMut(&str) -> io::Result<()>) -> io::Result<()> {
        match &mut self.kind {
            DecKind::Rs(dec) => {
                let mut off = 0;
                loop {
                    let piece_end = (off + SUB).min(src.len());
                    let is_last = last && piece_end == src.len();
                    let need = dec.max_utf8_buffer_length(piece_end - off).unwrap_or(SUB * 3) + 16;
                    if self.scratch.len() < need {
                        self.scratch.resize(need, 0);
                    }
                    let mut p = off;
                    loop {
                        let (res, read, written, err) = dec.decode_to_utf8(&src[p..piece_end], &mut self.scratch, is_last);
                        p += read;
                        self.replaced |= err;
                        if written > 0 {
                            emit(std::str::from_utf8(&self.scratch[..written]).expect("encoding_rs writes valid UTF-8"))?;
                        }
                        if res == encoding_rs::CoderResult::InputEmpty {
                            break;
                        }
                    }
                    off = piece_end;
                    if off >= src.len() {
                        break;
                    }
                }
            }
            DecKind::Utf32 { be, pending } => {
                pending.extend_from_slice(src);
                let n = pending.len() / 4 * 4;
                let mut s = String::with_capacity(SUB.min(n));
                for c in pending[..n].chunks_exact(4) {
                    let a = [c[0], c[1], c[2], c[3]];
                    let u = if *be { u32::from_be_bytes(a) } else { u32::from_le_bytes(a) };
                    s.push(char::from_u32(u).unwrap_or_else(|| {
                        self.replaced = true;
                        '\u{FFFD}'
                    }));
                    if s.len() >= SUB {
                        emit(&s)?;
                        s.clear();
                    }
                }
                if !s.is_empty() {
                    emit(&s)?;
                }
                pending.drain(..n);
                if last && !pending.is_empty() {
                    pending.clear();
                    self.replaced = true;
                    emit("\u{FFFD}")?;
                }
            }
            DecKind::Latin1 => {
                for piece in src.chunks(SUB) {
                    let s: String = piece.iter().map(|&b| b as char).collect();
                    emit(&s)?;
                }
            }
            DecKind::Ascii => {
                for piece in src.chunks(SUB) {
                    let s: String = piece
                        .iter()
                        .map(|&b| {
                            if b < 0x80 {
                                b as char
                            } else {
                                self.replaced = true;
                                '\u{FFFD}'
                            }
                        })
                        .collect();
                    emit(&s)?;
                }
            }
        }
        Ok(())
    }
}

/// Incremental encoder: UTF-8 text in (whole characters), target bytes out. No BOM is written.
pub struct StreamEncoder {
    kind: EncKind,
    scratch: Vec<u8>,
    /// Characters the target cannot represent (each written as `?`).
    pub unmappable: usize,
}
enum EncKind {
    Utf8,
    Utf16 { be: bool },
    Utf32 { be: bool },
    Latin1,
    Ascii,
    Rs(encoding_rs::Encoder),
}

impl StreamEncoder {
    pub fn new(codec: Codec) -> Self {
        let kind = match codec {
            Codec::Rs(e) if e == encoding_rs::UTF_8 => EncKind::Utf8,
            Codec::Rs(e) if e == encoding_rs::UTF_16LE => EncKind::Utf16 { be: false },
            Codec::Rs(e) if e == encoding_rs::UTF_16BE => EncKind::Utf16 { be: true },
            Codec::Rs(e) => EncKind::Rs(e.new_encoder()),
            Codec::Utf32 { be } => EncKind::Utf32 { be },
            Codec::Latin1 => EncKind::Latin1,
            Codec::Ascii => EncKind::Ascii,
        };
        Self { kind, scratch: Vec::new(), unmappable: 0 }
    }

    pub fn push(&mut self, text: &str, last: bool, sink: &mut dyn FnMut(&[u8]) -> io::Result<()>) -> io::Result<()> {
        let latin = matches!(self.kind, EncKind::Latin1);
        match &mut self.kind {
            EncKind::Utf8 => {
                if !text.is_empty() {
                    sink(text.as_bytes())?;
                }
            }
            EncKind::Utf16 { be } => {
                self.scratch.clear();
                for u in text.encode_utf16() {
                    self.scratch.extend_from_slice(&if *be { u.to_be_bytes() } else { u.to_le_bytes() });
                    if self.scratch.len() >= SUB {
                        sink(&self.scratch)?;
                        self.scratch.clear();
                    }
                }
                if !self.scratch.is_empty() {
                    sink(&self.scratch)?;
                }
            }
            EncKind::Utf32 { be } => {
                self.scratch.clear();
                for c in text.chars() {
                    let u = c as u32;
                    self.scratch.extend_from_slice(&if *be { u.to_be_bytes() } else { u.to_le_bytes() });
                    if self.scratch.len() >= SUB {
                        sink(&self.scratch)?;
                        self.scratch.clear();
                    }
                }
                if !self.scratch.is_empty() {
                    sink(&self.scratch)?;
                }
            }
            EncKind::Latin1 | EncKind::Ascii => {
                self.scratch.clear();
                for c in text.chars() {
                    let lim = if latin { 0x100 } else { 0x80 };
                    if (c as u32) < lim {
                        self.scratch.push(c as u32 as u8);
                    } else {
                        self.unmappable += 1;
                        self.scratch.push(b'?');
                    }
                    if self.scratch.len() >= SUB {
                        sink(&self.scratch)?;
                        self.scratch.clear();
                    }
                }
                if !self.scratch.is_empty() {
                    sink(&self.scratch)?;
                }
            }
            EncKind::Rs(enc) => {
                if self.scratch.len() < SUB {
                    self.scratch.resize(SUB, 0);
                }
                let mut rest = text;
                loop {
                    let (res, read, written) = enc.encode_from_utf8_without_replacement(rest, &mut self.scratch, last);
                    if written > 0 {
                        sink(&self.scratch[..written])?;
                    }
                    rest = &rest[read..];
                    match res {
                        encoding_rs::EncoderResult::InputEmpty => break,
                        encoding_rs::EncoderResult::OutputFull => {}
                        encoding_rs::EncoderResult::Unmappable(_) => {
                            // `read` already includes the unmappable char; write '?' through the
                            // encoder so a stateful target (ISO-2022-JP) stays consistent.
                            self.unmappable += 1;
                            let (_, _, w) = enc.encode_from_utf8_without_replacement("?", &mut self.scratch, false);
                            if w > 0 {
                                sink(&self.scratch[..w])?;
                            }
                        }
                    }
                }
            }
        }
        Ok(())
    }
}

/// Re-joins UTF-8 characters cut by a chunk boundary. Invalid UTF-8 is an error (the lazy store
/// guarantees valid text for an editable document).
#[derive(Default)]
pub struct Utf8Joiner {
    carry: Vec<u8>,
    buf: Vec<u8>,
}
impl Utf8Joiner {
    pub fn push(&mut self, bytes: &[u8], emit: &mut dyn FnMut(&str) -> io::Result<()>) -> io::Result<()> {
        let data: &[u8] = if self.carry.is_empty() {
            bytes
        } else {
            self.buf.clear();
            self.buf.extend_from_slice(&self.carry);
            self.buf.extend_from_slice(bytes);
            &self.buf
        };
        let (valid, tail): (&str, Vec<u8>) = match std::str::from_utf8(data) {
            Ok(s) => (s, Vec::new()),
            Err(e) if e.error_len().is_none() => {
                let v = e.valid_up_to();
                (std::str::from_utf8(&data[..v]).unwrap(), data[v..].to_vec())
            }
            Err(e) => return Err(io::Error::new(io::ErrorKind::InvalidData, format!("document is not valid UTF-8 (byte {})", e.valid_up_to()))),
        };
        if !valid.is_empty() {
            emit(valid)?;
        }
        self.carry = tail;
        Ok(())
    }
    pub fn finish(&self) -> io::Result<()> {
        if self.carry.is_empty() {
            Ok(())
        } else {
            Err(io::Error::new(io::ErrorKind::InvalidData, "document ends inside a UTF-8 sequence"))
        }
    }
}

/// Streaming line-ending conversion (same result as `eol::convert` on the joined text).
pub struct EolConv {
    sep: &'static str,
    held_cr: bool,
    out: String,
}
impl EolConv {
    pub fn new(kind: EolKind) -> Self {
        Self { sep: kind.separator(), held_cr: false, out: String::new() }
    }
    /// Convert `text`; the result is valid until the next call.
    pub fn push(&mut self, text: &str) -> &str {
        self.out.clear();
        let mut last = 0;
        let b = text.as_bytes();
        let mut i = 0;
        if self.held_cr {
            self.held_cr = false;
            self.out.push_str(self.sep);
            if b.first() == Some(&b'\n') {
                i = 1;
                last = 1;
            }
        }
        while i < b.len() {
            match b[i] {
                b'\r' => {
                    self.out.push_str(&text[last..i]);
                    if i + 1 == b.len() {
                        self.held_cr = true;
                        last = b.len();
                        i = b.len();
                        break;
                    }
                    self.out.push_str(self.sep);
                    i += if b[i + 1] == b'\n' { 2 } else { 1 };
                    last = i;
                }
                b'\n' => {
                    self.out.push_str(&text[last..i]);
                    self.out.push_str(self.sep);
                    i += 1;
                    last = i;
                }
                _ => i += 1,
            }
        }
        self.out.push_str(&text[last..]);
        &self.out
    }
    /// Flush a CR held back at the end of the stream.
    pub fn finish(&mut self) -> &str {
        self.out.clear();
        if self.held_cr {
            self.held_cr = false;
            self.out.push_str(self.sep);
        }
        &self.out
    }
}

/// Line-ending census over pieces cut anywhere (a CRLF split across pieces counts once).
#[derive(Default)]
pub struct EolCounter {
    pub scan: EolScan,
    held_cr: bool,
}
impl EolCounter {
    pub fn push(&mut self, text: &str) {
        for &b in text.as_bytes() {
            if self.held_cr {
                self.held_cr = false;
                if b == b'\n' {
                    self.scan.crlf += 1;
                    continue;
                }
                self.scan.cr += 1;
            }
            match b {
                b'\r' => self.held_cr = true,
                b'\n' => self.scan.lf += 1,
                0 => self.scan.nul += 1,
                _ => {}
            }
        }
    }
    pub fn finish(mut self) -> EolScan {
        if self.held_cr {
            self.scan.cr += 1;
        }
        self.scan
    }
}

// ------------------------------------------------------------------------------------------ round-trip check

/// Compares two byte streams that arrive in different chunkings.
struct Cmp {
    lag: Vec<u8>,
    lag_is_src: bool,
    pos: u64,
    mismatch: Option<u64>,
}
impl Cmp {
    fn new() -> Self {
        Self { lag: Vec::new(), lag_is_src: true, pos: 0, mismatch: None }
    }
    fn push(&mut self, from_src: bool, b: &[u8]) {
        if self.mismatch.is_some() || b.is_empty() {
            return;
        }
        if self.lag.is_empty() || self.lag_is_src == from_src {
            self.lag_is_src = from_src;
            self.lag.extend_from_slice(b);
            return;
        }
        let n = self.lag.len().min(b.len());
        if let Some(i) = (0..n).find(|&i| self.lag[i] != b[i]) {
            self.mismatch = Some(self.pos + i as u64);
            self.lag = Vec::new();
            return;
        }
        self.lag.drain(..n);
        self.pos += n as u64;
        if b.len() > n {
            self.lag_is_src = from_src;
            self.lag.extend_from_slice(&b[n..]);
        }
    }
    fn finish(&mut self) {
        if self.mismatch.is_none() && !self.lag.is_empty() {
            self.mismatch = Some(self.pos);
        }
    }
}

// ------------------------------------------------------------------------------------------ transcode to sidecar

/// A temporary UTF-8 file, deleted when dropped.
pub struct Sidecar(tempfile::TempPath);
impl Sidecar {
    pub fn path(&self) -> &Path {
        &self.0
    }
}

pub struct Transcoded {
    pub sidecar: Sidecar,
    /// Codepage id the file was decoded with (`iso-8859-1` after a fallback).
    pub codepage: &'static str,
    /// The file started with this codepage's BOM (stripped; write it back on save).
    pub bom: bool,
    pub src_len: u64,
    pub utf8_len: u64,
    pub eol: EolScan,
    /// Saving the decoded text back in `codepage` would NOT reproduce the source bytes.
    pub lossy: bool,
    /// Absolute source offset of the first byte that does not round-trip (when `lossy`).
    pub first_mismatch: Option<u64>,
    /// `lossy` with the requested codepage, so the byte-faithful ISO-8859-1 view is used instead.
    pub fell_back_to_latin1: bool,
}

fn cancelled() -> FileIoError {
    FileIoError::Io(io::Error::new(io::ErrorKind::Interrupted, "cancelled"))
}

/// Decode `path` (in codepage `id`) into a temporary UTF-8 sidecar, streaming.
pub fn transcode_to_sidecar(path: &Path, id: &str, cfg: &StreamCfg, ctl: &mut Ctl) -> Result<Transcoded, FileIoError> {
    let cp = codepages::supported(id)?;
    let codec = Codec::for_id(cp.id)?;
    let src = FileSource::open(path)?;
    let total = ByteSource::len(&src);

    let mut head = [0u8; 4];
    let hn = src.read_at(0, &mut head);
    let bom_bytes = if cp.is_unicode() { bom_of(cp.id) } else { &[][..] };
    let bom_len = if !bom_bytes.is_empty() && head[..hn].starts_with(bom_bytes) { bom_bytes.len() } else { 0 };

    let named = match &cfg.sidecar_dir {
        Some(d) => tempfile::Builder::new().prefix("brouillon-lazy-").suffix(".utf8").tempfile_in(d)?,
        None => tempfile::Builder::new().prefix("brouillon-lazy-").suffix(".utf8").tempfile()?,
    };
    let (file, tmp_path) = named.into_parts();
    let mut out = BufWriter::with_capacity(1 << 20, file);

    let mut dec = StreamDecoder::new(codec);
    let mut enc = StreamEncoder::new(codec);
    let mut cmp = Cmp::new();
    let mut eol = EolCounter::default();
    let mut utf8_len = 0u64;
    let mut stop = false;
    let mut io_err: Option<io::Error> = None;

    let chunk = cfg.chunk.max(1);
    let mut buf = vec![0u8; chunk];
    let mut pos = bom_len as u64;
    let mut sink_text = |text: &str, cmp: &mut Cmp, enc: &mut StreamEncoder, eol: &mut EolCounter, out: &mut BufWriter<File>, utf8_len: &mut u64| -> io::Result<()> {
        out.write_all(text.as_bytes())?;
        *utf8_len += text.len() as u64;
        eol.push(text);
        enc.push(text, false, &mut |b| {
            cmp.push(false, b);
            Ok(())
        })
    };
    loop {
        let want = ((total - pos.min(total)) as usize).min(chunk);
        let n = if want == 0 { 0 } else { src.read_at(pos, &mut buf[..want]) };
        let last = n == 0 || pos + n as u64 >= total;
        cmp.push(true, &buf[..n]);
        if let Err(e) = dec.push(&buf[..n], last, &mut |t| sink_text(t, &mut cmp, &mut enc, &mut eol, &mut out, &mut utf8_len)) {
            io_err = Some(e);
            break;
        }
        pos += n as u64;
        if !ctl.tick(pos, total) {
            stop = true;
            break;
        }
        if last {
            break;
        }
    }
    if let Some(e) = io_err {
        return Err(e.into());
    }
    if stop {
        return Err(cancelled());
    }
    enc.push("", true, &mut |b| {
        cmp.push(false, b);
        Ok(())
    })?;
    cmp.finish();
    out.flush()?;
    out.get_ref().sync_all().ok();
    drop(out);
    Ok(Transcoded {
        sidecar: Sidecar(tmp_path),
        codepage: cp.id,
        bom: bom_len > 0,
        src_len: total,
        utf8_len,
        eol: eol.finish(),
        lossy: cmp.mismatch.is_some() || dec.replaced,
        first_mismatch: cmp.mismatch.map(|m| m + bom_len as u64),
        fell_back_to_latin1: false,
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LossyPolicy {
    /// Keep the best-effort decode; the document must be read-only (or saved elsewhere as UTF-8).
    ReadOnlyView,
    /// Re-read as ISO-8859-1 (every byte maps to one char): editable and byte-faithful, but mojibake.
    FallbackLatin1,
}

/// `transcode_to_sidecar`, applying `policy` when the file does not round-trip.
pub fn transcode_with_policy(path: &Path, id: &str, policy: LossyPolicy, cfg: &StreamCfg, ctl: &mut Ctl) -> Result<Transcoded, FileIoError> {
    let first = transcode_to_sidecar(path, id, cfg, ctl)?;
    if !first.lossy || policy == LossyPolicy::ReadOnlyView {
        return Ok(first);
    }
    let mut second = transcode_to_sidecar(path, "iso-8859-1", cfg, ctl)?;
    second.fell_back_to_latin1 = true;
    second.first_mismatch = first.first_mismatch;
    Ok(second)
}

// ------------------------------------------------------------------------------------------ detection

/// What to do to open a file lazily.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OpenPlan {
    /// UTF-8 (BOM or not): open the file itself with the lazy store.
    LazyUtf8,
    /// Anything else: transcode to a sidecar first.
    Transcode { codepage: &'static str },
}

/// Detect the encoding from the first MiB (BOM > valid UTF-8 > guess > `fallback_id`).
pub fn plan_open(path: &Path, fallback_id: &str) -> io::Result<OpenPlan> {
    let src = FileSource::open(path)?;
    let mut head = vec![0u8; (1 << 20).min(ByteSource::len(&src) as usize)];
    let n = src.read_at(0, &mut head);
    head.truncate(n);
    let more = ByteSource::len(&src) > n as u64;
    if more {
        if let Err(e) = std::str::from_utf8(&head) {
            if e.error_len().is_none() {
                head.truncate(e.valid_up_to()); // a character cut by the 1 MiB limit
            }
        }
    }
    let d = brouillon_core::encoding::detect(&head, fallback_id);
    Ok(if d.id == "utf-8" { OpenPlan::LazyUtf8 } else { OpenPlan::Transcode { codepage: d.id } })
}

// ------------------------------------------------------------------------------------------ encode (save back)

#[derive(Debug, Clone, Copy)]
pub struct EncodeSpec<'a> {
    pub codepage: &'a str,
    pub bom: bool,
    /// Convert line endings on the fly (`None` = as is).
    pub eol: Option<EolKind>,
}

/// Stream `range` of a UTF-8 source through the encoder of `spec` into `sink` (BOM first). Returns
/// the number of unmappable characters (written as `?`). Memory: one `cfg.chunk` plus codec scratch.
pub fn encode_to_sink(
    src: &dyn ByteSource,
    range: Range<u64>,
    spec: &EncodeSpec,
    cfg: &StreamCfg,
    ctl: &mut Ctl,
    sink: &mut dyn FnMut(&[u8]) -> io::Result<()>,
) -> Result<usize, FileIoError> {
    let cp = codepages::supported(spec.codepage)?;
    let mut enc = StreamEncoder::new(Codec::for_id(cp.id)?);
    if spec.bom && cp.is_unicode() {
        sink(bom_of(cp.id))?;
    }
    let mut joiner = Utf8Joiner::default();
    let mut eolc = spec.eol.map(EolConv::new);
    let total = range.end.min(src.len()).saturating_sub(range.start);
    let start = range.start;
    let mut err: Option<io::Error> = None;
    let mut cancelled_flag = false;
    read_blocks(src, range, cfg.chunk, &mut |pos, bytes| {
        let r = joiner.push(bytes, &mut |t| match eolc.as_mut() {
            Some(c) => enc.push(c.push(t), false, sink),
            None => enc.push(t, false, sink),
        });
        if let Err(e) = r {
            err = Some(e);
            return false;
        }
        if !ctl.tick(pos + bytes.len() as u64 - start, total) {
            cancelled_flag = true;
            return false;
        }
        true
    });
    if let Some(e) = err {
        return Err(e.into());
    }
    if cancelled_flag {
        return Err(cancelled());
    }
    joiner.finish()?;
    if let Some(c) = eolc.as_mut() {
        let tail = c.finish().to_string();
        if !tail.is_empty() {
            enc.push(&tail, false, sink)?;
        }
    }
    enc.push("", true, sink)?;
    Ok(enc.unmappable)
}

/// Unmappable characters a save would replace (nothing is written).
pub fn count_unmappable_stream(src: &dyn ByteSource, spec: &EncodeSpec, cfg: &StreamCfg, ctl: &mut Ctl) -> Result<usize, FileIoError> {
    let cp = codepages::supported(spec.codepage)?;
    if cp.is_unicode() {
        return Ok(0);
    }
    encode_to_sink(src, 0..src.len(), &EncodeSpec { bom: false, eol: None, ..*spec }, cfg, ctl, &mut |_| Ok(()))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SaveStats {
    pub unmappable: usize,
    pub bytes_written: u64,
}

/// Atomic, durable, streaming save of a UTF-8 `src` into `path` in `spec`'s encoding. A lossy
/// conversion is refused with `FileIoError::Lossy` unless `accept_lossy` (checked with a first
/// streaming pass, nothing is written then).
pub fn save_encoded_file(
    path: &Path,
    src: &dyn ByteSource,
    spec: &EncodeSpec,
    accept_lossy: bool,
    cfg: &StreamCfg,
    ctl: &mut Ctl,
) -> Result<SaveStats, FileIoError> {
    let cp = codepages::supported(spec.codepage)?;
    if !accept_lossy && !cp.is_unicode() {
        let n = count_unmappable_stream(src, spec, cfg, ctl)?;
        if n > 0 {
            return Err(FileIoError::Lossy { unmappable: n, codepage: cp.id });
        }
    }
    let mut stats = SaveStats { unmappable: 0, bytes_written: 0 };
    let mut keep: Option<FileIoError> = None;
    let r = crate::durable::durable_write_with(path, |f| {
        let mut w = BufWriter::with_capacity(1 << 20, &mut *f);
        let mut written = 0u64;
        let res = encode_to_sink(src, 0..src.len(), spec, cfg, ctl, &mut |b| {
            written += b.len() as u64;
            w.write_all(b)
        });
        match res {
            Ok(n) => {
                stats = SaveStats { unmappable: n, bytes_written: written };
                w.flush()
            }
            Err(FileIoError::Io(e)) => Err(e),
            Err(other) => {
                keep = Some(other);
                Err(io::Error::new(io::ErrorKind::Other, "encode failed"))
            }
        }
    });
    if let Some(e) = keep {
        return Err(e);
    }
    r?;
    Ok(stats)
}

/// Save a lazy/rope store in its original encoding and leave the store clean:
/// 1. the document is encoded straight from the store into `path` (atomic);
/// 2. only then `store.save(sidecar_path)` rewrites the UTF-8 sidecar, which makes the store
///    clean, rebases its pieces on the sidecar and frees the old base (same as a normal save).
///
/// Step 2 is one extra sequential write of the UTF-8 text; a `mark_saved()` on the lazy store would
/// remove it (listed in the report as a foundation-agent item).
pub fn save_store_encoded(
    store: &mut dyn brouillon_textstore_spike::store::TextStore,
    path: &Path,
    sidecar_path: &Path,
    spec: &EncodeSpec,
    accept_lossy: bool,
    cfg: &StreamCfg,
    ctl: &mut Ctl,
) -> Result<SaveStats, FileIoError> {
    let stats = save_encoded_file(path, &StoreSource(&*store), spec, accept_lossy, cfg, ctl)?;
    store.save(sidecar_path)?;
    Ok(stats)
}

// ------------------------------------------------------------------------------------------ decoded windows (Find in Files)

/// Decode `path` (codepage `id`, BOM skipped) and hand the text to `f` in windows of about `window`
/// bytes, each ending after a `\n` (the last one at the end of the file). `f` returns false to stop.
/// Memory: one window plus codec scratch. `cancelled` is polled once per chunk.
pub fn for_each_decoded_window(path: &Path, id: &str, window: usize, cancelled: &dyn Fn() -> bool, f: &mut dyn FnMut(&str) -> bool) -> io::Result<()> {
    let cp = codepages::supported(id).map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e.to_string()))?;
    let src = FileSource::open(path)?;
    let total = ByteSource::len(&src);
    let mut head = [0u8; 4];
    let hn = src.read_at(0, &mut head);
    let bom = if cp.is_unicode() { bom_of(cp.id) } else { &[][..] };
    let mut pos = if !bom.is_empty() && head[..hn].starts_with(bom) { bom.len() as u64 } else { 0 };
    let mut dec = StreamDecoder::new(Codec::for_id(cp.id).map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e.to_string()))?);
    let mut buf = vec![0u8; 256 << 10];
    let mut acc = String::new();
    let mut go = true;
    while go {
        if cancelled() {
            return Err(io::Error::new(io::ErrorKind::Interrupted, "cancelled"));
        }
        let want = ((total - pos.min(total)) as usize).min(buf.len());
        let n = if want == 0 { 0 } else { src.read_at(pos, &mut buf[..want]) };
        let last = n == 0 || pos + n as u64 >= total;
        dec.push(&buf[..n], last, &mut |t| {
            acc.push_str(t);
            while go && acc.len() >= window {
                match acc.as_bytes()[..acc.len()].iter().rposition(|&b| b == b'\n') {
                    Some(k) => {
                        go = f(&acc[..=k]);
                        acc.drain(..=k);
                    }
                    None => break, // no newline yet: keep growing
                }
            }
            Ok(())
        })?;
        pos += n as u64;
        if last {
            break;
        }
    }
    if go && !acc.is_empty() {
        f(&acc);
    }
    Ok(())
}

// ------------------------------------------------------------------------------------------ the one-call open

/// A document opened lazily in any encoding.
pub struct LazyDoc {
    pub store: brouillon_textstore_spike::lazy::LazyStore,
    /// Encoding to save back in.
    pub codepage: &'static str,
    pub bom: bool,
    pub eol: Option<EolScan>,
    /// Saving back would not be byte-faithful: treat the document as READ-ONLY (the store itself does
    /// not know; the caller must refuse edits and saves in `codepage`).
    pub read_only: bool,
    /// The temporary UTF-8 sidecar the store reads (None for a plain UTF-8 file). Keep it alive as
    /// long as the store; drop = delete.
    pub sidecar: Option<Sidecar>,
    /// Status-bar text explaining a transcoded/read-only/fallback view.
    pub note: Option<String>,
}

/// Open `path` lazily whatever its encoding: UTF-8 directly (O(1)), anything else through the
/// sidecar. `forced` = "Open As" codepage id.
pub fn open_lazy_document(
    path: &Path,
    forced: Option<&str>,
    fallback_id: &str,
    policy: LossyPolicy,
    cfg: &StreamCfg,
    lazy_cfg: brouillon_textstore_spike::lazy::Config,
    ctl: &mut Ctl,
) -> Result<LazyDoc, FileIoError> {
    use brouillon_textstore_spike::lazy::LazyStore;
    let plan = match forced {
        Some(id) => {
            let cp = codepages::supported(id)?;
            if cp.id == "utf-8" {
                OpenPlan::LazyUtf8
            } else {
                OpenPlan::Transcode { codepage: cp.id }
            }
        }
        None => plan_open(path, fallback_id)?,
    };
    match plan {
        OpenPlan::LazyUtf8 => {
            let mut head = [0u8; 3];
            let n = FileSource::open(path)?.read_at(0, &mut head);
            let bom = head[..n] == [0xEF, 0xBB, 0xBF];
            let store = LazyStore::open(path, lazy_cfg)?;
            Ok(LazyDoc { store, codepage: "utf-8", bom, eol: None, read_only: false, sidecar: None, note: None })
        }
        OpenPlan::Transcode { codepage } => {
            let t = transcode_with_policy(path, codepage, policy, cfg, ctl)?;
            let store = LazyStore::open(t.sidecar.path(), lazy_cfg)?;
            let label = codepages::find(codepage).map(|c| c.label).unwrap_or(codepage);
            let (read_only, note) = if t.fell_back_to_latin1 {
                (false, format!("{label} could not represent every byte: shown as ISO-8859-1 (byte-faithful) from a temporary UTF-8 view"))
            } else if t.lossy {
                (true, format!("Read-only: this file is not valid {label} (first bad byte at offset {}); shown from a temporary UTF-8 view", t.first_mismatch.unwrap_or(0)))
            } else {
                (false, format!("Opened from {label} through a temporary UTF-8 view; saved back as {label}"))
            };
            Ok(LazyDoc { store, codepage: t.codepage, bom: t.bom, eol: Some(t.eol), read_only, sidecar: Some(t.sidecar), note: Some(note) })
        }
    }
}

// ------------------------------------------------------------------------------------------ editor integration

static KEPT: std::sync::Mutex<Vec<Sidecar>> = std::sync::Mutex::new(Vec::new());
/// Sidecars kept alive for open documents; the oldest is deleted beyond this many (a deleted file stays
/// readable through the handle the lazy store already holds, on Windows and Unix alike).
const KEEP_SIDECARS: usize = 8;

/// Keep `s` alive for the rest of the session (bounded) and return its path.
pub fn keep_sidecar(s: Sidecar) -> PathBuf {
    let path = s.path().to_path_buf();
    let mut k = KEPT.lock().unwrap_or_else(|e| e.into_inner());
    k.push(s);
    while k.len() > KEEP_SIDECARS {
        k.remove(0);
    }
    path
}

/// Delete sidecars a crashed session left in the temp dir (older than `age`).
pub fn sweep_stale_sidecars(age: std::time::Duration) {
    let Ok(rd) = std::fs::read_dir(std::env::temp_dir()) else { return };
    for e in rd.flatten() {
        let name = e.file_name().to_string_lossy().into_owned();
        if name.starts_with("brouillon-lazy-") && name.ends_with(".utf8") {
            let old = e.metadata().and_then(|m| m.modified()).ok().and_then(|t| t.elapsed().ok()).is_some_and(|d| d > age);
            if old {
                let _ = std::fs::remove_file(e.path());
            }
        }
    }
}

/// Large non-UTF-8 file (UTF-16/32 or a legacy codepage) as a lazy buffer: transcoded once to a sidecar,
/// opened lazily, metadata set so Save writes it back in the original encoding (see
/// `brouillon::wiring::save_document_lazy_encoded`). `Ok(None)` = not a case for this path.
/// A file that does not round-trip is opened as ISO-8859-1 (byte-faithful), like the in-memory path.
pub fn open_transcoded_buffer(path: &Path) -> io::Result<Option<(crate::buffer::Buffer, crate::tabs::DocMeta)>> {
    let OpenPlan::Transcode { codepage } = plan_open(path, "windows-1252")? else { return Ok(None) };
    sweep_stale_sidecars(std::time::Duration::from_secs(24 * 3600));
    let cancel = crate::search::CancelToken::new();
    let t = transcode_with_policy(path, codepage, LossyPolicy::FallbackLatin1, &StreamCfg::default(), &mut Ctl::new(&cancel)).map_err(|e| match e {
        FileIoError::Io(e) => e,
        other => io::Error::other(other.to_string()),
    })?;
    let doc = crate::file_io::LoadedDoc { text: String::new(), codepage: t.codepage, bom: t.bom, eol: t.eol, lossy: false };
    let sidecar = keep_sidecar(t.sidecar);
    let buffer = crate::buffer::Buffer::open_lazy(&sidecar)?;
    Ok(Some((buffer, crate::tabs::DocMeta::from_loaded(&doc))))
}

/// Write the text of a lazy buffer to a fresh sidecar and return its path: what `lazy_rebase_after_external_save`
/// must be given after a save in a non-UTF-8 encoding (the saved file is not UTF-8, so it cannot be the base).
pub fn write_rebase_sidecar(buf: &crate::buffer::Buffer) -> io::Result<PathBuf> {
    let named = tempfile::Builder::new().prefix("brouillon-lazy-").suffix(".utf8").tempfile()?;
    let (file, tmp) = named.into_parts();
    let mut w = BufWriter::with_capacity(1 << 20, file);
    buf.write_to(&mut w)?;
    w.flush()?;
    drop(w);
    Ok(keep_sidecar(Sidecar(tmp)))
}

// ------------------------------------------------------------------------------------------ tests

#[cfg(test)]
mod tests {
    use super::*;
    use crate::search::CancelToken;
    use brouillon_core::eol;

    fn ctl_none(c: &CancelToken) -> Ctl<'_> {
        Ctl::new(c)
    }

    /// Reference full decode/encode through the in-memory path.
    fn ref_decode(id: &str, bytes: &[u8]) -> String {
        codepages::decode(codepages::supported(id).unwrap(), bytes).unwrap()
    }

    fn stream_decode(id: &str, bytes: &[u8], chunk: usize) -> (String, bool) {
        let mut d = StreamDecoder::new(Codec::for_id(id).unwrap());
        let mut out = String::new();
        let chunks: Vec<&[u8]> = bytes.chunks(chunk).collect();
        if chunks.is_empty() {
            d.push(&[], true, &mut |t| {
                out.push_str(t);
                Ok(())
            })
            .unwrap();
        }
        for (i, c) in chunks.iter().enumerate() {
            d.push(c, i + 1 == chunks.len(), &mut |t| {
                out.push_str(t);
                Ok(())
            })
            .unwrap();
        }
        (out, d.replaced)
    }

    fn stream_encode(id: &str, text: &str, chunk_chars: usize) -> (Vec<u8>, usize) {
        let mut e = StreamEncoder::new(Codec::for_id(id).unwrap());
        let mut out = Vec::new();
        let chars: Vec<char> = text.chars().collect();
        let pieces: Vec<String> = chars.chunks(chunk_chars.max(1)).map(|c| c.iter().collect()).collect();
        for p in &pieces {
            e.push(p, false, &mut |b| {
                out.extend_from_slice(b);
                Ok(())
            })
            .unwrap();
        }
        e.push("", true, &mut |b| {
            out.extend_from_slice(b);
            Ok(())
        })
        .unwrap();
        (out, e.unmappable)
    }

    const MIXED: &str = "plain ascii\r\nline two \u{e9}\u{e8}\u{e0} caf\u{e9}\n\u{20ac} \u{4e2d}\u{6587} \u{65e5}\u{672c}\u{8a9e} \u{d55c}\u{ad6d}\u{c5b4} \u{1f600} \u{10348}\nend";

    #[test]
    fn utf16_and_utf32_round_trip_at_every_chunk_size_including_surrogates() {
        for id in ["utf-16le", "utf-16be", "utf-32le", "utf-32be", "utf-8"] {
            let (bytes, un) = stream_encode(id, MIXED, 1000);
            assert_eq!(un, 0);
            assert_eq!(bytes, codepages::encode(codepages::supported(id).unwrap(), MIXED).unwrap().bytes, "{id} encode");
            for chunk in 1..=9 {
                let (s, rep) = stream_decode(id, &bytes, chunk);
                assert_eq!(s, MIXED, "{id} decode chunk {chunk}");
                assert!(!rep, "{id}");
            }
            for ce in [1, 2, 3, 5] {
                assert_eq!(stream_encode(id, MIXED, ce).0, bytes, "{id} encode in {ce}-char pieces");
            }
        }
    }

    #[test]
    fn every_supported_codepage_matches_the_in_memory_path_at_tiny_chunks() {
        // Text each table can mostly represent; unmappable chars exercise the '?' path.
        for cp in codepages::TABLE.iter().filter(|c| c.is_supported()) {
            let reference_bytes = codepages::encode(cp, MIXED).unwrap();
            for ce in [1usize, 2, 3, 7, 1000] {
                let (b, un) = stream_encode(cp.id, MIXED, ce);
                assert_eq!(b, reference_bytes.bytes, "{} encode pieces of {ce}", cp.id);
                assert_eq!(un, reference_bytes.unmappable, "{} unmappable", cp.id);
            }
            let want = ref_decode(cp.id, &reference_bytes.bytes);
            for chunk in 1..=7 {
                let (s, _) = stream_decode(cp.id, &reference_bytes.bytes, chunk);
                assert_eq!(s, want, "{} decode chunk {chunk}", cp.id);
            }
        }
    }

    #[test]
    fn cjk_legacy_codepages_survive_every_split_point() {
        let text = "\u{65e5}\u{672c}\u{8a9e}abc\u{6f22}\u{5b57}\u{30c6}\u{30b9}\u{30c8}\n\u{4e2d}\u{6587}\u{7b80}\u{4f53}";
        for id in ["shift_jis", "euc-jp", "gbk", "gb18030", "big5", "euc-kr", "iso-2022-jp"] {
            let cp = codepages::supported(id).unwrap();
            let enc = codepages::encode(cp, text).unwrap();
            let want = ref_decode(id, &enc.bytes);
            for chunk in 1..=6 {
                assert_eq!(stream_decode(id, &enc.bytes, chunk).0, want, "{id} chunk {chunk}");
            }
        }
        // ISO-2022-JP: the streamed encoder keeps its state across pieces; the bytes decode to the same text.
        let jp = "\u{65e5}\u{672c}\u{8a9e} abc \u{30c6}\u{30b9}\u{30c8}";
        let one = stream_encode("iso-2022-jp", jp, 1000).0;
        let split = stream_encode("iso-2022-jp", jp, 1).0;
        assert_eq!(ref_decode("iso-2022-jp", &one), jp);
        assert_eq!(ref_decode("iso-2022-jp", &split), jp);
    }

    #[test]
    fn invalid_input_is_flagged() {
        // lone high surrogate, odd trailing byte, bad UTF-32 code point, bad Shift_JIS lead byte.
        assert!(stream_decode("utf-16le", &[0x00, 0xD8, b'a', 0], 2).1);
        assert!(stream_decode("utf-16le", &[b'a', 0, b'b'], 1).1);
        assert!(stream_decode("utf-32le", &[0, 0, 0x11, 0], 3).1);
        assert!(stream_decode("utf-32le", &[b'a', 0, 0], 2).1);
        assert!(stream_decode("us-ascii", &[b'a', 0xE9], 1).1);
        assert!(stream_decode("shift_jis", &[b'a', 0x81], 1).1);
    }

    #[test]
    fn eol_conversion_streams_like_the_whole_text_version() {
        let text = "a\r\nb\nc\rd\r\n\r\ne\n\nf\r";
        for kind in [EolKind::Lf, EolKind::Crlf, EolKind::Cr] {
            let want = eol::convert(text, kind);
            for step in 1..=5 {
                let mut c = EolConv::new(kind);
                let mut got = String::new();
                let chars: Vec<char> = text.chars().collect();
                for p in chars.chunks(step) {
                    let s: String = p.iter().collect();
                    got.push_str(c.push(&s));
                }
                got.push_str(c.finish());
                assert_eq!(got, want, "{kind:?} step {step}");
            }
        }
    }

    #[test]
    fn eol_counter_matches_scan_across_splits() {
        let text = "a\r\nb\nc\rd\r\n\r\ne\0\n\nf\r";
        let want = eol::scan(text);
        for step in 1..=6 {
            let mut c = EolCounter::default();
            for p in text.as_bytes().chunks(step) {
                c.push(std::str::from_utf8(p).unwrap());
            }
            assert_eq!(c.finish(), want, "step {step}");
        }
    }

    #[test]
    fn unmappable_becomes_question_mark_and_is_counted() {
        let (b, n) = stream_encode("windows-1252", "a\u{4e2d}b\u{4e2d}", 1);
        assert_eq!(b, b"a?b?");
        assert_eq!(n, 2);
        let (b, n) = stream_encode("us-ascii", "\u{e9}x", 1);
        assert_eq!((b.as_slice(), n), (&b"?x"[..], 1));
    }

    fn write(dir: &Path, name: &str, bytes: &[u8]) -> PathBuf {
        let p = dir.join(name);
        std::fs::write(&p, bytes).unwrap();
        p
    }

    #[test]
    fn transcode_and_save_back_are_byte_exact_for_every_codepage_at_tiny_chunks() {
        let dir = tempfile::tempdir().unwrap();
        let cancel = CancelToken::new();
        let big: String = MIXED.repeat(40);
        let mut tested = 0;
        for cp in codepages::TABLE.iter().filter(|c| c.is_supported()) {
            let enc = codepages::encode(cp, &big).unwrap();
            let mut variants: Vec<(String, Vec<u8>)> = vec![(cp.id.into(), enc.bytes.clone())];
            if cp.is_unicode() {
                let mut with_bom = bom_of(cp.id).to_vec();
                with_bom.extend_from_slice(&enc.bytes);
                variants.push((format!("{}+bom", cp.id), with_bom));
            }
            for (name, bytes) in variants {
                let src = write(dir.path(), &format!("{name}.txt"), &bytes);
                for chunk in [1usize, 3, 5, 64, 4096] {
                    let cfg = StreamCfg { chunk, sidecar_dir: Some(dir.path().into()) };
                    let t = transcode_to_sidecar(&src, cp.id, &cfg, &mut ctl_none(&cancel)).unwrap();
                    let utf8 = std::fs::read(t.sidecar.path()).unwrap();
                    let full = ref_decode(cp.id, &bytes[if t.bom { bom_of(cp.id).len() } else { 0 }..]);
                    assert_eq!(String::from_utf8(utf8).unwrap(), full, "{name} chunk {chunk}");
                    assert_eq!(t.bom, name.ends_with("+bom"), "{name}");
                    assert_eq!(t.eol, eol::scan(&full), "{name} eol");
                    // The in-memory reference decides whether this table can round-trip MIXED.
                    let mem_lossy = codepages::encode(cp, &full).unwrap().bytes != bytes[if t.bom { bom_of(cp.id).len() } else { 0 }..];
                    assert_eq!(t.lossy, mem_lossy, "{name} lossy flag, chunk {chunk}");
                    if !t.lossy {
                        let out = dir.path().join(format!("{name}.out"));
                        let spec = EncodeSpec { codepage: cp.id, bom: t.bom, eol: None };
                        let side = FileSource::open(t.sidecar.path()).unwrap();
                        save_encoded_file(&out, &side, &spec, false, &cfg, &mut ctl_none(&cancel)).unwrap();
                        assert_eq!(std::fs::read(&out).unwrap(), bytes, "{name} chunk {chunk} byte-exact");
                        tested += 1;
                    }
                }
            }
        }
        assert!(tested > 100, "only {tested} exact round trips exercised");
    }

    #[test]
    fn lossy_file_is_detected_with_offset_and_policy_applies() {
        let dir = tempfile::tempdir().unwrap();
        let cancel = CancelToken::new();
        // Shift_JIS text with a stray 0x81 lead byte at offset 5.
        let mut bytes = b"abcde".to_vec();
        bytes.push(0x81);
        bytes.extend_from_slice(b"\nfghij\n");
        let p = write(dir.path(), "bad.txt", &bytes);
        let cfg = StreamCfg { chunk: 3, sidecar_dir: Some(dir.path().into()) };
        let t = transcode_to_sidecar(&p, "shift_jis", &cfg, &mut ctl_none(&cancel)).unwrap();
        assert!(t.lossy);
        assert_eq!(t.first_mismatch, Some(5));
        let f = transcode_with_policy(&p, "shift_jis", LossyPolicy::FallbackLatin1, &cfg, &mut ctl_none(&cancel)).unwrap();
        assert!(!f.lossy && f.fell_back_to_latin1 && f.codepage == "iso-8859-1");
        let out = dir.path().join("o.txt");
        let side = FileSource::open(f.sidecar.path()).unwrap();
        save_encoded_file(&out, &side, &EncodeSpec { codepage: "iso-8859-1", bom: false, eol: None }, false, &cfg, &mut ctl_none(&cancel)).unwrap();
        assert_eq!(std::fs::read(&out).unwrap(), bytes, "the latin1 fallback is byte-faithful");
    }

    #[test]
    fn save_refuses_lossy_unless_accepted_and_converts_eol() {
        let dir = tempfile::tempdir().unwrap();
        let cancel = CancelToken::new();
        let cfg = StreamCfg { chunk: 4, sidecar_dir: None };
        let text = "a\u{4e2d}\r\nb\nc".as_bytes().to_vec();
        let out = dir.path().join("o.txt");
        let spec = EncodeSpec { codepage: "windows-1252", bom: false, eol: Some(EolKind::Lf) };
        match save_encoded_file(&out, &text, &spec, false, &cfg, &mut ctl_none(&cancel)) {
            Err(FileIoError::Lossy { unmappable: 1, .. }) => {}
            other => panic!("{:?}", other.map(|s| s.unmappable)),
        }
        assert!(!out.exists(), "nothing written when refused");
        let st = save_encoded_file(&out, &text, &spec, true, &cfg, &mut ctl_none(&cancel)).unwrap();
        assert_eq!(st.unmappable, 1);
        assert_eq!(std::fs::read(&out).unwrap(), b"a?\nb\nc");
    }

    #[test]
    fn cancel_stops_the_transcode_and_leaves_no_file() {
        let dir = tempfile::tempdir().unwrap();
        let bytes = codepages::encode(codepages::supported("utf-16le").unwrap(), &"x".repeat(5000)).unwrap().bytes;
        let p = write(dir.path(), "c.txt", &bytes);
        let cancel = CancelToken::new();
        let mut seen = 0;
        let mut cb = |_d: u64, _t: u64| {
            seen += 1;
            if seen == 3 {
                cancel.cancel();
            }
        };
        let mut ctl = Ctl::new(&cancel).with_progress(&mut cb);
        let cfg = StreamCfg { chunk: 100, sidecar_dir: Some(dir.path().into()) };
        let r = transcode_to_sidecar(&p, "utf-16le", &cfg, &mut ctl);
        assert!(matches!(r, Err(FileIoError::Io(ref e)) if e.kind() == io::ErrorKind::Interrupted));
        let leftovers = std::fs::read_dir(dir.path()).unwrap().filter(|e| e.as_ref().unwrap().file_name().to_string_lossy().starts_with("brouillon-lazy-")).count();
        assert_eq!(leftovers, 0, "sidecar removed on cancel");
    }

    #[test]
    fn plan_open_picks_lazy_utf8_or_transcode() {
        let dir = tempfile::tempdir().unwrap();
        let p = write(dir.path(), "u8.txt", "h\u{e9}llo".as_bytes());
        assert_eq!(plan_open(&p, "windows-1252").unwrap(), OpenPlan::LazyUtf8);
        let p = write(dir.path(), "u16.txt", &[0xFF, 0xFE, b'a', 0]);
        assert_eq!(plan_open(&p, "windows-1252").unwrap(), OpenPlan::Transcode { codepage: "utf-16le" });
        let p = write(dir.path(), "l1.txt", &[b'a', 0xE9, b'b']);
        assert!(matches!(plan_open(&p, "windows-1252").unwrap(), OpenPlan::Transcode { .. }));
    }

    #[test]
    fn transcoded_buffer_is_lazy_searchable_with_multiline_patterns() {
        use crate::search::live::{run, Direction, Job};
        use crate::search::{Matcher, SearchMode, SearchOptions};
        let dir = tempfile::tempdir().unwrap();
        let text = "alpha\r\nbeta \u{e9}\r\ngamma\r\nbeta \u{e9}\r\ndelta\r\n";
        let mut bytes = bom_of("utf-16le").to_vec();
        bytes.extend(codepages::encode(codepages::supported("utf-16le").unwrap(), text).unwrap().bytes);
        let p = write(dir.path(), "u16.txt", &bytes);
        let (buf, meta) = open_transcoded_buffer(&p).unwrap().unwrap();
        assert!(buf.is_lazy());
        assert_eq!((meta.codepage, meta.bom), ("utf-16le", true));
        let snap = buf.snapshot();
        assert_eq!(snap.slice(0..snap.byte_len()), text);
        let m = Matcher::new("beta \u{e9}\\r?\\ngamma", &SearchOptions { mode: SearchMode::Regex, ..Default::default() }).unwrap();
        assert!(m.spans_lines());
        let mut last = None;
        run(&snap, &m, &Job { dir: Direction::Next(0), count: true, count_delay: std::time::Duration::ZERO }, &CancelToken::new(), &mut |p| last = Some(p.clone())).unwrap();
        let p = last.unwrap();
        assert!(!p.unsupported && p.done);
        assert_eq!(p.count, 1);
        assert_eq!(p.target, Some(7..21));
    }

    #[test]
    fn open_lazy_document_edit_and_save_back_in_utf16() {
        use brouillon_textstore_spike::lazy::Config;
        use brouillon_textstore_spike::store::TextStore;
        let dir = tempfile::tempdir().unwrap();
        let cancel = CancelToken::new();
        let text = "first line\r\nsecond \u{e9} line\r\nthird \u{1f600}\r\n";
        let mut bytes = bom_of("utf-16le").to_vec();
        bytes.extend(codepages::encode(codepages::supported("utf-16le").unwrap(), text).unwrap().bytes);
        let p = write(dir.path(), "doc16.txt", &bytes);
        let cfg = StreamCfg { chunk: 7, sidecar_dir: Some(dir.path().into()) };
        let mut doc = open_lazy_document(&p, None, "windows-1252", LossyPolicy::ReadOnlyView, &cfg, Config::default(), &mut ctl_none(&cancel)).unwrap();
        assert_eq!(doc.codepage, "utf-16le");
        assert!(doc.bom && !doc.read_only);
        assert_eq!(doc.store.slice_string(0..doc.store.byte_len()), text);
        // untouched: byte-exact
        let spec = EncodeSpec { codepage: doc.codepage, bom: doc.bom, eol: None };
        let side = dir.path().join("side.utf8");
        let out = dir.path().join("saved16.txt");
        save_store_encoded(&mut doc.store, &out, &side, &spec, false, &cfg, &mut ctl_none(&cancel)).unwrap();
        assert_eq!(std::fs::read(&out).unwrap(), bytes);
        // edit, save, re-open
        let at = doc.store.byte_of_line(1).unwrap();
        doc.store.insert(at, "NEW ").unwrap();
        assert!(doc.store.is_dirty());
        save_store_encoded(&mut doc.store, &out, &side, &spec, false, &cfg, &mut ctl_none(&cancel)).unwrap();
        assert!(!doc.store.is_dirty());
        let want = text.replacen("second", "NEW second", 1);
        let mut want_bytes = bom_of("utf-16le").to_vec();
        want_bytes.extend(codepages::encode(codepages::supported("utf-16le").unwrap(), &want).unwrap().bytes);
        assert_eq!(std::fs::read(&out).unwrap(), want_bytes);
    }
}
