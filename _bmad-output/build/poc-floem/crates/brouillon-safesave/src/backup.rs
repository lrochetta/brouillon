//! Local per-profile backup store. Nothing is ever written to the server.
//! Layout: `<root>/<profile>/<hash16(path)>/<ms:016>-<sha8>-<source>.bin`.
//! Retention: at most `keep_versions`, none older than `max_age_ms` (newest always kept).
//! Rollback = read a version and run it through the normal safe save.

use crate::fs::sha256_hex;
use crate::journal::{atomic_write, now_ms};
use std::io;
use std::path::PathBuf;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BackupSource {
    BeforeSave,
    /// Forced: the real current remote bytes before an overwrite / in-place write.
    BeforeOverwrite,
}

impl BackupSource {
    fn tag(self) -> &'static str {
        match self {
            BackupSource::BeforeSave => "save",
            BackupSource::BeforeOverwrite => "overwrite",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Version {
    pub file: PathBuf,
    pub ts_ms: u64,
    pub sha8: String,
    pub source: String,
}

pub struct BackupStore {
    root: PathBuf,
    pub keep_versions: usize,
    pub max_age_ms: u64,
}

fn clean(profile: &str) -> String {
    let s: String = profile.chars().map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '_' }).collect();
    if s.is_empty() { "_".into() } else { s.chars().take(64).collect() }
}

impl BackupStore {
    pub fn new(root: impl Into<PathBuf>) -> BackupStore {
        BackupStore { root: root.into(), keep_versions: 20, max_age_ms: 30 * 24 * 3600 * 1000 }
    }

    fn dir(&self, profile: &str, path: &str) -> PathBuf {
        self.root.join(clean(profile)).join(&sha256_hex(path.as_bytes())[..16])
    }

    pub fn put(&self, profile: &str, path: &str, bytes: &[u8], source: BackupSource) -> io::Result<Version> {
        self.put_at(profile, path, bytes, source, now_ms())
    }

    pub fn put_at(&self, profile: &str, path: &str, bytes: &[u8], source: BackupSource, now: u64) -> io::Result<Version> {
        let sha8 = sha256_hex(bytes)[..8].to_string();
        // Same content as the newest version: nothing new to keep.
        if let Some(v) = self.list(profile, path).into_iter().next() {
            if v.sha8 == sha8 && std::fs::metadata(&v.file).map(|m| m.len()).ok() == Some(bytes.len() as u64) {
                return Ok(v);
            }
        }
        let file = self.dir(profile, path).join(format!("{now:016}-{sha8}-{}.bin", source.tag()));
        atomic_write(&file, bytes)?;
        self.prune(profile, path, now);
        Ok(Version { file, ts_ms: now, sha8, source: source.tag().into() })
    }

    /// Newest first.
    pub fn list(&self, profile: &str, path: &str) -> Vec<Version> {
        let Ok(rd) = std::fs::read_dir(self.dir(profile, path)) else { return Vec::new() };
        let mut v: Vec<Version> = rd
            .flatten()
            .filter_map(|e| {
                let name = e.file_name().to_string_lossy().to_string();
                let stem = name.strip_suffix(".bin")?;
                let mut p = stem.splitn(3, '-');
                let ts_ms = p.next()?.parse().ok()?;
                Some(Version { file: e.path(), ts_ms, sha8: p.next()?.into(), source: p.next()?.into() })
            })
            .collect();
        v.sort_by(|a, b| b.ts_ms.cmp(&a.ts_ms).then_with(|| b.file.cmp(&a.file)));
        v
    }

    pub fn read(&self, v: &Version) -> io::Result<Vec<u8>> {
        std::fs::read(&v.file)
    }

    pub fn prune(&self, profile: &str, path: &str, now: u64) {
        for (i, v) in self.list(profile, path).into_iter().enumerate() {
            let too_old = now.saturating_sub(v.ts_ms) > self.max_age_ms;
            if i >= self.keep_versions || (i > 0 && too_old) {
                let _ = std::fs::remove_file(&v.file);
            }
        }
    }
}
