//! `brouillon-docs/1`: framing, messages, limits and the operation registry of the documents helper.
//! No I/O beyond `Read`/`Write` traits, no network.

pub mod frame;
pub mod limits;
pub mod msg;
pub mod registry;

pub use frame::{read_frame, write_control, write_data, write_frame, Frame, FrameError};
pub use limits::{Limits, PROTO_VERSION};
pub use msg::{ErrorCode, Request};
