//! Site profiles: the NON-secret description of a server, kept in a small human-readable TOML
//! file (`sites.toml`, `[[site]]` tables). A password is never a field of anything in this module.
//! Where a secret will live (OS vault / agent / key passphrase) is only a [`SecretRef`] handle.

use std::path::Path;

use serde::{Deserialize, Serialize};

pub type SiteId = String;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Protocol {
    #[default]
    Sftp,
    Ftps,
    Ftp,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "method", rename_all = "snake_case")]
pub enum AuthMethod {
    /// ssh-agent / Pageant / OpenSSH agent: Brouillon handles no secret at all.
    #[default]
    Agent,
    /// Key file: only its PATH is stored; the passphrase is asked at connect time.
    KeyFile { path: String },
    /// Password asked at connect time (and, later, optionally kept in the vault via `SecretRef`).
    Password,
}

impl AuthMethod {
    pub fn label(&self) -> &'static str {
        match self {
            AuthMethod::Agent => "SSH agent",
            AuthMethod::KeyFile { .. } => "SSH key file",
            AuthMethod::Password => "Password",
        }
    }
}

/// An opaque handle naming an entry of the secret vault (the vault's random id in hex), never the
/// secret itself. `Some` means "a password is saved for this site"; the value lives only in the vault.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SecretRef(pub Option<String>);

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SiteProfile {
    pub id: SiteId,
    pub name: String,
    #[serde(default)]
    pub protocol: Protocol,
    pub host: String,
    #[serde(default = "default_port")]
    pub port: u16,
    pub user: String,
    #[serde(default)]
    pub auth: AuthMethod,
    /// Folder the explorer starts in.
    #[serde(default = "default_root")]
    pub root_path: String,
    /// Destructive actions ask for stricter confirmation.
    #[serde(default)]
    pub production: bool,
    /// No write, rename, create or delete is offered.
    #[serde(default)]
    pub read_only: bool,
    /// Vault handle (placeholder, see [`SecretRef`]).
    #[serde(default)]
    pub secret_ref: SecretRef,
    /// "Keep this password?" was already asked for this site (asked once per site, decision Q2).
    #[serde(default)]
    pub keep_asked: bool,
}

fn default_port() -> u16 {
    22
}

fn default_root() -> String {
    "/".into()
}

/// `user@host:port`, `sftp://user@host:port/path` or `ssh user@host` (the shapes of spec 02, 4).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ParsedAddress {
    pub protocol: Protocol,
    pub user: Option<String>,
    pub host: String,
    pub port: Option<u16>,
    pub path: Option<String>,
}

pub fn parse_address(input: &str) -> Option<ParsedAddress> {
    let mut s = input.trim();
    if let Some(r) = s.strip_prefix("ssh ") {
        s = r.trim();
    }
    let mut protocol = Protocol::Sftp;
    for (pre, p) in [("sftp://", Protocol::Sftp), ("ssh://", Protocol::Sftp), ("ftps://", Protocol::Ftps), ("ftp://", Protocol::Ftp)] {
        if let Some(r) = s.strip_prefix(pre) {
            protocol = p;
            s = r;
            break;
        }
    }
    let (authority, path) = match s.find('/') {
        Some(i) => (&s[..i], Some(s[i..].to_string())),
        None => (s, None),
    };
    let (user, hostport) = match authority.rsplit_once('@') {
        Some((u, h)) if !u.is_empty() => (Some(u.to_string()), h),
        _ => (None, authority),
    };
    let (host, port) = match hostport.rsplit_once(':') {
        Some((h, p)) if !h.contains(':') => (h, Some(p.parse::<u16>().ok().filter(|p| *p > 0)?)),
        _ => (hostport, None),
    };
    let host_ok = !host.is_empty() && host.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_'));
    host_ok.then(|| ParsedAddress { protocol, user, host: host.to_string(), port, path })
}

impl SiteProfile {
    /// Profile from the three connect fields. `None` when the address cannot be understood or no user is given.
    pub fn from_quick(address: &str, auth: AuthMethod, folder: &str) -> Option<Self> {
        let a = parse_address(address)?;
        let user = a.user.filter(|u| !u.is_empty())?;
        let root = if folder.trim().is_empty() { a.path.unwrap_or_else(default_root) } else { folder.trim().to_string() };
        let name = a.host.split('.').next().unwrap_or(&a.host).to_string();
        Some(Self {
            id: slug(&format!("{user}@{}", a.host)),
            name,
            protocol: a.protocol,
            host: a.host,
            port: a.port.unwrap_or_else(default_port),
            user,
            auth,
            root_path: super::path::RemotePath::new(&root).to_string(),
            production: false,
            read_only: false,
            secret_ref: SecretRef::default(),
            keep_asked: false,
        })
    }

    /// Plain FTP is refused by default (spec 04): it needs its own explicit opt-in dialog.
    pub fn is_unencrypted(&self) -> bool {
        self.protocol == Protocol::Ftp
    }

    /// `sftp://user@host:port` (no path, never a secret).
    pub fn url(&self) -> String {
        let scheme = match self.protocol {
            Protocol::Sftp => "sftp",
            Protocol::Ftps => "ftps",
            Protocol::Ftp => "ftp",
        };
        let port = if self.port == default_port() && self.protocol == Protocol::Sftp { String::new() } else { format!(":{}", self.port) };
        format!("{scheme}://{}@{}{port}", self.user, self.host)
    }
}

