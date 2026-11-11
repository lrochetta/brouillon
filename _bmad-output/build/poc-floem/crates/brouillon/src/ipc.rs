//! BK-051 — single instance over a local socket (decision D12).
//!
//! The first instance listens on a per-user socket (Windows named pipe /
//! Unix socket file) and owns a random token stored in the user's settings
//! dir. A later invocation connects, sends one JSON line `{token, verb, ..}`
//! and gets one line back (`ok` / `rejected` / `invalid`). Requests with a
//! wrong token are dropped without reaching the editor.

use std::io::{self, BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::Arc;
use std::time::Duration;

use interprocess::local_socket::{
    prelude::*, GenericFilePath, GenericNamespaced, ListenerNonblockingMode, ListenerOptions, Name, Stream,
};
use rand::{Rng, SeedableRng};
use serde::{Deserialize, Serialize};

/// What a second invocation asks the running instance to do.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "verb", rename_all = "snake_case")]
pub enum Request {
    Open {
        path: PathBuf,
        line: Option<usize>,
        col: Option<usize>,
        readonly: bool,
        new_window: bool,
        wait: bool,
    },
    Cursor { line: usize, col: usize },
    Select { start_line: usize, start_col: usize, end_line: usize, end_col: usize },
    New,
    Activate,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Response {
    Ok,
    Rejected,
    Invalid,
}

impl Response {
    fn wire(self) -> &'static str {
        match self {
            Response::Ok => "ok",
            Response::Rejected => "rejected",
            Response::Invalid => "invalid",
        }
    }

    fn parse(s: &str) -> Self {
        match s.trim() {
            "ok" => Response::Ok,
            "rejected" => Response::Rejected,
            _ => Response::Invalid,
        }
    }
}

#[derive(Serialize, Deserialize)]
struct Envelope {
    token: String,
    #[serde(flatten)]
    req: Request,
}

/// Per-user socket name; `tag` separates profiles (and test instances).
pub fn socket_name(tag: &str) -> String {
    let user = std::env::var("USERNAME").or_else(|_| std::env::var("USER")).unwrap_or_default();
    let user: String = user.chars().filter(|c| c.is_ascii_alphanumeric()).collect();
    format!("brouillon-{user}-{tag}")
}

fn name_for(s: &str) -> io::Result<Name<'_>> {
    if cfg!(windows) {
        s.to_ns_name::<GenericNamespaced>()
    } else {
        // Unix: a socket file in the temp dir (abstract names are Linux-only).
        // The temp dir is per-user on macOS; elsewhere the token protects us.
        let p = std::env::temp_dir().join(format!("{s}.sock"));
        p.to_str()
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "non-UTF-8 temp dir"))?
            .to_string()
            .to_fs_name::<GenericFilePath>()
            .map(Name::into_owned)
    }
}

/// Read `<dir>/ipc.token`, creating it (random, owner-only) if missing.
pub fn load_or_create_token(dir: &Path) -> io::Result<String> {
    let path = dir.join("ipc.token");
    if let Ok(t) = std::fs::read_to_string(&path) {
        if t.trim().len() >= 32 {
            return Ok(t.trim().to_string());
        }
    }
    std::fs::create_dir_all(dir)?;
    let mut rng = rand::rngs::StdRng::from_entropy();
    let token: String = (0..48).map(|_| rng.sample(rand::distributions::Alphanumeric) as char).collect();
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create(true).truncate(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut opts, 0o600);
    opts.open(&path)?.write_all(token.as_bytes())?;
    Ok(token)
}

/// Listening side: yields validated requests.
pub struct Server {
    rx: Receiver<Request>,
    stop: Arc<AtomicBool>,
}

impl Server {
    pub fn start(sock: &str, token: &str) -> io::Result<Self> {
        let name = name_for(sock)?;
        let listener = ListenerOptions::new()
            .name(name)
            .nonblocking(ListenerNonblockingMode::Accept)
            .try_overwrite(true)
            .create_sync()?;
        let (tx, rx) = channel();
        let stop = Arc::new(AtomicBool::new(false));
        let (stop2, token) = (stop.clone(), token.to_string());
        std::thread::spawn(move || {
            while !stop2.load(Ordering::Relaxed) {
                match listener.accept() {
                    Ok(conn) => {
                        let (tx, token) = (tx.clone(), token.clone());
                        std::thread::spawn(move || handle(conn, &token, tx));
                    }
                    Err(e) if e.kind() == io::ErrorKind::WouldBlock => std::thread::sleep(Duration::from_millis(20)),
                    Err(_) => break,
                }
            }
        });
        Ok(Self { rx, stop })
    }

