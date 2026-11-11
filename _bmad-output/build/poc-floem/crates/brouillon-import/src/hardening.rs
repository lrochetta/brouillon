//! Importer hardening: explicit secret-key rules, credential-looking VALUES, size caps and safe
//! report labels. The importer reads another program's folder, so its input is hostile.
//!
//! * Keys are matched by explicit rules on the normalised name (exact / prefix / suffix), not by
//!   a substring: `Pw`, `FtpPass`, `Remote Password`, `Community` are caught, `Compass` and
//!   `Passthrough` are not.
//! * Values are scanned too: `Argument=-p hunter2`, `sshpass`, `--token=...`, `://user:pass@host`
//!   and anything `brouillon-secrets` flags. The value is dropped, the report names the key only.
//! * `reg_u.ini` and `ftp_u.ini` are never opened by the generic importer. For a future,
//!   dedicated sites importer, [`ftp_profile_metadata`] turns the text of an FTP profile file into
//!   host/port/user/dir records; a password has nowhere to go in those records.

use brouillon_secrets::Severity;
use std::path::Path;

/// A source file larger than this is not read.
pub const MAX_FILE_BYTES: u64 = 4 * 1024 * 1024;
/// At most this many files of a folder are considered.
pub const MAX_FILES: usize = 2000;
/// Longer lines are ignored (binary blobs, minified data).
pub const MAX_LINE: usize = 16 * 1024;
/// At most this many `key=value` entries per file.
pub const MAX_ENTRIES: usize = 50_000;

const EXACT: [&str; 17] = [
    "pw", "pass", "pwd", "auth", "cred", "secret", "token", "login", "licence", "license", "serial", "regcode", "regname", "regkey", "community", "psk", "otp",
];
const PREFIX: [&str; 18] = [
    "password", "passwd", "passphrase", "passcode", "pwd", "secret", "token", "apikey", "privatekey", "accesskey", "secretkey", "authtoken", "authkey", "authpass",
    "credential", "login", "licen", "serial",
];
const SUFFIX: [&str; 18] = [
    "password", "passwd", "passphrase", "passcode", "pwd", "secret", "token", "apikey", "privatekey", "secretkey", "accesskey", "credential", "credentials",
    "licensekey", "licencekey", "regcode", "serialnumber", "serialno",
];
/// Words that end in `pass` and are not passwords.
const PASS_WORDS: [&str; 7] = ["bypass", "compass", "overpass", "underpass", "trespass", "surpass", "encompass"];

fn norm(s: &str) -> String {
    s.chars().filter(|c| c.is_alphanumeric()).flat_map(char::to_lowercase).collect()
}

/// Is this INI key a credential name? Explicit rules on the normalised key.
pub fn is_secret_key(key: &str) -> bool {
    let k = norm(key);
    if k.is_empty() {
        return false;
    }
    if EXACT.contains(&k.as_str()) || PREFIX.iter().any(|p| k.starts_with(p)) || SUFFIX.iter().any(|s| k.ends_with(s)) {
        return true;
    }
    // `FtpPass`, `RemotePass`: ends in "pass" unless it is an ordinary word like "bypass".
    k.ends_with("pass") && !PASS_WORDS.iter().any(|w| k.ends_with(w))
}

fn is_command_key(k: &str) -> bool {
    matches!(norm(k).as_str(), "argument" | "arguments" | "args" | "param" | "parameters" | "command" | "cmd" | "commandline")
}

/// Flags that take a password/token as the next word, or `flag=value` forms.
const SECRET_FLAGS: [&str; 13] = ["-p", "-pw", "-pass", "--pass", "--password", "--passwd", "--pwd", "--token", "--secret", "--api-key", "--apikey", "--auth", "--passphrase"];
const SECRET_FLAG_EQ: [&str; 10] = ["--password=", "--pass=", "--passwd=", "--pwd=", "--token=", "--secret=", "--api-key=", "--apikey=", "-p=", "--passphrase="];

