//! Permissions: scopes S0-S4 per client x folder, deny wins, path confinement, secret denylist,
//! kill switch. The single choke point is [`Policy::authorize`].

use std::collections::HashSet;

use serde_json::Value;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Scope {
    /// Deny (a grant with S0 overrides every other grant on that folder).
    S0,
    /// Read.
    S1,
    /// Propose.
    S2,
    /// Apply (V2, earned trust).
    S3,
    /// Execute (V3).
    S4,
}

impl Scope {
    pub fn parse(s: &str) -> Option<Scope> {
        Some(match s {
            "S0" | "none" => Scope::S0,
            "S1" | "read" => Scope::S1,
            "S2" | "propose" => Scope::S2,
            "S3" | "apply" => Scope::S3,
            "S4" | "execute" => Scope::S4,
            _ => return None,
        })
    }
    pub fn name(self) -> &'static str {
        ["S0", "S1", "S2", "S3", "S4"][self as usize]
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Grant {
    pub client: String,
    /// Absolute folder this grant covers.
    pub folder: String,
    pub scope: Scope,
    /// Unix seconds; `None` = until revoked.
    pub expires_at: Option<u64>,
    /// Best-effort executable hash; a different hash at call time forces re-consent.
    pub exe_hash: Option<String>,
}

#[derive(Debug, Clone)]
pub struct PolicyConfig {
    /// Ceiling for every grant. V1 ships S2: S3/S4 grants are clamped.
    pub max_scope: Scope,
}

impl Default for PolicyConfig {
    fn default() -> Self {
        Self { max_scope: Scope::S2 }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Denial {
    pub code: &'static str,
    pub message: String,
    pub hint: String,
}

pub const MSG_PROGRAM_CHANGED: &str = "The client program changed";
pub const MSG_TOOLS_CHANGED: &str = "The set of tools changed";
pub const MSG_OUTSIDE: &str = "This path is outside the folders you allowed";

impl Denial {
    pub fn program_changed(&self) -> bool {
        self.message == MSG_PROGRAM_CHANGED
    }
    pub fn outside_folders(&self) -> bool {
        self.message == MSG_OUTSIDE
    }
}

fn denial(code: &'static str, message: &str, hint: &str) -> Denial {
    Denial { code, message: message.into(), hint: hint.into() }
}

#[derive(Default)]
pub struct Policy {
    pub cfg: PolicyConfig,
    grants: Vec<Grant>,
    blocked: HashSet<String>,
    killed: bool,
    never_share: NeverShareConfig,
    /// Tool-set hash each client was asked about (see [`Policy::grant_pinned`]).
    tool_hashes: std::collections::HashMap<String, String>,
}

impl Policy {
    pub fn new(cfg: PolicyConfig) -> Self {
        Self { cfg, ..Default::default() }
    }

    pub fn set_never_share(&mut self, n: NeverShareConfig) {
        self.never_share = n;
    }

    /// Is this (normalised) path refused to agents? Core list always; user globs always;
    /// web-app configs unless the user allowed the folder that holds them.
    pub fn is_never_shared(&self, comps: &[String]) -> bool {
        if is_core_blocked(comps) || user_glob_hits(&self.never_share.globs, comps) {
            return true;
        }
        is_web_config(comps)
            && !self.never_share.allow_web_configs.iter().any(|f| normalize(f).map_or(false, |nf| comps.starts_with(&nf)))
    }

    /// Like [`Policy::grant`], for a grant restored from disk together with the tool-set hash it
    /// was given under. If the catalogue changed since (`tools::tool_set_hash()`), the client must
    /// be asked again (rug-pull defence, spec 60).
    pub fn grant_pinned(&mut self, g: Grant, tool_hash: &str) {
        let client = g.client.clone();
        self.grant(g);
        self.tool_hashes.insert(client, tool_hash.into());
    }

    /// The tool-set hash a client's grant was given under (to persist next to the grant).
    pub fn grant_tool_hash(&self, client: &str) -> Option<&str> {
        self.tool_hashes.get(client).map(String::as_str)
    }

    /// A new answer for the same client and folder replaces the old one (so a re-consent after a
    /// program change cannot leave a stale grant behind).
    pub fn grant(&mut self, g: Grant) {
        self.blocked.remove(&g.client);
        self.tool_hashes.insert(g.client.clone(), crate::tools::tool_set_hash());
        let same = |o: &Grant| o.client == g.client && normalize(&o.folder).ok() == normalize(&g.folder).ok();
        self.grants.retain(|o| !same(o));
        self.grants.push(g);
    }

    /// Is `folder` (or a parent of it) covered by a live grant of `client`, whatever its scope?
    pub fn covers(&self, client: &str, folder: &str, now: u64) -> bool {
        let Ok(f) = normalize(folder) else { return false };
        self.grants.iter().any(|g| g.client == client && g.expires_at.map_or(true, |e| e > now) && normalize(&g.folder).map_or(false, |gf| f.starts_with(&gf)))
    }

    pub fn block(&mut self, client: &str) {
        self.grants.retain(|g| g.client != client);
        self.blocked.insert(client.into());
    }

    pub fn revoke(&mut self, client: &str) {
        self.grants.retain(|g| g.client != client);
    }

    pub fn grants(&self) -> &[Grant] {
        &self.grants
    }

    pub fn is_killed(&self) -> bool {
        self.killed
    }

    /// Kill switch: every call is refused and every grant is gone until re-enabled.
    pub fn kill(&mut self) {
        self.killed = true;
        self.grants.clear();
    }

    pub fn rearm(&mut self) {
        self.killed = false;
    }

    pub fn has_consent(&self, client: &str, now: u64) -> bool {
        self.grants.iter().any(|g| g.client == client && g.expires_at.map_or(true, |e| e > now))
    }

    /// Effective scope for `client` on `path` (`None` = an unsaved buffer / editor-level call).
    pub fn effective_scope(&self, client: &str, exe_hash: Option<&str>, path: Option<&str>, now: u64) -> Result<Scope, Denial> {
        if self.killed {
            return Err(denial("KILLED", "Link was stopped by the user", "Ask the user to re-enable Agents."));
        }
        if self.blocked.contains(client) {
            return Err(denial("DENIED", "This client is blocked", "The user blocked this client."));
        }
        let live: Vec<&Grant> = self.grants.iter().filter(|g| g.client == client && g.expires_at.map_or(true, |e| e > now)).collect();
        if live.is_empty() {
            return Err(denial("CONSENT_REQUIRED", "No permission yet for this client", "The user must allow this client in Brouillon."));
        }
        // A grant bound to a program hash only holds for that program: a different hash, or no
        // hash at all, needs a new answer from the user.
        if live.iter().any(|g| g.exe_hash.is_some() && g.exe_hash.as_deref() != exe_hash) {
            return Err(denial("CONSENT_REQUIRED", MSG_PROGRAM_CHANGED, "The user must allow it again."));
        }
        // The tool catalogue changed since the user answered: ask again.
        if self.tool_hashes.get(client).map_or(false, |h| *h != crate::tools::tool_set_hash()) {
            return Err(denial("CONSENT_REQUIRED", MSG_TOOLS_CHANGED, "The set of tools changed since you allowed this client; the user must allow it again."));
        }
        let ceiling = self.cfg.max_scope;
        let Some(path) = path else {
            let best = live.iter().map(|g| g.scope).max().unwrap_or(Scope::S0);
            return match best {
                Scope::S0 => Err(denial("DENIED", "Access denied", "")),
                s => Ok(s.min(ceiling)),
            };
        };
        let comps = normalize(path).map_err(|why| denial("OUT_OF_SCOPE", &format!("Path rejected: {why}"), "Use a plain absolute path inside a granted folder."))?;
        if self.is_never_shared(&comps) {
            return Err(denial("DENYLISTED", "This file is never exposed to agents", "Secrets and Brouillon's own files are always hidden."));
        }
        let mut best: Option<Scope> = None;
        for g in &live {
            let Ok(folder) = normalize(&g.folder) else { continue };
            if comps.starts_with(&folder) {
                if g.scope == Scope::S0 {
                    return Err(denial("DENIED", "Access denied for this folder", "Deny rules win over allow rules."));
                }
                best = Some(best.map_or(g.scope, |b| b.max(g.scope)));
            }
        }
        match best {
            Some(s) => Ok(s.min(ceiling)),
            None => Err(denial("OUT_OF_SCOPE", MSG_OUTSIDE, "Only granted folders are visible.")),
        }
    }

    /// Does the client have at least `need` on `path`?
    pub fn authorize(&self, client: &str, exe_hash: Option<&str>, need: Scope, path: Option<&str>, now: u64) -> Result<Scope, Denial> {
        let have = self.effective_scope(client, exe_hash, path, now)?;
        if have >= need {
            Ok(have)
        } else {
            Err(denial("OUT_OF_SCOPE", &format!("This needs {} but the client has {}", need.name(), have.name()), "The user can raise the permission in Agents > Permissions."))
        }
    }
}

/// Scope a tool call needs (before the per-path check).
pub fn required_scope(tool: &str, args: &Value) -> Scope {
    let flag = |k: &str| args.get(k).and_then(Value::as_bool).unwrap_or(false);
    match tool {
        "editor_state" | "read" | "search" | "diagnostics" => Scope::S1,
        "edit" => {
            if flag("dry_run") {
                Scope::S1
            } else {
                Scope::S2
            }
        }
        "proposal" => match args.get("action").and_then(Value::as_str) {
            Some("cancel") => Scope::S2,
            _ => Scope::S1,
        },
        "open" => {
            if flag("focus") {
                Scope::S2
            } else {
                Scope::S1
            }
        }
        "run_command" => {
            if flag("list") {
                Scope::S1
            } else {
                Scope::S2 // raised to S3/S4 by the command's own flags in the tool
            }
        }
        "annotate" => Scope::S2,
        "save" => Scope::S3,
        _ => Scope::S4,
    }
}

const RESERVED: [&str; 22] = [
    "con", "prn", "aux", "nul", "com1", "com2", "com3", "com4", "com5", "com6", "com7", "com8", "com9", "lpt1", "lpt2", "lpt3", "lpt4", "lpt5",
    "lpt6", "lpt7", "lpt8", "lpt9",
];

/// Lexical canonicalisation: lower-cased components of an absolute path, or the reason it is refused.
/// Refuses `..`, UNC and `\\?\`, alternate data streams, reserved device names, 8.3 aliases and
/// names Windows would silently trim (trailing dot or space).
pub fn normalize(path: &str) -> Result<Vec<String>, &'static str> {
    if path.starts_with("\\\\") || path.starts_with("//") {
        return Err("UNC or device path");
    }
    let p = path.replace('\\', "/");
    let mut it = p.split('/').peekable();
    let mut out = Vec::new();
    let first = *it.peek().ok_or("empty path")?;
    let drive = first.len() == 2 && first.as_bytes()[1] == b':' && first.as_bytes()[0].is_ascii_alphabetic();
    if !(first.is_empty() || drive) {
        return Err("not absolute");
    }
    for (i, c) in it.enumerate() {
        if i == 0 {
            if drive {
                out.push(c.to_ascii_lowercase());
            }
            continue;
        }
        if c.is_empty() || c == "." {
            continue;
        }
        if c == ".." {
            return Err("parent traversal");
        }
        if c.contains(':') {
            return Err("alternate data stream");
        }
        if c.chars().any(|ch| ch.is_control() || "<>\"|?*".contains(ch)) {
            return Err("illegal character");
        }
        if c.ends_with('.') || c.ends_with(' ') {
            return Err("trailing dot or space");
        }
        let lc = c.to_lowercase();
        let stem = lc.split('.').next().unwrap_or("");
        if RESERVED.contains(&stem) {
            return Err("reserved device name");
        }
        if let Some(pos) = lc.find('~') {
            if lc[pos + 1..].chars().next().map_or(false, |d| d.is_ascii_digit()) {
                return Err("8.3 alias");
            }
        }
        out.push(lc);
    }
    if out.is_empty() {
        return Err("empty path");
    }
    Ok(out)
}

/// Never-share settings that come from the USER layer. A workspace file can only add globs
/// ([`NeverShareConfig::tightened_by_workspace`]); it can never remove the core list, a user
/// glob, or grant a web-config allowance.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct NeverShareConfig {
    /// Extra globs (`*` and `?`; a pattern without `/` is matched against each path component,
    /// one with `/` against runs of components). Widening only.
    pub globs: Vec<String>,
    /// Explicit user action: folders inside which web-app config files (`wp-config.php`...) are
    /// not blocked. Never read from a workspace file.
    pub allow_web_configs: Vec<String>,
}

impl NeverShareConfig {
    /// Apply a workspace layer: its globs are added, nothing else it says has any effect.
    pub fn tightened_by_workspace(&self, workspace_globs: &[String]) -> Self {
        let mut out = self.clone();
        for g in workspace_globs {
            if !out.globs.contains(g) {
                out.globs.push(g.clone());
            }
        }
        out
    }
}

const CORE_DIRS: [&str; 13] = [".ssh", ".aws", ".gnupg", ".brouillon", "brouillon-link", "hex-regions", ".kube", ".azure", ".docker-credentials", ".password-store", ".terraform.d", "bitwarden", "keepass"];
const CORE_NAMES: [&str; 17] = [
    ".npmrc", ".git-credentials", ".netrc", "_netrc", ".pgpass", ".htpasswd", "known_hosts", "sites.toml", "vault.bsv", "terraform.tfvars", ".env", ".dev.vars",
    ".pypirc", ".dockercfg", ".my.cnf", ".s3cfg", "link.token",
];
const CORE_SUFFIXES: [&str; 18] = [
    ".pem", ".key", ".kdbx", ".kdb", ".pfx", ".p12", ".jks", ".keystore", ".ppk", ".tfstate", ".tfstate.backup", ".ovpn", ".rdp", ".1pux", ".1pif", ".env",
    ".enc.json", ".sops.yaml",
];
const SOURCE_EXT: [&str; 16] = ["rs", "py", "js", "ts", "tsx", "jsx", "go", "java", "kt", "cs", "rb", "c", "h", "cpp", "md", "txt"];
const WEB_CONFIGS: [&str; 4] = ["wp-config.php", "configuration.php", "localsettings.php", "config.inc.php"];

/// The non-removable core list: private keys, tokens, credential stores, state files, the
/// editor's own files. Blocked, never masked. `comps` are lower-cased normalised components.
pub fn is_core_blocked(comps: &[String]) -> bool {
    if comps.windows(2).any(|w| (w[0] == ".docker" && w[1] == "config.json") || (w[0] == ".config" && (w[1] == "gcloud" || w[1] == "gh" || w[1] == "rclone"))) {
        return true;
    }
    comps.iter().any(|c| {
        let c = c.as_str();
        let ext = c.rsplit_once('.').map(|x| x.1).unwrap_or("");
        let source_like = SOURCE_EXT.contains(&ext);
        CORE_DIRS.contains(&c)
            || CORE_NAMES.contains(&c)
            || c.starts_with(".env.")
            // The vault's backup (`.bak`) and interrupted-write files (`.tmp.*`), and the site list's temp file.
            || c.starts_with("vault.bsv.")
            || c.starts_with("sites.toml.")
            || CORE_SUFFIXES.iter().any(|s| c.ends_with(s))
            || c.starts_with("id_")
            || c.starts_with("bitwarden_export")
            || ((c.starts_with("secrets.") || c.starts_with("credentials") || c.starts_with("secrets-")) && !source_like)
    })
}

/// Web-application configuration files (database passwords, salts): blocked by default, but the
/// user may allow them per folder.
pub fn is_web_config(comps: &[String]) -> bool {
    comps.last().map_or(false, |c| WEB_CONFIGS.contains(&c.as_str()))
}

/// Secrets and the editor's own files: never listed, read, or written (core list + web configs,
/// no user settings). [`Policy`] adds the user globs and the per-folder web-config allowance.
pub fn is_denylisted(comps: &[String]) -> bool {
    is_core_blocked(comps) || is_web_config(comps)
}

/// `*` and `?` glob over lower-cased text.
pub fn glob_match(pat: &str, text: &str) -> bool {
    let (p, t): (Vec<char>, Vec<char>) = (pat.to_lowercase().chars().collect(), text.chars().collect());
    let (mut pi, mut ti, mut star, mut mark) = (0, 0, None::<usize>, 0);
    while ti < t.len() {
        if pi < p.len() && (p[pi] == '?' || p[pi] == t[ti]) {
            pi += 1;
            ti += 1;
        } else if pi < p.len() && p[pi] == '*' {
            star = Some(pi);
            mark = ti;
            pi += 1;
        } else if let Some(s) = star {
            pi = s + 1;
            mark += 1;
            ti = mark;
        } else {
            return false;
        }
    }
    while pi < p.len() && p[pi] == '*' {
        pi += 1;
    }
    pi == p.len()
}

fn user_glob_hits(globs: &[String], comps: &[String]) -> bool {
    globs.iter().any(|g| {
        let g = g.replace('\\', "/");
        let g = g.trim_matches('/');
        if g.is_empty() {
            return false;
        }
        let parts = g.split('/').count();
        if parts == 1 {
            comps.iter().any(|c| glob_match(g, c))
        } else {
            comps.windows(parts).any(|w| glob_match(g, &w.join("/")))
        }
    })
}

/// Paths agents may read but never write (supply-chain footguns).
pub fn is_write_protected(path: &str) -> bool {
    let Ok(comps) = normalize(path) else { return true };
    comps.iter().any(|c| c == ".git") || comps.windows(2).any(|w| w[0] == ".vscode" && w[1] == "tasks.json")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pol() -> Policy {
        let mut p = Policy::new(PolicyConfig::default());
        p.grant(Grant { client: "c".into(), folder: "C:\\proj".into(), scope: Scope::S2, expires_at: None, exe_hash: None });
        p
    }

    #[test]
    fn confinement_corpus_is_denied() {
        let p = pol();
        let bad = [
            "C:\\proj\\..\\secret.txt",
            "C:\\proj\\sub\\..\\..\\x",
            "\\\\server\\share\\proj\\a.txt",
            "\\\\?\\C:\\proj\\a.txt",
            "C:\\proj\\a.txt:hidden",
            "C:\\proj\\CON",
            "C:\\proj\\nul.txt",
            "C:\\proj\\PROGRA~1\\a",
            "C:\\proj\\a.txt.",
            "C:\\proj-evil\\a.txt",
            "C:\\projx\\a.txt",
            "D:\\proj\\a.txt",
            "relative\\a.txt",
            "",
        ];
        for b in bad {
            assert!(p.effective_scope("c", None, Some(b), 0).is_err(), "must deny {b:?}");
        }
        assert_eq!(p.effective_scope("c", None, Some("C:\\proj\\src\\a.rs"), 0), Ok(Scope::S2));
        // Case and separators do not matter (Windows), and `.` is harmless.
        assert_eq!(p.effective_scope("c", None, Some("c:/PROJ/./src//a.rs"), 0), Ok(Scope::S2));
    }

    #[test]
    fn denylist_hides_secrets_even_inside_a_grant() {
        let p = pol();
        for f in [".env", ".env.local", "k.pem", "server.key", "id_rsa", "id_ed25519.pub", ".ssh\\config", ".aws\\credentials", ".npmrc", ".git-credentials", "x.kdbx"] {
            let path = format!("C:\\proj\\{f}");
            let e = p.effective_scope("c", None, Some(&path), 0).unwrap_err();
            assert_eq!(e.code, "DENYLISTED", "{f}");
        }
        assert!(p.effective_scope("c", None, Some("C:\\proj\\environment.md"), 0).is_ok());
    }

    #[test]
    fn consent_is_required_and_deny_wins() {
        let mut p = pol();
        assert_eq!(p.effective_scope("stranger", None, None, 0).unwrap_err().code, "CONSENT_REQUIRED");
        p.grant(Grant { client: "c".into(), folder: "C:\\proj\\private".into(), scope: Scope::S0, expires_at: None, exe_hash: None });
        assert_eq!(p.effective_scope("c", None, Some("C:\\proj\\private\\a"), 0).unwrap_err().code, "DENIED");
        assert!(p.effective_scope("c", None, Some("C:\\proj\\public\\a"), 0).is_ok());
    }

    #[test]
    fn grants_expire_and_exe_change_forces_reconsent() {
        let mut p = Policy::new(PolicyConfig::default());
        p.grant(Grant { client: "c".into(), folder: "/w".into(), scope: Scope::S1, expires_at: Some(100), exe_hash: Some("h1".into()) });
        assert!(p.effective_scope("c", Some("h1"), Some("/w/a"), 99).is_ok());
        assert_eq!(p.effective_scope("c", Some("h2"), Some("/w/a"), 99).unwrap_err().code, "CONSENT_REQUIRED");
        assert_eq!(p.effective_scope("c", Some("h1"), Some("/w/a"), 100).unwrap_err().code, "CONSENT_REQUIRED");
    }

    #[test]
    fn ceiling_clamps_s3_and_s4() {
        let mut p = Policy::new(PolicyConfig::default());
        p.grant(Grant { client: "c".into(), folder: "/w".into(), scope: Scope::S4, expires_at: None, exe_hash: None });
        assert_eq!(p.effective_scope("c", None, Some("/w/a"), 0), Ok(Scope::S2));
        assert_eq!(p.authorize("c", None, Scope::S3, Some("/w/a"), 0).unwrap_err().code, "OUT_OF_SCOPE");
        let mut p3 = Policy::new(PolicyConfig { max_scope: Scope::S3 });
        p3.grant(Grant { client: "c".into(), folder: "/w".into(), scope: Scope::S4, expires_at: None, exe_hash: None });
        assert_eq!(p3.effective_scope("c", None, Some("/w/a"), 0), Ok(Scope::S3));
    }

    #[test]
    fn kill_switch_revokes_everything_until_rearmed() {
        let mut p = pol();
        p.kill();
        assert_eq!(p.effective_scope("c", None, None, 0).unwrap_err().code, "KILLED");
        p.rearm();
        // Grants did not come back: consent again.
        assert_eq!(p.effective_scope("c", None, None, 0).unwrap_err().code, "CONSENT_REQUIRED");
    }

    #[test]
    fn block_removes_grants() {
        let mut p = pol();
        p.block("c");
        assert_eq!(p.effective_scope("c", None, None, 0).unwrap_err().code, "DENIED");
    }

    fn code(p: &Policy, f: &str) -> Option<&'static str> {
        p.effective_scope("c", None, Some(&format!("C:\\proj\\{f}")), 0).err().map(|e| e.code)
    }

