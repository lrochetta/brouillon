//! Provider-neutral file operations on top of an [`SftpClient`]: root confinement, symlink policy,
//! hostile-name filtering, size and time caps. Everything the server says is validated here.

use crate::sftp::packet::*;
use crate::sftp::{Attrs as SAttrs, SftpClient, SftpError};
use brouillon_remote_proto::*;
use std::collections::HashSet;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

pub struct Session {
    pub client: SftpClient,
    pub root: Option<RemotePath>,
    pub limits: Limits,
}

type R<T> = Result<T, RemoteError>;

fn cancelled(c: &AtomicBool) -> Result<(), SftpError> {
    if c.load(Ordering::Relaxed) {
        Err(SftpError::Cancelled)
    } else {
        Ok(())
    }
}

fn entry_kind(a: &SAttrs) -> EntryKind {
    match a.file_type() {
        Some(S_IFDIR) => EntryKind::Dir,
        Some(S_IFREG) => EntryKind::File,
        Some(S_IFLNK) => EntryKind::Symlink,
        _ => EntryKind::Other,
    }
}

pub struct WriteParams {
    pub path: RemotePath,
    pub size: u64,
    pub create: CreateMode,
    pub perm: Option<u32>,
    pub expect: Option<Expect>,
    pub fsync: bool,
    pub allow_symlink: bool,
}

/// Caller-provided throughput watchdog (minimum rate over a window).
struct Watch {
    start: Instant,
    bytes: u64,
    window: Duration,
    min: u64,
}

impl Watch {
    fn new(l: &Limits) -> Watch {
        Watch { start: Instant::now(), bytes: 0, window: Duration::from_secs(l.min_rate_window_s), min: l.min_rate_bytes }
    }
    fn tick(&mut self, n: u64) -> Result<(), SftpError> {
        self.bytes += n;
        if self.start.elapsed() >= self.window {
            if self.bytes < self.min {
                return Err(SftpError::Timeout);
            }
            self.start = Instant::now();
            self.bytes = 0;
        }
        Ok(())
    }
}

impl Session {
    /// Handshake is already done; resolve the home / root directory.
    pub fn open(client: SftpClient, root: Option<RemotePath>, limits: Limits) -> R<(Session, RemotePath)> {
        let s = Session { client, root: root.clone(), limits };
        let start = root.as_ref().map(|r| r.as_wire_str().to_string()).unwrap_or_else(|| ".".into());
        let home = s.realpath_raw(&start)?;
        Ok((s, home))
    }

    fn realpath_raw(&self, p: &str) -> R<RemotePath> {
        let b = self.client.realpath(p)?;
        let s = std::str::from_utf8(&b).map_err(|_| RemoteError::new(ErrorKind::Protocol, "server returned a non-UTF-8 path"))?;
        RemotePath::parse(s).map_err(|e| RemoteError::new(ErrorKind::Protocol, format!("server returned a bad path: {e}")))
    }

    fn check(&self, p: &RemotePath) -> R<()> {
        match &self.root {
            Some(r) if !p.starts_with(r) => Err(RemoteError::new(ErrorKind::OutsideRoot, format!("{p} is outside the session root"))),
            _ => Ok(()),
        }
    }

    fn outside_root(&self, p: &RemotePath) -> bool {
        self.root.as_ref().is_some_and(|r| !p.starts_with(r))
    }

    /// Symlink policy for read/write: a final symlink pointing outside the root is refused unless allowed.
    /// (Check-then-use: a server can still swap the link between this call and the open; documented.)
    fn symlink_gate(&self, p: &RemotePath, allow: bool) -> R<()> {
        if allow || self.root.is_none() {
            return Ok(());
        }
        match self.client.lstat(p.as_wire_str()) {
            Ok(a) if entry_kind(&a) == EntryKind::Symlink => {
                let target = self.realpath_raw(p.as_wire_str())?;
                if self.outside_root(&target) {
                    return Err(RemoteError::new(ErrorKind::SymlinkRefused, format!("{p} points outside the session root")));
                }
                Ok(())
            }
            Ok(_) => Ok(()),
            Err(SftpError::Status { code: FX_NO_SUCH_FILE, .. }) => Ok(()),
            Err(e) => Err(e.into()),
        }
    }

