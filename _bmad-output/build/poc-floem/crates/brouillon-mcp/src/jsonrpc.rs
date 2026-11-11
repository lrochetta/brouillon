//! JSON-RPC 2.0 framing helpers (newline-delimited, one message per line).

use std::io::{self, BufRead};

use serde_json::{json, Value};

pub const PARSE_ERROR: i64 = -32700;
pub const INVALID_REQUEST: i64 = -32600;
pub const METHOD_NOT_FOUND: i64 = -32601;
pub const INVALID_PARAMS: i64 = -32602;
pub const INTERNAL_ERROR: i64 = -32603;
/// Editor unreachable / server stopped (shared with the relay).
pub const UNAVAILABLE: i64 = -32001;

/// Hard cap on one message (spec: 1 MiB).
pub const MAX_MESSAGE_BYTES: usize = 1024 * 1024;

pub fn error_response(id: Value, code: i64, message: &str) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": message}})
}

pub fn result_response(id: Value, result: Value) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "result": result})
}

pub enum Line {
    Eof,
    /// A complete line (without the newline).
    Text(String),
    /// The line exceeded [`MAX_MESSAGE_BYTES`]; the rest of it was discarded.
    TooLong,
}

/// Read one newline-terminated line without ever buffering more than the cap.
pub fn read_line_bounded<R: BufRead>(r: &mut R) -> io::Result<Line> {
    let mut buf: Vec<u8> = Vec::new();
    let mut too_long = false;
    loop {
        let (done, used) = {
            let chunk = match r.fill_buf() {
                Ok(c) => c,
                Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                Err(e) => return Err(e),
            };
            if chunk.is_empty() {
                if buf.is_empty() && !too_long {
                    return Ok(Line::Eof);
                }
                (true, 0)
            } else if let Some(i) = chunk.iter().position(|&b| b == b'\n') {
                if !too_long {
                    buf.extend_from_slice(&chunk[..i]);
                }
                (true, i + 1)
            } else {
                if !too_long {
                    buf.extend_from_slice(chunk);
                }
                (false, chunk.len())
            }
        };
        r.consume(used);
        if buf.len() > MAX_MESSAGE_BYTES {
            too_long = true;
            buf.clear();
        }
        if done {
            break;
        }
    }
    if too_long {
        return Ok(Line::TooLong);
    }
    if buf.last() == Some(&b'\r') {
        buf.pop();
    }
    Ok(Line::Text(String::from_utf8_lossy(&buf).into_owned()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn lines_and_eof() {
        let mut c = Cursor::new(b"a\r\nb\nlast".to_vec());
        assert!(matches!(read_line_bounded(&mut c).unwrap(), Line::Text(t) if t == "a"));
        assert!(matches!(read_line_bounded(&mut c).unwrap(), Line::Text(t) if t == "b"));
        assert!(matches!(read_line_bounded(&mut c).unwrap(), Line::Text(t) if t == "last"));
        assert!(matches!(read_line_bounded(&mut c).unwrap(), Line::Eof));
    }

    #[test]
    fn oversize_line_is_discarded_and_next_line_survives() {
        let mut data = vec![b'x'; MAX_MESSAGE_BYTES + 10];
        data.extend_from_slice(b"\nok\n");
        let mut c = Cursor::new(data);
        assert!(matches!(read_line_bounded(&mut c).unwrap(), Line::TooLong));
        assert!(matches!(read_line_bounded(&mut c).unwrap(), Line::Text(t) if t == "ok"));
    }
}
