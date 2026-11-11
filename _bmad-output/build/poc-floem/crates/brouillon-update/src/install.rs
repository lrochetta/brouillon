//! Installer verification and launch. Nothing here runs a file that was not verified.

use crate::manifest::Installer;
use crate::UpdateError;
use sha2::{Digest, Sha256};
use std::io::Read;
use std::path::Path;

/// Inno Setup, silent, per user. `/RELAUNCH=1` is read by the `[Run]` entry of brouillon.iss that reopens the app
/// (Restart Manager only restarts programs that registered for it, Brouillon does not). `/CURRENTUSER` plus `PrivilegesRequired=lowest`: no elevation prompt.
pub const INSTALL_ARGS: [&str; 6] = ["/VERYSILENT", "/SUPPRESSMSGBOXES", "/NORESTART", "/CLOSEAPPLICATIONS", "/CURRENTUSER", "/RELAUNCH=1"];

/// (SHA-256 hex, size) streamed, so a 200 MB file never sits in memory.
pub fn sha256_file(path: &Path) -> std::io::Result<(String, u64)> {
    let mut f = std::fs::File::open(path)?;
    let mut h = Sha256::new();
    let mut buf = vec![0u8; 256 * 1024];
    let mut total = 0u64;
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        h.update(&buf[..n]);
        total += n as u64;
    }
    Ok((crate::keys::hex(&h.finalize()), total))
}

/// `MZ` header and a `PE\0\0` signature at `e_lfanew`: plausibly a Windows executable.
pub fn check_pe(path: &Path) -> Result<(), UpdateError> {
    let mut f = std::fs::File::open(path)?;
    let mut head = [0u8; 64];
    f.read_exact(&mut head).map_err(|_| UpdateError::NotPe)?;
    if &head[..2] != b"MZ" {
        return Err(UpdateError::NotPe);
    }
    let off = u32::from_le_bytes([head[60], head[61], head[62], head[63]]) as u64;
    if off < 64 || off > 1 << 20 {
        return Err(UpdateError::NotPe);
    }
    use std::io::{Seek, SeekFrom};
    f.seek(SeekFrom::Start(off))?;
    let mut sig = [0u8; 4];
    f.read_exact(&mut sig).map_err(|_| UpdateError::NotPe)?;
    if &sig != b"PE\0\0" {
        return Err(UpdateError::NotPe);
    }
    Ok(())
}

/// Size, SHA-256 and PE header against the (already signature-verified) manifest.
pub fn verify_installer(path: &Path, want: &Installer) -> Result<(), UpdateError> {
    let (sha, size) = sha256_file(path)?;
    if size != want.size_bytes {
        return Err(UpdateError::SizeMismatch);
    }
    if !sha.eq_ignore_ascii_case(&want.sha256) {
        return Err(UpdateError::HashMismatch);
    }
    check_pe(path)
}

/// The command that runs the installer: never elevated (a manifest asking for admin makes
/// CreateProcess fail with error 740, no UAC prompt is ever raised), and with TMP/TEMP pointing at
/// `tmp_dir` (a folder of the app data): Inno extracts its stub into TMP, and some machines forbid
/// executing files from the system temp folder to a standard-user process.
pub fn installer_command(path: &Path, tmp_dir: &Path) -> std::process::Command {
    let mut cmd = std::process::Command::new(path);
    cmd.args(INSTALL_ARGS)
        .env("TMP", tmp_dir)
        .env("TEMP", tmp_dir)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const DETACHED_PROCESS: u32 = 0x0000_0008;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(DETACHED_PROCESS | CREATE_NO_WINDOW);
    }
    cmd
}

/// Start the installer detached (it survives our exit). Call only after `verify_installer`.
pub fn launch_installer(path: &Path, tmp_dir: &Path) -> std::io::Result<std::process::Child> {
    std::fs::create_dir_all(tmp_dir)?;
    installer_command(path, tmp_dir).spawn()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EarlyState {
    /// Still running: it is installing (it will close this program itself).
    Running,
    /// Exited at once with success while we are still alive: nothing was applied.
    ExitedOk,
    /// Exited with an error code (or was killed): the install did not happen.
    Failed,
}

/// Wait up to `window` for an early exit. A healthy Inno setup keeps running while it closes us.
pub fn watch_early(child: &mut std::process::Child, window: std::time::Duration) -> EarlyState {
    let t0 = std::time::Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(st)) => return classify_exit(st.code()),
            Ok(None) if t0.elapsed() >= window => return EarlyState::Running,
            Ok(None) => std::thread::sleep(std::time::Duration::from_millis(100)),
            Err(_) => return EarlyState::Failed,
        }
    }
}

pub fn classify_exit(code: Option<i32>) -> EarlyState {
    match code {
        Some(0) => EarlyState::ExitedOk,
        _ => EarlyState::Failed,
    }
}

