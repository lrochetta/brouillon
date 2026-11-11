//! Pipelined SFTP v3 client.
//!
//! One reader thread decodes packets and routes each reply to the request with the same id.
//! A reply whose id was never issued (and was not abandoned after a timeout) is a protocol
//! violation and kills the session. Callers wait with a per-reply idle timeout.

use super::packet::*;
use super::SftpError;
use std::collections::{HashMap, HashSet, VecDeque};
use std::io::{Read, Write};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::mpsc::{channel, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::time::Duration;

#[derive(Debug, Clone)]
pub struct ClientConfig {
    /// Largest packet we accept (declared length checked before allocating).
    pub max_packet: usize,
    /// No reply for this long = `Timeout`.
    pub idle_timeout: Duration,
    /// How long the INIT/VERSION handshake may take (authentication prompts included).
    pub handshake_timeout: Duration,
    /// Outstanding READ/WRITE requests per transfer.
    pub window: usize,
    /// Bytes per READ/WRITE request.
    pub chunk: u32,
}

impl Default for ClientConfig {
    fn default() -> Self {
        ClientConfig {
            max_packet: (1 << 20) + 4096,
idle_timeout: Duration::from_secs(20),
            handshake_timeout: Duration::from_secs(180),
            window: 16,
            chunk: 32 * 1024,
        }
    }
}

struct Shared {
    pending: Mutex<HashMap<u32, Sender<Reply>>>,
    abandoned: Mutex<HashSet<u32>>,
    dead: Mutex<Option<SftpError>>,
    next_id: AtomicU32,
}

impl Shared {
    fn kill(&self, e: SftpError) {
        {
            let mut d = self.dead.lock().unwrap();
            if d.is_none() {
                *d = Some(e);
            }
        }
        self.pending.lock().unwrap().clear(); // drops the senders: waiters wake up as disconnected
    }
}

pub struct Pending {
    id: u32,
    rx: Receiver<Reply>,
}

pub struct SftpClient {
    shared: Arc<Shared>,
    writer: Mutex<Box<dyn Write + Send>>,
    pub cfg: ClientConfig,
    pub version: u32,
    pub extensions: Vec<(String, Vec<u8>)>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReadOutcome {
    pub bytes: u64,
    pub eof: bool,
}

impl SftpClient {
    /// Do the INIT/VERSION handshake over the given pipes.
    pub fn connect(
        reader: Box<dyn Read + Send>,
        mut writer: Box<dyn Write + Send>,
        cfg: ClientConfig,
    ) -> Result<SftpClient, SftpError> {
        let shared = Arc::new(Shared {
            pending: Mutex::new(HashMap::new()),
            abandoned: Mutex::new(HashSet::new()),
            dead: Mutex::new(None),
            next_id: AtomicU32::new(1),
        });
        let (init_tx, init_rx) = channel();
        {
            let shared = shared.clone();
            let max = cfg.max_packet;
            std::thread::Builder::new()
                .name("sftp-reader".into())
                .spawn(move || reader_loop(reader, shared, max, init_tx))
                .map_err(|e| SftpError::Io(e.to_string()))?;
        }
        writer
            .write_all(&Msg::Init { version: 3 }.encode())
            .and_then(|_| writer.flush())
            .map_err(|e| SftpError::Io(e.to_string()))?;
        let first = match init_rx.recv_timeout(cfg.handshake_timeout) {
            Ok(r) => r?,
            Err(RecvTimeoutError::Timeout) => return Err(SftpError::Timeout),
            Err(RecvTimeoutError::Disconnected) => return Err(SftpError::Eof),
        };
        let Reply::Version { version, extensions } = first else {
            return Err(SftpError::Protocol("expected VERSION".into()));
        };
        if version != 3 {
            return Err(SftpError::Protocol(format!("server speaks SFTP version {version}, expected 3")));
        }
        Ok(SftpClient { shared, writer: Mutex::new(writer), cfg, version, extensions })
    }

    pub fn has_extension(&self, name: &str) -> bool {
        self.extensions.iter().any(|(n, _)| n == name)
    }

    pub fn is_dead(&self) -> bool {
        self.shared.dead.lock().unwrap().is_some()
    }

    pub fn dead_reason(&self) -> Option<SftpError> {
        self.shared.dead.lock().unwrap().clone()
    }

    fn dead_error(&self) -> SftpError {
        self.dead_reason().unwrap_or(SftpError::Eof)
    }

    /// Send a request built from a fresh id.
    pub fn request(&self, build: impl FnOnce(u32) -> Msg) -> Result<Pending, SftpError> {
        if let Some(e) = self.dead_reason() {
            return Err(e);
        }
        let id = self.shared.next_id.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = channel();
        self.shared.pending.lock().unwrap().insert(id, tx);
        let pkt = build(id).encode();
        let res = {
            let mut w = self.writer.lock().unwrap();
            w.write_all(&pkt).and_then(|_| w.flush())
        };
        if let Err(e) = res {
            let e = SftpError::Io(e.to_string());
            self.shared.kill(e.clone());
            return Err(e);
        }
        Ok(Pending { id, rx })
    }

    pub fn wait(&self, p: Pending) -> Result<Reply, SftpError> {
        match p.rx.recv_timeout(self.cfg.idle_timeout) {
            Ok(r) => Ok(r),
            Err(RecvTimeoutError::Timeout) => {
                self.abandon(p.id);
                Err(SftpError::Timeout)
            }
            Err(RecvTimeoutError::Disconnected) => Err(self.dead_error()),
        }
    }

    fn abandon(&self, id: u32) {
        if self.shared.pending.lock().unwrap().remove(&id).is_some() {
            let mut a = self.shared.abandoned.lock().unwrap();
            if a.len() < 4096 {
                a.insert(id);
            }
        }
    }

    pub fn call(&self, build: impl FnOnce(u32) -> Msg) -> Result<Reply, SftpError> {
        let p = self.request(build)?;
        self.wait(p)
    }

    // ------------------------------------------------------------ typed operations

    pub fn open(&self, path: &str, pflags: u32, attrs: Attrs) -> Result<Vec<u8>, SftpError> {
        handle_of(self.call(|id| Msg::Open { id, path: path.as_bytes().to_vec(), pflags, attrs })?)
    }
    pub fn close(&self, handle: &[u8]) -> Result<(), SftpError> {
        status_ok(self.call(|id| Msg::Close { id, handle: handle.to_vec() })?)
    }
    pub fn opendir(&self, path: &str) -> Result<Vec<u8>, SftpError> {
        handle_of(self.call(|id| Msg::Opendir { id, path: path.as_bytes().to_vec() })?)
    }
    /// `None` at end of directory.
    pub fn readdir(&self, handle: &[u8]) -> Result<Option<Vec<NameEntry>>, SftpError> {
        match self.call(|id| Msg::Readdir { id, handle: handle.to_vec() })? {
            Reply::Name { entries, .. } => Ok(Some(entries)),
            Reply::Status { code: FX_EOF, .. } => Ok(None),
            other => Err(unexpected(other)),
        }
    }
    pub fn stat(&self, path: &str) -> Result<Attrs, SftpError> {
        attrs_of(self.call(|id| Msg::Stat { id, path: path.as_bytes().to_vec() })?)
    }
    pub fn lstat(&self, path: &str) -> Result<Attrs, SftpError> {
        attrs_of(self.call(|id| Msg::Lstat { id, path: path.as_bytes().to_vec() })?)
    }
    pub fn fstat(&self, handle: &[u8]) -> Result<Attrs, SftpError> {
        attrs_of(self.call(|id| Msg::Fstat { id, handle: handle.to_vec() })?)
    }
    pub fn set_mode(&self, path: &str, mode: u32) -> Result<(), SftpError> {
        status_ok(self.call(|id| Msg::Setstat { id, path: path.as_bytes().to_vec(), attrs: Attrs::with_perm(mode & 0o7777) })?)
    }
    pub fn mkdir(&self, path: &str, mode: Option<u32>) -> Result<(), SftpError> {
        let attrs = mode.map(|m| Attrs::with_perm(m & 0o7777)).unwrap_or_default();
        status_ok(self.call(|id| Msg::Mkdir { id, path: path.as_bytes().to_vec(), attrs })?)
    }
    pub fn rmdir(&self, path: &str) -> Result<(), SftpError> {
        status_ok(self.call(|id| Msg::Rmdir { id, path: path.as_bytes().to_vec() })?)
    }
    pub fn remove(&self, path: &str) -> Result<(), SftpError> {
        status_ok(self.call(|id| Msg::Remove { id, path: path.as_bytes().to_vec() })?)
    }
    /// Plain SFTP RENAME: OpenSSH refuses when the target exists.
    pub fn rename(&self, from: &str, to: &str) -> Result<(), SftpError> {
        status_ok(self.call(|id| Msg::Rename { id, from: from.as_bytes().to_vec(), to: to.as_bytes().to_vec() })?)
    }
    /// `posix-rename@openssh.com`: atomic, replaces the target.
    pub fn posix_rename(&self, from: &str, to: &str) -> Result<(), SftpError> {
        if !self.has_extension(EXT_POSIX_RENAME) {
            return Err(SftpError::Status { code: FX_OP_UNSUPPORTED, message: "posix-rename not offered".into() });
        }
        let mut data = Vec::new();
        wire::put_str(&mut data, from.as_bytes());
        wire::put_str(&mut data, to.as_bytes());
        status_ok(self.call(|id| Msg::Extended { id, name: EXT_POSIX_RENAME.into(), data })?)
    }
    pub fn fsync(&self, handle: &[u8]) -> Result<(), SftpError> {
        if !self.has_extension(EXT_FSYNC) {
            return Err(SftpError::Status { code: FX_OP_UNSUPPORTED, message: "fsync not offered".into() });
        }
        let mut data = Vec::new();
        wire::put_str(&mut data, handle);
        status_ok(self.call(|id| Msg::Extended { id, name: EXT_FSYNC.into(), data })?)
    }
    pub fn realpath(&self, path: &str) -> Result<Vec<u8>, SftpError> {
        single_name(self.call(|id| Msg::Realpath { id, path: path.as_bytes().to_vec() })?)
    }
    pub fn readlink(&self, path: &str) -> Result<Vec<u8>, SftpError> {
        single_name(self.call(|id| Msg::Readlink { id, path: path.as_bytes().to_vec() })?)
    }

    /// Pipelined read of `len` bytes (or to EOF) starting at `offset`.
    ///
    /// * never trusts a size: stops at `max_bytes` with `Limit` (a server cannot stream past the cap);
    /// * a reply longer than the request is a protocol violation;
    /// * a short reply that is not EOF is re-requested for the missing part, in order;
    /// * an empty DATA reply is a protocol violation (it would loop forever).
    ///
    /// `sink(offset, bytes)` is called in order; returning an error aborts.
    pub fn read_range(
        &self,
        handle: &[u8],
        offset: u64,
        len: Option<u64>,
        max_bytes: u64,
        mut sink: impl FnMut(u64, &[u8]) -> Result<(), SftpError>,
    ) -> Result<ReadOutcome, SftpError> {
        if let Some(l) = len {
            if l > max_bytes {
                return Err(SftpError::Limit(format!("requested {l} bytes, cap is {max_bytes}")));
            }
        }
        // To detect "more than the cap" on an unbounded read we ask for one byte beyond it.
        let issue_end = match len {
            Some(l) => offset.saturating_add(l),
            None => offset.saturating_add(max_bytes).saturating_add(1),
        };
        let chunk = self.cfg.chunk as u64;
        let mut next_issue = offset;
        let mut expect_off = offset;
        let mut delivered = 0u64;
        let mut queue: VecDeque<(u64, u32, Pending)> = VecDeque::new();
        loop {
            while queue.len() < self.cfg.window && next_issue < issue_end {
                let n = chunk.min(issue_end - next_issue) as u32;
                let off = next_issue;
                let p = self.request(|id| Msg::Read { id, handle: handle.to_vec(), offset: off, len: n })?;
                queue.push_back((off, n, p));
                next_issue += n as u64;
            }
            let Some((off, n, p)) = queue.pop_front() else {
                return Ok(ReadOutcome { bytes: delivered, eof: false });
            };
            match self.wait(p)? {
                Reply::Data { data, .. } => {
                    if data.len() > n as usize {
                        return Err(SftpError::Protocol(format!("READ returned {} bytes for a {} byte request", data.len(), n)));
                    }
                    if data.is_empty() {
                        return Err(SftpError::Protocol("READ returned an empty DATA".into()));
                    }
                    debug_assert_eq!(off, expect_off);
                    if len.is_none() && delivered + data.len() as u64 > max_bytes {
                        return Err(SftpError::Limit(format!("file is larger than the {max_bytes} byte cap")));
                    }
                    sink(off, &data)?;
                    delivered += data.len() as u64;
                    expect_off = off + data.len() as u64;
                    if (data.len() as u32) < n {
                        let rem_off = off + data.len() as u64;
                        let rem = n - data.len() as u32;
                        let p = self.request(|id| Msg::Read { id, handle: handle.to_vec(), offset: rem_off, len: rem })?;
                        queue.push_front((rem_off, rem, p));
                    }
                }
                Reply::Status { code: FX_EOF, .. } => {
                    return Ok(ReadOutcome { bytes: delivered, eof: true });
                }
                other => return Err(unexpected(other)),
            }
        }
    }

    /// Start a pipelined write at offset 0 on an open handle.
    pub fn write_stream<'a>(&'a self, handle: &[u8]) -> WriteStream<'a> {
        WriteStream { client: self, handle: handle.to_vec(), offset: 0, acked: 0, inflight: VecDeque::new() }
    }
}

