//! `ByteSource`: random-access, read-only bytes. The one input type of the huge-file paths
//! (streaming transcoder/encoder, windowed search, hex viewer), so none of them needs the whole
//! text as a `String`/`&str`.
//!
//! A source is an immutable snapshot: its length is fixed when it is created. Implementations that
//! read a file return fewer bytes than asked when the file shrank behind our back (the callers treat
//! a short read as the end of the data, never as an error).

use std::fs::File;
use std::io;
use std::ops::Range;
use std::path::Path;
use std::sync::Arc;

pub trait ByteSource {
    fn len(&self) -> u64;
    fn is_empty(&self) -> bool {
        self.len() == 0
    }
    /// Fill `buf` with the bytes starting at `off`. Returns how many bytes were written: only fewer
    /// than `buf.len()` at the end of the source (or when the backing file shrank).
    fn read_at(&self, off: u64, buf: &mut [u8]) -> usize;
}

impl ByteSource for [u8] {
    fn len(&self) -> u64 {
        <[u8]>::len(self) as u64
    }
    fn read_at(&self, off: u64, buf: &mut [u8]) -> usize {
        let Some(rest) = usize::try_from(off).ok().and_then(|o| self.get(o..)) else { return 0 };
        let n = rest.len().min(buf.len());
        buf[..n].copy_from_slice(&rest[..n]);
        n
    }
}
impl ByteSource for Vec<u8> {
    fn len(&self) -> u64 {
        self.as_slice().len() as u64
    }
    fn read_at(&self, off: u64, buf: &mut [u8]) -> usize {
        self.as_slice().read_at(off, buf)
    }
}
impl<T: ByteSource + ?Sized> ByteSource for &T {
    fn len(&self) -> u64 {
        (**self).len()
    }
    fn read_at(&self, off: u64, buf: &mut [u8]) -> usize {
        (**self).read_at(off, buf)
    }
}
impl<T: ByteSource + ?Sized> ByteSource for Arc<T> {
    fn len(&self) -> u64 {
        (**self).len()
    }
    fn read_at(&self, off: u64, buf: &mut [u8]) -> usize {
        (**self).read_at(off, buf)
    }
}
impl<T: ByteSource + ?Sized> ByteSource for Box<T> {
    fn len(&self) -> u64 {
        (**self).len()
    }
    fn read_at(&self, off: u64, buf: &mut [u8]) -> usize {
        (**self).read_at(off, buf)
    }
}

/// A window of another source (`range` of `inner`), used to scope a search to a selection.
pub struct SubSource<'a> {
    pub inner: &'a dyn ByteSource,
    pub range: Range<u64>,
}
impl ByteSource for SubSource<'_> {
    fn len(&self) -> u64 {
        self.range.end.min(self.inner.len()).saturating_sub(self.range.start)
    }
    fn read_at(&self, off: u64, buf: &mut [u8]) -> usize {
        let avail = self.len().saturating_sub(off) as usize;
        let n = buf.len().min(avail);
        self.inner.read_at(self.range.start + off, &mut buf[..n])
    }
}

#[cfg(windows)]
fn pread(f: &File, buf: &mut [u8], off: u64) -> io::Result<usize> {
    use std::os::windows::fs::FileExt;
    f.seek_read(buf, off)
}
#[cfg(unix)]
fn pread(f: &File, buf: &mut [u8], off: u64) -> io::Result<usize> {
    use std::os::unix::fs::FileExt;
    f.read_at(buf, off)
}

/// Positional reads of a file through a shared handle (`&self`, no seek state, so it is `Sync` and
/// can be shared by worker threads). Length is read once at open.
pub struct FileSource {
    file: File,
    len: u64,
}

impl FileSource {
    pub fn open(path: &Path) -> io::Result<Self> {
        let file = File::open(path)?;
        let len = file.metadata()?.len();
        Ok(Self { file, len })
    }
}

impl ByteSource for FileSource {
    fn len(&self) -> u64 {
        self.len
    }
    fn read_at(&self, off: u64, buf: &mut [u8]) -> usize {
        let want = (self.len.saturating_sub(off) as usize).min(buf.len());
        let mut done = 0;
        while done < want {
            match pread(&self.file, &mut buf[done..want], off + done as u64) {
                Ok(0) => break,
                Ok(n) => done += n,
                Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                Err(_) => break,
            }
        }
        done
    }
}

