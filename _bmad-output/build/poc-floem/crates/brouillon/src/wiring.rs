//! Single wiring point (step 0 rule, BK-067): the shell concatenates every
//! lot's `commands()` into ONE registry and routes `run` to the owning lot.
//!
//! Everything here is pure (no floem), so it is unit-tested. `main.rs` only
//! calls into this module and applies the results to its signals.
//!
//! Registration order: `command_registry` first, then the shell's own
//! commands (handled in `main.rs`), then each lot. Duplicates and invalid ids
//! are rejected by [`Registry::register_all`] and logged by [`build_registry`].
//!
//! Ids are stable snake_case. Legacy dotted ids that `main.rs`, the menus and
//! the toolbar still emit (`file.open`, `edit.find`...) go through [`canonical`].

use std::io;
use std::path::{Path, PathBuf};

use brouillon_core::command_registry::{self, Registry, RegistryError};
use brouillon_core::contract::{CmdCtx, CmdOutcome, CommandSpec, EditError, Region};
use brouillon_core::keymap::{self, Binding, ChordResolver, Context, KeyCombo, Resolution};
use brouillon_core::eol::EolKind;
use brouillon_editor_engine::backup::{self, BackupOptions, SaveError};
use brouillon_editor_engine::buffer::Buffer;
use brouillon_editor_engine::file_io::{self, LoadedDoc, SaveSpec};
use brouillon_editor_engine::tabs::{DocMeta, TabManager};
use brouillon_ui::status_bar::{DocStatus, Pane};

// ---------------------------------------------------------------------------
// Shell commands (handled by main.rs, not by a lot)
// ---------------------------------------------------------------------------

/// `(id, caption, category, default chord, implemented by the shell)`.
/// `implemented == false` entries are registered (so palette, menus and the
/// Classic overlay can name them) but never dispatched from a chord.
const SHELL: &[(&str, &str, &str, Option<&str>, bool)] = &[
    ("file_new", "File: New", "File", Some("ctrl+n"), true),
    ("file_open", "File: Open...", "File", Some("ctrl+o"), true),
    ("file_save", "File: Save", "File", Some("ctrl+s"), true),
    ("file_save_as", "File: Save As...", "File", Some("ctrl+shift+s"), true),
    ("file_close", "File: Close Tab", "File", Some("ctrl+w"), true),
    ("file_print", "File: Print...", "File", None, true),
    ("app_quit", "File: Quit", "File", None, true),
    ("project_load", "File: Open Project...", "File", None, true),
    ("project_save", "File: Save Project", "File", None, true),
    ("project_open_folder", "File: Open Folder...", "File", None, true),
    ("tab_next", "Tab: Next Tab", "Tab", Some("ctrl+tab"), true),
    ("tab_previous", "Tab: Previous Tab", "Tab", Some("ctrl+shift+tab"), true),
    ("tab_last_visited", "Tab: Last Visited Tab", "Tab", None, true),
    ("edit_cut", "Edit: Cut", "Edit", Some("ctrl+x"), true),
    ("edit_copy", "Edit: Copy", "Edit", Some("ctrl+c"), true),
    ("edit_paste", "Edit: Paste", "Edit", Some("ctrl+v"), true),
    ("select_all", "Edit: Select All", "Edit", Some("ctrl+a"), true),
    ("toggle_overwrite", "Edit: Toggle Insert / Overwrite", "Edit", None, true),
    ("search_open_replace", "Search: Replace...", "Search", Some("ctrl+h"), true),
    ("goto_function", "Navigate: Go to Function", "Navigate", None, true),
    ("markers_list", "Bookmarks: List", "Bookmarks", None, true),
    ("macro_record_toggle", "Macro: Record / Stop", "Macro", None, true),
    ("macro_play", "Macro: Play", "Macro", Some("ctrl+shift+q"), true),
    ("emmet_expand", "Edit: Expand Abbreviation (Emmet)", "Edit", Some("ctrl+e"), true),
    ("view_theme_cycle", "View: Cycle Theme", "View", Some("ctrl+t"), true),
    ("theme_dark", "View: Theme: Dark", "View", None, true),
    ("theme_light", "View: Theme: Light", "View", None, true),
    ("theme_system", "View: Theme: Follow System", "View", None, true),
    ("view_toggle_mode", "View: Toggle Moldu / Geek Mode", "View", None, true),
    ("view_toggle_crayon", "View: Toggle Crayon Mode", "View", None, true),
    ("view_toggle_preview", "View: Toggle Markdown Preview", "View", Some("ctrl+shift+v"), true),
    // Ctrl+Alt+letter chords are kept: main.rs `chord_key_name` now detects them on AZERTY
    // (left Alt; the right Alt = AltGr deliberately types characters instead).
    ("view_toggle_ai", "View: Toggle AI Assistant Panel", "View", Some("ctrl+alt+a"), true),
    ("view_toggle_sftp", "View: Toggle Local SFTP Simulation Panel (no network)", "View", Some("ctrl+alt+s"), true),
    ("browser_toggle", "Browser: Toggle", "Browser", None, true),
    ("browser_switch_source", "Browser: Switch Source...", "Browser", None, true),
    ("browser_focus", "Browser: Focus File List (keyboard)", "Browser", None, true),
    ("browser_go_up", "Browser: Go Up", "Browser", None, true),
    ("browser_reveal", "Browser: Reveal Active File", "Browser", None, true),
    ("remote_connect", "Remote: Connect...", "Remote", None, true),
    ("remote_sites", "Remote: Manage Sites...", "Remote", None, true),
    ("remote_demo", "Remote: Connect to Demo Server (built-in fake, no network)", "Remote", None, true),
    ("remote_disconnect", "Remote: Disconnect", "Remote", None, true),
    ("remote_refresh", "Remote: Refresh Folder", "Remote", None, true),
    ("remote_status", "Remote: Show Status of This Document", "Remote", None, true),
    ("remote_recover", "Remote: Recover unsaved changes", "Remote", None, true),
    ("credentials_show", "Credentials: Show", "Credentials", None, true),
    ("credentials_panic", "Credentials: Panic (lock and forget secrets in memory)", "Credentials", None, true),
    ("remote_save_again", "Remote: Save again (retry a pending or failed save)", "Remote", None, true),
    ("view_toggle_hex", "View: Toggle Hex View", "View", Some("ctrl+shift+h"), true),
    // Hex mode (kept in step with `brouillon_ui::hex_editor::COMMANDS`, pinned by a test). Not agent-runnable.
    ("hex_goto", "Hex: Go to Offset...", "Hex", None, true),
    ("hex_toggle_inspector", "Hex: Toggle Data Inspector", "Hex", None, true),
    ("hex_toggle_regions", "Hex: Toggle Regions Panel", "Hex", None, true),
    ("hex_add_region", "Hex: Add Region from Selection", "Hex", None, true),
    ("hex_toggle_endian", "Hex: Toggle Little/Big Endian", "Hex", None, true),
    ("hex_hash_selection", "Hex: Hash Selection (CRC-32, MD5, SHA-1, SHA-256)", "Hex", None, true),
    ("hex_copy_as_hex", "Hex: Copy as hex string", "Hex", None, true),
    ("hex_copy_as_spaced_hex", "Hex: Copy as spaced hex", "Hex", None, true),
    ("hex_copy_as_c_array", "Hex: Copy as C array", "Hex", None, true),
    ("hex_copy_as_rust_array", "Hex: Copy as Rust array", "Hex", None, true),
    ("hex_copy_as_python_bytes", "Hex: Copy as Python bytes", "Hex", None, true),
    ("hex_copy_as_base64", "Hex: Copy as Base64", "Hex", None, true),
    ("hex_copy_as_ascii", "Hex: Copy as ASCII text", "Hex", None, true),
    ("view_toggle_line_numbers", "View: Toggle Line Numbers", "View", None, true),
    ("view_toggle_tree", "View: Toggle Folder Tree", "View", Some("ctrl+shift+e"), true),
    ("view_toggle_minimap", "View: Toggle Minimap", "View", None, true),
    ("view_toggle_sidebar", "View: Toggle Sidebar", "View", Some("ctrl+b"), true),
    ("view_tabs_position_toggle", "View: Tabs at Top / Bottom", "View", None, true),
    ("diff_with_disk", "View: Compare with File on Disk", "View", None, true),
    ("diff_with_last_tab", "View: Compare with Last Visited Tab", "View", None, true),
    ("diff_close", "View: Close Comparison", "View", None, true),
    ("view_toggle_indent_guides", "View: Toggle Indent Guides", "View", None, true),
    ("view_toggle_ruler", "View: Toggle Column Ruler", "View", None, true),
    ("view_distraction_free", "View: Toggle Distraction-Free Mode", "View", Some("ctrl+alt+z"), true),
    ("view_command_palette", "View: Command Palette", "View", Some("ctrl+k"), true),
    ("preset_moldu", "Preset: Moldu", "Preset", None, true),
    ("preset_geek", "Preset: Geek", "Preset", None, true),
    ("tools_infinite", "Tools: Toggle Infinite Layout", "Tools", None, true),
    // Kill switch of Brouillon Link: always available, also while a dialog is open (see main.rs).
    ("link_kill", "Agents: Stop (kill switch)", "Agents", Some("ctrl+alt+k"), true),
    ("tools_import_settings", "Tools: Import Settings from Another Editor...", "Tools", None, true),
    ("help_first_run", "Help: First-Run Setup...", "Help", None, true),
    ("help_try_agent", "Help: Try an Agent...", "Help", None, true),
    ("help_about", "Help: About Brouillon...", "Help", None, true),
    ("help_shortcuts", "Help: Show Shortcuts", "Help", None, true),
    ("help_welcome", "Help: Welcome", "Help", None, true),
    ("help_docs", "Help: Documentation", "Help", None, true),
    ("help_release_notes", "Help: Release Notes", "Help", None, true),
];

