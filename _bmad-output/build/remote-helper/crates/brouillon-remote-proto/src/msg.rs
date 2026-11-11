//! Typed control messages. JSON shapes are documented in docs/REMOTE_HELPER_PROTOCOL.md.

use crate::limits::{Limits, MAX_ERROR_TEXT};
use crate::path::{RemoteName, RemotePath};
use crate::secret::Secret;
use serde::{Deserialize, Serialize};
use serde_json::Value;

// ---------------------------------------------------------------- errors

/// Provider-neutral error classes (the same set will serve an FTP/FTPS backend).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorKind {
    InvalidArgument,
    /// The path is outside the root the session was opened with.
    OutsideRoot,
    NotFound,
    PermissionDenied,
    AlreadyExists,
    NotADirectory,
    /// The `expect` precondition of a write failed: the remote file changed under the editor.
    Conflict,
    Unsupported,
    /// A size/entry cap was exceeded (also when the server lied about a size).
    TooLarge,
    LimitExceeded,
    Timeout,
    Cancelled,
    ConnectionLost,
    ConnectFailed,
    AuthFailed,
    HostKeyRejected,
    HostKeyChanged,
    /// The `ssh` executable was not found.
    NoSsh,
    /// The server (or the editor) sent something malformed.
    Protocol,
    /// Too many concurrent requests.
    Busy,
    /// A symlink was refused (points outside the root, or follow not allowed).
    SymlinkRefused,
    NotConnected,
    /// The server reported a generic failure.
    Failure,
    Internal,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, thiserror::Error)]
#[error("{kind:?}: {message}")]
pub struct RemoteError {
    pub kind: ErrorKind,
    pub message: String,
}

impl RemoteError {
    /// `message` may contain server text: control characters are neutralised and it is capped to 4 KiB.
    pub fn new(kind: ErrorKind, message: impl AsRef<str>) -> Self {
        RemoteError { kind, message: sanitize_text(message.as_ref()) }
    }
}

/// Neutralise control characters and cap length (text from a server is never trusted).
pub fn sanitize_text(s: &str) -> String {
    let mut out = String::with_capacity(s.len().min(MAX_ERROR_TEXT));
    for c in s.chars() {
        let c = if c.is_control() { ' ' } else { c };
        if out.len() + c.len_utf8() > MAX_ERROR_TEXT {
            break;
        }
        out.push(c);
    }
    out
}

// ---------------------------------------------------------------- values

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EntryKind {
    File,
    Dir,
    Symlink,
    Other,
}

/// A directory entry. `name` is a validated single component; `mtime` is Unix seconds from the server
/// (never a content hash); `mode` is the permission bits (`& 0o7777`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Entry {
    pub name: RemoteName,
    pub kind: EntryKind,
    pub size: Option<u64>,
    pub mtime: Option<i64>,
    pub mode: Option<u32>,
    /// Legal remotely but unsafe as a Windows file name (display hint only; see RemoteName).
    #[serde(default)]
    pub local_unsafe: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HostKeyPolicy {
    /// `StrictHostKeyChecking=yes`: unknown or changed key -> failure.
    Strict,
    /// `StrictHostKeyChecking=accept-new`: trust on first use without a prompt, refuse changes.
    AcceptNew,
    /// `StrictHostKeyChecking=ask`: the confirmation reaches the editor as a `prompt` event (needs `interactive`).
    Ask,
}

