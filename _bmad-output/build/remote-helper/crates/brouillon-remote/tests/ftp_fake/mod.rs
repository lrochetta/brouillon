//! In-process fake FTP / FTPS server for the helper's tests. No socket is ever opened: the control
//! and data connections are in-memory duplex pipes with read timeouts, handed out by [`MemDialer`].
//! TLS is real (rustls on both ends, certificates in `tests/testdata`, test-only keys).
#![allow(dead_code)]

use brouillon_remote::ftp::{Closer, Dialed, Dialer, FtpEnv, Stream};
use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};
use rustls::{RootCertStore, ServerConfig, ServerConnection, StreamOwned};
use std::collections::{BTreeMap, VecDeque};
use std::io::{Read, Write};
use std::net::{IpAddr, Ipv4Addr};
use std::sync::atomic::{AtomicBool, AtomicU16, AtomicUsize, Ordering};
use std::sync::mpsc::{channel, sync_channel, Receiver, RecvTimeoutError, Sender, SyncSender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

pub const PASSWORD: &str = "s3cret-Passw0rd!";
pub const PEER: IpAddr = IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1));

// ------------------------------------------------------------------ in-memory pipe

pub struct MemEnd {
    tx: Option<SyncSender<Vec<u8>>>,
    pub lenient: bool,
    rx: Receiver<Vec<u8>>,
    buf: VecDeque<u8>,
    timeout: Duration,
    closed: Arc<AtomicBool>,
}

pub fn pair(ct: Duration, st: Duration) -> (MemEnd, MemEnd, Arc<AtomicBool>) {
    let (t1, r1) = sync_channel(256);
    let (t2, r2) = sync_channel(256);
    let closed = Arc::new(AtomicBool::new(false));
    (
        MemEnd { lenient: false, tx: Some(t1), rx: r2, buf: VecDeque::new(), timeout: ct, closed: closed.clone() },
        MemEnd { lenient: false, tx: Some(t2), rx: r1, buf: VecDeque::new(), timeout: st, closed: closed.clone() },
        closed,
    )
}

impl Read for MemEnd {
    fn read(&mut self, out: &mut [u8]) -> std::io::Result<usize> {
        let start = Instant::now();
        while self.buf.is_empty() {
            if self.closed.load(Ordering::Relaxed) {
                return Ok(0);
            }
            let left = self.timeout.saturating_sub(start.elapsed());
            if left.is_zero() {
                return Err(std::io::ErrorKind::TimedOut.into());
            }
            match self.rx.recv_timeout(left.min(Duration::from_millis(25))) {
                Ok(v) => self.buf.extend(v),
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => return Ok(0),
            }
        }
        let n = out.len().min(self.buf.len());
        for o in out.iter_mut().take(n) {
            *o = self.buf.pop_front().unwrap();
        }
        Ok(n)
    }
}

