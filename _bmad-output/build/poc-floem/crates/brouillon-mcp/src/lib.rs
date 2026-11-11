//! Brouillon Link: the opt-in MCP server, minus the GUI.
//!
//! - [`core::McpCore`]: JSON-RPC 2.0 / MCP handler over an abstract [`api::EditorApi`].
//! - [`tools`]: the ten core tools (static catalogue).
//! - [`policy`], [`audit`]: scopes per client and folder, kill switch, hash-chained audit log.
//! - [`link`], [`relay`]: IPC protocol, listener, instance registry, and the stdio relay.
//! - [`settings`]: `[mcp] enabled = false` by default; nothing listens unless enabled.
//!
//! No GUI types and no rmcp: this slice is dependency-light on purpose (spec 60 section 3.1
//! keeps rmcp optional behind a feature; the wire format here is plain MCP JSON-RPC).

pub mod api;
pub mod audit;
pub mod bridge;
pub mod core;
pub mod diag;
pub mod diff;
pub mod fake;
pub mod limits;
pub mod http;
pub mod jsonrpc;
pub mod link;
pub mod os_guard;
pub mod policy;
pub mod relay;
pub mod settings;
pub mod tools;

use sha2::{Digest, Sha256};

/// Best-effort fingerprint of a client executable: hash of the file when readable (and not huge),
/// else of the path text. Informational input to re-consent, never a security boundary.
pub fn exe_fingerprint(path: &str) -> String {
    let bytes = match std::fs::metadata(path) {
        Ok(m) if m.is_file() && m.len() <= 64 * 1024 * 1024 => std::fs::read(path).unwrap_or_else(|_| path.as_bytes().to_vec()),
        _ => path.as_bytes().to_vec(),
    };
    Sha256::digest(&bytes)[..12].iter().map(|b| format!("{b:02x}")).collect()
}
