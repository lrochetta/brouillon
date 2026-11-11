//! The stdio relay (`brouillon-mcp` binary): pure line relay between an MCP client on
//! stdin/stdout and the running editor over the Link socket.
//!
//! It never parses MCP traffic. The only JSON it ever touches is its own handshake and, on
//! failure, the single JSON-RPC error it must print so the client sees a clean message.
//! stdout carries nothing but JSON-RPC lines.

use std::io::{BufRead, BufReader, Read, Write};
use std::sync::mpsc::channel;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::{json, Value};

use crate::jsonrpc::{error_response, read_line_bounded, Line, INVALID_REQUEST, UNAVAILABLE};
use crate::link::{client_handshake, ClientHello};

/// How long to keep draining editor replies after the client closed stdin.
pub const DRAIN_GRACE: Duration = Duration::from_millis(1500);

type SharedOut = Arc<Mutex<Box<dyn Write + Send>>>;

fn emit(out: &SharedOut, v: &Value) {
    if let Ok(mut o) = out.lock() {
        let _ = writeln!(o, "{v}");
        let _ = o.flush();
    }
}

/// Print one JSON-RPC error for the first request on `input`, then return exit code 1.
/// Used when the editor cannot be reached.
pub fn fail_with_error<R: BufRead, W: Write>(mut input: R, mut output: W, message: &str) -> i32 {
    let id = match read_line_bounded(&mut input) {
        Ok(Line::Text(t)) => serde_json::from_str::<Value>(&t).ok().and_then(|v| v.get("id").cloned()).unwrap_or(Value::Null),
        _ => Value::Null,
    };
    let _ = writeln!(output, "{}", error_response(id, UNAVAILABLE, message));
    let _ = output.flush();
    1
}

/// Handshake with the editor, then relay until stdin closes. Returns the process exit code.
pub fn run<R, W, CR, CW>(input: R, output: W, conn_r: CR, mut conn_w: CW, token: &str, client: &ClientHello) -> i32
where
    R: BufRead + Send + 'static,
    W: Write + Send + 'static,
    CR: Read + Send + 'static,
    CW: Write + Send + 'static,
{
    let mut cr = BufReader::new(conn_r);
    let mut input = input;
    let out: SharedOut = Arc::new(Mutex::new(Box::new(output)));
    let reply = match client_handshake(&mut cr, &mut conn_w, token, client) {
        Ok(r) => r,
        Err(e) => {
            let msg = format!("Brouillon did not answer the handshake: {e}");
            return fail_via(&mut input, &out, &msg);
        }
    };
    if !reply.ok {
        let why = match reply.error.as_deref() {
            Some("bad_token") => "Brouillon rejected this relay (token mismatch). Restart Brouillon.",
            Some("killed") => "Brouillon Link was stopped by the user.",
            Some("disabled") => "Brouillon Link is disabled. Enable it in Settings > Agents.",
            Some("version") => "This relay and Brouillon speak different Link versions. Update both.",
            _ => "Brouillon refused the connection.",
        };
        return fail_via(&mut input, &out, why);
    }

    // editor -> client
    let (done_tx, done_rx) = channel::<()>();
    let out2 = out.clone();
    std::thread::spawn(move || {
        loop {
            match read_line_bounded(&mut cr) {
                Ok(Line::Text(t)) => {
                    if let Ok(mut o) = out2.lock() {
                        if writeln!(o, "{t}").and_then(|_| o.flush()).is_err() {
                            break;
                        }
                    }
                }
                Ok(Line::TooLong) => continue,
                Ok(Line::Eof) | Err(_) => break,
            }
        }
        let _ = done_tx.send(());
    });

    // client -> editor
    let mut editor_gone = false;
    loop {
        match read_line_bounded(&mut input) {
            Ok(Line::Text(t)) => {
                if t.trim().is_empty() {
                    continue;
                }
                if writeln!(conn_w, "{t}").and_then(|_| conn_w.flush()).is_err() {
                    emit(&out, &error_response(Value::Null, UNAVAILABLE, "Brouillon closed the connection."));
                    editor_gone = true;
                    break;
                }
            }
            Ok(Line::TooLong) => emit(&out, &error_response(Value::Null, INVALID_REQUEST, "Message too large")),
            Ok(Line::Eof) | Err(_) => break,
        }
    }
    if editor_gone {
        return 1;
    }
    drop(conn_w);
    // Let in-flight replies reach stdout before exiting.
    let _ = done_rx.recv_timeout(DRAIN_GRACE);
    0
}

fn fail_via<R: BufRead>(input: &mut R, out: &SharedOut, message: &str) -> i32 {
    let id = match read_line_bounded(input) {
        Ok(Line::Text(t)) => serde_json::from_str::<Value>(&t).ok().and_then(|v| v.get("id").cloned()).unwrap_or(Value::Null),
        _ => Value::Null,
    };
    emit(out, &json!({"jsonrpc": "2.0", "id": id, "error": {"code": UNAVAILABLE, "message": message}}));
    1
}

/// An in-memory duplex pipe for loopback tests: `(a_reader, a_writer), (b_reader, b_writer)` where
/// what `a_writer` writes is read by `b_reader` and vice versa. Dropping a writer is EOF for the peer.
pub mod loopback {
    use std::io::{self, Read, Write};
    use std::sync::mpsc::{channel, Receiver, Sender};

    pub struct ChanReader {
        rx: Receiver<Vec<u8>>,
        buf: Vec<u8>,
        pos: usize,
    }
    pub struct ChanWriter(Sender<Vec<u8>>);

    pub fn pipe() -> (ChanReader, ChanWriter) {
        let (tx, rx) = channel();
        (ChanReader { rx, buf: vec![], pos: 0 }, ChanWriter(tx))
    }

    impl Read for ChanReader {
        fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
            while self.pos >= self.buf.len() {
                match self.rx.recv() {
                    Ok(b) => {
                        self.buf = b;
                        self.pos = 0;
                    }
                    Err(_) => return Ok(0),
                }
            }
            let n = (self.buf.len() - self.pos).min(out.len());
            out[..n].copy_from_slice(&self.buf[self.pos..self.pos + n]);
            self.pos += n;
            Ok(n)
        }
    }

    impl Write for ChanWriter {
        fn write(&mut self, b: &[u8]) -> io::Result<usize> {
            self.0.send(b.to_vec()).map_err(|_| io::Error::new(io::ErrorKind::BrokenPipe, "peer gone"))?;
            Ok(b.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    /// Cloneable in-memory sink that tests can read back.
    #[derive(Clone, Default)]
    pub struct Sink(pub std::sync::Arc<std::sync::Mutex<Vec<u8>>>);
    impl Write for Sink {
        fn write(&mut self, b: &[u8]) -> io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(b);
            Ok(b.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }
    impl Sink {
        pub fn text(&self) -> String {
            String::from_utf8_lossy(&self.0.lock().unwrap()).into_owned()
        }
    }
}