impl Write for MemEnd {
    fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
        if self.closed.load(Ordering::Relaxed) {
            return Err(std::io::ErrorKind::BrokenPipe.into());
        }
        match &self.tx {
            Some(t) => match t.send(b.to_vec()) {
                Ok(()) => Ok(b.len()),
                // a lenient end behaves like a TCP write racing a close: it is accepted and lost
                Err(_) if self.lenient => Ok(b.len()),
                Err(_) => Err(std::io::ErrorKind::BrokenPipe.into()),
            },
            None => Err(std::io::ErrorKind::BrokenPipe.into()),
        }
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

// ------------------------------------------------------------------ the world

#[derive(Clone, Debug)]
pub enum Node {
    File { data: Vec<u8>, mtime: i64, mode: u32 },
    Dir,
    Link(String),
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Cert {
    Leaf,
    Other,
}

#[derive(Clone)]
pub struct Opts {
    pub tls: bool,
    pub implicit: bool,
    pub tls12_only: bool,
    pub refuse_auth_tls: bool,
    pub inject_after_auth: bool,
    pub require_reuse: bool,
    pub mlst: bool,
    pub hash: bool,
    pub dos_list: bool,
    pub no_epsv: bool,
    /// Host advertised in PASV/EPSV replies (never reachable: the client must ignore it).
    pub pasv_host: String,
    pub epsv_with_address: bool,
    pub rnto_overwrites: bool,
    pub no_close_notify: bool,
    /// LIST / MLSD send this raw payload instead of the real listing.
    pub raw_listing: Option<Vec<u8>>,
    /// LIST / MLSD never stop sending (until the client hangs up).
    pub endless_listing: bool,
    /// Reply to PWD with a line this long.
    pub huge_pwd_reply: usize,
    /// STOR: kill the data connection AND the control connection after this many bytes.
    pub drop_upload_after: Option<usize>,
    /// STOR: store only this many bytes but still say 226.
    pub store_only: Option<usize>,
    /// RETR: send only this many bytes then claim success.
    pub retr_short: Option<usize>,
    /// Never answer a command that starts with this prefix.
    pub stall_on: Option<String>,
    pub password: String,
    pub idle: Duration,
}

impl Default for Opts {
    fn default() -> Self {
        Opts {
            tls: true,
            implicit: false,
            tls12_only: false,
            refuse_auth_tls: false,
            inject_after_auth: false,
            require_reuse: false,
            mlst: true,
            hash: false,
            dos_list: false,
            no_epsv: false,
            pasv_host: "6.6.6.6".into(),
            epsv_with_address: false,
            rnto_overwrites: true,
            no_close_notify: false,
            raw_listing: None,
            endless_listing: false,
            huge_pwd_reply: 0,
            drop_upload_after: None,
            store_only: None,
            retr_short: None,
            stall_on: None,
            password: PASSWORD.into(),
            idle: Duration::from_secs(3),
        }
    }
}

pub struct World {
    pub fs: Mutex<BTreeMap<String, Node>>,
    pub log: Mutex<Vec<String>>,
    pub dials: Mutex<Vec<(IpAddr, u16)>>,
    pub data_handshakes_resumed: Mutex<Vec<bool>>,
    pub pass_in_clear: AtomicBool,
    pub pass_seen: AtomicBool,
    pub cert: Mutex<Cert>,
    pub opts: Mutex<Opts>,
    pub stop: AtomicBool,
    pub control_connections: AtomicUsize,
    data_tx: Mutex<Sender<MemEnd>>,
    data_rx: Arc<Mutex<Receiver<MemEnd>>>,
    port: AtomicU16,
    tls_cfg: Mutex<Option<(Cert, bool, Arc<ServerConfig>)>>,
}

pub struct Fake {
    pub world: Arc<World>,
    pub env: FtpEnv,
}

impl Drop for Fake {
    fn drop(&mut self) {
        self.world.stop.store(true, Ordering::Relaxed);
    }
}

fn data_file(name: &str) -> Vec<u8> {
    std::fs::read(std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/testdata").join(name)).unwrap()
}

pub fn ca_roots() -> Arc<RootCertStore> {
    let mut r = RootCertStore::empty();
    r.add(CertificateDer::from(data_file("ca.der"))).unwrap();
    Arc::new(r)
}

pub fn fingerprint_of(c: Cert) -> String {
    let der = data_file(if c == Cert::Leaf { "leaf.der" } else { "other.der" });
    let d = ring::digest::digest(&ring::digest::SHA256, &der);
    d.as_ref().iter().map(|b| format!("{b:02x}")).collect()
}

fn server_config(c: Cert, tls12: bool) -> Arc<ServerConfig> {
    let (cert, key) = if c == Cert::Leaf { ("leaf.der", "leaf.key.der") } else { ("other.der", "other.key.der") };
    let chain = vec![CertificateDer::from(data_file(cert))];
    let key = PrivateKeyDer::from(PrivatePkcs8KeyDer::from(data_file(key)));
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let b = ServerConfig::builder_with_provider(provider);
    let b = if tls12 { b.with_protocol_versions(&[&rustls::version::TLS12]).unwrap() } else { b.with_safe_default_protocol_versions().unwrap() };
    Arc::new(b.with_no_client_auth().with_single_cert(chain, key).unwrap())
}

impl World {
    pub fn log_has(&self, prefix: &str) -> bool {
        self.log.lock().unwrap().iter().any(|l| l.starts_with(prefix))
    }
    pub fn log_count(&self, prefix: &str) -> usize {
        self.log.lock().unwrap().iter().filter(|l| l.starts_with(prefix)).count()
    }
    pub fn file(&self, p: &str) -> Option<Vec<u8>> {
        match self.fs.lock().unwrap().get(p) {
            Some(Node::File { data, .. }) => Some(data.clone()),
            _ => None,
        }
    }
    pub fn names(&self) -> Vec<String> {
        self.fs.lock().unwrap().keys().cloned().collect()
    }
    pub fn put(&self, p: &str, data: &[u8]) {
        self.fs.lock().unwrap().insert(p.into(), Node::File { data: data.to_vec(), mtime: 1_700_000_000, mode: 0o644 });
    }
    pub fn dir(&self, p: &str) {
        self.fs.lock().unwrap().insert(p.into(), Node::Dir);
    }
    pub fn link(&self, p: &str, target: &str) {
        self.fs.lock().unwrap().insert(p.into(), Node::Link(target.into()));
    }
    pub fn set_cert(&self, c: Cert) {
        *self.cert.lock().unwrap() = c;
    }
    pub fn update(&self, f: impl FnOnce(&mut Opts)) {
        f(&mut self.opts.lock().unwrap());
    }
    fn tls_config(&self) -> Arc<ServerConfig> {
        let want = (*self.cert.lock().unwrap(), self.opts.lock().unwrap().tls12_only);
        let mut g = self.tls_cfg.lock().unwrap();
        match &*g {
            Some((c, t, cfg)) if (*c, *t) == want => cfg.clone(),
            _ => {
                let cfg = server_config(want.0, want.1);
                *g = Some((want.0, want.1, cfg.clone()));
                cfg
            }
        }
    }
}

pub struct MemDialer {
    world: Arc<World>,
}

impl Dialer for MemDialer {
    fn control(&self, _host: &str, _port: u16, _ct: Duration, _idle: Duration) -> std::io::Result<Dialed> {
        let w = self.world.clone();
        let opts = w.opts.lock().unwrap().clone();
        let (client, server, closed) = pair(opts.idle, Duration::from_secs(30));
        w.control_connections.fetch_add(1, Ordering::Relaxed);
        std::thread::spawn(move || serve(w, server));
        let c = closed.clone();
        let closer: Closer = Arc::new(move || c.store(true, Ordering::Relaxed));
        Ok(Dialed { stream: Box::new(client), peer_ip: PEER, closer })
    }
    fn data(&self, ip: IpAddr, port: u16, _ct: Duration, _idle: Duration) -> std::io::Result<Dialed> {
        self.world.dials.lock().unwrap().push((ip, port));
        let rx = self.world.data_rx.lock().unwrap();
        match rx.recv_timeout(Duration::from_secs(2)) {
            Ok(end) => {
                let closed = end.closed.clone();
                let closer: Closer = Arc::new(move || closed.store(true, Ordering::Relaxed));
                Ok(Dialed { stream: Box::new(end), peer_ip: ip, closer })
            }
            Err(_) => Err(std::io::ErrorKind::ConnectionRefused.into()),
        }
    }
}

impl Fake {
    pub fn new(opts: Opts) -> Fake {
        let (tx, rx) = channel();
        let mut fs = BTreeMap::new();
        fs.insert("/".to_string(), Node::Dir);
        fs.insert("/home".to_string(), Node::Dir);
        fs.insert("/home/u".to_string(), Node::Dir);
        let world = Arc::new(World {
            fs: Mutex::new(fs),
            log: Mutex::new(vec![]),
            dials: Mutex::new(vec![]),
            data_handshakes_resumed: Mutex::new(vec![]),
            pass_in_clear: AtomicBool::new(false),
            pass_seen: AtomicBool::new(false),
            cert: Mutex::new(Cert::Leaf),
            opts: Mutex::new(opts),
            stop: AtomicBool::new(false),
            control_connections: AtomicUsize::new(0),
            data_tx: Mutex::new(tx),
            data_rx: Arc::new(Mutex::new(rx)),
            port: AtomicU16::new(40000),
            tls_cfg: Mutex::new(None),
        });
        let env = FtpEnv { dialer: Arc::new(MemDialer { world: world.clone() }), roots: Some(ca_roots()) };
        Fake { world, env }
    }
}

// ------------------------------------------------------------------ the server

enum Io {
    Plain(MemEnd),
    Tls(Box<StreamOwned<ServerConnection, MemEnd>>),
}

impl Read for Io {
    fn read(&mut self, b: &mut [u8]) -> std::io::Result<usize> {
        match self {
            Io::Plain(s) => s.read(b),
            Io::Tls(s) => s.read(b),
        }
    }
}
impl Write for Io {
    fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
        match self {
            Io::Plain(s) => s.write(b),
            Io::Tls(s) => s.write(b),
        }
    }
    fn flush(&mut self) -> std::io::Result<()> {
        match self {
            Io::Plain(s) => s.flush(),
            Io::Tls(s) => s.flush(),
        }
    }
}

struct Srv {
    w: Arc<World>,
    io: Option<Io>,
    rbuf: Vec<u8>,
    user: Option<String>,
    authed: bool,
    tls_ctrl: bool,
    prot_p: bool,
    cwd: String,
    rnfr: Option<String>,
    data: Option<MemEnd>,
    rest: u64,
}

fn serve(w: Arc<World>, end: MemEnd) {
    let opts = w.opts.lock().unwrap().clone();
    let mut s = Srv { w: w.clone(), io: None, rbuf: vec![], user: None, authed: false, tls_ctrl: false, prot_p: false, cwd: "/home/u".into(), rnfr: None, data: None, rest: 0 };
    if opts.tls && opts.implicit {
        s.io = Some(Io::Tls(Box::new(StreamOwned::new(ServerConnection::new(w.tls_config()).unwrap(), end))));
        s.tls_ctrl = true;
    } else {
        s.io = Some(Io::Plain(end));
    }
    if s.send("220 fake ftp ready").is_err() {
        return;
    }
    while !w.stop.load(Ordering::Relaxed) {
        let Some(line) = s.read_line() else { break };
        let opts = w.opts.lock().unwrap().clone();
        let shown = if line.to_ascii_uppercase().starts_with("PASS ") {
            w.pass_seen.store(true, Ordering::Relaxed);
            if !s.tls_ctrl && opts.tls {
                w.pass_in_clear.store(true, Ordering::Relaxed);
            }
            if !opts.tls {
                w.pass_in_clear.store(true, Ordering::Relaxed);
            }
            "PASS ***".to_string()
        } else {
            line.clone()
        };
        w.log.lock().unwrap().push(shown);
        if let Some(p) = &opts.stall_on {
            if line.starts_with(p.as_str()) {
                while !w.stop.load(Ordering::Relaxed) {
                    std::thread::sleep(Duration::from_millis(20));
                    if s.io.is_none() {
                        break;
                    }
                }
                return;
            }
        }
        if !s.handle(&line, &opts) {
            break;
        }
    }
}

impl Srv {
    fn send(&mut self, l: &str) -> std::io::Result<()> {
        let io = self.io.as_mut().ok_or(std::io::ErrorKind::BrokenPipe)?;
        io.write_all(format!("{l}\r\n").as_bytes())?;
        io.flush()
    }
    fn send_raw(&mut self, b: &[u8]) -> std::io::Result<()> {
        let io = self.io.as_mut().ok_or(std::io::ErrorKind::BrokenPipe)?;
        io.write_all(b)?;
        io.flush()
    }
    fn read_line(&mut self) -> Option<String> {
        loop {
            if let Some(i) = self.rbuf.iter().position(|b| *b == b'\n') {
                let l: Vec<u8> = self.rbuf.drain(..=i).collect();
                return Some(String::from_utf8_lossy(&l).trim_end_matches(['\r', '\n']).to_string());
            }
            let mut c = [0u8; 1024];
            match self.io.as_mut()?.read(&mut c) {
                Ok(0) | Err(_) => return None,
                Ok(n) => self.rbuf.extend_from_slice(&c[..n]),
            }
        }
    }
    fn abs(&self, arg: &str) -> String {
        if arg.starts_with('/') {
            arg.to_string()
        } else if self.cwd == "/" {
            format!("/{arg}")
        } else {
            format!("{}/{}", self.cwd, arg)
        }
    }