fn shell_commands() -> Vec<CommandSpec> {
    SHELL
        .iter()
        .map(|(id, caption, category, chord, _)| CommandSpec {
            id: (*id).into(),
            caption: (*caption).into(),
            category: (*category).into(),
            default_chord: chord.map(Into::into),
            ..Default::default()
        })
        .collect()
}

/// Legacy ids (palette defaults, menus, toolbar, old `dispatch` arms) to the
/// stable snake_case ids.
const ALIASES: &[(&str, &str)] = &[
    ("file_import_editplus", "tools_import_settings"),
    ("file.open", "file_open"),
    ("file.save", "file_save"),
    ("file.save-as", "file_save_as"),
    ("file.new-tab", "file_new"),
    ("file.close-tab", "file_close"),
    ("edit.undo", "edit_undo"),
    ("edit.redo", "edit_redo"),
    ("edit.cut", "edit_cut"),
    ("edit.copy", "edit_copy"),
    ("edit.paste", "edit_paste"),
    ("edit.find", "search_open_find"),
    ("edit.replace", "search_open_replace"),
    ("edit.go-to-line", "go_to"),
    ("view.theme.cycle", "view_theme_cycle"),
    ("theme.dark", "theme_dark"),
    ("theme.light", "theme_light"),
    ("theme.system", "theme_system"),
    ("view.toggle-mode", "view_toggle_mode"),
    ("view.toggle-crayon", "view_toggle_crayon"),
    ("view.toggle-preview", "view_toggle_preview"),
    ("view.toggle-ai", "view_toggle_ai"),
    ("view.toggle-sftp", "view_toggle_sftp"),
    ("view.command-palette", "view_command_palette"),
    ("preset.moldu", "preset_moldu"),
    ("preset.geek", "preset_geek"),
    ("tools.infinite", "tools_infinite"),
    ("help.about", "help_about"),
    ("help.shortcuts", "help_shortcuts"),
    ("app.quit", "app_quit"),
    // status bar pane commands
    ("goto_line", "go_to"),
];

/// Canonical (snake_case) id for `id`; unknown ids are returned unchanged.
pub fn canonical(id: &str) -> String {
    ALIASES.iter().find(|(old, _)| *old == id).map_or_else(|| id.to_string(), |(_, new)| (*new).to_string())
}

// ---------------------------------------------------------------------------
// Registry
// ---------------------------------------------------------------------------

/// Every lot's specs in registration order (before annotation).
fn lot_specs() -> Vec<CommandSpec> {
    let mut v = Vec::new();
    v.extend(command_registry::commands());
    v.extend(shell_commands());
    v.extend(brouillon_editor_engine::edit_apply::commands());
    v.extend(brouillon_editor_engine::file_io::commands());
    v.extend(brouillon_editor_engine::search::commands());
    v.extend(brouillon_editor_engine::search_files::commands());
    v.extend(brouillon_editor_engine::textops::commands());
    v.extend(brouillon_editor_engine::autoindent::commands());
    v.extend(brouillon_editor_engine::selection_cmds::commands());
    v.extend(brouillon_editor_engine::classic_cmds::commands());
    v.extend(brouillon_editor_engine::recovery::commands());
    v.extend(brouillon_editor_engine::cliptext::commands());
    v.extend(brouillon_editor_engine::acp::commands());
    v.extend(brouillon_editor_engine::templates::commands());
    v.extend(brouillon_editor_engine::tools_config::commands());
    v.extend(brouillon_import::commands());
    v
}

/// Commands that can change text or files (MCP clients must ask first).
fn mutates(id: &str, category: &str) -> bool {
    match category {
        "Case" | "Whitespace" | "Sort" | "Format" | "Fill" | "Comment" | "Cliptext" | "Import" => true,
        "Transform" => !matches!(id, "count_text" | "sum_numbers"),
        _ => matches!(
            id,
            "edit_undo" | "edit_redo" | "edit_undo_all" | "edit_undo_to_save" | "edit_cut" | "edit_paste" | "edit_paste_column"
                | "file_save" | "file_save_as" | "file_new_from_template" | "abbrev_expand" | "newline_auto_indent"
                | "search_replace_all" | "replace_in_files" | "eol_to_lf" | "eol_to_crlf" | "eol_to_cr" | "eol_cycle"
                | "encoding_reopen_as" | "encoding_convert_to" | "delete_line" | "delete_word" | "delete_word_back"
                | "delete_to_line_end" | "delete_to_line_start" | "duplicate_line" | "move_line_up" | "move_line_down"
                | "join_lines" | "join_lines_space" | "split_lines" | "transpose_chars" | "transpose_lines"
                | "insert_line_above" | "insert_line_below" | "reverse_lines" | "delete_duplicate_lines"
                | "delete_consecutive_duplicate_lines" | "delete_blank_lines" | "delete_whitespace_only_lines"
                | "insert_datetime" | "toggle_line_comment" | "toggle_block_comment" | "file.recover"
        ) || id.starts_with("tools_run_slot_"),
    }
}

/// JSON Schema (as JSON text) of the arguments some commands take.
fn args_schema(id: &str) -> Option<&'static str> {
    Some(match id {
        "search_find_next" | "search_find_prev" | "search_count" => {
            r#"{"type":"array","items":{"type":"string"},"description":"[pattern, flags]; flags: c case, w word, e extended, r regex, s dot-all, x free-spacing, S selection, n no-wrap"}"#
        }
        "search_replace_all" => {
            r#"{"type":"array","items":{"type":"string"},"description":"[pattern, flags, replacement]"}"#
        }
        "encoding_reopen_as" | "encoding_convert_to" => {
            r#"{"type":"array","items":{"type":"string"},"minItems":1,"description":"[codepage id]"}"#
        }
        "insert_datetime" => r#"{"type":"array","items":{"type":"string"},"description":"[strftime-like pattern]"}"#,
        "go_to" => r#"{"type":"array","items":{"type":"string"},"minItems":1,"description":"[line | line:col | @offset]"}"#,
        "import_editplus_folder" | "import_theme" | "import_stx" => {
            r#"{"type":"array","items":{"type":"string"},"minItems":2,"description":"[source, destination]"}"#
        }
        _ => return None,
    })
}

/// Orchestrator post-processing of a lot's spec: `mutates`, `args_schema`, and
/// `when` guards that tell F4 / Shift+F4 apart by active panel.
fn annotate(mut s: CommandSpec) -> CommandSpec {
    s.mutates = s.mutates || mutates(&s.id, &s.category);
    if s.args_schema.is_none() {
        s.args_schema = args_schema(&s.id).map(Into::into);
    }
    if s.when.is_none() {
        s.when = match s.id.as_str() {
            "results_next_hit" | "results_prev_hit" => Some("active_panel == results".into()),
            "tools_next_error" | "tools_prev_error" => Some("active_panel == output".into()),
            _ => None,
        };
    }
    // Chord clashes between lots and long-standing shell shortcuts are settled
    // here: the shell keeps its shortcut, the lot's command stays in the palette.
    if matches!(s.id.as_str(), "replace_in_files" | "select_inside_brackets" | "undo_last_added" | "go_to") {
        s.default_chord = None;
    }
    s
}

/// Build the registry. Rejected specs (duplicates, bad ids) are returned
/// as readable messages; the rest is kept.
pub fn build_registry() -> (Registry, Vec<String>) {
    let mut reg = Registry::new();
    let errs: Vec<RegistryError> = reg.register_all(lot_specs().into_iter().map(annotate).collect());
    (reg, errs.iter().map(|e| e.to_string()).collect())
}

/// Route a command to the lot that owns it. `None` = not a lot command (the
/// shell handles it, or it is unknown).
pub fn run(id: &str, ctx: &CmdCtx) -> Option<CmdOutcome> {
    let id = &canonical(id);
    command_registry::run(id, ctx)
        .or_else(|| brouillon_editor_engine::edit_apply::run(id, ctx))
        .or_else(|| brouillon_editor_engine::file_io::run(id, ctx))
        .or_else(|| brouillon_editor_engine::search::run(id, ctx))
        .or_else(|| brouillon_editor_engine::search_files::run(id, ctx))
        .or_else(|| brouillon_editor_engine::textops::run(id, ctx))
        .or_else(|| brouillon_editor_engine::autoindent::run(id, ctx))
        .or_else(|| brouillon_editor_engine::selection_cmds::run(id, ctx))
        .or_else(|| brouillon_editor_engine::classic_cmds::run(id, ctx))
        .or_else(|| brouillon_editor_engine::recovery::run(id, ctx))
        .or_else(|| brouillon_editor_engine::cliptext::run(id, ctx))
        .or_else(|| brouillon_editor_engine::acp::run(id, ctx))
        .or_else(|| brouillon_editor_engine::templates::run(id, ctx))
        .or_else(|| brouillon_editor_engine::tools_config::run(id, ctx))
        .or_else(|| brouillon_import::run(id, ctx))
}

