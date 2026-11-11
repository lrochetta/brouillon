//! SFTP version 3 packets (draft-ietf-secsh-filexfer-02 plus the OpenSSH extensions
//! `posix-rename@openssh.com` and `fsync@openssh.com`), both directions:
//! the client encodes [`Msg`] and decodes [`Reply`]; the in-process test server does the opposite.

use super::wire::*;
use super::SftpError;
use std::io::Read;

pub const FXP_INIT: u8 = 1;
pub const FXP_VERSION: u8 = 2;
pub const FXP_OPEN: u8 = 3;
pub const FXP_CLOSE: u8 = 4;
pub const FXP_READ: u8 = 5;
pub const FXP_WRITE: u8 = 6;
pub const FXP_LSTAT: u8 = 7;
pub const FXP_FSTAT: u8 = 8;
pub const FXP_SETSTAT: u8 = 9;
pub const FXP_OPENDIR: u8 = 11;
pub const FXP_READDIR: u8 = 12;
pub const FXP_REMOVE: u8 = 13;
pub const FXP_MKDIR: u8 = 14;
pub const FXP_RMDIR: u8 = 15;
pub const FXP_REALPATH: u8 = 16;
pub const FXP_STAT: u8 = 17;
pub const FXP_RENAME: u8 = 18;
pub const FXP_READLINK: u8 = 19;
pub const FXP_STATUS: u8 = 101;
pub const FXP_HANDLE: u8 = 102;
pub const FXP_DATA: u8 = 103;
pub const FXP_NAME: u8 = 104;
pub const FXP_ATTRS: u8 = 105;
pub const FXP_EXTENDED: u8 = 200;
pub const FXP_EXTENDED_REPLY: u8 = 201;

pub const FX_OK: u32 = 0;
pub const FX_EOF: u32 = 1;
pub const FX_NO_SUCH_FILE: u32 = 2;
pub const FX_PERMISSION_DENIED: u32 = 3;
pub const FX_FAILURE: u32 = 4;
pub const FX_BAD_MESSAGE: u32 = 5;
pub const FX_NO_CONNECTION: u32 = 6;
pub const FX_CONNECTION_LOST: u32 = 7;
pub const FX_OP_UNSUPPORTED: u32 = 8;

pub const OPEN_READ: u32 = 1;
pub const OPEN_WRITE: u32 = 2;
pub const OPEN_APPEND: u32 = 4;
pub const OPEN_CREAT: u32 = 8;
pub const OPEN_TRUNC: u32 = 0x10;
pub const OPEN_EXCL: u32 = 0x20;

const ATTR_SIZE: u32 = 1;
const ATTR_UIDGID: u32 = 2;
const ATTR_PERMISSIONS: u32 = 4;
const ATTR_ACMODTIME: u32 = 8;
const ATTR_EXTENDED: u32 = 0x8000_0000;

pub const S_IFMT: u32 = 0o170000;
pub const S_IFDIR: u32 = 0o040000;
pub const S_IFREG: u32 = 0o100000;
pub const S_IFLNK: u32 = 0o120000;

pub const EXT_POSIX_RENAME: &str = "posix-rename@openssh.com";
pub const EXT_FSYNC: &str = "fsync@openssh.com";

/// Largest handle we accept from a server.
pub const MAX_HANDLE: usize = 256;
/// Sanity cap on extension pairs / extended attributes.
const MAX_EXT_PAIRS: u32 = 64;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Attrs {
    pub size: Option<u64>,
    pub uid_gid: Option<(u32, u32)>,
    pub perm: Option<u32>,
    pub times: Option<(u32, u32)>,
}