    fn handle(&mut self, line: &str, o: &Opts) -> bool {
        let (cmd, arg) = line.split_once(' ').unwrap_or((line, ""));
        let cmd = cmd.to_ascii_uppercase();
        let ok = |s: &mut Srv, l: &str| s.send(l).is_ok();
        match cmd.as_str() {
            "AUTH" => {
                if !o.tls || o.refuse_auth_tls {
                    return ok(self, "504 AUTH not available");
                }
                let reply: &[u8] = if o.inject_after_auth { b"234 go ahead\r\n220 injected greeting\r\n" } else { b"234 go ahead\r\n" };
                if self.send_raw(reply).is_err() {
                    return false;
                }
                let Some(Io::Plain(end)) = self.io.take() else { return false };
                self.io = Some(Io::Tls(Box::new(StreamOwned::new(ServerConnection::new(self.w.tls_config()).unwrap(), end))));
                self.tls_ctrl = true;
                true
            }
            "PBSZ" => ok(self, "200 PBSZ=0"),
            "PROT" => {
                self.prot_p = arg.eq_ignore_ascii_case("P");
                ok(self, if self.prot_p { "200 protection P" } else { "200 protection C" })
            }
            "USER" => {
                self.user = Some(arg.to_string());
                if arg == "anonymous" {
                    self.authed = true;
                    return ok(self, "230 anonymous ok");
                }
                ok(self, "331 password please")
            }
            "PASS" => {
                if arg == o.password {
                    self.authed = true;
                    ok(self, "230 logged in")
                } else {
                    ok(self, "530 Login incorrect.")
                }
            }
            _ if !self.authed => ok(self, "530 Please login with USER and PASS."),
            "FEAT" => {
                let mut f = vec!["211-Features:".to_string(), " UTF8".into(), " SIZE".into(), " MDTM".into(), " REST STREAM".into(), " EPSV".into()];
                if o.mlst {
                    f.push(" MLST type*;size*;modify*;UNIX.mode*;".into());
                }
                if o.hash {
                    f.push(" HASH SHA-256*;SHA-1".into());
                }
                f.push("211 End".into());
                let _ = o;
                self.send_raw((f.join("\r\n") + "\r\n").as_bytes()).is_ok()
            }
            "OPTS" => ok(self, "200 UTF8 set"),
            "TYPE" => ok(self, "200 binary"),
            "NOOP" => ok(self, "200 ok"),
            "QUIT" => {
                let _ = self.send("221 bye");
                false
            }
            "PWD" => {
                if o.huge_pwd_reply > 0 {
                    let big = format!("257 \"{}\"\r\n", "a".repeat(o.huge_pwd_reply));
                    return self.send_raw(big.as_bytes()).is_ok();
                }
                let l = format!("257 \"{}\" is the current directory", self.cwd);
                ok(self, &l)
            }
            "CWD" => {
                let p = self.abs(arg);
                if matches!(self.w.fs.lock().unwrap().get(&p), Some(Node::Dir)) {
                    self.cwd = p;
                    ok(self, "250 ok")
                } else {
                    ok(self, "550 Failed to change directory.")
                }
            }
            "SIZE" => {
                let p = self.abs(arg);
                let r = match self.w.fs.lock().unwrap().get(&p) {
                    Some(Node::File { data, .. }) => format!("213 {}", data.len()),
                    _ => "550 Could not get file size.".to_string(),
                };
                ok(self, &r)
            }
            "MDTM" => {
                let p = self.abs(arg);
                let r = match self.w.fs.lock().unwrap().get(&p) {
                    Some(Node::File { mtime, .. }) => format!("213 {}", fmt_mdtm(*mtime)),
                    _ => "550 Could not get file modification time.".to_string(),
                };
                ok(self, &r)
            }
            "MLST" if o.mlst => {
                let p = self.abs(arg);
                let fs = self.w.fs.lock().unwrap();
                match fs.get(&p) {
                    Some(n) => {
                        let facts = facts_of(n);
                        let name = p.clone();
                        drop(fs);
                        self.send_raw(format!("250-Listing {p}\r\n {facts} {name}\r\n250 End\r\n").as_bytes()).is_ok()
                    }
                    None => {
                        drop(fs);
                        ok(self, "550 No such file or directory")
                    }
                }
            }
            "MLST" => ok(self, "500 Unknown command"),
            "REST" => {
                self.rest = arg.parse().unwrap_or(0);
                ok(self, "350 restarting")
            }
            "EPSV" => {
                if o.no_epsv {
                    return ok(self, "500 EPSV not understood");
                }
                let port = self.new_data_port();
                let l = if o.epsv_with_address { format!("229 Entering Extended Passive Mode (|1|{}|{port}|)", o.pasv_host) } else { format!("229 Entering Extended Passive Mode (|||{port}|)") };
                ok(self, &l)
            }
            "PASV" => {
                let port = self.new_data_port();
                let ip = o.pasv_host.replace('.', ",");
                let l = format!("227 Entering Passive Mode ({ip},{},{})", port / 256, port % 256);
                ok(self, &l)
            }
            "LIST" | "MLSD" => self.do_list(&cmd, arg, o),
            "RETR" => self.do_retr(arg, o),
            "STOR" => self.do_stor(arg, o),
            "DELE" => {
                let p = self.abs(arg);
                let mut fs = self.w.fs.lock().unwrap();
                let r = if matches!(fs.get(&p), Some(Node::File { .. }) | Some(Node::Link(_))) {
                    fs.remove(&p);
                    "250 deleted"
                } else {
                    "550 Delete operation failed."
                };
                drop(fs);
                ok(self, r)
            }
            "MKD" => {
                let p = self.abs(arg);
                let mut fs = self.w.fs.lock().unwrap();
                let r = if fs.contains_key(&p) {
                    "550 Create directory operation failed."
                } else {
                    fs.insert(p, Node::Dir);
                    "257 created"
                };
                drop(fs);
                ok(self, r)
            }
            "RMD" => {
                let p = self.abs(arg);
                let mut fs = self.w.fs.lock().unwrap();
                let r = if matches!(fs.get(&p), Some(Node::Dir)) {
                    fs.remove(&p);
                    "250 removed"
                } else {
                    "550 Remove directory operation failed."
                };
                drop(fs);
                ok(self, r)
            }
            "RNFR" => {
                let p = self.abs(arg);
                if self.w.fs.lock().unwrap().contains_key(&p) {
                    self.rnfr = Some(p);
                    ok(self, "350 Ready for RNTO.")
                } else {
                    ok(self, "550 RNFR command failed.")
                }
            }
            "RNTO" => {
                let to = self.abs(arg);
                let Some(from) = self.rnfr.take() else { return ok(self, "503 RNFR required first") };
                let mut fs = self.w.fs.lock().unwrap();
                if fs.contains_key(&to) && !o.rnto_overwrites {
                    drop(fs);
                    return ok(self, "553 Could not rename: file exists.");
                }
                let n = fs.remove(&from).unwrap();
                fs.insert(to, n);
                drop(fs);
                ok(self, "250 Rename successful.")
            }
            "SITE" => {
                let up = arg.to_ascii_uppercase();
                if let Some(rest) = up.strip_prefix("CHMOD ") {
                    let _ = rest;
                    return ok(self, "200 SITE CHMOD command ok.");
                }
                ok(self, "500 Unknown SITE command.")
            }
            _ => ok(self, "500 Unknown command."),
        }
    }

