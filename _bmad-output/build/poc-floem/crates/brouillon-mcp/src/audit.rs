//! Append-only, hash-chained JSONL audit log. Metadata only: never contents, diffs or tokens.

use std::fs::{self, OpenOptions};
use std::io::{self, BufRead, BufReader, Write};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

pub const GENESIS: &str = "genesis";

/// What callers record. There is deliberately no field that can carry text content.
#[derive(Debug, Clone, Default)]
pub struct AuditEvent {
    pub client: String,
    pub tool: String,
    /// Path or doc id.
    pub target: Option<String>,
    pub size: Option<u64>,
    /// Content hash of the target at the time (a hash, not content).
    pub content_hash: Option<String>,
    /// allow | deny | prompt | accepted | rejected | ...
    pub decision: String,
    pub approver: Option<String>,
    /// ok | error code
    pub result: String,
    pub bytes: u64,
}

#[derive(Serialize, Deserialize, Clone)]
struct Entry {
    seq: u64,
    ts: u64,
    client: String,
    tool: String,
    target: Option<String>,
    size: Option<u64>,
    hash: Option<String>,
    decision: String,
    approver: Option<String>,
    result: String,
    bytes: u64,
    prev: String,
    h: String,
}

fn digest(e: &Entry) -> String {
    let mut c = e.clone();
    c.h.clear();
    let body = serde_json::to_string(&c).unwrap_or_default();
    let mut s = Sha256::new();
    s.update(c.prev.as_bytes());
    s.update(body.as_bytes());
    s.finalize().iter().map(|b| format!("{b:02x}")).collect()
}

pub struct AuditLog {
    path: PathBuf,
    max_bytes: u64,
    seq: u64,
    prev: String,
}

impl AuditLog {
    /// Open (or create) the log, resuming the chain from its last line.
    pub fn open(path: impl Into<PathBuf>, max_bytes: u64) -> io::Result<Self> {
        let path = path.into();
        if let Some(dir) = path.parent() {
            fs::create_dir_all(dir)?;
        }
        let (mut seq, mut prev) = (0, GENESIS.to_string());
        if let Ok(f) = fs::File::open(&path) {
            for line in BufReader::new(f).lines().map_while(Result::ok) {
                if let Ok(e) = serde_json::from_str::<Entry>(&line) {
                    seq = e.seq;
                    prev = e.h;
                }
            }
        }
        Ok(Self { path, max_bytes, seq, prev })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn append(&mut self, ev: &AuditEvent, now: u64) -> io::Result<()> {
        if fs::metadata(&self.path).map(|m| m.len() >= self.max_bytes).unwrap_or(false) {
            let rotated = self.path.with_extension("jsonl.1");
            let _ = fs::remove_file(&rotated);
            fs::rename(&self.path, rotated)?;
        }
        self.seq += 1;
        // Defense in depth: the log is metadata only, but a hostile path or client name could
        // still carry a credential (`C:\x\ghp_...`, `://u:p@`): every text field is redacted.
        let red = |s: &str| brouillon_secrets::redact_text(s);
        let mut e = Entry {
            seq: self.seq,
            ts: now,
            client: red(&ev.client),
            tool: red(&ev.tool),
            target: ev.target.as_deref().map(red),
            size: ev.size,
            hash: ev.content_hash.clone(),
            decision: red(&ev.decision),
            approver: ev.approver.as_deref().map(red),
            result: red(&ev.result),
            bytes: ev.bytes,
            prev: self.prev.clone(),
            h: String::new(),
        };
        e.h = digest(&e);
        let mut opts = OpenOptions::new();
        opts.create(true).append(true);
        #[cfg(unix)]
        std::os::unix::fs::OpenOptionsExt::mode(&mut opts, 0o600);
        let existed = self.path.exists();
        let mut f = opts.open(&self.path)?;
        if !existed {
            // New file (first run or after rotation): owner-only on Windows too. Best effort.
            let _ = crate::os_guard::restrict_to_owner(&self.path);
        }
        writeln!(f, "{}", serde_json::to_string(&e).map_err(io::Error::other)?)?;
        self.prev = e.h;
        Ok(())
    }
}

/// Verify the chain of one log file. `Ok(n)` = n valid entries; `Err(line)` = first bad line (1-based).
pub fn verify_chain(path: &Path) -> Result<u64, u64> {
    let f = fs::File::open(path).map_err(|_| 0u64)?;
    let (mut prev, mut n): (Option<String>, u64) = (None, 0);
    for (i, line) in BufReader::new(f).lines().enumerate() {
        let line = line.map_err(|_| i as u64 + 1)?;
        let e: Entry = serde_json::from_str(&line).map_err(|_| i as u64 + 1)?;
        if let Some(p) = &prev {
            if &e.prev != p {
                return Err(i as u64 + 1);
            }
        }
        if digest(&e) != e.h {
            return Err(i as u64 + 1);
        }
        prev = Some(e.h);
        n += 1;
    }
    Ok(n)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ev(tool: &str) -> AuditEvent {
        AuditEvent { client: "claude".into(), tool: tool.into(), target: Some("C:\\p\\a.rs".into()), size: Some(10), decision: "allow".into(), result: "ok".into(), ..Default::default() }
    }

    #[test]
    fn chain_verifies_and_resumes_across_reopen() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("audit.jsonl");
        let mut log = AuditLog::open(&p, 1 << 20).unwrap();
        log.append(&ev("read"), 1).unwrap();
        log.append(&ev("edit"), 2).unwrap();
        drop(log);
        let mut log = AuditLog::open(&p, 1 << 20).unwrap();
        log.append(&ev("search"), 3).unwrap();
        assert_eq!(verify_chain(&p), Ok(3));
    }

    #[test]
    fn tampering_is_detected() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("audit.jsonl");
        let mut log = AuditLog::open(&p, 1 << 20).unwrap();
        for t in ["read", "edit", "search"] {
            log.append(&ev(t), 1).unwrap();
        }
        let s = fs::read_to_string(&p).unwrap().replacen("\"edit\"", "\"read\"", 1);
        fs::write(&p, s).unwrap();
        assert_eq!(verify_chain(&p), Err(2));
        // Deleting a line breaks the link.
        let mut log = AuditLog::open(d.path().join("b.jsonl"), 1 << 20).unwrap();
        for t in ["read", "edit", "search"] {
            log.append(&ev(t), 1).unwrap();
        }
        let b = d.path().join("b.jsonl");
        let lines: Vec<String> = fs::read_to_string(&b).unwrap().lines().map(String::from).collect();
        fs::write(&b, format!("{}\n{}\n", lines[0], lines[2])).unwrap();
        assert_eq!(verify_chain(&b), Err(2));
    }

    #[test]
    fn rotation_keeps_the_chain_going() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("audit.jsonl");
        let mut log = AuditLog::open(&p, 300).unwrap();
        for _ in 0..6 {
            log.append(&ev("read"), 1).unwrap();
        }
        assert!(d.path().join("audit.jsonl.1").exists());
        assert!(verify_chain(&p).is_ok());
    }
}