impl Attrs {
    pub fn with_perm(perm: u32) -> Attrs {
        Attrs { perm: Some(perm), ..Default::default() }
    }
    pub fn encode(&self, o: &mut Vec<u8>) {
        let mut flags = 0;
        if self.size.is_some() {
            flags |= ATTR_SIZE;
        }
        if self.uid_gid.is_some() {
            flags |= ATTR_UIDGID;
        }
        if self.perm.is_some() {
            flags |= ATTR_PERMISSIONS;
        }
        if self.times.is_some() {
            flags |= ATTR_ACMODTIME;
        }
        put_u32(o, flags);
        if let Some(s) = self.size {
            put_u64(o, s);
        }
        if let Some((u, g)) = self.uid_gid {
            put_u32(o, u);
            put_u32(o, g);
        }
        if let Some(p) = self.perm {
            put_u32(o, p);
        }
        if let Some((a, m)) = self.times {
            put_u32(o, a);
            put_u32(o, m);
        }
    }
    pub fn decode(r: &mut Rd) -> Result<Attrs, SftpError> {
        let flags = r.u32()?;
        let mut a = Attrs::default();
        if flags & ATTR_SIZE != 0 {
            a.size = Some(r.u64()?);
        }
        if flags & ATTR_UIDGID != 0 {
            a.uid_gid = Some((r.u32()?, r.u32()?));
        }
        if flags & ATTR_PERMISSIONS != 0 {
            a.perm = Some(r.u32()?);
        }
        if flags & ATTR_ACMODTIME != 0 {
            a.times = Some((r.u32()?, r.u32()?));
        }
        if flags & ATTR_EXTENDED != 0 {
            let n = r.u32()?;
            if n > MAX_EXT_PAIRS {
                return Err(SftpError::Protocol(format!("{n} extended attributes")));
            }
            for _ in 0..n {
                r.string()?;
                r.string()?;
            }
        }
        Ok(a)
    }
    pub fn file_type(&self) -> Option<u32> {
        self.perm.map(|p| p & S_IFMT)
    }
    pub fn mtime(&self) -> Option<i64> {
        self.times.map(|(_, m)| m as i64)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NameEntry {
    pub filename: Vec<u8>,
    /// Free text from the server; never used.
    pub longname: Vec<u8>,
    pub attrs: Attrs,
}

/// Client -> server.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Msg {
    Init { version: u32 },
    Open { id: u32, path: Vec<u8>, pflags: u32, attrs: Attrs },
    Close { id: u32, handle: Vec<u8> },
    Read { id: u32, handle: Vec<u8>, offset: u64, len: u32 },
    Write { id: u32, handle: Vec<u8>, offset: u64, data: Vec<u8> },
    Lstat { id: u32, path: Vec<u8> },
    Fstat { id: u32, handle: Vec<u8> },
    Setstat { id: u32, path: Vec<u8>, attrs: Attrs },
    Opendir { id: u32, path: Vec<u8> },
    Readdir { id: u32, handle: Vec<u8> },
    Remove { id: u32, path: Vec<u8> },
    Mkdir { id: u32, path: Vec<u8>, attrs: Attrs },
    Rmdir { id: u32, path: Vec<u8> },
    Realpath { id: u32, path: Vec<u8> },
    Stat { id: u32, path: Vec<u8> },
    Rename { id: u32, from: Vec<u8>, to: Vec<u8> },
    Readlink { id: u32, path: Vec<u8> },
    /// `data` is the request-specific body (already encoded).
    Extended { id: u32, name: String, data: Vec<u8> },
}

impl Msg {
    pub fn id(&self) -> Option<u32> {
        use Msg::*;
        match self {
            Init { .. } => None,
            Open { id, .. } | Close { id, .. } | Read { id, .. } | Write { id, .. } | Lstat { id, .. }
            | Fstat { id, .. } | Setstat { id, .. } | Opendir { id, .. } | Readdir { id, .. }
            | Remove { id, .. } | Mkdir { id, .. } | Rmdir { id, .. } | Realpath { id, .. } | Stat { id, .. }
            | Rename { id, .. } | Readlink { id, .. } | Extended { id, .. } => Some(*id),
        }
    }