    fn new_data_port(&mut self) -> u16 {
        let port = self.w.port.fetch_add(1, Ordering::Relaxed);
        let (c, s, _closed) = pair(self.w.opts.lock().unwrap().idle, Duration::from_secs(30));
        self.w.data_tx.lock().unwrap().send(c).unwrap();
        self.data = Some(s);
        port
    }

    /// Wrap the data end with TLS when PROT P; returns a `Stream`-like pair used by the transfer fns.
    fn open_data(&mut self, o: &Opts) -> Option<DataIo> {
        let Some(end) = self.data.take() else {
            let _ = self.send("425 Use PASV first.");
            return None;
        };
        if self.send("150 Here comes the data.").is_err() {
            return None;
        }
        if self.prot_p && o.tls {
            let mut s = StreamOwned::new(ServerConnection::new(self.w.tls_config()).unwrap(), end);
            // drive the handshake now so reuse can be checked before any data flows
            while s.conn.is_handshaking() {
                if let Err(e) = s.conn.complete_io(&mut s.sock) {
                    if !s.conn.is_handshaking() {
                        break; // peer already finished and hung up (small upload): same as a TCP write that races a close
                    }
                    eprintln!("fake: data TLS handshake failed: {e:?}");
                    let _ = self.send("425 TLS negotiation failed.");
                    return None;
                }
            }
            let resumed = s.conn.handshake_kind() == Some(rustls::HandshakeKind::Resumed);
            self.w.data_handshakes_resumed.lock().unwrap().push(resumed);
            if o.require_reuse && !resumed {
                let _ = self.send("522 TLS session reuse required.");
                return None;
            }
            Some(DataIo::Tls(Box::new(s)))
        } else {
            Some(DataIo::Plain(end))
        }
    }