/// Incremental pipelined writer: `push` data as it arrives, `finish` to wait for every status.
pub struct WriteStream<'a> {
    client: &'a SftpClient,
    handle: Vec<u8>,
    offset: u64,
    acked: u64,
    inflight: VecDeque<(u64, Pending)>,
}

impl WriteStream<'_> {
    /// Bytes the server has acknowledged so far.
    pub fn acked(&self) -> u64 {
        self.acked
    }
    pub fn sent(&self) -> u64 {
        self.offset
    }

    fn reap_one(&mut self) -> Result<(), SftpError> {
        if let Some((n, p)) = self.inflight.pop_front() {
            status_ok(self.client.wait(p)?)?;
            self.acked += n;
        }
        Ok(())
    }

    pub fn push(&mut self, data: &[u8]) -> Result<(), SftpError> {
        for chunk in data.chunks(self.client.cfg.chunk as usize) {
            while self.inflight.len() >= self.client.cfg.window {
                self.reap_one()?;
            }
            let off = self.offset;
            let p = self.client.request(|id| Msg::Write { id, handle: self.handle.clone(), offset: off, data: chunk.to_vec() })?;
            self.inflight.push_back((chunk.len() as u64, p));
            self.offset += chunk.len() as u64;
        }
        // also opportunistically collect finished ones so `acked` advances
        while let Some((_, p)) = self.inflight.front() {
            match p.rx.try_recv() {
                Ok(r) => {
                    let (n, _) = self.inflight.pop_front().unwrap();
                    status_ok(r)?;
                    self.acked += n;
                }
                Err(_) => break,
            }
        }
        Ok(())
    }

    pub fn finish(mut self) -> Result<u64, SftpError> {
        while !self.inflight.is_empty() {
            self.reap_one()?;
        }
        Ok(self.acked)
    }
}

