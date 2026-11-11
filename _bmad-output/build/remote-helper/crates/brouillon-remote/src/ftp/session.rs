//! The FTP operations layer: the same method set and semantics as [`crate::ops::Session`], on a
//! single control connection (serialised by a mutex) plus one passive data connection per transfer.

use super::ctrl::{map_io, Ctrl, Reply, Stream, MAX_LINE};
use super::parse::{self, Feat, ListLine, Parsed};
use super::tls::{TlsCtx, TlsStream};
use super::{Closer, Dialer};
use crate::ops::WriteParams;
use crate::sftp::SftpError;
use crate::transport::Killer;
use brouillon_remote_proto::*;
use std::collections::HashSet;
use std::io::{Read, Write};
use std::net::IpAddr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

type R<T> = Result<T, RemoteError>;

pub(crate) struct Shared {
    pub root: Option<RemotePath>,
    pub limits: Limits,
    pub feat: Feat,
    pub tls: Option<Arc<TlsCtx>>,
    pub dialer: Arc<dyn Dialer>,
    pub idle: Duration,
    pub connect_timeout: Duration,
    pub closed: AtomicBool,
    pub ctrl_closer: Closer,
    pub data_closer: Mutex<Option<Closer>>,
}

pub(crate) struct Inner {
    pub ctrl: Ctrl,
    pub peer_ip: IpAddr,
    pub no_epsv: bool,
    pub broken: bool,
}

pub struct FtpSession {
    inner: Mutex<Inner>,
    shared: Arc<Shared>,
}

impl FtpSession {
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new(
        ctrl: Ctrl,
        peer_ip: IpAddr,
        ctrl_closer: Closer,
        root: Option<RemotePath>,
        limits: Limits,
        feat: Feat,
        tls: Option<Arc<TlsCtx>>,
        dialer: Arc<dyn Dialer>,
        connect_timeout: Duration,
    ) -> FtpSession {
        let idle = Duration::from_secs(limits.op_idle_timeout_s.max(1));
        FtpSession {
            inner: Mutex::new(Inner { ctrl, peer_ip, no_epsv: false, broken: false }),
            shared: Arc::new(Shared {
                root,
                limits,
                feat,
                tls,
                dialer,
                idle,
                connect_timeout,
                closed: AtomicBool::new(false),
                ctrl_closer,
                data_closer: Mutex::new(None),
            }),
        }
    }

    pub fn killer(&self) -> Box<dyn Killer> {
        Box::new(FtpKiller(self.shared.clone()))
    }

    /// (resumed, full) TLS handshakes performed on data connections (diagnostics / tests).
    pub fn tls_data_stats(&self) -> (u32, u32) {
        match &self.shared.tls {
            Some(t) => (t.resumed.load(Ordering::Relaxed), t.full.load(Ordering::Relaxed)),
            None => (0, 0),
        }
    }

    fn conn(&self) -> R<Conn<'_>> {
        if self.shared.closed.load(Ordering::Relaxed) {
            return Err(RemoteError::new(ErrorKind::ConnectionLost, "the FTP session was closed"));
        }
        let g = self.inner.lock().unwrap_or_else(|p| p.into_inner());
        if g.broken {
            return Err(RemoteError::new(ErrorKind::ConnectionLost, "the FTP session is out of sync and was dropped"));
        }
        Ok(Conn { s: &self.shared, i: g })
    }

    fn check(&self, p: &RemotePath) -> R<()> {
        match &self.shared.root {
            Some(r) if !p.starts_with(r) => Err(RemoteError::new(ErrorKind::OutsideRoot, format!("{p} is outside the session root"))),
            _ => Ok(()),
        }
    }
}

struct FtpKiller(Arc<Shared>);
impl Killer for FtpKiller {
    fn kill(&mut self) {
        self.0.closed.store(true, Ordering::Relaxed);
        (self.0.ctrl_closer)();
        if let Some(c) = self.0.data_closer.lock().unwrap_or_else(|p| p.into_inner()).take() {
            c();
        }
    }
}

// ---------------------------------------------------------------- error mapping