    fn do_list(&mut self, cmd: &str, arg: &str, o: &Opts) -> bool {
        let dir = if cmd == "MLSD" { self.abs(arg) } else { self.cwd.clone() };
        let payload: Option<Vec<u8>> = if let Some(r) = &o.raw_listing {
            Some(r.clone())
        } else if o.endless_listing {
            None
        } else {
            let fs = self.w.fs.lock().unwrap();
            if !matches!(fs.get(&dir), Some(Node::Dir)) {
                drop(fs);
                self.data = None;
                return self.send("550 Failed to open directory.").is_ok();
            }
            let prefix = if dir == "/" { "/".to_string() } else { format!("{dir}/") };
            let mut out = String::new();
            for (k, n) in fs.iter() {
                if k.starts_with(&prefix) && k.len() > prefix.len() && !k[prefix.len()..].contains('/') {
                    let name = &k[prefix.len()..];
                    if cmd == "MLSD" {
                        out += &format!("{} {name}\r\n", facts_of(n));
                    } else if o.dos_list {
                        match n {
                            Node::Dir => out += &format!("01-15-24  03:45PM       <DIR>          {name}\r\n"),
                            Node::File { data, .. } => out += &format!("01-15-24  03:45PM {:>20} {name}\r\n", data.len()),
                            Node::Link(_) => {}
                        }
                    } else {
                        match n {
                            Node::Dir => out += &format!("drwxr-xr-x   2 ftp ftp 4096 Jan 12  2023 {name}\r\n"),
                            Node::File { data, mode, .. } => out += &format!("-{} 1 ftp ftp {} Jan 12  2023 {name}\r\n", perm_str(*mode), data.len()),
                            Node::Link(t) => out += &format!("lrwxrwxrwx   1 ftp ftp {} Jan 12  2023 {name} -> {t}\r\n", t.len()),
                        }
                    }
                }
            }
            Some(out.into_bytes())
        };
        let Some(mut d) = self.open_data(o) else { return self.io.is_some() };
        let mut failed = false;
        match payload {
            Some(p) => {
                if d.write_all(&p).is_err() {
                    failed = true;
                }
            }
            None => {
                let mut n = 0u64;
                while !self.w.stop.load(Ordering::Relaxed) {
                    let mut batch = String::new();
                    for _ in 0..200 {
                        n += 1;
                        if cmd == "MLSD" {
                            batch += &format!("type=file;size=1; endless-file-{n}\r\n");
                        } else {
                            batch += &format!("-rw-r--r-- 1 ftp ftp 1 Jan 12  2023 endless-file-{n}\r\n");
                        }
                    }
                    if d.write_all(batch.as_bytes()).is_err() {
                        failed = true;
                        break;
                    }
                }
            }
        }
        if !failed {
            d.finish(o.no_close_notify);
        }
        drop(d);
        if failed {
            self.send("426 Connection closed; transfer aborted.").is_ok()
        } else {
            self.send("226 Directory send OK.").is_ok()
        }
    }

