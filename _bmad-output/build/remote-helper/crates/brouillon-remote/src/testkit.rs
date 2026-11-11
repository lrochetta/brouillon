//! In-process fake SFTP server (server half of the same codec) with fault injection.
//! Test support only: nothing in the helper binary uses it. It speaks over `std::io::pipe`s.

use crate::sftp::packet::*;
use crate::sftp::wire::*;
use std::collections::{BTreeMap, HashMap};
use std::io::{Read, Write};
use std::sync::{Arc, Mutex};
use std::time::Duration;

#[derive(Debug, Clone)]
pub enum Node {
    File { data: Vec<u8>, mode: u32, mtime: u32 },
    Dir { mode: u32, mtime: u32 },
    Link { target: String },
}

#[derive(Debug, Default)]
pub struct FakeFs {
    pub nodes: BTreeMap<String, Node>,
}

pub const HOME: &str = "/home/u";

impl FakeFs {
    pub fn new() -> FakeFs {
        let mut f = FakeFs::default();
        f.nodes.insert("/".into(), Node::Dir { mode: 0o755, mtime: 1 });
        f.nodes.insert("/home".into(), Node::Dir { mode: 0o755, mtime: 1 });
        f.nodes.insert(HOME.into(), Node::Dir { mode: 0o755, mtime: 1 });
        f
    }
    pub fn dir(&mut self, p: &str) -> &mut Self {
        self.nodes.insert(p.into(), Node::Dir { mode: 0o755, mtime: 1000 });
        self
    }
    pub fn file(&mut self, p: &str, data: &[u8]) -> &mut Self {
        self.nodes.insert(p.into(), Node::File { data: data.to_vec(), mode: 0o644, mtime: 1000 });
        self
    }
    pub fn link(&mut self, p: &str, target: &str) -> &mut Self {
        self.nodes.insert(p.into(), Node::Link { target: target.into() });
        self
    }
    pub fn data(&self, p: &str) -> Option<Vec<u8>> {
        match self.nodes.get(p) {
            Some(Node::File { data, .. }) => Some(data.clone()),
            _ => None,
        }
    }
    fn children(&self, dir: &str) -> Vec<(String, &Node)> {
        let prefix = if dir == "/" { "/".to_string() } else { format!("{dir}/") };
        self.nodes
            .iter()
            .filter(|(k, _)| k.starts_with(&prefix) && k.len() > prefix.len() && !k[prefix.len()..].contains('/'))
            .map(|(k, v)| (k[prefix.len()..].to_string(), v))
            .collect()
    }
}

#[derive(Clone)]
pub struct Faults {
    /// Hostile raw names appended to every directory listing.
    pub extra_names: Vec<Vec<u8>>,
    /// `/huge` lists this many synthetic entries.
    pub huge_listing: usize,
    pub reply_delay: Duration,
    /// Per-path extra delay (path -> delay) applied to STAT/LSTAT/OPEN/READ replies for it.
    pub slow_paths: HashMap<String, Duration>,
    /// Hang up (without replying) once this many bytes of WRITE data have been received.
    pub drop_after_write_bytes: Option<u64>,
    /// Never answer more than this many bytes per READ (short reads).
    pub max_read_reply: Option<usize>,
    /// Answer READ with a packet whose declared length is 2 GiB.
    pub oversize_reply_on_read: bool,
    /// Answer READ with more bytes than requested.
    pub read_returns_more: bool,
    /// Answer READ with empty DATA forever.
    pub zero_data: bool,
    /// `/endless` never reaches EOF.
    pub endless: bool,
    /// Reply to the first STAT with a wrong id.
    pub wrong_id_once: bool,
    /// Send an unsolicited STATUS before the first reply.
    pub unsolicited: bool,
    pub no_posix_rename: bool,
    pub no_fsync: bool,
    /// STAT/LSTAT report this size for regular files.
    pub lie_size: Option<u64>,
    /// VERSION number announced.
    pub version: u32,
    /// Return NAME replies with count lying (claims more entries than present).
    pub lie_name_count: bool,
    /// Handle returned by OPEN is this many bytes long.
    pub handle_len: usize,
}

