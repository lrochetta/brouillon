//! LSP protocol type definitions for Language Server Protocol communication.
//!
//! Defines the core JSON-RPC message types, LSP request/notification
//! structures, and capability negotiation types needed to communicate
//! with any LSP server (rust-analyzer, pyright, tsserver, etc.).
//!
//! This is the **type foundation** — the actual async transport and
//! server lifecycle management will be in `lsp_client.rs` (M2 Sprint 3).

use std::path::PathBuf;

/// LSP position (0-based line and character offset).
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Position {
    pub line: u32,
    pub character: u32,
}

impl Position {
    pub fn new(line: u32, character: u32) -> Self {
        Self { line, character }
    }
}

/// LSP range (start and end positions).
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Range {
    pub start: Position,
    pub end: Position,
}

impl Range {
    pub fn new(start: Position, end: Position) -> Self {
        Self { start, end }
    }

    pub fn single_line(line: u32, start_char: u32, end_char: u32) -> Self {
        Self {
            start: Position::new(line, start_char),
            end: Position::new(line, end_char),
        }
    }
}

/// LSP location (URI + range).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Location {
    pub uri: String,
    pub range: Range,
}

/// Document identifier.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct TextDocumentIdentifier {
    pub uri: String,
}

/// Versioned document identifier.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct VersionedTextDocumentIdentifier {
    pub uri: String,
    pub version: i32,
}

/// Content change event (full text or incremental).
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct TextDocumentContentChangeEvent {
    /// Range of text replaced (None = full document).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub range: Option<Range>,
    /// New text content.
    pub text: String,
}

/// Diagnostic severity level.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[repr(u8)]
pub enum DiagnosticSeverity {
    Error = 1,
    Warning = 2,
    Information = 3,
    Hint = 4,
}

/// A diagnostic message from the LSP server.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Diagnostic {
    pub range: Range,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub severity: Option<DiagnosticSeverity>,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
}

/// Completion item kind (matches LSP spec).
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[repr(u8)]
pub enum CompletionItemKind {
    Text = 1,
    Method = 2,
    Function = 3,
    Constructor = 4,
    Field = 5,
    Variable = 6,
    Class = 7,
    Interface = 8,
    Module = 9,
    Property = 10,
    Unit = 11,
    Value = 12,
    Enum = 13,
    Keyword = 14,
    Snippet = 15,
    Color = 16,
    File = 17,
    Reference = 18,
    Folder = 19,
    EnumMember = 20,
    Constant = 21,
    Struct = 22,
    Event = 23,
    Operator = 24,
    TypeParameter = 25,
}

/// A completion item from the LSP server.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct CompletionItem {
    pub label: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub kind: Option<CompletionItemKind>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub documentation: Option<String>,
    #[serde(rename = "insertText", skip_serializing_if = "Option::is_none")]
    pub insert_text: Option<String>,
}

/// Hover result from the LSP server.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Hover {
    pub contents: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub range: Option<Range>,
}

/// Symbol kind for document symbols.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[repr(u8)]
pub enum SymbolKind {
    File = 1,
    Module = 2,
    Namespace = 3,
    Package = 4,
    Class = 5,
    Method = 6,
    Property = 7,
    Field = 8,
    Constructor = 9,
    Enum = 10,
    Interface = 11,
    Function = 12,
    Variable = 13,
    Constant = 14,
    String = 15,
    Number = 16,
    Boolean = 17,
    Array = 18,
    Object = 19,
    Key = 20,
    Null = 21,
    EnumMember = 22,
    Struct = 23,
    Event = 24,
    Operator = 25,
    TypeParameter = 26,
}

/// A document symbol (outline entry).
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct DocumentSymbol {
    pub name: String,
    pub kind: SymbolKind,
    pub range: Range,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    #[serde(default)]
    pub children: Vec<DocumentSymbol>,
}

/// LSP server configuration entry.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct LspServerConfig {
    /// Language ID this server handles (e.g. "rust", "python").
    pub language: String,
    /// Command to start the server.
    pub command: String,
    /// Command arguments.
    #[serde(default)]
    pub args: Vec<String>,
    /// Root path pattern (e.g. "Cargo.toml" for Rust).
    #[serde(default)]
    pub root_markers: Vec<String>,
}

/// Convert a file path to an LSP URI.
pub fn path_to_uri(path: &std::path::Path) -> String {
    // On Windows, paths start with drive letter like C:\
    let path_str = path.to_string_lossy().replace('\\', "/");
    if path_str.starts_with('/') {
        format!("file://{}", path_str)
    } else {
        format!("file:///{}", path_str)
    }
}

/// Convert an LSP URI back to a file path.
pub fn uri_to_path(uri: &str) -> Option<PathBuf> {
    let stripped = uri.strip_prefix("file:///")
        .or_else(|| uri.strip_prefix("file://"))?;
    Some(PathBuf::from(stripped.replace('/', &std::path::MAIN_SEPARATOR.to_string())))
}

