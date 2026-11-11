//! FTP / FTPS transport of the helper: a small FTP client written on `std` + `rustls` (ring), exposing
//! the same operations as the SFTP [`crate::ops::Session`] behind the same stdio protocol.
//!
//! Why not `suppaftp`: see docs/REMOTE_FTP.md ("Decision"). In short, it is `TcpStream`-only (no
//! in-process test server), reads replies and `LIST` output without bounds, and pulls `regex`, `chrono`
//! and `lazy-regex` for ~300 lines of parsing that we need to be strict anyway.
//!
//! Security properties (each one is covered by a test in `tests/ftp_*.rs` or a unit test here):
//! * TLS cannot be downgraded: explicit FTPS needs `234` to `AUTH TLS` and fails closed otherwise; bytes
//!   buffered before the upgrade abort it; `PBSZ 0` / `PROT P` are mandatory.
//! * Certificate: OS roots + host name, or a user-confirmed SHA-256 pin; a changed pin is a hard stop;
//!   there is no option to skip validation; the password is only sent after the handshake succeeded.
//! * Passive mode only; the address in `227`/`229` is ignored, data always goes to the control peer.
//! * Every server-controlled quantity is bounded: reply line/lines/bytes, listing bytes/entries/time,
//!   download size, names (via `RemoteName`).
//! * Plain FTP exists only behind a per-site opt-in and is flagged `insecure` for the whole session.

pub mod connect;
pub mod ctrl;
pub mod parse;
pub mod session;
pub mod tls;

pub use connect::connect;
pub use ctrl::Stream;
pub use session::FtpSession;

use std::io;
use std::net::{IpAddr, TcpStream, ToSocketAddrs};
use std::sync::Arc;
use std::time::Duration;

/// Closes a connection from another thread (unblocks a pending read). Idempotent.
pub type Closer = Arc<dyn Fn() + Send + Sync>;

pub struct Dialed {
    pub stream: Box<dyn Stream>,
    /// Address of the peer. Data connections are opened to THIS address only.
    pub peer_ip: IpAddr,
    pub closer: Closer,
}

/// How the helper reaches the network. The real implementation is [`TcpDialer`]; tests inject an
/// in-memory one so no socket is opened.
pub trait Dialer: Send + Sync {
    fn control(&self, host: &str, port: u16, connect_timeout: Duration, idle: Duration) -> io::Result<Dialed>;
    fn data(&self, ip: IpAddr, port: u16, connect_timeout: Duration, idle: Duration) -> io::Result<Dialed>;
}

pub struct TcpDialer;

fn finish(s: TcpStream, idle: Duration) -> io::Result<Dialed> {
    s.set_read_timeout(Some(idle))?;
    s.set_write_timeout(Some(idle))?;
    let _ = s.set_nodelay(true);
    let peer_ip = s.peer_addr()?.ip();
    let c = s.try_clone()?;
    Ok(Dialed { stream: Box::new(s), peer_ip, closer: Arc::new(move || drop(c.shutdown(std::net::Shutdown::Both))) })
}

impl Dialer for TcpDialer {
    fn control(&self, host: &str, port: u16, connect_timeout: Duration, idle: Duration) -> io::Result<Dialed> {
        let mut last = io::Error::new(io::ErrorKind::NotFound, "the host name did not resolve");
        for addr in (host, port).to_socket_addrs()? {
            match TcpStream::connect_timeout(&addr, connect_timeout) {
                Ok(s) => return finish(s, idle),
                Err(e) => last = e,
            }
        }
        Err(last)
    }
    fn data(&self, ip: IpAddr, port: u16, connect_timeout: Duration, idle: Duration) -> io::Result<Dialed> {
        finish(TcpStream::connect_timeout(&std::net::SocketAddr::new(ip, port), connect_timeout)?, idle)
    }
}

/// Environment of an FTP connection (injectable for tests).
#[derive(Clone)]
pub struct FtpEnv {
    pub dialer: Arc<dyn Dialer>,
    /// Trust anchors. `None` = the operating system store.
    pub roots: Option<Arc<rustls::RootCertStore>>,
}

impl Default for FtpEnv {
    fn default() -> Self {
        FtpEnv { dialer: Arc::new(TcpDialer), roots: None }
    }
}
