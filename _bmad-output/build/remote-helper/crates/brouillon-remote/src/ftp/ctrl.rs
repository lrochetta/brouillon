//! The FTP control connection: bounded line reader, reply parser, command writer.
//!
//! Every read is bounded (line length, number of lines, total bytes) and every byte read is timed
//! out by the stream itself (the dialer sets the idle timeout). A command can never carry CR, LF or
//! NUL. Nothing here logs; errors never contain the command text (PASS must not leak).

use brouillon_remote_proto::{sanitize_text, ErrorKind, RemoteError};
use std::io::{Read, Write};

/// Any byte stream the FTP client can run over: a TCP socket, a TLS stream, or an in-memory pipe in tests.
pub trait Stream: Read + Write + Send {}
impl<T: Read + Write + Send + ?Sized> Stream for T {}

pub const MAX_LINE: usize = 4096;
pub const MAX_REPLY_LINES: usize = 512;
pub const MAX_REPLY_BYTES: usize = 64 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Reply {
    pub code: u16,
    /// Text of every line without the `NNN-` / `NNN ` prefix, sanitised (no control characters).
    pub lines: Vec<String>,
}

impl Reply {
    pub fn text(&self) -> String {
        sanitize_text(&self.lines.join(" | "))
    }
    pub fn first(&self) -> &str {
        self.lines.first().map(String::as_str).unwrap_or("")
    }
    pub fn class(&self) -> u16 {
        self.code / 100
    }
    pub fn ok(&self) -> bool {
        self.class() == 2
    }
}

fn io_err(e: &std::io::Error) -> RemoteError {
    use std::io::ErrorKind as K;
    match e.kind() {
        K::TimedOut | K::WouldBlock => RemoteError::new(ErrorKind::Timeout, "the FTP server stopped answering"),
        _ => RemoteError::new(ErrorKind::ConnectionLost, format!("FTP connection error: {e}")),
    }
}

pub fn map_io(e: &std::io::Error) -> RemoteError {
    io_err(e)
}

/// Read one line (terminated by LF, optional CR) of at most `MAX_LINE` bytes from `buf`/`io`.
pub fn read_line(io: &mut dyn Stream, buf: &mut Vec<u8>) -> Result<Vec<u8>, RemoteError> {
    loop {
        if let Some(i) = buf.iter().position(|b| *b == b'\n') {
            if i > MAX_LINE {
                return Err(RemoteError::new(ErrorKind::Protocol, "FTP reply line too long"));
            }
            let mut line: Vec<u8> = buf.drain(..=i).collect();
            line.pop();
            if line.last() == Some(&b'\r') {
                line.pop();
            }
            return Ok(line);
        }
        if buf.len() > MAX_LINE {
            return Err(RemoteError::new(ErrorKind::Protocol, "FTP reply line too long"));
        }
        let mut chunk = [0u8; 2048];
        match io.read(&mut chunk) {
            Ok(0) => return Err(RemoteError::new(ErrorKind::ConnectionLost, "the FTP server closed the connection")),
            Ok(n) => buf.extend_from_slice(&chunk[..n]),
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
            Err(e) => return Err(io_err(&e)),
        }
    }
}

pub struct Ctrl {
    io: Box<dyn Stream>,
    rbuf: Vec<u8>,
}

