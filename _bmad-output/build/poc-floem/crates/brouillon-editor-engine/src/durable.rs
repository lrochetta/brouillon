//! R02 — the one durable write path.
//!
//! `durable_write` replaces a file so that, whatever happens (process killed,
//! power cut, full disk), a reader sees either the complete old content or the
//! complete new content, never a truncated mix:
//!
//! 1. the new bytes go to a temporary file in the SAME directory (same volume,
//!    so the rename below is atomic) and are `fsync`ed;
//! 2. the temp file is renamed over the target (`MoveFileEx` replace on
//!    Windows, `rename(2)` elsewhere);
//! 3. the parent directory is `fsync`ed where the OS allows it (best effort:
//!    Windows can refuse to flush a directory handle).
//!
//! Metadata: the Unix mode is copied to the new file, the Windows HIDDEN /
//! SYSTEM attributes are re-applied at creation, and a read-only target is
//! refused up front (`PermissionDenied`) rather than silently replaced. A
//! symlink is followed so the link itself survives and the data lands in its
//! target.
//!
//! In-place fallback (documented, NOT atomic): when a temp file cannot be
//! created next to the target or the final rename is refused (hard links, SMB
//! shares without delete rights, an antivirus holding the target open), the
//! bytes are written into the existing file and `fsync`ed. That keeps the file
//! identity, which is what those setups need, at the price of a small window
//! where a crash can leave a partly written file. An error from the caller's
//! own `fill` closure never triggers the fallback.

use std::fs::{self, File, OpenOptions};
use std::io::{self, Seek, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

static COUNTER: AtomicU64 = AtomicU64::new(0);

/// Atomically and durably replace `path` with `bytes`.
pub fn durable_write(path: &Path, bytes: &[u8]) -> io::Result<()> {
    durable_write_with(path, |f| f.write_all(bytes))
}

/// Like `durable_write` but the content is produced by `fill` (it may run
/// twice when the in-place fallback kicks in, so it must be restartable).
/// If `fill` fails the target keeps its previous content and no temp file is
/// left behind.
pub fn durable_write_with<F>(path: &Path, mut fill: F) -> io::Result<()>
where
    F: FnMut(&mut File) -> io::Result<()>,
{
    let target = resolve_link(path);
    let existing = fs::metadata(&target).ok();
    if existing.as_ref().is_some_and(|m| m.permissions().readonly()) {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!("{} is read-only", target.display()),
        ));
    }
    let dir = match target.parent() {
        Some(d) if !d.as_os_str().is_empty() => d.to_path_buf(),
        _ => PathBuf::from("."),
    };

    let (tmp_path, mut tmp) = match create_temp(&dir, &target, existing.as_ref()) {
        Ok(t) => t,
        Err(_) => return write_in_place(&target, &mut fill),
    };
    let mut guard = TempGuard(Some(tmp_path.clone()));
    fill(&mut tmp)?;
    tmp.flush()?;
    tmp.sync_all()?;
    #[cfg(unix)]
    if let Some(m) = &existing {
        let _ = fs::set_permissions(&tmp_path, m.permissions());
    }
    drop(tmp);
    if fs::rename(&tmp_path, &target).is_err() {
        drop(guard); // remove the temp file, then overwrite in place
        return write_in_place(&target, &mut fill);
    }
    guard.0 = None; // renamed: nothing to clean up
    sync_dir(&dir);
    Ok(())
}

/// Follow a symlink so the link survives the write. Anything unresolvable
/// (missing file, no permission) keeps the given path.
fn resolve_link(path: &Path) -> PathBuf {
    match fs::symlink_metadata(path) {
        Ok(m) if m.file_type().is_symlink() => fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf()),
        _ => path.to_path_buf(),
    }
}

struct TempGuard(Option<PathBuf>);

impl Drop for TempGuard {
    fn drop(&mut self) {
        if let Some(p) = self.0.take() {
            let _ = fs::remove_file(p);
        }
    }
}