    /// Full packet including the 4-byte length prefix.
    pub fn encode(&self) -> Vec<u8> {
        use Msg::*;
        let mut b = Vec::with_capacity(64);
        put_u32(&mut b, 0); // length placeholder
        match self {
            Init { version } => {
                put_u8(&mut b, FXP_INIT);
                put_u32(&mut b, *version);
            }
            Open { id, path, pflags, attrs } => {
                put_u8(&mut b, FXP_OPEN);
                put_u32(&mut b, *id);
                put_str(&mut b, path);
                put_u32(&mut b, *pflags);
                attrs.encode(&mut b);
            }
            Close { id, handle } => {
                put_u8(&mut b, FXP_CLOSE);
                put_u32(&mut b, *id);
                put_str(&mut b, handle);
            }
            Read { id, handle, offset, len } => {
                put_u8(&mut b, FXP_READ);
                put_u32(&mut b, *id);
                put_str(&mut b, handle);
                put_u64(&mut b, *offset);
                put_u32(&mut b, *len);
            }
            Write { id, handle, offset, data } => {
                put_u8(&mut b, FXP_WRITE);
                put_u32(&mut b, *id);
                put_str(&mut b, handle);
                put_u64(&mut b, *offset);
                put_str(&mut b, data);
            }
            Lstat { id, path } => path_msg(&mut b, FXP_LSTAT, *id, path),
            Fstat { id, handle } => path_msg(&mut b, FXP_FSTAT, *id, handle),
            Setstat { id, path, attrs } => {
                path_msg(&mut b, FXP_SETSTAT, *id, path);
                attrs.encode(&mut b);
            }
            Opendir { id, path } => path_msg(&mut b, FXP_OPENDIR, *id, path),
            Readdir { id, handle } => path_msg(&mut b, FXP_READDIR, *id, handle),
            Remove { id, path } => path_msg(&mut b, FXP_REMOVE, *id, path),
            Mkdir { id, path, attrs } => {
                path_msg(&mut b, FXP_MKDIR, *id, path);
                attrs.encode(&mut b);
            }
            Rmdir { id, path } => path_msg(&mut b, FXP_RMDIR, *id, path),
            Realpath { id, path } => path_msg(&mut b, FXP_REALPATH, *id, path),
            Stat { id, path } => path_msg(&mut b, FXP_STAT, *id, path),
            Rename { id, from, to } => {
                path_msg(&mut b, FXP_RENAME, *id, from);
                put_str(&mut b, to);
            }
            Readlink { id, path } => path_msg(&mut b, FXP_READLINK, *id, path),
            Extended { id, name, data } => {
                put_u8(&mut b, FXP_EXTENDED);
                put_u32(&mut b, *id);
                put_str(&mut b, name.as_bytes());
                b.extend_from_slice(data);
            }
        }
        let len = (b.len() - 4) as u32;
        b[..4].copy_from_slice(&len.to_be_bytes());
        b
    }

    /// Decode a packet body (type byte + payload, no length prefix). Used by the test server.
    pub fn decode(p: &[u8]) -> Result<Msg, SftpError> {
        let mut r = Rd::new(p);
        let t = r.u8()?;
        if t == FXP_INIT {
            let version = r.u32()?;
            return Ok(Msg::Init { version });
        }
        let id = r.u32()?;
        let m = match t {
            FXP_OPEN => {
                let path = r.string()?.to_vec();
                let pflags = r.u32()?;
                let attrs = Attrs::decode(&mut r)?;
                Msg::Open { id, path, pflags, attrs }
            }
            FXP_CLOSE => Msg::Close { id, handle: r.string()?.to_vec() },
            FXP_READ => {
                let handle = r.string()?.to_vec();
                let offset = r.u64()?;
                let len = r.u32()?;
                Msg::Read { id, handle, offset, len }
            }
            FXP_WRITE => {
                let handle = r.string()?.to_vec();
                let offset = r.u64()?;
                let data = r.string()?.to_vec();
                Msg::Write { id, handle, offset, data }
            }
            FXP_LSTAT => Msg::Lstat { id, path: r.string()?.to_vec() },
            FXP_FSTAT => Msg::Fstat { id, handle: r.string()?.to_vec() },
            FXP_SETSTAT => {
                let path = r.string()?.to_vec();
                Msg::Setstat { id, path, attrs: Attrs::decode(&mut r)? }
            }
            FXP_OPENDIR => Msg::Opendir { id, path: r.string()?.to_vec() },
            FXP_READDIR => Msg::Readdir { id, handle: r.string()?.to_vec() },
            FXP_REMOVE => Msg::Remove { id, path: r.string()?.to_vec() },
            FXP_MKDIR => {
                let path = r.string()?.to_vec();
                Msg::Mkdir { id, path, attrs: Attrs::decode(&mut r)? }
            }
            FXP_RMDIR => Msg::Rmdir { id, path: r.string()?.to_vec() },
            FXP_REALPATH => Msg::Realpath { id, path: r.string()?.to_vec() },
            FXP_STAT => Msg::Stat { id, path: r.string()?.to_vec() },
            FXP_RENAME => {
                let from = r.string()?.to_vec();
                Msg::Rename { id, from, to: r.string()?.to_vec() }
            }
            FXP_READLINK => Msg::Readlink { id, path: r.string()?.to_vec() },
            FXP_EXTENDED => {
                let name = String::from_utf8_lossy(r.string()?).into_owned();
                return Ok(Msg::Extended { id, name, data: r.rest().to_vec() });
            }
            t => return Err(SftpError::Protocol(format!("unknown request type {t}"))),
        };
        r.finish()?;
        Ok(m)
    }
}

fn path_msg(b: &mut Vec<u8>, t: u8, id: u32, s: &[u8]) {
    put_u8(b, t);
    put_u32(b, id);
    put_str(b, s);
}

/// Server -> client.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Reply {
    Version { version: u32, extensions: Vec<(String, Vec<u8>)> },
    Status { id: u32, code: u32, message: String },
    Handle { id: u32, handle: Vec<u8> },
    Data { id: u32, data: Vec<u8> },
    Name { id: u32, entries: Vec<NameEntry> },
    Attrs { id: u32, attrs: Attrs },
    ExtReply { id: u32, data: Vec<u8> },
}

