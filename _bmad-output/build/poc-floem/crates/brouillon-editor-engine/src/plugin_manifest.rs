//! WASM plugin manifest and host types.
//!
//! Defines the plugin manifest format, capability declarations, and
//! the host API surface that plugins can call. This is the type foundation
//! for the wasmtime-based plugin runtime (M2 Sprint 3).
//!
//! Plugin manifest format (`plugin.toml`):
//! ```toml
//! [plugin]
//! name = "my-formatter"
//! version = "1.0.0"
//! description = "Custom code formatter"
//! author = "Jane Doe"
//! license = "MIT"
//!
//! [capabilities]
//! file_read = true
//! file_write = false
//! network = false
//! ui_panel = true
//! commands = ["format.selection", "format.document"]
//! ```

use std::path::{Path, PathBuf};

/// Plugin metadata from the manifest.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct PluginManifest {
    pub plugin: PluginInfo,
    pub capabilities: PluginCapabilities,
}

/// Core plugin information.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct PluginInfo {
    pub name: String,
    pub version: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub author: String,
    #[serde(default)]
    pub license: String,
    /// Path to the WASM binary (relative to manifest).
    #[serde(default = "default_wasm_path")]
    pub wasm_path: String,
}

fn default_wasm_path() -> String {
    "plugin.wasm".to_string()
}

/// Capability declarations — what the plugin is allowed to do.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct PluginCapabilities {
    /// Can read files from the workspace.
    #[serde(default)]
    pub file_read: bool,
    /// Can write files to the workspace.
    #[serde(default)]
    pub file_write: bool,
    /// Can make network requests.
    #[serde(default)]
    pub network: bool,
    /// Can spawn a UI panel.
    #[serde(default)]
    pub ui_panel: bool,
    /// Can access the clipboard.
    #[serde(default)]
    pub clipboard: bool,
    /// Can run shell commands.
    #[serde(default)]
    pub shell: bool,
    /// List of editor commands this plugin provides.
    #[serde(default)]
    pub commands: Vec<String>,
    /// File extensions this plugin activates on.
    #[serde(default)]
    pub activation_extensions: Vec<String>,
}

impl PluginCapabilities {
    /// Create a minimal (safe) capability set.
    pub fn minimal() -> Self {
        Self {
            file_read: false,
            file_write: false,
            network: false,
            ui_panel: false,
            clipboard: false,
            shell: false,
            commands: Vec::new(),
            activation_extensions: Vec::new(),
        }
    }

    /// Check if this capability set requires any dangerous permissions.
    pub fn has_dangerous_permissions(&self) -> bool {
        self.file_write || self.network || self.shell
    }

    /// Human-readable summary of permissions for the Privacy Dot UI.
    pub fn permission_summary(&self) -> Vec<String> {
        let mut perms = Vec::new();
        if self.file_read { perms.push("Read files".to_string()); }
        if self.file_write { perms.push("Write files".to_string()); }
        if self.network { perms.push("Network access".to_string()); }
        if self.clipboard { perms.push("Clipboard access".to_string()); }
        if self.shell { perms.push("Shell commands".to_string()); }
        if self.ui_panel { perms.push("UI panel".to_string()); }
        perms
    }
}

/// A loaded plugin (manifest + resolved paths).
#[derive(Debug, Clone)]
pub struct LoadedPlugin {
    pub manifest: PluginManifest,
    /// Absolute path to the plugin directory.
    pub plugin_dir: PathBuf,
    /// Whether the plugin is currently enabled.
    pub enabled: bool,
}

impl LoadedPlugin {
    /// Absolute path to the WASM binary.
    pub fn wasm_path(&self) -> PathBuf {
        self.plugin_dir.join(&self.manifest.plugin.wasm_path)
    }
}

/// Plugin registry — manages installed plugins.
#[derive(Debug, Clone)]
pub struct PluginRegistry {
    plugins: Vec<LoadedPlugin>,
}

impl PluginRegistry {
    pub fn empty() -> Self {
        Self { plugins: Vec::new() }
    }

    /// Scan a directory for plugin manifests.
    pub fn scan_directory(dir: &Path) -> Self {
        let mut plugins = Vec::new();

        if let Ok(entries) = std::fs::read_dir(dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_dir() {
                    let manifest_path = path.join("plugin.toml");
                    if manifest_path.exists() {
                        if let Some(plugin) = load_plugin_manifest(&manifest_path) {
                            plugins.push(LoadedPlugin {
                                manifest: plugin,
                                plugin_dir: path,
                                enabled: true,
                            });
                        }
                    }
                }
            }
        }

        Self { plugins }
    }

    /// Get all loaded plugins.
    pub fn all(&self) -> &[LoadedPlugin] {
        &self.plugins
    }

    /// Get enabled plugins only.
    pub fn enabled(&self) -> Vec<&LoadedPlugin> {
        self.plugins.iter().filter(|p| p.enabled).collect()
    }

    /// Find plugins that provide a specific command.
    pub fn find_by_command(&self, command: &str) -> Vec<&LoadedPlugin> {
        self.plugins.iter()
            .filter(|p| p.enabled && p.manifest.capabilities.commands.iter().any(|c| c == command))
            .collect()
    }

    /// Find plugins that activate on a file extension.
    pub fn find_by_extension(&self, ext: &str) -> Vec<&LoadedPlugin> {
        self.plugins.iter()
            .filter(|p| p.enabled && p.manifest.capabilities.activation_extensions.iter().any(|e| e == ext))
            .collect()
    }

    /// Total plugin count.
    pub fn len(&self) -> usize {
        self.plugins.len()
    }

    pub fn is_empty(&self) -> bool {
        self.plugins.is_empty()
    }
}