/// True when the shell itself implements `id` (see [`SHELL`]).
pub fn shell_implements(id: &str) -> bool {
    SHELL.iter().any(|(sid, _, _, _, ok)| *sid == id && *ok)
        || matches!(
            id,
            "edit_undo" | "edit_redo" | "edit_undo_all" | "edit_undo_to_save" | "bookmark_toggle" | "bookmark_clear" | "bookmark_invert" | "nav_back" | "nav_forward"
        )
}

/// Commands the shell executes by a chord: its own, or a lot command that
/// returns an outcome.
pub fn dispatchable(id: &str) -> bool {
    shell_implements(id) || (!SHELL.iter().any(|(sid, ..)| *sid == id) && run(id, &CmdCtx::default()).is_some())
}

// ---------------------------------------------------------------------------
// Keymap
// ---------------------------------------------------------------------------

/// Classic ids (lot 10 table) that differ from the id the owning lot registered.
const CLASSIC_REMAP: &[(&str, &str)] = &[
    ("find_open", "search_open_find"),
    ("replace_open", "search_open_replace"),
    ("find_next_word", "search_find_next_word"),
    ("find_previous_word", "search_find_prev_word"),
    ("goto_line", "go_to"),
    ("delete_word_end", "delete_word"),
    ("delete_line_end", "delete_to_line_end"),
    ("line_duplicate", "duplicate_line"),
    ("lines_join", "join_lines"),
    ("lines_split", "split_lines"),
    ("text_transpose", "transpose_chars"),
    ("comment_toggle", "toggle_line_comment"),
    ("insert_date_short", "insert_datetime"),
    ("markers_clear", "bookmark_clear"),
    ("tool_run_1", "tools_run_slot_1"),
    ("tool_run_2", "tools_run_slot_2"),
    ("tool_run_3", "tools_run_slot_3"),
    ("tool_run_4", "tools_run_slot_4"),
    ("tool_run_5", "tools_run_slot_5"),
    ("tool_run_6", "tools_run_slot_6"),
    ("tool_run_7", "tools_run_slot_7"),
    ("tool_run_8", "tools_run_slot_8"),
    ("tool_run_9", "tools_run_slot_9"),
    ("tool_run_10", "tools_run_slot_10"),
];

/// The Classic overlay reconciled with the registered ids: each lot-10 id is
/// remapped when needed, then bindings to unknown commands are dropped
/// (`retain_known`).
pub fn classic_bindings(reg: &Registry) -> Vec<Binding> {
    let remapped = keymap::classic_bindings()
        .into_iter()
        .map(|mut b| {
            if let Some((_, new)) = CLASSIC_REMAP.iter().find(|(old, _)| *old == b.command) {
                b.command = (*new).to_string();
            }
            b
        })
        .collect();
    keymap::retain_known(remapped, |id| reg.contains(id))
}

/// Chord table the shell dispatches: registry defaults, the Classic overlay if
/// asked, then the user layer; limited to commands the shell can execute.
pub fn dispatch_bindings(reg: &Registry, classic: bool, user: &[Binding]) -> Vec<Binding> {
    let defaults = keymap::bindings_from_specs(reg.all());
    let classic_layer = if classic { classic_bindings(reg) } else { Vec::new() };
    let all = keymap::compose(&[&defaults, &classic_layer, user]);
    // An unbinding (empty command) must stay so it shadows lower layers.
    keymap::retain_known(all, dispatchable)
}

/// Chords below the user layer as the shortcut editor shows them: registry defaults plus the
/// Classic overlay when active, whether or not the shell can execute the command yet.
pub fn base_bindings(reg: &Registry, classic: bool) -> Vec<Binding> {
    let defaults = keymap::bindings_from_specs(reg.all());
    let classic_layer = if classic { classic_bindings(reg) } else { Vec::new() };
    keymap::compose(&[&defaults, &classic_layer])
}

/// `(id, caption)` of every registered command, by caption (the shortcut editor's table).
pub fn command_list(reg: &Registry) -> Vec<(String, String)> {
    let mut v: Vec<(String, String)> = reg.all().iter().map(|s| (s.id.clone(), s.caption.clone())).collect();
    v.sort_by_key(|(_, c)| c.to_lowercase());
    v
}

/// Combo from the pieces `main.rs` extracts from a floem key event.
pub fn combo(ctrl: bool, shift: bool, alt: bool, key: &str) -> KeyCombo {
    KeyCombo::new(ctrl, shift, alt, key)
}

/// Stateful chord dispatcher: resolver + bindings + `when` context.
pub struct Dispatcher {
    pub resolver: ChordResolver,
    pub bindings: Vec<Binding>,
    pub ctx: Context,
}

impl Dispatcher {
    pub fn new(bindings: Vec<Binding>) -> Self {
        Self { resolver: ChordResolver::default(), bindings, ctx: Context::new() }
    }

    pub fn press(&mut self, combo: KeyCombo, now_ms: u64) -> Resolution {
        self.resolver.feed(combo, now_ms, &self.bindings, &self.ctx)
    }
}

// ---------------------------------------------------------------------------
// Applying a command outcome
// ---------------------------------------------------------------------------

/// Apply `out.edits` to a copy of `buf` as ONE undo group (atomic: on error
/// the original is untouched and returned in `Err`). Selections to restore on
/// undo / redo are recorded with the group.
pub fn apply_outcome(buf: &Buffer, out: &CmdOutcome, sel_before: &[Region]) -> Result<Buffer, EditError> {
    use brouillon_editor_engine::buffer::EditMeta;
    let mut nb = buf.clone();
    if !out.edits.is_empty() {
        nb.break_typing();
        let meta = EditMeta {
            selection_before: Some(sel_before.to_vec()),
            selection_after: out.selection.clone(),
            ..EditMeta::default()
        };
        nb.apply_edits_with(&out.edits, meta)?;
    }
    Ok(nb)
}

// ---------------------------------------------------------------------------
// Caret / selection conversion (1-based line, col  <->  byte regions)
// ---------------------------------------------------------------------------

/// Regions for a command context from the shell's 1-based caret and optional
/// selection `(anchor, head)`. Always returns at least one region.
pub fn regions_from(text: &str, cursor: (usize, usize), selection: Option<((usize, usize), (usize, usize))>) -> Vec<Region> {
    use brouillon_editor_engine::cursor::line_col_to_byte;
    let at = |(l, c): (usize, usize)| line_col_to_byte(text, l.saturating_sub(1), c.saturating_sub(1));
    match selection {
        Some((a, h)) if a != h => vec![Region { anchor: at(a), head: at(h) }],
        _ => vec![Region::caret(at(cursor))],
    }
}

/// All regions of the multi-caret state: the extra carets (1-based line, col) first, the primary
/// (caret or selection) LAST, so that `collapse_to_primary` and `split_regions` agree.
pub fn regions_with_extras(
    text: &str,
    cursor: (usize, usize),
    selection: Option<((usize, usize), (usize, usize))>,
    extras: &[(usize, usize)],
) -> Vec<Region> {
    let mut v: Vec<Region> = extras.iter().map(|&p| regions_from(text, p, None)[0]).collect();
    v.extend(regions_from(text, cursor, selection));
    v
}

/// Inverse of `regions_with_extras`: the LAST region becomes the primary (anchor, head), the
/// others become extra carets at their head (an extra never keeps a selection), duplicates dropped.
pub fn split_regions(text: &str, regions: &[Region]) -> ((usize, usize), (usize, usize), Vec<(usize, usize)>) {
    let last = regions.last().copied().unwrap_or_else(|| Region::caret(0));
    let anchor = line_col_of(text, last.anchor);
    let head = line_col_of(text, last.head);
    let mut extras: Vec<(usize, usize)> = Vec::new();
    for r in &regions[..regions.len().saturating_sub(1)] {
        let p = line_col_of(text, r.head);
        if p != head && !extras.contains(&p) {
            extras.push(p);
        }
    }
    (anchor, head, extras)
}

// -- Buffer-based conversions: no copy of the document -----------------------
//
// The `&str` versions above need the whole text as one `String`. On a 100 MB file that is a 100 MB
// copy (and a linear scan) per keystroke. These read the rope directly: O(log n) and no allocation.

/// Byte offset of a 1-based `(line, col)` (columns count characters, clamped to the line end, a
/// line past the end clamps to the last one). Same result as `cursor::line_col_to_byte` on the text.
pub fn byte_of_line_col(buf: &Buffer, (l, c): (usize, usize)) -> usize {
    let n = buf.line_count();
    let line = l.saturating_sub(1).min(n.saturating_sub(1));
    let Some(range) = buf.line_byte_range(line) else { return buf.byte_len() };
    let start = range.start;
    let mut off = 0;
    let mut remaining = c.saturating_sub(1);
    // Stream the line instead of copying it: a one-line 100 MB file must not cost 100 MB per keystroke.
    buf.visit_str(range, &mut |piece| {
        for ch in piece.chars() {
            if remaining == 0 || ch == '\n' || ch == '\r' {
                remaining = 0;
                return false;
            }
            off += ch.len_utf8();
            remaining -= 1;
        }
        true
    });
    start + off
}