pub fn map_reply(r: &Reply, lookup: bool) -> RemoteError {
    let t = r.text();
    let l = t.to_ascii_lowercase();
    let kind = match r.code {
        421 => ErrorKind::ConnectionLost,
        530 | 532 => ErrorKind::AuthFailed,
        500 | 502 | 504 | 202 => ErrorKind::Unsupported,
        501 => ErrorKind::InvalidArgument,
        452 | 552 => ErrorKind::TooLarge,
        550 | 551 | 553 | 450 => {
            if l.contains("not a directory") {
                ErrorKind::NotADirectory
            } else if l.contains("no such") || l.contains("not found") || l.contains("not exist") || l.contains("cannot find") || l.contains("does not exist") {
                ErrorKind::NotFound
            } else if l.contains("exist") {
                ErrorKind::AlreadyExists
            } else if l.contains("permission") || l.contains("denied") || l.contains("not allowed") || l.contains("access") || l.contains("forbidden") {
                ErrorKind::PermissionDenied
            } else if lookup && r.code == 550 {
                ErrorKind::NotFound
            } else {
                ErrorKind::Failure
            }
        }
        _ => ErrorKind::Failure,
    };
    RemoteError::new(kind, format!("server replied {}: {}", r.code, t))
}

fn cancelled(c: &AtomicBool) -> R<()> {
    if c.load(Ordering::Relaxed) {
        Err(RemoteError::new(ErrorKind::Cancelled, "cancelled"))
    } else {
        Ok(())
    }
}

fn sftp_to_remote(e: SftpError) -> RemoteError {
    e.into()
}

// ---------------------------------------------------------------- data connection

enum Data {
    Plain(Box<dyn Stream>),
    Tls(Box<TlsStream>),
}

impl Data {
    fn read(&mut self, b: &mut [u8]) -> std::io::Result<usize> {
        match self {
            Data::Plain(s) => s.read(b),
            Data::Tls(s) => s.read(b),
        }
    }
    fn write_all(&mut self, b: &[u8]) -> std::io::Result<()> {
        match self {
            Data::Plain(s) => s.write_all(b),
            Data::Tls(s) => s.write_all(b),
        }
    }
    /// Orderly end of an upload: TLS `close_notify`, flush, then the caller drops the stream.
    fn finish_write(&mut self) -> std::io::Result<()> {
        match self {
            Data::Plain(s) => s.flush(),
            Data::Tls(s) => {
                s.flush()?;
                s.conn.send_close_notify();
                s.flush()
            }
        }
    }
}

/// `true` for the "peer closed without close_notify" error of rustls.
fn truncated_tls(e: &std::io::Error) -> bool {
    e.kind() == std::io::ErrorKind::UnexpectedEof
}

struct Rate {
    start: Instant,
    bytes: u64,
    window: Duration,
    min: u64,
}
impl Rate {
    fn new(l: &Limits) -> Rate {
        Rate { start: Instant::now(), bytes: 0, window: Duration::from_secs(l.min_rate_window_s.max(1)), min: l.min_rate_bytes }
    }
    fn tick(&mut self, n: u64) -> R<()> {
        self.bytes += n;
        if self.start.elapsed() >= self.window {
            if self.bytes < self.min {
                return Err(RemoteError::new(ErrorKind::Timeout, "the transfer is too slow"));
            }
            self.start = Instant::now();
            self.bytes = 0;
        }
        Ok(())
    }
}

// ---------------------------------------------------------------- the locked connection

struct Conn<'a> {
    s: &'a Shared,
    i: MutexGuard<'a, Inner>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ListKind {
    Mlsd,
    List,
}

fn rand_hex() -> String {
    let mut b = [0u8; 6];
    if getrandom::getrandom(&mut b).is_err() {
        let n = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.subsec_nanos()).unwrap_or(7);
        b[..4].copy_from_slice(&n.to_le_bytes());
    }
    b.iter().map(|x| format!("{x:02x}")).collect()
}

fn sibling(path: &RemotePath, tag: &str) -> R<RemotePath> {
    let parent = path.parent().ok_or_else(|| RemoteError::new(ErrorKind::InvalidArgument, "the root has no sibling"))?;
    let name = path.file_name().ok_or_else(|| RemoteError::new(ErrorKind::InvalidArgument, "path has no file name"))?;
    let mut n: String = name.as_wire_str().chars().take(120).collect();
    n = format!(".{n}.brouillon-{tag}-{}.tmp", rand_hex());
    let rn = RemoteName::new(&n).map_err(|e| RemoteError::new(ErrorKind::InvalidArgument, e.to_string()))?;
    parent.join(&rn).map_err(|e| RemoteError::new(ErrorKind::InvalidArgument, e.to_string()))
}