fn command_line_has_secret(v: &str) -> bool {
    let words: Vec<String> = v.split_whitespace().map(|w| w.trim_matches(|c| c == '"' || c == '\'').to_lowercase()).collect();
    let mysqlish = words.iter().any(|w| w.contains("mysql") || w.contains("mariadb"));
    words.iter().enumerate().any(|(i, w)| {
        SECRET_FLAG_EQ.iter().any(|f| w.starts_with(f))
            || (SECRET_FLAGS.contains(&w.as_str()) && i + 1 < words.len())
            // `mysql -phunter2`: the password is glued to the flag
            || (mysqlish && w.starts_with("-p") && w.len() > 2 && !w.starts_with("--"))
    })
}

/// Does this value (of `key`) look like it carries a credential? Such a value is never copied.
pub fn value_has_credential(key: &str, value: &str) -> bool {
    let v = value.trim();
    if v.is_empty() {
        return false;
    }
    // sshpass can sit in any value (a shell one-liner in a tool's command)
    if v.to_ascii_lowercase().contains("sshpass") {
        return true;
    }
    if is_command_key(key) && command_line_has_secret(v) {
        return true;
    }
    brouillon_secrets::scan(&format!("{key}={v}")).count_at_least(Severity::Medium) > 0
}

/// A key or section name safe to print in the report: no control characters, no credential
/// shapes, at most 48 characters.
pub fn safe_label(s: &str) -> String {
    safe_label_n(s, 48)
}

/// [`safe_label`] with another length bound.
pub fn safe_label_n(s: &str, max: usize) -> String {
    let clean: String = s.chars().map(|c| if c.is_control() { ' ' } else { c }).collect();
    let red = brouillon_secrets::redact_text(&clean);
    if red.chars().count() > max {
        format!("{}...", red.chars().take(max).collect::<String>())
    } else {
        red
    }
}

/// A section name that itself looks like a credential (hostile file): its entries are not read.
pub fn section_is_credential(section: &str) -> bool {
    brouillon_secrets::redact_text(section) != section
}

pub fn too_large(path: &Path) -> bool {
    std::fs::metadata(path).map(|m| m.len() > MAX_FILE_BYTES).unwrap_or(false)
}

/// What a sites importer may keep of an FTP profile. There is deliberately no password field.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct FtpProfileMeta {
    pub name: String,
    pub host: String,
    pub port: Option<u16>,
    pub user: String,
    pub dir: String,
}

/// Metadata of the FTP profiles in the text of an EditPlus-style `ftp*.ini`: keys such as
/// `Description2`, `Server2`/`Host2`, `User2`, `Port2`, `InitialDir2`, grouped by their number.
/// Every other key (passwords included) is ignored. Pure function on text: it opens nothing.
pub fn ftp_profile_metadata(text: &str) -> Vec<FtpProfileMeta> {
    let mut by_index: std::collections::BTreeMap<(String, u32), FtpProfileMeta> = Default::default();
    let mut section = String::new();
    for line in text.lines().take(MAX_ENTRIES).filter(|l| l.len() <= MAX_LINE) {
        let line = line.trim().trim_start_matches('\u{feff}');
        if let Some(s) = line.strip_prefix('[').and_then(|l| l.strip_suffix(']')) {
            section = s.trim().to_string();
            continue;
        }
        let Some((k, v)) = line.split_once('=') else { continue };
        let nk = norm(k);
        let stem = nk.trim_end_matches(|c: char| c.is_ascii_digit());
        let idx: u32 = nk[stem.len()..].parse().unwrap_or(0);
        let v = v.trim();
        let entry = by_index.entry((section.clone(), idx)).or_default();
        match stem {
            "description" | "name" | "title" | "label" => entry.name = v.chars().take(64).collect(),
            "server" | "host" | "hostname" | "address" => entry.host = host_only(v),
            "user" | "username" | "userid" => entry.user = v.chars().take(64).collect(),
            "port" => entry.port = v.parse().ok(),
            "initialdir" | "dir" | "remotedir" | "path" => entry.dir = v.chars().take(256).collect(),
            _ => {} // passwords, keys, anything else: ignored
        }
    }
    by_index.into_values().filter(|p| !p.host.is_empty()).collect()
}