/// 1-based `(line, col)` of a byte offset, read from the buffer.
pub fn line_col_of_buf(buf: &Buffer, byte: usize) -> (usize, usize) {
    let byte = byte.min(buf.byte_len());
    let line = buf.line_of_byte(byte);
    let start = buf.byte_of_line(line);
    let mut cols = 0;
    buf.visit_str(start..byte, &mut |piece| {
        cols += piece.chars().count();
        true
    });
    (line + 1, cols + 1)
}

/// `regions_from` without a text copy.
pub fn regions_from_buf(buf: &Buffer, cursor: (usize, usize), selection: Option<((usize, usize), (usize, usize))>) -> Vec<Region> {
    match selection {
        Some((a, h)) if a != h => vec![Region { anchor: byte_of_line_col(buf, a), head: byte_of_line_col(buf, h) }],
        _ => vec![Region::caret(byte_of_line_col(buf, cursor))],
    }
}

/// `regions_with_extras` without a text copy.
pub fn regions_with_extras_buf(
    buf: &Buffer,
    cursor: (usize, usize),
    selection: Option<((usize, usize), (usize, usize))>,
    extras: &[(usize, usize)],
) -> Vec<Region> {
    let mut v: Vec<Region> = extras.iter().map(|&p| Region::caret(byte_of_line_col(buf, p))).collect();
    v.extend(regions_from_buf(buf, cursor, selection));
    v
}

/// `split_regions` without a text copy.
pub fn split_regions_buf(buf: &Buffer, regions: &[Region]) -> ((usize, usize), (usize, usize), Vec<(usize, usize)>) {
    let last = regions.last().copied().unwrap_or_else(|| Region::caret(0));
    let anchor = line_col_of_buf(buf, last.anchor);
    let head = line_col_of_buf(buf, last.head);
    let mut extras: Vec<(usize, usize)> = Vec::new();
    for r in &regions[..regions.len().saturating_sub(1)] {
        let p = line_col_of_buf(buf, r.head);
        if p != head && !extras.contains(&p) {
            extras.push(p);
        }
    }
    (anchor, head, extras)
}

/// Documents up to this size are handed whole to the text-based editing helpers (unchanged
/// behaviour); above it they get a window of lines around the caret.
pub const WINDOW_WHOLE_MAX: usize = 1 << 20;
/// The first `max_bytes` of the rope (cut at a character boundary) as a `String`: enough for language
/// detection, which only reads the first line, without copying a large document.
pub fn rope_head(rope: &crop::Rope, max_bytes: usize) -> String {
    let mut end = max_bytes.min(rope.byte_len());
    while !rope.is_char_boundary(end) {
        end -= 1;
    }
    rope.byte_slice(..end).to_string()
}

/// Lines `first..last` that word completion scans for the caret line `caret_line`: every line when the rope is
/// at most `budget_bytes`, else the lines within `budget_bytes / 2` bytes on each side of the caret line. The
/// caret line is always inside.
pub fn completion_window(rope: &crop::Rope, caret_line: usize, budget_bytes: usize) -> (usize, usize) {
    let lines = rope.line_len();
    if rope.byte_len() <= budget_bytes || lines == 0 {
        return (0, lines);
    }
    let caret_line = caret_line.min(lines - 1);
    let at = rope.byte_of_line(caret_line);
    let lo = at.saturating_sub(budget_bytes / 2);
    let hi = (at + budget_bytes / 2).min(rope.byte_len());
    let first = rope.line_of_byte(lo).min(caret_line);
    let last = (rope.line_of_byte(hi.min(rope.byte_len().saturating_sub(1))) + 1).min(lines).max(caret_line + 1);
    (first, last)
}

/// Lines kept on each side of the caret line in a window.
pub const WINDOW_MARGIN_LINES: usize = 50;

/// A piece of the document around a caret, for edits that only look at nearby text (Enter,
/// Delete, overwrite). `base` is the document byte offset of `text[0]`.
pub struct Window {
    pub text: String,
    pub base: usize,
}

impl Window {
    /// The whole text for a document up to [`WINDOW_WHOLE_MAX`], else the lines around `byte`.
    pub fn around(buf: &Buffer, byte: usize) -> Self {
        let len = buf.byte_len();
        if len <= WINDOW_WHOLE_MAX {
            return Self { text: buf.text(), base: 0 };
        }
        let line = buf.line_of_byte(byte.min(len));
        let lo = line.saturating_sub(WINDOW_MARGIN_LINES);
        let start = buf.byte_of_line(lo);
        let end = buf.byte_of_line(line + WINDOW_MARGIN_LINES + 1);
        Self { text: buf.slice(start..end), base: start }
    }

    /// Document regions to window coordinates.
    pub fn rel(&self, regions: &[Region]) -> Vec<Region> {
        regions.iter().map(|r| Region { anchor: r.anchor.saturating_sub(self.base), head: r.head.saturating_sub(self.base) }).collect()
    }

    /// Window regions back to document coordinates.
    pub fn abs_regions(&self, regions: Vec<Region>) -> Vec<Region> {
        regions.into_iter().map(|r| Region { anchor: r.anchor + self.base, head: r.head + self.base }).collect()
    }

    /// Window edits back to document coordinates.
    pub fn abs_edits(&self, mut edits: brouillon_core::contract::EditSet) -> brouillon_core::contract::EditSet {
        for e in &mut edits.0 {
            e.start_byte += self.base;
            e.end_byte += self.base;
        }
        edits
    }

    /// A whole command outcome back to document coordinates.
    pub fn abs_outcome(&self, mut out: CmdOutcome) -> CmdOutcome {
        out.edits = self.abs_edits(out.edits);
        out.selection = out.selection.map(|s| self.abs_regions(s));
        out
    }
}

/// 1-based `(line, col)` of a byte offset.
pub fn line_col_of(text: &str, byte: usize) -> (usize, usize) {
    let (l, c) = brouillon_editor_engine::cursor::byte_to_line_col(text, byte);
    (l + 1, c + 1)
}

// ---------------------------------------------------------------------------
// Files
// ---------------------------------------------------------------------------

/// Read a file faithfully (codepage, BOM and EOL remembered).
pub fn open_file(path: &Path) -> io::Result<LoadedDoc> {
    file_io::read_file(path, "windows-1252")
}

