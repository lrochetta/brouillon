//! Fault-injecting in-memory provider. Cloneable handle (shared state) so a test
//! can keep inspecting the "server" after the saver is gone, or poll it from a thread.
//!
//! Every provider call is numbered. `Fault { at, kind }` fires on call number `at`:
//! crash before it, crash after it was applied, crash halfway through a write
//! (half the bytes land), or a one-shot error. After a crash the server is "dead"
//! (every call returns `Crash`) until `revive()` simulates the next launch.

use crate::fs::{Caps, FsError, FsResult, Kind, RemoteFs, Stat};
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

#[derive(Debug, Clone)]
pub enum FaultKind {
    CrashBefore,
    CrashAfter,
    CrashMid,
    Fail(FsError),
}

#[derive(Debug, Clone)]
pub struct Fault {
    pub at: usize,
    pub kind: FaultKind,
}

#[derive(Debug, Clone)]
struct Node {
    kind: Kind,
    data: Vec<u8>,
    mode: u32,
    mtime: i64,
    link: String,
}

struct Inner {
    nodes: BTreeMap<String, Node>,
    clock: i64,
    ops: usize,
    fault: Option<Fault>,
    triggered: bool,
    dead: bool,
    caps: Caps,
    /// Every mutation as "op path", for "no write outside the allowed set" assertions.
    log: Vec<String>,
}

#[derive(Clone)]
pub struct FakeFs(Arc<Mutex<Inner>>);

impl FakeFs {
    pub fn new(caps: Caps) -> FakeFs {
        FakeFs(Arc::new(Mutex::new(Inner {
            nodes: BTreeMap::new(),
            clock: 1_000,
            ops: 0,
            fault: None,
            triggered: false,
            dead: false,
            caps,
            log: Vec::new(),
        })))
    }

    fn g(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.0.lock().unwrap()
    }

    pub fn put_file(&self, path: &str, data: &[u8], mode: u32) {
        let mut g = self.g();
        g.clock += 1;
        let mtime = g.clock;
        g.nodes.insert(path.into(), Node { kind: Kind::File, data: data.to_vec(), mode, mtime, link: String::new() });
    }
    pub fn put_symlink(&self, path: &str, target: &str) {
        self.g().nodes.insert(path.into(), Node { kind: Kind::Symlink, data: vec![], mode: 0o777, mtime: 0, link: target.into() });
    }
    /// Another writer changes the file behind our back.
    pub fn external_write(&self, path: &str, data: &[u8]) {
        let mode = self.g().nodes.get(path).map(|n| n.mode).unwrap_or(0o644);
        self.put_file(path, data, mode);
    }
    pub fn external_remove(&self, path: &str) {
        self.g().nodes.remove(path);
    }
    pub fn get(&self, path: &str) -> Option<Vec<u8>> {
        self.g().nodes.get(path).filter(|n| n.kind == Kind::File).map(|n| n.data.clone())
    }
    pub fn mode(&self, path: &str) -> Option<u32> {
        self.g().nodes.get(path).map(|n| n.mode)
    }
    pub fn is_symlink(&self, path: &str) -> bool {
        self.g().nodes.get(path).is_some_and(|n| n.kind == Kind::Symlink)
    }
    pub fn paths(&self) -> Vec<String> {
        self.g().nodes.keys().cloned().collect()
    }
    pub fn mutation_log(&self) -> Vec<String> {
        self.g().log.clone()
    }
    pub fn set_fault(&self, f: Option<Fault>) {
        let mut g = self.g();
        g.fault = f;
        g.triggered = false;
        g.ops = 0;
    }
    pub fn triggered(&self) -> bool {
        self.g().triggered
    }
    pub fn revive(&self) {
        let mut g = self.g();
        g.dead = false;
        g.fault = None;
    }
    pub fn clear_log(&self) {
        self.g().log.clear();
    }

    fn op<T>(&self, mutating: bool, f: impl FnOnce(&mut Inner, bool) -> FsResult<T>) -> FsResult<T> {
        let mut g = self.g();
        if g.dead {
            return Err(FsError::Crash);
        }
        let i = g.ops;
        g.ops += 1;
        let mut after = None;
        if let Some(fl) = g.fault.clone() {
            if fl.at == i {
                g.triggered = true;
                match fl.kind {
                    FaultKind::Fail(e) => {
                        g.fault = None;
                        return Err(e);
                    }
                    FaultKind::CrashAfter if mutating => after = Some(false),
                    FaultKind::CrashMid if mutating => after = Some(true),
                    _ => {
                        g.dead = true;
                        return Err(FsError::Crash);
                    }
                }
            }
        }
        let r = f(&mut g, after == Some(true));
        if after.is_some() {
            g.dead = true;
            return Err(FsError::Crash);
        }
        r
    }
}

