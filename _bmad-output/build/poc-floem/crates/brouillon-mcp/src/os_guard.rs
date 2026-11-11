//! OS-level access control for the local pipe, as far as the `interprocess` crate allows.
//!
//! * Windows: the named pipe is created with a DACL that grants access to the current user's SID
//!   only (`D:P(A;;GA;;;<sid>)`: protected, no inherited entries, so no "Everyone" and no
//!   "Anonymous"). `interprocess` also creates the first instance with `FILE_FLAG_FIRST_PIPE_INSTANCE`
//!   (a squatted name makes `create` fail) and rejects remote clients by default.
//!   After accept, [`peer_user_matches`] re-checks that the connecting process runs as the same user.
//! * Unix: the socket file is created in the user's own directory; `peer_user_matches` compares the
//!   peer's effective uid with the owner of that directory.
//!
//! Not done (documented in SECURITY.md): a verified binding between the connecting shim and the
//! *client program* behind it. The shim reports its parent executable and the editor trusts that
//! only as informational input to re-consent; the pipe peer is the shim, not the agent.

use std::io;

/// Result of the post-accept same-user check.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Peer {
    SameUser,
    OtherUser,
    /// The OS would not tell (for example a more privileged process). The pipe DACL still applies.
    Unknown,
}

#[cfg(windows)]
mod win {
    use super::*;
    use std::ptr::null_mut;
    use windows_sys::Win32::Foundation::{CloseHandle, LocalFree, HANDLE};
    use windows_sys::Win32::Security::Authorization::ConvertSidToStringSidW;
    use windows_sys::Win32::Security::{EqualSid, GetTokenInformation, TokenUser, TOKEN_QUERY, TOKEN_USER};
    use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcess, OpenProcessToken, PROCESS_QUERY_LIMITED_INFORMATION};

    struct Handle(HANDLE);
    impl Drop for Handle {
        fn drop(&mut self) {
            if !self.0.is_null() {
                unsafe { CloseHandle(self.0) };
            }
        }
    }

    /// TOKEN_USER of a token, as a buffer whose first bytes are the struct (the SID lives inside it).
    fn token_user(token: HANDLE) -> io::Result<Vec<u8>> {
        let mut needed = 0u32;
        unsafe { GetTokenInformation(token, TokenUser, null_mut(), 0, &mut needed) };
        if needed == 0 {
            return Err(io::Error::last_os_error());
        }
        let mut buf = vec![0u8; needed as usize];
        let ok = unsafe { GetTokenInformation(token, TokenUser, buf.as_mut_ptr().cast(), needed, &mut needed) };
        if ok == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(buf)
    }

    fn own_token_user() -> io::Result<Vec<u8>> {
        let mut t: HANDLE = null_mut();
        if unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut t) } == 0 {
            return Err(io::Error::last_os_error());
        }
        let t = Handle(t);
        token_user(t.0)
    }

    pub fn current_user_sid_string() -> io::Result<String> {
        let buf = own_token_user()?;
        let tu = unsafe { &*(buf.as_ptr() as *const TOKEN_USER) };
        let mut wide: *mut u16 = null_mut();
        if unsafe { ConvertSidToStringSidW(tu.User.Sid, &mut wide) } == 0 {
            return Err(io::Error::last_os_error());
        }
        let mut len = 0;
        while unsafe { *wide.add(len) } != 0 {
            len += 1;
        }
        let s = String::from_utf16_lossy(unsafe { std::slice::from_raw_parts(wide, len) });
        unsafe { LocalFree(wide.cast()) };
        Ok(s)
    }

    pub fn owner_only_descriptor() -> io::Result<interprocess::os::windows::security_descriptor::SecurityDescriptor> {
        let sid = current_user_sid_string()?;
        let sddl = widestring::U16CString::from_str(format!("D:P(A;;GA;;;{sid})")).map_err(io::Error::other)?;
        interprocess::os::windows::security_descriptor::SecurityDescriptor::deserialize(&sddl)
    }

    /// Replace the DACL of `path` with one entry for the current user only and cut inheritance
    /// (`D:P(A;;FA;;;<sid>)`, `OICI` for a directory). Administrators and SYSTEM lose implicit read
    /// access to the file; the owner (this user) keeps full control.
    pub fn restrict_to_owner(path: &std::path::Path) -> io::Result<()> {
        use windows_sys::Win32::Security::Authorization::{ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1};
        use windows_sys::Win32::Security::{SetFileSecurityW, DACL_SECURITY_INFORMATION, PROTECTED_DACL_SECURITY_INFORMATION};
        let sid = current_user_sid_string()?;
        let flags = if path.is_dir() { "OICI" } else { "" };
        let sddl = widestring::U16CString::from_str(format!("D:P(A;{flags};FA;;;{sid})")).map_err(io::Error::other)?;
        let wpath = widestring::U16CString::from_os_str(path.as_os_str()).map_err(io::Error::other)?;
        let mut sd = null_mut();
        if unsafe { ConvertStringSecurityDescriptorToSecurityDescriptorW(sddl.as_ptr(), SDDL_REVISION_1, &mut sd, null_mut()) } == 0 {
            return Err(io::Error::last_os_error());
        }
        let ok = unsafe { SetFileSecurityW(wpath.as_ptr(), DACL_SECURITY_INFORMATION | PROTECTED_DACL_SECURITY_INFORMATION, sd) };
        let err = io::Error::last_os_error();
        unsafe { LocalFree(sd) };
        if ok == 0 {
            return Err(err);
        }
        Ok(())
    }

    pub fn pid_user_matches(pid: u32) -> Peer {
        let Ok(own) = own_token_user() else { return Peer::Unknown };
        let p = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) };
        if p.is_null() {
            return Peer::Unknown;
        }
        let p = Handle(p);
        let mut t: HANDLE = null_mut();
        if unsafe { OpenProcessToken(p.0, TOKEN_QUERY, &mut t) } == 0 {
            return Peer::Unknown;
        }
        let t = Handle(t);
        let Ok(theirs) = token_user(t.0) else { return Peer::Unknown };
        let (a, b) = unsafe { (&*(own.as_ptr() as *const TOKEN_USER), &*(theirs.as_ptr() as *const TOKEN_USER)) };
        if unsafe { EqualSid(a.User.Sid, b.User.Sid) } != 0 {
            Peer::SameUser
        } else {
            Peer::OtherUser
        }
    }
}