/// Load a plugin manifest from a TOML file.
pub fn load_plugin_manifest(path: &Path) -> Option<PluginManifest> {
    let content = std::fs::read_to_string(path).ok()?;
    parse_manifest(&content)
}

/// Parse a plugin manifest from TOML content.
pub fn parse_manifest(content: &str) -> Option<PluginManifest> {
    toml::from_str(content).ok()
}

/// Default plugins directory: `~/.brouillon/plugins/`.
pub fn plugins_dir() -> Option<PathBuf> {
    dirs::home_dir().map(|h| h.join(".brouillon").join("plugins"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_manifest_basic() {
        let toml = r#"
[plugin]
name = "test-plugin"
version = "1.0.0"
description = "A test plugin"
author = "Test Author"

[capabilities]
file_read = true
commands = ["test.run", "test.debug"]
"#;
        let manifest = parse_manifest(toml);
        assert!(manifest.is_some());
        let m = manifest.unwrap();
        assert_eq!(m.plugin.name, "test-plugin");
        assert_eq!(m.plugin.version, "1.0.0");
        assert!(m.capabilities.file_read);
        assert!(!m.capabilities.file_write);
        assert_eq!(m.capabilities.commands.len(), 2);
    }

    #[test]
    fn parse_manifest_minimal() {
        let toml = r#"
[plugin]
name = "minimal"
version = "0.1.0"

[capabilities]
"#;
        let m = parse_manifest(toml).unwrap();
        assert_eq!(m.plugin.name, "minimal");
        assert!(!m.capabilities.file_read);
        assert!(m.capabilities.commands.is_empty());
    }

    #[test]
    fn parse_manifest_invalid() {
        assert!(parse_manifest("not valid toml {{{").is_none());
    }

    #[test]
    fn dangerous_permissions_detection() {
        let safe = PluginCapabilities::minimal();
        assert!(!safe.has_dangerous_permissions());

        let mut dangerous = PluginCapabilities::minimal();
        dangerous.network = true;
        assert!(dangerous.has_dangerous_permissions());

        let mut dangerous2 = PluginCapabilities::minimal();
        dangerous2.shell = true;
        assert!(dangerous2.has_dangerous_permissions());
    }

    #[test]
    fn permission_summary_output() {
        let mut caps = PluginCapabilities::minimal();
        caps.file_read = true;
        caps.network = true;
        let summary = caps.permission_summary();
        assert_eq!(summary.len(), 2);
        assert!(summary.contains(&"Read files".to_string()));
        assert!(summary.contains(&"Network access".to_string()));
    }

    #[test]
    fn permission_summary_empty() {
        let caps = PluginCapabilities::minimal();
        assert!(caps.permission_summary().is_empty());
    }

    #[test]
    fn registry_empty() {
        let reg = PluginRegistry::empty();
        assert!(reg.is_empty());
        assert_eq!(reg.len(), 0);
    }

    #[test]
    fn registry_find_by_command() {
        let reg = PluginRegistry {
            plugins: vec![LoadedPlugin {
                manifest: parse_manifest(r#"
[plugin]
name = "formatter"
version = "1.0.0"
[capabilities]
commands = ["format.document"]
"#).unwrap(),
                plugin_dir: PathBuf::from("/tmp/formatter"),
                enabled: true,
            }],
        };

        let found = reg.find_by_command("format.document");
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].manifest.plugin.name, "formatter");

        let not_found = reg.find_by_command("lint.file");
        assert!(not_found.is_empty());
    }

    #[test]
    fn registry_find_by_extension() {
        let reg = PluginRegistry {
            plugins: vec![LoadedPlugin {
                manifest: parse_manifest(r#"
[plugin]
name = "go-support"
version = "1.0.0"
[capabilities]
activation_extensions = ["go", "mod"]
"#).unwrap(),
                plugin_dir: PathBuf::from("/tmp/go-support"),
                enabled: true,
            }],
        };

        let found = reg.find_by_extension("go");
        assert_eq!(found.len(), 1);
    }

    #[test]
    fn wasm_path_resolution() {
        let plugin = LoadedPlugin {
            manifest: parse_manifest(r#"
[plugin]
name = "test"
version = "1.0.0"
wasm_path = "build/plugin.wasm"
[capabilities]
"#).unwrap(),
            plugin_dir: PathBuf::from("/home/user/.brouillon/plugins/test"),
            enabled: true,
        };

        let wasm = plugin.wasm_path();
        assert!(wasm.to_str().unwrap().contains("build"));
        assert!(wasm.to_str().unwrap().ends_with("plugin.wasm"));
    }

    #[test]
    fn default_wasm_path_is_plugin_wasm() {
        let m = parse_manifest(r#"
[plugin]
name = "test"
version = "1.0.0"
[capabilities]
"#).unwrap();
        assert_eq!(m.plugin.wasm_path, "plugin.wasm");
    }
}