/// `user:pass@host:21/dir`, `ftp://user:pass@host` -> `host` (no credentials, scheme, port or path).
fn host_only(s: &str) -> String {
    let s = s.split("://").last().unwrap_or(s);
    let s = s.rsplit('@').next().unwrap_or(s);
    let s = s.split(['/', ' ']).next().unwrap_or("");
    let s = if s.starts_with('[') { s } else { s.split(':').next().unwrap_or("") };
    s.chars().take(253).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn key_rules_are_explicit_not_substring() {
        for k in [
            "Password", "Remote Password", "FtpPass", "Pwd", "PW", "Pw", "Passwd", "pass", "Secret", "ApiKey", "api_key", "AuthToken", "AccessKey", "Community", "LoginName",
            "RegCode", "License", "Serial", "PrivateKey", "db_password", "ProxyPass", "Credentials",
        ] {
            assert!(is_secret_key(k), "{k} must be a secret key");
        }
        for k in ["Compass", "Bypass", "Passthrough", "PassCount", "Command", "Argument", "Dir", "Capture", "Menu", "Key", "Spaces", "Keyword", "Font"] {
            assert!(!is_secret_key(k), "{k} is not a secret key");
        }
    }

    #[test]
    fn embedded_credentials_in_values_are_caught() {
        for (k, v) in [
            ("Argument", "-p hunter2"),
            ("Argument", "--password=hunter2 --user bob"),
            ("Argument", "--token hunter2"),
            ("Command", "sshpass -p hunter2 ssh host"),
            ("Command", "C:\\tools\\sshpass.exe -f x ssh h"),
            ("Argument", "https://bob:hunter2x@example.org/repo.git"),
            ("Command", "mysql -u root -phunter2 mydb"),
            ("Menu", "run sshpass"),
            ("Dir", "postgres://app:Zq8vLm2xPw@db/x"),
        ] {
            assert!(value_has_credential(k, v), "{k}={v}");
        }
        for (k, v) in [("Argument", "-j4 --verbose"), ("Command", "make"), ("Argument", "-print0 -name *.rs"), ("Dir", "C:\\proj"), ("Argument", "--port 8080"), ("Menu", "&Build")] {
            assert!(!value_has_credential(k, v), "{k}={v}");
        }
    }

    #[test]
    fn labels_are_truncated_and_scrubbed() {
        let l = safe_label(&format!("key\u{0}ghp_{}", "Zq8vLm2xPw4rT6yU9iO1aS3dF5gH7jK0lN2b"));
        assert!(!l.contains("Zq8v") && !l.contains('\0'));
        assert!(safe_label(&"x".repeat(500)).chars().count() <= 51);
    }

    #[test]
    fn ftp_metadata_has_no_place_for_a_password() {
        let text = "[FTP]\nDescription1=Blog\nServer1=ftp://bob:hunter2@blog.example.org:21/pub\nUser1=bob\nPassword1=hunter2\nPort1=2121\nInitialDir1=/www\nDescription2=Other\nHost2=o.example.net\nUser2=amy\nPwd2=zzz\n";
        let m = ftp_profile_metadata(text);
        assert_eq!(m.len(), 2);
        assert_eq!(m[0], FtpProfileMeta { name: "Blog".into(), host: "blog.example.org".into(), port: Some(2121), user: "bob".into(), dir: "/www".into() });
        let dump = format!("{m:?}");
        assert!(!dump.contains("hunter2") && !dump.contains("zzz"));
    }
}
