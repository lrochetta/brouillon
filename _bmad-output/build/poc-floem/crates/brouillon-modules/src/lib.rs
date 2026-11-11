//! Optional modules on demand (design: `docs-formats/05` section 3, `04` section 0b).
//!
//! Absent by default. Detecting that a file needs a module touches no module and no network.
//! Only an explicit click (a [`Consent`] value, constructed by the UI handler of the Install button) may
//! download; the payload is checked (Ed25519 signature with its own domain tag, pinned SHA-256, size, PE
//! header, compatibility, rollback floor) before it is renamed into place. A module is loaded at its first
//! request and stays loaded until the application exits. Nothing here runs at startup: the modules folder
//! is first touched by the first state query for a kind that has a module.
//!
//! States: absent -> offered -> installing -> ready -> loaded (plus failed, back to an offer on retry).
//! Spawning the module's process is the next wave's job (the helper does not exist yet); [`ModuleManager::request`]
//! returns the verified, locked path to spawn.

use brouillon_core::docview::ModuleState;
use brouillon_update::keys::TrustedKey;
use brouillon_update::manifest::{check_file_name, check_url, verify_signature, UrlPolicy};
use brouillon_update::{install, net, timeutil};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// Host and base path the modules are served from (same host as the update channel).
pub const DEFAULT_BASE_URL: &str = "https://brouillon.rochetta.fr/modules";
/// Wire protocol version this build speaks to modules.
pub const PROTO: u32 = 1;
pub const MAX_MANIFEST_BYTES: u64 = 64 * 1024;
pub const MAX_SIG_BYTES: u64 = 4 * 1024;
pub const MAX_MODULE_BYTES: u64 = 64 * 1024 * 1024;
/// A module signature covers this tag then the manifest bytes, so an update-manifest signature (raw bytes)
/// never verifies as a module manifest and the reverse.
pub const SIG_DOMAIN: &[u8] = b"brouillon-module-v1\0";
const STATE_FILE: &str = "state.json";

/// Proof that the user clicked Install (or Install from a file). Required by everything that may touch the
/// network. Only UI event handlers should call [`Consent::explicit_click`].
#[derive(Debug, Clone, Copy)]
pub struct Consent(());