#[cfg(windows)]
pub use win::{current_user_sid_string, owner_only_descriptor};

/// Best effort: make `path` (a token, link or audit file) readable by the current user only.
/// Unix: mode 0600 (0700 for a directory). Windows: a protected DACL with one entry for the
/// current user (see `win::restrict_to_owner`). Returns the error so a caller can log it; callers
/// treat failure as non-fatal because the file already lives under the user profile.
pub fn restrict_to_owner(path: &std::path::Path) -> io::Result<()> {
    #[cfg(windows)]
    {
        win::restrict_to_owner(path)
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = if path.is_dir() { 0o700 } else { 0o600 };
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode))
    }
    #[cfg(not(any(windows, unix)))]
    {
        let _ = path;
        Ok(())
    }
}

/// Bytes that can be read from this accepted pipe right now without blocking (`PeekNamedPipe`).
/// `interprocess` cannot give a non-blocking read on Windows (an empty `PIPE_NOWAIT` pipe reads
/// as EOF), so the connection loop peeks first and only then does its ordinary blocking read.
/// An error means the other end is gone.
#[cfg(windows)]
pub fn pipe_bytes_available(stream: &interprocess::local_socket::Stream) -> io::Result<u32> {
    use std::os::windows::io::{AsHandle, AsRawHandle};
    use std::ptr::null_mut;
    use windows_sys::Win32::System::Pipes::PeekNamedPipe;
    let interprocess::local_socket::Stream::NamedPipe(p) = stream;
    let mut avail = 0u32;
    let ok = unsafe { PeekNamedPipe(p.as_handle().as_raw_handle(), null_mut(), 0, null_mut(), &mut avail, null_mut()) };
    if ok == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(avail)
}