#[derive(Debug)]
pub enum SaveResult {
    Saved { backup: Option<PathBuf> },
    /// The target codepage cannot hold `unmappable` characters; ask the user, then retry with `accept_lossy`.
    Lossy { unmappable: usize, codepage: &'static str },
    Failed(String),
}

/// Encode `text` per `meta` and write it atomically with an optional backup.
pub fn save_document(path: &Path, text: &str, meta: &DocMeta, accept_lossy: bool, backup_opts: &BackupOptions) -> SaveResult {
    let spec = SaveSpec { codepage: meta.codepage, bom: meta.bom, eol: None };
    let prepared = match file_io::prepare_save(text, &spec) {
        Ok(p) => p,
        Err(e) => return SaveResult::Failed(e.to_string()),
    };
    if prepared.unmappable > 0 && !accept_lossy {
        return SaveResult::Lossy { unmappable: prepared.unmappable, codepage: meta.codepage };
    }
    match backup::save_with_backup(path, &prepared.bytes, backup_opts) {
        Ok(backup) => SaveResult::Saved { backup },
        Err(e @ (SaveError::ReadOnly(_) | SaveError::Backup { .. } | SaveError::Write { .. })) => SaveResult::Failed(e.to_string()),
    }
}

/// Save a LAZY (large file mode) document: UTF-8 only (BOM kept), no re-encoding, no EOL conversion. The pieces
/// are streamed into a temp file next to the target, fsynced and renamed over it; the same `.bak` copy as every
/// save is made first. UNLIKE `save_document_rope` there is deliberately NO in-place fallback: the document reads
/// from the very file being replaced, so a truncating in-place write would destroy the text still to be written.
/// If the rename is refused the save fails and the document stays intact in memory.
pub fn save_document_lazy(path: &Path, buf: &Buffer, backup_opts: &BackupOptions) -> SaveResult {
    use std::io::Write;
    if buf.is_read_only() {
        return SaveResult::Failed("this file is not valid UTF-8: it is open read-only, use the hex view or reopen it with an encoding".into());
    }
    if let Ok(m) = std::fs::metadata(path) {
        if m.permissions().readonly() {
            return SaveResult::Failed(format!("{} is read-only", path.display()));
        }
    }
    let backup = match backup::make_backup(path, backup_opts) {
        Ok(b) => b,
        Err(e) => return SaveResult::Failed(e.to_string()),
    };
    // `.<name>.brouillon-<pid>.tmp`: the name an interrupted save leaves behind is recognised and swept at the
    // next save / launch (`persist::sweep_orphan_temps`).
    brouillon_editor_engine::recovery::sweep_save_temps(path);
    let mut tmp_name = std::ffi::OsString::from(".");
    tmp_name.push(path.file_name().unwrap_or_default());
    tmp_name.push(format!(".brouillon-{}.tmp", std::process::id()));
    let tmp = path.with_file_name(tmp_name);
    let written = (|| -> io::Result<()> {
        let mut w = io::BufWriter::with_capacity(1 << 20, std::fs::File::create(&tmp)?);
        if buf.lazy_has_bom() {
            w.write_all(&[0xEF, 0xBB, 0xBF])?;
        }
        buf.write_to(&mut w)?;
        w.flush()?;
        w.get_ref().sync_all()?;
        Ok(())
    })();
    let result = written.and_then(|()| std::fs::rename(&tmp, path));
    match result {
        Ok(()) => SaveResult::Saved { backup },
        Err(e) => {
            let _ = std::fs::remove_file(&tmp);
            SaveResult::Failed(e.to_string())
        }
    }
}

/// [`save_document_lazy`] for a lazy document whose file is NOT UTF-8 (UTF-16/32, a legacy codepage, opened
/// through the transcoding sidecar): the text is streamed from an immutable snapshot through the encoder of
/// `meta.codepage` (BOM kept), with the same backup and the same atomic temp-file save. Nothing is copied
/// whole: window of 256 KiB plus codec scratch. Unmappable characters give `Lossy` unless `accept_lossy`.
pub fn save_document_lazy_encoded(path: &Path, buf: &Buffer, meta: &DocMeta, accept_lossy: bool, backup_opts: &BackupOptions) -> SaveResult {
    use brouillon_editor_engine::byte_source::{ByteSource, Ctl};
    use brouillon_editor_engine::stream_codec::{count_unmappable_stream, encode_to_sink, EncodeSpec, StreamCfg};
    use std::io::Write;
    if buf.is_read_only() {
        return SaveResult::Failed("this file is open read-only".into());
    }
    let snap = buf.snapshot();
    let spec = EncodeSpec { codepage: meta.codepage, bom: meta.bom, eol: None };
    let cfg = StreamCfg::default();
    let cancel = brouillon_editor_engine::search::CancelToken::new();
    let unmappable = match count_unmappable_stream(&snap, &spec, &cfg, &mut Ctl::new(&cancel)) {
        Ok(n) => n,
        Err(e) => return SaveResult::Failed(e.to_string()),
    };
    if unmappable > 0 && !accept_lossy {
        return SaveResult::Lossy { unmappable, codepage: meta.codepage };
    }
    let fill = |f: &mut std::fs::File| -> io::Result<()> {
        let mut w = io::BufWriter::with_capacity(1 << 20, f);
        encode_to_sink(&snap, 0..snap.len(), &spec, &cfg, &mut Ctl::new(&cancel), &mut |b| w.write_all(b)).map_err(|e| match e {
            file_io::FileIoError::Io(e) => e,
            other => io::Error::other(other.to_string()),
        })?;
        w.flush()
    };
    match backup::save_with_backup_with(path, backup_opts, fill) {
        Ok(backup) => SaveResult::Saved { backup },
        Err(e @ (SaveError::ReadOnly(_) | SaveError::Backup { .. } | SaveError::Write { .. })) => SaveResult::Failed(e.to_string()),
    }
}

/// The file a lazy buffer must be re-based on after a save: `saved` itself for UTF-8, a fresh UTF-8 sidecar
/// when the document was written in another encoding by [`save_document_lazy_encoded`].
pub fn lazy_rebase_path(buf: &Buffer, meta: &DocMeta, saved: &Path) -> io::Result<std::path::PathBuf> {
    if meta.codepage == "utf-8" {
        Ok(saved.to_path_buf())
    } else {
        brouillon_editor_engine::stream_codec::write_rebase_sidecar(buf)
    }
}

/// [`save_document`] for a document that lives in a rope: the chunks are encoded and streamed into the
/// atomic temp file (same backup, same lossy question, same bytes), so a 100 MB save needs no String and
/// no encoded copy of the document.
pub fn save_document_rope(path: &Path, rope: &crop::Rope, meta: &DocMeta, accept_lossy: bool, backup_opts: &BackupOptions) -> SaveResult {
    use std::io::Write;
    let spec = SaveSpec { codepage: meta.codepage, bom: meta.bom, eol: None };
    let unmappable = match file_io::count_unmappable(rope.chunks(), &spec) {
        Ok(n) => n,
        Err(e) => return SaveResult::Failed(e.to_string()),
    };
    if unmappable > 0 && !accept_lossy {
        return SaveResult::Lossy { unmappable, codepage: meta.codepage };
    }
    let fill = |f: &mut std::fs::File| -> io::Result<()> {
        let mut w = io::BufWriter::with_capacity(256 << 10, f);
        file_io::encode_stream(rope.chunks(), &spec, &mut |b| w.write_all(b)).map_err(|e| match e {
            file_io::FileIoError::Io(e) => e,
            other => io::Error::other(other.to_string()),
        })?;
        w.flush()
    };
    match backup::save_with_backup_with(path, backup_opts, fill) {
        Ok(backup) => SaveResult::Saved { backup },
        Err(e @ (SaveError::ReadOnly(_) | SaveError::Backup { .. } | SaveError::Write { .. })) => SaveResult::Failed(e.to_string()),
    }
}

// ---------------------------------------------------------------------------
// Status bar
// ---------------------------------------------------------------------------

/// Everything `status_model` needs for the active tab.
pub fn doc_status(buf: &Buffer, cursor: (usize, usize), meta: &DocMeta, overwrite: bool) -> DocStatus {
    let (line, col) = cursor;
    let char_at_caret = buf
        .nth_line(line.saturating_sub(1))
        .and_then(|l| l.chars().nth(col.saturating_sub(1)));
    let name = brouillon_core::codepages::find(meta.codepage).map_or_else(|_| meta.codepage.to_string(), |c| c.label.to_string());
    DocStatus {
        line,
        col,
        total_lines: buf.line_count(),
        char_at_caret,
        eol: Some(meta.eol_kind),
        eol_mixed: meta.eol_mixed,
        encoding: name,
        bom: meta.bom,
        overwrite,
        readonly: meta.readonly,
        large_file: meta.large_file,
        decimal_char_code: false,
        remote: None,
    }
}

pub fn status_panes(buf: &Buffer, cursor: (usize, usize), meta: &DocMeta, overwrite: bool) -> Vec<Pane> {
    status_panes_remote(buf, cursor, meta, overwrite, None)
}

/// Same, plus the remote chip when the active document lives on a server.
pub fn status_panes_remote(buf: &Buffer, cursor: (usize, usize), meta: &DocMeta, overwrite: bool, remote: Option<brouillon_editor_engine::remote::RemoteDocState>) -> Vec<Pane> {
    let mut d = doc_status(buf, cursor, meta, overwrite);
    d.remote = remote;
    brouillon_ui::status_bar::status_model(&d)
}

/// Separator for a newly typed Enter in this document.
pub fn eol_separator(kind: EolKind) -> &'static str {
    kind.separator()
}

// ---------------------------------------------------------------------------
// Startup
// ---------------------------------------------------------------------------

/// `(path, line, col)` targets to open at launch, from the command line.
pub fn startup_targets(cli: &crate::cli::Cli) -> Vec<(PathBuf, Option<usize>, Option<usize>)> {
    use crate::ipc::Request;
    cli.requests()
        .into_iter()
        .filter_map(|r| match r {
            Request::Open { path, line, col, .. } => Some((path, line, col)),
            _ => None,
        })
        .collect()
}

/// Apply one forwarded or startup request to the tab manager. Returns the
/// 1-based `(line, col)` to place the caret at, if any.
pub fn apply_request(tm: &mut TabManager, req: &crate::ipc::Request) -> Option<(usize, usize)> {
    use crate::ipc::Request;
    match req {
        Request::Open { path, line, col, readonly, .. } => {
            let idx = tm.iter().position(|t| t.path.as_deref() == Some(path.as_path()));
            match idx {
                Some(i) => tm.switch_to(i),
                None => {
                    if let Err(e) = tm.open_file(path) {
                        eprintln!("[open] {}: {e}", path.display());
                        return None;
                    }
                }
            }
            tm.current_mut().meta.readonly |= *readonly;
            line.map(|l| (l.max(1), col.unwrap_or(1).max(1)))
        }
        Request::Cursor { line, col } => Some(((*line).max(1), (*col).max(1))),
        Request::New => {
            tm.new_scratch();
            None
        }
        Request::Select { .. } | Request::Activate => None,
    }
}

/// One indentation unit for generated code: a tab when the document already indents with tabs,
/// four spaces otherwise.
pub fn indent_unit(text: &str) -> &'static str {
    if text.lines().take(400).any(|l| l.starts_with('\t')) {
        "\t"
    } else {
        "    "
    }
}

/// Emmet: expand the abbreviation that ends at `caret_byte` (HTML/CSS by file extension).
/// Returns the edit and the byte offset of the caret after it, or `None` when the document
/// is not HTML/CSS, the caret is not after an abbreviation, or it does not parse (the caller
/// then lets Tab behave normally).
pub fn emmet_edit(text: &str, caret_byte: usize, ext: &str) -> Option<(brouillon_core::contract::TextEdit, usize)> {
    let lang = brouillon_editor_engine::emmet::Lang::from_ext(ext)?;
    let caret = caret_byte.min(text.len());
    if !text.is_char_boundary(caret) {
        return None;
    }
    let line_start = text[..caret].rfind('\n').map_or(0, |i| i + 1);
    let prefix = &text[line_start..caret];
    let base: String = prefix.chars().take_while(|c| *c == ' ' || *c == '\t').collect();
    let (start, exp) = brouillon_editor_engine::emmet::expand_before_caret(prefix, lang, indent_unit(text))?;
    let new_text = exp.text.replace('\n', &format!("\n{base}"));
    let newlines_before_cursor = exp.text[..exp.cursor].matches('\n').count();
    let cursor = exp.cursor + newlines_before_cursor * base.len();
    Some((brouillon_core::contract::TextEdit::new(line_start + start, caret, new_text), line_start + start + cursor))
}

