//! One connection, two transports: the helper's request loop talks to a [`Backend`] and never needs
//! to know whether the server speaks SFTP (through the OS `ssh`) or FTP/FTPS (built-in client).

use crate::ftp::FtpSession;
use crate::ops::{Session, WriteParams};
use crate::sftp::SftpError;
use brouillon_remote_proto::*;
use std::sync::atomic::AtomicBool;

type R<T> = Result<T, RemoteError>;

pub enum Backend {
    Sftp(Session),
    Ftp(FtpSession),
}

macro_rules! fwd {
    ($self:ident, $m:ident ( $($a:expr),* )) => {
        match $self {
            Backend::Sftp(s) => s.$m($($a),*),
            Backend::Ftp(s) => s.$m($($a),*),
        }
    };
}

impl Backend {
    pub fn list_dir(&self, path: &RemotePath, cancel: &AtomicBool, emit: impl FnMut(Vec<Entry>)) -> R<ListResult> {
        fwd!(self, list_dir(path, cancel, emit))
    }
    pub fn stat(&self, path: &RemotePath, follow: bool) -> R<StatResult> {
        fwd!(self, stat(path, follow))
    }
    #[allow(clippy::too_many_arguments)]
    pub fn read(
        &self,
        path: &RemotePath,
        offset: u64,
        len: Option<u64>,
        max_bytes: Option<u64>,
        allow_symlink: bool,
        cancel: &AtomicBool,
        emit: impl FnMut(u64, &[u8]) -> Result<(), SftpError>,
    ) -> R<ReadResult> {
        fwd!(self, read(path, offset, len, max_bytes, allow_symlink, cancel, emit))
    }
    pub fn write(&self, p: &WriteParams, cancel: &AtomicBool, next: impl FnMut() -> R<Option<Vec<u8>>>, progress: impl FnMut(u64)) -> R<WriteResult> {
        fwd!(self, write(p, cancel, next, progress))
    }
    pub fn rename(&self, from: &RemotePath, to: &RemotePath, overwrite: bool, allow_nonatomic: bool) -> R<RenameResult> {
        fwd!(self, rename(from, to, overwrite, allow_nonatomic))
    }
    pub fn remove(&self, p: &RemotePath) -> R<()> {
        fwd!(self, remove(p))
    }
    pub fn rmdir(&self, p: &RemotePath) -> R<()> {
        fwd!(self, rmdir(p))
    }
    pub fn mkdir(&self, p: &RemotePath, mode: Option<u32>) -> R<()> {
        fwd!(self, mkdir(p, mode))
    }
    pub fn chmod(&self, p: &RemotePath, mode: u32) -> R<()> {
        fwd!(self, chmod(p, mode))
    }
    pub fn realpath(&self, p: &RemotePath) -> R<RemotePath> {
        fwd!(self, realpath(p))
    }
    pub fn readlink(&self, p: &RemotePath) -> R<String> {
        fwd!(self, readlink(p))
    }
}