    fn do_retr(&mut self, arg: &str, o: &Opts) -> bool {
        let p = self.abs(arg);
        let data = match self.w.fs.lock().unwrap().get(&p) {
            Some(Node::File { data, .. }) => Some(data.clone()),
            _ => None,
        };
        let Some(data) = data else {
            self.data = None;
            return self.send("550 Failed to open file.").is_ok();
        };
        let rest = std::mem::take(&mut self.rest) as usize;
        let Some(mut d) = self.open_data(o) else { return self.io.is_some() };
        let body = &data[rest.min(data.len())..];
        let body = match o.retr_short {
            Some(n) => &body[..n.min(body.len())],
            None => body,
        };
        let failed = d.write_all(body).is_err();
        if !failed {
            d.finish(o.no_close_notify);
        }
        drop(d);
        if failed {
            self.send("426 Failure writing network stream.").is_ok()
        } else {
            self.send("226 Transfer complete.").is_ok()
        }
    }

    fn do_stor(&mut self, arg: &str, o: &Opts) -> bool {
        let p = self.abs(arg);
        if let Some(e) = self.data.as_mut() {
            e.lenient = true;
        }
        let Some(mut d) = self.open_data(o) else { return self.io.is_some() };
        let mut buf = Vec::new();
        let mut chunk = [0u8; 8192];
        loop {
            match d.read(&mut chunk) {
                Ok(0) => break,
                Ok(n) => {
                    buf.extend_from_slice(&chunk[..n]);
                    if let Some(limit) = o.drop_upload_after {
                        if buf.len() >= limit {
                            // the network dies: both connections vanish, a partial file is left behind
                            self.w.fs.lock().unwrap().insert(p, Node::File { data: buf, mtime: 1_700_000_000, mode: 0o644 });
                            drop(d);
                            self.io = None;
                            return false;
                        }
                    }
                }
                Err(_) => {
                    drop(d);
                    return self.send("426 Failure reading network stream.").is_ok();
                }
            }
        }
        drop(d);
        if let Some(n) = o.store_only {
            buf.truncate(n);
        }
        self.w.fs.lock().unwrap().insert(p, Node::File { data: buf, mtime: 1_700_000_000, mode: 0o644 });
        self.send("226 Transfer complete.").is_ok()
    }
}

enum DataIo {
    Plain(MemEnd),
    Tls(Box<StreamOwned<ServerConnection, MemEnd>>),
}

impl DataIo {
    fn write_all(&mut self, b: &[u8]) -> std::io::Result<()> {
        match self {
            DataIo::Plain(s) => s.write_all(b),
            DataIo::Tls(s) => s.write_all(b),
        }
    }
    fn read(&mut self, b: &mut [u8]) -> std::io::Result<usize> {
        match self {
            DataIo::Plain(s) => s.read(b),
            DataIo::Tls(s) => s.read(b),
        }
    }
    fn finish(&mut self, no_close_notify: bool) {
        if let DataIo::Tls(s) = self {
            let _ = s.flush();
            if !no_close_notify {
                s.conn.send_close_notify();
            }
            let _ = s.flush();
        }
    }
}

fn perm_str(mode: u32) -> String {
    let mut s = String::new();
    for (i, c) in "rwxrwxrwx".chars().enumerate() {
        s.push(if mode & (1 << (8 - i)) != 0 { c } else { '-' });
    }
    s
}

fn fmt_mdtm(t: i64) -> String {
    // t is within 2023-2030 in the tests: civil-from-days
    let days = t.div_euclid(86_400);
    let rem = t.rem_euclid(86_400);
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    format!("{y:04}{m:02}{d:02}{:02}{:02}{:02}", rem / 3600, rem % 3600 / 60, rem % 60)
}

fn facts_of(n: &Node) -> String {
    match n {
        Node::Dir => "type=dir;modify=20240229120000;UNIX.mode=0755;".into(),
        Node::File { data, mtime, mode } => format!("type=file;size={};modify={};UNIX.mode={:04o};", data.len(), fmt_mdtm(*mtime), mode),
        Node::Link(t) => format!("type=OS.unix=slink:{t};size=4;modify=20240229120000;"),
    }
}

/// Convenience: a boxed stream type check so `Stream` stays used by the tests' imports.
pub fn _assert_stream(_: &dyn Stream) {}
