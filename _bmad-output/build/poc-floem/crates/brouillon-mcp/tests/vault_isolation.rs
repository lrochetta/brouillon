//! The agent side never reaches the secret vault (decision: the vault crate must not be a dependency of
//! `brouillon-mcp`; agents see at most "a credential exists" for a site they already see, and today the
//! tool surface does not even carry that). Four independent walls, each tested here:
//! 1. the dependency graph: the manifest and the sources name neither the vault nor the editor engine;
//! 2. the tool list: no tool is about credentials, vaults, saved passwords or connecting to a site;
//! 3. calling such a tool by name is refused as unknown, and says nothing about vault state;
//! 4. the vault file and the site list are on the never-share list (blocked, not masked).

use std::sync::Arc;

use brouillon_mcp::core::{McpConfig, McpCore, Session};
use brouillon_mcp::fake::FakeEditor;
use brouillon_mcp::policy::{Grant, Scope};
use brouillon_mcp::tools;
use serde_json::{json, Value};

fn crate_dir() -> std::path::PathBuf {
    std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

#[test]
fn the_mcp_crate_does_not_depend_on_the_vault_or_the_editor() {
    let manifest = std::fs::read_to_string(crate_dir().join("Cargo.toml")).unwrap();
    for banned in ["brouillon-vault", "brouillon-editor-engine", "brouillon-ui", "brouillon-remote"] {
        assert!(!manifest.contains(banned), "brouillon-mcp must not depend on {banned}");
    }
    let mut checked = 0;
    for dir in ["src", "tests"] {
        for e in std::fs::read_dir(crate_dir().join(dir)).unwrap().flatten() {
            if e.path().extension().is_some_and(|x| x == "rs") && e.file_name() != "vault_isolation.rs" {
                let text = std::fs::read_to_string(e.path()).unwrap();
                assert!(!text.contains("brouillon_vault") && !text.contains("brouillon_editor_engine"), "{:?}", e.path());
                checked += 1;
            }
        }
    }
    assert!(checked >= 10);
}

#[test]
fn no_tool_is_about_credentials_vaults_or_connecting_to_a_site() {
    let defs = tools::definitions();
    assert!(!defs.is_empty());
    for d in &defs {
        let name = d["name"].as_str().unwrap().to_lowercase();
        let text = d.to_string().to_lowercase();
        for banned in ["credential", "vault", "keychain", "dpapi", "saved password", "passphrase"] {
            assert!(!name.contains(banned) && !text.contains(banned), "tool {name} mentions {banned}");
        }
        for banned in ["connect", "site", "login", "unlock", "keep"] {
            assert!(!name.contains(banned), "tool {name} looks like it reaches a site or the vault ({banned})");
        }
    }
}

fn call(core: &McpCore<FakeEditor>, s: &mut Session, tool: &str) -> (Value, String) {
    let v = json!({"jsonrpc": "2.0", "id": 1, "method": "tools/call", "params": {"name": tool, "arguments": {}}});
    let line = core.handle_line(s, &v.to_string()).expect("a reply");
    (serde_json::from_str(&line).unwrap(), line)
}

#[test]
fn credential_looking_tool_names_are_unknown_and_reveal_nothing() {
    let api = Arc::new(FakeEditor::new());
    api.add_folder("C:\\proj");
    let core = McpCore::new(api, McpConfig::default()).unwrap();
    core.grant(Grant { client: "claude".into(), folder: "C:\\proj".into(), scope: Scope::S2, expires_at: None, exe_hash: None });
    let mut s = Session::new("claude");
    let (_, baseline) = call(&core, &mut s, "definitely_not_a_tool");
    for name in ["credentials", "list_credentials", "credential_status", "vault", "vault_list", "vault_unlock", "get_secret", "saved_password", "connect", "connect_site", "unlock", "sites"] {
        let (r, line) = call(&core, &mut s, name);
        let refused = r["error"].is_object() || r["result"]["isError"].as_bool() == Some(true);
        assert!(refused, "{name} must be refused: {line}");
        // Same refusal as for any unknown name: it cannot be used to probe whether a vault exists.
        assert_eq!(line.replace(name, "X"), baseline.replace("definitely_not_a_tool", "X"), "{name}");
    }
}

#[test]
fn the_vault_file_and_the_site_list_are_blocked_not_masked() {
    for p in ["C:\\proj\\vault.bsv", "C:\\proj\\sub\\vault.bsv", "C:\\proj\\sites.toml", "C:\\proj\\vault.bsv.bak", "C:\\proj\\vault.bsv.tmp.0a1b2c3d", "C:\\proj\\sites.toml.tmp"] {
        let api = Arc::new(FakeEditor::new());
        api.add_folder("C:\\proj");
        api.add_file(p, "BSV1 canary-bytes\n");
        let core = McpCore::new(api, McpConfig::default()).unwrap();
        core.grant(Grant { client: "claude".into(), folder: "C:\\proj".into(), scope: Scope::S1, expires_at: None, exe_hash: None });
        let mut s = Session::new("claude");
        let v = json!({"jsonrpc": "2.0", "id": 1, "method": "tools/call", "params": {"name": "read", "arguments": {"path": p}}});
        let line = core.handle_line(&mut s, &v.to_string()).unwrap();
        assert!(!line.contains("canary-bytes"), "{p}: {line}");
        let r: Value = serde_json::from_str(&line).unwrap();
        assert_eq!(r["result"]["isError"].as_bool(), Some(true), "{p}: {line}");
    }
}
