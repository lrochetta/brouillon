//! The `TextStore` trait: the surface the editor needs from a text container, small enough that both the
//! rope-backed `Buffer` (small files) and the lazy store (huge files) can sit behind it.
//!
//! It is derived from what the engine actually calls on `Buffer`/`crop::Rope` today (see the design doc, section
//! "Buffer API surface"): byte_len, line_len, byte_of_line, line_of_byte, is_char_boundary, byte_slice, chunks
//! (streaming), insert/delete/replace, undo/redo, save, plus version/dirty. `to_string()` of the whole text is
//! deliberately NOT in the trait: the 90 `rope().to_string()` call sites are the migration cost (see doc).
//!
//! ADDITIVE CHANGES LOG (several agents code against this contract: existing method names and signatures never
//! change, only additions are allowed, and each one is listed here):
//! - `StoreError::ReadOnly` (a `StoreSnapshot` or an invalid-UTF-8 document refused a mutation).
//! - `lazy::LazyStore` is now `Clone` (cheap: copy-on-write blocks, shared append-only add buffer; the store's own
//!   undo logs are not cloned) and `Send + Sync` (atomic newline caches instead of `Cell`).
//! - `lazy::StoreSnapshot` (`LazyStore::snapshot()`): immutable `Arc` view for worker threads, implements
//!   `TextStore` read-only.
//! - `lazy::EditRec` / `LazyStore::{edit, apply_rec_inverse, apply_rec_forward}`: edit with an external history.
//! - `LazyStore::{line_start_ready, estimated_line_count, index_fraction}`: non-blocking line queries.
//! - `LazyStore::{compact_add, gc_bases_with, rebase_to_file, is_read_only, has_bom, add_len, base_path, base_len}`.
use std::io;
use std::ops::Range;
use std::path::Path;

#[derive(Debug)]
pub enum StoreError {
    /// The target refuses mutations (snapshot, or a document whose base file is not valid UTF-8).
    ReadOnly,
    OutOfRange,
    NotCharBoundary(usize),
    /// The base file is not valid UTF-8 (first bad byte offset): the store is read-only, use the decode/hex path.
    InvalidUtf8(u64),
    Io(io::Error),
}
impl std::fmt::Display for StoreError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            StoreError::ReadOnly => write!(f, "read-only"),
            StoreError::OutOfRange => write!(f, "range out of bounds"),
            StoreError::NotCharBoundary(p) => write!(f, "byte {p} is not on a UTF-8 boundary"),
            StoreError::InvalidUtf8(p) => write!(f, "file is not valid UTF-8 (first invalid byte at {p})"),
            StoreError::Io(e) => write!(f, "{e}"),
        }
    }
}
impl std::error::Error for StoreError {}
impl From<io::Error> for StoreError {
    fn from(e: io::Error) -> Self {
        StoreError::Io(e)
    }
}

pub trait TextStore {
    fn byte_len(&self) -> usize;
    /// Number of lines = number of `\n` + 1. On the lazy store this may have to finish the line index (blocking).
    fn line_count(&self) -> usize;
    /// Byte offset where line `line` starts (`None` past the end). Exact; on the lazy store it only scans up to
    /// the requested line.
    fn byte_of_line(&self, line: usize) -> Option<usize>;
    fn line_of_byte(&self, byte: usize) -> usize;
    fn is_char_boundary(&self, byte: usize) -> bool;
    /// Stream the bytes of `r` in order, in chunks of unspecified size (return false to stop). Raw bytes, so a
    /// save of untouched regions is byte-exact. Chunk edges may split a UTF-8 character.
    fn visit(&self, r: Range<usize>, f: &mut dyn FnMut(&[u8]) -> bool);
    fn replace_range(&mut self, r: Range<usize>, text: &str) -> Result<(), StoreError>;
    fn begin_group(&mut self);
    fn end_group(&mut self);
    fn undo(&mut self) -> bool;
    fn redo(&mut self) -> bool;
    fn can_undo(&self) -> bool;
    fn can_redo(&self) -> bool;
    fn version(&self) -> u64;
    fn is_dirty(&self) -> bool;
    /// Atomic streaming save (temp file in the same directory, fsync, rename). The store keeps working afterwards.
    fn save(&mut self, path: &Path) -> io::Result<()>;
    /// Heap bytes the store itself holds (not the OS file cache, not mapped views).
    fn heap_estimate(&self) -> usize;

    // ---- provided ----
    fn slice_string(&self, r: Range<usize>) -> String {
        let mut v = Vec::with_capacity(r.len().min(1 << 20));
        self.visit(r, &mut |c| {
            v.extend_from_slice(c);
            true
        });
        match String::from_utf8(v) {
            Ok(s) => s,
            Err(e) => String::from_utf8_lossy(e.as_bytes()).into_owned(),
        }
    }
    fn line_string(&self, line: usize) -> Option<String> {
        let a = self.byte_of_line(line)?;
        let b = self.byte_of_line(line + 1).unwrap_or(self.byte_len());
        Some(self.slice_string(a..b))
    }
    fn insert(&mut self, at: usize, text: &str) -> Result<(), StoreError> {
        self.replace_range(at..at, text)
    }
    fn delete(&mut self, r: Range<usize>) -> Result<(), StoreError> {
        self.replace_range(r, "")
    }
}

/// Streaming substring count (no whole-text copy): carries the last `needle.len()-1` bytes across chunks.
pub fn count_matches(s: &dyn TextStore, needle: &[u8]) -> usize {
    let n = needle.len();
    if n == 0 {
        return 0;
    }
    let finder = memchr::memmem::Finder::new(needle);
    let mut total = 0usize;
    let mut carry: Vec<u8> = Vec::with_capacity(n);
    let mut join: Vec<u8> = Vec::with_capacity(2 * n);
    s.visit(0..s.byte_len(), &mut |chunk| {
        if !carry.is_empty() {
            join.clear();
            join.extend_from_slice(&carry);
            join.extend_from_slice(&chunk[..chunk.len().min(n - 1)]);
            // a match that starts inside the carry (fully inside the carry is impossible: carry < needle)
            total += finder.find_iter(&join).filter(|&p| p < carry.len()).count();
        }
        total += finder.find_iter(chunk).count();
        // new carry = last n-1 bytes of (carry ++ chunk)
        if chunk.len() >= n - 1 {
            carry.clear();
            carry.extend_from_slice(&chunk[chunk.len() - (n - 1)..]);
        } else {
            carry.extend_from_slice(chunk);
            let drop = carry.len().saturating_sub(n - 1);
            carry.drain(..drop);
        }
        true
    });
    total
}

/// FNV-1a over the whole text, streaming (used to prove byte-exactness at scale).
pub fn hash_all(s: &dyn TextStore) -> u64 {
    let mut h = 0xcbf29ce484222325u64;
    s.visit(0..s.byte_len(), &mut |c| {
        for &b in c {
            h ^= b as u64;
            h = h.wrapping_mul(0x100000001b3);
        }
        true
    });
    h
}

/// What the view needs for one frame: byte range of `rows` lines starting at `line`, read in full.
pub fn viewport(s: &dyn TextStore, line: usize, rows: usize) -> usize {
    let a = match s.byte_of_line(line) {
        Some(a) => a,
        None => return 0,
    };
    let b = s.byte_of_line(line + rows).unwrap_or(s.byte_len());
    let mut n = 0;
    s.visit(a..b, &mut |c| {
        n += c.len();
        true
    });
    n
}
