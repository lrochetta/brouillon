//! Brouillon release tool: `keygen`, `sign`, `verify`.
//!
//! The private key (32-byte seed, hex) lives in `%USERPROFILE%\.brouillon-release\update-signing.key`
//! and is never printed. Signature file: `brouillon-sig-v1 <key_id> <128 hex>\n`.
//! `key_id` = first 8 bytes of SHA-256(public key), hex.

use ed25519_dalek::{Signature, Signer, SigningKey, Verifier, VerifyingKey};
use rand_core::{OsRng, RngCore};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

const SIG_TAG: &str = "brouillon-sig-v1";

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}
fn unhex(s: &str) -> Result<Vec<u8>, String> {
    let s = s.trim();
    if s.len() % 2 != 0 || !s.is_ascii() {
        return Err("bad hex".into());
    }
    (0..s.len()).step_by(2).map(|i| u8::from_str_radix(&s[i..i + 2], 16).map_err(|_| "bad hex".to_string())).collect()
}
fn key_id(public: &[u8; 32]) -> String {
    hex(&Sha256::digest(public)[..8])
}

fn default_key_path() -> Result<PathBuf, String> {
    let home = std::env::var_os("USERPROFILE").or_else(|| std::env::var_os("HOME")).ok_or("no USERPROFILE/HOME")?;
    Ok(PathBuf::from(home).join(".brouillon-release").join("update-signing.key"))
}

/// Restrict `dir` to the current user (Windows: icacls; Unix: chmod 700).
fn restrict_dir(dir: &Path) -> Result<(), String> {
    #[cfg(windows)]
    {
        let user = std::env::var("USERNAME").map_err(|_| "no USERNAME")?;
        let st = std::process::Command::new("icacls")
            .arg(dir)
            .args(["/inheritance:r", "/grant:r"])
            .arg(format!("{user}:(OI)(CI)F"))
            .output()
            .map_err(|e| format!("icacls: {e}"))?;
        if !st.status.success() {
            return Err("icacls failed to restrict the key directory".into());
        }
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700)).map_err(|e| e.to_string())?;
    }
    Ok(())
}

fn load_key(path: &Path) -> Result<SigningKey, String> {
    let txt = std::fs::read_to_string(path).map_err(|e| format!("cannot read key file {}: {e}", path.display()))?;
    let seed = unhex(&txt)?;
    let seed: [u8; 32] = seed.try_into().map_err(|_| "key file is not a 32-byte seed".to_string())?;
    Ok(SigningKey::from_bytes(&seed))
}

fn keygen(path: Option<PathBuf>) -> Result<(), String> {
    let path = match path {
        Some(p) => p,
        None => default_key_path()?,
    };
    if path.exists() {
        return Err(format!("refusing to overwrite existing key: {}", path.display()));
    }
    let dir = path.parent().ok_or("bad key path")?;
    std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    restrict_dir(dir)?; // ACL first, so the key never exists with inherited permissions
    let mut seed = [0u8; 32];
    OsRng.fill_bytes(&mut seed);
    let sk = SigningKey::from_bytes(&seed);
    std::fs::write(&path, format!("{}\n", hex(&seed))).map_err(|e| e.to_string())?;
    seed.fill(0);
    let public = sk.verifying_key().to_bytes();
    println!("public_key_hex: {}", hex(&public));
    println!("key_id:         {}", key_id(&public));
    println!("private_key:    {}", path.display());
    println!("BACK UP the private key file now (offline). Never share it. If it is lost, installed copies can no longer be updated.");
    Ok(())
}

fn sign(key: Option<PathBuf>, manifest: &Path, out: Option<PathBuf>) -> Result<(), String> {
    let key = match key {
        Some(k) => k,
        None => default_key_path()?,
    };
    let sk = load_key(&key)?;
    let data = std::fs::read(manifest).map_err(|e| format!("cannot read {}: {e}", manifest.display()))?;
    let sig = sk.sign(&data);
    let public = sk.verifying_key().to_bytes();
    let line = format!("{SIG_TAG} {} {}\n", key_id(&public), hex(&sig.to_bytes()));
    let out = out.unwrap_or_else(|| PathBuf::from(format!("{}.sig", manifest.display())));
    std::fs::write(&out, line).map_err(|e| e.to_string())?;
    sk.verifying_key().verify(&data, &sig).map_err(|e| format!("self-check failed: {e}"))?;
    println!("signed {} -> {} (key_id {})", manifest.display(), out.display(), key_id(&public));
    Ok(())
}

