//! SFTP v3 client: wire primitives, packet codec, and a pipelined client over any `Read`/`Write` pair.

pub mod client;
pub mod packet;
pub mod wire;

pub use client::{ClientConfig, SftpClient, WriteStream};
pub use packet::{Attrs, Msg, NameEntry, Reply};

use brouillon_remote_proto::{ErrorKind, RemoteError};

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SftpError {
    #[error("i/o error: {0}")]
    Io(String),
    #[error("connection closed")]
    Eof,
    #[error("protocol violation: {0}")]
    Protocol(String),
    #[error("packet of {0} bytes exceeds the cap")]
    TooLarge(usize),
    #[error("server status {code}: {message}")]
    Status { code: u32, message: String },
    #[error("timed out")]
    Timeout,
    #[error("cancelled")]
    Cancelled,
    /// A cap of ours was exceeded (size lie, endless stream...).
    #[error("limit exceeded: {0}")]
    Limit(String),
}

impl SftpError {
    /// Whether the connection can no longer be used.
    pub fn is_fatal(&self) -> bool {
        matches!(self, SftpError::Io(_) | SftpError::Eof | SftpError::Protocol(_) | SftpError::TooLarge(_))
    }
}

impl From<SftpError> for RemoteError {
    fn from(e: SftpError) -> RemoteError {
        use packet::*;
        let (kind, msg) = match &e {
            SftpError::Status { code, message } => (
                match *code {
                    FX_NO_SUCH_FILE => ErrorKind::NotFound,
                    FX_PERMISSION_DENIED => ErrorKind::PermissionDenied,
                    FX_OP_UNSUPPORTED => ErrorKind::Unsupported,
                    FX_NO_CONNECTION | FX_CONNECTION_LOST => ErrorKind::ConnectionLost,
                    FX_BAD_MESSAGE => ErrorKind::Protocol,
                    FX_EOF => ErrorKind::Failure,
                    _ => ErrorKind::Failure,
                },
                format!("server: {message}"),
            ),
            SftpError::Io(_) | SftpError::Eof => (ErrorKind::ConnectionLost, e.to_string()),
            SftpError::Protocol(_) | SftpError::TooLarge(_) => (ErrorKind::Protocol, e.to_string()),
            SftpError::Timeout => (ErrorKind::Timeout, e.to_string()),
            SftpError::Cancelled => (ErrorKind::Cancelled, e.to_string()),
            SftpError::Limit(_) => (ErrorKind::TooLarge, e.to_string()),
        };
        RemoteError::new(kind, msg)
    }
}
