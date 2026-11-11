//! Helper main loop. The helper never opens files or sockets: bytes come in as frames and go out as
//! frames. A reader thread parses frames and registers cancel tokens; requests run one at a time.

use brouillon_docs_proto::msg::{self, hello_result};
use brouillon_docs_proto::registry::{self, Status, OPERATIONS};
use brouillon_docs_proto::{read_frame, write_control, ErrorCode, Frame, Limits, Request, PROTO_VERSION};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::io::{Read, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::time::Duration;

pub const HELPER_VERSION: &str = env!("CARGO_PKG_VERSION");

type Tokens = Arc<Mutex<HashMap<u64, Arc<AtomicBool>>>>;

enum Inbound {
    Req(Request, Arc<AtomicBool>),
    /// Unrecoverable framing or JSON error: reported, then the loop ends.
    Fatal(String),
    Eof,
}

/// Serve until `shutdown`, end of input or a fatal protocol error. Returns the process exit code.
pub fn serve<R, W>(input: R, mut out: W) -> i32
where
    R: Read + Send + 'static,
    W: Write,
{
    let tokens: Tokens = Arc::default();
    let (tx, rx) = mpsc::channel();
    let reader_tokens = tokens.clone();
    std::thread::spawn(move || reader(input, tx, reader_tokens));

    let mut greeted = false;
    for item in rx {
        let (req, cancel) = match item {
            Inbound::Req(r, c) => (r, c),
            Inbound::Eof => return 0,
            Inbound::Fatal(m) => {
                let _ = write_control(&mut out, &msg::reply_err(0, ErrorCode::Protocol, m));
                return 2;
            }
        };
        let id = req.id;
        if !greeted && req.method != "hello" {
            let _ = write_control(&mut out, &msg::reply_err(id, ErrorCode::Protocol, "the first request must be hello"));
            return 2;
        }
        let (reply, stop) = dispatch(&req, &cancel, &mut out, &mut greeted);
        tokens.lock().unwrap().remove(&id);
        if write_control(&mut out, &reply).is_err() || stop {
            return 0;
        }
    }
    0
}

fn reader<R: Read>(mut input: R, tx: mpsc::Sender<Inbound>, tokens: Tokens) {
    loop {
        let item = match read_frame(&mut input) {
            Ok(None) => Inbound::Eof,
            Err(e) => Inbound::Fatal(e.to_string()),
            // DATA frames serve document streaming (later waves); with no open document they are ignored.
            Ok(Some(Frame::Data { .. })) => continue,
            Ok(Some(Frame::Control(b))) => match serde_json::from_slice::<Request>(&b) {
                Err(e) => Inbound::Fatal(format!("invalid request: {e}")),
                Ok(req) => {
                    let token = Arc::new(AtomicBool::new(false));
                    {
                        let mut t = tokens.lock().unwrap();
                        if req.method == "cancel" {
                            // Flag the target now: it may be running while this request waits in the queue.
                            if let Some(target) = req.params.get("id").and_then(Value::as_u64) {
                                if let Some(tok) = t.get(&target) {
                                    tok.store(true, Ordering::SeqCst);
                                }
                            }
                        } else {
                            t.insert(req.id, token.clone());
                        }
                    }
                    Inbound::Req(req, token)
                }
            },
        };
        let end = matches!(item, Inbound::Eof | Inbound::Fatal(_));
        if tx.send(item).is_err() || end {
            return;
        }
    }
}

fn dispatch<W: Write>(req: &Request, cancel: &AtomicBool, out: &mut W, greeted: &mut bool) -> (Value, bool) {
    let id = req.id;
    let Some(op) = registry::find(&req.method) else {
        return (msg::reply_err(id, ErrorCode::UnknownMethod, format!("unknown method {}", req.method)), false);
    };
    if op.status == Status::Planned {
        return (msg::reply_err(id, ErrorCode::Unsupported, format!("{} is not available in this helper build", op.name)), false);
    }
    let r = match op.name {
        "hello" => {
            let proto = req.params.get("proto").and_then(Value::as_u64);
            if proto != Some(PROTO_VERSION as u64) {
                return (msg::reply_err(id, ErrorCode::BadRequest, format!("unsupported proto, expected {PROTO_VERSION}")), false);
            }
            *greeted = true;
            msg::reply_ok(id, hello_result(HELPER_VERSION, &[], &Limits::default()))
        }
        "ping" | "cancel" => msg::reply_ok(id, json!({})),
        "operations" => msg::reply_ok(id, serde_json::to_value(OPERATIONS).unwrap_or(Value::Null)),
        "shutdown" => return (msg::reply_ok(id, json!({})), true),
        "debug.spin" => spin(req, cancel, out),
        _ => msg::reply_err(id, ErrorCode::Internal, "operation has no handler"),
    };
    (r, false)
}

fn spin<W: Write>(req: &Request, cancel: &AtomicBool, out: &mut W) -> Value {
    let id = req.id;
    let Some(steps) = req.params.get("steps").and_then(Value::as_u64) else {
        return msg::reply_err(id, ErrorCode::BadRequest, "steps is required");
    };
    let step_ms = req.params.get("step_ms").and_then(Value::as_u64).unwrap_or(1).min(1000);
    for done in 0..steps.min(1_000_000) {
        if cancel.load(Ordering::SeqCst) {
            return msg::reply_err(id, ErrorCode::Cancelled, "cancelled");
        }
        std::thread::sleep(Duration::from_millis(step_ms));
        let _ = write_control(out, &msg::event_progress(id, done + 1, steps));
    }
    msg::reply_ok(id, json!({ "steps": steps }))
}