    // ------------------------------------------------------------ list

    pub fn list_dir(&self, path: &RemotePath, cancel: &AtomicBool, mut emit: impl FnMut(Vec<Entry>)) -> R<ListResult> {
        self.check(path)?;
        let h = self.client.opendir(path.as_wire_str())?;
        let r = self.list_inner(&h, cancel, &mut emit);
        let _ = self.client.close(&h);
        Ok(r?)
    }

    fn list_inner(&self, h: &[u8], cancel: &AtomicBool, emit: &mut impl FnMut(Vec<Entry>)) -> Result<ListResult, SftpError> {
        let started = Instant::now();
        let total = Duration::from_secs(self.limits.list_total_timeout_s);
        let mut buf: Vec<Entry> = Vec::new();
        let mut seen: HashSet<Vec<u8>> = HashSet::new();
        let (mut count, mut hidden, mut bytes, mut empty) = (0usize, 0usize, 0u64, 0u32);
        let mut truncated = false;
        'outer: loop {
            cancelled(cancel)?;
            if started.elapsed() > total {
                return Err(SftpError::Timeout);
            }
            let Some(batch) = self.client.readdir(h)? else { break };
            if batch.is_empty() {
                empty += 1;
                if empty > 3 {
                    return Err(SftpError::Protocol("endless empty NAME replies".into()));
                }
                continue;
            }
            empty = 0;
            for e in batch {
                if e.filename == b"." || e.filename == b".." {
                    continue;
                }
                bytes += e.filename.len() as u64 + 64; // longname is ignored but still counted as raw text
                bytes += e.longname.len() as u64;
                if bytes > self.limits.max_list_bytes || count >= self.limits.max_list_entries {
                    truncated = true;
                    break 'outer;
                }
                let name = match RemoteName::from_bytes(&e.filename) {
                    Ok(n) if seen.insert(e.filename.clone()) => n,
                    _ => {
                        hidden += 1;
                        continue;
                    }
                };
                buf.push(Entry {
                    local_unsafe: name.is_local_unsafe(),
                    name,
                    kind: entry_kind(&e.attrs),
                    size: e.attrs.size,
                    mtime: e.attrs.mtime(),
                    mode: e.attrs.perm.map(|p| p & 0o7777),
                });
                count += 1;
                if buf.len() >= self.limits.list_page {
                    emit(std::mem::take(&mut buf));
                }
            }
        }
        if !buf.is_empty() {
            emit(buf);
        }
        Ok(ListResult { count, truncated, hidden_unsafe: hidden })
    }

    // ------------------------------------------------------------ stat

    pub fn stat(&self, path: &RemotePath, follow: bool) -> R<StatResult> {
        self.check(path)?;
        let a = if follow { self.client.stat(path.as_wire_str())? } else { self.client.lstat(path.as_wire_str())? };
        let mut res = StatResult {
            kind: entry_kind(&a),
            size: a.size,
            mtime: a.mtime(),
            mode: a.perm.map(|p| p & 0o7777),
            resolved: None,
            outside_root: false,
        };
        if follow {
            // STAT followed the link; find out where it went.
            if let Ok(l) = self.client.lstat(path.as_wire_str()) {
                if entry_kind(&l) == EntryKind::Symlink {
                    let t = self.realpath_raw(path.as_wire_str())?;
                    res.outside_root = self.outside_root(&t);
                    res.resolved = Some(t);
                }
            }
        } else if res.kind == EntryKind::Symlink {
            if let Ok(t) = self.realpath_raw(path.as_wire_str()) {
                res.outside_root = self.outside_root(&t);
                res.resolved = Some(t);
            }
        }
        Ok(res)
    }

    // ------------------------------------------------------------ read

    pub fn read(
        &self,
        path: &RemotePath,
        offset: u64,
        len: Option<u64>,
        max_bytes: Option<u64>,
        allow_symlink: bool,
        cancel: &AtomicBool,
        mut emit: impl FnMut(u64, &[u8]) -> Result<(), SftpError>,
    ) -> R<ReadResult> {
        self.check(path)?;
        let cap = max_bytes.unwrap_or(self.limits.max_read_bytes).min(self.limits.max_read_bytes);
        self.symlink_gate(path, allow_symlink)?;
        let h = self.client.open(path.as_wire_str(), OPEN_READ, SAttrs::default())?;
        let mut watch = Watch::new(&self.limits);
        let out = self.client.read_range(&h, offset, len, cap, |off, data| {
            cancelled(cancel)?;
            watch.tick(data.len() as u64)?;
            emit(off, data)
        });
        let _ = self.client.close(&h);
        let o = out?;
        Ok(ReadResult { bytes: o.bytes, eof: o.eof })
    }

    // ------------------------------------------------------------ write

    /// `next()` yields the next chunk of the payload (`None` = no more); `progress(done)` reports bytes
    /// acknowledged by the server. Exactly `p.size` bytes must be produced.
    pub fn write(
        &self,
        p: &WriteParams,
        cancel: &AtomicBool,
        mut next: impl FnMut() -> R<Option<Vec<u8>>>,
        mut progress: impl FnMut(u64),
    ) -> R<WriteResult> {
        self.check(&p.path)?;
        if p.size > self.limits.max_write_bytes {
            return Err(RemoteError::new(ErrorKind::TooLarge, format!("{} bytes exceeds the {} byte write cap", p.size, self.limits.max_write_bytes)));
        }
        self.symlink_gate(&p.path, p.allow_symlink)?;
        if let Some(e) = p.expect {
            match self.client.stat(p.path.as_wire_str()) {
                Ok(a) if a.size == Some(e.size) && a.mtime() == Some(e.mtime) => {}
                Ok(_) => return Err(RemoteError::new(ErrorKind::Conflict, "the remote file changed since it was loaded")),
                Err(SftpError::Status { code: FX_NO_SUCH_FILE, .. }) => {
                    return Err(RemoteError::new(ErrorKind::Conflict, "the remote file was deleted since it was loaded"))
                }
                Err(e) => return Err(e.into()),
            }
        }
        let pflags = OPEN_WRITE | OPEN_CREAT | if p.create == CreateMode::New { OPEN_EXCL } else { OPEN_TRUNC };
        let attrs = p.perm.map(|m| SAttrs::with_perm(m & 0o7777)).unwrap_or_default();
        let h = match self.client.open(p.path.as_wire_str(), pflags, attrs) {
            Ok(h) => h,
            Err(SftpError::Status { code: FX_FAILURE, .. }) if p.create == CreateMode::New => {
                return Err(RemoteError::new(ErrorKind::AlreadyExists, format!("{} already exists", p.path)));
            }
            Err(e) => return Err(e.into()),
        };
        let r = self.write_inner(&h, p, cancel, &mut next, &mut progress);
        let closed = self.client.close(&h);
        match r {
            Ok(mut res) => {
                closed?;
                res.size = p.size;
                Ok(res)
            }
            Err(e) => Err(e),
        }
    }

    fn write_inner(
        &self,
        h: &[u8],
        p: &WriteParams,
        cancel: &AtomicBool,
        next: &mut impl FnMut() -> R<Option<Vec<u8>>>,
        progress: &mut impl FnMut(u64),
    ) -> R<WriteResult> {
        let mut ws = self.client.write_stream(h);
        let mut got = 0u64;
        while got < p.size {
            cancelled(cancel)?;
            let chunk = next()?.ok_or_else(|| RemoteError::new(ErrorKind::Protocol, "data ended before the announced size"))?;
            got += chunk.len() as u64;
            if got > p.size {
                return Err(RemoteError::new(ErrorKind::Protocol, "more data than the announced size"));
            }
            ws.push(&chunk)?;
            progress(ws.acked());
        }
        // (data beyond `size` is never read: the helper drops late DATA frames of a finished request)
        let done = ws.finish()?;
        progress(done);
        let mut fsynced = false;
        if p.fsync && self.client.has_extension(EXT_FSYNC) {
            self.client.fsync(h)?;
            fsynced = true;
        }
        let mtime = self.client.fstat(h).ok().and_then(|a| a.mtime());
        Ok(WriteResult { size: p.size, mtime, fsynced, atomic: false, swapped: false })
    }

    // ------------------------------------------------------------ namespace

    pub fn rename(&self, from: &RemotePath, to: &RemotePath, overwrite: bool, allow_nonatomic: bool) -> R<RenameResult> {
        self.check(from)?;
        self.check(to)?;
        let (a, b) = (from.as_wire_str(), to.as_wire_str());
        if !overwrite {
            return match self.client.rename(a, b) {
                Ok(()) => Ok(RenameResult { atomic: true }),
                Err(SftpError::Status { code: FX_FAILURE, .. }) if self.client.lstat(b).is_ok() => {
                    Err(RemoteError::new(ErrorKind::AlreadyExists, format!("{to} already exists")))
                }
                Err(e) => Err(e.into()),
            };
        }
        if self.client.has_extension(EXT_POSIX_RENAME) {
            self.client.posix_rename(a, b)?;
            return Ok(RenameResult { atomic: true });
        }
        if !allow_nonatomic {
            return Err(RemoteError::new(ErrorKind::Unsupported, "the server has no atomic replace (posix-rename@openssh.com)"));
        }
        match self.client.remove(b) {
            Ok(()) | Err(SftpError::Status { code: FX_NO_SUCH_FILE, .. }) => {}
            Err(e) => return Err(e.into()),
        }
        self.client.rename(a, b)?;
        Ok(RenameResult { atomic: false })
    }

    pub fn remove(&self, p: &RemotePath) -> R<()> {
        self.check(p)?;
        Ok(self.client.remove(p.as_wire_str())?)
    }
    pub fn rmdir(&self, p: &RemotePath) -> R<()> {
        self.check(p)?;
        Ok(self.client.rmdir(p.as_wire_str())?)
    }
    pub fn mkdir(&self, p: &RemotePath, mode: Option<u32>) -> R<()> {
        self.check(p)?;
        match self.client.mkdir(p.as_wire_str(), mode) {
            Ok(()) => Ok(()),
            Err(SftpError::Status { code: FX_FAILURE, .. }) if self.client.lstat(p.as_wire_str()).is_ok() => {
                Err(RemoteError::new(ErrorKind::AlreadyExists, format!("{p} already exists")))
            }
            Err(e) => Err(e.into()),
        }
    }
    pub fn chmod(&self, p: &RemotePath, mode: u32) -> R<()> {
        self.check(p)?;
        Ok(self.client.set_mode(p.as_wire_str(), mode)?)
    }
    pub fn realpath(&self, p: &RemotePath) -> R<RemotePath> {
        self.check(p)?;
        let r = self.realpath_raw(p.as_wire_str())?;
        // a server-supplied realpath is a hint: it must still be inside the root
        self.check(&r)?;
        Ok(r)
    }
    pub fn readlink(&self, p: &RemotePath) -> R<String> {
        self.check(p)?;
        let b = self.client.readlink(p.as_wire_str())?;
        Ok(sanitize_text(&String::from_utf8_lossy(&b)))
    }
}
