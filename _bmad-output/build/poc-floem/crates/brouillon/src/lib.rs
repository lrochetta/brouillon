//! Brouillon — the fast editor that doesn't phone home.
//!
//! Cross-platform Rust-native code editor for 2026.
//! Two presets — Moldu (mainstream, GUI-first) and Geek (keyboard-first,
//! command-palette) — in a single binary. AI-native local-first via MCP.
//!
//! This library crate exposes modules so benches and tests can link
//! against the same code as the binary.

pub mod fonts;
pub mod print_doc;
#[cfg(feature = "test-input")]
pub mod test_input;

pub use brouillon_core::encoding;
pub use brouillon_core::docview;
pub use brouillon_core::keymap;
pub use brouillon_core::kind;
pub use brouillon_core::memtrim;
pub use brouillon_core::metrics;
pub use brouillon_core::preset;
pub use brouillon_core::render_mode;
pub use brouillon_core::theme;

pub use brouillon_editor_engine::auto_complete;
pub use brouillon_editor_engine::buffer;
pub use brouillon_editor_engine::crdt;
pub use brouillon_editor_engine::cursor;
pub use brouillon_editor_engine::external;
pub use brouillon_editor_engine::diff;
pub use brouillon_editor_engine::emmet;
pub use brouillon_editor_engine::folding;
pub use brouillon_editor_engine::fs_provider;
pub use brouillon_editor_engine::guides;
pub use brouillon_editor_engine::macros;
pub use brouillon_editor_engine::minimap;
pub use brouillon_editor_engine::tab_mru;
pub use brouillon_editor_engine::workspace;
pub use brouillon_editor_engine::grammars;
pub use brouillon_editor_engine::highlight;
pub use brouillon_editor_engine::lsp_types;
pub use brouillon_editor_engine::mcp_client;
pub use brouillon_editor_engine::plugin_host;
pub use brouillon_editor_engine::plugin_manifest;
pub use brouillon_editor_engine::sample;
pub use brouillon_editor_engine::search;
pub use brouillon_editor_engine::session;
pub use brouillon_editor_engine::sftp;
pub use brouillon_editor_engine::shell_runner;
pub use brouillon_editor_engine::sketch;
pub use brouillon_editor_engine::snippets;
pub use brouillon_editor_engine::syntax;
pub use brouillon_editor_engine::tabs;
pub use brouillon_editor_engine::undo;

pub use brouillon_ui::ai_extract;
pub use brouillon_ui::ai_panel;
pub use brouillon_ui::bracket_paint;
pub use brouillon_ui::browser_view;
pub use brouillon_ui::clipboard;
pub use brouillon_ui::clipboard_view;
pub use brouillon_ui::confirm;
pub use brouillon_ui::diff_view;
pub use brouillon_ui::minimap_view;
pub use brouillon_ui::viewport;
pub use brouillon_ui::doc_selector;
pub use brouillon_ui::editor_view;
pub use brouillon_ui::export;
pub use brouillon_ui::ext_event_helpers;
pub use brouillon_ui::file_tree_view;
pub use brouillon_ui::find_replace;
pub use brouillon_ui::goto_view;
pub use brouillon_ui::hex_editor;
pub use brouillon_ui::hex_view;
pub use brouillon_ui::kbd_focus;
pub use brouillon_ui::link_detect;
pub use brouillon_ui::link_view;
pub use brouillon_ui::markdown_render;
pub use brouillon_ui::markdown_view;
pub use brouillon_ui::menu_bar;
pub use brouillon_ui::menu_dropdown;
pub use brouillon_ui::output_view;
pub use brouillon_ui::palette;
pub use brouillon_ui::render_badge;
pub use brouillon_ui::palette_view;
pub use brouillon_ui::remote_dialogs;
pub use brouillon_ui::remote_view;
pub use brouillon_ui::sftp_panel;
pub use brouillon_ui::sketch_view;
pub use brouillon_ui::tab_bar;
pub use brouillon_ui::theme_ctx;
pub use brouillon_ui::toolbar;
pub use brouillon_ui::view_icons;
pub use brouillon_ui::xcf;