fn create_temp(dir: &Path, target: &Path, _existing: Option<&fs::Metadata>) -> io::Result<(PathBuf, File)> {
    let name = target.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let mut last = io::Error::new(io::ErrorKind::Other, "no temp name");
    for _ in 0..8 {
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let tmp_path = dir.join(format!(".{name}.{}-{n}.tmp", std::process::id()));
        let mut opts = OpenOptions::new();
        opts.write(true).create_new(true);
        #[cfg(windows)]
        {
            use std::os::windows::fs::{MetadataExt, OpenOptionsExt};
            const HIDDEN_SYSTEM: u32 = 0x2 | 0x4;
            if let Some(m) = _existing {
                opts.attributes(m.file_attributes() & HIDDEN_SYSTEM);
            }
        }
        match opts.open(&tmp_path) {
            Ok(f) => return Ok((tmp_path, f)),
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => last = e,
            Err(e) => return Err(e),
        }
    }
    Err(last)
}

/// Documented non-atomic fallback: rewrite the existing file's content.
fn write_in_place<F>(target: &Path, fill: &mut F) -> io::Result<()>
where
    F: FnMut(&mut File) -> io::Result<()>,
{
    // No truncate on open: a failing `fill` then leaves the old head intact
    // instead of an empty file; the tail is trimmed once the new bytes are in.
    let mut f = OpenOptions::new().write(true).create(true).truncate(false).open(target)?;
    fill(&mut f)?;
    let end = f.stream_position()?;
    f.set_len(end)?;
    f.sync_all()
}