impl Reply {
    pub fn id(&self) -> Option<u32> {
        use Reply::*;
        match self {
            Version { .. } => None,
            Status { id, .. } | Handle { id, .. } | Data { id, .. } | Name { id, .. } | Attrs { id, .. }
            | ExtReply { id, .. } => Some(*id),
        }
    }

    pub fn encode(&self) -> Vec<u8> {
        let mut b = Vec::with_capacity(64);
        put_u32(&mut b, 0);
        match self {
            Reply::Version { version, extensions } => {
                put_u8(&mut b, FXP_VERSION);
                put_u32(&mut b, *version);
                for (n, d) in extensions {
                    put_str(&mut b, n.as_bytes());
                    put_str(&mut b, d);
                }
            }
            Reply::Status { id, code, message } => {
                put_u8(&mut b, FXP_STATUS);
                put_u32(&mut b, *id);
                put_u32(&mut b, *code);
                put_str(&mut b, message.as_bytes());
                put_str(&mut b, b"");
            }
            Reply::Handle { id, handle } => {
                put_u8(&mut b, FXP_HANDLE);
                put_u32(&mut b, *id);
                put_str(&mut b, handle);
            }
            Reply::Data { id, data } => {
                put_u8(&mut b, FXP_DATA);
                put_u32(&mut b, *id);
                put_str(&mut b, data);
            }
            Reply::Name { id, entries } => {
                put_u8(&mut b, FXP_NAME);
                put_u32(&mut b, *id);
                put_u32(&mut b, entries.len() as u32);
                for e in entries {
                    put_str(&mut b, &e.filename);
                    put_str(&mut b, &e.longname);
                    e.attrs.encode(&mut b);
                }
            }
            Reply::Attrs { id, attrs } => {
                put_u8(&mut b, FXP_ATTRS);
                put_u32(&mut b, *id);
                attrs.encode(&mut b);
            }
            Reply::ExtReply { id, data } => {
                put_u8(&mut b, FXP_EXTENDED_REPLY);
                put_u32(&mut b, *id);
                b.extend_from_slice(data);
            }
        }
        let len = (b.len() - 4) as u32;
        b[..4].copy_from_slice(&len.to_be_bytes());
        b
    }