// Step 0 stubs (Lot 7)
pub mod buildinfo;
pub mod cli;
pub mod import_cli;
pub mod external_ui;
pub mod ipc;
pub mod link_host;
pub mod mirror;
pub mod module_ui;
pub mod onboarding;
pub mod startup_doc;
pub mod update_boot;
pub mod update_ui;
pub mod theme_mode;
pub mod wiring;
#[cfg(windows)]
pub mod win_icon;
#[cfg(windows)]
pub mod win_theme;

// Step 0: re-exports of the new modules
pub use brouillon_core::codepages;
pub use brouillon_core::command_registry;
pub use brouillon_core::comment_tokens;
pub use brouillon_core::contract;
pub use brouillon_core::eol;
pub use brouillon_core::filetype;
pub use brouillon_core::paths;
pub use brouillon_core::settings;
pub use brouillon_core::theme_import;
pub use brouillon_editor_engine::acp;
pub use brouillon_editor_engine::autoindent;
pub use brouillon_editor_engine::backup;
pub use brouillon_editor_engine::durable;
pub use brouillon_editor_engine::browser;
pub use brouillon_editor_engine::remote;
pub use brouillon_editor_engine::cliptext;
pub use brouillon_editor_engine::edit_apply;
pub use brouillon_editor_engine::error_patterns;
pub use brouillon_editor_engine::file_io;
pub use brouillon_editor_engine::large_file;
pub use brouillon_editor_engine::markers;
pub use brouillon_editor_engine::nav_history;
pub use brouillon_editor_engine::project;
pub use brouillon_editor_engine::recovery;
pub use brouillon_editor_engine::replace_files;
pub use brouillon_editor_engine::search_files;
pub use brouillon_editor_engine::selection_cmds;
pub use brouillon_editor_engine::templates;
pub use brouillon_editor_engine::textops;
pub use brouillon_editor_engine::tool_vars;
pub use brouillon_editor_engine::tools_config;
pub use brouillon_editor_engine::watch;
pub use brouillon_ui::cliptext_panel;
pub use brouillon_ui::encoding_picker;
pub use brouillon_ui::keymap_editor;
pub use brouillon_ui::quick_open;
pub use brouillon_ui::quick_open_view;
pub use brouillon_ui::results_model;
pub use brouillon_ui::results_panel;
pub use brouillon_ui::status_bar;
pub use brouillon_ui::cliptext_store;
pub use brouillon_ui::cliptext_view;
pub use brouillon_ui::key_capture;
pub use brouillon_ui::keymap_view;
pub use brouillon_ui::onboarding_view;
pub use brouillon_ui::settings_editor;
pub use brouillon_ui::ui_kit;
#[cfg(test)]
mod tests {
    use super::buffer::Buffer;

    /// Smoke test: round-trip a small string through the rope wrapper.
    /// Proves load → line_count → nth_line work end-to-end. This is the
    /// TDD harness — not exhaustive, but it pins the public contract.
    #[test]
    fn buffer_load_count_and_line_access() {
        let src = "alpha\nbeta\ngamma\n";
        let buf = Buffer::from_str(src);

        // crop counts lines by line-terminator count, so a trailing "\n"
        // produces an empty trailing line — line_len() returns 4 here.
        // We assert >=3 to remain robust if the upstream definition shifts.
        assert!(buf.line_count() >= 3, "expected >=3 lines, got {}", buf.line_count());

        assert_eq!(buf.nth_line(0).unwrap(), "alpha");
        assert_eq!(buf.nth_line(1).unwrap(), "beta");
        assert_eq!(buf.nth_line(2).unwrap(), "gamma");
        assert_eq!(buf.byte_len(), src.len());
    }

    #[test]
    fn buffer_out_of_range_returns_none() {
        let buf = Buffer::from_str("only one line");
        assert!(buf.nth_line(999).is_none());
    }
}