fn trusted_from_json(json: &str) -> Result<Vec<(String, [u8; 32])>, String> {
    // Minimal extraction of "public_key_hex" values; the file format is controlled by this repo.
    let mut out = Vec::new();
    let mut rest = json;
    while let Some(i) = rest.find("\"public_key_hex\"") {
        rest = &rest[i + 16..];
        let q1 = rest.find('"').ok_or("bad json")?;
        let r2 = &rest[q1 + 1..];
        let q2 = r2.find('"').ok_or("bad json")?;
        let pk: [u8; 32] = unhex(&r2[..q2])?.try_into().map_err(|_| "bad public key length".to_string())?;
        out.push((key_id(&pk), pk));
        rest = &r2[q2..];
    }
    if out.is_empty() {
        return Err("no keys in trusted-keys.json".into());
    }
    Ok(out)
}

fn verify(manifest: &Path, sig: Option<PathBuf>, pubkey_hex: Option<String>, keys_json: Option<PathBuf>) -> Result<(), String> {
    let data = std::fs::read(manifest).map_err(|e| format!("cannot read {}: {e}", manifest.display()))?;
    let sigp = sig.unwrap_or_else(|| PathBuf::from(format!("{}.sig", manifest.display())));
    let line = std::fs::read_to_string(&sigp).map_err(|e| format!("cannot read {}: {e}", sigp.display()))?;
    let parts: Vec<&str> = line.split_whitespace().collect();
    if parts.len() != 3 || parts[0] != SIG_TAG {
        return Err("malformed signature file".into());
    }
    let trusted: Vec<(String, [u8; 32])> = if let Some(h) = pubkey_hex {
        let pk: [u8; 32] = unhex(&h)?.try_into().map_err(|_| "bad public key length".to_string())?;
        vec![(key_id(&pk), pk)]
    } else {
        let p = keys_json.ok_or("give --pub <hex> or --keys <trusted-keys.json>")?;
        trusted_from_json(&std::fs::read_to_string(&p).map_err(|e| e.to_string())?)?
    };
    let (_, pk) = trusted.iter().find(|(id, _)| id == parts[1]).ok_or_else(|| format!("unknown key_id {}", parts[1]))?;
    let sig: [u8; 64] = unhex(parts[2])?.try_into().map_err(|_| "bad signature length".to_string())?;
    VerifyingKey::from_bytes(pk)
        .map_err(|e| e.to_string())?
        .verify(&data, &Signature::from_bytes(&sig))
        .map_err(|_| "SIGNATURE INVALID".to_string())?;
    println!("signature OK (key_id {})", parts[1]);
    Ok(())
}

fn flag(args: &[String], name: &str) -> Option<String> {
    args.iter().position(|a| a == name).and_then(|i| args.get(i + 1)).cloned()
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let r = match args.first().map(String::as_str) {
        Some("keygen") => keygen(flag(&args, "--out").map(PathBuf::from)),
        Some("sign") => match args.get(1) {
            Some(m) if !m.starts_with("--") => sign(flag(&args, "--key").map(PathBuf::from), Path::new(m), flag(&args, "--out").map(PathBuf::from)),
            _ => Err("usage: sign <latest.json> [--key <file>] [--out <sig>]".into()),
        },
        Some("verify") => match args.get(1) {
            Some(m) if !m.starts_with("--") => verify(Path::new(m), flag(&args, "--sig").map(PathBuf::from), flag(&args, "--pub"), flag(&args, "--keys").map(PathBuf::from)),
            _ => Err("usage: verify <latest.json> [--sig <file>] (--pub <hex> | --keys <trusted-keys.json>)".into()),
        },
        _ => Err("usage: brouillon-release keygen [--out <file>] | sign <file> | verify <file>".into()),
    };
    match r {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sign_then_verify_roundtrip_and_tamper() {
        let dir = std::env::temp_dir().join(format!("brel-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let key = dir.join("k.key");
        let mut seed = [7u8; 32];
        seed[0] = 1;
        std::fs::write(&key, hex(&seed)).unwrap();
        let m = dir.join("latest.json");
        std::fs::write(&m, b"{\"a\":1}").unwrap();
        sign(Some(key.clone()), &m, None).unwrap();
        let pk = hex(&SigningKey::from_bytes(&seed).verifying_key().to_bytes());
        verify(&m, None, Some(pk.clone()), None).unwrap();
        std::fs::write(&m, b"{\"a\":2}").unwrap();
        assert!(verify(&m, None, Some(pk), None).is_err());
        std::fs::remove_dir_all(&dir).ok();
    }
}