    /// Decode a packet body (type byte + payload). Never panics, never allocates from a declared count.
    pub fn decode(p: &[u8]) -> Result<Reply, SftpError> {
        let mut r = Rd::new(p);
        let t = r.u8()?;
        if t == FXP_VERSION {
            let version = r.u32()?;
            let mut extensions = Vec::new();
            while r.remaining() > 0 {
                if extensions.len() as u32 >= MAX_EXT_PAIRS {
                    return Err(SftpError::Protocol("too many extensions".into()));
                }
                let n = String::from_utf8_lossy(r.string()?).into_owned();
                let d = r.string()?.to_vec();
                extensions.push((n, d));
            }
            return Ok(Reply::Version { version, extensions });
        }
        let id = r.u32()?;
        let m = match t {
            FXP_STATUS => {
                let code = r.u32()?;
                // message and language tag are optional in the wild; the message is free text.
                let message = if r.remaining() >= 4 { String::from_utf8_lossy(r.string()?).into_owned() } else { String::new() };
                let _ = r.rest();
                return Ok(Reply::Status { id, code, message });
            }
            FXP_HANDLE => {
                let h = r.string()?;
                if h.is_empty() || h.len() > MAX_HANDLE {
                    return Err(SftpError::Protocol(format!("handle of {} bytes", h.len())));
                }
                Reply::Handle { id, handle: h.to_vec() }
            }
            FXP_DATA => Reply::Data { id, data: r.string()?.to_vec() },
            FXP_NAME => {
                let count = r.u32()? as usize;
                // each entry is at least filename(4) + longname(4) + attr flags(4)
                if count.saturating_mul(12) > r.remaining() {
                    return Err(SftpError::Protocol(format!("NAME claims {count} entries")));
                }
                let mut entries = Vec::with_capacity(count);
                for _ in 0..count {
                    let filename = r.string()?.to_vec();
                    let longname = r.string()?.to_vec();
                    let attrs = Attrs::decode(&mut r)?;
                    entries.push(NameEntry { filename, longname, attrs });
                }
                Reply::Name { id, entries }
            }
            FXP_ATTRS => Reply::Attrs { id, attrs: Attrs::decode(&mut r)? },
            FXP_EXTENDED_REPLY => return Ok(Reply::ExtReply { id, data: r.rest().to_vec() }),
            t => return Err(SftpError::Protocol(format!("unknown reply type {t}"))),
        };
        r.finish()?;
        Ok(m)
    }
}

