//! `[mcp]` settings. Link is OFF unless `enabled = true` (or the session-only `--mcp` flag).

use serde::Deserialize;

use crate::policy::Scope;

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(default)]
pub struct McpSettings {
    pub enabled: bool,
    /// "read" (S1) | "ask" (S2, default) | "trusted" (S3, V2).
    pub mode: String,
    /// Optional auto-off after N minutes without a client (None = stay on until exit).
    pub auto_off_minutes: Option<u32>,
    /// Explicit opt-in to raise the ceiling from S2 (propose) to S3 (apply directly, save). Never S4.
    pub allow_apply: bool,
    /// Advanced: also listen on 127.0.0.1 (Streamable HTTP, bearer token). Off by default.
    pub http: bool,
    /// Session-only enable (command-line `--mcp`): never written back.
    #[serde(skip)]
    pub session_only: bool,
    /// Extra files agents may never see (globs). Widening only; the core list cannot be removed.
    pub never_share: Vec<String>,
    /// Explicit allowance: folders where web-app configs such as `wp-config.php` are not blocked.
    /// USER settings only; ignored in a workspace layer.
    pub allow_web_configs: Vec<String>,
}

impl Default for McpSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            mode: "ask".into(),
            auto_off_minutes: None,
            allow_apply: false,
            http: false,
            session_only: false,
            never_share: vec![],
            allow_web_configs: vec![],
        }
    }
}

#[derive(Deserialize, Default)]
struct File {
    #[serde(default)]
    mcp: McpSettings,
}

impl McpSettings {
    /// Parse the `[mcp]` table of a settings file. Anything unreadable means OFF.
    pub fn from_toml(s: &str) -> Self {
        toml::from_str::<File>(s).map(|f| f.mcp).unwrap_or_default()
    }

    /// Never-share configuration for the policy.
    pub fn never_share_config(&self) -> crate::policy::NeverShareConfig {
        crate::policy::NeverShareConfig { globs: self.never_share.clone(), allow_web_configs: self.allow_web_configs.clone() }
    }

    /// Never-share configuration with a workspace file's `[mcp] never_share` merged in. A workspace
    /// file can only add globs: its `allow_web_configs`, `enabled`, `mode`... have no effect here.
    pub fn never_share_with_workspace(&self, workspace_toml: &str) -> crate::policy::NeverShareConfig {
        let ws = Self::from_toml(workspace_toml);
        self.never_share_config().tightened_by_workspace(&ws.never_share)
    }

    /// `brouillon --mcp`: enabled for this session only.
    pub fn enable_for_session(mut self) -> Self {
        self.enabled = true;
        self.session_only = true;
        self
    }

    /// Highest scope any grant may reach: S2 unless the user explicitly set `allow_apply = true` (S3).
    pub fn ceiling(&self) -> Scope {
        if self.allow_apply {
            Scope::S3
        } else {
            Scope::S2
        }
    }

    /// The starting scope the user picked; unknown values fall back to the safe default.
    pub fn starting_scope(&self) -> Scope {
        match self.mode.as_str() {
            "read" => Scope::S1,
            "trusted" => Scope::S3,
            _ => Scope::S2,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn off_by_default_everywhere() {
        assert!(!McpSettings::default().enabled);
        assert!(!McpSettings::from_toml("").enabled);
        assert!(!McpSettings::from_toml("[editor]\nfont = 12\n").enabled);
        assert!(!McpSettings::from_toml("[mcp]\n").enabled);
        // Garbage means off, not a crash and not on.
        assert!(!McpSettings::from_toml("[mcp\nenabled = true").enabled);
        assert!(!McpSettings::from_toml("[mcp]\nenabled = \"yes\"").enabled);
    }

    #[test]
    fn explicit_enable_and_session_flag() {
        let s = McpSettings::from_toml("[mcp]\nenabled = true\nmode = \"read\"\n");
        assert!(s.enabled && !s.session_only);
        assert_eq!(s.starting_scope(), Scope::S1);
        let t = McpSettings::default().enable_for_session();
        assert!(t.enabled && t.session_only);
        assert_eq!(McpSettings::from_toml("[mcp]\nmode = \"weird\"").starting_scope(), Scope::S2);
    }

    #[test]
    fn apply_ceiling_needs_an_explicit_setting_and_never_reaches_s4() {
        assert_eq!(McpSettings::default().ceiling(), Scope::S2);
        assert_eq!(McpSettings::from_toml("[mcp]\nmode = \"trusted\"\n").ceiling(), Scope::S2, "choosing trusted mode alone does not raise it");
        assert_eq!(McpSettings::from_toml("[mcp]\nallow_apply = true\n").ceiling(), Scope::S3);
        assert_eq!(McpSettings::from_toml("[mcp]\nallow_apply = \"yes\"\n").ceiling(), Scope::S2, "garbage is the safe default");
    }

    #[test]
    fn workspace_layer_only_tightens() {
        let user = McpSettings::from_toml("[mcp]\nnever_share = [\"*.db\"]\nallow_web_configs = [\"C:/blog\"]\n");
        let n = user.never_share_with_workspace("[mcp]\nnever_share = [\"*.bak\"]\nallow_web_configs = [\"C:/\"]\nenabled = true\n");
        assert_eq!(n.globs, vec!["*.db".to_string(), "*.bak".to_string()]);
        assert_eq!(n.allow_web_configs, vec!["C:/blog".to_string()], "a workspace cannot widen the allowance");
        let none = McpSettings::default().never_share_with_workspace("[mcp]\nallow_web_configs = [\"C:/\"]\n");
        assert!(none.allow_web_configs.is_empty());
    }

    #[test]
    fn http_and_auto_off_are_opt_in() {
        let s = McpSettings::default();
        assert!(!s.http && s.auto_off_minutes.is_none());
        let t = McpSettings::from_toml("[mcp]\nenabled = true\nhttp = true\nauto_off_minutes = 15\n");
        assert!(t.http);
        assert_eq!(t.auto_off_minutes, Some(15));
    }
}