fn slug(s: &str) -> String {
    let mut out: String = s.chars().map(|c| if c.is_ascii_alphanumeric() { c.to_ascii_lowercase() } else { '-' }).collect();
    while out.contains("--") {
        out = out.replace("--", "-");
    }
    out.trim_matches('-').to_string()
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SiteStore {
    #[serde(default, rename = "site")]
    pub sites: Vec<SiteProfile>,
}

impl SiteStore {
    pub fn parse(text: &str) -> Self {
        toml::from_str(text).unwrap_or_default()
    }

    pub fn to_text(&self) -> String {
        let header = "# Brouillon remote sites. No password or passphrase is ever stored in this file.\n";
        format!("{header}{}", toml::to_string(self).unwrap_or_default())
    }

    /// A missing or unreadable file is an empty list (never an error at startup).
    pub fn load(path: &Path) -> Self {
        std::fs::read_to_string(path).map(|t| Self::parse(&t)).unwrap_or_default()
    }

    /// Write through a temp file so a crash cannot leave a half-written list.
    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let tmp = path.with_extension("toml.tmp");
        std::fs::write(&tmp, self.to_text())?;
        std::fs::rename(&tmp, path)
    }

    pub fn get(&self, id: &str) -> Option<&SiteProfile> {
        self.sites.iter().find(|s| s.id == id)
    }

    /// Insert or replace by id; a different site with the same id gets a numeric suffix only on `add_unique`.
    pub fn upsert(&mut self, p: SiteProfile) {
        match self.sites.iter_mut().find(|s| s.id == p.id) {
            Some(s) => *s = p,
            None => self.sites.push(p),
        }
    }

    pub fn remove(&mut self, id: &str) -> bool {
        let n = self.sites.len();
        self.sites.retain(|s| s.id != id);
        self.sites.len() != n
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn address_shapes_are_understood() {
        let a = parse_address("deploy@vps.example.org").unwrap();
        assert_eq!((a.user.as_deref(), a.host.as_str(), a.port), (Some("deploy"), "vps.example.org", None));
        let b = parse_address("sftp://bob@10.0.0.7:2222/var/www").unwrap();
        assert_eq!((b.port, b.path.as_deref(), b.protocol), (Some(2222), Some("/var/www"), Protocol::Sftp));
        assert_eq!(parse_address("ssh me@box").unwrap().host, "box");
        assert_eq!(parse_address("ftp://u@h").unwrap().protocol, Protocol::Ftp);
        assert!(parse_address("").is_none());
        assert!(parse_address("user@").is_none());
        assert!(parse_address("a@b:notaport").is_none());
        assert!(parse_address("a@ho st").is_none());
    }

    #[test]
    fn quick_profile_needs_a_user_and_defaults_the_rest() {
        let p = SiteProfile::from_quick("deploy@vps.example.org", AuthMethod::Agent, "").unwrap();
        assert_eq!((p.port, p.root_path.as_str(), p.name.as_str()), (22, "/", "vps"));
        assert_eq!(p.id, "deploy-vps-example-org");
        assert!(SiteProfile::from_quick("vps.example.org", AuthMethod::Agent, "").is_none());
        let q = SiteProfile::from_quick("a@h", AuthMethod::Password, "/var/../srv/www/").unwrap();
        assert_eq!(q.root_path, "/srv/www");
        assert_eq!(q.url(), "sftp://a@h");
    }

    #[test]
    fn store_round_trips_and_holds_no_secret() {
        let mut st = SiteStore::default();
        let mut p = SiteProfile::from_quick("deploy@vps.example.org:2200", AuthMethod::KeyFile { path: "~/.ssh/id_ed25519".into() }, "/var/www").unwrap();
        p.production = true;
        st.upsert(p.clone());
        st.upsert(SiteProfile::from_quick("me@box", AuthMethod::Password, "").unwrap());
        let text = st.to_text();
        assert!(!text.to_lowercase().contains("password ="), "{text}");
        assert!(text.contains("~/.ssh/id_ed25519"));
        let back = SiteStore::parse(&text);
        assert_eq!(back, st);
        assert_eq!(back.get(&p.id).unwrap().production, true);
    }

    #[test]
    fn a_profile_type_has_no_secret_bearing_field() {
        // The serialized shape is the whole contract: every key is non-secret or the placeholder.
        let p = SiteProfile::from_quick("a@h", AuthMethod::Password, "").unwrap();
        let v = toml::Value::try_from(&p).unwrap();
        let keys: Vec<String> = v.as_table().unwrap().keys().cloned().collect();
        assert!(!keys.iter().any(|k| k.contains("pass") || k.contains("pwd") || k.contains("private")), "{keys:?}");
        assert_eq!(p.secret_ref, SecretRef(None));
    }

    #[test]
    fn corrupt_or_missing_file_is_an_empty_list_and_save_is_atomic() {
        let d = tempfile::tempdir().unwrap();
        let f = d.path().join("sites.toml");
        assert!(SiteStore::load(&f).sites.is_empty());
        std::fs::write(&f, "[[site\nbroken").unwrap();
        assert!(SiteStore::load(&f).sites.is_empty());
        let mut st = SiteStore::default();
        st.upsert(SiteProfile::from_quick("a@h", AuthMethod::Agent, "").unwrap());
        st.save(&f).unwrap();
        assert_eq!(SiteStore::load(&f), st);
        assert!(!d.path().join("sites.toml.tmp").exists());
        assert!(st.remove("a-h"));
        assert!(!st.remove("a-h"));
    }
}
