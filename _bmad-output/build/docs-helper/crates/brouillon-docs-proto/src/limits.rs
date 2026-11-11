//! Caps announced in the `hello` result.

use serde::{Deserialize, Serialize};

/// Protocol name and major version: `brouillon-docs/1`.
pub const PROTO_VERSION: u32 = 1;
/// Largest frame body (kind byte included); larger is a protocol error, rejected before allocation.
pub const MAX_FRAME: usize = 1 << 20;
/// Largest payload of one DATA frame.
pub const MAX_CHUNK: usize = 256 * 1024;
/// Requests in flight before `busy`.
pub const MAX_IN_FLIGHT: usize = 6;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Limits {
    pub max_frame: usize,
    pub max_chunk: usize,
    pub max_in_flight: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Self { max_frame: MAX_FRAME, max_chunk: MAX_CHUNK, max_in_flight: MAX_IN_FLIGHT }
    }
}