/// Adapter over any `TextStore` (the lazy store or the rope store) for single-threaded use: the
/// bytes are the store's document bytes (UTF-8, without a UTF-8 BOM). The lazy store is `!Sync`
/// today, so a worker thread needs an immutable snapshot instead (a `FileSource` of the saved
/// file, or whatever snapshot type the store grows).
pub struct StoreSource<'a>(pub &'a dyn brouillon_textstore_spike::store::TextStore);

impl ByteSource for StoreSource<'_> {
    fn len(&self) -> u64 {
        self.0.byte_len() as u64
    }
    fn read_at(&self, off: u64, buf: &mut [u8]) -> usize {
        let end = (off as usize).saturating_add(buf.len()).min(self.0.byte_len());
        let start = (off as usize).min(end);
        let mut n = 0;
        self.0.visit(start..end, &mut |c| {
            buf[n..n + c.len()].copy_from_slice(c);
            n += c.len();
            true
        });
        n
    }
}

/// Cooperative control of a long streaming pass: cancel flag plus an optional progress callback
/// `(bytes_done, bytes_total)`, called once per window/chunk.
pub struct Ctl<'a> {
    pub cancel: &'a crate::search::CancelToken,
    pub progress: Option<&'a mut dyn FnMut(u64, u64)>,
}
impl<'a> Ctl<'a> {
    pub fn new(cancel: &'a crate::search::CancelToken) -> Self {
        Self { cancel, progress: None }
    }
    pub fn with_progress(mut self, f: &'a mut dyn FnMut(u64, u64)) -> Self {
        self.progress = Some(f);
        self
    }
    /// Report progress; false = the pass must stop (cancelled).
    pub fn tick(&mut self, done: u64, total: u64) -> bool {
        if let Some(p) = self.progress.as_mut() {
            p(done, total);
        }
        !self.cancel.is_cancelled()
    }
}

/// A buffer snapshot (rope or lazy store) is a `Send + Sync` byte source: the search workers and the
/// encoded save read it in windows, never as one string.
impl ByteSource for crate::buffer::BufferSnapshot {
    fn len(&self) -> u64 {
        self.byte_len() as u64
    }
    fn read_at(&self, off: u64, buf: &mut [u8]) -> usize {
        let total = self.byte_len();
        let start = (off as usize).min(total);
        let end = start.saturating_add(buf.len()).min(total);
        let mut n = 0;
        self.visit_bytes(start..end, &mut |c| {
            buf[n..n + c.len()].copy_from_slice(c);
            n += c.len();
            true
        });
        n
    }
}

/// Read `range` of `src` through `f` in blocks of at most `block` bytes (no allocation beyond one
/// block). Returns false if `f` stopped early.
pub fn read_blocks(src: &dyn ByteSource, range: Range<u64>, block: usize, f: &mut dyn FnMut(u64, &[u8]) -> bool) -> bool {
    let mut buf = vec![0u8; block.max(1)];
    let mut pos = range.start;
    let end = range.end.min(src.len());
    while pos < end {
        let want = ((end - pos) as usize).min(buf.len());
        let n = src.read_at(pos, &mut buf[..want]);
        if n == 0 {
            break;
        }
        if !f(pos, &buf[..n]) {
            return false;
        }
        pos += n as u64;
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slice_and_sub_sources() {
        let v: Vec<u8> = (0..100u8).collect();
        let mut b = [0u8; 10];
        assert_eq!(v.read_at(95, &mut b), 5);
        assert_eq!(&b[..5], &[95, 96, 97, 98, 99]);
        assert_eq!(v.read_at(500, &mut b), 0);
        let sub = SubSource { inner: &v, range: 10..20 };
        assert_eq!(ByteSource::len(&sub), 10);
        assert_eq!(sub.read_at(8, &mut b), 2);
        assert_eq!(&b[..2], &[18, 19]);
    }

    #[test]
    fn file_source_reads_and_short_reads_at_end() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("f.bin");
        std::fs::write(&p, (0..=255u8).cycle().take(1000).collect::<Vec<_>>()).unwrap();
        let s = FileSource::open(&p).unwrap();
        assert_eq!(ByteSource::len(&s), 1000);
        let mut b = vec![0u8; 300];
        assert_eq!(s.read_at(900, &mut b), 100);
        assert_eq!(b[0], (900 % 256) as u8);
        let mut total = 0;
        read_blocks(&s, 0..1000, 77, &mut |_, c| {
            total += c.len();
            true
        });
        assert_eq!(total, 1000);
    }
}