    /// Next forwarded request, if any (call from the UI tick).
    pub fn try_recv(&self) -> Option<Request> {
        self.rx.try_recv().ok()
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

fn handle(conn: Stream, token: &str, tx: Sender<Request>) {
    let mut reader = BufReader::new(conn);
    let mut line = String::new();
    let reply = match reader.read_line(&mut line) {
        Ok(n) if n > 0 && n < 64 * 1024 => match serde_json::from_str::<Envelope>(&line) {
            Ok(env) if ct_eq(env.token.as_bytes(), token.as_bytes()) => {
                let _ = tx.send(env.req);
                Response::Ok
            }
            Ok(_) => Response::Rejected,
            Err(_) => Response::Invalid,
        },
        _ => Response::Invalid,
    };
    let mut conn = reader.into_inner();
    let _ = writeln!(conn, "{}", reply.wire());
}

/// Comparison that does not exit early on the first differing byte.
fn ct_eq(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

/// Send one request to the running instance.
pub fn send(sock: &str, token: &str, req: &Request) -> io::Result<Response> {
    let name = name_for(sock)?;
    let mut conn = Stream::connect(name)?;
    let line = serde_json::to_string(&Envelope { token: token.to_string(), req: req.clone() })
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
    writeln!(conn, "{line}")?;
    conn.flush()?;
    let mut reply = String::new();
    BufReader::new(conn).read_line(&mut reply)?;
    Ok(Response::parse(&reply))
}

pub enum Role {
    /// We are the first instance: poll this for forwarded requests.
    Primary(Server),
    /// Another instance accepted the requests; the caller should exit 0.
    Forwarded,
}

/// Forward `reqs` to a running instance, or become the primary one.
/// An empty `reqs` forwards `Activate`.
pub fn acquire(sock: &str, token: &str, reqs: &[Request]) -> io::Result<Role> {
    let reqs: &[Request] = if reqs.is_empty() { &[Request::Activate] } else { reqs };
    let forward = || -> io::Result<bool> {
        for r in reqs {
            match send(sock, token, r)? {
                Response::Ok => {}
                Response::Rejected => {
                    return Err(io::Error::new(io::ErrorKind::PermissionDenied, "instance rejected our token"))
                }
                Response::Invalid => return Err(io::Error::new(io::ErrorKind::InvalidData, "invalid reply")),
            }
        }
        Ok(true)
    };
    match forward() {
        Ok(_) => return Ok(Role::Forwarded),
        Err(e) if e.kind() == io::ErrorKind::PermissionDenied => return Err(e),
        Err(_) => {} // nobody listening (or stale socket): take over
    }
    match Server::start(sock, token) {
        Ok(s) => Ok(Role::Primary(s)),
        // Lost a start-up race with another instance: forward to it instead.
        Err(e) if e.kind() == io::ErrorKind::AddrInUse => forward().map(|_| Role::Forwarded),
        Err(e) => Err(e),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicU32;
    use std::time::Instant;

    static N: AtomicU32 = AtomicU32::new(0);

    fn unique() -> String {
        socket_name(&format!("test-{}-{}", std::process::id(), N.fetch_add(1, Ordering::Relaxed)))
    }

    fn recv_within(s: &Server, ms: u64) -> Option<Request> {
        let t0 = Instant::now();
        while t0.elapsed() < Duration::from_millis(ms) {
            if let Some(r) = s.try_recv() {
                return Some(r);
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        None
    }

    fn open(path: &str) -> Request {
        Request::Open { path: path.into(), line: Some(12), col: Some(3), readonly: false, new_window: false, wait: false }
    }

    #[test]
    fn second_instance_forwards_path_line_col() {
        let (sock, dir) = (unique(), tempfile::tempdir().unwrap());
        let token = load_or_create_token(dir.path()).unwrap();
        let Role::Primary(server) = acquire(&sock, &token, &[]).unwrap() else { panic!("first must be primary") };
        // A second "process" (same token file) forwards and gets Forwarded.
        let token2 = load_or_create_token(dir.path()).unwrap();
        assert_eq!(token, token2);
        assert!(matches!(acquire(&sock, &token2, &[open("a.rs")]).unwrap(), Role::Forwarded));
        assert_eq!(recv_within(&server, 1000), Some(open("a.rs")));
    }

    #[test]
    fn wrong_token_is_rejected_and_not_delivered() {
        let sock = unique();
        let server = Server::start(&sock, &"x".repeat(48)).unwrap();
        assert_eq!(send(&sock, "wrong", &open("evil.rs")).unwrap(), Response::Rejected);
        assert!(recv_within(&server, 200).is_none());
        let err = acquire(&sock, "wrong", &[open("evil.rs")]).err().unwrap();
        assert_eq!(err.kind(), io::ErrorKind::PermissionDenied);
    }

    #[test]
    fn all_verbs_roundtrip() {
        let sock = unique();
        let tok = "t".repeat(48);
        let server = Server::start(&sock, &tok).unwrap();
        let verbs = [
            Request::Cursor { line: 4, col: 2 },
            Request::Select { start_line: 1, start_col: 1, end_line: 2, end_col: 5 },
            Request::New,
            Request::Activate,
        ];
        for v in &verbs {
            assert_eq!(send(&sock, &tok, v).unwrap(), Response::Ok);
            assert_eq!(recv_within(&server, 1000).as_ref(), Some(v));
        }
    }

    #[test]
    fn garbage_is_invalid() {
        let sock = unique();
        let _server = Server::start(&sock, &"t".repeat(48)).unwrap();
        let mut conn = Stream::connect(name_for(&sock).unwrap()).unwrap();
        writeln!(conn, "not json").unwrap();
        let mut reply = String::new();
        BufReader::new(conn).read_line(&mut reply).unwrap();
        assert_eq!(Response::parse(&reply), Response::Invalid);
    }

    #[test]
    fn token_is_stable_and_long() {
        let dir = tempfile::tempdir().unwrap();
        let a = load_or_create_token(dir.path()).unwrap();
        assert!(a.len() >= 32);
        assert_eq!(a, load_or_create_token(dir.path()).unwrap());
    }
}