#[cfg(test)]
mod tests {

    #[test]
    fn buffer_conversions_match_the_text_ones() {
        let text = "alpha\nbéta gamma\r\n\nδ\nlast line, no eol";
        let buf = Buffer::from_str(text);
        for l in 1..8 {
            for c in 1..25 {
                assert_eq!(byte_of_line_col(&buf, (l, c)), regions_from(text, (l, c), None)[0].head, "({l},{c})");
            }
        }
        for b in 0..=text.len() {
            if text.is_char_boundary(b) && !(b > 0 && text.as_bytes()[b - 1] == b'\r' && text.as_bytes().get(b) == Some(&b'\n')) {
                assert_eq!(line_col_of_buf(&buf, b), line_col_of(text, b), "byte {b}");
            }
        }
        let t2 = "x\ny\n";
        let b2 = Buffer::from_str(t2);
        assert_eq!(line_col_of_buf(&b2, t2.len()), line_col_of(t2, t2.len()));
    }

    #[test]
    fn window_edits_equal_whole_text_edits() {
        let line = "some text on a line é\n";
        let text = line.repeat(60_000); // ~ 1.4 MB: over the window threshold
        let buf = Buffer::from_str(&text);
        assert!(buf.byte_len() > WINDOW_WHOLE_MAX);
        let caret = byte_of_line_col(&buf, (30_000, 5));
        let w = Window::around(&buf, caret);
        assert!(w.text.len() < 10_000 && w.base > 0 && w.base <= caret);
        let rel = w.rel(&[Region::caret(caret)]);
        let (edits, after) = brouillon_editor_engine::cursor::delete_chars(&w.text, &rel, true);
        let (edits, after) = (w.abs_edits(edits), w.abs_regions(after));
        let (e2, a2) = brouillon_editor_engine::cursor::delete_chars(&text, &[Region::caret(caret)], true);
        assert_eq!(edits, e2);
        assert_eq!(after, a2);
    }
    use super::*;

    #[test]
    fn emmet_edit_replaces_the_abbreviation_and_keeps_the_line_indent() {
        let text = "<body>\n  ul>li*2";
        let (edit, caret) = emmet_edit(text, text.len(), "html").unwrap();
        assert_eq!((edit.start_byte, edit.end_byte), (9, text.len()));
        assert_eq!(edit.text, "<ul>\n      <li></li>\n      <li></li>\n  </ul>");
        // caret inside the first <li>
        let after = format!("{}{}", &text[..edit.start_byte], edit.text);
        assert_eq!(&after[..caret], "<body>\n  <ul>\n      <li>");
    }

    #[test]
    fn emmet_edit_uses_tabs_when_the_document_does_and_ignores_other_languages() {
        let text = "<div>\n\t<p></p>\n</div>\ndiv>p";
        let (edit, _) = emmet_edit(text, text.len(), "htm").unwrap();
        assert_eq!(edit.text, "<div>\n\t<p></p>\n</div>");
        assert!(emmet_edit("div>p", 5, "rs").is_none());
        assert!(emmet_edit("hello world", 11, "css").is_none());
        assert_eq!(emmet_edit("a { m10", 7, "css").unwrap().0.text, "margin: 10px;");
        assert!(emmet_edit("é", 1, "html").is_none(), "not a char boundary");
    }
    use brouillon_core::contract::{EditSet, TextEdit};

    #[test]
    fn registry_builds_without_rejections() {
        let (reg, errs) = build_registry();
        // The only expected duplicate would be a command two lots both declare.
        assert!(errs.is_empty(), "registry rejected: {errs:?}");
        assert!(reg.len() > 150, "{}", reg.len());
        for id in ["edit_undo", "file_save", "search_find_next", "case_upper", "sort_asc", "tools_run_slot_1", "find_in_files", "go_to"] {
            assert!(reg.contains(id), "{id}");
        }
    }

    #[test]
    fn every_dotted_alias_maps_to_a_registered_id() {
        let (reg, _) = build_registry();
        for (old, new) in ALIASES {
            assert!(reg.contains(new), "{old} -> {new} is not registered");
            assert_eq!(canonical(old), *new);
        }
        assert_eq!(canonical("case_upper"), "case_upper");
        assert_eq!(canonical("unknown.thing"), "unknown.thing");
    }

    /// Classic ids that no lot implements yet. Adding to this list is a conscious act.
    /// Empty since `file_print` hands the text to the system print handler (print_doc.rs).
    const CLASSIC_UNIMPLEMENTED: &[&str] = &[];

    #[test]
    fn every_classic_binding_is_dispatchable() {
        let (reg, _) = build_registry();
        assert!(CLASSIC_UNIMPLEMENTED.is_empty());
        let all = classic_bindings(&reg);
        assert!(all.iter().any(|b| b.command == "file_print"), "Ctrl+P is bound in the Classic overlay");
        for b in all {
            assert!(dispatchable(&b.command), "{}", b.command);
        }
    }

    #[test]
    fn classic_ids_all_exist_in_registry() {
        let (reg, _) = build_registry();
        // Remap alone (without retain_known) must yield only registered ids.
        for b in keymap::classic_bindings() {
            let id = CLASSIC_REMAP.iter().find(|(o, _)| *o == b.command).map_or(b.command.as_str(), |(_, n)| *n);
            assert!(reg.contains(id), "Classic id `{}` (-> `{id}`) is not registered by any lot", b.command);
        }
        // None is silently dropped by retain_known.
        assert_eq!(classic_bindings(&reg).len(), keymap::classic_bindings().len());
    }

    #[test]
    fn classic_unimplemented_list_is_accurate() {
        let (reg, _) = build_registry();
        for id in CLASSIC_UNIMPLEMENTED {
            assert!(reg.contains(id) && !dispatchable(id), "{id} is listed as unimplemented but is dispatchable or unknown");
        }
        // And everything Classic binds is either dispatchable or on that list.
        for b in classic_bindings(&reg) {
            assert!(dispatchable(&b.command) || CLASSIC_UNIMPLEMENTED.contains(&b.command.as_str()), "{}", b.command);
        }
    }

    /// Every menu row and toolbar button must reach a command that the shell or
    /// a lot executes: the dispatcher ignores unknown ids without a sound, so an
    /// id drift would leave a dead entry.
    #[test]
    fn every_menu_and_toolbar_id_is_a_registered_executable_command() {
        let (reg, _) = build_registry();
        let mut ids: Vec<(String, &str)> = Vec::new();
        for (menu, _) in crate::menu_bar::MENU_ITEMS {
            let rows = crate::menu_dropdown::get_dropdown_items(menu);
            assert!(!rows.is_empty(), "menu `{menu}` has no rows");
            ids.extend(rows.iter().map(|r| (r.action_id.to_string(), *menu)));
        }
        ids.extend(crate::toolbar::TOOLBAR_ITEMS.iter().map(|(id, _, _)| (id.to_string(), "toolbar")));
        for (raw, origin) in ids {
            let id = canonical(&raw);
            assert!(reg.contains(&id), "{origin}: `{raw}` -> `{id}` is not a registered command");
            assert!(dispatchable(&id), "{origin}: `{raw}` -> `{id}` is registered but nothing executes it");
        }
    }


    /// The dropdowns keep a hand-curated layout (captions and order), but every row must be a registered command that
    /// is listed once per menu, and a command whose registry category is the menu's must also be in the registry-derived
    /// `menu_entries` for that menu: nothing the registry knows for a menu is lost or doubled.
    #[test]
    fn menu_rows_are_unique_registered_and_consistent_with_the_registry_derived_entries() {
        let (reg, _) = build_registry();
        let binds = dispatch_bindings(&reg, false, &[]);
        for (menu, _) in crate::menu_bar::MENU_ITEMS {
            let rows = crate::menu_dropdown::get_dropdown_items(menu);
            let derived = brouillon_ui::menu_dropdown::menu_entries(&reg, &binds, menu, brouillon_core::preset::Preset::Moldu, &|_| false);
            let mut seen = std::collections::HashSet::new();
            for row in &rows {
                let id = canonical(row.action_id);
                assert!(reg.contains(&id), "{menu}: `{}` is not registered", row.action_id);
                assert!(seen.insert(id.clone()), "{menu}: `{id}` appears twice");
                let spec = reg.all().iter().find(|s| s.id == id).unwrap();
                if spec.category.eq_ignore_ascii_case(menu) {
                    assert_eq!(derived.iter().filter(|e| e.command == id).count(), 1, "{menu}: `{id}` is missing from (or doubled in) the registry-derived entries");
                }
            }
        }
    }
    #[test]
    fn menu_shortcut_hints_match_effective_bindings() {
        let (reg, _) = build_registry();
        let binds = dispatch_bindings(&reg, false, &[]);
        for (menu, _) in crate::menu_bar::MENU_ITEMS {
            for row in crate::menu_dropdown::get_dropdown_items(menu) {
                if row.shortcut.is_empty() {
                    continue;
                }
                let want = row.shortcut.to_ascii_lowercase().replace('-', "+");
                let id = canonical(row.action_id);
                // Ctrl+G is caught by the shell's own key handler (the lot's default chord is dropped in `annotate`).
                if id == "go_to" && want == "ctrl+g" {
                    continue;
                }
                let have: Vec<String> = keymap::chords_for(&binds, &id).iter().map(|c| c.to_ascii_lowercase()).collect();
                assert!(have.contains(&want), "`{}` shows {} but is bound to {have:?}", row.label, row.shortcut);
            }
        }
    }

