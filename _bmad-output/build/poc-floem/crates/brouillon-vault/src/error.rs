use std::fmt;

/// Errors never carry secret material, labels or paths: only fixed text and numeric counters.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VaultError {
    /// The vault is locked (or `lock()` was called).
    Locked,
    /// No such secret.
    NotFound,
    /// Reserved for the passphrase slot.
    WrongPassphrase,
    /// Structurally invalid file (bad magic, footer, lengths).
    Corrupt,
    /// Authentication failed: the file was modified, or built for another vault/key.
    Tampered,
    /// File format newer than this build.
    UnsupportedVersion,
    /// Unknown AEAD / KDF id.
    UnsupportedAlgorithm,
    /// File system error (kind only).
    Io(std::io::ErrorKind),
    /// Refused by policy (portable mode, purpose rules).
    Policy(&'static str),
    /// Format-reserved feature that is not implemented yet.
    NotImplemented(&'static str),
    /// This platform has no key wrap (non-Windows in the MVP).
    Unsupported,
    /// The OS key wrap or the random source failed. The UI turns this into "ask every time".
    Unavailable(&'static str),
    /// The file is older than the newest version seen on this machine.
    Rollback { file_counter: u64, seen_counter: u64 },
    /// Input exceeds a hard size limit.
    TooLarge,
}

impl VaultError {
    /// True when the caller should degrade to "ask every time" (decision Q1) instead of
    /// storing anything.
    pub fn means_ask_every_time(&self) -> bool {
        matches!(
            self,
            VaultError::Unavailable(_)
                | VaultError::Unsupported
                | VaultError::Policy(_)
                | VaultError::NotImplemented(_)
        )
    }
}

impl fmt::Display for VaultError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            VaultError::Locked => f.write_str("vault is locked"),
            VaultError::NotFound => f.write_str("secret not found"),
            VaultError::WrongPassphrase => f.write_str("wrong passphrase"),
            VaultError::Corrupt => f.write_str("vault file is corrupt"),
            VaultError::Tampered => f.write_str("vault file failed authentication"),
            VaultError::UnsupportedVersion => f.write_str("vault format is newer than this build"),
            VaultError::UnsupportedAlgorithm => f.write_str("vault uses an unknown algorithm"),
            VaultError::Io(k) => write!(f, "vault i/o error ({k:?})"),
            VaultError::Policy(why) => write!(f, "refused by policy: {why}"),
            VaultError::NotImplemented(what) => write!(f, "not implemented: {what}"),
            VaultError::Unsupported => f.write_str("no secure key wrap on this platform"),
            VaultError::Unavailable(why) => write!(f, "vault unavailable: {why}"),
            VaultError::Rollback { file_counter, seen_counter } => write!(
                f,
                "vault is older than the last version seen on this machine ({file_counter} < {seen_counter})"
            ),
            VaultError::TooLarge => f.write_str("input too large"),
        }
    }
}

impl std::error::Error for VaultError {}

impl From<std::io::Error> for VaultError {
    fn from(e: std::io::Error) -> Self {
        VaultError::Io(e.kind())
    }
}
