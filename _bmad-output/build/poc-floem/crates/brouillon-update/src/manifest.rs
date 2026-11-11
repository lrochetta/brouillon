//! Manifest: signature check on the exact bytes, then strict field validation.

use crate::keys::{unhex, TrustedKey};
use crate::timeutil::parse_utc;
use crate::{UpdateError, ALLOWED_HOSTS, MAX_INSTALLER_BYTES};
use ed25519_dalek::{Signature, VerifyingKey};
use serde::Deserialize;

pub const SIG_TAG: &str = "brouillon-sig-v1";

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct Installer {
    pub file: String,
    pub url: String,
    pub size_bytes: u64,
    pub sha256: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct Manifest {
    pub schema: u32,
    #[serde(default)]
    pub channel: String,
    pub version: String,
    pub build: u64,
    pub build_date_utc: String,
    pub published_utc: String,
    #[serde(default)]
    pub min_supported_build: u64,
    pub installer: Installer,
    #[serde(default)]
    pub notes_url: String,
    #[serde(default)]
    pub notes: String,
}

/// Where URLs may point. `dev` allows plain HTTP on loopback and any host equal to the manifest host.
#[derive(Debug, Clone)]
pub struct UrlPolicy {
    pub dev: bool,
    pub manifest_host: String,
    pub allowed_hosts: Vec<String>,
}

impl UrlPolicy {
    pub fn production(manifest_host: &str) -> Self {
        Self { dev: false, manifest_host: manifest_host.to_ascii_lowercase(), allowed_hosts: ALLOWED_HOSTS.iter().map(|s| s.to_string()).collect() }
    }
}

pub fn is_loopback(host: &str) -> bool {
    matches!(host, "127.0.0.1" | "localhost" | "[::1]" | "::1")
}

/// (scheme, lowercase host, port) of an absolute URL; userinfo is refused.
pub fn split_url(url: &str) -> Option<(String, String, Option<u16>)> {
    let uri: ureq::http::Uri = url.parse().ok()?;
    let scheme = uri.scheme_str()?.to_ascii_lowercase();
    let auth = uri.authority()?;
    if auth.as_str().contains('@') {
        return None;
    }
    Some((scheme, uri.host()?.to_ascii_lowercase(), auth.port_u16()))
}

/// URL gate used for the manifest, the signature, every redirect and the installer.
pub fn check_url(url: &str, p: &UrlPolicy) -> Result<(), UpdateError> {
    let bad = || UpdateError::InsecureUrl(url.chars().take(120).collect());
    let (scheme, host, port) = split_url(url).ok_or_else(bad)?;
    if p.dev && is_loopback(&host) {
        return if scheme == "http" || scheme == "https" { Ok(()) } else { Err(bad()) };
    }
    if scheme != "https" || !matches!(port, None | Some(443)) {
        return Err(bad());
    }
    let ok = host == p.manifest_host || p.allowed_hosts.iter().any(|h| h.eq_ignore_ascii_case(&host));
    if ok {
        Ok(())
    } else {
        Err(bad())
    }
}

/// Parse `brouillon-sig-v1 <key_id> <128 hex>` and verify it over `data`.
pub fn verify_signature(data: &[u8], sig_text: &str, keys: &[TrustedKey]) -> Result<(), UpdateError> {
    let parts: Vec<&str> = sig_text.split_whitespace().collect();
    if parts.len() != 3 || parts[0] != SIG_TAG {
        return Err(UpdateError::SigMalformed);
    }
    let sig: [u8; 64] = unhex(parts[2]).and_then(|v| v.try_into().ok()).ok_or(UpdateError::SigMalformed)?;
    let key = keys.iter().find(|k| k.key_id == parts[1]).ok_or_else(|| UpdateError::UnknownKey(parts[1].chars().take(32).collect()))?;
    let vk = VerifyingKey::from_bytes(&key.public).map_err(|_| UpdateError::BadSignature)?;
    vk.verify_strict(data, &Signature::from_bytes(&sig)).map_err(|_| UpdateError::BadSignature)
}

/// Windows-safe bare file name for the downloaded installer.
pub fn check_file_name(name: &str) -> Result<(), UpdateError> {
    let bad = || UpdateError::BadFileName(name.chars().take(80).collect());
    let ok_chars = !name.is_empty()
        && name.len() <= 120
        && name.is_ascii()
        && name.bytes().all(|c| c.is_ascii_alphanumeric() || matches!(c, b'.' | b'_' | b'-'))
        && name.as_bytes()[0].is_ascii_alphanumeric();
    if !ok_chars || !name.to_ascii_lowercase().ends_with(".exe") || name.contains("..") {
        return Err(bad());
    }
    let stem = name[..name.len() - 4].split('.').next().unwrap_or("").to_ascii_uppercase();
    let reserved = ["CON", "PRN", "AUX", "NUL", "CLOCK$"];
    let is_com_lpt = (stem.starts_with("COM") || stem.starts_with("LPT")) && stem.len() == 4 && stem.as_bytes()[3].is_ascii_digit();
    if reserved.contains(&stem.as_str()) || is_com_lpt {
        return Err(bad());
    }
    Ok(())
}

pub fn validate(m: &Manifest, p: &UrlPolicy) -> Result<(), UpdateError> {
    let bm = |s: &str| UpdateError::BadManifest(s.to_string());
    if m.schema != 1 {
        return Err(bm("unsupported schema"));
    }
    if m.version.is_empty() || m.version.len() > 64 || !m.version.bytes().all(|c| c.is_ascii_alphanumeric() || matches!(c, b'.' | b'-' | b'+')) {
        return Err(bm("bad version"));
    }
    if m.build == 0 {
        return Err(bm("bad build"));
    }
    if parse_utc(&m.build_date_utc).is_none() || parse_utc(&m.published_utc).is_none() {
        return Err(bm("bad date"));
    }
    let i = &m.installer;
    check_file_name(&i.file)?;
    if i.sha256.len() != 64 || !i.sha256.bytes().all(|c| c.is_ascii_hexdigit()) {
        return Err(bm("bad sha256"));
    }
    if i.size_bytes == 0 || i.size_bytes > MAX_INSTALLER_BYTES {
        return Err(bm("installer size out of range"));
    }
    check_url(&i.url, p)?;
    let path = i.url.split(['?', '#']).next().unwrap_or("");
    if !path.ends_with(&format!("/{}", i.file)) {
        return Err(bm("installer url does not end with the file name"));
    }
    if !m.notes_url.is_empty() && !m.notes_url.starts_with("https://") {
        return Err(bm("notes_url must be https"));
    }
    Ok(())
}

/// Signature first, then parse + validate. `data` are the exact downloaded bytes.
pub fn parse_verified(data: &[u8], sig_text: &str, keys: &[TrustedKey], p: &UrlPolicy) -> Result<Manifest, UpdateError> {
    verify_signature(data, sig_text, keys)?;
    let m: Manifest = serde_json::from_slice(data).map_err(|e| UpdateError::BadManifest(e.to_string()))?;
    validate(&m, p)?;
    Ok(m)
}

#[cfg(test)]
pub(crate) mod testkit {
    use super::*;
    use crate::keys::{hex, key_id_of};
    use ed25519_dalek::{Signer, SigningKey};

    pub fn signing_key(n: u8) -> SigningKey {
        SigningKey::from_bytes(&[n; 32])
    }
    pub fn trusted(sk: &SigningKey) -> TrustedKey {
        let public = sk.verifying_key().to_bytes();
        TrustedKey { key_id: key_id_of(&public), public }
    }
    pub fn sign(sk: &SigningKey, data: &[u8]) -> String {
        format!("{SIG_TAG} {} {}\n", trusted(sk).key_id, hex(&sk.sign(data).to_bytes()))
    }
    pub fn manifest_json(build: u64, published: &str, url: &str, file: &str, size: u64, sha: &str, min: u64) -> String {
        format!(
            r#"{{"schema":1,"channel":"alpha-test","version":"0.9.0","build":{build},"build_date_utc":"2026-11-05T14:32:10Z","published_utc":"{published}","min_supported_build":{min},"installer":{{"file":"{file}","url":"{url}","size_bytes":{size},"sha256":"{sha}"}},"notes_url":"https://brouillon.rochetta.fr/changelog/","notes":""}}"#
        )
    }
}

#[cfg(test)]
mod tests {
    use super::testkit::*;
    use super::*;

    const SHA: &str = "0000000000000000000000000000000000000000000000000000000000000000";
    fn prod() -> UrlPolicy {
        UrlPolicy::production("brouillon.rochetta.fr")
    }
    fn good(file: &str) -> String {
        manifest_json(90, "2026-11-05T14:32:10Z", &format!("https://brouillon.rochetta.fr/downloads/{file}"), file, 1000, SHA, 0)
    }

    #[test]
    fn valid_signature_and_manifest_parse() {
        let sk = signing_key(1);
        let j = good("B-setup.exe");
        let m = parse_verified(j.as_bytes(), &sign(&sk, j.as_bytes()), &[trusted(&sk)], &prod()).unwrap();
        assert_eq!(m.build, 90);
    }

    #[test]
    fn tampered_bytes_fail_signature() {
        let sk = signing_key(1);
        let j = good("B-setup.exe");
        let sig = sign(&sk, j.as_bytes());
        let t = j.replace("\"build\":90", "\"build\":91");
        assert_eq!(parse_verified(t.as_bytes(), &sig, &[trusted(&sk)], &prod()), Err(UpdateError::BadSignature));
    }

    #[test]
    fn unknown_key_and_wrong_key_are_refused() {
        let (a, b) = (signing_key(1), signing_key(2));
        let j = good("B-setup.exe");
        let sig_b = sign(&b, j.as_bytes());
        assert!(matches!(parse_verified(j.as_bytes(), &sig_b, &[trusted(&a)], &prod()), Err(UpdateError::UnknownKey(_))));
        // same key_id claimed but signed by another key
        let forged = sig_b.replace(&trusted(&b).key_id, &trusted(&a).key_id);
        assert_eq!(parse_verified(j.as_bytes(), &forged, &[trusted(&a)], &prod()), Err(UpdateError::BadSignature));
    }

    #[test]
    fn malformed_signature_files() {
        let sk = signing_key(1);
        for s in ["", "garbage", "brouillon-sig-v2 a b", "brouillon-sig-v1 only-two"] {
            assert_eq!(verify_signature(b"x", s, &[trusted(&sk)]), Err(UpdateError::SigMalformed), "{s}");
        }
    }

    #[test]
    fn malicious_file_names_refused() {
        for n in ["..\\evil.exe", "../evil.exe", "a/b.exe", "C:evil.exe", "evil.exe ", "evil.exe:stream", "con.exe", "NUL.exe", "com1.exe", "LPT9.exe", "evil.bat", "evil", "", ".exe", "-x.exe", "é.exe", "a..b.exe"] {
            assert!(check_file_name(n).is_err(), "{n:?} should be refused");
        }
        for n in ["Brouillon-0.1.0-alpha.13-build84-win-x64-setup.exe", "setup.exe"] {
            assert!(check_file_name(n).is_ok(), "{n}");
        }
    }

    #[test]
    fn urls_http_foreign_host_userinfo_and_port_refused() {
        let p = prod();
        for u in [
            "http://brouillon.rochetta.fr/downloads/a.exe",
            "https://evil.example/downloads/a.exe",
            "https://brouillon.rochetta.fr@evil.example/a.exe",
            "https://brouillon.rochetta.fr:8443/a.exe",
            "ftp://brouillon.rochetta.fr/a.exe",
            "http://127.0.0.1:1/a.exe",
            "not a url",
        ] {
            assert!(check_url(u, &p).is_err(), "{u}");
        }
        assert!(check_url("https://brouillon.rochetta.fr/downloads/a.exe", &p).is_ok());
    }

    #[test]
    fn dev_mode_allows_loopback_http_only() {
        let mut p = prod();
        p.dev = true;
        assert!(check_url("http://127.0.0.1:8080/a.exe", &p).is_ok());
        assert!(check_url("http://evil.example/a.exe", &p).is_err());
    }

    #[test]
    fn field_validation() {
        let p = prod();
        let ok = |j: String| serde_json::from_str::<Manifest>(&j).unwrap();
        let url = "https://brouillon.rochetta.fr/downloads/a.exe";
        assert!(validate(&ok(manifest_json(5, "2026-11-05T14:32:10Z", url, "a.exe", 10, SHA, 0)), &p).is_ok());
        assert!(validate(&ok(manifest_json(0, "2026-11-05T14:32:10Z", url, "a.exe", 10, SHA, 0)), &p).is_err()); // build 0
        assert!(validate(&ok(manifest_json(5, "yesterday", url, "a.exe", 10, SHA, 0)), &p).is_err());
        assert!(validate(&ok(manifest_json(5, "2026-11-05T14:32:10Z", url, "a.exe", 10, "abc", 0)), &p).is_err());
        assert!(validate(&ok(manifest_json(5, "2026-11-05T14:32:10Z", url, "a.exe", 0, SHA, 0)), &p).is_err());
        assert!(validate(&ok(manifest_json(5, "2026-11-05T14:32:10Z", url, "a.exe", MAX_INSTALLER_BYTES + 1, SHA, 0)), &p).is_err()); // excessive size
        assert!(validate(&ok(manifest_json(5, "2026-11-05T14:32:10Z", "https://brouillon.rochetta.fr/downloads/other.exe", "a.exe", 10, SHA, 0)), &p).is_err()); // url/file mismatch
        let mut m = ok(manifest_json(5, "2026-11-05T14:32:10Z", url, "a.exe", 10, SHA, 0));
        m.schema = 2;
        assert!(validate(&m, &p).is_err());
    }
}