    #[test]
    fn extended_core_list_is_blocked() {
        let p = pol();
        for f in [
            ".netrc", "_netrc", ".pgpass", ".docker\\config.json", "infra\\prod.tfstate", "terraform.tfstate.backup", "terraform.tfvars", "cert.pfx", "a.p12", "app.jks",
            "release.keystore", "id_rsa", "id_rsa.pub", "k.ppk", ".htpasswd", "known_hosts", "sites.toml", "vault.bsv", "client.ovpn", "x.rdp", ".config\\gcloud\\creds.db",
            ".kube\\config", "secrets.yaml", "credentials.json", "prod.env", ".env.production", "link.token", ".pypirc", "export\\bitwarden_export_2026.json", "wp-config.php",
        ] {
            assert_eq!(code(&p, f), Some("DENYLISTED"), "{f}");
        }
        // Source files that merely carry the word are not blocked (their content is masked instead).
        for f in ["src\\secrets.rs", "credentials.md", "environment.md", "src\\idle.rs", "tokenizer.rs"] {
            assert_eq!(code(&p, f), None, "{f}");
        }
    }

    /// Hex-mode regions (names and offsets of a binary the user is inspecting) live in a
    /// `hex-regions` folder under the settings dir: agents never see it, whether the settings dir is
    /// the default `.brouillon` or a custom / portable one.
    #[test]
    fn hex_region_sidecars_are_blocked_for_agents() {
        let p = pol();
        for f in [".brouillon\\hex-regions\\0123456789abcdef0123456789abcdef.json", "portable\\data\\hex-regions\\0123456789abcdef0123456789abcdef.json", "hex-regions"] {
            assert_eq!(code(&p, f), Some("DENYLISTED"), "{f}");
        }
    }

