//! Per-document sync state, base stamp and byte-format metadata.

use crate::fs::{file_name, sha256_hex, FsError, FsResult, Kind, RemoteFs, Stat};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SyncState {
    Clean,
    Dirty,
    Queued,
    Preflight,
    Uploading,
    Verify,
    Swap,
    Conflict,
    /// Server file is newer, local buffer unchanged: offer reload.
    Stale,
    RemoteGone,
    Failed,
    /// Restored from the journal after a crash; never uploaded without the user.
    Recovered,
}

/// What we saw at open: server stamp plus the hash of the bytes we received.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Base {
    pub size: u64,
    pub mtime: i64,
    pub mode: u32,
    pub sha256: String,
}

impl Base {
    pub(crate) fn from(stat: &Stat, sha256: String) -> Base {
        Base { size: stat.size, mtime: stat.mtime, mode: stat.mode, sha256 }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Eol {
    Lf,
    CrLf,
    Cr,
    None,
}

/// Byte-format facts recorded at open. The saver never transforms bytes; this is
/// carried so the editor can re-encode identically and the UI can show it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileMeta {
    /// "utf-8", "utf-16le", "utf-16be" (BOM-detected) or "unknown".
    pub encoding: String,
    pub bom: bool,
    pub eol: Eol,
    pub mixed_eol: bool,
    pub final_newline: bool,
}

impl FileMeta {
    pub fn detect(b: &[u8]) -> FileMeta {
        let (encoding, bom) = if b.starts_with(&[0xEF, 0xBB, 0xBF]) {
            ("utf-8", true)
        } else if b.starts_with(&[0xFF, 0xFE]) {
            ("utf-16le", true)
        } else if b.starts_with(&[0xFE, 0xFF]) {
            ("utf-16be", true)
        } else {
            ("utf-8", false)
        };
        let (mut crlf, mut lf, mut cr) = (0usize, 0usize, 0usize);
        let mut i = 0;
        while i < b.len() {
            match b[i] {
                b'\r' if b.get(i + 1) == Some(&b'\n') => {
                    crlf += 1;
                    i += 1;
                }
                b'\r' => cr += 1,
                b'\n' => lf += 1,
                _ => {}
            }
            i += 1;
        }
        let kinds = [(crlf, Eol::CrLf), (lf, Eol::Lf), (cr, Eol::Cr)];
        let mixed_eol = kinds.iter().filter(|k| k.0 > 0).count() > 1;
        let eol = kinds.iter().max_by_key(|k| k.0).filter(|k| k.0 > 0).map(|k| k.1).unwrap_or(Eol::None);
        let final_newline = matches!(b.last(), Some(b'\n') | Some(b'\r'));
        FileMeta { encoding: encoding.into(), bom, eol, mixed_eol, final_newline }
    }
}

/// Stable id of a (profile, path) pair; names the journal files.
pub fn doc_id(profile: &str, path: &str) -> String {
    sha256_hex(format!("{profile}\0{path}").as_bytes())[..16].to_string()
}

/// An open remote document. `base == None` means a file not yet on the server.
#[derive(Debug, Clone)]
pub struct RemoteDoc {
    pub id: String,
    pub profile: String,
    /// Path the user opened (may be a symlink).
    pub path: String,
    pub base: Option<Base>,
    /// Server bytes at open (for diff / stale detection). Empty after `from_entry`.
    pub shadow: Vec<u8>,
    pub meta: FileMeta,
    pub state: SyncState,
}

impl RemoteDoc {
    pub fn new_file(profile: &str, path: &str) -> RemoteDoc {
        RemoteDoc {
            id: doc_id(profile, path),
            profile: profile.into(),
            path: path.into(),
            base: None,
            shadow: Vec::new(),
            meta: FileMeta::detect(b""),
            state: SyncState::Dirty,
        }
    }

    /// Open an existing remote file: stat, read, stat again (must agree), hash.
    pub fn open(fs: &mut dyn RemoteFs, profile: &str, path: &str) -> FsResult<(Vec<u8>, RemoteDoc)> {
        let (real, s1) = resolve_file(fs, path)?;
        let bytes = fs.read(&real)?;
        let s2 = fs.stat(&real)?.ok_or(FsError::NotFound)?;
        if (s1.size, s1.mtime) != (s2.size, s2.mtime) || s2.size != bytes.len() as u64 {
            return Err(FsError::Other("file changed while opening".into()));
        }
        let doc = RemoteDoc {
            id: doc_id(profile, path),
            profile: profile.into(),
            path: path.into(),
            base: Some(Base::from(&s2, sha256_hex(&bytes))),
            shadow: bytes.clone(),
            meta: FileMeta::detect(&bytes),
            state: SyncState::Clean,
        };
        Ok((bytes, doc))
    }

    pub fn file_name(&self) -> &str {
        file_name(&self.path)
    }
}

/// Resolve `path` to the real regular file (following a symlink) and stat it.
pub(crate) fn resolve_file(fs: &mut dyn RemoteFs, path: &str) -> FsResult<(String, Stat)> {
    let s = fs.stat(path)?.ok_or(FsError::NotFound)?;
    let real = if s.kind == Kind::Symlink { fs.realpath(path)? } else { path.to_string() };
    let rs = fs.stat(&real)?.ok_or(FsError::NotFound)?;
    if rs.kind != Kind::File {
        return Err(FsError::Other("not a regular file".into()));
    }
    Ok((real, rs))
}
