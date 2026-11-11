//! Signed automatic update (design: internal design notes (not published)).
//!
//! Pure logic and network; no UI. The binary glues it in `brouillon::update_boot`.
//! Order of trust: signature over the exact manifest bytes -> field validation ->
//! policy (no downgrade, no replay) -> installer size + SHA-256 + PE header ->
//! only then is anything executed.

pub mod flow;
pub mod install;
pub mod keys;
pub mod manifest;
pub mod msg;
pub mod net;
pub mod policy;
pub mod state;
pub mod timeutil;

use std::fmt;

/// Hard limits (also documented in the design file).
pub const MAX_MANIFEST_BYTES: u64 = 64 * 1024;
pub const MAX_SIG_BYTES: u64 = 4 * 1024;
pub const MAX_INSTALLER_BYTES: u64 = 200 * 1024 * 1024;
pub const MANIFEST_TIMEOUT_SECS: u64 = 15;
pub const DOWNLOAD_TIMEOUT_SECS: u64 = 600;
pub const USER_AGENT: &str = "Brouillon-update/1";
pub const DEFAULT_MANIFEST_URL: &str = "https://brouillon.rochetta.fr/updates/latest.json";
/// Hosts an installer URL may point to (besides the manifest host itself).
pub const ALLOWED_HOSTS: &[&str] = &["brouillon.rochetta.fr"];

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UpdateError {
    Network(String),
    BadStatus(u16),
    TooLarge,
    RedirectRefused(String),
    InsecureUrl(String),
    SigMalformed,
    UnknownKey(String),
    BadSignature,
    BadManifest(String),
    Replay,
    FutureDated,
    HashMismatch,
    SizeMismatch,
    NotPe,
    BadFileName(String),
    Io(String),
}

impl fmt::Display for UpdateError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        use UpdateError::*;
        match self {
            Network(e) => write!(f, "network error: {e}"),
            BadStatus(s) => write!(f, "unexpected HTTP status {s}"),
            TooLarge => write!(f, "response larger than the allowed limit"),
            RedirectRefused(u) => write!(f, "redirect refused: {u}"),
            InsecureUrl(u) => write!(f, "URL refused (not HTTPS or host not allowed): {u}"),
            SigMalformed => write!(f, "malformed signature file"),
            UnknownKey(k) => write!(f, "signature made by an unknown key ({k})"),
            BadSignature => write!(f, "signature does not verify"),
            BadManifest(e) => write!(f, "invalid manifest: {e}"),
            Replay => write!(f, "manifest older than the last accepted one (replay)"),
            FutureDated => write!(f, "manifest dated in the future"),
            HashMismatch => write!(f, "installer SHA-256 does not match"),
            SizeMismatch => write!(f, "installer size does not match"),
            NotPe => write!(f, "installer is not a Windows executable"),
            BadFileName(n) => write!(f, "unsafe installer file name: {n}"),
            Io(e) => write!(f, "I/O error: {e}"),
        }
    }
}

impl std::error::Error for UpdateError {}

impl From<std::io::Error> for UpdateError {
    fn from(e: std::io::Error) -> Self {
        UpdateError::Io(e.to_string())
    }
}