impl Default for Faults {
    fn default() -> Faults {
        Faults {
            extra_names: vec![],
            huge_listing: 0,
            reply_delay: Duration::ZERO,
            slow_paths: HashMap::new(),
            drop_after_write_bytes: None,
            max_read_reply: None,
            oversize_reply_on_read: false,
            read_returns_more: false,
            zero_data: false,
            endless: false,
            wrong_id_once: false,
            unsolicited: false,
            no_posix_rename: false,
            no_fsync: false,
            lie_size: None,
            version: 3,
            lie_name_count: false,
            handle_len: 0,
        }
    }
}

pub struct FakeHandle {
    pub fs: Arc<Mutex<FakeFs>>,
    /// Every mutating request the server accepted, as text.
    pub log: Arc<Mutex<Vec<String>>>,
    /// Number of requests received.
    pub requests: Arc<Mutex<usize>>,
}

type Pipe = (Box<dyn Read + Send>, Box<dyn Write + Send>);

/// Start the server thread. Returns the client's (reader, writer) pair and a handle to inspect the state.
pub fn spawn_fake(fs: FakeFs, faults: Faults) -> (Pipe, FakeHandle) {
    let (srv_r, cli_w) = std::io::pipe().unwrap();
    let (cli_r, srv_w) = std::io::pipe().unwrap();
    let handle = FakeHandle { fs: Arc::new(Mutex::new(fs)), log: Arc::new(Mutex::new(vec![])), requests: Arc::new(Mutex::new(0)) };
    let (f, l, q) = (handle.fs.clone(), handle.log.clone(), handle.requests.clone());
    std::thread::Builder::new()
        .name("fake-sftp".into())
        .spawn(move || {
            let mut s = Server { fs: f, log: l, faults, handles: HashMap::new(), next: 1, written: 0, first_stat: true };
            let _ = s.run(Box::new(srv_r), Box::new(srv_w), q);
        })
        .unwrap();
    ((Box::new(cli_r), Box::new(cli_w)), handle)
}

enum H {
    File { path: String, write: bool },
    Dir { entries: Vec<NameEntry>, pos: usize },
}

struct Server {
    fs: Arc<Mutex<FakeFs>>,
    log: Arc<Mutex<Vec<String>>>,
    faults: Faults,
    handles: HashMap<Vec<u8>, H>,
    next: u32,
    written: u64,
    first_stat: bool,
}

fn norm(p: &str) -> String {
    let p = if p.starts_with('/') { p.to_string() } else if p == "." || p.is_empty() { HOME.to_string() } else { format!("{HOME}/{p}") };
    let mut parts: Vec<&str> = vec![];
    for c in p.split('/') {
        match c {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            c => parts.push(c),
        }
    }
    if parts.is_empty() { "/".into() } else { format!("/{}", parts.join("/")) }
}

fn status(id: u32, code: u32, m: &str) -> Reply {
    Reply::Status { id, code, message: m.into() }
}

fn attrs_of(n: &Node, lie: Option<u64>) -> Attrs {
    match n {
        Node::File { data, mode, mtime } => Attrs {
            size: Some(lie.unwrap_or(data.len() as u64)),
            uid_gid: Some((1000, 1000)),
            perm: Some(S_IFREG | mode),
            times: Some((*mtime, *mtime)),
        },
        Node::Dir { mode, mtime } => Attrs { size: Some(4096), uid_gid: Some((1000, 1000)), perm: Some(S_IFDIR | mode), times: Some((*mtime, *mtime)) },
        Node::Link { target } => Attrs { size: Some(target.len() as u64), uid_gid: Some((1000, 1000)), perm: Some(S_IFLNK | 0o777), times: Some((1, 1)) },
    }
}

impl Server {
    fn log(&self, s: String) {
        self.log.lock().unwrap().push(s);
    }