impl Conn<'_> {
    fn poison(&mut self, e: &RemoteError) {
        if matches!(e.kind, ErrorKind::ConnectionLost | ErrorKind::Timeout | ErrorKind::Protocol) {
            self.i.broken = true;
        }
    }

    fn cmd(&mut self, c: &str) -> R<Reply> {
        match self.i.ctrl.cmd(c) {
            Ok(r) => {
                if r.code == 421 {
                    self.i.broken = true;
                }
                Ok(r)
            }
            Err(e) => {
                self.poison(&e);
                Err(e)
            }
        }
    }

    fn reply(&mut self) -> R<Reply> {
        match self.i.ctrl.reply() {
            Ok(r) => Ok(r),
            Err(e) => {
                self.poison(&e);
                Err(e)
            }
        }
    }

    fn expect(&mut self, c: &str, lookup: bool) -> R<Reply> {
        let r = self.cmd(c)?;
        if r.ok() {
            Ok(r)
        } else {
            Err(map_reply(&r, lookup))
        }
    }

    // ---- data connection

    fn pasv(&mut self) -> R<u16> {
        if !self.i.no_epsv {
            let r = self.cmd("EPSV")?;
            if r.code == 229 {
                return parse::parse_epsv_port(&r.lines.join(" ")).ok_or_else(|| RemoteError::new(ErrorKind::Protocol, "unusable EPSV reply"));
            }
            if r.class() == 1 || r.class() == 2 || r.class() == 3 {
                return Err(RemoteError::new(ErrorKind::Protocol, "unexpected reply to EPSV"));
            }
            self.i.no_epsv = true; // 4xx/5xx: fall back to PASV for the rest of the session
        }
        let r = self.cmd("PASV")?;
        if r.code != 227 {
            return Err(map_reply(&r, false));
        }
        parse::parse_pasv_port(&r.lines.join(" ")).ok_or_else(|| RemoteError::new(ErrorKind::Protocol, "unusable PASV reply"))
    }

    /// Passive data connection to the CONTROL peer's address (the host in 227/229 is never used),
    /// then the transfer command, then TLS on the data channel when the control channel has it.
    fn open_data(&mut self, command: &str) -> R<Data> {
        let port = self.pasv()?;
        let dialed = self
            .s
            .dialer
            .data(self.i.peer_ip, port, self.s.connect_timeout, self.s.idle)
            .map_err(|e| RemoteError::new(ErrorKind::ConnectFailed, format!("could not open the data connection: {e}")))?;
        *self.s.data_closer.lock().unwrap_or_else(|p| p.into_inner()) = Some(dialed.closer.clone());
        let r = self.cmd(command)?;
        if r.class() != 1 {
            return Err(map_reply(&r, true));
        }
        match &self.s.tls {
            None => Ok(Data::Plain(dialed.stream)),
            Some(t) => {
                let s = t.handshake(dialed.stream)?;
                if s.conn.handshake_kind() == Some(rustls::HandshakeKind::Resumed) {
                    t.resumed.fetch_add(1, Ordering::Relaxed);
                } else {
                    t.full.fetch_add(1, Ordering::Relaxed);
                }
                Ok(Data::Tls(Box::new(s)))
            }
        }
    }

    fn end_data(&mut self) {
        self.s.data_closer.lock().unwrap_or_else(|p| p.into_inner()).take();
    }

    /// After the data connection ended normally: the final reply must be a 2xx.
    fn finish_transfer(&mut self) -> R<()> {
        self.end_data();
        let r = self.reply()?;
        if r.ok() {
            Ok(())
        } else {
            Err(map_reply(&r, true))
        }
    }

    /// After dropping the data connection early: consume the server's final reply (426/451/226) so the
    /// control channel stays in sync. Whatever it says is irrelevant; failing to get one poisons the session.
    fn abort_transfer(&mut self) {
        self.end_data();
        let _ = self.reply();
    }

    // ---- listing

    fn list_inner(&mut self, path: &RemotePath, cancel: &AtomicBool, emit: &mut dyn FnMut(Vec<Entry>)) -> R<ListResult> {
        let mut kind = if self.s.feat.mlst { ListKind::Mlsd } else { ListKind::List };
        loop {
            match self.list_try(path, kind, cancel, emit) {
                Err(e) if kind == ListKind::Mlsd && e.kind == ErrorKind::Unsupported => kind = ListKind::List,
                r => return r,
            }
        }
    }

    fn list_try(&mut self, path: &RemotePath, kind: ListKind, cancel: &AtomicBool, emit: &mut dyn FnMut(Vec<Entry>)) -> R<ListResult> {
        let mut d = match kind {
            ListKind::Mlsd => self.open_data(&format!("MLSD {}", path.as_wire_str()))?,
            ListKind::List => {
                // CWD + bare LIST: an argument to LIST is subject to globbing and option parsing on many servers
                self.expect(&format!("CWD {}", path.as_wire_str()), true)?;
                self.open_data("LIST")?
            }
        };
        let started = Instant::now();
        let total = Duration::from_secs(self.s.limits.list_total_timeout_s.max(1));
        let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0);
        let (mut count, mut hidden, mut bytes) = (0usize, 0usize, 0u64);
        let mut truncated = false;
        let mut seen: HashSet<String> = HashSet::new();
        let mut page: Vec<Entry> = Vec::new();
        let mut pending: Vec<u8> = Vec::new();
        let mut discarding = false;
        let mut chunk = vec![0u8; 16 * 1024];
        let mut failure: Option<RemoteError> = None;
        let mut eof = false;
        'read: loop {
            if let Err(e) = cancelled(cancel) {
                failure = Some(e);
                break;
            }
            if started.elapsed() > total {
                failure = Some(RemoteError::new(ErrorKind::Timeout, "the directory listing took too long"));
                break;
            }
            let n = match d.read(&mut chunk) {
                Ok(0) => {
                    eof = true;
                    0
                }
                Ok(n) => n,
                // a listing is only metadata, the 226 on the (TLS protected) control channel confirms completeness
                Err(e) if truncated_tls(&e) => {
                    eof = true;
                    0
                }
                Err(e) => {
                    failure = Some(map_io(&e));
                    break;
                }
            };
            bytes += n as u64;
            if bytes > self.s.limits.max_list_bytes {
                truncated = true;
                break;
            }
            pending.extend_from_slice(&chunk[..n]);
            if eof && !pending.is_empty() && pending.last() != Some(&b'\n') {
                pending.push(b'\n');
            }
            while let Some(i) = pending.iter().position(|b| *b == b'\n') {
                let mut line: Vec<u8> = pending.drain(..=i).collect();
                line.pop();
                if line.last() == Some(&b'\r') {
                    line.pop();
                }
                if std::mem::take(&mut discarding) {
                    continue; // tail of an over-long line already counted
                }
                if line.len() > MAX_LINE {
                    hidden += 1;
                    continue;
                }
                let Ok(text) = std::str::from_utf8(&line) else {
                    hidden += 1;
                    continue;
                };
                let parsed: Option<Parsed> = match kind {
                    ListKind::Mlsd => match parse::parse_mlsx(text) {
                        Ok(p) => p,
                        Err(()) => {
                            hidden += 1;
                            continue;
                        }
                    },
                    ListKind::List => match parse::parse_list_line(text, now) {
                        ListLine::Entry(p) => Some(p),
                        ListLine::Skip => None,
                        ListLine::Bad => {
                            hidden += 1;
                            continue;
                        }
                    },
                };
                let Some(p) = parsed else { continue };
                if count >= self.s.limits.max_list_entries {
                    truncated = true;
                    break 'read;
                }
                let name = match RemoteName::new(&p.name) {
                    Ok(n) if seen.insert(p.name.clone()) => n,
                    _ => {
                        hidden += 1;
                        continue;
                    }
                };
                page.push(Entry { local_unsafe: name.is_local_unsafe(), name, kind: p.kind, size: p.size, mtime: p.mtime, mode: p.mode });
                count += 1;
                if page.len() >= self.s.limits.list_page {
                    emit(std::mem::take(&mut page));
                }
            }
            if pending.len() > MAX_LINE {
                // an over-long line without a newline: count it once and drop everything up to its end
                hidden += 1;
                pending.clear();
                discarding = true;
            }
            if eof {
                break;
            }
        }
        if let Some(e) = failure {
            drop(d);
            self.abort_transfer();
            return Err(e);
        }
        if !page.is_empty() {
            emit(page);
        }
        if truncated {
            drop(d);
            self.abort_transfer();
        } else {
            drop(d);
            self.finish_transfer()?;
        }
        Ok(ListResult { count, truncated, hidden_unsafe: hidden })
    }

    // ---- stat

    fn size_of(&mut self, p: &RemotePath) -> R<Option<u64>> {
        let r = self.cmd(&format!("SIZE {}", p.as_wire_str()))?;
        if r.code == 213 {
            let n = r.first().trim().parse::<u64>().map_err(|_| RemoteError::new(ErrorKind::Protocol, "unparseable SIZE reply"))?;
            Ok(Some(n))
        } else if r.class() == 4 {
            Err(map_reply(&r, true))
        } else {
            Ok(None)
        }
    }

    fn mdtm_of(&mut self, p: &RemotePath) -> Option<i64> {
        let r = self.cmd(&format!("MDTM {}", p.as_wire_str())).ok()?;
        if r.code == 213 {
            parse::parse_mdtm(r.first())
        } else {
            None
        }
    }

    fn stat_inner(&mut self, path: &RemotePath) -> R<StatResult> {
        let mk = |kind, size, mtime, mode| StatResult { kind, size, mtime, mode, resolved: None, outside_root: false };
        if path.as_wire_str() == "/" {
            return Ok(mk(EntryKind::Dir, None, None, None));
        }
        if self.s.feat.mlst {
            let r = self.cmd(&format!("MLST {}", path.as_wire_str()))?;
            if r.code == 250 {
                let facts: Vec<&String> = r.lines.iter().skip(1).take(r.lines.len().saturating_sub(2)).filter(|l| l.starts_with(' ')).collect();
                if facts.len() != 1 {
                    return Err(RemoteError::new(ErrorKind::Protocol, "unexpected MLST reply shape"));
                }
                return match parse::parse_mlsx(&facts[0][1..]) {
                    Ok(Some(p)) => Ok(mk(p.kind, p.size, p.mtime, p.mode)),
                    Ok(None) => Ok(mk(EntryKind::Dir, None, None, None)),
                    Err(()) => Err(RemoteError::new(ErrorKind::Protocol, "unparseable MLST reply")),
                };
            }
            if !matches!(r.code, 500 | 501 | 502 | 504) {
                return Err(map_reply(&r, true));
            }
        }
        // fallback without MLST: SIZE says "regular file", CWD says "directory"
        match self.size_of(path)? {
            Some(sz) => {
                let mtime = self.mdtm_of(path);
                Ok(mk(EntryKind::File, Some(sz), mtime, None))
            }
            None => {
                self.expect(&format!("CWD {}", path.as_wire_str()), true)?;
                Ok(mk(EntryKind::Dir, None, None, None))
            }
        }
    }

    fn exists(&mut self, p: &RemotePath) -> R<Option<StatResult>> {
        match self.stat_inner(p) {
            Ok(s) => Ok(Some(s)),
            Err(e) if e.kind == ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e),
        }
    }

    /// FTP cannot tell where a symlink points. With a root configured and no explicit allowance,
    /// a final symlink is refused (its kind comes from the parent listing).
    fn symlink_gate(&mut self, p: &RemotePath, allow: bool) -> R<()> {
        if allow || self.s.root.is_none() {
            return Ok(());
        }
        let (Some(parent), Some(name)) = (p.parent(), p.file_name()) else { return Ok(()) };
        let mut hit = false;
        let never = AtomicBool::new(false);
        self.list_inner(&parent, &never, &mut |es| {
            hit |= es.iter().any(|e| e.name == name && e.kind == EntryKind::Symlink);
        })?;
        if hit {
            return Err(RemoteError::new(ErrorKind::SymlinkRefused, format!("{p} is a symlink and FTP cannot verify where it points")));
        }
        Ok(())
    }

    // ---- rename helpers

    fn rename_raw(&mut self, from: &RemotePath, to: &RemotePath) -> R<()> {
        let r = self.cmd(&format!("RNFR {}", from.as_wire_str()))?;
        if r.code != 350 {
            return Err(map_reply(&r, true));
        }
        let r = self.cmd(&format!("RNTO {}", to.as_wire_str()))?;
        if r.ok() {
            Ok(())
        } else {
            Err(map_reply(&r, false))
        }
    }

    fn fatal(e: &RemoteError) -> bool {
        matches!(e.kind, ErrorKind::ConnectionLost | ErrorKind::Timeout | ErrorKind::Protocol | ErrorKind::Cancelled)
    }

    /// target -> old, tmp -> target, delete old. A window with no file at `target` exists between
    /// steps 1 and 2; if step 2 fails the old file is moved back.
    fn swap(&mut self, tmp: &RemotePath, target: &RemotePath) -> R<()> {
        let old = sibling(target, "old")?;
        self.rename_raw(target, &old)?;
        match self.rename_raw(tmp, target) {
            Ok(()) => {
                let _ = self.cmd(&format!("DELE {}", old.as_wire_str()));
                Ok(())
            }
            Err(e) => {
                let _ = self.rename_raw(&old, target);
                Err(e)
            }
        }
    }

    /// Returns whether the 3-step swap had to be used.
    fn replace(&mut self, tmp: &RemotePath, target: &RemotePath, target_exists: bool) -> R<bool> {
        match self.rename_raw(tmp, target) {
            Ok(()) => Ok(false),
            Err(e) if target_exists && !Self::fatal(&e) => {
                self.swap(tmp, target)?;
                Ok(true)
            }
            Err(e) => Err(e),
        }
    }

    fn delete_quiet(&mut self, p: &RemotePath) {
        if !self.i.broken {
            let _ = self.cmd(&format!("DELE {}", p.as_wire_str()));
        }
    }

    fn site_chmod(&mut self, p: &RemotePath, mode: u32) -> R<()> {
        let r = self.cmd(&format!("SITE CHMOD {:o} {}", mode & 0o7777, p.as_wire_str()))?;
        if r.ok() {
            Ok(())
        } else {
            Err(map_reply(&r, false))
        }
    }

    // ---- read / write

    #[allow(clippy::too_many_arguments)]
    fn read_inner(
        &mut self,
        path: &RemotePath,
        offset: u64,
        len: Option<u64>,
        cap: u64,
        cancel: &AtomicBool,
        emit: &mut dyn FnMut(u64, &[u8]) -> Result<(), SftpError>,
    ) -> R<ReadResult> {
        if let Some(l) = len {
            if l > cap {
                return Err(RemoteError::new(ErrorKind::TooLarge, format!("requested {l} bytes, cap is {cap}")));
            }
        }
        // with TLS a data connection that ends without close_notify is only accepted when the byte count matches SIZE
        let size_hint = if self.s.tls.is_some() { self.size_of(path).ok().flatten() } else { None };
        if offset > 0 {
            let r = self.cmd(&format!("REST {offset}"))?;
            if r.code != 350 {
                return Err(RemoteError::new(ErrorKind::Unsupported, "the server cannot resume a download at an offset"));
            }
        }
        let mut d = self.open_data(&format!("RETR {}", path.as_wire_str()))?;
        let limit = len.unwrap_or(cap);
        let mut rate = Rate::new(&self.s.limits);
        let mut buf = vec![0u8; 64 * 1024];
        let (mut got, mut eof, mut missing_notify) = (0u64, false, false);
        let mut failure: Option<RemoteError> = None;
        loop {
            if let Err(e) = cancelled(cancel) {
                failure = Some(e);
                break;
            }
            let n = match d.read(&mut buf) {
                Ok(0) => {
                    eof = true;
                    break;
                }
                Ok(n) => n,
                Err(e) if truncated_tls(&e) => {
                    eof = true;
                    missing_notify = true;
                    break;
                }
                Err(e) => {
                    failure = Some(map_io(&e));
                    break;
                }
            };
            if let Err(e) = rate.tick(n as u64) {
                failure = Some(e);
                break;
            }
            let room = limit.saturating_sub(got);
            let take = (n as u64).min(room) as usize;
            if len.is_none() && (n as u64) > room {
                failure = Some(RemoteError::new(ErrorKind::TooLarge, format!("file is larger than the {cap} byte cap")));
                break;
            }
            if let Err(e) = emit(offset + got, &buf[..take]) {
                failure = Some(sftp_to_remote(e));
                break;
            }
            got += take as u64;
            if len.is_some() && got >= limit {
                break; // the caller asked for a prefix: stop here, the rest is abandoned
            }
        }
        if let Some(e) = failure {
            drop(d);
            self.abort_transfer();
            return Err(e);
        }
        drop(d);
        if eof {
            if missing_notify && size_hint != Some(offset + got) {
                self.abort_transfer();
                return Err(RemoteError::new(ErrorKind::ConnectionLost, "the data connection ended without a TLS close_notify and the size does not match: the download may be truncated"));
            }
            self.finish_transfer()?;
        } else {
            self.abort_transfer();
        }
        Ok(ReadResult { bytes: got, eof })
    }

    fn upload(
        &mut self,
        to: &RemotePath,
        size: u64,
        cancel: &AtomicBool,
        next: &mut dyn FnMut() -> R<Option<Vec<u8>>>,
        progress: &mut dyn FnMut(u64),
    ) -> R<()> {
        let mut d = self.open_data(&format!("STOR {}", to.as_wire_str()))?;
        let mut got = 0u64;
        let mut rate = Rate::new(&self.s.limits);
        let res: R<()> = (|| {
            while got < size {
                cancelled(cancel)?;
                let chunk = next()?.ok_or_else(|| RemoteError::new(ErrorKind::Protocol, "data ended before the announced size"))?;
                got += chunk.len() as u64;
                if got > size {
                    return Err(RemoteError::new(ErrorKind::Protocol, "more data than the announced size"));
                }
                d.write_all(&chunk).map_err(|e| map_io(&e))?;
                rate.tick(chunk.len() as u64)?;
                progress(got);
            }
            d.finish_write().map_err(|e| map_io(&e))
        })();
        drop(d);
        match res {
            Ok(()) => self.finish_transfer(),
            Err(e) => {
                self.abort_transfer();
                Err(e)
            }
        }
    }

    fn write_inner(
        &mut self,
        p: &WriteParams,
        cancel: &AtomicBool,
        next: &mut dyn FnMut() -> R<Option<Vec<u8>>>,
        progress: &mut dyn FnMut(u64),
    ) -> R<WriteResult> {
        let existing = self.exists(&p.path)?;
        let check_expect = |st: &Option<StatResult>| -> R<()> {
            if let Some(e) = p.expect {
                match st {
                    Some(s) if s.size == Some(e.size) && s.mtime.is_none_or(|m| m == e.mtime) => Ok(()),
                    Some(_) => Err(RemoteError::new(ErrorKind::Conflict, "the remote file changed since it was loaded")),
                    None => Err(RemoteError::new(ErrorKind::Conflict, "the remote file was deleted since it was loaded")),
                }
            } else {
                Ok(())
            }
        };
        check_expect(&existing)?;
        if p.create == CreateMode::New && existing.is_some() {
            return Err(RemoteError::new(ErrorKind::AlreadyExists, format!("{} already exists", p.path)));
        }
        if let Some(s) = &existing {
            if s.kind == EntryKind::Dir {
                return Err(RemoteError::new(ErrorKind::Failure, format!("{} is a directory", p.path)));
            }
        }
        let direct = p.create == CreateMode::New;
        let dest = if direct { p.path.clone() } else { sibling(&p.path, "w")? };

        if let Err(e) = self.upload(&dest, p.size, cancel, next, progress) {
            self.delete_quiet(&dest);
            return Err(e);
        }
        let verify = (|| -> R<()> {
            if let Some(sz) = self.size_of(&dest)? {
                if sz != p.size {
                    return Err(RemoteError::new(ErrorKind::Failure, format!("the server stored {sz} bytes instead of {}", p.size)));
                }
            }
            Ok(())
        })();
        if let Err(e) = verify {
            self.delete_quiet(&dest);
            return Err(e);
        }
        // mode: explicit, else keep the old file's when the server told us (best effort)
        if let Some(m) = p.perm.or(existing.as_ref().and_then(|s| s.mode)) {
            let _ = self.site_chmod(&dest, m);
        }
        let mut swapped = false;
        if !direct {
            // narrow the race: look at the target again right before replacing it
            if p.expect.is_some() {
                let again = match self.exists(&p.path) {
                    Ok(s) => s,
                    Err(e) => {
                        self.delete_quiet(&dest);
                        return Err(e);
                    }
                };
                if let Err(e) = check_expect(&again) {
                    self.delete_quiet(&dest);
                    return Err(e);
                }
            }
            match self.replace(&dest, &p.path, existing.is_some()) {
                Ok(s) => swapped = s,
                Err(e) => {
                    self.delete_quiet(&dest);
                    return Err(e);
                }
            }
        }
        let mtime = self.mdtm_of(&p.path);
        Ok(WriteResult { size: p.size, mtime, fsynced: false, atomic: false, swapped })
    }
}