impl Consent {
    pub fn explicit_click() -> Self {
        Consent(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ModuleError {
    Network(String),
    BadSignature,
    BadManifest(String),
    /// Build lower than the highest one ever accepted for this module.
    Rollback,
    Incompatible(String),
    HashMismatch,
    SizeMismatch,
    NotPe,
    Io(String),
    NotInstalled,
    /// Already being installed: the second request waits for the first.
    Busy,
}

impl std::fmt::Display for ModuleError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        use ModuleError::*;
        match self {
            Network(_) => write!(f, "Could not reach the download server. Nothing was installed."),
            BadSignature | HashMismatch | SizeMismatch | NotPe => {
                write!(f, "The downloaded module did not match its signature, so it was not installed and was deleted.")
            }
            BadManifest(e) => write!(f, "The module description is not valid ({e}). Nothing was installed."),
            Rollback => write!(f, "This is an older version than the one installed before, so it was refused."),
            Incompatible(e) => write!(f, "This module does not fit this version of Brouillon ({e})."),
            Io(e) => write!(f, "Could not write the module files ({e})."),
            NotInstalled => write!(f, "The module is not installed."),
            Busy => write!(f, "The module is already being installed."),
        }
    }
}

impl From<brouillon_update::UpdateError> for ModuleError {
    fn from(e: brouillon_update::UpdateError) -> Self {
        use brouillon_update::UpdateError as U;
        match e {
            U::HashMismatch => ModuleError::HashMismatch,
            U::SizeMismatch | U::TooLarge => ModuleError::SizeMismatch,
            U::NotPe => ModuleError::NotPe,
            U::BadSignature | U::SigMalformed | U::UnknownKey(_) => ModuleError::BadSignature,
            U::Io(s) => ModuleError::Io(s),
            U::BadManifest(s) => ModuleError::BadManifest(s),
            U::InsecureUrl(_) | U::BadFileName(_) => ModuleError::BadManifest("address or file name refused".into()),
            other => ModuleError::Network(other.to_string()),
        }
    }
}

impl From<std::io::Error> for ModuleError {
    fn from(e: std::io::Error) -> Self {
        ModuleError::Io(e.to_string())
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct Requires {
    #[serde(default)]
    pub min_app_build: u64,
    #[serde(default)]
    pub max_app_build: Option<u64>,
    #[serde(default)]
    pub min_proto: u32,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ModFile {
    pub name: String,
    pub url: String,
    pub size: u64,
    pub sha256: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ModuleManifest {
    pub schema: u32,
    pub id: String,
    pub title: String,
    pub version: String,
    pub build: u64,
    pub published_utc: String,
    pub proto: u32,
    pub kinds: Vec<String>,
    pub requires: Requires,
    pub files: Vec<ModFile>,
    #[serde(default)]
    pub idle_ram_budget_mb: u32,
    #[serde(default)]
    pub min_rollback_build: u64,
}

/// What is recorded about an installed module.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Entry {
    pub version: String,
    pub build: u64,
    /// Path relative to the modules folder.
    pub file: String,
    pub sha256: String,
    pub size: u64,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct DiskState {
    #[serde(default)]
    modules: BTreeMap<String, Entry>,
    /// Highest build ever accepted per module; survives uninstall.
    #[serde(default)]
    floors: BTreeMap<String, u64>,
    /// File kinds (by key) the user chose to always open as text.
    #[serde(default)]
    always_text: Vec<String>,
}

/// Verified, locked module ready to be started by the host.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoadedModule {
    pub id: String,
    pub exe: PathBuf,
    pub version: String,
}

fn valid_ident(s: &str) -> bool {
    !s.is_empty() && s.len() <= 48 && s.bytes().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'-')
}

/// Everything a background thread needs to install one module without holding the manager.
#[derive(Debug, Clone)]
pub struct InstallJob {
    id: String,
    root: PathBuf,
    keys: Vec<TrustedKey>,
    app_build: u64,
    floor: u64,
}

impl InstallJob {
    pub fn id(&self) -> &str {
        &self.id
    }

    /// Signature first (domain-separated), then strict field validation and compatibility.
    pub fn verify_manifest(&self, manifest: &[u8], sig_text: &str, policy: &UrlPolicy) -> Result<ModuleManifest, ModuleError> {
        // `brouillon-sig-v1 module <key_id> <hex>`  ->  the update verifier's `brouillon-sig-v1 <key_id> <hex>`
        let p: Vec<&str> = sig_text.split_whitespace().collect();
        if p.len() != 4 || p[0] != brouillon_update::manifest::SIG_TAG || p[1] != "module" {
            return Err(ModuleError::BadSignature);
        }
        let mut signed = Vec::with_capacity(SIG_DOMAIN.len() + manifest.len());
        signed.extend_from_slice(SIG_DOMAIN);
        signed.extend_from_slice(manifest);
        verify_signature(&signed, &format!("{} {} {}", p[0], p[2], p[3]), &self.keys).map_err(|_| ModuleError::BadSignature)?;
        let m: ModuleManifest = serde_json::from_slice(manifest).map_err(|e| ModuleError::BadManifest(e.to_string()))?;
        let bad = |s: &str| ModuleError::BadManifest(s.to_string());
        if m.schema != 1 {
            return Err(bad("unsupported schema"));
        }
        if m.id != self.id || !valid_ident(&m.id) {
            return Err(bad("module id"));
        }
        if m.version.is_empty() || m.version.len() > 64 || !m.version.bytes().all(|c| c.is_ascii_alphanumeric() || matches!(c, b'.' | b'-' | b'+')) {
            return Err(bad("version"));
        }
        if m.build == 0 || timeutil::parse_utc(&m.published_utc).is_none() {
            return Err(bad("build or date"));
        }
        if m.files.len() != 1 {
            return Err(bad("exactly one file expected"));
        }
        let f = &m.files[0];
        check_file_name(&f.name)?;
        if f.sha256.len() != 64 || !f.sha256.bytes().all(|c| c.is_ascii_hexdigit()) {
            return Err(bad("sha256"));
        }
        if f.size == 0 || f.size > MAX_MODULE_BYTES {
            return Err(bad("size out of range"));
        }
        check_url(&f.url, policy)?;
        if !f.url.split(['?', '#']).next().unwrap_or("").ends_with(&format!("/{}", f.name)) {
            return Err(bad("file url does not end with the file name"));
        }
        let r = &m.requires;
        if self.app_build < r.min_app_build || r.max_app_build.map_or(false, |x| self.app_build > x) {
            return Err(ModuleError::Incompatible("app build".into()));
        }
        if r.min_proto > PROTO || m.proto != PROTO {
            return Err(ModuleError::Incompatible("protocol".into()));
        }
        if m.build < self.floor {
            return Err(ModuleError::Rollback);
        }
        Ok(m)
    }

    /// Size, SHA-256 and PE header of `payload` against the verified manifest, then an atomic rename into
    /// `<root>/<id>/<version>/<file>`. Nothing is executed. The staged file is removed on any failure.
    pub fn finalize(&self, m: &ModuleManifest, payload: &Path) -> Result<Entry, ModuleError> {
        let f = &m.files[0];
        let want = brouillon_update::manifest::Installer { file: f.name.clone(), url: f.url.clone(), size_bytes: f.size, sha256: f.sha256.clone() };
        let res = (|| {
            install::verify_installer(payload, &want)?;
            let dir = self.root.join(&m.id).join(&m.version);
            fs::create_dir_all(&dir)?;
            let dest = dir.join(&f.name);
            if dest.exists() {
                fs::remove_file(&dest)?;
            }
            fs::rename(payload, &dest).or_else(|_| fs::copy(payload, &dest).map(|_| ()).and_then(|_| fs::remove_file(payload)))?;
            Ok::<_, ModuleError>(Entry { version: m.version.clone(), build: m.build, file: format!("{}/{}/{}", m.id, m.version, f.name), sha256: f.sha256.to_ascii_lowercase(), size: f.size })
        })();
        if res.is_err() {
            let _ = fs::remove_file(payload);
        }
        res
    }

    /// Install from local files (Install from a file; same verification as a download).
    pub fn install_local(&self, manifest: &[u8], sig_text: &str, payload: &Path, policy: &UrlPolicy) -> Result<Entry, ModuleError> {
        let m = self.verify_manifest(manifest, sig_text, policy)?;
        // Stage a copy: the user's file is never moved.
        let staging = self.staging_path(&m)?;
        fs::copy(payload, &staging)?;
        self.finalize(&m, &staging)
    }

    /// Download `<base>/<id>/module.json`, its `.sig`, then the payload the manifest names. Plain HTTPS GETs:
    /// no cookie, no query string, no identifier. Call only after an explicit click.
    pub fn download_and_install(&self, _c: Consent, base_url: &str, policy: &UrlPolicy, dev: bool) -> Result<Entry, ModuleError> {
        let agent = net::UreqTransport { dev };
        let short = Duration::from_secs(60);
        let base = base_url.trim_end_matches('/');
        let manifest = net::fetch_limited(&agent, short, &format!("{base}/{}/module.json", self.id), MAX_MANIFEST_BYTES, policy)?;
        let sig = net::fetch_limited(&agent, short, &format!("{base}/{}/module.json.sig", self.id), MAX_SIG_BYTES, policy)?;
        let sig = String::from_utf8(sig).map_err(|_| ModuleError::BadSignature)?;
        let m = self.verify_manifest(&manifest, &sig, policy)?;
        let staging = self.staging_path(&m)?;
        let long = Duration::from_secs(brouillon_update::DOWNLOAD_TIMEOUT_SECS);
        net::download_to(&agent, long, &m.files[0].url, &staging, m.files[0].size, policy, &mut |_, _| {})?;
        self.finalize(&m, &staging)
    }

    fn staging_path(&self, m: &ModuleManifest) -> Result<PathBuf, ModuleError> {
        let dir = self.root.join(".staging");
        fs::create_dir_all(&dir)?;
        Ok(dir.join(format!("{}-{}.part", m.id, std::process::id())))
    }
}

pub struct ModuleManager {
    root: PathBuf,
    keys: Vec<TrustedKey>,
    app_build: u64,
    scanned: bool,
    disk: DiskState,
    states: HashMap<String, ModuleState>,
    /// Open read handles on loaded modules (share-deny-write on Windows): the verified file cannot be swapped.
    locks: HashMap<String, fs::File>,
}

impl ModuleManager {
    /// Does not touch the disk: the modules folder is first read by the first state query.
    pub fn new(root: PathBuf, keys: Vec<TrustedKey>, app_build: u64) -> Self {
        Self { root, keys, app_build, scanned: false, disk: DiskState::default(), states: HashMap::new(), locks: HashMap::new() }
    }

    fn scan(&mut self) {
        if self.scanned {
            return;
        }
        self.scanned = true;
        self.disk = fs::read(self.root.join(STATE_FILE)).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default();
    }

    fn save(&self) -> Result<(), ModuleError> {
        fs::create_dir_all(&self.root)?;
        let tmp = self.root.join(format!("{STATE_FILE}.tmp"));
        fs::write(&tmp, serde_json::to_vec_pretty(&self.disk).map_err(|e| ModuleError::Io(e.to_string()))?)?;
        fs::rename(&tmp, self.root.join(STATE_FILE))?;
        Ok(())
    }

    pub fn state(&mut self, id: &str) -> ModuleState {
        self.scan();
        if let Some(s) = self.states.get(id) {
            return s.clone();
        }
        let s = if self.disk.modules.contains_key(id) { ModuleState::Ready } else { ModuleState::Absent };
        self.states.insert(id.to_string(), s.clone());
        s
    }

    /// A file of this module's kind was opened: absent -> offered. No network, no file written.
    pub fn offer(&mut self, id: &str) -> ModuleState {
        let s = self.state(id);
        if matches!(s, ModuleState::Absent | ModuleState::Failed(_)) {
            self.states.insert(id.to_string(), ModuleState::Offered);
            return ModuleState::Offered;
        }
        s
    }

    /// Not now: offered -> absent.
    pub fn decline(&mut self, id: &str) {
        if matches!(self.state(id), ModuleState::Offered | ModuleState::Failed(_)) {
            self.states.insert(id.to_string(), ModuleState::Absent);
        }
    }

    /// Begin an install after an explicit click: offered/failed/absent -> installing. `Busy` when one is running.
    pub fn begin_install(&mut self, _c: Consent, id: &str) -> Result<InstallJob, ModuleError> {
        if !valid_ident(id) {
            return Err(ModuleError::BadManifest("module id".into()));
        }
        match self.state(id) {
            ModuleState::Installing => return Err(ModuleError::Busy),
            ModuleState::Ready | ModuleState::Loaded => return Err(ModuleError::BadManifest("already installed".into())),
            _ => {}
        }
        self.states.insert(id.to_string(), ModuleState::Installing);
        let floor = self.disk.floors.get(id).copied().unwrap_or(0);
        Ok(InstallJob { id: id.to_string(), root: self.root.clone(), keys: self.keys.clone(), app_build: self.app_build, floor })
    }

    /// Record the outcome of a job: installing -> ready, or failed (nothing recorded, staging already removed).
    pub fn finish_install(&mut self, id: &str, result: Result<Entry, ModuleError>) -> ModuleState {
        let st = match result {
            Ok(entry) => {
                let floor = self.disk.floors.entry(id.to_string()).or_insert(0);
                *floor = (*floor).max(entry.build);
                self.disk.modules.insert(id.to_string(), entry);
                match self.save() {
                    Ok(()) => ModuleState::Ready,
                    Err(e) => ModuleState::Failed(e.to_string()),
                }
            }
            Err(e) => ModuleState::Failed(e.to_string()),
        };
        self.states.insert(id.to_string(), st.clone());
        st
    }

    /// First request for a module: re-verify the installed file through one locked handle, then loaded.
    /// Stays loaded until [`shutdown_all`](Self::shutdown_all).
    pub fn request(&mut self, id: &str) -> Result<LoadedModule, ModuleError> {
        let s = self.state(id);
        let Some(entry) = self.disk.modules.get(id).cloned().filter(|_| matches!(s, ModuleState::Ready | ModuleState::Loaded)) else {
            return Err(ModuleError::NotInstalled);
        };
        let exe = self.root.join(&entry.file);
        if matches!(s, ModuleState::Loaded) && self.locks.contains_key(id) {
            return Ok(LoadedModule { id: id.into(), exe, version: entry.version });
        }
        let verified = open_locked(&exe).and_then(|mut f| {
            let (sha, size) = sha256_of(&mut f)?;
            if size != entry.size {
                return Err(ModuleError::SizeMismatch);
            }
            if !sha.eq_ignore_ascii_case(&entry.sha256) {
                return Err(ModuleError::HashMismatch);
            }
            Ok(f)
        });
        match verified {
            Ok(f) => {
                self.locks.insert(id.to_string(), f);
                self.states.insert(id.to_string(), ModuleState::Loaded);
                Ok(LoadedModule { id: id.into(), exe, version: entry.version })
            }
            Err(e) => {
                // Missing, swapped or quarantined: a plain failure, never a retry loop.
                self.states.insert(id.to_string(), ModuleState::Failed(e.to_string()));
                Err(e)
            }
        }
    }

    /// Application exit: release every loaded module (the host has asked its processes to quit first).
    pub fn shutdown_all(&mut self) -> usize {
        let n = self.locks.len();
        self.locks.clear();
        for s in self.states.values_mut() {
            if *s == ModuleState::Loaded {
                *s = ModuleState::Ready;
            }
        }
        n
    }

    /// Remove the files and the entry; the rollback floor stays.
    pub fn uninstall(&mut self, id: &str) -> Result<(), ModuleError> {
        self.scan();
        self.locks.remove(id);
        if !valid_ident(id) {
            return Err(ModuleError::NotInstalled);
        }
        let _ = fs::remove_dir_all(self.root.join(id));
        self.disk.modules.remove(id);
        self.states.insert(id.to_string(), ModuleState::Absent);
        self.save()
    }

    /// Kinds (by key) the user chose to always open as text.
    pub fn always_text(&mut self) -> Vec<String> {
        self.scan();
        self.disk.always_text.clone()
    }

    pub fn set_always_text(&mut self, kind_key: &str, on: bool) -> Result<(), ModuleError> {
        self.scan();
        self.disk.always_text.retain(|k| k != kind_key);
        if on {
            self.disk.always_text.push(kind_key.to_string());
        }
        self.save()
    }
}

#[cfg(windows)]
fn open_locked(p: &Path) -> Result<fs::File, ModuleError> {
    use std::os::windows::fs::OpenOptionsExt;
    // FILE_SHARE_READ only: no other process can write, rename or delete it while it is held.
    Ok(fs::OpenOptions::new().read(true).share_mode(1).open(p)?)
}

#[cfg(not(windows))]
fn open_locked(p: &Path) -> Result<fs::File, ModuleError> {
    Ok(fs::File::open(p)?)
}

fn sha256_of(f: &mut fs::File) -> Result<(String, u64), ModuleError> {
    use sha2::{Digest, Sha256};
    use std::io::Read;
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
    Ok((brouillon_update::keys::hex(&h.finalize()), total))
}

#[cfg(test)]
mod tests;
