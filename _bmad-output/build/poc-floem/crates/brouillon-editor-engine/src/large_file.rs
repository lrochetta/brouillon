//! Large-file profile (BK-012): above a threshold the file is memory-mapped
//! and read lazily, and costly editor features are switched off.

use std::fs::File;
use std::io;
use std::path::Path;

use memmap2::Mmap;

/// Default threshold: 200 MB.
pub const DEFAULT_THRESHOLD: u64 = 200 * 1024 * 1024;

// ---- lazy text store threshold (`[files] lazy_threshold_mb`) ------------------------------------------------

/// Default size from which a UTF-8 file opens on the lazy text store (the live-highlight limit).
pub const DEFAULT_LAZY_THRESHOLD_MB: u64 = 16;
/// A non-zero threshold is raised to this many MB: below it the rope is simply faster and every feature works.
pub const MIN_LAZY_THRESHOLD_MB: u64 = 8;

static LAZY_THRESHOLD_MB: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(DEFAULT_LAZY_THRESHOLD_MB);

/// Apply the `[files] lazy_threshold_mb` setting (0 = never).
pub fn set_lazy_threshold_mb(mb: i64) {
    let mb = if mb <= 0 { 0 } else { (mb as u64).max(MIN_LAZY_THRESHOLD_MB) };
    LAZY_THRESHOLD_MB.store(mb, std::sync::atomic::Ordering::Relaxed);
}

/// Size in bytes from which a file opens lazily; `0` = never.
pub fn lazy_threshold_bytes() -> u64 {
    LAZY_THRESHOLD_MB.load(std::sync::atomic::Ordering::Relaxed) * 1024 * 1024
}

/// Does a file of `len` bytes open on the lazy store?
pub fn use_lazy(len: u64) -> bool {
    let t = lazy_threshold_bytes();
    t > 0 && len >= t
}

/// Cheap look at the head and the tail (1 MB each) of `path`: `true` when both are valid UTF-8 (cut characters at
/// the sample edges tolerated), so the lazy store, which is UTF-8 only, is the right choice. A legacy codepage
/// file (accented text in windows-1252, say) fails here and keeps the decode-into-memory path, which can edit it.
pub fn sniff_utf8_for_lazy(path: &Path) -> io::Result<bool> {
    use std::io::{Read, Seek, SeekFrom};
    const SAMPLE: u64 = 1 << 20;
    let mut f = File::open(path)?;
    let len = f.metadata()?.len();
    let mut check = |off: u64, skip_leading: bool| -> io::Result<bool> {
        f.seek(SeekFrom::Start(off))?;
        let mut buf = Vec::new();
        (&mut f).take(SAMPLE).read_to_end(&mut buf)?;
        // a sample starting mid-character begins with up to 3 continuation bytes
        let start = if skip_leading { buf.iter().take(3).take_while(|b| **b & 0xC0 == 0x80).count() } else { 0 };
        Ok(match std::str::from_utf8(&buf[start..]) {
            Ok(_) => true,
            Err(e) => e.error_len().is_none(), // only an incomplete character at the very end
        })
    };
    let head = check(0, false)?;
    if !head || len <= SAMPLE {
        return Ok(head);
    }
    check(len.saturating_sub(SAMPLE).max(SAMPLE.min(len)), true)
}

/// Feature switches applied to a large document.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Profile {
    pub highlight: bool,
    pub bracket_match: bool,
    pub word_wrap: bool,
    pub completion: bool,
    /// Maximum undo entries kept.
    pub undo_cap: usize,
}

impl Profile {
    pub const NORMAL: Profile = Profile { highlight: true, bracket_match: true, word_wrap: true, completion: true, undo_cap: 1000 };
    pub const LARGE: Profile = Profile { highlight: false, bracket_match: false, word_wrap: false, completion: false, undo_cap: 50 };
}

pub fn is_large(len: u64, threshold: u64) -> bool {
    len >= threshold
}

/// Profile for a file of `len` bytes.
pub fn profile_for(len: u64, threshold: u64) -> Profile {
    if is_large(len, threshold) {
        Profile::LARGE
    } else {
        Profile::NORMAL
    }
}

/// Status-bar chip text explaining the profile.
pub fn chip(len: u64) -> String {
    format!("Large file ({} MB): highlighting, brackets, wrap and completion off", len / (1024 * 1024))
}

/// Read-only memory-mapped view; pages are loaded by the OS on demand, so
/// opening costs no copy of the file.
pub struct LargeFile {
    map: Mmap,
}

impl LargeFile {
    pub fn open(path: &Path) -> io::Result<Self> {
        let file = File::open(path)?;
        // SAFETY: the map is read-only; a concurrent external truncation can
        // fault, which is the accepted trade-off of mmap for huge files.
        let map = unsafe { Mmap::map(&file)? };
        Ok(Self { map })
    }

    pub fn len(&self) -> usize {
        self.map.len()
    }

    pub fn is_empty(&self) -> bool {
        self.map.is_empty()
    }

    /// Text of `start..start+len`, widened/narrowed to UTF-8 char boundaries
    /// (invalid bytes become U+FFFD). Touches only those pages.
    pub fn window(&self, start: usize, len: usize) -> String {
        let end = (start.saturating_add(len)).min(self.map.len());
        let start = start.min(end);
        String::from_utf8_lossy(&self.map[start..end]).into_owned()
    }

    /// First `n` lines (for the first screen); stops reading after them.
    pub fn head_lines(&self, n: usize) -> String {
        let mut end = 0;
        let mut seen = 0;
        for (i, &b) in self.map.iter().enumerate() {
            if b == b'\n' {
                seen += 1;
                end = i + 1;
                if seen == n {
                    break;
                }
            }
        }
        if seen < n {
            end = self.map.len();
        }
        self.window(0, end)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn threshold_selects_profile() {
        assert_eq!(profile_for(10, 100), Profile::NORMAL);
        let p = profile_for(100, 100);
        assert!(!p.highlight && !p.bracket_match && !p.word_wrap && !p.completion);
        assert!(p.undo_cap < Profile::NORMAL.undo_cap);
        assert_eq!(DEFAULT_THRESHOLD, 200 << 20);
        assert!(chip(300 << 20).contains("300 MB"));
    }

    #[test]
    fn mmap_first_screen_is_fast_and_lazy() {
        let mut f = tempfile::NamedTempFile::new().unwrap();
        let line = b"0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef\n";
        for _ in 0..(32 << 20) / line.len() {
            f.write_all(line).unwrap();
        }
        f.flush().unwrap();
        let t = std::time::Instant::now();
        let lf = LargeFile::open(f.path()).unwrap();
        let head = lf.head_lines(40);
        assert!(t.elapsed().as_millis() < 1000);
        assert_eq!(head.lines().count(), 40);
        assert!(lf.len() > 31 << 20 && !lf.is_empty());
        assert_eq!(lf.window(lf.len() - 3, 100).len(), 3);
        assert_eq!(lf.window(usize::MAX, 5), "");
    }
}