    #[test]
    fn dispatch_table_has_no_same_layer_conflicts_and_f4_is_split_by_panel() {
        let (reg, _) = build_registry();
        let defaults = dispatch_bindings(&reg, false, &[]);
        let conflicts = keymap::find_conflicts(&defaults);
        // f4 clashes are resolved by `when`; anything else is a real conflict.
        let real: Vec<_> = conflicts.iter().filter(|c| c.chord != "f4" && c.chord != "shift+f4").collect();
        assert!(real.is_empty(), "{real:?}");
        let mut d = Dispatcher::new(defaults);
        d.ctx.set_str("active_panel", "results");
        assert_eq!(d.press(combo(false, false, false, "f4"), 0), Resolution::Command("results_next_hit".into()));
        d.ctx.set_str("active_panel", "output");
        assert_eq!(d.press(combo(false, false, false, "f4"), 10), Resolution::Command("tools_next_error".into()));
    }

    #[test]
    fn shortcut_editor_sees_every_command_with_its_default_and_classic_chords() {
        let (reg, _) = build_registry();
        let list = command_list(&reg);
        assert_eq!(list.len(), reg.len());
        assert!(list.windows(2).all(|w| w[0].1.to_lowercase() <= w[1].1.to_lowercase()), "sorted by caption");
        let chords = |classic: bool, id: &str| keymap::chords_for(&base_bindings(&reg, classic), id);
        assert_eq!(chords(false, "file_save"), vec!["ctrl+s"]);
        assert_eq!(chords(false, "preferences_open_settings"), vec!["ctrl+,"]);
        // Commands the shell cannot run yet still show their chords (dispatch_bindings drops them).
        assert!(!chords(true, "select_line").is_empty() && chords(false, "select_line").is_empty());
        // A user binding made in the editor reaches the live dispatcher.
        let user = vec![Binding::new("ctrl+j", "duplicate_line", None).unwrap()];
        let mut d = Dispatcher::new(dispatch_bindings(&reg, false, &user));
        assert_eq!(d.press(combo(true, false, false, "j"), 0), Resolution::Command("duplicate_line".into()));
    }

    #[test]
    fn ctrl_tab_switches_tabs_without_the_classic_overlay() {
        let (reg, _) = build_registry();
        let mut d = Dispatcher::new(dispatch_bindings(&reg, false, &[]));
        assert_eq!(d.press(combo(true, false, false, "tab"), 0), Resolution::Command("tab_next".into()));
        assert_eq!(d.press(combo(true, true, false, "tab"), 10), Resolution::Command("tab_previous".into()));
    }

    #[test]
    fn ctrl_digits_run_tool_slots_and_user_layer_wins() {
        let (reg, _) = build_registry();
        let user = vec![Binding::new("ctrl+1", "", None).unwrap()];
        let mut d = Dispatcher::new(dispatch_bindings(&reg, false, &[]));
        assert_eq!(d.press(combo(true, false, false, "2"), 0), Resolution::Command("tools_run_slot_2".into()));
        let mut d = Dispatcher::new(dispatch_bindings(&reg, false, &user));
        assert_eq!(d.press(combo(true, false, false, "1"), 0), Resolution::NoMatch);
    }

    #[test]
    fn routing_reaches_the_owning_lot() {
        let sel = [Region { anchor: 0, head: 5 }];
        let ctx = CmdCtx { text: "hello world", selection: &sel, path: None, args: &[] };
        let up = run("case_upper", &ctx).expect("textops");
        assert_eq!(up.edits.apply_to_str("hello world").unwrap(), "HELLO world");
        assert!(run("search_open_find", &ctx).unwrap().open_panel.is_some());
        assert!(run("find_in_files", &ctx).unwrap().open_panel.is_some());
        assert!(run("eol_to_crlf", &ctx).is_some());
        assert!(run("tools_run_slot_1", &ctx).is_some());
        assert!(run("file_save", &ctx).is_none(), "shell command, not a lot command");
        assert!(run("no_such_command", &ctx).is_none());
        // legacy ids are accepted too
        assert!(run("edit.find", &ctx).is_some());
    }

    #[test]
    fn mcp_metadata_is_filled() {
        let (reg, _) = build_registry();
        let get = |id: &str| reg.get(id).unwrap();
        assert!(get("case_upper").mutates);
        assert!(get("sort_asc").mutates);
        assert!(!get("count_text").mutates);
        assert!(!get("search_open_find").mutates);
        assert!(get("tools_run_slot_1").mutates);
        assert!(get("search_replace_all").args_schema.as_deref().unwrap().contains("replacement"));
        for s in reg.all() {
            if let Some(schema) = &s.args_schema {
                serde_json::from_str::<serde_json::Value>(schema).unwrap_or_else(|e| panic!("{}: {e}", s.id));
            }
        }
    }

    #[test]
    fn apply_outcome_is_one_undo_group_and_atomic() {
        let buf = Buffer::from_str("a b c d");
        let mut set: Vec<TextEdit> = Vec::new();
        for (i, _) in "a b c d".match_indices(' ') {
            set.push(TextEdit::new(i, i + 1, "_"));
        }
        let out = CmdOutcome { edits: EditSet(set), ..Default::default() };
        let mut nb = apply_outcome(&buf, &out, &[Region::caret(0)]).unwrap();
        assert_eq!(nb.rope().to_string(), "a_b_c_d");
        assert!(nb.undo(), "one undo");
        assert_eq!(nb.rope().to_string(), "a b c d");
        assert!(!nb.can_undo(), "the three edits were a single group");
        // out of range edit: Err and the source buffer is untouched
        let bad = CmdOutcome { edits: EditSet(vec![TextEdit::new(0, 99, "x")]), ..Default::default() };
        assert!(apply_outcome(&buf, &bad, &[]).is_err());
        assert_eq!(buf.rope().to_string(), "a b c d");
    }

    #[test]
    fn typing_coalesces_and_enter_closes_groups() {
        let mut b = Buffer::from_str("");
        for (i, c) in "hello".chars().enumerate() {
            b.typing_insert(i, &c.to_string(), 100 + i as u64);
        }
        assert!(b.undo());
        assert_eq!(b.rope().to_string(), "", "five letters are one undo step");
        assert!(b.redo());
        assert_eq!(b.rope().to_string(), "hello");
    }

    #[test]
    fn rope_save_writes_the_same_bytes_as_the_string_save() {
        let dir = tempfile::tempdir().unwrap();
        let opts = BackupOptions { enabled: false, ..Default::default() };
        let text = "caf\u{e9} \u{20ac} \u{4e2d}\r\nl2\n".repeat(2000);
        let rope = crop::Rope::from(text.as_str());
        for (cp, bom) in [("utf-8", false), ("utf-8", true), ("utf-16le", true), ("windows-1252", false), ("iso-2022-jp", false)] {
            let mut meta = DocMeta::default();
            meta.codepage = cp;
            meta.bom = bom;
            let (a, b) = (dir.path().join("a.txt"), dir.path().join("b.txt"));
            for accept in [false, true] {
                let ra = save_document(&a, &text, &meta, accept, &opts);
                let rb = save_document_rope(&b, &rope, &meta, accept, &opts);
                match (ra, rb) {
                    (SaveResult::Saved { .. }, SaveResult::Saved { .. }) => assert_eq!(std::fs::read(&a).unwrap(), std::fs::read(&b).unwrap(), "{cp} bom={bom}"),
                    (SaveResult::Lossy { unmappable: x, codepage: c1 }, SaveResult::Lossy { unmappable: y, codepage: c2 }) => assert_eq!((x, c1), (y, c2), "{cp}"),
                    (x, y) => panic!("{cp}: {x:?} vs {y:?}"),
                }
            }
        }
    }

