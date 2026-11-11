//! Changed host key: the one action behind the "the server's identity changed" stop dialog.
//!
//! The helper has no override for a changed key (a hard failure by design). Replacing the pin means
//! removing the old `known_hosts` entry with the OS `ssh-keygen -R`, which the editor only does after
//! the user typed the site name.

use std::path::PathBuf;

/// The `known_hosts` entry name `ssh-keygen -R` expects: the host, or `[host]:port` off port 22.
/// `None` when the host is not a plain name (it would be read as an option).
pub fn known_hosts_entry(host: &str, port: u16) -> Option<String> {
    let ok = !host.is_empty() && host.len() <= 255 && !host.starts_with('-') && host.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_' | ':'));
    ok.then(|| if port == 22 { host.to_string() } else { format!("[{host}]:{port}") })
}

/// Remove the pinned (old) identity of a server. Opens no connection.
pub fn forget_host_key(host: &str, port: u16) -> Result<(), String> {
    let entry = known_hosts_entry(host, port).ok_or_else(|| "the host name is not a plain name".to_string())?;
    let mut candidates: Vec<PathBuf> = vec![PathBuf::from("ssh-keygen")];
    #[cfg(windows)]
    if let Some(root) = std::env::var_os("SystemRoot") {
        candidates.insert(0, PathBuf::from(root).join("System32").join("OpenSSH").join("ssh-keygen.exe"));
    }
    for exe in candidates {
        let mut cmd = std::process::Command::new(&exe);
        cmd.args(["-R", &entry]).stdin(std::process::Stdio::null()).stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null());
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
        }
        match cmd.status() {
            Ok(s) if s.success() => return Ok(()),
            Ok(_) => return Err("ssh-keygen could not remove it (it may not be listed)".into()),
            Err(_) => continue,
        }
    }
    Err("the OpenSSH tools were not found".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_hosts_entry_is_plain_and_port_aware() {
        assert_eq!(known_hosts_entry("vps.example.org", 22).as_deref(), Some("vps.example.org"));
        assert_eq!(known_hosts_entry("vps.example.org", 2222).as_deref(), Some("[vps.example.org]:2222"));
        for bad in ["", "-oProxyCommand=x", "a b", "a;b", "a\"b", "../x"] {
            assert!(known_hosts_entry(bad, 22).is_none(), "{bad:?}");
        }
    }
}