// ---------------------------------------------------------------- helpers

fn reader_loop(mut r: Box<dyn Read + Send>, shared: Arc<Shared>, max: usize, init_tx: Sender<Result<Reply, SftpError>>) {
    let first = read_packet(&mut r, max).and_then(|p| Reply::decode(&p));
    match first {
        Ok(v @ Reply::Version { .. }) => {
            let _ = init_tx.send(Ok(v));
        }
        Ok(_) => {
            let e = SftpError::Protocol("first packet is not VERSION".into());
            let _ = init_tx.send(Err(e.clone()));
            shared.kill(e);
            return;
        }
        Err(e) => {
            let _ = init_tx.send(Err(e.clone()));
            shared.kill(e);
            return;
        }
    }
    loop {
        match read_packet(&mut r, max).and_then(|p| Reply::decode(&p)) {
            Ok(reply) => {
                let Some(id) = reply.id() else {
                    shared.kill(SftpError::Protocol("unexpected VERSION".into()));
                    return;
                };
                let tx = shared.pending.lock().unwrap().remove(&id);
                match tx {
                    Some(tx) => {
                        let _ = tx.send(reply);
                    }
                    None => {
                        if !shared.abandoned.lock().unwrap().remove(&id) {
                            shared.kill(SftpError::Protocol(format!("reply for unknown request id {id}")));
                            return;
                        }
                    }
                }
            }
            Err(e) => {
                shared.kill(e);
                return;
            }
        }
    }
}

fn unexpected(r: Reply) -> SftpError {
    match r {
        Reply::Status { code, message, .. } => SftpError::Status { code, message },
        other => SftpError::Protocol(format!("unexpected reply {:?}", std::mem::discriminant(&other))),
    }
}

fn status_ok(r: Reply) -> Result<(), SftpError> {
    match r {
        Reply::Status { code: FX_OK, .. } => Ok(()),
        other => Err(unexpected(other)),
    }
}

fn handle_of(r: Reply) -> Result<Vec<u8>, SftpError> {
    match r {
        Reply::Handle { handle, .. } => Ok(handle),
        other => Err(unexpected(other)),
    }
}

fn attrs_of(r: Reply) -> Result<Attrs, SftpError> {
    match r {
        Reply::Attrs { attrs, .. } => Ok(attrs),
        other => Err(unexpected(other)),
    }
}

fn single_name(r: Reply) -> Result<Vec<u8>, SftpError> {
    match r {
        Reply::Name { mut entries, .. } if entries.len() == 1 => Ok(entries.remove(0).filename),
        Reply::Name { entries, .. } => Err(SftpError::Protocol(format!("expected 1 name, got {}", entries.len()))),
        other => Err(unexpected(other)),
    }
}

use super::wire;