    fn resolve(&self, fs: &FakeFs, p: &str) -> Option<String> {
        let mut cur = norm(p);
        for _ in 0..8 {
            match fs.nodes.get(&cur) {
                Some(Node::Link { target }) => {
                    let base = cur.rsplit_once('/').map(|x| if x.0.is_empty() { "/" } else { x.0 }).unwrap_or("/").to_string();
                    cur = if target.starts_with('/') { norm(target) } else { norm(&format!("{base}/{target}")) };
                }
                Some(_) => return Some(cur),
                None => return None,
            }
        }
        None
    }

    fn run(&mut self, mut r: Box<dyn Read + Send>, mut w: Box<dyn Write + Send>, q: Arc<Mutex<usize>>) -> Result<(), crate::sftp::SftpError> {
        let init = read_packet(&mut r, 1 << 22)?;
        match Msg::decode(&init)? {
            Msg::Init { .. } => {}
            _ => return Ok(()),
        }
        let mut ext = vec![];
        if !self.faults.no_posix_rename {
            ext.push((EXT_POSIX_RENAME.to_string(), b"1".to_vec()));
        }
        if !self.faults.no_fsync {
            ext.push((EXT_FSYNC.to_string(), b"1".to_vec()));
        }
        let _ = w.write_all(&Reply::Version { version: self.faults.version, extensions: ext }.encode());
        if self.faults.unsolicited {
            let _ = w.write_all(&status(424242, FX_OK, "").encode());
        }
        loop {
            let pkt = match read_packet(&mut r, 1 << 22) {
                Ok(p) => p,
                Err(_) => return Ok(()),
            };
            *q.lock().unwrap() += 1;
            let msg = Msg::decode(&pkt)?;
            if !self.faults.reply_delay.is_zero() {
                std::thread::sleep(self.faults.reply_delay);
            }
            let Some(reply) = self.handle(msg) else { return Ok(()) }; // None = hang up
            let bytes = match reply {
                Ok(rep) => rep.encode(),
                Err(raw) => raw,
            };
            if w.write_all(&bytes).and_then(|_| w.flush()).is_err() {
                return Ok(());
            }
        }
    }

    /// `None` = drop the connection. `Err(bytes)` = send these raw bytes instead of a reply.
    fn handle(&mut self, m: Msg) -> Option<Result<Reply, Vec<u8>>> {
        use Msg::*;
        let fs_arc = self.fs.clone();
        let mut fs = fs_arc.lock().unwrap();
        let lie = self.faults.lie_size;
        let id = m.id().unwrap_or(0);
        if let Some(path) = match &m {
            Stat { path, .. } | Lstat { path, .. } | Open { path, .. } => Some(String::from_utf8_lossy(path).into_owned()),
            _ => None,
        } {
            if let Some(d) = self.faults.slow_paths.get(&norm(&path)) {
                drop(fs);
                std::thread::sleep(*d);
                fs = fs_arc.lock().unwrap();
            }
        }
        let rep = match m {
            Init { .. } => status(id, FX_FAILURE, "second INIT"),
            Realpath { path, .. } => {
                let raw = String::from_utf8_lossy(&path).into_owned();
                let p = self.resolve(&fs, &raw).unwrap_or_else(|| norm(&raw));
                Reply::Name { id, entries: vec![NameEntry { filename: p.into_bytes(), longname: vec![], attrs: Attrs::default() }] }
            }
            Stat { path, .. } => {
                let p = String::from_utf8_lossy(&path).into_owned();
                if std::mem::take(&mut self.first_stat) && self.faults.wrong_id_once {
                    return Some(Ok(status(id + 1000, FX_OK, "")));
                }
                match self.resolve(&fs, &p) {
                    Some(t) => Reply::Attrs { id, attrs: attrs_of(fs.nodes.get(&t).unwrap(), lie) },
                    None => status(id, FX_NO_SUCH_FILE, "no such file"),
                }
            }
            Lstat { path, .. } => {
                let p = String::from_utf8_lossy(&path).into_owned();
                if std::mem::take(&mut self.first_stat) && self.faults.wrong_id_once {
                    return Some(Ok(status(id + 1000, FX_OK, "")));
                }
                match fs.nodes.get(&norm(&p)) {
                    Some(node) => Reply::Attrs { id, attrs: attrs_of(node, lie) },
                    None => status(id, FX_NO_SUCH_FILE, "no such file"),
                }
            }
            _ => return self.handle2(m, fs, id),
        };
        Some(Ok(rep))
    }