/// Portable copy: the `brouillon.portable` marker sits beside the exe. Such a copy never runs an installer.
pub fn is_portable(exe: &Path) -> bool {
    exe.parent().map_or(false, |d| d.join("brouillon.portable").exists())
}

/// True for a copy put there by the installer: Windows, `unins000.exe` beside the exe, no portable marker.
pub fn is_installed_copy(exe: &Path) -> bool {
    if !cfg!(windows) {
        return false;
    }
    let Some(dir) = exe.parent() else { return false };
    dir.join("unins000.exe").is_file() && !dir.join("brouillon.portable").exists()
}

/// Remove everything in the updates folder except `keep`.
pub fn clean_dir(dir: &Path, keep: Option<&str>) {
    if let Ok(rd) = std::fs::read_dir(dir) {
        for e in rd.flatten() {
            if keep.map_or(true, |k| e.file_name().to_string_lossy() != k) && e.path().is_file() {
                let _ = std::fs::remove_file(e.path());
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pe_bytes() -> Vec<u8> {
        let mut v = vec![0u8; 256];
        v[0] = b'M';
        v[1] = b'Z';
        v[60] = 128;
        v[128..132].copy_from_slice(b"PE\0\0");
        v
    }

    #[test]
    fn pe_header_check() {
        let d = tempfile::tempdir().unwrap();
        let ok = d.path().join("ok.exe");
        std::fs::write(&ok, pe_bytes()).unwrap();
        assert!(check_pe(&ok).is_ok());
        for (name, bytes) in [("text.exe", b"#!/bin/sh\necho hi\n".repeat(10)), ("short.exe", b"MZ".to_vec()), ("nope.exe", vec![0u8; 300])] {
            let p = d.path().join(name);
            std::fs::write(&p, bytes).unwrap();
            assert_eq!(check_pe(&p), Err(UpdateError::NotPe), "{name}");
        }
    }

    #[test]
    fn verify_installer_size_hash_pe() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("a.exe");
        let bytes = pe_bytes();
        std::fs::write(&p, &bytes).unwrap();
        let (sha, size) = sha256_file(&p).unwrap();
        let mk = |sha: &str, size: u64| Installer { file: "a.exe".into(), url: "https://x/a.exe".into(), size_bytes: size, sha256: sha.into() };
        assert!(verify_installer(&p, &mk(&sha, size)).is_ok());
        assert_eq!(verify_installer(&p, &mk(&sha, size + 1)), Err(UpdateError::SizeMismatch));
        assert_eq!(verify_installer(&p, &mk(&"0".repeat(64), size)), Err(UpdateError::HashMismatch));
        // right hash, right size, but not an executable
        let t = d.path().join("t.exe");
        std::fs::write(&t, b"plain text, not an executable at all ........................................................").unwrap();
        let (sha2, size2) = sha256_file(&t).unwrap();
        assert_eq!(verify_installer(&t, &mk(&sha2, size2)), Err(UpdateError::NotPe));
    }

    #[test]
    fn installer_gets_tmp_and_temp_inside_the_data_folder_and_is_never_elevated() {
        let d = tempfile::tempdir().unwrap();
        let tmp = d.path().join("updates").join("tmp");
        let cmd = installer_command(Path::new("x-setup.exe"), &tmp);
        let envs: std::collections::HashMap<_, _> = cmd.get_envs().map(|(k, v)| (k.to_string_lossy().into_owned(), v.map(|v| v.to_owned()))).collect();
        assert_eq!(envs["TMP"].as_deref(), Some(tmp.as_os_str()));
        assert_eq!(envs["TEMP"].as_deref(), Some(tmp.as_os_str()));
        let args: Vec<_> = cmd.get_args().map(|a| a.to_string_lossy().into_owned()).collect();
        assert!(args.contains(&"/CURRENTUSER".to_string()) && args.contains(&"/VERYSILENT".to_string()));
        // plain CreateProcess (no runas / ShellExecute): Windows never raises a UAC prompt
        assert_eq!(cmd.get_program(), "x-setup.exe");
    }

    #[test]
    fn early_exit_classification() {
        assert_eq!(classify_exit(Some(0)), EarlyState::ExitedOk);
        assert_eq!(classify_exit(Some(5)), EarlyState::Failed);
        assert_eq!(classify_exit(None), EarlyState::Failed);
    }

    #[test]
    fn installed_copy_detection() {
        let d = tempfile::tempdir().unwrap();
        let exe = d.path().join("brouillon.exe");
        assert!(!is_installed_copy(&exe));
        std::fs::write(d.path().join("unins000.exe"), b"x").unwrap();
        assert_eq!(is_installed_copy(&exe), cfg!(windows));
        std::fs::write(d.path().join("brouillon.portable"), b"").unwrap();
        assert!(!is_installed_copy(&exe), "portable marker wins");
    }
}