/// Flush the directory entry so the rename itself survives a power cut.
fn sync_dir(dir: &Path) {
    #[cfg(unix)]
    {
        if let Ok(d) = File::open(dir) {
            let _ = d.sync_all();
        }
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        const FILE_FLAG_BACKUP_SEMANTICS: u32 = 0x0200_0000;
        if let Ok(d) = OpenOptions::new().write(true).custom_flags(FILE_FLAG_BACKUP_SEMANTICS).open(dir) {
            let _ = d.sync_all();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::panic::{catch_unwind, AssertUnwindSafe};

    fn temps(dir: &Path) -> Vec<String> {
        fs::read_dir(dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n.ends_with(".tmp"))
            .collect()
    }

    #[test]
    fn writes_and_replaces() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("a.txt");
        durable_write(&p, b"one").unwrap();
        assert_eq!(fs::read(&p).unwrap(), b"one");
        durable_write(&p, b"second, longer").unwrap();
        assert_eq!(fs::read(&p).unwrap(), b"second, longer");
        durable_write(&p, b"x").unwrap();
        assert_eq!(fs::read(&p).unwrap(), b"x");
        assert!(temps(d.path()).is_empty());
    }

    /// Fake crash: the writer dies after `cut` bytes. The target must still be
    /// the complete old content and no temp file may linger.
    #[test]
    fn interrupted_write_keeps_old_content() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("doc.txt");
        let old = b"OLD CONTENT, fully written\n".to_vec();
        durable_write(&p, &old).unwrap();
        let new = vec![b'n'; 5000];
        for cut in [0usize, 1, 100, 4999] {
            let r = durable_write_with(&p, |f| {
                f.write_all(&new[..cut])?;
                // mid-write: the target is untouched
                assert_eq!(fs::read(&p).unwrap(), old);
                Err(io::Error::new(io::ErrorKind::Other, "simulated crash"))
            });
            assert!(r.is_err());
            assert_eq!(fs::read(&p).unwrap(), old, "cut at {cut}");
            assert!(temps(d.path()).is_empty(), "temp left after cut {cut}");
        }
        // a panic (the closest in-process stand-in for a kill) behaves the same
        let r = catch_unwind(AssertUnwindSafe(|| {
            let _ = durable_write_with(&p, |f| {
                f.write_all(b"half")?;
                panic!("killed");
            });
        }));
        assert!(r.is_err());
        assert_eq!(fs::read(&p).unwrap(), old);
        assert!(temps(d.path()).is_empty());
    }

    /// Property: whatever the old length, new length and crash point, a save
    /// never leaves a truncated or mixed file: the target is either all old or
    /// all new.
    #[test]
    fn save_never_leaves_a_truncated_file() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("prop.bin");
        let mut seed: u64 = 0x9E37_79B9_7F4A_7C15;
        let mut next = move || {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed
        };
        for round in 0..200 {
            let old: Vec<u8> = (0..(next() % 3000) as usize).map(|i| (i % 251) as u8).collect();
            let new: Vec<u8> = (0..(next() % 3000) as usize).map(|i| (i % 241) as u8 ^ 0x55).collect();
            durable_write(&p, &old).unwrap();
            let crash = next() % 2 == 0;
            let cut = if new.is_empty() { 0 } else { (next() as usize) % new.len() };
            let r = durable_write_with(&p, |f| {
                if crash {
                    f.write_all(&new[..cut])?;
                    Err(io::Error::new(io::ErrorKind::Other, "crash"))
                } else {
                    f.write_all(&new)
                }
            });
            let now = fs::read(&p).unwrap();
            if r.is_ok() {
                assert_eq!(now, new, "round {round}: success must publish the new bytes");
            } else {
                assert_eq!(now, old, "round {round}: failure must keep the old bytes");
            }
            assert!(temps(d.path()).is_empty());
        }
    }

    #[test]
    fn read_only_target_is_refused_and_untouched() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("ro.txt");
        fs::write(&p, "keep").unwrap();
        let mut perm = fs::metadata(&p).unwrap().permissions();
        perm.set_readonly(true);
        fs::set_permissions(&p, perm.clone()).unwrap();
        let e = durable_write(&p, b"nope").unwrap_err();
        assert_eq!(e.kind(), io::ErrorKind::PermissionDenied);
        assert_eq!(fs::read(&p).unwrap(), b"keep");
        #[allow(clippy::permissions_set_readonly_false)]
        perm.set_readonly(false);
        fs::set_permissions(&p, perm).unwrap();
    }

    #[test]
    fn in_place_fallback_shortens_the_file() {
        // The fallback itself: shorter content written over longer content.
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("f.txt");
        fs::write(&p, "a much longer previous content").unwrap();
        write_in_place(&p, &mut |f: &mut File| f.write_all(b"short")).unwrap();
        assert_eq!(fs::read(&p).unwrap(), b"short");
    }

    #[cfg(unix)]
    #[test]
    fn keeps_mode_and_follows_symlinks() {
        use std::os::unix::fs::{symlink, PermissionsExt};
        let d = tempfile::tempdir().unwrap();
        let real = d.path().join("real.sh");
        fs::write(&real, "#!/bin/sh\n").unwrap();
        fs::set_permissions(&real, fs::Permissions::from_mode(0o750)).unwrap();
        let link = d.path().join("link.sh");
        symlink(&real, &link).unwrap();
        durable_write(&link, b"#!/bin/sh\necho hi\n").unwrap();
        assert!(fs::symlink_metadata(&link).unwrap().file_type().is_symlink());
        assert_eq!(fs::read(&real).unwrap(), b"#!/bin/sh\necho hi\n");
        assert_eq!(fs::metadata(&real).unwrap().permissions().mode() & 0o777, 0o750);
    }

    #[cfg(windows)]
    #[test]
    fn keeps_hidden_attribute() {
        use std::os::windows::fs::MetadataExt;
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("h.txt");
        fs::write(&p, "x").unwrap();
        let st = std::process::Command::new("attrib").arg("+H").arg(&p).status().unwrap();
        assert!(st.success());
        durable_write(&p, b"y").unwrap();
        assert_eq!(fs::read(&p).unwrap(), b"y");
        assert_ne!(fs::metadata(&p).unwrap().file_attributes() & 0x2, 0, "hidden attribute lost");
    }
}