/// Is the peer of this accepted connection running as the same user as the editor?
/// `own_dir` is the (user-owned) link directory, used on Unix as the reference owner.
pub fn peer_user_matches(creds: &interprocess::local_socket::PeerCreds, own_dir: &std::path::Path) -> Peer {
    #[cfg(windows)]
    {
        let _ = own_dir;
        match creds.pid() {
            Some(pid) => win::pid_user_matches(pid),
            None => Peer::Unknown,
        }
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        match (creds.euid(), std::fs::metadata(own_dir).map(|m| m.uid())) {
            (Some(peer), Ok(me)) => {
                if peer == me {
                    Peer::SameUser
                } else {
                    Peer::OtherUser
                }
            }
            _ => Peer::Unknown,
        }
    }
    #[cfg(not(any(windows, unix)))]
    {
        let _ = (creds, own_dir);
        Peer::Unknown
    }
}

/// Pid of the connecting process, when the OS tells.
pub fn peer_pid(creds: &interprocess::local_socket::PeerCreds) -> Option<u32> {
    creds.pid().map(|p| p as u32)
}

#[allow(dead_code)]
fn _unused(_: io::Error) {}

#[cfg(all(test, windows))]
mod tests {
    use super::*;

    #[test]
    fn own_sid_is_a_sid_string_and_the_descriptor_builds() {
        let sid = current_user_sid_string().unwrap();
        assert!(sid.starts_with("S-1-5-21-") || sid.starts_with("S-1-5-"), "{sid}");
        owner_only_descriptor().unwrap();
    }

    #[test]
    fn restrict_to_owner_leaves_the_file_usable_by_us_and_drops_everyone_else() {
        let d = tempfile::tempdir().unwrap();
        let f = d.path().join("secret.token");
        std::fs::write(&f, "t0k3n").unwrap();
        restrict_to_owner(&f).unwrap();
        // We (the owner) can still read, append and replace it.
        assert_eq!(std::fs::read_to_string(&f).unwrap(), "t0k3n");
        std::fs::OpenOptions::new().append(true).open(&f).unwrap();
        // The DACL is now protected with a single entry. `icacls` ships with Windows.
        let out = std::process::Command::new("icacls").arg(&f).output().unwrap();
        let text = String::from_utf8_lossy(&out.stdout).to_string();
        let sid = current_user_sid_string().unwrap();
        assert!(text.contains(&sid) || text.to_lowercase().contains(&std::env::var("USERNAME").unwrap_or_default().to_lowercase()), "{text}");
        for other in ["Everyone", "BUILTIN\\Users", "BUILTIN\\Administrators", "AUTORITE NT\\Utilisateurs", "NT AUTHORITY\\Authenticated Users", "NT AUTHORITY\\SYSTEM", "AUTORITE NT\\Système", "BUILTIN\\Administrateurs"] {
            assert!(!text.contains(other), "{other} still has access: {text}");
        }
        // Rotation (rename) keeps the restriction; deleting is allowed for the owner.
        std::fs::rename(&f, d.path().join("rotated.token")).unwrap();
        std::fs::remove_file(d.path().join("rotated.token")).unwrap();
    }

    #[test]
    fn our_own_pid_is_the_same_user_and_a_bogus_pid_is_unknown_not_a_match() {
        assert_eq!(win::pid_user_matches(std::process::id()), Peer::SameUser);
        assert_ne!(win::pid_user_matches(4_000_000_000), Peer::SameUser);
    }
}
