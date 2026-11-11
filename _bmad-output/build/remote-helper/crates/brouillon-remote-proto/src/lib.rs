//! Wire protocol between the editor and `brouillon-remote.exe` (see docs/REMOTE_HELPER_PROTOCOL.md).
//!
//! * [`frame`]: length-prefixed frames (JSON control + binary data chunks).
//! * [`msg`]: typed requests, results, errors and events.
//! * [`path`]: [`RemotePath`] / [`RemoteName`], server-supplied strings that cannot be used as local paths.
//! * [`limits`]: every cap, with the defaults the helper announces in `hello`.

pub mod frame;
pub mod limits;
pub mod msg;
pub mod path;
pub mod secret;

pub use frame::{read_frame, write_control, write_data, write_frame, Frame, FrameError};
pub use limits::*;
pub use msg::*;
pub use path::{PathError, RemoteName, RemotePath};
pub use secret::Secret;