/// Read one packet body (type byte + payload). `max` bounds the declared length before allocation.
pub fn read_packet<R: Read>(r: &mut R, max: usize) -> Result<Vec<u8>, SftpError> {
    let mut l = [0u8; 4];
    match r.read_exact(&mut l) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => return Err(SftpError::Eof),
        Err(e) => return Err(SftpError::Io(e.to_string())),
    }
    let len = u32::from_be_bytes(l) as usize;
    if len == 0 {
        return Err(SftpError::Protocol("zero-length packet".into()));
    }
    if len > max {
        return Err(SftpError::TooLarge(len));
    }
    let mut body = vec![0u8; len];
    match r.read_exact(&mut body) {
        Ok(()) => Ok(body),
        Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => Err(SftpError::Eof),
        Err(e) => Err(SftpError::Io(e.to_string())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    fn body(full: &[u8]) -> &[u8] {
        assert_eq!(u32::from_be_bytes(full[..4].try_into().unwrap()) as usize, full.len() - 4);
        &full[4..]
    }

    #[test]
    fn requests_round_trip() {
        let a = Attrs { size: Some(9), uid_gid: Some((1, 2)), perm: Some(0o100644), times: Some((3, 4)) };
        let msgs = vec![
            Msg::Init { version: 3 },
            Msg::Open { id: 1, path: b"/a".to_vec(), pflags: OPEN_WRITE | OPEN_CREAT, attrs: a.clone() },
            Msg::Close { id: 2, handle: b"h".to_vec() },
            Msg::Read { id: 3, handle: b"h".to_vec(), offset: 1 << 33, len: 32768 },
            Msg::Write { id: 4, handle: b"h".to_vec(), offset: 7, data: vec![1, 2, 3] },
            Msg::Lstat { id: 5, path: b"/x".to_vec() },
            Msg::Fstat { id: 6, handle: b"h".to_vec() },
            Msg::Setstat { id: 7, path: b"/x".to_vec(), attrs: Attrs::with_perm(0o600) },
            Msg::Opendir { id: 8, path: b"/".to_vec() },
            Msg::Readdir { id: 9, handle: b"h".to_vec() },
            Msg::Remove { id: 10, path: b"/x".to_vec() },
            Msg::Mkdir { id: 11, path: b"/d".to_vec(), attrs: Attrs::default() },
            Msg::Rmdir { id: 12, path: b"/d".to_vec() },
            Msg::Realpath { id: 13, path: b".".to_vec() },
            Msg::Stat { id: 14, path: b"/x".to_vec() },
            Msg::Rename { id: 15, from: b"/a".to_vec(), to: b"/b".to_vec() },
            Msg::Readlink { id: 16, path: b"/l".to_vec() },
            Msg::Extended { id: 17, name: EXT_POSIX_RENAME.into(), data: vec![0, 0, 0, 1, b'a', 0, 0, 0, 1, b'b'] },
        ];
        for m in msgs {
            let enc = m.encode();
            assert_eq!(Msg::decode(body(&enc)).unwrap(), m);
        }
    }

    #[test]
    fn replies_round_trip() {
        let replies = vec![
            Reply::Version { version: 3, extensions: vec![(EXT_POSIX_RENAME.into(), b"1".to_vec())] },
            Reply::Status { id: 1, code: FX_NO_SUCH_FILE, message: "No such file".into() },
            Reply::Handle { id: 2, handle: b"abc".to_vec() },
            Reply::Data { id: 3, data: vec![9; 100] },
            Reply::Name {
                id: 4,
                entries: vec![NameEntry { filename: b"f".to_vec(), longname: b"-rw f".to_vec(), attrs: Attrs::with_perm(0o100600) }],
            },
            Reply::Attrs { id: 5, attrs: Attrs { size: Some(1), ..Default::default() } },
            Reply::ExtReply { id: 6, data: vec![1, 2] },
        ];
        for r in replies {
            let enc = r.encode();
            assert_eq!(Reply::decode(body(&enc)).unwrap(), r);
        }
    }

    #[test]
    fn status_without_language_tag_or_message_is_accepted() {
        let mut p = vec![FXP_STATUS];
        put_u32(&mut p, 7);
        put_u32(&mut p, FX_EOF);
        assert_eq!(Reply::decode(&p).unwrap(), Reply::Status { id: 7, code: FX_EOF, message: String::new() });
    }

    #[test]
    fn hostile_packets_do_not_panic_or_overallocate() {
        // string length overruns
        let mut p = vec![FXP_DATA];
        put_u32(&mut p, 1);
        put_u32(&mut p, u32::MAX);
        assert!(Reply::decode(&p).is_err());
        // NAME claiming 4 billion entries
        let mut p = vec![FXP_NAME];
        put_u32(&mut p, 1);
        put_u32(&mut p, u32::MAX);
        assert!(Reply::decode(&p).is_err());
        // oversized handle
        let mut p = vec![FXP_HANDLE];
        put_u32(&mut p, 1);
        put_str(&mut p, &vec![0u8; MAX_HANDLE + 1]);
        assert!(Reply::decode(&p).is_err());
        // empty handle
        let mut p = vec![FXP_HANDLE];
        put_u32(&mut p, 1);
        put_str(&mut p, b"");
        assert!(Reply::decode(&p).is_err());
        // unknown type and trailing bytes
        assert!(Reply::decode(&[77, 0, 0, 0, 1]).is_err());
        let mut p = vec![FXP_HANDLE];
        put_u32(&mut p, 1);
        put_str(&mut p, b"h");
        p.push(0);
        assert!(Reply::decode(&p).is_err());
        // truncated everything
        let enc = Reply::Name { id: 1, entries: vec![NameEntry { filename: b"abc".to_vec(), longname: vec![], attrs: Attrs::default() }] }.encode();
        for cut in 0..enc.len() - 4 {
            let _ = Reply::decode(&enc[4..4 + cut]);
        }
        // attrs with absurd extended count
        let mut p = vec![FXP_ATTRS];
        put_u32(&mut p, 1);
        put_u32(&mut p, ATTR_EXTENDED);
        put_u32(&mut p, 1_000_000);
        assert!(Reply::decode(&p).is_err());
    }

    #[test]
    fn read_packet_enforces_cap_before_allocating() {
        let mut v = (u32::MAX).to_be_bytes().to_vec();
        v.push(0);
        assert!(matches!(read_packet(&mut Cursor::new(v), 1 << 20), Err(SftpError::TooLarge(_))));
        assert!(matches!(read_packet(&mut Cursor::new(vec![0, 0, 0, 0]), 100), Err(SftpError::Protocol(_))));
        assert!(matches!(read_packet(&mut Cursor::new(Vec::<u8>::new()), 100), Err(SftpError::Eof)));
        assert!(matches!(read_packet(&mut Cursor::new(vec![0, 0, 0, 9, 1]), 100), Err(SftpError::Eof)));
    }

    #[test]
    fn attrs_ignore_extended_pairs() {
        let mut p = Vec::new();
        put_u32(&mut p, ATTR_SIZE | ATTR_EXTENDED);
        put_u64(&mut p, 5);
        put_u32(&mut p, 1);
        put_str(&mut p, b"k");
        put_str(&mut p, b"v");
        let a = Attrs::decode(&mut Rd::new(&p)).unwrap();
        assert_eq!(a.size, Some(5));
    }
}