/// Default LSP server configurations for common languages.
pub fn default_lsp_configs() -> Vec<LspServerConfig> {
    vec![
        LspServerConfig {
            language: "rust".into(),
            command: "rust-analyzer".into(),
            args: Vec::new(),
            root_markers: vec!["Cargo.toml".into()],
        },
        LspServerConfig {
            language: "python".into(),
            command: "pyright-langserver".into(),
            args: vec!["--stdio".into()],
            root_markers: vec!["pyproject.toml".into(), "setup.py".into()],
        },
        LspServerConfig {
            language: "javascript".into(),
            command: "typescript-language-server".into(),
            args: vec!["--stdio".into()],
            root_markers: vec!["package.json".into(), "tsconfig.json".into()],
        },
        LspServerConfig {
            language: "typescript".into(),
            command: "typescript-language-server".into(),
            args: vec!["--stdio".into()],
            root_markers: vec!["tsconfig.json".into(), "package.json".into()],
        },
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn position_creation() {
        let pos = Position::new(10, 5);
        assert_eq!(pos.line, 10);
        assert_eq!(pos.character, 5);
    }

    #[test]
    fn range_single_line() {
        let range = Range::single_line(5, 0, 10);
        assert_eq!(range.start.line, 5);
        assert_eq!(range.start.character, 0);
        assert_eq!(range.end.character, 10);
    }

    #[test]
    fn path_to_uri_unix() {
        let uri = path_to_uri(std::path::Path::new("/home/user/file.rs"));
        assert_eq!(uri, "file:///home/user/file.rs");
    }

    #[test]
    fn uri_to_path_roundtrip() {
        let path = uri_to_path("file:///home/user/file.rs");
        assert!(path.is_some());
    }

    #[test]
    fn diagnostic_severity_values() {
        assert_eq!(DiagnosticSeverity::Error as u8, 1);
        assert_eq!(DiagnosticSeverity::Warning as u8, 2);
        assert_eq!(DiagnosticSeverity::Information as u8, 3);
        assert_eq!(DiagnosticSeverity::Hint as u8, 4);
    }

    #[test]
    fn completion_item_kind_values() {
        assert_eq!(CompletionItemKind::Text as u8, 1);
        assert_eq!(CompletionItemKind::Function as u8, 3);
        assert_eq!(CompletionItemKind::Variable as u8, 6);
        assert_eq!(CompletionItemKind::Keyword as u8, 14);
    }

    #[test]
    fn symbol_kind_values() {
        assert_eq!(SymbolKind::Function as u8, 12);
        assert_eq!(SymbolKind::Variable as u8, 13);
        assert_eq!(SymbolKind::Struct as u8, 23);
    }

    #[test]
    fn default_lsp_configs_present() {
        let configs = default_lsp_configs();
        assert!(configs.len() >= 3);
        assert!(configs.iter().any(|c| c.language == "rust"));
        assert!(configs.iter().any(|c| c.language == "python"));
        assert!(configs.iter().any(|c| c.language == "javascript"));
    }

    #[test]
    fn diagnostic_serde_roundtrip() {
        let diag = Diagnostic {
            range: Range::single_line(0, 0, 5),
            severity: Some(DiagnosticSeverity::Error),
            message: "syntax error".into(),
            source: Some("rustc".into()),
        };
        let json = serde_json::to_string(&diag).unwrap();
        let parsed: Diagnostic = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.message, "syntax error");
        assert_eq!(parsed.severity, Some(DiagnosticSeverity::Error));
    }

    #[test]
    fn completion_item_serde() {
        let item = CompletionItem {
            label: "println!".into(),
            kind: Some(CompletionItemKind::Function),
            detail: Some("macro".into()),
            documentation: None,
            insert_text: Some("println!(\"$0\")".into()),
        };
        let json = serde_json::to_string(&item).unwrap();
        assert!(json.contains("println!"));
        let parsed: CompletionItem = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.label, "println!");
    }

    #[test]
    fn document_symbol_nested() {
        let sym = DocumentSymbol {
            name: "MyStruct".into(),
            kind: SymbolKind::Struct,
            range: Range::single_line(0, 0, 20),
            detail: None,
            children: vec![
                DocumentSymbol {
                    name: "new".into(),
                    kind: SymbolKind::Method,
                    range: Range::single_line(1, 4, 30),
                    detail: None,
                    children: Vec::new(),
                },
            ],
        };
        assert_eq!(sym.children.len(), 1);
        assert_eq!(sym.children[0].name, "new");
    }

    #[test]
    fn lsp_config_serde() {
        let config = LspServerConfig {
            language: "rust".into(),
            command: "rust-analyzer".into(),
            args: Vec::new(),
            root_markers: vec!["Cargo.toml".into()],
        };
        let json = serde_json::to_string(&config).unwrap();
        let parsed: LspServerConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.language, "rust");
    }
}
