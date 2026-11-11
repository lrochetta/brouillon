//! Control messages (JSON inside CONTROL frames).
//!
//! Request `{id, method, params}`; reply `{id, result}` or `{id, error}`; event `{event, id?, ...}`.
//! The first request of a session is `hello`. `cancel` is `{method:"cancel", params:{id}}` and is
//! answered itself; the cancelled request then ends with the `cancelled` error.

use crate::limits::{Limits, PROTO_VERSION};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Request {
    pub id: u64,
    pub method: String,
    #[serde(default)]
    pub params: Value,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCode {
    Protocol,
    BadRequest,
    UnknownMethod,
    /// Known operation whose implementation is not in this helper build.
    Unsupported,
    Busy,
    Cancelled,
    Encrypted,
    Corrupt,
    TooComplex,
    UnsupportedFeature,
    StaleRevision,
    Internal,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ErrorBody {
    pub code: ErrorCode,
    /// Plain text, never document text.
    pub message: String,
}

pub fn reply_ok(id: u64, result: Value) -> Value {
    json!({ "id": id, "result": result })
}

pub fn reply_err(id: u64, code: ErrorCode, message: impl Into<String>) -> Value {
    json!({ "id": id, "error": ErrorBody { code, message: message.into() } })
}

pub fn event_progress(id: u64, done: u64, total: u64) -> Value {
    json!({ "event": "progress", "id": id, "done": done, "total": total })
}

pub fn hello_result(helper_version: &str, features: &[&str], limits: &Limits) -> Value {
    json!({
        "proto": PROTO_VERSION,
        "helper_version": helper_version,
        "caps": ["progress", "cancel", "operations"],
        "limits": limits,
        "features": features,
    })
}
