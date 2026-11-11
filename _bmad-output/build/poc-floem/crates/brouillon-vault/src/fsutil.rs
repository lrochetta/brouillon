//! Crash-safe whole-file replace: temp file in the same directory, flush, atomic replace that
//! keeps exactly one `.bak` generation. On Windows: `ReplaceFileW` / `MoveFileExW`.

use std::fs::OpenOptions;
use std::io::Write;
use std::path::{Path, PathBuf};

use crate::error::VaultError;

pub(crate) fn bak_path(path: &Path) -> PathBuf {
    let mut s = path.as_os_str().to_owned();
    s.push(".bak");
    PathBuf::from(s)
}

fn file_name(path: &Path) -> String {
    path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "vault.bsv".into())
}

/// Removes leftover `<name>.tmp.*` files (already-encrypted bytes only) from an interrupted write.
pub(crate) fn clean_orphans(path: &Path) {
    let Some(dir) = path.parent() else { return };
    let prefix = format!("{}.tmp.", file_name(path));
    if let Ok(rd) = std::fs::read_dir(dir) {
        for e in rd.flatten() {
            if e.file_name().to_string_lossy().starts_with(&prefix) {
                let _ = std::fs::remove_file(e.path());
            }
        }
    }
}

/// `fault_before_replace` simulates a crash between the temp write and the replace.
pub(crate) fn atomic_write(path: &Path, bytes: &[u8], fault_before_replace: bool) -> Result<(), VaultError> {
    let dir = path.parent().filter(|p| !p.as_os_str().is_empty()).unwrap_or_else(|| Path::new("."));
    let rnd = crate::crypto::random::<4>()?;
    let tmp = dir.join(format!(
        "{}.tmp.{:02x}{:02x}{:02x}{:02x}",
        file_name(path),
        rnd[0],
        rnd[1],
        rnd[2],
        rnd[3]
    ));
    {
        let mut o = OpenOptions::new();
        o.write(true).create_new(true);
        #[cfg(windows)]
        {
            use std::os::windows::fs::OpenOptionsExt;
            o.custom_flags(0x8000_0000); // FILE_FLAG_WRITE_THROUGH
        }
        let mut f = o.open(&tmp)?;
        f.write_all(bytes)?;
        f.sync_all()?;
    }
    if fault_before_replace {
        return Err(VaultError::Io(std::io::ErrorKind::Other));
    }
    let r = replace(path, &tmp);
    if r.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    r
}

#[cfg(windows)]
#[allow(unsafe_code)]
fn replace(path: &Path, tmp: &Path) -> Result<(), VaultError> {
    use std::os::windows::ffi::OsStrExt;
    fn wide(p: &Path) -> Vec<u16> {
        p.as_os_str().encode_wide().chain(std::iter::once(0)).collect()
    }
    #[link(name = "kernel32")]
    extern "system" {
        fn ReplaceFileW(
            replaced: *const u16,
            replacement: *const u16,
            backup: *const u16,
            flags: u32,
            exclude: *mut std::ffi::c_void,
            reserved: *mut std::ffi::c_void,
        ) -> i32;
        fn MoveFileExW(from: *const u16, to: *const u16, flags: u32) -> i32;
    }
    const REPLACEFILE_WRITE_THROUGH: u32 = 0x1;
    const MOVEFILE_REPLACE_EXISTING: u32 = 0x1;
    const MOVEFILE_WRITE_THROUGH: u32 = 0x8;
    let (target, tmpw) = (wide(path), wide(tmp));
    // SAFETY: all pointers are NUL-terminated UTF-16 buffers alive for the call.
    let ok = if path.exists() {
        let bak = wide(&bak_path(path));
        unsafe {
            ReplaceFileW(
                target.as_ptr(),
                tmpw.as_ptr(),
                bak.as_ptr(),
                REPLACEFILE_WRITE_THROUGH,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
            )
        }
    } else {
        unsafe {
            MoveFileExW(tmpw.as_ptr(), target.as_ptr(), MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH)
        }
    };
    if ok == 0 {
        return Err(VaultError::Io(std::io::Error::last_os_error().kind()));
    }
    Ok(())
}

#[cfg(not(windows))]
fn replace(path: &Path, tmp: &Path) -> Result<(), VaultError> {
    if path.exists() {
        std::fs::copy(path, bak_path(path))?;
    }
    std::fs::rename(tmp, path)?;
    if let Some(dir) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
        if let Ok(d) = std::fs::File::open(dir) {
            let _ = d.sync_all();
        }
    }
    Ok(())
}