/// Transport the helper speaks to the server. Absent in `connect` = `sftp` (backward compatible).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Protocol {
    /// SFTP v3 through the OS `ssh` (the original path, untouched).
    #[default]
    Sftp,
    /// FTP over TLS (explicit `AUTH TLS` by default, or implicit), certificate validated, `PROT P`.
    Ftps,
    /// Plain FTP. Refused unless `ftp.allow_plain` is set; the result then carries `insecure: true`.
    Ftp,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TlsMode {
    /// Connect in clear, then `AUTH TLS` (port 21). Refusal by the server = hard failure.
    #[default]
    Explicit,
    /// TLS from the first byte (port 990).
    Implicit,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FingerprintPolicy {
    /// System roots only. A certificate they do not accept fails the connection.
    Strict,
    /// System roots, or the certificate whose SHA-256 is `pinned_fingerprint`. Unknown = failure.
    Pinned,
    /// Like `pinned`, but an unknown certificate raises a `host_key` prompt showing the SHA-256
    /// fingerprint (answer `yes` to trust it for this connection and read it back from the result).
    #[default]
    Ask,
}

/// FTP/FTPS-only connect parameters (`protocol` = `ftps` | `ftp`). There is deliberately no option
/// that disables certificate validation.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct FtpOptions {
    #[serde(default)]
    pub tls: TlsMode,
    #[serde(default)]
    pub fingerprint_policy: FingerprintPolicy,
    /// SHA-256 of the server's leaf certificate (hex, `:` separators tolerated). When set it is ALWAYS
    /// enforced: a different certificate is a hard stop (`host_key_changed`), even if a CA signed it.
    pub pinned_fingerprint: Option<String>,
    /// Per-site opt-in for protocol `ftp` (credentials and data cross the network in clear).
    #[serde(default)]
    pub allow_plain: bool,
    /// Password. Never logged, never in argv, redacted by `Debug`. Absent = ask through a `password` prompt.
    pub password: Option<Secret>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConnectParams {
    /// Host name, address or an `ssh_config` alias (honoured).
    pub host: String,
    pub port: Option<u16>,
    pub user: Option<String>,
    pub identity_file: Option<String>,
    /// `-J` value (pass-through of ProxyJump).
    pub proxy_jump: Option<String>,
    pub host_key_policy: HostKeyPolicy,
    /// Brouillon-owned known_hosts file; default: OpenSSH's own.
    pub known_hosts_file: Option<String>,
    /// Alternative ssh_config (`-F`).
    pub ssh_config_file: Option<String>,
    /// Allow prompts (password/passphrase/host key) through the editor. `false` = `BatchMode=yes`.
    #[serde(default)]
    pub interactive: bool,
    pub connect_timeout_s: Option<u32>,
    /// Explicit path to `ssh`; default: Windows OpenSSH, then PATH.
    pub ssh_path: Option<String>,
    /// All later paths must stay under this root (lexically checked).
    pub root: Option<RemotePath>,
    /// Transport. Default `sftp`; with `ftps`/`ftp` the ssh-specific fields above are ignored.
    #[serde(default)]
    pub protocol: Protocol,
    /// FTP/FTPS options (safe defaults when absent).
    #[serde(default)]
    pub ftp: Option<FtpOptions>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CreateMode {
    /// Fail if the file exists (`O_EXCL`): used for the temp file of a safe save.
    New,
    /// Create or truncate.
    Truncate,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Expect {
    pub size: u64,
    pub mtime: i64,
}

// ---------------------------------------------------------------- requests

/// Request body; the envelope is [`RequestFrame`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "method", content = "params", rename_all = "snake_case")]
pub enum Request {
    Hello { proto: u32, client: String },
    Ping {},
    Connect(ConnectParams),
    Disconnect {},
    ListDir { path: RemotePath },
    Stat {
        path: RemotePath,
        /// Follow a final symlink (and report where it resolves).
        #[serde(default)]
        follow: bool,
    },
    /// Content comes back as DATA frames (`id`, `offset`, bytes) followed by the result.
    Read {
        path: RemotePath,
        #[serde(default)]
        offset: u64,
        /// `None` = to end of file (still capped by `max_bytes`).
        len: Option<u64>,
        /// Hard cap on bytes actually received; default `limits.max_read_bytes`.
        max_bytes: Option<u64>,
        #[serde(default)]
        allow_symlink: bool,
    },
    /// Exactly `size` bytes follow as DATA frames with contiguous offsets from 0. The editor must
    /// keep at most `limits.write_window` bytes unacknowledged (see `progress` events).
    Write {
        path: RemotePath,
        size: u64,
        create: CreateMode,
        /// Mode for a newly created file (e.g. 0o644); `None` = server default.
        perm: Option<u32>,
        /// Refuse (Conflict) when the existing file's size/mtime differ.
        expect: Option<Expect>,
        /// Ask the server to fsync before close when it supports `fsync@openssh.com`.
        #[serde(default)]
        fsync: bool,
        #[serde(default)]
        allow_symlink: bool,
    },
    Rename {
        from: RemotePath,
        to: RemotePath,
        /// Replace an existing target. Atomic only with `posix-rename@openssh.com`.
        #[serde(default)]
        overwrite: bool,
        /// With `overwrite` and no posix-rename: allow remove+rename (result says `atomic:false`).
        #[serde(default)]
        allow_nonatomic: bool,
    },
    /// Remove a file or a symlink (never follows). Not for directories.
    Remove { path: RemotePath },
    Rmdir { path: RemotePath },
    Mkdir { path: RemotePath, mode: Option<u32> },
    Chmod { path: RemotePath, mode: u32 },
    Realpath { path: RemotePath },
    Readlink { path: RemotePath },
    /// Cancel the in-flight request `target`.
    Cancel { target: u64 },
    /// Answer to a `prompt` event; `secret: None` = the user cancelled.
    PromptAnswer { prompt_id: u64, secret: Option<Secret> },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RequestFrame {
    pub id: u64,
    #[serde(flatten)]
    pub request: Request,
}

// ---------------------------------------------------------------- results

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HelloResult {
    pub proto: u32,
    pub helper_version: String,
    pub caps: Vec<String>,
    pub limits: Limits,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ConnectResult {
    /// The server's working directory (REALPATH of "."), or the root when one was given.
    pub home: RemotePath,
    pub sftp_version: u32,
    pub extensions: Vec<String>,
    pub posix_rename: bool,
    pub fsync: bool,
    /// Transport that was actually negotiated.
    #[serde(default)]
    pub protocol: Protocol,
    /// Permanent warning flag: the session is not encrypted (plain FTP). Show it for the whole session.
    #[serde(default)]
    pub insecure: bool,
    /// FTP/FTPS only: what the server can do (all flags are truthful, see docs).
    #[serde(default)]
    pub ftp: Option<FtpCaps>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct FtpCaps {
    pub tls: bool,
    /// e.g. `TLSv1.3`.
    pub tls_version: Option<String>,
    /// SHA-256 (lowercase hex) of the certificate this session was verified against; store it as the pin.
    pub cert_fingerprint: Option<String>,
    /// How the certificate was accepted: `system_roots`, `pinned` or `user_confirmed`.
    pub cert_trust: Option<String>,
    /// FTP has no `posix-rename`: replacing a file is never reported atomic.
    pub atomic_replace: bool,
    pub mlst: bool,
    pub mdtm: bool,
    pub size: bool,
    pub utf8: bool,
    /// `true` only if the server offers HASH or XCRC/XMD5/XSHA*.
    pub supports_hash: bool,
    pub hash_algos: Vec<String>,
    /// Human-readable warnings (e.g. plain FTP).
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ListResult {
    pub count: usize,
    /// More entries existed than `max_list_entries` (or the byte cap was hit).
    pub truncated: bool,
    /// Entries dropped because of an unsafe name or a duplicate.
    pub hidden_unsafe: usize,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StatResult {
    pub kind: EntryKind,
    pub size: Option<u64>,
    pub mtime: Option<i64>,
    pub mode: Option<u32>,
    /// With `follow` on a symlink: where it resolves (REALPATH) and whether that is outside the root.
    pub resolved: Option<RemotePath>,
    #[serde(default)]
    pub outside_root: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ReadResult {
    /// Bytes sent as DATA frames.
    pub bytes: u64,
    pub eof: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WriteResult {
    pub size: u64,
    /// Mtime reported by the server after the write (FSTAT before close), if any.
    pub mtime: Option<i64>,
    pub fsynced: bool,
    /// The target was replaced atomically. Always `false` on FTP.
    #[serde(default)]
    pub atomic: bool,
    /// FTP: the 3-step swap (target -> old, temp -> target, delete old) was needed because the server
    /// refuses to rename over an existing file; a crash in the middle can leave `.brouillon-old-*`.
    #[serde(default)]
    pub swapped: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RenameResult {
    pub atomic: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RealpathResult {
    pub path: RemotePath,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ReadlinkResult {
    /// Untrusted display text (may be relative, absolute, or garbage).
    pub target: String,
}

// ---------------------------------------------------------------- events

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PromptKind {
    HostKey,
    Passphrase,
    Password,
    KeyboardInteractive,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConnState {
    Connected,
    Lost,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum Event {
    /// `done` bytes have been accepted by the server for write request `id` (acknowledgement for flow control).
    Progress { id: u64, done: u64, total: Option<u64> },
    ListPage { id: u64, entries: Vec<Entry> },
    /// Answer with `prompt_answer`. `text` is the (sanitised) prompt printed by ssh, shown verbatim.
    Prompt { prompt_id: u64, kind: PromptKind, text: String },
    State { state: ConnState, reason: Option<String> },
}

// ---------------------------------------------------------------- server -> editor envelope

#[derive(Debug, Clone, PartialEq)]
pub enum ServerMessage {
    Result { id: u64, result: Value },
    Error { id: Option<u64>, error: RemoteError },
    Event(Event),
}

impl ServerMessage {
    pub fn ok<T: Serialize>(id: u64, result: &T) -> ServerMessage {
        ServerMessage::Result { id, result: serde_json::to_value(result).expect("serialisable result") }
    }
    pub fn err(id: Option<u64>, error: RemoteError) -> ServerMessage {
        ServerMessage::Error { id, error }
    }

    pub fn to_bytes(&self) -> Vec<u8> {
        let v = match self {
            ServerMessage::Result { id, result } => serde_json::json!({ "id": id, "result": result }),
            ServerMessage::Error { id, error } => serde_json::json!({ "id": id, "error": error }),
            ServerMessage::Event(e) => serde_json::to_value(e).expect("serialisable event"),
        };
        serde_json::to_vec(&v).expect("json")
    }

    pub fn from_slice(b: &[u8]) -> Result<ServerMessage, String> {
        let v: Value = serde_json::from_slice(b).map_err(|e| e.to_string())?;
        let obj = v.as_object().ok_or("not an object")?;
        if obj.contains_key("event") {
            return serde_json::from_value(v).map(ServerMessage::Event).map_err(|e| e.to_string());
        }
        let id = obj.get("id").and_then(Value::as_u64);
        if let Some(e) = obj.get("error") {
            let error: RemoteError = serde_json::from_value(e.clone()).map_err(|e| e.to_string())?;
            return Ok(ServerMessage::Error { id, error });
        }
        if let Some(r) = obj.get("result") {
            return Ok(ServerMessage::Result { id: id.ok_or("result without id")?, result: r.clone() });
        }
        Err("neither event, result nor error".into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rt_req(r: Request) {
        let f = RequestFrame { id: 42, request: r };
        let s = serde_json::to_string(&f).unwrap();
        let back: RequestFrame = serde_json::from_str(&s).unwrap();
        assert_eq!(back, f, "{s}");
    }

    #[test]
    fn every_request_round_trips() {
        let p = RemotePath::parse("/srv/a").unwrap();
        rt_req(Request::Hello { proto: 1, client: "t".into() });
        rt_req(Request::Ping {});
        rt_req(Request::Disconnect {});
        rt_req(Request::ListDir { path: p.clone() });
        rt_req(Request::Stat { path: p.clone(), follow: true });
        rt_req(Request::Read { path: p.clone(), offset: 5, len: Some(10), max_bytes: None, allow_symlink: false });
        rt_req(Request::Write {
            path: p.clone(),
            size: 3,
            create: CreateMode::New,
            perm: Some(0o644),
            expect: Some(Expect { size: 1, mtime: 2 }),
            fsync: true,
            allow_symlink: false,
        });
        rt_req(Request::Rename { from: p.clone(), to: p.clone(), overwrite: true, allow_nonatomic: false });
        rt_req(Request::Remove { path: p.clone() });
        rt_req(Request::Rmdir { path: p.clone() });
        rt_req(Request::Mkdir { path: p.clone(), mode: None });
        rt_req(Request::Chmod { path: p.clone(), mode: 0o600 });
        rt_req(Request::Realpath { path: p.clone() });
        rt_req(Request::Readlink { path: p.clone() });
        rt_req(Request::Cancel { target: 9 });
        rt_req(Request::PromptAnswer { prompt_id: 1, secret: Some(Secret::new("x")) });
        rt_req(Request::Connect(ConnectParams {
            host: "myvps".into(),
            port: Some(2222),
            user: None,
            identity_file: None,
            proxy_jump: None,
            host_key_policy: HostKeyPolicy::Strict,
            known_hosts_file: None,
            ssh_config_file: None,
            interactive: true,
            connect_timeout_s: None,
            ssh_path: None,
            root: Some(p),
            protocol: Protocol::Sftp,
            ftp: None,
        }));
        rt_req(Request::Connect(ConnectParams {
            host: "ftp.example".into(),
            port: Some(21),
            user: Some("u".into()),
            identity_file: None,
            proxy_jump: None,
            host_key_policy: HostKeyPolicy::Strict,
            known_hosts_file: None,
            ssh_config_file: None,
            interactive: true,
            connect_timeout_s: None,
            ssh_path: None,
            root: None,
            protocol: Protocol::Ftps,
            ftp: Some(FtpOptions { pinned_fingerprint: Some("ab".repeat(32)), ..FtpOptions::default() }),
        }));
    }

    #[test]
    fn old_connect_params_still_parse_and_password_is_redacted() {
        let old = r#"{"id":1,"method":"connect","params":{"host":"h","port":null,"user":null,"identity_file":null,"proxy_jump":null,"host_key_policy":"strict","known_hosts_file":null,"ssh_config_file":null,"connect_timeout_s":null,"ssh_path":null,"root":null}}"#;
        let f: RequestFrame = serde_json::from_str(old).unwrap();
        let Request::Connect(c) = f.request else { panic!() };
        assert_eq!(c.protocol, Protocol::Sftp);
        assert!(c.ftp.is_none());
        let o = FtpOptions { password: Some(Secret::new("hunter2")), ..FtpOptions::default() };
        assert!(!format!("{o:?}").contains("hunter2"));
        assert_eq!(o.fingerprint_policy, FingerprintPolicy::Ask);
        assert!(!o.allow_plain);
    }

    #[test]
    fn wire_shape_is_stable() {
        let f: RequestFrame =
            serde_json::from_str(r#"{"id":3,"method":"list_dir","params":{"path":"/a//b/../c"}}"#).unwrap();
        assert_eq!(f.id, 3);
        assert_eq!(f.request, Request::ListDir { path: RemotePath::parse("/a/c").unwrap() });
        let f: RequestFrame = serde_json::from_str(r#"{"id":4,"method":"ping","params":{}}"#).unwrap();
        assert_eq!(f.request, Request::Ping {});
    }

    #[test]
    fn hostile_request_paths_are_rejected_at_parse() {
        assert!(serde_json::from_str::<RequestFrame>(r#"{"id":1,"method":"stat","params":{"path":"rel"}}"#).is_err());
        assert!(serde_json::from_str::<RequestFrame>(r#"{"id":1,"method":"stat","params":{"path":"/a\u0000b"}}"#).is_err());
        assert!(serde_json::from_str::<RequestFrame>(r#"{"id":1,"method":"nope","params":{}}"#).is_err());
    }

    #[test]
    fn server_messages_round_trip() {
        let m = ServerMessage::ok(5, &ReadResult { bytes: 3, eof: true });
        assert_eq!(ServerMessage::from_slice(&m.to_bytes()).unwrap(), m);
        let m = ServerMessage::err(Some(5), RemoteError::new(ErrorKind::NotFound, "no such file"));
        assert_eq!(ServerMessage::from_slice(&m.to_bytes()).unwrap(), m);
        let m = ServerMessage::err(None, RemoteError::new(ErrorKind::Protocol, "bad frame"));
        assert_eq!(ServerMessage::from_slice(&m.to_bytes()).unwrap(), m);
        let m = ServerMessage::Event(Event::Progress { id: 1, done: 10, total: Some(20) });
        assert_eq!(ServerMessage::from_slice(&m.to_bytes()).unwrap(), m);
        let m = ServerMessage::Event(Event::Prompt { prompt_id: 1, kind: PromptKind::HostKey, text: "x".into() });
        assert_eq!(ServerMessage::from_slice(&m.to_bytes()).unwrap(), m);
        let m = ServerMessage::Event(Event::State { state: ConnState::Lost, reason: None });
        assert_eq!(ServerMessage::from_slice(&m.to_bytes()).unwrap(), m);
        assert!(ServerMessage::from_slice(b"[]").is_err());
        assert!(ServerMessage::from_slice(b"{\"x\":1}").is_err());
    }

    #[test]
    fn error_text_is_sanitised_and_capped() {
        let e = RemoteError::new(ErrorKind::Failure, "bad\r\n\x1b[31mtext");
        assert!(!e.message.chars().any(|c| c.is_control()));
        let e = RemoteError::new(ErrorKind::Failure, "é".repeat(10_000));
        assert!(e.message.len() <= MAX_ERROR_TEXT);
    }

    #[test]
    fn secret_never_in_debug() {
        let r = Request::PromptAnswer { prompt_id: 1, secret: Some(Secret::new("hunter2")) };
        assert!(!format!("{r:?}").contains("hunter2"));
    }
}
