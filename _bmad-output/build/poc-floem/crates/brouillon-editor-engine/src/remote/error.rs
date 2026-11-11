//! Plain-language remote errors (spec 02 section 9.3): the raw cause is kept for "Details", the
//! user sees one sentence a non-expert can act on.

use std::io;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum RemoteErrorKind {
    Unreachable,
    AuthFailed,
    PermissionDenied,
    NotFound,
    AlreadyExists,
    DiskFull,
    HostKeyChanged,
    /// The user (or a strict policy) refused an unknown server identity.
    HostKeyRejected,
    /// `brouillon-remote.exe` is not next to the editor.
    HelperMissing,
    /// The OpenSSH client is not installed.
    NoSsh,
    Dropped,
    TooLarge,
    Unsupported,
    Cancelled,
    Other,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RemoteError {
    pub kind: RemoteErrorKind,
    /// Raw technical cause, for the "Details" view only. Never shown as the main message.
    pub detail: String,
}

impl RemoteError {
    pub fn new(kind: RemoteErrorKind, detail: impl Into<String>) -> Self {
        Self { kind, detail: detail.into() }
    }

    pub fn cancelled() -> Self {
        Self::new(RemoteErrorKind::Cancelled, "cancelled")
    }

    /// Map an `io::Error` (what the blocking provider returns) to a bucket.
    pub fn from_io(e: &io::Error) -> Self {
        let msg = e.to_string();
        let low = msg.to_lowercase();
        let kind = if low.contains("host key") || low.contains("identity changed") {
            RemoteErrorKind::HostKeyChanged
        } else if low.contains("auth") || low.contains("permission denied (publickey") || low.contains("sign-in") {
            RemoteErrorKind::AuthFailed
        } else if low.contains("no space") || low.contains("disk full") || low.contains("quota") {
            RemoteErrorKind::DiskFull
        } else {
            match e.kind() {
                io::ErrorKind::ConnectionRefused | io::ErrorKind::TimedOut | io::ErrorKind::NotConnected | io::ErrorKind::AddrNotAvailable => RemoteErrorKind::Unreachable,
                io::ErrorKind::ConnectionReset | io::ErrorKind::ConnectionAborted | io::ErrorKind::BrokenPipe | io::ErrorKind::UnexpectedEof => RemoteErrorKind::Dropped,
                io::ErrorKind::PermissionDenied => RemoteErrorKind::PermissionDenied,
                io::ErrorKind::NotFound => RemoteErrorKind::NotFound,
                io::ErrorKind::AlreadyExists => RemoteErrorKind::AlreadyExists,
                io::ErrorKind::Unsupported => RemoteErrorKind::Unsupported,
                io::ErrorKind::FileTooLarge => RemoteErrorKind::TooLarge,
                _ => RemoteErrorKind::Other,
            }
        };
        Self::new(kind, msg)
    }

    /// The sentence shown to the user.
    pub fn plain(&self) -> String {
        match self.kind {
            RemoteErrorKind::Unreachable => "Can't reach the server. Check the address and that the server is running.".into(),
            RemoteErrorKind::AuthFailed => "The server refused your sign-in. Check the user name and key or password.".into(),
            RemoteErrorKind::PermissionDenied => "You don't have permission to do that here.".into(),
            RemoteErrorKind::NotFound => "That file or folder no longer exists on the server.".into(),
            RemoteErrorKind::AlreadyExists => "Something with that name already exists here.".into(),
            RemoteErrorKind::DiskFull => "The server has no space left.".into(),
            RemoteErrorKind::HostKeyChanged => "The server identity changed. Nothing was sent.".into(),
            RemoteErrorKind::HostKeyRejected => "The server identity was not trusted. Nothing was sent.".into(),
            RemoteErrorKind::HelperMissing => "Brouillon's remote helper (brouillon-remote.exe) was not found next to the editor. Reinstall Brouillon to get it back.".into(),
            RemoteErrorKind::NoSsh => "The OpenSSH client was not found on this computer. Install it (Windows: Settings > Apps > Optional features > OpenSSH Client).".into(),
            RemoteErrorKind::Dropped => "The connection dropped. Try again.".into(),
            RemoteErrorKind::TooLarge => "That file is too large to edit remotely.".into(),
            RemoteErrorKind::Unsupported => "The server does not support that action.".into(),
            RemoteErrorKind::Cancelled => "Cancelled.".into(),
            RemoteErrorKind::Other => "The server reported a problem. See details.".into(),
        }
    }
}

impl std::fmt::Display for RemoteError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.plain())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn io_errors_land_in_plain_buckets() {
        let k = |kind| RemoteError::from_io(&io::Error::new(kind, "x")).kind;
        assert_eq!(k(io::ErrorKind::ConnectionRefused), RemoteErrorKind::Unreachable);
        assert_eq!(k(io::ErrorKind::TimedOut), RemoteErrorKind::Unreachable);
        assert_eq!(k(io::ErrorKind::PermissionDenied), RemoteErrorKind::PermissionDenied);
        assert_eq!(k(io::ErrorKind::NotFound), RemoteErrorKind::NotFound);
        assert_eq!(k(io::ErrorKind::BrokenPipe), RemoteErrorKind::Dropped);
        let disk = RemoteError::from_io(&io::Error::other("write failed: No space left on device"));
        assert_eq!(disk.kind, RemoteErrorKind::DiskFull);
        let auth = RemoteError::from_io(&io::Error::other("Authentication failed"));
        assert_eq!(auth.kind, RemoteErrorKind::AuthFailed);
    }

    #[test]
    fn the_plain_message_never_leaks_the_raw_detail() {
        let e = RemoteError::from_io(&io::Error::other("ssh_exchange_identification: 10.0.0.7 reset"));
        assert!(!e.plain().contains("10.0.0.7"));
        assert!(e.detail.contains("10.0.0.7"));
    }
}