// ---------------------------------------------------------------- public operations

impl FtpSession {
    pub fn list_dir(&self, path: &RemotePath, cancel: &AtomicBool, mut emit: impl FnMut(Vec<Entry>)) -> R<ListResult> {
        self.check(path)?;
        self.conn()?.list_inner(path, cancel, &mut emit)
    }

    pub fn stat(&self, path: &RemotePath, _follow: bool) -> R<StatResult> {
        self.check(path)?;
        self.conn()?.stat_inner(path)
    }

    #[allow(clippy::too_many_arguments)]
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
        let cap = max_bytes.unwrap_or(self.shared.limits.max_read_bytes).min(self.shared.limits.max_read_bytes);
        let mut c = self.conn()?;
        c.symlink_gate(path, allow_symlink)?;
        c.read_inner(path, offset, len, cap, cancel, &mut emit)
    }

    pub fn write(
        &self,
        p: &WriteParams,
        cancel: &AtomicBool,
        mut next: impl FnMut() -> R<Option<Vec<u8>>>,
        mut progress: impl FnMut(u64),
    ) -> R<WriteResult> {
        self.check(&p.path)?;
        if p.size > self.shared.limits.max_write_bytes {
            return Err(RemoteError::new(ErrorKind::TooLarge, format!("{} bytes exceeds the {} byte write cap", p.size, self.shared.limits.max_write_bytes)));
        }
        let mut c = self.conn()?;
        c.symlink_gate(&p.path, p.allow_symlink)?;
        c.write_inner(p, cancel, &mut next, &mut progress)
    }

    pub fn rename(&self, from: &RemotePath, to: &RemotePath, overwrite: bool, allow_nonatomic: bool) -> R<RenameResult> {
        self.check(from)?;
        self.check(to)?;
        let mut c = self.conn()?;
        let exists = c.exists(to)?.is_some();
        if exists && !overwrite {
            return Err(RemoteError::new(ErrorKind::AlreadyExists, format!("{to} already exists")));
        }
        match c.rename_raw(from, to) {
            Ok(()) => {}
            Err(e) if exists && overwrite && !Conn::fatal(&e) => {
                if !allow_nonatomic {
                    return Err(RemoteError::new(ErrorKind::Unsupported, "the server refuses to rename over an existing file and a non-atomic replace was not allowed"));
                }
                c.swap(from, to)?;
            }
            Err(e) => return Err(e),
        }
        // FTP has no posix-rename: even a direct RNTO over an existing file is not known to be atomic
        Ok(RenameResult { atomic: false })
    }

    pub fn remove(&self, p: &RemotePath) -> R<()> {
        self.check(p)?;
        self.conn()?.expect(&format!("DELE {}", p.as_wire_str()), true).map(|_| ())
    }

    pub fn rmdir(&self, p: &RemotePath) -> R<()> {
        self.check(p)?;
        self.conn()?.expect(&format!("RMD {}", p.as_wire_str()), true).map(|_| ())
    }

    pub fn mkdir(&self, p: &RemotePath, mode: Option<u32>) -> R<()> {
        self.check(p)?;
        let mut c = self.conn()?;
        let r = c.cmd(&format!("MKD {}", p.as_wire_str()))?;
        if !r.ok() {
            let e = map_reply(&r, false);
            if e.kind == ErrorKind::Failure && c.exists(p)?.is_some() {
                return Err(RemoteError::new(ErrorKind::AlreadyExists, format!("{p} already exists")));
            }
            return Err(e);
        }
        if let Some(m) = mode {
            let _ = c.site_chmod(p, m);
        }
        Ok(())
    }

    pub fn chmod(&self, p: &RemotePath, mode: u32) -> R<()> {
        self.check(p)?;
        self.conn()?.site_chmod(p, mode)
    }

    /// FTP has no `realpath`: the lexical path is returned once it is known to exist.
    pub fn realpath(&self, p: &RemotePath) -> R<RemotePath> {
        self.check(p)?;
        match self.conn()?.exists(p)? {
            Some(_) => Ok(p.clone()),
            None => Err(RemoteError::new(ErrorKind::NotFound, format!("{p} does not exist"))),
        }
    }

    pub fn readlink(&self, _p: &RemotePath) -> R<String> {
        Err(RemoteError::new(ErrorKind::Unsupported, "FTP cannot read symbolic links"))
    }
}