struct Dead;
impl Read for Dead {
    fn read(&mut self, _: &mut [u8]) -> std::io::Result<usize> {
        Err(std::io::ErrorKind::NotConnected.into())
    }
}
impl Write for Dead {
    fn write(&mut self, _: &[u8]) -> std::io::Result<usize> {
        Err(std::io::ErrorKind::NotConnected.into())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl Ctrl {
    pub fn new(io: Box<dyn Stream>) -> Ctrl {
        Ctrl { io, rbuf: Vec::new() }
    }

    /// Hand the raw stream out for a TLS upgrade. Fails closed when the server already sent more
    /// bytes than the reply we consumed (a plaintext injection attempt that would otherwise be
    /// parsed as if it came from the TLS channel).
    pub fn take_for_tls(&mut self) -> Result<Box<dyn Stream>, RemoteError> {
        if !self.rbuf.is_empty() {
            return Err(RemoteError::new(ErrorKind::Protocol, "the server sent extra data before the TLS handshake; refusing to upgrade (possible injection)"));
        }
        Ok(std::mem::replace(&mut self.io, Box::new(Dead)))
    }

    pub fn put_stream(&mut self, io: Box<dyn Stream>) {
        self.io = io;
        self.rbuf.clear();
    }

    pub fn send(&mut self, cmd: &str) -> Result<(), RemoteError> {
        if cmd.bytes().any(|b| b == b'\r' || b == b'\n' || b == 0) || cmd.len() > MAX_LINE {
            return Err(RemoteError::new(ErrorKind::InvalidArgument, "command contains CR, LF or NUL"));
        }
        let mut line = Vec::with_capacity(cmd.len() + 2);
        line.extend_from_slice(cmd.as_bytes());
        line.extend_from_slice(b"\r\n");
        self.io.write_all(&line).and_then(|_| self.io.flush()).map_err(|e| io_err(&e))
    }

    pub fn reply(&mut self) -> Result<Reply, RemoteError> {
        let mut lines: Vec<String> = Vec::new();
        let mut total = 0usize;
        let mut code: Option<u16> = None;
        loop {
            let raw = read_line(&mut *self.io, &mut self.rbuf)?;
            total += raw.len();
            if total > MAX_REPLY_BYTES || lines.len() >= MAX_REPLY_LINES {
                return Err(RemoteError::new(ErrorKind::Protocol, "FTP reply too large"));
            }
            let text = String::from_utf8_lossy(&raw).into_owned();
            let b = text.as_bytes();
            let has_code = b.len() >= 3 && b[..3].iter().all(|c| c.is_ascii_digit());
            match code {
                None => {
                    if !has_code || (b.len() > 3 && b[3] != b' ' && b[3] != b'-') {
                        return Err(RemoteError::new(ErrorKind::Protocol, "malformed FTP reply"));
                    }
                    let c: u16 = text[..3].parse().unwrap_or(0);
                    if !(100..600).contains(&c) {
                        return Err(RemoteError::new(ErrorKind::Protocol, "FTP reply code out of range"));
                    }
                    code = Some(c);
                    let multi = b.len() > 3 && b[3] == b'-';
                    lines.push(sanitize_text(text.get(4..).unwrap_or("")));
                    if !multi {
                        break;
                    }
                }
                Some(c) => {
                    if has_code && text[..3].parse::<u16>().ok() == Some(c) && (b.len() == 3 || b[3] == b' ') {
                        lines.push(sanitize_text(text.get(4..).unwrap_or("")));
                        break;
                    }
                    // continuation line: keep it verbatim minus control characters; MLST facts rely on the leading blank
                    lines.push(sanitize_text(&text));
                }
            }
        }
        Ok(Reply { code: code.unwrap_or(0), lines })
    }

    pub fn cmd(&mut self, cmd: &str) -> Result<Reply, RemoteError> {
        self.send(cmd)?;
        self.reply()
    }

    /// Like `cmd` for secrets: the (possibly secret) command is never part of any error.
    pub fn cmd_secret(&mut self, cmd: &str) -> Result<Reply, RemoteError> {
        self.cmd(cmd)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    struct Rw(Cursor<Vec<u8>>, Vec<u8>);
    impl Read for Rw {
        fn read(&mut self, b: &mut [u8]) -> std::io::Result<usize> {
            self.0.read(b)
        }
    }
    impl Write for Rw {
        fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
            self.1.extend_from_slice(b);
            Ok(b.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    fn ctrl(s: &[u8]) -> Ctrl {
        Ctrl::new(Box::new(Rw(Cursor::new(s.to_vec()), vec![])))
    }

    #[test]
    fn single_and_multiline() {
        let r = ctrl(b"220 hello\r\n").reply().unwrap();
        assert_eq!((r.code, r.first()), (220, "hello"));
        let r = ctrl(b"211-Features:\r\n MLST type*;\r\n UTF8\r\n211 End\r\n").reply().unwrap();
        assert_eq!(r.code, 211);
        assert_eq!(r.lines, ["Features:", " MLST type*;", " UTF8", "End"]);
        // a continuation line that starts with other digits does not end the reply
        let r = ctrl(b"214-help\r\n999 not the end\r\n214 done\r\n").reply().unwrap();
        assert_eq!(r.lines.len(), 3);
    }

    #[test]
    fn malformed_and_oversized_replies_fail() {
        for bad in [&b"hello\r\n"[..], b"22 x\r\n", b"2200 x\r\n", b"999 x\r\n", b"000 x\r\n", b"220x y\r\n"] {
            assert_eq!(ctrl(bad).reply().unwrap_err().kind, ErrorKind::Protocol, "{bad:?}");
        }
        let mut long = b"220 ".to_vec();
        long.extend(std::iter::repeat(b'a').take(MAX_LINE + 10));
        long.extend_from_slice(b"\r\n");
        assert_eq!(ctrl(&long).reply().unwrap_err().kind, ErrorKind::Protocol);
        // endless line without newline
        let endless = vec![b'a'; 100_000];
        assert_eq!(ctrl(&endless).reply().unwrap_err().kind, ErrorKind::Protocol);
        // endless multiline reply
        let mut m = Vec::new();
        for _ in 0..(MAX_REPLY_LINES + 5) {
            m.extend_from_slice(b"211-x\r\n");
        }
        assert_eq!(ctrl(&m).reply().unwrap_err().kind, ErrorKind::Protocol);
        // closed early
        assert_eq!(ctrl(b"220-never ends\r\n").reply().unwrap_err().kind, ErrorKind::ConnectionLost);
    }

    #[test]
    fn control_characters_in_replies_are_neutralised() {
        let r = ctrl(b"550 bad\x1b[2J\x00name\r\n").reply().unwrap();
        assert!(!r.first().chars().any(|c| c.is_control()));
    }

    #[test]
    fn commands_cannot_be_injected() {
        let mut c = ctrl(b"");
        assert_eq!(c.send("RETR a\r\nDELE b").unwrap_err().kind, ErrorKind::InvalidArgument);
        assert_eq!(c.send("RETR a\0").unwrap_err().kind, ErrorKind::InvalidArgument);
        assert!(c.send("NOOP").is_ok());
    }

    #[test]
    fn tls_upgrade_refused_when_extra_bytes_were_buffered() {
        let mut c = ctrl(b"234 ok\r\n220 injected\r\n");
        assert_eq!(c.reply().unwrap().code, 234);
        assert_eq!(c.take_for_tls().err().unwrap().kind, ErrorKind::Protocol);
        let mut c = ctrl(b"234 ok\r\n");
        c.reply().unwrap();
        assert!(c.take_for_tls().is_ok());
    }
}