    #[test]
    fn web_config_is_overridable_only_by_an_explicit_user_folder_and_core_never_is() {
        let mut p = pol();
        assert_eq!(code(&p, "blog\\wp-config.php"), Some("DENYLISTED"));
        p.set_never_share(NeverShareConfig { globs: vec![], allow_web_configs: vec!["C:\\proj\\blog".into()] });
        assert_eq!(code(&p, "blog\\wp-config.php"), None);
        assert_eq!(code(&p, "other\\wp-config.php"), Some("DENYLISTED"), "only the allowed folder");
        // The allowance never unlocks the core list.
        for f in ["blog\\.env", "blog\\id_rsa", "blog\\.netrc", "blog\\a.tfstate", "blog\\.pgpass"] {
            assert_eq!(code(&p, f), Some("DENYLISTED"), "{f}");
        }
    }

    #[test]
    fn user_globs_widen_and_a_workspace_layer_can_only_tighten() {
        let user = NeverShareConfig { globs: vec!["*.sqlite".into(), "private/*".into()], allow_web_configs: vec!["C:\\proj\\blog".into()] };
        // A hostile workspace file tries to loosen: it can add globs, nothing else.
        let merged = user.tightened_by_workspace(&["*.bak".into()]);
        assert_eq!(merged.globs.len(), 3);
        assert_eq!(merged.allow_web_configs, user.allow_web_configs);
        let loosened = NeverShareConfig::default().tightened_by_workspace(&[]);
        assert!(loosened.allow_web_configs.is_empty() && loosened.globs.is_empty());
        let mut p = pol();
        p.set_never_share(merged);
        assert_eq!(code(&p, "db.sqlite"), Some("DENYLISTED"));
        assert_eq!(code(&p, "x\\private\\notes.txt"), Some("DENYLISTED"));
        assert_eq!(code(&p, "old.bak"), Some("DENYLISTED"));
        assert_eq!(code(&p, "src\\a.rs"), None);
        assert!(glob_match("*.TXT", "a.txt") && glob_match("a?c", "abc") && !glob_match("a?c", "ac"));
    }

    #[test]
    fn changed_tool_set_forces_reconsent() {
        let mut p = Policy::new(PolicyConfig::default());
        let g = || Grant { client: "c".into(), folder: "/w".into(), scope: Scope::S1, expires_at: None, exe_hash: None };
        p.grant_pinned(g(), "0000000000000000");
        let e = p.effective_scope("c", None, Some("/w/a"), 0).unwrap_err();
        assert_eq!((e.code, e.message.as_str()), ("CONSENT_REQUIRED", MSG_TOOLS_CHANGED));
        p.grant(g());
        assert!(p.effective_scope("c", None, Some("/w/a"), 0).is_ok());
        assert_eq!(p.grant_tool_hash("c"), Some(crate::tools::tool_set_hash().as_str()));
        p.grant_pinned(g(), &crate::tools::tool_set_hash());
        assert!(p.effective_scope("c", None, Some("/w/a"), 0).is_ok());
    }

    #[test]
    fn write_protected_paths() {
        assert!(is_write_protected("C:\\proj\\.git\\config"));
        assert!(is_write_protected("C:\\proj\\.vscode\\tasks.json"));
        assert!(!is_write_protected("C:\\proj\\src\\main.rs"));
    }
}