    fn handle2(&mut self, m: Msg, mut fs: std::sync::MutexGuard<'_, FakeFs>, id: u32) -> Option<Result<Reply, Vec<u8>>> {
        use Msg::*;
        let lie = self.faults.lie_size;
        let rep = match m {
            Open { path, pflags, attrs, .. } => {
                let p = norm(&String::from_utf8_lossy(&path));
                let write = pflags & (OPEN_WRITE | OPEN_APPEND) != 0;
                let target = self.resolve(&fs, &p).unwrap_or(p.clone());
                let exists = matches!(fs.nodes.get(&target), Some(Node::File { .. }));
                if matches!(fs.nodes.get(&target), Some(Node::Dir { .. })) {
                    status(id, FX_FAILURE, "is a directory")
                } else if pflags & OPEN_EXCL != 0 && pflags & OPEN_CREAT != 0 && fs.nodes.contains_key(&p) {
                    status(id, FX_FAILURE, "exists")
                } else if !exists && pflags & OPEN_CREAT == 0 {
                    status(id, FX_NO_SUCH_FILE, "no such file")
                } else {
                    if let Some(Node::File { mode, .. }) = fs.nodes.get(&target) {
                        if write && mode & 0o200 == 0 {
                            return Some(Ok(status(id, FX_PERMISSION_DENIED, "denied")));
                        }
                    }
                    if write {
                        self.log(format!("open-write {p}"));
                    }
                    if !exists {
                        let mode = attrs.perm.unwrap_or(0o644) & 0o7777;
                        fs.nodes.insert(target.clone(), Node::File { data: vec![], mode, mtime: 2000 });
                    } else if pflags & OPEN_TRUNC != 0 {
                        if let Some(Node::File { data, .. }) = fs.nodes.get_mut(&target) {
                            data.clear();
                        }
                    }
                    let h = self.new_handle(H::File { path: target, write });
                    Reply::Handle { id, handle: h }
                }
            }
            Close { handle, .. } => {
                if self.handles.remove(&handle).is_some() { status(id, FX_OK, "") } else { status(id, FX_FAILURE, "bad handle") }
            }
            Read { handle, offset, len, .. } => {
                if self.faults.oversize_reply_on_read {
                    let mut raw = vec![0x80, 0, 0, 0, FXP_DATA];
                    raw.extend_from_slice(&id.to_be_bytes());
                    return Some(Err(raw));
                }
                if self.faults.zero_data {
                    return Some(Ok(Reply::Data { id, data: vec![] }));
                }
                let Some(H::File { path, .. }) = self.handles.get(&handle) else { return Some(Ok(status(id, FX_FAILURE, "bad handle"))) };
                if self.faults.endless && path == "/endless" {
                    let n = self.faults.max_read_reply.unwrap_or(len as usize).min(len as usize);
                    return Some(Ok(Reply::Data { id, data: vec![b'x'; n] }));
                }
                let Some(Node::File { data, .. }) = fs.nodes.get(path) else { return Some(Ok(status(id, FX_FAILURE, "gone"))) };
                if offset >= data.len() as u64 {
                    status(id, FX_EOF, "eof")
                } else {
                    let mut n = (len as usize).min(data.len() - offset as usize);
                    if let Some(m) = self.faults.max_read_reply {
                        n = n.min(m);
                    }
                    let mut out = data[offset as usize..offset as usize + n].to_vec();
                    if self.faults.read_returns_more {
                        out.extend(std::iter::repeat(b'!').take(len as usize + 10));
                    }
                    Reply::Data { id, data: out }
                }
            }
            Write { handle, offset, data, .. } => {
                self.written += data.len() as u64;
                if let Some(limit) = self.faults.drop_after_write_bytes {
                    if self.written > limit {
                        return None;
                    }
                }
                let Some(H::File { path, write: true }) = self.handles.get(&handle) else { return Some(Ok(status(id, FX_FAILURE, "bad handle"))) };
                let path = path.clone();
                if let Some(Node::File { data: d, mtime, .. }) = fs.nodes.get_mut(&path) {
                    let end = offset as usize + data.len();
                    if d.len() < end {
                        d.resize(end, 0);
                    }
                    d[offset as usize..end].copy_from_slice(&data);
                    *mtime = 3000;
                }
                self.log(format!("write {path} @{offset} +{}", data.len()));
                status(id, FX_OK, "")
            }
            Fstat { handle, .. } => match self.handles.get(&handle) {
                Some(H::File { path, .. }) => Reply::Attrs { id, attrs: attrs_of(fs.nodes.get(path).unwrap(), lie) },
                _ => status(id, FX_FAILURE, "bad handle"),
            },
            Setstat { path, attrs, .. } => {
                let p = norm(&String::from_utf8_lossy(&path));
                self.log(format!("setstat {p} {:o}", attrs.perm.unwrap_or(0)));
                match fs.nodes.get_mut(&p) {
                    Some(Node::File { mode, .. }) | Some(Node::Dir { mode, .. }) => {
                        if let Some(pm) = attrs.perm {
                            *mode = pm & 0o7777;
                        }
                        status(id, FX_OK, "")
                    }
                    _ => status(id, FX_NO_SUCH_FILE, "no such file"),
                }
            }
            Opendir { path, .. } => {
                let p = norm(&String::from_utf8_lossy(&path));
                match fs.nodes.get(&p) {
                    Some(Node::Dir { .. }) => {
                        let mut entries: Vec<NameEntry> = vec![];
                        for n in [".", ".."] {
                            entries.push(NameEntry { filename: n.into(), longname: vec![], attrs: Attrs::with_perm(S_IFDIR | 0o755) });
                        }
                        for (name, node) in fs.children(&p) {
                            entries.push(NameEntry { filename: name.into_bytes(), longname: b"-rw-r--r-- garbage".to_vec(), attrs: attrs_of(node, lie) });
                        }
                        if p == "/huge" {
                            for i in 0..self.faults.huge_listing {
                                entries.push(NameEntry { filename: format!("f{i}").into_bytes(), longname: vec![], attrs: Attrs { size: Some(1), perm: Some(S_IFREG | 0o644), ..Default::default() } });
                            }
                        }
                        for n in &self.faults.extra_names {
                            entries.push(NameEntry { filename: n.clone(), longname: vec![], attrs: Attrs::with_perm(S_IFREG | 0o644) });
                        }
                        let h = self.new_handle(H::Dir { entries, pos: 0 });
                        Reply::Handle { id, handle: h }
                    }
                    Some(_) => status(id, FX_FAILURE, "not a directory"),
                    None => status(id, FX_NO_SUCH_FILE, "no such directory"),
                }
            }
            Readdir { handle, .. } => match self.handles.get_mut(&handle) {
                Some(H::Dir { entries, pos }) => {
                    if *pos >= entries.len() {
                        status(id, FX_EOF, "eof")
                    } else {
                        let end = (*pos + 100).min(entries.len());
                        let batch = entries[*pos..end].to_vec();
                        *pos = end;
                        if self.faults.lie_name_count {
                            let mut raw = Reply::Name { id, entries: batch }.encode();
                            raw[9..13].copy_from_slice(&u32::MAX.to_be_bytes());
                            return Some(Err(raw));
                        }
                        Reply::Name { id, entries: batch }
                    }
                }
                _ => status(id, FX_FAILURE, "bad handle"),
            },
            Remove { path, .. } => {
                let p = norm(&String::from_utf8_lossy(&path));
                self.log(format!("remove {p}"));
                match fs.nodes.get(&p) {
                    Some(Node::Dir { .. }) => status(id, FX_FAILURE, "is a directory"),
                    Some(_) => {
                        fs.nodes.remove(&p);
                        status(id, FX_OK, "")
                    }
                    None => status(id, FX_NO_SUCH_FILE, "no such file"),
                }
            }
            Mkdir { path, attrs, .. } => {
                let p = norm(&String::from_utf8_lossy(&path));
                self.log(format!("mkdir {p}"));
                if fs.nodes.contains_key(&p) {
                    status(id, FX_FAILURE, "exists")
                } else {
                    fs.nodes.insert(p, Node::Dir { mode: attrs.perm.unwrap_or(0o755), mtime: 2000 });
                    status(id, FX_OK, "")
                }
            }
            Rmdir { path, .. } => {
                let p = norm(&String::from_utf8_lossy(&path));
                self.log(format!("rmdir {p}"));
                match fs.nodes.get(&p) {
                    Some(Node::Dir { .. }) if fs.children(&p).is_empty() => {
                        fs.nodes.remove(&p);
                        status(id, FX_OK, "")
                    }
                    Some(Node::Dir { .. }) => status(id, FX_FAILURE, "not empty"),
                    Some(_) => status(id, FX_FAILURE, "not a directory"),
                    None => status(id, FX_NO_SUCH_FILE, "no such file"),
                }
            }
            Rename { from, to, .. } => {
                let (a, b) = (norm(&String::from_utf8_lossy(&from)), norm(&String::from_utf8_lossy(&to)));
                self.log(format!("rename {a} {b}"));
                if !fs.nodes.contains_key(&a) {
                    status(id, FX_NO_SUCH_FILE, "no such file")
                } else if fs.nodes.contains_key(&b) {
                    status(id, FX_FAILURE, "target exists")
                } else {
                    let n = fs.nodes.remove(&a).unwrap();
                    fs.nodes.insert(b, n);
                    status(id, FX_OK, "")
                }
            }
            Readlink { path, .. } => {
                let p = norm(&String::from_utf8_lossy(&path));
                match fs.nodes.get(&p) {
                    Some(Node::Link { target }) => Reply::Name { id, entries: vec![NameEntry { filename: target.clone().into_bytes(), longname: vec![], attrs: Attrs::default() }] },
                    Some(_) => status(id, FX_FAILURE, "not a link"),
                    None => status(id, FX_NO_SUCH_FILE, "no such file"),
                }
            }
            Extended { name, data, .. } if name == EXT_POSIX_RENAME && !self.faults.no_posix_rename => {
                let mut r = Rd::new(&data);
                let (a, b) = match (r.string(), r.string()) {
                    (Ok(a), Ok(b)) => (norm(&String::from_utf8_lossy(a)), norm(&String::from_utf8_lossy(b))),
                    _ => return Some(Ok(status(id, FX_BAD_MESSAGE, "bad ext"))),
                };
                self.log(format!("posix-rename {a} {b}"));
                match fs.nodes.remove(&a) {
                    Some(n) => {
                        fs.nodes.insert(b, n);
                        status(id, FX_OK, "")
                    }
                    None => status(id, FX_NO_SUCH_FILE, "no such file"),
                }
            }
            Extended { name, .. } if name == EXT_FSYNC && !self.faults.no_fsync => {
                self.log("fsync".into());
                status(id, FX_OK, "")
            }
            Extended { .. } => status(id, FX_OP_UNSUPPORTED, "unsupported"),
            Init { .. } | Stat { .. } | Lstat { .. } | Realpath { .. } => status(id, FX_FAILURE, "unreachable"),
        };
        Some(Ok(rep))
    }

    fn new_handle(&mut self, h: H) -> Vec<u8> {
        let n = self.next;
        self.next += 1;
        let mut key = format!("h{n}").into_bytes();
        if self.faults.handle_len > 0 {
            key.resize(self.faults.handle_len, b'x');
        }
        self.handles.insert(key.clone(), h);
        key
    }
}