fn follow(g: &Inner, path: &str) -> Option<String> {
    let mut p = path.to_string();
    for _ in 0..8 {
        match g.nodes.get(&p) {
            Some(n) if n.kind == Kind::Symlink => p = n.link.clone(),
            Some(_) => return Some(p),
            None => return None,
        }
    }
    None
}

impl RemoteFs for FakeFs {
    fn caps(&self) -> Caps {
        self.g().caps
    }
    fn stat(&mut self, path: &str) -> FsResult<Option<Stat>> {
        self.op(false, |g, _| {
            Ok(g.nodes.get(path).map(|n| Stat { kind: n.kind, size: n.data.len() as u64, mtime: n.mtime, mode: n.mode }))
        })
    }
    fn realpath(&mut self, path: &str) -> FsResult<String> {
        self.op(false, |g, _| follow(g, path).ok_or(FsError::NotFound))
    }
    fn read(&mut self, path: &str) -> FsResult<Vec<u8>> {
        self.op(false, |g, _| g.nodes.get(path).filter(|n| n.kind == Kind::File).map(|n| n.data.clone()).ok_or(FsError::NotFound))
    }
    fn write_new(&mut self, path: &str, data: &[u8], mode: Option<u32>) -> FsResult<()> {
        self.op(true, |g, mid| {
            if g.nodes.contains_key(path) {
                return Err(FsError::Exists);
            }
            g.log.push(format!("write_new {path}"));
            g.clock += 1;
            let d = if mid { data[..data.len() / 2].to_vec() } else { data.to_vec() };
            let mtime = g.clock;
            g.nodes.insert(path.into(), Node { kind: Kind::File, data: d, mode: mode.unwrap_or(0o600), mtime, link: String::new() });
            Ok(())
        })
    }
    fn write_truncate(&mut self, path: &str, data: &[u8]) -> FsResult<()> {
        self.op(true, |g, mid| {
            g.log.push(format!("write_truncate {path}"));
            g.clock += 1;
            let mtime = g.clock;
            let n = g.nodes.get_mut(path).ok_or(FsError::NotFound)?;
            n.data = if mid { data[..data.len() / 2].to_vec() } else { data.to_vec() };
            n.mtime = mtime;
            Ok(())
        })
    }
    fn set_mode(&mut self, path: &str, mode: u32) -> FsResult<()> {
        self.op(true, |g, _| {
            g.log.push(format!("set_mode {path}"));
            g.nodes.get_mut(path).ok_or(FsError::NotFound)?.mode = mode;
            Ok(())
        })
    }
    fn rename(&mut self, from: &str, to: &str) -> FsResult<()> {
        self.op(true, |g, _| {
            if g.nodes.contains_key(to) {
                return Err(FsError::Exists);
            }
            g.log.push(format!("rename {from} {to}"));
            let n = g.nodes.remove(from).ok_or(FsError::NotFound)?;
            g.nodes.insert(to.into(), n);
            Ok(())
        })
    }
    fn rename_replace(&mut self, from: &str, to: &str) -> FsResult<()> {
        self.op(true, |g, _| {
            g.log.push(format!("rename_replace {from} {to}"));
            let n = g.nodes.remove(from).ok_or(FsError::NotFound)?;
            g.nodes.insert(to.into(), n);
            Ok(())
        })
    }
    fn remove(&mut self, path: &str) -> FsResult<()> {
        self.op(true, |g, _| {
            g.log.push(format!("remove {path}"));
            g.nodes.remove(path).map(|_| ()).ok_or(FsError::NotFound)
        })
    }
    fn hash(&mut self, path: &str) -> FsResult<Option<String>> {
        self.op(false, |g, _| {
            if !g.caps.hash {
                return Ok(None);
            }
            let n = g.nodes.get(path).ok_or(FsError::NotFound)?;
            Ok(Some(crate::fs::sha256_hex(&n.data)))
        })
    }
}
