//! Caps. The helper announces its effective [`Limits`] in the `hello` result.

use serde::{Deserialize, Serialize};

pub const PROTO_VERSION: u32 = 1;
/// Largest frame body (kind byte included). Larger frames are a protocol error, rejected before allocation.
pub const MAX_FRAME: usize = 1 << 20;
/// Largest payload of one DATA frame.
pub const MAX_CHUNK: usize = 256 * 1024;
pub const MAX_PATH_BYTES: usize = 4096;
pub const MAX_NAME_BYTES: usize = 255;
/// Entries per `list_page` event.
pub const LIST_PAGE: usize = 500;
/// Server-originated text (error messages, banners) is capped to this many bytes.
pub const MAX_ERROR_TEXT: usize = 4096;
/// The editor may have at most this many unacknowledged bytes of write data in flight.
pub const WRITE_WINDOW: u64 = 1 << 20;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Limits {
    pub max_frame: usize,
    pub max_chunk: usize,
    pub max_path_bytes: usize,
    pub max_name_bytes: usize,
    pub max_list_entries: usize,
    pub max_list_bytes: u64,
    pub list_page: usize,
    pub max_in_flight: usize,
    pub max_read_bytes: u64,
    pub max_write_bytes: u64,
    pub write_window: u64,
    /// No byte from the server for this long aborts the operation.
    pub op_idle_timeout_s: u64,
    pub list_total_timeout_s: u64,
    /// Minimum throughput watchdog: fewer than `min_rate_bytes` per `min_rate_window_s` aborts a transfer.
    pub min_rate_bytes: u64,
    pub min_rate_window_s: u64,
}

impl Default for Limits {
    fn default() -> Self {
        Limits {
            max_frame: MAX_FRAME,
            max_chunk: MAX_CHUNK,
            max_path_bytes: MAX_PATH_BYTES,
            max_name_bytes: MAX_NAME_BYTES,
            max_list_entries: 50_000,
            max_list_bytes: 64 << 20,
            list_page: LIST_PAGE,
            max_in_flight: 4,
            max_read_bytes: 100 << 20,
            max_write_bytes: 100 << 20,
            write_window: WRITE_WINDOW,
            op_idle_timeout_s: 20,
            list_total_timeout_s: 30,
            min_rate_bytes: 256 * 20,
            min_rate_window_s: 20,
        }
    }
}