    #[test]
    fn rope_save_keeps_the_backup_and_leaves_no_temp_file() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("doc.txt");
        std::fs::write(&p, "old\n").unwrap();
        let opts = BackupOptions { enabled: true, dated: false, ..Default::default() };
        let meta = DocMeta::default();
        let r = save_document_rope(&p, &crop::Rope::from("new text\n"), &meta, false, &opts);
        let SaveResult::Saved { backup: Some(bak) } = r else { panic!("{r:?}") };
        assert_eq!(std::fs::read_to_string(&p).unwrap(), "new text\n");
        assert_eq!(std::fs::read_to_string(bak).unwrap(), "old\n");
        let names: Vec<_> = std::fs::read_dir(dir.path()).unwrap().map(|e| e.unwrap().file_name().to_string_lossy().into_owned()).collect();
        assert_eq!(names.len(), 2, "doc + backup only: {names:?}");
    }

    #[test]
    fn save_roundtrip_keeps_bom_codepage_and_reports_lossy() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("a.txt");
        let bytes = [&[0xEF, 0xBB, 0xBF][..], "h\u{e9}llo\r\nw\n".as_bytes()].concat();
        std::fs::write(&p, &bytes).unwrap();
        let loaded = open_file(&p).unwrap();
        let meta = DocMeta::from_loaded(&loaded);
        assert!(meta.bom && meta.codepage == "utf-8" && meta.eol_mixed);
        let opts = BackupOptions { enabled: true, ..Default::default() };
        match save_document(&p, &loaded.text, &meta, false, &opts) {
            SaveResult::Saved { backup } => assert!(backup.is_some()),
            other => panic!("{other:?}"),
        }
        assert_eq!(std::fs::read(&p).unwrap(), bytes, "byte-identical after open+save");
        // converting to a narrow codepage with unmappable characters warns first
        let mut narrow = meta.clone();
        narrow.codepage = "windows-1252";
        match save_document(&p, "\u{20ac}\u{4e2d}", &narrow, false, &opts) {
            SaveResult::Lossy { unmappable, .. } => assert_eq!(unmappable, 1),
            other => panic!("{other:?}"),
        }
        assert_eq!(std::fs::read(&p).unwrap(), bytes, "nothing written on a lossy refusal");
        assert!(matches!(save_document(&p, "\u{20ac}\u{4e2d}", &narrow, true, &opts), SaveResult::Saved { .. }));
    }

    #[test]
    fn status_model_reflects_document() {
        let loaded = file_io::decode_bytes(b"ab\r\ncd\r\n", "windows-1252");
        let meta = DocMeta::from_loaded(&loaded);
        let buf = Buffer::from_str(&loaded.text);
        let panes = status_panes(&buf, (2, 1), &meta, false);
        let get = |id: &str| panes.iter().find(|p| p.id == id).map(|p| p.label.clone());
        assert_eq!(get("pos").unwrap(), "Ln 2, Col 1");
        assert_eq!(get("eol").unwrap(), "CRLF");
        assert_eq!(get("char").unwrap(), "U+0063");
        assert_eq!(get("mode").unwrap(), "INS");
        assert!(panes.iter().find(|p| p.id == "encoding").unwrap().command.is_some());
    }

    #[test]
    fn requests_open_files_and_place_the_caret() {
        use crate::ipc::Request;
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("x.txt");
        std::fs::write(&p, "one\ntwo\n").unwrap();
        let mut tm = TabManager::new();
        let open = Request::Open { path: p.clone(), line: Some(2), col: Some(3), readonly: true, new_window: false, wait: false };
        assert_eq!(apply_request(&mut tm, &open), Some((2, 3)));
        assert!(tm.current().meta.readonly);
        let n = tm.count();
        assert_eq!(apply_request(&mut tm, &open), Some((2, 3)));
        assert_eq!(tm.count(), n, "an already open file is activated, not duplicated");
        apply_request(&mut tm, &Request::New);
        assert_eq!(tm.count(), n + 1);
    }

    #[test]
    fn every_registered_command_is_routed_or_listed_as_pending() {
        let (reg, _) = build_registry();
        let sample = "fn main() {\n    let a = 1;\n}\n";
        let sel = [Region { anchor: 0, head: 9 }];
        let mut pending = Vec::new();
        for s in reg.all() {
            let args = vec!["x".to_string(), "c".to_string()];
            let ctx = CmdCtx { text: sample, selection: &sel, path: None, args: &args };
            // Must never panic whatever the command, even with junk arguments.
            let routed = run(&s.id, &ctx).is_some();
            if !routed && !shell_implements(&s.id) {
                pending.push(s.id.as_str());
            }
        }
        pending.sort_unstable();
        // Shell-only commands without an implementation (SHELL, implemented == false) and
        // stateful lot commands the shell does not drive yet (nav history, multi-region painting).
        let mut expected: Vec<&str> = SHELL.iter().filter(|(_, _, _, _, ok)| !ok).map(|(id, ..)| *id).collect();
        expected.extend(["bookmark_next_unused"].iter().copied().filter(|id| reg.contains(id)));
        expected.sort_unstable();
        assert_eq!(pending, expected);
    }

    /// The hex commands in SHELL are exactly the ones the hex view implements, and an agent can run none of them.
    #[test]
    fn hex_commands_match_the_hex_view_and_are_not_agent_runnable() {
        let shell: Vec<(&str, &str)> = SHELL.iter().filter(|(id, ..)| id.starts_with("hex_")).map(|(id, cap, ..)| (*id, *cap)).collect();
        assert_eq!(shell, brouillon_ui::hex_editor::COMMANDS.to_vec());
        for (id, _) in &shell {
            assert!(shell_implements(id));
            assert!(!crate::link_host::is_safe_command(id), "{id} must not be runnable by an agent");
        }
    }

    #[test]
    fn caret_and_selection_convert_both_ways() {
        let text = "h\u{e9}llo\nworld\n";
        // "h\u{e9}llo\n" is 7 bytes, so (2, 3) is byte 9.
        assert_eq!(regions_from(text, (2, 3), None), vec![Region::caret(9)]);
        assert_eq!(line_col_of(text, 9), (2, 3));
        assert_eq!(regions_from(text, (2, 3), Some(((1, 1), (2, 1)))), vec![Region { anchor: 0, head: 7 }]);
        assert_eq!(line_col_of(text, 7), (2, 1));
    }

    #[test]
    fn multi_caret_regions_round_trip_and_split_keeps_primary_last() {
        let text = "ab
cd
ef
";
        let rs = regions_with_extras(text, (3, 2), None, &[(1, 2), (2, 1)]);
        assert_eq!(rs, vec![Region::caret(1), Region::caret(3), Region::caret(7)]);
        let (a, h, extras) = split_regions(text, &rs);
        assert_eq!((a, h), ((3, 2), (3, 2)));
        assert_eq!(extras, vec![(1, 2), (2, 1)]);
        // a caret sitting on the primary is dropped (no duplicate caret)
        let (_, _, ex) = split_regions(text, &[Region::caret(7), Region::caret(7)]);
        assert!(ex.is_empty());
    }

    #[test]
    fn typing_at_every_caret_is_one_undo_step() {
        let text = "a
b
c
";
        let buf = Buffer::from_str(text);
        let rs = regions_with_extras(text, (3, 1), None, &[(1, 1), (2, 1)]);
        let (edits, after) = brouillon_editor_engine::cursor::insert_text(&rs, "X");
        let out = CmdOutcome { edits, selection: Some(after.clone()), ..Default::default() };
        let mut nb = apply_outcome(&buf, &out, &rs).unwrap();
        assert_eq!(nb.rope().to_string(), "Xa
Xb
Xc
");
        let new_text = nb.rope().to_string();
        let (_, head, extras) = split_regions(&new_text, &after);
        assert_eq!(head, (3, 2));
        assert_eq!(extras, vec![(1, 2), (2, 2)]);
        // backspace at all three carets, then ONE undo restores the typed state, a second the original
        let (e2, a2) = brouillon_editor_engine::cursor::delete_chars(&new_text, &after, false);
        let out2 = CmdOutcome { edits: e2, selection: Some(a2), ..Default::default() };
        nb = apply_outcome(&nb, &out2, &after).unwrap();
        assert_eq!(nb.rope().to_string(), text);
        nb.undo_restoring();
        assert_eq!(nb.rope().to_string(), "Xa
Xb
Xc
");
        nb.undo_restoring();
        assert_eq!(nb.rope().to_string(), text);
    }

    #[test]
    fn cli_targets_parse_line_and_column() {
        use clap::Parser;
        let cli = crate::cli::Cli::parse_from(["brouillon", "a.rs:12:3", "b.txt"]);
        let t = startup_targets(&cli);
        assert_eq!(t, vec![(PathBuf::from("a.rs"), Some(12), Some(3)), (PathBuf::from("b.txt"), None, None)]);
    }

    #[test]
    fn rope_head_cuts_on_a_character_boundary() {
        let rope: crop::Rope = "caf\u{e9} \u{20ac}\nsecond".parse().unwrap();
        assert_eq!(rope_head(&rope, 3), "caf");
        assert_eq!(rope_head(&rope, 4), "caf", "never inside the two bytes of the accent");
        assert_eq!(rope_head(&rope, 5), "caf\u{e9}");
        assert_eq!(rope_head(&rope, 1000), rope.to_string());
        assert_eq!(rope_head(&crop::Rope::new(), 8), "");
    }

    #[test]
    fn completion_window_is_the_whole_document_until_the_budget_and_follows_the_caret_after() {
        let text: String = (0..1000).map(|i| format!("line number {i:04}\n")).collect();
        let rope: crop::Rope = text.parse().unwrap();
        assert_eq!(completion_window(&rope, 500, text.len()), (0, rope.line_len()), "under the budget: everything");
        let (a, b) = completion_window(&rope, 500, 2000);
        assert!(a <= 500 && 500 < b, "the caret line is inside ({a}, {b})");
        assert!(rope.byte_slice(rope.byte_of_line(a)..rope.byte_of_line(b.min(rope.line_len() - 1))).byte_len() <= 2000 + 2 * 17);
        assert!(b - a < 200, "a window, not the document ({a}..{b})");
        // At the ends of the document the window stays inside it.
        let (a, b) = completion_window(&rope, 0, 2000);
        assert_eq!(a, 0);
        assert!(b > 1);
        let (a, b) = completion_window(&rope, 5000, 2000);
        assert!(a < 999 && b >= 1000 && b <= rope.line_len(), "caret clamped to the last line ({a}, {b})");
        assert_eq!(completion_window(&crop::Rope::new(), 0, 10), (0, 0));
    }
}
