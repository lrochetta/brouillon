//! Local journal: dirty remote buffers persisted atomically, independent of the server.
//! Layout: `<dir>/<id>.json` (entry) + `<dir>/<id>.bin` (buffer snapshot),
//! `<dir>/discarded/` (kept 7 days). Credentials never appear here.

use crate::doc::{Base, FileMeta, RemoteDoc, SyncState};
use serde::{Deserialize, Serialize};
use std::io;
use std::path::{Path, PathBuf};

pub const DISCARD_KEEP_MS: u64 = 7 * 24 * 3600 * 1000;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Entry {
    pub id: String,
    pub profile: String,
    /// Path the user opened.
    pub path: String,
    /// Real file the save targets (symlink resolved); where temp files live.
    pub target: String,
    pub base: Option<Base>,
    pub meta: FileMeta,
    pub snapshot_len: u64,
    pub snapshot_sha256: String,
    pub state: SyncState,
    /// Remote temp file we may have created (full path).
    pub temp: Option<String>,
    /// Remote "old" name used by the 3-step swap (full path).
    pub old: Option<String>,
    /// Our own leftovers from completed attempts; deleted unconditionally on repair.
    #[serde(default)]
    pub orphans: Vec<String>,
    pub in_place: bool,
    pub attempts: u32,
    pub updated_ms: u64,
}

impl Entry {
    /// Rebuild a document handle after a crash. `shadow` is empty (diff/stale
    /// detection degrade safely to Conflict).
    pub fn to_doc(&self) -> RemoteDoc {
        RemoteDoc {
            id: self.id.clone(),
            profile: self.profile.clone(),
            path: self.path.clone(),
            base: self.base.clone(),
            shadow: Vec::new(),
            meta: self.meta.clone(),
            state: self.state,
        }
    }
}

pub struct Journal {
    dir: PathBuf,
}

pub fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// Write-temp + fsync + rename in the same directory.
pub(crate) fn atomic_write(path: &Path, bytes: &[u8]) -> io::Result<()> {
    use std::io::Write;
    let dir = path.parent().unwrap_or(Path::new("."));
    std::fs::create_dir_all(dir)?;
    let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("x");
    let tmp = dir.join(format!(".{name}.{}.tmp", crate::fs::nonce()));
    let res = (|| {
        let mut f = std::fs::File::create(&tmp)?;
        f.write_all(bytes)?;
        f.sync_all()?;
        std::fs::rename(&tmp, path)
    })();
    if res.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    res
}

impl Journal {
    pub fn new(dir: impl Into<PathBuf>) -> Journal {
        Journal { dir: dir.into() }
    }

    fn json(&self, id: &str) -> PathBuf {
        self.dir.join(format!("{id}.json"))
    }
    fn bin(&self, id: &str) -> PathBuf {
        self.dir.join(format!("{id}.bin"))
    }

    /// Persist entry + snapshot (snapshot first, so an entry never points to nothing).
    pub fn put(&self, entry: &Entry, snapshot: &[u8]) -> io::Result<()> {
        atomic_write(&self.bin(&entry.id), snapshot)?;
        self.update(entry)
    }

    /// Rewrite the entry only (snapshot unchanged).
    pub fn update(&self, entry: &Entry) -> io::Result<()> {
        let json = serde_json::to_vec_pretty(entry).map_err(io::Error::other)?;
        atomic_write(&self.json(&entry.id), &json)
    }

    pub fn get(&self, id: &str) -> io::Result<Option<Entry>> {
        match std::fs::read(self.json(id)) {
            Ok(b) => serde_json::from_slice(&b).map(Some).map_err(io::Error::other),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e),
        }
    }

    pub fn snapshot(&self, id: &str) -> io::Result<Vec<u8>> {
        std::fs::read(self.bin(id))
    }

    /// All entries (unreadable ones are skipped, never deleted).
    pub fn list(&self) -> Vec<Entry> {
        let Ok(rd) = std::fs::read_dir(&self.dir) else { return Vec::new() };
        let mut v: Vec<Entry> = rd
            .flatten()
            .filter(|e| e.path().extension().is_some_and(|x| x == "json"))
            .filter_map(|e| std::fs::read(e.path()).ok())
            .filter_map(|b| serde_json::from_slice(&b).ok())
            .collect();
        v.sort_by(|a: &Entry, b| a.id.cmp(&b.id));
        v
    }

    pub fn clear(&self, id: &str) -> io::Result<()> {
        for p in [self.json(id), self.bin(id)] {
            match std::fs::remove_file(p) {
                Ok(()) => {}
                Err(e) if e.kind() == io::ErrorKind::NotFound => {}
                Err(e) => return Err(e),
            }
        }
        Ok(())
    }

    /// Move an entry to `discarded/` (kept 7 days) instead of deleting it.
    pub fn discard(&self, id: &str) -> io::Result<()> {
        let d = self.dir.join("discarded");
        std::fs::create_dir_all(&d)?;
        let ts = now_ms();
        for (src, ext) in [(self.bin(id), "bin"), (self.json(id), "json")] {
            if src.exists() {
                std::fs::rename(&src, d.join(format!("{id}-{ts:016}.{ext}")))?;
            }
        }
        Ok(())
    }

    /// Delete discarded items older than `DISCARD_KEEP_MS`.
    pub fn sweep_discarded(&self, now_ms: u64) {
        let Ok(rd) = std::fs::read_dir(self.dir.join("discarded")) else { return };
        for e in rd.flatten() {
            let name = e.file_name().to_string_lossy().to_string();
            let ts = name.split('-').nth(1).and_then(|t| t.split('.').next()).and_then(|t| t.parse::<u64>().ok());
            if ts.is_some_and(|t| now_ms.saturating_sub(t) > DISCARD_KEEP_MS) {
                let _ = std::fs::remove_file(e.path());
            }
        }
    }
}
