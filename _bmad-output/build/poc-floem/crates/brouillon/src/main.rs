//! Day-1 PoC binary — minimal Floem editor surface to measure the four
//! hard budgets (start-up, memory, binary size, scrolling).
//!
//! API surface anchored on floem 0.2.0 (crates.io, 2024-11-15). The
//! reactive layer is re-exported from `floem_reactive 0.2.0`; the view
//! constructors live under `floem::views::*` (not the prelude — that's
//! the unreleased main branch).
//!
//! Assumption (Karpathy G1): `virtual_list` (0.2.0 spelling) virtualizes
//! vertically by line index using a `VirtualVector` data source. We feed
//! it a `Range<usize>` (which implements `VirtualVector`) and render each
//! line by indexing into a shared `Arc<Vec<String>>`. This avoids the cost
//! of `nth_line()` on the rope per draw call — day-1 accepts the memory
//! overhead (~5MB extra for 100k lines) to validate the rendering budget;
//! day-3 will switch to lazy rope slicing once we've proven Floem can hit
//! 60fps at all.
// Release builds on Windows are GUI-subsystem programs: no console window behind the app.
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

// Global allocator. Default: mimalloc (C, MIT) instead of the Windows heap; `--no-default-features`
// builds the system-heap baseline for A/B measurements (see tools/smoke/ab-memory.ps1).
#[cfg(all(feature = "alloc-mimalloc", not(feature = "dhat-heap")))]
#[global_allocator]
static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;
#[cfg(feature = "dhat-heap")]
#[global_allocator]
static GLOBAL: dhat::Alloc = dhat::Alloc;

/// Allocator-specific memory hooks: the idle trimmer's "return free pages" call and, with the
/// `purge-eager` feature, immediate decommit of freed pages (mimalloc's default is a 10 ms delay
/// that only advances while the allocator is being used, so an idle process keeps those pages).
fn configure_allocator() {
    #[cfg(all(feature = "alloc-mimalloc", not(feature = "dhat-heap")))]
    {
        brouillon::memtrim::set_allocator_collect(|| {
            // SAFETY: documented thread-safe entry point; it only releases unused pages.
            unsafe { libmimalloc_sys::mi_collect(true) }
        });
        #[cfg(feature = "purge-eager")]
        {
            // mi_option_purge_delay is index 15 in both mimalloc 2.x and 3.x (not exported by the -sys crate);
            // only set it if the default (10 ms) is there, so a wrong index can never change another option.
            const PURGE_DELAY: libmimalloc_sys::mi_option_t = 15;
            // SAFETY: plain option accessors.
            unsafe {
                if libmimalloc_sys::mi_option_get(PURGE_DELAY) == 10 {
                    libmimalloc_sys::mi_option_set(PURGE_DELAY, 0);
                }
            }
        }
    }
}

use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use floem::event::{Event, EventListener};
use floem::ext_event::create_ext_action;
use floem::keyboard::{Key, KeyCode, NamedKey, PhysicalKey};
use floem::kurbo::Size;
use floem::reactive::{create_effect, create_memo, create_rw_signal, RwSignal, Scope, SignalGet, SignalUpdate, SignalWith};
use floem::style::Style;
use floem::views::{empty, label, stack, Decorators, TooltipExt};
use floem::window::WindowConfig;
use floem::{Application, IntoView};

use brouillon::buffer::Buffer;
use brouillon::bracket_paint::{self, BracketPaint};
use brouillon::cursor::{handle_cursor_key_folded, Selection};
use brouillon::find_replace::{self, FindReplaceState};
use brouillon::highlight::Highlighter;
use brouillon::menu_bar;
use brouillon::metrics::{
    current_fps, first_frame_reported, mark_process_start, report_first_frame, tick_frame, trace_stage,
    RamSampler,
};
use brouillon::palette::PaletteState;
use brouillon::palette_view;
use brouillon::session::{self, SessionSnapshot};
use brouillon::tab_bar;
use brouillon::tabs::TabManager;
use brouillon::toolbar;
use brouillon::preset::{Preset, DEFAULT_PRESET};
use brouillon::sample;
use brouillon::tabs;
use brouillon::theme::current_theme;

/// Launch-time state computed in `main()` (see the startup order there) and
/// read by `app_view`. Statics keep `app_view`'s signature stable.
struct Startup {
    paths: brouillon::paths::Paths,
    /// Files from the command line: (path, line, col).
    targets: Vec<(PathBuf, Option<usize>, Option<usize>)>,
    no_session: bool,
    classic_keymap: bool,
    backup: brouillon::backup::BackupOptions,
    /// `files.auto_reload_unmodified`: reload a document nobody edited when another program changes it.
    auto_reload_unmodified: bool,
    user_bindings: Vec<brouillon::keymap::Binding>,
    /// Journaled documents from a crashed run, opened dirty after the session.
    recovered: Vec<brouillon::recovery::RecoveredDoc>,
    /// The journal was written by a normal shutdown (hot exit), not a crash.
    recovered_clean: bool,
    /// What to open when nothing is restored (Welcome, release notes...): see `startup_doc`.
    help_facts: brouillon::startup_doc::Facts,
    help_env: brouillon::startup_doc::HelpEnv,
    /// Brouillon Link settings (`[mcp]`, or `--mcp` for this session). Off unless asked.
    mcp: brouillon_mcp::settings::McpSettings,
    /// Where the Link token, instance file and audit log live.
    link_dir: PathBuf,
    /// `--folder DIR`: workspace to open in the folder tree.
    folder: Option<PathBuf>,
}

static STARTUP: std::sync::OnceLock<Startup> = std::sync::OnceLock::new();
/// Receiving end of the single-instance socket (polled from the UI tick).
/// Above this size (bytes) typing no longer re-highlights or scans the document for completions.
const LIVE_HIGHLIGHT_MAX_BYTES: usize = 16 * 1024 * 1024;
/// Word completion scans the whole document up to this many bytes, else a window of this size around the caret.
const COMPLETION_SCAN_BYTES: usize = 2 * 1024 * 1024;
/// A tree-sitter syntax tree costs about 30 bytes of heap per source byte (measured: 335 MB for 10 MB of
/// Rust, invisible to the Rust allocator): above this size a Rust file is shown as plain text.
const RUST_TREE_MAX_BYTES: usize = 2 * 1024 * 1024;

fn rust_tree_too_big(lang: brouillon::grammars::LanguageId, bytes: usize) -> bool {
    lang == brouillon::grammars::LanguageId::Rust && bytes > RUST_TREE_MAX_BYTES
}
/// Fold regions are re-detected per edit only up to this size (a full pass over the text per keystroke).
const FOLD_REFRESH_MAX_BYTES: usize = 2 * 1024 * 1024;
/// Problems found while loading the user syntax definitions (shown once in the status bar).
/// Largest document (bytes) the comparison view diffs on the UI thread.
const DIFF_MAX_BYTES: usize = 8 * 1024 * 1024;
static SYNTAX_ERRORS: std::sync::OnceLock<Vec<String>> = std::sync::OnceLock::new();
static IPC_SERVER: Mutex<Option<brouillon::ipc::Server>> = Mutex::new(None);

fn startup() -> &'static Startup {
    STARTUP.get().expect("startup() is initialised in main()")
}

const SAMPLE_LINES: usize = 100_000;
const SAMPLE_SEED: u64 = 0xED17_2026;
const SAMPLE_PATH: &str = "sample/sample-100k.txt";

/// Renderer used when `render.backend` is not set: CPU first, GPU optional.
///
/// The CPU rasteriser needs no GPU driver (61 MB for an empty document on
/// Windows against 152 MB with Direct3D 12 and 116 MB for wgpu alone), works
/// in virtual machines and without any adapter, and keeps the 80 MB budget
/// reachable. A GPU backend stays one setting away.
const DEFAULT_RENDER_BACKEND: &str = "cpu";

/// Pick the renderer from the `render.backend` setting (`cpu` by default),
/// unless the user already set `WGPU_BACKEND` / `FLOEM_FORCE_TINY_SKIA`.
///
/// * `cpu`   - CPU rasteriser (tiny-skia): no GPU driver at all, 61 MB for an
///   empty document (Windows), at the cost of CPU time per frame.
/// * `gpu`   - Windows: Direct3D 12 only; elsewhere: wgpu's own choice.
///   floem asks wgpu for `Backends::all()`, so wgpu also initialises the
///   Vulkan loader and the vendor Vulkan driver, about 130 MB resident
///   (Intel iGPU, empty document: 295 MB vs 152 MB) without drawing with it.
///   `auto` is kept as an alias of `gpu` for settings files written before
///   the default became `cpu`.
/// * `dx12` / `vulkan` / `gl` / `metal` - force that wgpu backend.
///
/// Must run before the window (and so the renderer) is created.
fn apply_render_backend(choice: &str) {
    let choice = choice.trim().to_ascii_lowercase();
    match choice.as_str() {
        "cpu" | "software" => {
            if std::env::var_os("FLOEM_FORCE_TINY_SKIA").is_none() {
                std::env::set_var("FLOEM_FORCE_TINY_SKIA", "1");
            }
            // floem still creates a wgpu instance (surface probing) in CPU mode;
            // with every backend enabled that costs ~20 MB (81 MB vs 61 MB).
            #[cfg(windows)]
            if std::env::var_os("WGPU_BACKEND").is_none() {
                std::env::set_var("WGPU_BACKEND", "dx12");
            }
        }
        "dx12" | "vulkan" | "gl" | "metal" => {
            if std::env::var_os("WGPU_BACKEND").is_none() {
                std::env::set_var("WGPU_BACKEND", &choice);
            }
        }
        "gpu" | "auto" => {
            #[cfg(windows)]
            if std::env::var_os("WGPU_BACKEND").is_none() {
                std::env::set_var("WGPU_BACKEND", "dx12");
            }
        }
        "" => apply_render_backend(DEFAULT_RENDER_BACKEND),
        other => {
            eprintln!("[settings] render.backend: unknown value {other:?} (use cpu, gpu, dx12, vulkan, gl, metal); using {DEFAULT_RENDER_BACKEND}");
            apply_render_backend(DEFAULT_RENDER_BACKEND);
        }
    }
}

/// A GUI-subsystem exe has no console. When it is started from a terminal, attach to that
/// terminal so `--help`, `--version` and the diagnostics still print there. When started from
/// Explorer or a shortcut there is no parent console, `AttachConsole` fails and nothing opens.
#[cfg(all(windows, not(debug_assertions)))]
fn attach_parent_console() {
    use std::os::windows::io::IntoRawHandle;
    extern "system" {
        fn AttachConsole(pid: u32) -> i32;
        fn GetStdHandle(n: u32) -> isize;
        fn SetStdHandle(n: u32, h: isize) -> i32;
    }
    const ATTACH_PARENT_PROCESS: u32 = u32::MAX;
    const STD_OUTPUT_HANDLE: u32 = -11i32 as u32;
    const STD_ERROR_HANDLE: u32 = -12i32 as u32;
    unsafe {
        if AttachConsole(ATTACH_PARENT_PROCESS) == 0 {
            return;
        }
        for n in [STD_OUTPUT_HANDLE, STD_ERROR_HANDLE] {
            let h = GetStdHandle(n);
            // Keep handles that were redirected to a file or a pipe.
            if h == 0 || h == -1 {
                if let Ok(f) = std::fs::OpenOptions::new().write(true).open("CONOUT$") {
                    SetStdHandle(n, f.into_raw_handle() as isize);
                }
            }
        }
    }
}

fn main() {
    #[cfg(feature = "dhat-heap")]
    {
        let out = std::env::var("BROUILLON_DHAT_OUT").unwrap_or_else(|_| "dhat-heap.json".into());
        let trigger = std::env::var("BROUILLON_DHAT_TRIGGER").unwrap_or_else(|_| "dhat.trigger".into());
        let prof = dhat::Profiler::builder().trim_backtraces(Some(60)).file_name(out).build();
        std::thread::spawn(move || {
            let mut prof = Some(prof);
            loop {
                std::thread::sleep(Duration::from_millis(500));
                if prof.is_some() && std::path::Path::new(&trigger).exists() {
                    let s = dhat::HeapStats::get();
                    eprintln!("[dhat] live {} bytes in {} blocks, total {} blocks", s.curr_bytes, s.curr_blocks, s.total_blocks);
                    drop(prof.take());
                    eprintln!("[dhat] profile written");
                }
            }
        });
    }
    #[cfg(all(windows, not(debug_assertions)))]
    attach_parent_console();
    // FIRST statement — captures process-spawn timestamp. Cold-start delta
    // is reported once at first-frame commit.
    mark_process_start();
    configure_allocator();

    // Startup order (Lot 7 / BK-047..052):
    //  1. parse the command line (clap);
    //  2. resolve the data directory (--settings-dir > portable marker > ~/.brouillon);
    //  3. load or create the IPC token in that directory;
    //  4. single instance: forward our file requests to a running instance and
    //     exit, or become the primary one and keep the socket server;
    //  5. load settings (backup options, Classic keymap overlay) and the user keymap;
    //  6. look for a crash journal (recovered docs are opened dirty by app_view);
    //  7. hand the session/recovery/targets to app_view, which restores the
    //     session, opens the targets and starts the autosave timer.
    let cli = <brouillon::cli::Cli as clap::Parser>::parse();
    trace_stage("cli parsed");
    let paths = brouillon::paths::Paths::system(cli.settings_dir.as_deref())
        .unwrap_or_else(|| brouillon::paths::Paths::resolve(std::path::Path::new("."), None, PathBuf::from(".brouillon")));
    // `--import-editplus`: headless one-shot import, report on stdout, no window and no instance handling.
    if let Some(src) = cli.import_editplus.as_deref() {
        let (text, code) = brouillon::import_cli::run_auto(src, paths.root(), cli.dry_run);
        print!("{text}");
        std::process::exit(code);
    }
    let _ = std::fs::create_dir_all(paths.root());
    // User language definitions (<data dir>/syntax/*.toml): load before any document is
    // highlighted; broken files are skipped, reported on stderr and in the status bar.
    let syntax_errors: Vec<String> = brouillon::syntax::reload_user_dir(&paths.syntax_dir()).iter().map(|e| e.to_string()).collect();
    for e in &syntax_errors {
        eprintln!("[syntax] {e}");
    }
    let _ = SYNTAX_ERRORS.set(syntax_errors);

    if !cli.new_window && cli.folder.is_none() {
        match brouillon::ipc::load_or_create_token(paths.root()) {
            Ok(token) => {
                let sock = brouillon::ipc::socket_name("main");
                match brouillon::ipc::acquire(&sock, &token, &cli.requests()) {
                    Ok(brouillon::ipc::Role::Forwarded) => {
                        eprintln!("[ipc] request forwarded to the running instance");
                        return;
                    }
                    Ok(brouillon::ipc::Role::Primary(server)) => {
                        *IPC_SERVER.lock().unwrap() = Some(server);
                    }
                    Err(e) => eprintln!("[ipc] single-instance disabled: {e}"),
                }
            }
            Err(e) => eprintln!("[ipc] token unavailable, single-instance disabled: {e}"),
        }
    }

    trace_stage("ipc done");
    let settings_text = std::fs::read_to_string(paths.settings_file()).unwrap_or_default();
    let settings = brouillon::settings::Settings::build(&[(brouillon::settings::Layer::User, settings_text.as_str())]);
    for e in &settings.errors {
        eprintln!("[settings] {e}");
    }
    apply_render_backend(&settings.get_str("render.backend", DEFAULT_RENDER_BACKEND));
    // [perf] idle_trim: one parked thread; after N idle seconds hand free memory back to the OS (once).
    if settings.get_bool("perf.idle_trim", true) {
        brouillon::memtrim::spawn(
            Duration::from_secs(settings.get_i64("perf.idle_trim_seconds", 30).clamp(5, 3600) as u64),
            settings.get_bool("perf.idle_trim_working_set", false),
        );
    }
    let backup = brouillon::backup::BackupOptions {
        enabled: settings.get_bool("backup.enabled", true),
        dir: Some(paths.backup_dir()),
        dated: true,
        max_count: settings.get_i64("backup.max_count", 10).max(0) as usize,
        max_age_days: settings.get_i64("backup.max_age_days", 30).max(0) as u64,
    };
    let auto_reload_unmodified = settings.get_bool("files.auto_reload_unmodified", true);
    let (user_bindings, keymap_warnings) = std::fs::read_to_string(paths.keymap_file())
        .map(|t| brouillon::keymap::parse_bindings_toml(&t))
        .unwrap_or_default();
    for w in keymap_warnings {
        eprintln!("[keymap] {w}");
    }
    let journal_at_start = brouillon::recovery::Journal::new(paths.journal_dir());
    let recovered = if cli.no_session { Vec::new() } else { journal_at_start.recover() };
    let recovered_clean = !recovered.is_empty() && journal_at_start.last_exit_clean();
    if !recovered.is_empty() {
        eprintln!(
            "[recovery] {} document(s) recovered from the autosave journal ({})",
            recovered.len(),
            if recovered_clean { "after a normal exit" } else { "after an unexpected shutdown" }
        );
    }
    brouillon::large_file::set_lazy_threshold_mb(settings.get_i64("files.lazy_threshold_mb", brouillon::large_file::DEFAULT_LAZY_THRESHOLD_MB as i64));
    let targets = brouillon::wiring::startup_targets(&cli);
    let no_session = cli.no_session;
    let session_snap = if no_session {
        None
    } else {
        match session::load_from_dir(paths.root()) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("[session] load error (ignored): {e}");
                None
            }
        }
    };
    if let Some(s) = session_snap.as_ref() {
        eprintln!("[session] found snapshot with {} tab(s)", s.tabs.len());
    }
    // Welcome page / release notes (see `startup_doc`): decided here from the files on
    // disk, applied in `app_view` once we know whether a session was restored.
    let (start_doc, start_doc_warning) = brouillon::startup_doc::StartupDocument::from_settings(&settings);
    if let Some(w) = start_doc_warning {
        eprintln!("[settings] {w}");
    }
    let this_run = brouillon::startup_doc::LastRun::current();
    let last_run = brouillon::startup_doc::LastRun::read(paths.root());
    let help_facts = brouillon::startup_doc::Facts::gather(
        last_run.as_ref(),
        &this_run,
        paths.settings_file().is_file(),
        paths.session_file().is_file(),
        start_doc,
        settings.get_bool(brouillon::startup_doc::SETTING_WHATS_NEW, true),
        !targets.is_empty() || cli.sample,
    );
    if help_facts.should_record(last_run.as_ref(), &this_run) {
        if let Err(e) = this_run.write(paths.root()) {
            eprintln!("[startup] could not write {}: {e}", brouillon::startup_doc::LAST_RUN_FILE);
        }
    }
    let help_env = brouillon::startup_doc::HelpEnv {
        data_dir: paths.root().display().to_string(),
        settings_file: paths.settings_file().display().to_string(),
        classic_keymap: settings.get_bool("keymap.classic", false),
        user_bindings: user_bindings.clone(),
    };
    let mcp = {
        let s = brouillon_mcp::settings::McpSettings::from_toml(&settings_text);
        if cli.mcp { s.enable_for_session() } else { s }
    };
    // A custom --settings-dir keeps its own link folder (and so its own token and instance file).
    let link_dir = if cli.settings_dir.is_some() { paths.root().join("link") } else { brouillon_mcp::link::default_link_dir() };
    let _ = STARTUP.set(Startup {
        mcp,
        link_dir,
        folder: cli.folder.clone(),
        classic_keymap: settings.get_bool("keymap.classic", false),
        help_facts,
        help_env,
        paths,
        targets: targets.clone(),
        no_session,
        backup,
        auto_reload_unmodified,
        user_bindings,
        recovered,
        recovered_clean,
    });

    trace_stage("settings, keymap, journal, session loaded");
    // First file of the command line; with none, a blank document (the Welcome page or the
    // restored session replaces it). The 100,000-line sample only exists behind `--sample`.
    let first_path: Option<PathBuf> = match targets.first() {
        Some(t) => Some(t.0.clone()),
        None if cli.sample => Some(PathBuf::from(SAMPLE_PATH)),
        None => None,
    };
    // A huge BOM-less UTF-8 file goes straight into the rope in chunks (no 100 MB `String` next to it).
    // A routed document (PDF, Word...) is never decoded as text: empty read-only placeholder + hex view.
    let first_kind = first_path.as_deref().map(tabs::kind_for_open).unwrap_or(brouillon::kind::FileKind::Text);
    let streamed = if first_kind.routed_to_viewer() { None } else { first_path.as_deref().and_then(stream_huge_file) };
    let (text, startup_meta, prebuilt) = match streamed {
        Some((buf, meta)) => {
            // Only the head is kept as text (language detection reads the first line).
            let mut head = String::new();
            buf.visit_str(0..buf.byte_len().min(8192), &mut |c| {
                head.push_str(c);
                false
            });
            let first = head.as_str();
            let mut cut = 1024.min(first.len());
            while !first.is_char_boundary(cut) {
                cut -= 1;
            }
            (first[..cut].to_string(), meta, Some(buf))
        }
        None => {
            let (t, m) = match first_path.as_ref() {
                Some(_) if first_kind.routed_to_viewer() => (String::new(), tabs::DocMeta { readonly: true, ..tabs::DocMeta::default() }),
                Some(p) => load_or_generate(p, cli.sample),
                None => (String::new(), tabs::DocMeta::default()),
            };
            (t, m, None)
        }
    };

    // Day-4 RAM optim + alpha.4 state refactor : Buffer is wrapped in Arc
    // (rope is structurally shared, so Arc::new + swap is O(1)) and we
    // store the Arc inside an RwSignal so the view reactively re-renders
    // when the buffer is swapped (e.g. Ctrl-O loads a new file, Find/Replace
    // commits, or the future undo system rolls back).
    trace_stage("file read");
    let initial_buffer = Arc::new(prebuilt.unwrap_or_else(|| Buffer::from_str(&text)));
    trace_stage("buffer built");

    eprintln!(
        "[startup] loaded {} lines, {} bytes",
        initial_buffer.line_count(),
        initial_buffer.byte_len(),
    );

    // Stash the resolved path so Ctrl-S writes back here by default ;
    // Ctrl-O updates it when a new file is selected. The text-for-parse
    // is moved into app_view so the bg-parse thread can install the
    // Highlighter into a signal via `create_ext_action` (the GUI scope
    // doesn't exist yet here in main()).
    let initial_path = first_path;
    // Over the syntax-highlighting limit nothing parses the text, and the rope already holds it:
    // keep only the head (language detection reads the first line) so the 100 MB `String` is
    // freed here instead of living on until the window closure runs.
    let initial_text_for_parse = if text.len() > LIVE_HIGHLIGHT_MAX_BYTES {
        let mut cut = 8192.min(text.len());
        while !text.is_char_boundary(cut) {
            cut -= 1;
        }
        text[..cut].to_string()
    } else {
        text
    };

    #[cfg(windows)]
    brouillon::win_icon::spawn_taskbar_icon_setter();
    trace_stage("entering Application::run");
    #[cfg(feature = "test-input")]
    brouillon::test_input::start_from_env();
    Application::new()
        .window(
            move |_| app_view(
                initial_buffer.clone(),
                initial_path.clone(),
                // Moved, not cloned: the closure is FnOnce, and a clone would keep
                // a second copy of the whole file alive for the app's lifetime.
                initial_text_for_parse,
                session_snap.clone(),
                startup_meta.clone(),
            ),
            Some(
                window_icon(WindowConfig::default())
                    .size(Size::new(1100.0, 720.0))
                    .title(format!("Brouillon {} (build {}, {}) — fast editor, doesn't phone home - {}", brouillon::buildinfo::VERSION, brouillon::buildinfo::BUILD, brouillon::buildinfo::DATE_UTC, brouillon::render_mode::effective_render_mode().long())),
            ),
        )
        .run();

    // No journal clean-up here: unsaved tabs were journaled on WindowClosed
    // (`persist_unsaved_on_exit`); entries leave the journal when the tab is
    // saved or closed, never because the app quit.
}

/// Final journal write on a normal exit: keep every tab that still holds
/// unsaved text (they come back modified at the next launch) and flag the
/// journal as written by a clean shutdown.
fn persist_unsaved_on_exit(tm: &TabManager) {
    save_session(tm);
    journal_unsaved(tm);
    // Journal is written: a verified update that was waiting for the exit may now start.
    brouillon::update_boot::on_exit();
    brouillon::module_ui::on_exit();
}

fn save_session(tm: &TabManager) {
    if startup().no_session {
        return;
    }
    if let Err(e) = session::save_to_dir(tm, startup().paths.root()) {
        eprintln!("[session] save error: {e}");
    }
}

fn journal_unsaved(tm: &TabManager) {
    if startup().no_session {
        return;
    }
    let journal = brouillon::recovery::Journal::new(startup().paths.journal_dir());
    if let Err(e) = journal.snapshot_tabs(tm).and_then(|_| journal.mark_clean_exit()) {
        eprintln!("[recovery] could not journal unsaved tabs on exit: {e}");
    }
}

/// Title-bar / taskbar / Alt+Tab icon: raw 32x32 RGBA generated by tools/icons.
fn window_icon(cfg: WindowConfig) -> WindowConfig {
    const RGBA: &[u8] = include_bytes!("../assets/window-icon-32.rgba");
    match floem::window::Icon::from_rgba(RGBA.to_vec(), 32, 32) {
        Ok(icon) => cfg.window_icon(icon),
        Err(_) => cfg,
    }
}

/// If the file exists, read it. If it does not: with `generate` (the `--sample`
/// development option) write the deterministic 100k-line sample there and read
/// that; otherwise open it as a new, empty file (saving creates it). Returns the
/// raw text so we can build buffer + lines + parse tree without re-reading.
fn stream_huge_file(path: &std::path::Path) -> Option<(Buffer, tabs::DocMeta)> {
    if std::fs::metadata(path).ok()?.len() < LIVE_HIGHLIGHT_MAX_BYTES as u64 {
        return None;
    }
    brouillon::external::note(path); // stamp before the read (see external.rs)
    // Large file mode: the lazy store (nothing read at open) when the size and the encoding allow it.
    if let Ok(Some((buf, meta))) = tabs::try_open_lazy(path) {
        eprintln!("[startup] opened {} bytes lazily from {} (large file mode), eol={}", buf.byte_len(), path.display(), meta.eol_kind.name());
        return Some((buf, meta));
    }
    let s = brouillon::file_io::read_utf8_rope(path).ok()??;
    let doc = brouillon::file_io::LoadedDoc { text: String::new(), codepage: "utf-8", bom: false, eol: s.eol, lossy: false };
    let meta = tabs::DocMeta::from_loaded(&doc);
    eprintln!("[startup] streamed {} bytes from {}, codepage=utf-8, eol={}{}", s.rope.byte_len(), path.display(), meta.eol_kind.name(), if meta.eol_mixed { " (mixed)" } else { "" });
    Some((Buffer::from_rope(s.rope), meta))
}

fn load_or_generate(path: &PathBuf, generate: bool) -> (String, tabs::DocMeta) {
    brouillon::external::note(path); // stamp before the read (see external.rs)
    match brouillon::wiring::open_file(path) {
        Ok(loaded) => {
            let meta = tabs::DocMeta::from_loaded(&loaded);
            eprintln!(
                "[startup] loaded {} chars from {}, codepage={}, bom={}, eol={}{}",
                loaded.text.chars().count(),
                path.display(),
                meta.codepage,
                meta.bom,
                meta.eol_kind.name(),
                if meta.eol_mixed { " (mixed)" } else { "" },
            );
            if loaded.lossy {
                eprintln!("[startup] warning: {} contains bytes invalid in {}; saving would alter them", path.display(), meta.codepage);
            }
            (loaded.text, meta)
        }
        Err(e) if !generate => {
            eprintln!("[startup] {} not readable ({e}); opening it as a new empty file", path.display());
            (String::new(), tabs::DocMeta::default())
        }
        Err(e) => {
            eprintln!(
                "[startup] {} not readable ({}), generating {} synthetic lines",
                path.display(),
                e,
                SAMPLE_LINES
            );
            let _ = std::fs::create_dir_all("sample");
            let text = sample::generate(SAMPLE_LINES, SAMPLE_SEED);
            if let Some(parent) = path.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            // Best-effort write; if the disk is read-only we still proceed.
            if let Err(e) = std::fs::write(path, &text) {
                eprintln!("[startup] could not write sample file: {e}");
            }
            (text, tabs::DocMeta::default())
        }
    }
}

/// Convert 1-based line and column to absolute byte offset in the buffer
fn line_col_to_byte(buf: &Buffer, line_1b: usize, col_1b: usize) -> usize {
    let line_idx = line_1b.saturating_sub(1);
    if line_idx >= buf.line_count() {
        return buf.byte_len();
    }
    let line_start = buf.byte_of_line(line_idx);
    let line_str = buf.nth_line(line_idx).unwrap_or_default();
    let col_idx = col_1b.saturating_sub(1);
    let mut byte_offset = 0;
    for (i, c) in line_str.chars().enumerate() {
        if i >= col_idx {
            break;
        }
        byte_offset += c.len_utf8();
    }
    line_start + byte_offset
}

/// Save `text` to `path` with the document's codepage / BOM (BK-007), a backup
/// of the previous content (BK-046) and an atomic write. A lossy conversion
/// asks first (BK-009). Must be called on the GUI thread (it may open a dialog).
fn save_text(buf: &Buffer, path: &std::path::Path, meta: &tabs::DocMeta) -> Result<(), String> {
    let opts = &startup().backup;
    if buf.is_lazy() {
        // Large file mode: UTF-8 only, streamed from the piece table (see `wiring::save_document_lazy`).
        // The caller re-bases its buffer on the written file (`Buffer::lazy_rebase_after_external_save`).
        let saved = if meta.codepage == "utf-8" {
            brouillon::wiring::save_document_lazy(path, buf, opts)
        } else {
            // opened through the transcoding sidecar: written back in the original encoding
            brouillon::wiring::save_document_lazy_encoded(path, buf, meta, false, opts)
        };
        let result = match saved {
            brouillon::wiring::SaveResult::Saved { .. } => Ok(()),
            brouillon::wiring::SaveResult::Failed(e) => Err(e),
            brouillon::wiring::SaveResult::Lossy { .. } => Err("lossy conversion".into()),
        };
        if result.is_ok() {
            brouillon::external::note(path);
        }
        return result;
    }
    let text = buf.rope();
    // The disk changed since this document was opened / last saved and holds other
    // content: write nothing (the caller shows the Overwrite / Reload / Save As bar).
    let spec = brouillon::file_io::SaveSpec { codepage: meta.codepage, bom: meta.bom, eol: None };
    if brouillon::external::save_gate_pieces(path, || text.chunks(), &spec) == brouillon::external::SaveGate::Conflict {
        return Err(brouillon::external_ui::CONFLICT.into());
    }
    let result = save_text_unguarded(text, path, meta, opts);
    if result.is_ok() {
        brouillon::external::note(path);
    }
    result
}

fn save_text_unguarded(text: &crop::Rope, path: &std::path::Path, meta: &tabs::DocMeta, opts: &brouillon::backup::BackupOptions) -> Result<(), String> {
    use brouillon::wiring::{save_document_rope as save_document, SaveResult};
    match save_document(path, text, meta, false, opts) {
        SaveResult::Saved { .. } => Ok(()),
        SaveResult::Failed(e) => Err(e),
        SaveResult::Lossy { unmappable, codepage } => {
            let warning = brouillon::file_io::lossy_warning(unmappable, codepage);
            let proceed = rfd::MessageDialog::new()
                .set_level(rfd::MessageLevel::Warning)
                .set_title("Characters will be lost")
                .set_description(format!("{warning}. They will be replaced when saving. Save anyway?"))
                .set_buttons(rfd::MessageButtons::YesNo)
                .show()
                == rfd::MessageDialogResult::Yes;
            if !proceed {
                return Err("save cancelled (lossy conversion refused)".into());
            }
            match save_document(path, text, meta, true, opts) {
                SaveResult::Saved { .. } => Ok(()),
                SaveResult::Failed(e) => Err(e),
                SaveResult::Lossy { .. } => Err("lossy conversion still refused".into()),
            }
        }
    }
}

/// Top-level view: title bar (FPS) + virtualized editor body + status line.
fn app_view(
    initial_buffer: Arc<Buffer>,
    initial_path: Option<PathBuf>,
    text_for_parse: String,
    session_snap: Option<SessionSnapshot>,
    startup_meta: tabs::DocMeta,
) -> impl IntoView {
    trace_stage("app_view begin");
    let startup_enc = startup_meta.legacy_encoding();
    let startup_eol = startup_meta.eol_mode();
    // Reactive signals driving the live header / status line.
    let fps_sig: RwSignal<f64> = create_rw_signal(0.0);
    let ram_sig: RwSignal<String> = create_rw_signal(String::from("--"));
    let ram_detail_sig: RwSignal<String> = create_rw_signal(String::from("Memory: measuring..."));
    // Cursor is stored 1-based (`(line, col)`) for display ; the buffer lookup
    // subtracts 1 to index from 0. (1, 1) means "first char of first line".
    let cursor_sig: RwSignal<(usize, usize)> = create_rw_signal((1, 1));
    let selection_sig: RwSignal<Selection> = create_rw_signal(Selection::new((1, 1), (1, 1), false));
    let is_dragging_selection: RwSignal<bool> = create_rw_signal(false);
    // Secondary carets of a multi-caret session (1-based line, col); the primary is `cursor_sig`.
    let extra_carets_sig: RwSignal<Vec<(usize, usize)>> = create_rw_signal(Vec::new());
    // Gutter line numbers (`editor.line_numbers`, View > Toggle Line Numbers).
    let line_numbers_sig: RwSignal<bool> = create_rw_signal({
        let text = std::fs::read_to_string(startup().paths.settings_file()).unwrap_or_default();
        brouillon::settings::Settings::build(&[(brouillon::settings::Layer::User, text.as_str())]).get_bool("editor.line_numbers", true)
    });
    // Indent guides, column ruler, minimap (settings-driven view options) and distraction-free mode.
    let editor_opts = {
        let text = std::fs::read_to_string(startup().paths.settings_file()).unwrap_or_default();
        let st = brouillon::settings::Settings::build(&[(brouillon::settings::Layer::User, text.as_str())]);
        let mut o = brouillon::editor_view::EditorOpts::new(line_numbers_sig);
        o.indent_guides.set(st.get_bool("editor.indent_guides", true));
        o.ruler_column.set(st.get_i64("editor.ruler_column", 0).clamp(0, 1000) as usize);
        o.minimap.set(st.get_bool("editor.minimap", false));
        o.tab_size = st.get_i64("editor.tab_size", 4).clamp(1, 16) as usize;
        o.apply_settings(&st);
        o
    };
    let distraction_sig: RwSignal<bool> = create_rw_signal(false);
    // Column used when the ruler is switched on from the menu (the configured one, else 80).
    let ruler_default_sig: RwSignal<usize> = create_rw_signal({
        let c = editor_opts.ruler_column.get_untracked();
        if c > 0 { c } else { 80 }
    });
    let active_menu_dropdown: RwSignal<Option<&'static str>> = create_rw_signal(None);
    let encoding_sig: RwSignal<brouillon::encoding::DocumentEncoding> = create_rw_signal(startup_enc);
    let eol_sig: RwSignal<brouillon::encoding::EolMode> = create_rw_signal(startup_eol);

    let ai_visible_sig = create_rw_signal(false);
    let ai_messages_sig = create_rw_signal(Vec::<brouillon::ai_panel::ChatMessage>::new());
    let ai_input_sig = create_rw_signal(String::new());
    let ai_privacy_sig = create_rw_signal(brouillon::ai_panel::PrivacyDot::Local);
    let mcp_client_ref: Arc<Mutex<Option<brouillon::mcp_client::McpClient>>> = Arc::new(Mutex::new(None));

    // Folders are browsed in the sidebar Browser section (`brouillon_ui::browser_view`); `--folder DIR`,
    // File > Open Folder and the last folder are applied once the remote UI exists, below.
    let sftp_visible_sig = create_rw_signal(false);
    let sftp_state = brouillon::sftp_panel::SftpPanelState::new("localhost".to_string(), String::new(), String::new(), "22".to_string());

    // alpha.4 state refactor : the buffer lives behind an RwSignal<Arc<Buffer>>
    // so the view re-renders on swap (Ctrl-O, Find/Replace commit, future undo).
    // Reads use `.get()` (subscribes the surrounding effect) or
    // `.get_untracked()` (one-shot, for button actions). Writes wrap a fresh
    // Arc::new(Buffer::from_str(...)) — crop::Rope is structurally shared so
    // the copy is O(1) of the rope tree.
    let buffer_sig: RwSignal<Arc<Buffer>> = create_rw_signal(initial_buffer.clone());
    let path_sig: RwSignal<Option<PathBuf>> = create_rw_signal(initial_path.clone());
    let remote_path_sig: RwSignal<Option<String>> = create_rw_signal(None);

    // alpha.8 — syntax painter foundation : Highlighter lives in a signal so
    // the editor body's view_fn can read it per-frame and compute spans
    // per visible line. None until bg-parse completes (~3 s on 100k LOC
    // Rust file) ; until then, viewport renders un-highlighted default-fg.
    let highlighter_sig: RwSignal<Option<Arc<Mutex<Highlighter>>>> = create_rw_signal(None);
    // Language of the startup document (name, extension, then shebang / first line).
    let initial_lang = initial_path.as_ref()
        .map(|p| brouillon::grammars::detect_language_for_text(p, &text_for_parse))
        .unwrap_or(brouillon::grammars::LanguageId::PlainText);

    // bg-parse moved out of main() into app_view so the GUI scope exists
    // for `create_ext_action` (Floem's documented cross-thread escape
    // hatch). After parsing, the action runs on the GUI thread and
    // installs the Highlighter into `highlighter_sig` — triggers a
    // viewport re-render which then paints spans.
    {
        // The parse reads the buffer's rope (shared, not copied); the startup `String` is only kept for the
        // language detection above and is released here instead of living until the parse thread ends.
        drop(text_for_parse);
        let rope_for_parse = initial_buffer.rope().clone();
        let lang = initial_lang;
        // Huge document: a full syntax-tree parse costs minutes and a second copy of the text.
        // Same threshold as the live re-parse below; the editor stays plain text (see the status message).
        let doc_bytes = initial_buffer.byte_len();
        let skip_parse = initial_buffer.is_lazy() || doc_bytes > LIVE_HIGHLIGHT_MAX_BYTES || rust_tree_too_big(lang, doc_bytes);
        if skip_parse {
            eprintln!("[bg-parse] skipped: {} bytes is over the {} byte syntax-highlighting limit", doc_bytes, LIVE_HIGHLIGHT_MAX_BYTES);
        }
        let install_hl = create_ext_action(Scope::current(), move |hl: Highlighter| {
            highlighter_sig.set(Some(Arc::new(Mutex::new(hl))));
            eprintln!("[bg-parse] highlighter installed in signal");
        });
        if !skip_parse {
        std::thread::spawn(move || {
            let start = std::time::Instant::now();
            let mut hl = Highlighter::new(lang);
            hl.parse_rope(&rope_for_parse);
            eprintln!(
                "[bg-parse] done in {} ms, parsed={}, lang={:?}",
                start.elapsed().as_millis(),
                hl.is_parsed(),
                lang
            );
            install_hl(hl);
        });
        }
    }

    // ── Model Context Protocol (MCP) and AI Assistant logic ──
    let (mcp_tx, mcp_rx) = std::sync::mpsc::channel::<brouillon::mcp_client::JsonRpcResponse>();
    let mcp_trigger = floem::reactive::Trigger::new();

    let mcp_scope = Scope::current();
    mcp_scope.create_effect(move |_| {
        mcp_trigger.track();
        while let Ok(resp) = mcp_rx.try_recv() {
            let reply = if let Some(err) = resp.error {
                format!("Error: {:?}", err)
            } else if let Some(res) = resp.result {
                if let Some(reply_val) = res.get("reply") {
                    reply_val.as_str().unwrap_or("").to_string()
                } else {
                    format!("Result: {}", res)
                }
            } else {
                "Empty response from server".to_string()
            };
            ai_messages_sig.update(|msgs| {
                msgs.push(brouillon::ai_panel::ChatMessage {
                    is_user: false,
                    content: reply,
                });
            });
        }
    });

    let mcp_tx_clone = mcp_tx.clone();
    let on_response_cb = move |resp| {
        let _ = mcp_tx_clone.send(resp);
        floem::ext_event::register_ext_trigger(mcp_trigger);
    };

    let mcp_client_ref_clone = mcp_client_ref.clone();
    let on_send_message = move |msg: String| {
        // 1. Add user message to UI
        ai_messages_sig.update(|msgs| {
            msgs.push(brouillon::ai_panel::ChatMessage {
                is_user: true,
                content: msg.clone(),
            });
        });

        // 2. Spawn MCP client process if needed
        let mut client_guard = mcp_client_ref_clone.lock().unwrap();
        if client_guard.is_none() {
            let command = "python";
            let args = vec!["sample/mock_mcp_server.py".to_string()];
            let on_resp = on_response_cb.clone();
            match brouillon::mcp_client::McpClient::spawn(command, &args, move |resp| {
                on_resp(resp);
            }) {
                Ok(client) => {
                    *client_guard = Some(client);
                    ai_privacy_sig.set(brouillon::ai_panel::PrivacyDot::Local);
                    eprintln!("[mcp] spawned mock mcp server subprocess");
                }
                Err(e) => {
                    eprintln!("[mcp] failed to spawn mcp server: {e}");
                    ai_messages_sig.update(|msgs| {
                        msgs.push(brouillon::ai_panel::ChatMessage {
                            is_user: false,
                            content: format!("Failed to spawn local MCP server: {e}. Make sure Python is in your PATH."),
                        });
                    });
                    return;
                }
            }
        }

        // 3. Send request
        if let Some(ref client) = *client_guard {
            let id = rand::random::<i64>().abs();
            let params = serde_json::json!({
                "message": msg
            });
            if let Err(e) = client.send_request(id, "chat", params) {
                eprintln!("[mcp] send error: {e}");
            }
        }
    };


    // alpha.9 — re-parse on buffer mutation. Without this, the syntax
    // painter shows STALE colors after Find/Replace commits, Ctrl-O
    // swaps, or tab switches. Every buffer_sig change spawns a fresh
    // parse thread ; the last `install_hl` to complete wins (acceptable
    // race for the alpha — debouncing + cancellation is a polish task
    // for v0.x). The initial buffer fire is skipped via an
    // `AtomicBool::swap` so we don't double-parse on startup (the
    // bg-parse block above already handles it).
    {
        let parsed_once = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let seq_counter = Arc::new(std::sync::atomic::AtomicU64::new(0));
        let hl_inbox: Arc<Mutex<Option<Arc<Mutex<Highlighter>>>>> = Arc::new(Mutex::new(None));
        let hl_trigger = Scope::current().create_trigger();
        {
            let inbox = hl_inbox.clone();
            create_effect(move |_| {
                hl_trigger.track();
                if let Some(hl) = inbox.lock().unwrap().take() {
                    // Same Arc after an incremental update: setting the signal still repaints.
                    highlighter_sig.set(Some(hl));
                }
            });
        }
        create_effect(move |_| {
            let buf = buffer_sig.get();
            let path_opt = path_sig.get();
            let rpath_opt = remote_path_sig.get();
            if !parsed_once.swap(true, std::sync::atomic::Ordering::SeqCst) {
                return;
            }
            // Huge document: copying it to a String on every keystroke froze typing for
            // seconds (100 MB). Past this size the live highlighter is dropped on the
            // first edit (plain text) instead of being kept stale.
            if buf.is_lazy() || buf.byte_len() > LIVE_HIGHLIGHT_MAX_BYTES {
                if highlighter_sig.get_untracked().is_some() {
                    highlighter_sig.set(None);
                }
                return;
            }
            // The rope is shared (O(1) clone); it becomes a `String` only in the worker thread and only
            // after the 150 ms pause, so a burst of keystrokes no longer copies the whole file once each.
            let rope = buf.rope().clone();
            let seq = seq_counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1;
            let seq_counter_clone = seq_counter.clone();

            // One shared inbox + trigger (created once, below): a per-keystroke `create_ext_action`
            // left its scope, trigger and effect alive whenever a newer keystroke superseded the parse.
            let install_hl = {
                let inbox = hl_inbox.clone();
                move |hl: Arc<Mutex<Highlighter>>| {
                    *inbox.lock().unwrap() = Some(hl);
                    floem::ext_event::register_ext_trigger(hl_trigger);
                }
            };
            let current_hl = highlighter_sig.get_untracked();
            std::thread::spawn(move || {
                std::thread::sleep(std::time::Duration::from_millis(150));
                if seq_counter_clone.load(std::sync::atomic::Ordering::SeqCst) != seq {
                    return;
                }
                // No copy of the document here: language detection reads the first line, and the
                // table-driven highlighter reads the shared rope in place (Rust still needs one String).
                let text_len = rope.byte_len();
                let lang = if let Some(p) = path_opt {
                    brouillon::grammars::detect_language_for_text(&p, &brouillon::wiring::rope_head(&rope, 512))
                } else if let Some(rp) = rpath_opt {
                    brouillon::grammars::detect_language_from_name(&rp)
                } else {
                    brouillon::grammars::LanguageId::PlainText
                };
                let start = std::time::Instant::now();
                // Same language and a table-driven lexer: update the existing highlighter
                // incrementally (apply_edit on the changed span) instead of rebuilding it.
                if let Some(h) = current_hl {
                    let applied = match h.lock() {
                        Ok(mut g) if g.language() == lang && g.is_incremental() => {
                            g.apply_rope(&rope);
                            true
                        }
                        _ => false,
                    };
                    if applied {
                        if seq_counter_clone.load(std::sync::atomic::Ordering::SeqCst) == seq {
                            eprintln!("[reparse] incremental in {} ms ({} bytes, lang={:?})", start.elapsed().as_millis(), text_len, lang);
                            install_hl(h);
                        }
                        return;
                    }
                }
                // A tree-sitter tree costs about 30x the source: over RUST_TREE_MAX_BYTES a Rust file stays plain.
                let lang = if rust_tree_too_big(lang, text_len) { brouillon::grammars::LanguageId::PlainText } else { lang };
                let mut hl = Highlighter::new(lang);
                hl.parse_rope(&rope);
                if seq_counter_clone.load(std::sync::atomic::Ordering::SeqCst) == seq {
                    eprintln!(
                        "[reparse] done in {} ms ({} bytes, lang={:?})",
                        start.elapsed().as_millis(),
                        text_len,
                        lang
                    );
                    install_hl(Arc::new(Mutex::new(hl)));
                }
            });
        });
    }

    // Find/Replace overlay state. Lives for the lifetime of the window — the
    // signals are cheap (a handful of bytes each); keeping the same Arc means
    // toggling the dialog preserves the query/replacement text between opens.
    let find_replace_state = Arc::new(FindReplaceState::new());
    let fr_for_keys = find_replace_state.clone();
    let fr_for_overlay = find_replace_state.clone();
    let fr_for_apply = find_replace_state.clone();

    // M1 Round 3 wire-ups: theme cycle (Ctrl-T), preset toggle (Cmd-Shift-M),
    // command palette (Ctrl-K), file open dialog (Ctrl-O).
    // Saved dark / light / system mode -> shared theme index (see theme_mode.rs).
    let theme_idx: RwSignal<usize> = brouillon::theme_mode::init(startup().paths.settings_file());
    let preset_sig: RwSignal<Preset> = create_rw_signal(DEFAULT_PRESET);
    let crayon_mode_sig: RwSignal<bool> = create_rw_signal(false);
    let sketch_doc_sig: RwSignal<brouillon::sketch::SketchDocument> = create_rw_signal(brouillon::sketch::SketchDocument::new());
    let active_tool_sig: RwSignal<brouillon::sketch::SketchTool> = create_rw_signal(brouillon::sketch::SketchTool::Ballpoint);
    let active_color_sig: RwSignal<brouillon::sketch::SketchColor> = create_rw_signal(brouillon::sketch::SketchColor::Black);
    let active_width_sig: RwSignal<f32> = create_rw_signal(2.0);
    let infinite_layout_sig: RwSignal<bool> = create_rw_signal(false);

    // M2 Sprint 1 — new signals for multi-language, hex view, folding, keymap
    let language_sig: RwSignal<brouillon::grammars::LanguageId> = create_rw_signal(initial_lang);
    let hex_mode_sig: RwSignal<bool> = create_rw_signal(false);
    let fold_state_sig: RwSignal<brouillon::folding::FoldState> = create_rw_signal(
        brouillon::folding::FoldState::empty()
    );
    // Fold regions follow the text: re-detected on every edit (small documents), per document. A
    // different document (tab switch) starts from its own detection instead of inheriting folds.
    {
        let last_path: Rc<std::cell::RefCell<Option<Option<PathBuf>>>> = Rc::new(std::cell::RefCell::new(None));
        create_effect(move |_| {
            let buf = buffer_sig.get();
            let path = path_sig.get();
            if buf.is_lazy() || buf.byte_len() > FOLD_REFRESH_MAX_BYTES {
                return;
            }
            // Straight from the rope (one u32 per line is all that is kept): no copy of the document per keystroke.
            let same_doc = last_path.borrow_mut().replace(path.clone()) == Some(path);
            let mut fs = fold_state_sig.get_untracked();
            let changed = if same_doc { fs.refresh_rope(buf.rope()) } else { fs = brouillon::folding::FoldState::new(brouillon::folding::compute_fold_regions_rope(buf.rope())); true };
            if changed {
                fold_state_sig.set(fs);
            }
        });
    }
    let markdown_preview_sig: RwSignal<bool> = create_rw_signal(false);
    // A help page is the active tab: its preview takes the whole width (the raw Markdown stays one Ctrl+Shift+V away).
    let help_active_sig: RwSignal<bool> = create_rw_signal(false);
    trace_stage("app_view: bg-parse + effects wired");
    let _keymap = brouillon::keymap::load_effective_keymap();

    // M2 Sprint 3 — snippets, user tools, auto-complete, output panel
    let snippet_store = brouillon::snippets::builtin_snippets();
    let user_snippets = brouillon::snippets::load_user_snippets();
    let merged_snippets = {
        let mut all = snippet_store.all().to_vec();
        all.extend_from_slice(user_snippets.all());
        brouillon::snippets::SnippetStore::new(all)
    };
    let _snippet_store_sig: RwSignal<brouillon::snippets::SnippetStore> = create_rw_signal(merged_snippets);
    let user_tools = brouillon::shell_runner::default_tools();
    let _user_tools_sig: RwSignal<Vec<brouillon::shell_runner::UserTool>> = create_rw_signal(user_tools);
    let output_visible_sig: RwSignal<bool> = create_rw_signal(false);
    let _auto_complete_config = brouillon::auto_complete::CompletionConfig::default();
    eprintln!("[startup] loaded {} builtin snippets, {} user tools", 
        _snippet_store_sig.get_untracked().len(),
        _user_tools_sig.get_untracked().len());
    // M2 Auto-complete overlay signals
    let ac_visible = create_rw_signal(false);
    let ac_completions: RwSignal<Vec<brouillon::auto_complete::Completion>> = create_rw_signal(Vec::new());
    let ac_index = create_rw_signal(0);
    // Buffer (Arc address) that was current when a plain arrow key was pressed: while the text is
    // still that buffer the popup stays closed, however many times the caret signal fires.
    let ac_suppress = create_rw_signal(0usize);

    {
        let ac_visible = ac_visible;
        let ac_completions = ac_completions;
        let ac_index = ac_index;
        let buffer_sig = buffer_sig;
        let cursor_sig = cursor_sig;
        let language_sig = language_sig;
        let hex_mode_sig = hex_mode_sig;
        let crayon_mode_sig = crayon_mode_sig;
        
        create_effect(move |_| {
            if hex_mode_sig.get() || crayon_mode_sig.get() {
                ac_visible.set(false);
                return;
            }
            let buf = buffer_sig.get();
            let (line_1b, col_1b) = cursor_sig.get();
            if ac_suppress.get_untracked() == Arc::as_ptr(&buf) as usize {
                ac_visible.set(false);
                return;
            }
            if buf.is_lazy() || buf.byte_len() > LIVE_HIGHLIGHT_MAX_BYTES {
                // Word completion scans the whole document: not on huge files.
                ac_visible.set(false);
                return;
            }
            let line_idx = line_1b.saturating_sub(1);
            let line_str = buf.nth_line(line_idx).unwrap_or_default();
            let prefix = brouillon::auto_complete::get_prefix_at(&line_str, col_1b);
            
            if prefix.len() >= 2 {
                let config = brouillon::auto_complete::CompletionConfig::default();
                // Straight from the rope, line by line: no copy of the whole document per keystroke.
                let rope = buf.rope();
                // The whole document up to COMPLETION_SCAN_BYTES; above it, the lines within half that on each
                // side of the caret (a scan of 16 MB per keystroke is the cost this bounds).
                let (first, last) = brouillon::wiring::completion_window(rope, line_idx, COMPLETION_SCAN_BYTES);
                let buf_comps = brouillon::auto_complete::complete_from_lines(
                    rope.line_slice(first..last).lines().map(|l| l.to_string()),
                    last - first,
                    &prefix,
                    line_idx - first,
                    &config,
                );
                
                let lang_name = language_sig.get().display_name().to_lowercase();
                let kw_comps = brouillon::auto_complete::complete_from_keywords(&prefix, &lang_name, &config);
                
                let merged = brouillon::auto_complete::merge_completions(vec![buf_comps, kw_comps], config.max_results);
                
                if !merged.is_empty() {
                    ac_completions.set(merged);
                    ac_index.set(0);
                    ac_visible.set(true);
                } else {
                    ac_visible.set(false);
                }
            } else {
                ac_visible.set(false);
            }
        });
    }

    let accept_completion = {
        let buffer_sig = buffer_sig;
        let cursor_sig = cursor_sig;
        let ac_visible = ac_visible;
        let ac_completions = ac_completions;
        let ac_index = ac_index;
        move || {
            let comps = ac_completions.get_untracked();
            let idx = ac_index.get_untracked();
            if idx < comps.len() {
                let comp = &comps[idx];
                let buf = buffer_sig.get_untracked();
                let (line_1b, col_1b) = cursor_sig.get_untracked();
                let line_str = buf.nth_line(line_1b - 1).unwrap_or_default();
                let prefix = brouillon::auto_complete::get_prefix_at(&line_str, col_1b);
                
                let prefix_len = prefix.chars().count();
                let byte_pos = line_col_to_byte(&buf, line_1b, col_1b);
                
                let mut nb = (*buf).clone();
                let start_byte = byte_pos - prefix.len();
                nb.remove_range(start_byte..byte_pos);
                nb.insert_at(start_byte, &comp.text);
                
                buffer_sig.set(Arc::new(nb));
                cursor_sig.set((line_1b, col_1b - prefix_len + comp.text.chars().count()));
            }
            ac_visible.set(false);
        }
    };

    // Command registry (single wiring point, see wiring.rs): palette entries,
    // chord table and MCP-ready metadata all come from it.
    trace_stage("app_view: before registry");
    let (registry, registry_errors) = brouillon::wiring::build_registry();
    trace_stage("app_view: registry built");
    for e in &registry_errors {
        eprintln!("[registry] rejected: {e}");
    }
    eprintln!("[registry] {} commands registered", registry.len());
    let registry = Arc::new(registry);
    let key_bindings = brouillon::wiring::dispatch_bindings(&registry, startup().classic_keymap, &startup().user_bindings);
    let dispatcher = Rc::new(RefCell::new(brouillon::wiring::Dispatcher::new(key_bindings.clone())));

    let palette_state = {
        let mut ps = PaletteState::new();
        ps.set_entries(brouillon::palette::entries_from_registry(&registry, &key_bindings, &brouillon::keymap::Context::new()));
        Arc::new(ps)
    };
    let palette_for_keys = palette_state.clone();
    let palette_for_keyup = palette_state.clone();
    let palette_for_overlay = palette_state.clone();
    let palette_for_dispatch = palette_state.clone();
    let fr_for_dispatch = find_replace_state.clone();

    // alpha.11 — Go-to-line dialog. Small reactive overlay : Ctrl-G or
    // palette edit.go-to-line toggles visible ; Enter parses input as
    // a line number, sets cursor_sig to (line, 1), closes ; Escape closes.
    let goto_visible: RwSignal<bool> = create_rw_signal(false);
    let goto_input: RwSignal<String> = create_rw_signal(String::new());
    // Encoding picker (status bar "encoding" cell / Encoding: Choose Encoding...).
    let encoding_picker_visible: RwSignal<bool> = create_rw_signal(false);
    let encoding_query: RwSignal<String> = create_rw_signal(String::new());
    // Preference surfaces (settings, shortcuts, cliptext): see brouillon-ui `ui_kit` and its siblings.
    let dialogs = brouillon::ui_kit::Dialogs::new();
    // Remote explorer (dev flag: only the built-in fake server is reachable until the SFTP helper is plugged in).
    // Creating it starts no thread and opens no socket: the hub starts workers on the first request.
    // Real SFTP goes through the helper process (brouillon-remote.exe next to this exe); the in-memory fake
    // answers only for host `demo` or BROUILLON_REMOTE_FAKE=1. Nothing is spawned until a site is connected.
    let remote_prompts = brouillon::remote::PromptBroker::new();
    let remote_saves = Arc::new(brouillon::remote::SaveService::new(startup().paths.root().join("remote").join("journal"), startup().paths.root().join("remote").join("backups")));
    let remote_ui = brouillon::remote_view::RemoteUi::new(
        Arc::new(brouillon::remote::RealOrDemoFactory::from_env(Arc::new(brouillon::remote::HelperFactory::new(remote_prompts.clone()))).with_demo_password_prompt(brouillon::remote::RealOrDemoFactory::ask_password_env().then(|| remote_prompts.clone()))),
        startup().paths.root().join("sites.toml"),
        dialogs.remote_connect,
        dialogs.remote_sites,
        dialogs.remote_dialog,
    );
    remote_ui.attach_services(remote_prompts, remote_saves);
    // Saved passwords: the vault is opened lazily, on the first password to read or to keep (never at startup).
    // Portable mode keeps nothing; no vault failure ever falls back to a plaintext copy.
    // Dev hook BROUILLON_REMOTE_FAKE_ASK_PASSWORD=1 also lets a --settings-dir (portable) run keep a password, so the vault UI can be driven.
    let remote_creds = brouillon::remote::Credentials::for_app(startup().paths.root(), startup().paths.is_portable() && !brouillon::remote::RealOrDemoFactory::ask_password_env());
    remote_ui.attach_credentials(remote_creds.clone());
    // Keyboard ownership: every press on the editor side (text, gutter, tabs, minimap) gives the keyboard to the
    // document, even when the view stops the event before it reaches the window root.
    brouillon::kbd_focus::set_editor_pointer_hook(Rc::new({
        let remote_ui = remote_ui.clone();
        move || remote_ui.kbd_event(brouillon::kbd_focus::Ev::EditorPointer)
    }));
    {
        // The palette and the preference-style dialogs are overlays on top of the pane's focus.
        let ui = remote_ui.clone();
        let palette_visible = palette_state.visible;
        create_effect(move |_| {
            let open = palette_visible.get();
            ui.palette_open.set(open);
            if open {
                ui.kbd_event(brouillon::kbd_focus::Ev::OverlayOpened);
            }
        });
        let ui = remote_ui.clone();
        create_effect(move |_| {
            let open = dialogs.settings.get() || dialogs.shortcuts.get() || dialogs.import.get() || dialogs.about.get() || dialogs.firstrun.get();
            ui.other_modal.set(open);
            if open {
                ui.kbd_event(brouillon::kbd_focus::Ev::OverlayOpened);
            }
        });
    }
    // Browser: `--folder DIR` shows that folder; else the browser reopens where it was (only if it was open).
    match startup().folder.clone().filter(|d| d.is_dir()) {
        Some(dir) => remote_ui.browse_local(&dir),
        None if remote_ui.browser_visible() => remote_ui.ensure_started(),
        None => {}
    }
    // Editor preferences (Tab key, indent unit, auto-indent, encoding of new documents), re-read
    // from settings.toml at start, after a Preferences change and after an import.
    let prefs_sig: RwSignal<brouillon_core::editor_prefs::EditorPrefs> = create_rw_signal({
        let text = std::fs::read_to_string(startup().paths.settings_file()).unwrap_or_default();
        brouillon_core::editor_prefs::EditorPrefs::from_settings(&brouillon::onboarding::settings_of(&text))
    });
    // First-run screens: shown once (`firstrun.done`), re-openable from Help > First-Run Setup.
    let start_connect: RwSignal<bool> = create_rw_signal(false);
    let wizard = {
        let text = std::fs::read_to_string(startup().paths.settings_file()).unwrap_or_default();
        let settings = brouillon::onboarding::settings_of(&text);
        let w = brouillon::onboarding_view::WizardState::new(dialogs.firstrun, brouillon::theme_mode::parse_settings(&text).0.as_str());
        w.classic.set(settings.get_bool("keymap.classic", false));
        dialogs.firstrun.set(brouillon::onboarding::should_show(&settings, std::env::var_os("BROUILLON_NO_FIRSTRUN").is_some_and(|v| v == "1")));
        w
    };
    // Re-read the Classic flag and the user keymap from disk and swap the chord table in place.
    let read_classic = || {
        let text = std::fs::read_to_string(startup().paths.settings_file()).unwrap_or_default();
        brouillon::settings::Settings::build(&[(brouillon::settings::Layer::User, text.as_str())]).get_bool("keymap.classic", false)
    };
    let rebind: Rc<dyn Fn()> = {
        let (registry, dispatcher) = (registry.clone(), dispatcher.clone());
        Rc::new(move || {
            let (user, _) = std::fs::read_to_string(startup().paths.keymap_file()).map(|t| brouillon::keymap::parse_bindings_toml(&t)).unwrap_or_default();
            dispatcher.borrow_mut().bindings = brouillon::wiring::dispatch_bindings(&registry, read_classic(), &user);
        })
    };

    // alpha.7 — tab bar wire-up. tabs_sig is the source of truth for tab
    // metadata + per-tab paths ; buffer_sig / cursor_sig / path_sig mirror
    // the ACTIVE tab's state for the editor body. On switch / close /
    // new-tab, we sync from `tabs_sig.current()` into the mirror signals.
    // alpha.13 — session persistence : if `~/.brouillon/session.toml` was
    // loaded in main(), restore each tab here ; the seed buffer/path from
    // main() are ignored in that case (the session is the source of truth
    // for "what was open last").
    let mut initial_tm = TabManager::new();
    trace_stage("app_view: before session restore");
    let session_restored = if let Some(snap) = session_snap {
        let n = session::restore_into(snap, &mut initial_tm);
        if n > 0 {
            eprintln!("[session] restored {} tab(s) from session.toml", n);
            // Push restored active tab's state into the mirror signals so
            // the editor body picks it up on first render.
            let active = initial_tm.current();
            buffer_sig.set(active.buffer.clone());
            cursor_sig.set(active.cursor);
            path_sig.set(active.path.clone());
            encoding_sig.set(active.encoding);
            eol_sig.set(active.eol);
            true
        } else {
            false
        }
    } else {
        false
    };
    if !session_restored {
        // Fresh launch — seed the single scratch tab with the CLI-arg/sample
        // buffer that main() resolved.
        let cur = initial_tm.current_mut();
        cur.buffer = initial_buffer.clone();
        cur.path = initial_path.clone();
        cur.cursor = (1, 1);
        cur.title = initial_path
            .as_ref()
            .and_then(|p| p.file_name())
            .and_then(|s| s.to_str())
            .map(String::from)
            .unwrap_or_else(|| "untitled-1".to_string());
        cur.encoding = startup_enc;
        cur.eol = startup_eol;
        cur.meta = startup_meta.clone();
        cur.kind = initial_path.as_deref().map(tabs::kind_for_open).unwrap_or(brouillon::kind::FileKind::Text);
        // The drawing saved next to the file (`<file>.sketch.json`), if any.
        if let Some(doc) = initial_path.as_deref().and_then(brouillon::sketch::SketchDocument::load_sidecar) {
            if !doc.pages.is_empty() {
                cur.sketch_saved = doc.stroke_count();
                cur.sketch = doc;
            }
        }
    }
    // Crash recovery: documents journaled by the previous run come back
    // modified (dirty buffers), merged into the tabs the session restored.
    brouillon::recovery::apply_recovered(&mut initial_tm, &startup().recovered, session_restored);
    if !startup().recovered.is_empty() {
        let t = initial_tm.current();
        buffer_sig.set(t.buffer.clone());
        cursor_sig.set(t.cursor);
        path_sig.set(t.path.clone());
    }
    // Welcome page / release notes: only when the user's own documents are not
    // restored (or, for the one-time release notes, as one extra tab).
    {
        let has_content = session_restored || !startup().recovered.is_empty();
        let plan = brouillon::startup_doc::decide(&startup().help_facts, has_content);
        if brouillon::startup_doc::apply(&plan, &mut initial_tm, &startup().help_env) > 0 {
            let t = initial_tm.current();
            buffer_sig.set(t.buffer.clone());
            cursor_sig.set(t.cursor);
            path_sig.set(t.path.clone());
            language_sig.set(brouillon::grammars::LanguageId::Markdown);
            eprintln!("[startup] opened {:?}", plan.open.iter().map(|d| d.title()).collect::<Vec<_>>());
        }
    }
    // The drawing mirror must start from the active tab too: the sync effect
    // writes the mirrors back into the tab, so an empty default here wiped the
    // strokes restored from the session.
    sketch_doc_sig.set(initial_tm.current().sketch.clone());
    trace_stage("app_view: session restored");
    let tabs_sig: RwSignal<TabManager> = create_rw_signal(initial_tm);
    // The Markdown preview follows the help documents (Welcome, Documentation, Release notes).
    brouillon::startup_doc::follow_preview(tabs_sig, markdown_preview_sig, help_active_sig);
    // DEV-ONLY test channel: what the `state` op reports (read on the GUI thread).
    #[cfg(feature = "test-input")]
    {
        let palette_visible = palette_state.visible;
        brouillon::test_input::set_state_hook(move || {
            tabs_sig.with_untracked(|tm| {
                let t = tm.current();
                let text = t.buffer.text();
                serde_json::json!({
                    "active_tab": tm.active_idx(),
                    "tab_count": tm.count(),
                    "tab_titles": tm.iter().map(|t| t.title.clone()).collect::<Vec<_>>(),
                    "tab_title": t.title,
                    "dirty": t.dirty,
                    "caret": [cursor_sig.get_untracked().0, cursor_sig.get_untracked().1],
                    "text_len": text.len(),
                    "text_head": text.chars().take(200).collect::<String>(),
                    "palette_open": palette_visible.get_untracked(),
                })
            })
        });
    }

    // Round 15 — Document selector entries (derived from tabs_sig)
    let doc_entries_sig: RwSignal<Vec<brouillon::doc_selector::DocSelectorEntry>> = create_rw_signal(Vec::new());
    // Round 15 — Recent files signal
    let recent_files_sig: RwSignal<Vec<PathBuf>> = create_rw_signal(session::load_recent());

    // Sync the ACTIVE tab's buffer/cursor/path into the mirror signals.
    // Called from on_switch / on_close / on_new_tab callbacks below.
    let mirrors = brouillon::mirror::Mirrors {
        buffer: buffer_sig,
        cursor: cursor_sig,
        path: path_sig,
        remote_path: remote_path_sig,
        sketch: sketch_doc_sig,
        encoding: encoding_sig,
        eol: eol_sig,
    };
    let sync_active_to_mirrors = move || {
        mirrors.pull_from_active(tabs_sig);
        extra_carets_sig.set(Vec::new());
    };

    let on_switch: Arc<dyn Fn(usize) + 'static> = Arc::new({
        let sync = sync_active_to_mirrors.clone();
        move |idx| {
            tabs_sig.update(|tm| tm.switch_to(idx));
            sync();
        }
    });
    let on_close: Arc<dyn Fn(usize) + 'static> = Arc::new({
        let sync = sync_active_to_mirrors.clone();
        move |idx| {
            // Round 15 — Confirm before closing dirty tabs
            let is_dirty = tabs_sig.with_untracked(|tm| {
                if idx < tm.count() {
                    tm.iter().nth(idx).map(|t| t.dirty).unwrap_or(false)
                } else {
                    false
                }
            });
            if is_dirty {
                let title = tabs_sig.with_untracked(|tm| {
                    tm.iter().nth(idx).map(|t| t.title.clone()).unwrap_or_default()
                });
                match brouillon::confirm::confirm_save(&title) {
                    brouillon::confirm::ConfirmResult::Save => {
                        // Save first, then close. A tab without a file (untitled, drawing
                        // only, remote) used to be closed here WITHOUT saving anything:
                        // it now asks for a file name and keeps the tab on cancel.
                        let (save_path, text, meta, sketch) = tabs_sig.with_untracked(|tm| {
                            tm.iter()
                                .nth(idx)
                                .map(|t| (t.path.clone(), t.buffer.clone(), t.meta.clone(), t.sketch.clone()))
                                .unwrap_or_else(|| (None, Arc::new(Buffer::from_str("")), Default::default(), Default::default()))
                        });
                        let target = match save_path {
                            Some(p) => p,
                            None => {
                                let suggested = if text.byte_len() == 0 && sketch.stroke_count() > 0 { "sketch.txt" } else { "untitled.txt" };
                                match tabs::show_save_dialog(Some(suggested)) {
                                    Some(p) => p,
                                    None => return, // cancelled: keep the tab open
                                }
                            }
                        };
                        if let Err(e) = save_text(&text, &target, &meta) {
                            eprintln!("[file] save before close failed: {e}");
                            return; // keep the tab open: never lose the document
                        }
                        if sketch.stroke_count() > 0 {
                            if let Err(e) = sketch.save_sidecar(&target) {
                                eprintln!("[file] drawing could not be saved: {e}");
                                return;
                            }
                        }
                        tabs_sig.update(|tm| { let _ = tm.close(idx); });
                        sync();
                    }
                    brouillon::confirm::ConfirmResult::Discard => {
                        tabs_sig.update(|tm| { let _ = tm.close(idx); });
                        sync();
                    }
                    brouillon::confirm::ConfirmResult::Cancel => {
                        // Do nothing — user cancelled
                    }
                }
            } else {
                tabs_sig.update(|tm| { let _ = tm.close(idx); });
                sync();
            }
        }
    });
    let on_new_tab: Arc<dyn Fn() + 'static> = Arc::new({
        let sync = sync_active_to_mirrors;
        move || {
            tabs_sig.update(|tm| {
                let _ = tm.new_scratch();
                // `files.default_encoding`: the codepage a new document is saved with.
                let enc = prefs_sig.get_untracked().default_encoding;
                if let Ok(cp) = brouillon::codepages::supported(&enc) {
                    tm.current_mut().meta.codepage = cp.id;
                }
            });
            sync();
        }
    });

    // Snapshot the initial buffer's totals for the status line ; the view
    // reads these reactively from buffer_sig anyway, but we keep the
    // closure signature simple.
    let total_bytes = buffer_sig.get_untracked().byte_len();

    // Background sampler — see fn doc. The thread updates signals every 500ms.
    spawn_metrics_pump(fps_sig, ram_sig, ram_detail_sig);

    // Report cold start once the reactive system has finished its first pass.
    // `create_effect` runs at least once on mount, which is close enough to
    // "first frame" at PoC fidelity. (M1: hook a real post-paint callback.)
    create_effect(move |_| {
        if !first_frame_reported() {
            trace_stage("first frame");
            report_first_frame();
        }
    });

    // alpha.4 : subscribe to pending_replacement from FindReplaceState. When
    // Replace / Replace All commits new text, we wrap it in a fresh Buffer
    // and swap the buffer_sig ; the active path is preserved so the next
    // Ctrl-S writes to the right file.
    // alpha.10 : explicitly mark the new buffer dirty — Buffer::from_str
    // returns dirty=false (clean), but a Find/Replace commit IS a mutation
    // and the status-bar dirty glyph should reflect that until next save.
    {
        let fr = fr_for_apply;
        create_effect(move |_| {
            if let Some(new_text) = fr.pending_replacement.get() {
                // Replace / Replace All: the result is applied as minimal local
                // edits in ONE undo group (apply_edits), so the caret, folds and
                // bookmarks stay anchored and a single Ctrl-Z reverts everything.
                let buf = buffer_sig.get_untracked();
                if buf.is_lazy() {
                    // Large file mode: a whole-document replacement result is never built (see find_replace.rs).
                    eprintln!("[find-replace] ignored: replace-all result on a large-file-mode document");
                    fr.pending_replacement.set(None);
                    return;
                }
                let old_text = buf.text();
                let set = brouillon::edit_apply::diff_edits(&old_text, &new_text);
                let mut nb = (*buf).clone();
                nb.break_typing();
                match nb.apply_edits(&set) {
                    Ok(()) => {
                        tabs_sig.update(|tm| {
                            let m = &mut tm.current_mut().meta.markers;
                            if !m.is_empty() {
                                let mut markers = brouillon::markers::Markers::from_lines(m.iter().copied());
                                markers.apply_edits(&old_text, &set);
                                *m = markers.to_vec();
                            }
                        });
                        eprintln!("[find-replace] applied {} local edit(s) as one undo group", set.len());
                        buffer_sig.set(Arc::new(nb));
                        let last = buffer_sig.get_untracked().line_count().max(1);
                        cursor_sig.update(|c| c.0 = c.0.min(last));
                    }
                    Err(e) => eprintln!("[find-replace] edit rejected: {e}"),
                }
                fr.pending_replacement.set(None);
            }
        });
    }

    // alpha.10 — source-of-truth unification : whenever the mirror signals
    // (buffer_sig / cursor_sig / path_sig) change, push the values back
    // into tabs_sig.current_mut(). Without this, tab titles and dirty
    // glyphs on the ACTIVE tab drift after Find/Replace commits and
    // cursor moves — the user sees stale labels on the very tab they're
    // editing.
    //
    // No feedback loop : the on_switch / Ctrl-N / Ctrl-W / Ctrl-PgUp/Dn
    // paths write to tabs_sig FIRST then set the mirrors from the new
    // active tab. The set-of-mirrors fires this effect, which writes the
    // SAME values back into the just-switched tab — idempotent.
    mirrors.install(tabs_sig);

    // Automatically open a new scratch tab if switching to Crayon Mode when the workspace is empty.
    {
        let on_new_tab = on_new_tab.clone();
        create_effect(move |_| {
            let is_crayon = crayon_mode_sig.get();
            if is_crayon && tabs_sig.with(|tm| tm.is_empty) {
                on_new_tab();
            }
        });
    }

    // alpha.13 — session save-on-change effect. Every mutation to tabs_sig
    // (open file, close tab, new scratch, switch active, cursor move via
    // back-sync from mirrors) writes the snapshot to disk. The save is
    // cheap (atomic rename of a tiny TOML file) and self-throttles via
    // tabs_sig's coalesced reactive updates ; no debounce needed for
    // alpha.13 file sizes. On window close this also runs once, which is
    // why we don't need a separate close-hook.
    {
        // Coalesced: one fsync'd write per quiet 250 ms instead of three per keystroke
        // (buffer, caret and selection mirrors each touch tabs_sig); flushed again on exit.
        let pending = Rc::new(std::cell::Cell::new(false));
        create_effect(move |_| {
            floem::reactive::SignalTrack::track(&tabs_sig);
            if startup().no_session || pending.replace(true) {
                return;
            }
            let pending = pending.clone();
            floem::action::exec_after(std::time::Duration::from_millis(250), move |_| {
                pending.set(false);
                tabs_sig.with_untracked(save_session);
            });
        });
    }

    // alpha.13 — bracket-match indicator. Resolves the bracket at the
    // cursor (if any) and its pair via the bracket_paint helper. Status
    // line subscribes to bracket_paint_sig for a small visual cue.
    // Per-line paint is deferred to alpha.14 — the data is here, just
    // not yet rendered as a colored overlay on the editor body.
    //
    // Cost guardrail : computing the pair requires the full buffer text
    // (rope -> String, O(N)). We cap that at 256 KB so a 100k-LOC file
    // doesn't burn cycles on every cursor tick — bracket matching on
    // multi-MB files is an alpha.14+ concern (rope-aware scan).
    const BRACKET_PAINT_MAX_BYTES: usize = 256 * 1024;
    let bracket_paint_sig: RwSignal<BracketPaint> = create_rw_signal(BracketPaint::None);
    {
        create_effect(move |_| {
            let buf = buffer_sig.get();
            let (line_1b, col_1b) = cursor_sig.get();
            if buf.is_lazy() || buf.byte_len() > BRACKET_PAINT_MAX_BYTES {
                bracket_paint_sig.set(BracketPaint::None);
                return;
            }
            let line0 = line_1b.saturating_sub(1);
            let col0 = col_1b.saturating_sub(1);
            // Walk col0 chars into the line to get a UTF-8 byte position.
            let Some(line_range) = buf.line_byte_range(line0) else {
                bracket_paint_sig.set(BracketPaint::None);
                return;
            };
            let line_text = buf.nth_line(line0).unwrap_or_default();
            let mut byte_pos = line_range.start;
            for (i, ch) in line_text.chars().enumerate() {
                if i == col0 { break; }
                byte_pos += ch.len_utf8();
            }
            let text = buf.text();
            // Pass None for Highlighter — the text fallback is fine for
            // alpha.13 and avoids holding the Mutex across the compute.
            let bp = bracket_paint::compute(&text, byte_pos, None);
            bracket_paint_sig.set(bp);
        });
    }

    // ───────────────────────────────────────────────────────────────────────
    // Integration wiring (orchestrator). Everything below goes through
    // `brouillon::wiring` (registry, aliases, outcome application, file I/O).
    // ───────────────────────────────────────────────────────────────────────
    let status_msg_sig: RwSignal<String> = create_rw_signal(if !startup().recovered.is_empty() {
        brouillon::recovery::recovery_notice(startup().recovered.len(), startup().recovered_clean)
    } else if buffer_sig.get_untracked().is_lazy() {
        "Large file mode: highlighting, wrap, folding, outline and word completion are off".to_string()
    } else if buffer_sig.get_untracked().byte_len() > LIVE_HIGHLIGHT_MAX_BYTES || rust_tree_too_big(initial_lang, buffer_sig.get_untracked().byte_len()) {
        "Large file: full syntax analysis is off".to_string()
    } else {
        match SYNTAX_ERRORS.get().map(|v| v.as_slice()) {
            Some([first, rest @ ..]) => format!("Syntax definitions: {} problem(s), first: {first}", rest.len() + 1),
            _ => String::new(),
        }
    });
    // Signed auto-update (docs: re/47). Starts a background pass after the window exists.
    let update_ui_ctl = brouillon::update_boot::init(
        &startup().paths,
        status_msg_sig,
        move || tabs_sig.with_untracked(|tm| tm.iter().any(|t| t.is_modified())),
        move || tabs_sig.with_untracked(journal_unsaved),
        !startup().no_session,
    );
    let overwrite_sig: RwSignal<bool> = create_rw_signal(false);
    let active_panel_sig: RwSignal<&'static str> = create_rw_signal("editor");
    let results_model_sig: RwSignal<brouillon::results_model::ResultsModel> = create_rw_signal(brouillon::results_model::ResultsModel::new());
    let results_visible_sig: RwSignal<bool> = create_rw_signal(false);
    let output_buf_sig: RwSignal<brouillon::error_patterns::OutputBuffer> = create_rw_signal(
        brouillon::error_patterns::OutputBuffer::new(
            std::env::current_dir().unwrap_or_default(),
            brouillon::error_patterns::builtin_patterns(),
        ),
    );
    let tool_running_sig: RwSignal<bool> = create_rw_signal(false);
    let tool_footer_sig: RwSignal<String> = create_rw_signal(String::new());
    let tool_handle: Rc<RefCell<Option<brouillon::shell_runner::RunHandle>>> = Rc::new(RefCell::new(None));

    // User tools (tools.toml in the data dir). Without a file the three
    // historical Cargo tools keep Ctrl+1..3.
    let tools_cfg: Rc<brouillon::tools_config::ToolsConfig> = Rc::new({
        let mut cfg = brouillon::tools_config::load(&startup().paths.tools_file());
        if cfg.tools.is_empty() {
            let mut text = String::new();
            for (i, t) in brouillon::shell_runner::default_tools().iter().enumerate() {
                let args = t.args.iter().map(|a| toml::Value::String(a.clone()).to_string()).collect::<Vec<_>>().join(", ");
                text.push_str(&format!(
                    "[[tool]]\nname = {}\ncommand = {}\nargs = [{}]\nslot = {}\n",
                    toml::Value::String(t.name.clone()),
                    toml::Value::String(t.command.clone()),
                    args,
                    i + 1
                ));
            }
            cfg = brouillon::tools_config::parse(&text);
        }
        for w in &cfg.warnings {
            eprintln!("[tools] {w}");
        }
        cfg
    });

    // Open a path as a tab (or activate it) and place the caret. Used by the
    // open dialog, the command line, IPC requests, search results and errors.
    let open_path = {
        let sync = sync_active_to_mirrors;
        move |p: PathBuf, line: Option<usize>, col: Option<usize>| {
            let req = brouillon::ipc::Request::Open { path: p.clone(), line, col, readonly: false, new_window: false, wait: false };
            let pos = tabs_sig.try_update(|tm| brouillon::wiring::apply_request(tm, &req)).flatten();
            sync();
            if let Some((l, c)) = pos {
                let last = buffer_sig.get_untracked().line_count().max(1);
                cursor_sig.set((l.min(last), c));
                selection_sig.set(Selection::new((1, 1), (1, 1), false));
            }
            if let Some(path) = path_sig.get_untracked() {
                let buf = buffer_sig.get_untracked();
                if buf.is_lazy() {
                    // Large file mode: language from the head of the file, no fold regions (no whole-text scan).
                    let head = buf.slice(0..{
                        let mut cut = buf.byte_len().min(8192);
                        while !buf.is_char_boundary(cut) {
                            cut -= 1;
                        }
                        cut
                    });
                    language_sig.set(brouillon::grammars::detect_language_for_text(&path, &head));
                    fold_state_sig.set(brouillon::folding::FoldState::empty());
                } else {
                    // Language detection reads the first line only; folds come from the rope (none above 16 MB).
                    let head = brouillon::wiring::rope_head(buf.rope(), 512);
                    let lang = brouillon::grammars::detect_language_for_text(&path, &head);
                    language_sig.set(lang);
                    fold_state_sig.set(brouillon::folding::detect_for_open(buf.rope()));
                }
                session::push_recent(&path);
                recent_files_sig.set(session::load_recent());
            }
        }
    };

    // ── Brouillon Link (Agents / MCP server). Nothing here opens a socket or writes a file unless
    // the user enabled it (`[mcp] enabled`, `--mcp`, or the Connect tab). Requests from the link
    // threads are queued to this thread and answered by `UiState` (see `link_host`).
    let link_ui = brouillon::link_view::LinkUi::new();
    let link_trigger = floem::reactive::Trigger::new();
    let (link_bridge, link_pump) = brouillon_mcp::bridge::channel_pair(move || floem::ext_event::register_ext_trigger(link_trigger), Duration::from_secs(5));
    let link_rt = Rc::new(brouillon::link_host::LinkRuntime::new(
        Arc::new(link_bridge),
        brouillon_mcp::link::LinkDirs { dir: startup().link_dir.clone(), pipe_tag: std::process::id().to_string() },
        link_ui,
    ));
    // Late-bound: the dispatcher and the save routine are created further down.
    let link_hooks = Rc::new(brouillon::link_host::LinkHooks::default());
    {
        let ui_state = brouillon::link_host::UiState {
            hooks: link_hooks.clone(),
            next_note: std::cell::Cell::new(0),
            tabs: tabs_sig,
            buffer: buffer_sig,
            cursor: cursor_sig,
            selection: selection_sig,
            status: status_msg_sig,
            ui: link_ui,
            open: Rc::new({
                let open_path = open_path.clone();
                move |p, l, c| open_path(p, l, c)
            }),
            activate: Rc::new({
                let on_switch = on_switch.clone();
                move |i| (on_switch)(i)
            }),
        };
        create_effect(move |_| {
            link_trigger.track();
            link_pump.drain(&ui_state);
        });
    }
    // The notes agents left follow the active tab (and vanish when their time is up).
    create_effect(move |_| {
        tabs_sig.with(|_| {});
        brouillon::link_host::refresh_notes(link_ui, tabs_sig);
    });
    let link_start = {
        let rt = link_rt.clone();
        Rc::new(move |s: &brouillon_mcp::settings::McpSettings| {
            if s.enabled {
                let roots = tabs_sig.with_untracked(brouillon::link_host::folders_of);
                if let Err(e) = rt.start(s, roots) {
                    status_msg_sig.set(format!("Agents could not start: {e}"));
                    eprintln!("[link] could not start: {e}");
                }
            } else {
                rt.stop();
            }
        })
    };
    link_start(&startup().mcp);
    // `[mcp] auto_off_minutes`: with no client connected for that long, the server switches itself
    // off (this session only; the setting is untouched). Checked every 20 s; nothing runs when unset.
    if let Some(minutes) = startup().mcp.auto_off_minutes.filter(|m| *m > 0) {
        fn tick(rt: Rc<brouillon::link_host::LinkRuntime>, status: RwSignal<String>, minutes: u32) {
            floem::action::exec_after(Duration::from_secs(20), move |_| {
                if let Some((connected, idle)) = rt.activity() {
                    if brouillon_mcp::limits::auto_off_due(Some(minutes), connected, idle) {
                        rt.stop();
                        status.set(format!("Agents switched off after {minutes} minutes without a client (Preferences > Connect turns them back on)."));
                    }
                }
                tick(rt, status, minutes);
            });
        }
        tick(link_rt.clone(), status_msg_sig, minutes);
    }
    let link_actions = brouillon::link_view::LinkActions {
        // Stop now AND switch `mcp.enabled` off: a panic stop must not come back at the next launch.
        kill: Rc::new({
            let rt = link_rt.clone();
            let creds = remote_creds.clone();
            move || {
                rt.stop();
                creds.lock();
                status_msg_sig.set(match brouillon::link_host::persist_disabled(&startup().paths.settings_file()) {
                    Ok(()) => "Agents stopped and switched off. Turn them on again in Preferences > Connect.".to_string(),
                    Err(e) => format!("Agents stopped, but the off setting could not be saved: {e}"),
                });
            }
        }),
        kill_session: Rc::new({
            let rt = link_rt.clone();
            let creds = remote_creds.clone();
            move || {
                rt.stop();
                creds.lock();
                status_msg_sig.set("Agents stopped for this session only. They start again at the next launch.".into());
            }
        }),
        consent: Rc::new({
            let rt = link_rt.clone();
            move |req, answer| {
                let folders = tabs_sig.with_untracked(brouillon::link_host::folders_of);
                rt.answer_consent(&req, answer, &folders, startup().mcp.starting_scope());
            }
        }),
        resolve: Rc::new({
            let rt = link_rt.clone();
            let scope = Scope::current();
            move |id, accepted| {
                let done = create_ext_action(scope, move |m: String| status_msg_sig.set(m));
                rt.resolve(&id, accepted, done);
            }
        }),
    };
    let link_kill_key = link_actions.kill.clone();
    let link_actions_key = link_actions.clone();
    let link_rt_close = link_rt.clone();
    let link_connect = brouillon::link_view::ConnectHost {
        ui: link_ui,
        snippet: Rc::new(|| {
            let name = if cfg!(windows) { "brouillon-mcp.exe" } else { "brouillon-mcp" };
            let relay = std::env::current_exe().map(|e| e.with_file_name(name)).unwrap_or_else(|_| PathBuf::from(name));
            let dir = &startup().link_dir;
            let custom = (*dir != brouillon_mcp::link::default_link_dir()).then_some(dir.as_path());
            brouillon::link_view::claude_snippet(&relay, custom)
        }),
        copy: Rc::new(|t| {
            if let Ok(mut cb) = arboard::Clipboard::new() {
                let _ = cb.set_text(t);
            }
        }),
        test: Rc::new({
            let rt = link_rt.clone();
            let scope = Scope::current();
            move || {
                let done = create_ext_action(scope, move |m: String| link_ui.test_result.set(m));
                rt.test(done);
            }
        }),
        start_connect,
    };

    // Re-read what an edit of settings.toml (Preferences, first-run answers, an import) changes live.
    let reload_settings: Rc<dyn Fn()> = {
        let rebind = rebind.clone();
        Rc::new(move || {
            let text = std::fs::read_to_string(startup().paths.settings_file()).unwrap_or_default();
            let settings = brouillon::onboarding::settings_of(&text);
            prefs_sig.set(brouillon_core::editor_prefs::EditorPrefs::from_settings(&settings));
            line_numbers_sig.set(settings.get_bool("editor.line_numbers", true));
            editor_opts.apply_settings(&settings);
            brouillon::theme_mode::reload();
            rebind();
            recent_files_sig.set(session::load_recent());
        })
    };
    // EditPlus import (dialog and first-run screen 2): dry run = `plan`, Apply = `plan` + `apply`.
    let import_host = brouillon::onboarding_view::ImportHost {
        detect: Rc::new(|| brouillon_import::editplus_full::detect_dirs().0),
        preview: Rc::new({
            let registry = registry.clone();
            move |dir: &std::path::Path| {
                let dest = startup().paths.root().to_path_buf();
                brouillon::import_cli::plan(dir, brouillon::import_cli::auto_defaults(dir).as_deref(), &dest, &|id| registry.contains(id))
                    .map(|p| brouillon::import_cli::to_preview(&p))
            }
        }),
        apply: Rc::new({
            let registry = registry.clone();
            let reload_settings = reload_settings.clone();
            move |dir: &std::path::Path| {
                let dest = startup().paths.root().to_path_buf();
                let plan = brouillon::import_cli::plan(dir, brouillon::import_cli::auto_defaults(dir).as_deref(), &dest, &|id| registry.contains(id))?;
                brouillon_import::editplus_full::apply(&plan, &dest).map_err(|e| format!("could not write under {}: {e}", dest.display()))?;
                reload_settings();
                // The import switched the Classic preset on: the first-run answer follows it.
                wizard.classic.set(true);
                Ok(brouillon::import_cli::apply_message(&plan))
            }
        }),
        pick_folder: Rc::new(|| rfd::FileDialog::new().set_title("Folder with your EditPlus settings").pick_folder()),
    };
    let wizard_host = brouillon::onboarding_view::WizardHost {
        import: import_host.clone(),
        preview_theme: Rc::new(|t: &str| {
            if let Some(m) = brouillon::theme::ThemeMode::parse(t) {
                brouillon::theme_mode::set_mode(m);
            }
        }),
        finish: Rc::new({
            let reload_settings = reload_settings.clone();
            move |ch: &brouillon::onboarding_view::FirstRunChoices| {
                let file = startup().paths.settings_file();
                let text = std::fs::read_to_string(&file).unwrap_or_default();
                if let Err(e) = brouillon::durable::durable_write(&file, brouillon::onboarding::finish_text(&text, ch).as_bytes()) {
                    eprintln!("[firstrun] cannot save {}: {e}", file.display());
                }
                reload_settings();
            }
        }),
        open_import: Rc::new(move || dialogs.import.set(true)),
        try_agent: Rc::new(move || {
            start_connect.set(true);
            dialogs.settings.set(true);
        }),
    };
    let wizard_host_keys = wizard_host.clone();

    // Help > Welcome / Documentation / Release notes: open (or activate) the built-in document.
    let open_help = {
        let sync = sync_active_to_mirrors;
        move |doc: brouillon::startup_doc::HelpDoc| {
            tabs_sig.update(|tm| {
                brouillon::startup_doc::open_doc(tm, doc, &startup().help_env);
            });
            sync();
            language_sig.set(brouillon::grammars::LanguageId::Markdown);
            selection_sig.set(Selection::new((1, 1), (1, 1), false));
        }
    };

    // External-change guard: the bar under the tabs + a 1 s stat of the active file.
    let ext_ctx = brouillon::external_ui::Ctx {
        tabs: tabs_sig,
        mirrors,
        selection: selection_sig,
        fold: fold_state_sig,
        status: status_msg_sig,
        theme: theme_idx,
        banner: create_rw_signal(None),
    };
    brouillon::external_ui::start_polling(ext_ctx, startup().auto_reload_unmodified);

    // Optional modules: only a path and a function pointer are stored here; the modules folder is first read
    // when the active tab is a file kind that has a module.
    brouillon::module_ui::configure(startup().paths.modules_dir());
    // A routed document (PDF, Word...) shows its real bytes in the hex view; leaving it restores the previous mode.
    {
        let routed = create_memo(move |_| tabs_sig.with(|tm| tm.current().kind.routed_to_viewer()));
        let auto_hex = Rc::new(std::cell::Cell::new(false));
        create_effect(move |_| {
            if routed.get() {
                if !hex_mode_sig.get_untracked() {
                    hex_mode_sig.set(true);
                    auto_hex.set(true);
                }
            } else if auto_hex.replace(false) {
                hex_mode_sig.set(false);
            }
        });
    }
    // "Always open as text" from the offer bar: re-read the active file as plain text.
    let module_as_text: Rc<dyn Fn()> = Rc::new(move || {
        tabs_sig.update(|tm| {
            let t = tm.current_mut();
            t.kind = brouillon::kind::FileKind::Text;
            t.meta.readonly = false;
        });
        brouillon::external_ui::reload_active(ext_ctx, false);
    });

    // Save the active tab to `path` (codepage / BOM / backup / lossy prompt).
    let save_active_to = move |p: PathBuf| -> bool {
        let text = buffer_sig.get_untracked();
        let meta = tabs_sig.with_untracked(|tm| tm.current().meta.clone());
        match save_text(&text, &p, &meta) {
            Ok(()) => {
                let mut nb = (*buffer_sig.get_untracked()).clone();
                if nb.is_lazy() {
                    let rebase = brouillon::wiring::lazy_rebase_path(&nb, &meta, &p).unwrap_or_else(|_| p.clone());
                    if let Err(e) = nb.lazy_rebase_after_external_save(&rebase) {
                        eprintln!("[file] saved, but the large document could not be re-based on the new file: {e}");
                    }
                }
                nb.mark_saved();
                buffer_sig.set(Arc::new(nb));
                path_sig.set(Some(p.clone()));
                // Drawn strokes are saved next to the text (`<file>.sketch.json`).
                let doc = sketch_doc_sig.get_untracked();
                let strokes = doc.stroke_count();
                let had_saved = tabs_sig.with_untracked(|tm| tm.current().sketch_saved) > 0;
                let sketch_ok = if strokes > 0 || had_saved {
                    match doc.save_sidecar(&p) {
                        Ok(()) => true,
                        Err(e) => {
                            eprintln!("[file] drawing could not be saved: {e}");
                            false
                        }
                    }
                } else {
                    true
                };
                tabs_sig.update(|tm| {
                    let t = tm.current_mut();
                    if sketch_ok {
                        t.sketch_saved = strokes;
                    }
                    t.dirty = t.sketch_unsaved();
                    t.meta.external_state = tabs::ExternalState::InSync;
                });
                status_msg_sig.set(if sketch_ok {
                    format!("Saved {}", p.display())
                } else {
                    format!("Saved {} but its drawing could not be saved", p.display())
                });
                eprintln!("[file] saved {}", p.display());
                true
            }
            Err(e) if e == brouillon::external_ui::CONFLICT => {
                brouillon::external_ui::show_save_conflict(ext_ctx, &p);
                eprintln!("[file] save refused: {} changed on disk", p.display());
                false
            }
            Err(e) => {
                status_msg_sig.set(format!("Save failed: {e}"));
                eprintln!("[file] save error: {e}");
                false
            }
        }
    };

    // Apply a lot command's outcome to the UI: edits as ONE undo group,
    // caret/selection, message, panel requests.
    let open_panel_named = {
        let fr = find_replace_state.clone();
        move |name: &str| match name {
            "find" => fr.visible.set(true),
            "output" => {
                output_visible_sig.set(true);
                active_panel_sig.set("output");
            }
            "search_results" => {
                results_visible_sig.set(true);
                active_panel_sig.set("results");
            }
            "encoding_picker" => {
                encoding_query.set(String::new());
                encoding_picker_visible.set(true);
            }
            other if dialogs.open_panel(other) => {}
            other => {
                status_msg_sig.set(format!("Panel `{other}` has no view yet"));
                eprintln!("[panel] `{other}` requested but not implemented (see 42-integration-report.md)");
            }
        }
    };
    // Keep bookmarks anchored through an edit (cheap no-op without bookmarks).
    let shift_markers = move |old: &Buffer, edits: &brouillon::contract::EditSet| {
        if edits.is_empty() || tabs_sig.with_untracked(|tm| tm.current().meta.markers.is_empty()) {
            return;
        }
        if old.is_lazy() {
            return; // bookmarks follow edits by line through a whole-text scan: off in large file mode
        }
        let old_text = old.text();
        tabs_sig.update(|tm| {
            let m = &mut tm.current_mut().meta.markers;
            let mut markers = brouillon::markers::Markers::from_lines(m.iter().copied());
            markers.apply_edits(&old_text, edits);
            *m = markers.to_vec();
        });
    };
    let apply_ui_outcome = move |out: brouillon::contract::CmdOutcome, sel_before: Vec<brouillon::contract::Region>| {
        let buf = buffer_sig.get_untracked();
        let had_edits = !out.edits.is_empty();
        match brouillon::wiring::apply_outcome(&buf, &out, &sel_before) {
            Ok(nb) => {
                if had_edits {
                    shift_markers(&buf, &out.edits);
                    buffer_sig.set(Arc::new(nb));
                }
            }
            Err(e) => {
                status_msg_sig.set(format!("Edit rejected: {e}"));
                return;
            }
        }
        match out.selection.as_ref().filter(|r| !r.is_empty()) {
            Some(regions) => {
                let (anchor, head, extras) = brouillon::wiring::split_regions_buf(&buffer_sig.get_untracked(), regions);
                cursor_sig.set(head);
                selection_sig.set(Selection::new(anchor, head, false));
                extra_carets_sig.set(extras);
            }
            None if had_edits => {
                let last = buffer_sig.get_untracked().line_count().max(1);
                cursor_sig.update(|c| c.0 = c.0.min(last));
                selection_sig.set(Selection::new((1, 1), (1, 1), false));
                extra_carets_sig.set(Vec::new());
            }
            None => {}
        }
        if let Some(m) = out.message {
            status_msg_sig.set(m);
        }
    };

    // Delete the active selection (one undo group). Returns true if there was one.
    let delete_selection = move || -> bool {
        let sel = selection_sig.get_untracked();
        if sel.is_empty() {
            return false;
        }
        let regions = brouillon::wiring::regions_from_buf(&buffer_sig.get_untracked(), cursor_sig.get_untracked(), Some((sel.anchor, sel.head)));
        let r = regions[0];
        let out = brouillon::contract::CmdOutcome {
            edits: brouillon::contract::EditSet(vec![brouillon::contract::TextEdit::delete(r.start(), r.end())]),
            selection: Some(vec![brouillon::contract::Region::caret(r.start())]),
            ..Default::default()
        };
        apply_ui_outcome(out, regions);
        true
    };

    // Run a registered lot command against the active document.
    let run_lot_command = {
        let fr = find_replace_state.clone();
        let apply_ui_outcome = apply_ui_outcome;
        let open_panel_named = open_panel_named.clone();
        move |id: &str| {
            let buf = buffer_sig.get_untracked();
            if buf.is_lazy() {
                // The registry commands take the whole text: not on a large-file-mode document.
                status_msg_sig.set(format!("`{id}` is not available in large file mode"));
                return;
            }
            let text = buf.text();
            let sel = selection_sig.get_untracked();
            let regions = brouillon::wiring::regions_with_extras(&text, cursor_sig.get_untracked(), Some((sel.anchor, sel.head)), &extra_carets_sig.get_untracked());
            let mut args: Vec<String> = Vec::new();
            match id {
                "search_find_next" | "search_find_prev" | "search_count" | "search_replace_all" => {
                    let mut flags = String::new();
                    if fr.case_sensitive.get_untracked() {
                        flags.push('c');
                    }
                    if fr.regex_mode.get_untracked() {
                        flags.push('r');
                    }
                    args = vec![fr.query.get_untracked(), flags, fr.replacement.get_untracked()];
                }
                "edit_paste_column" => {
                    args = vec![arboard::Clipboard::new().and_then(|mut cb| cb.get_text()).unwrap_or_default()];
                }
                "bookmark_next" | "bookmark_prev" => {
                    args = tabs_sig.with_untracked(|tm| tm.current().meta.markers.iter().map(|l| l.to_string()).collect());
                }
                // Ctrl+Alt+1..0: the library chosen in the Cliptext panel (the default one otherwise).
                _ if id.starts_with("cliptext_insert_") => {
                    args = vec![brouillon::cliptext_store::active_library(&startup().paths.cliptext_dir()).to_string_lossy().into_owned()];
                }
                _ => {}
            }
            let path = path_sig.get_untracked();
            let ctx = brouillon::contract::CmdCtx { text: &text, selection: &regions, path: path.as_deref(), args: &args };
            match brouillon::wiring::run(id, &ctx) {
                Some(mut out) => {
                    let panel = out.open_panel.take();
                    apply_ui_outcome(out, regions.clone());
                    if let Some(p) = panel {
                        open_panel_named(&p.0);
                    }
                }
                None => {
                    status_msg_sig.set(format!("Command `{id}` is not available"));
                    eprintln!("[command] `{id}` is registered but has no implementation yet");
                }
            }
        }
    };

    // Find in Files: the Find overlay's query over the active file's folder
    // (or the working directory), with unsaved buffers searched in memory.
    let run_find_in_files = {
        let fr = find_replace_state.clone();
        move || {
            let pattern = fr.query.get_untracked();
            if pattern.is_empty() {
                status_msg_sig.set("Find in Files: type a pattern in the Find box first".into());
                fr.visible.set(true);
                return;
            }
            let matcher = match brouillon::search_files::RegexMatcher::new(&pattern, fr.regex_mode.get_untracked(), fr.case_sensitive.get_untracked(), false) {
                Ok(m) => m,
                Err(e) => {
                    status_msg_sig.set(format!("Find in Files: {e}"));
                    return;
                }
            };
            let root = path_sig
                .get_untracked()
                .and_then(|p| p.parent().map(PathBuf::from))
                .filter(|p| !p.as_os_str().is_empty())
                .unwrap_or_else(|| std::env::current_dir().unwrap_or_default());
            let mut opts = brouillon::search_files::FindOptions::new(root.clone());
            tabs_sig.with_untracked(|tm| {
                for t in tm.iter().filter(|t| t.dirty && !t.buffer.is_lazy()) {
                    if let Some(p) = &t.path {
                        opts.buffers.insert(p.clone(), t.buffer.text());
                    }
                }
            });
            status_msg_sig.set(format!("Searching {} ...", root.display()));
            let title = format!("{pattern}  in {}", root.display());
            let apply = create_ext_action(Scope::current(), move |res: Result<(Vec<brouillon::search_files::FileResult>, brouillon::search_files::SearchStats), String>| {
                match res {
                    Ok((files, stats)) => {
                        results_model_sig.update(|m| {
                            m.begin_search(&title);
                            for f in &files {
                                m.add_file(f);
                            }
                        });
                        results_visible_sig.set(true);
                        active_panel_sig.set("results");
                        status_msg_sig.set(format!("{} hit(s) in {} file(s) ({} scanned)", stats.hits, stats.files_with_hits, stats.files_scanned));
                    }
                    Err(e) => status_msg_sig.set(format!("Find in Files: {e}")),
                }
            });
            std::thread::spawn(move || {
                let cancel = brouillon::search_files::CancelToken::new();
                apply(brouillon::search_files::search_files_collect(&opts, &matcher, &cancel));
            });
        }
    };

    // Launch the tool bound to Ctrl+`slot` (streaming output, Stop, F4 jumps).
    let run_tool_slot = {
        let tools_cfg = tools_cfg.clone();
        let tool_handle = tool_handle.clone();
        move |slot: u8| {
            if tool_handle.borrow().is_some() {
                status_msg_sig.set("A tool is already running (Tools: Stop Running Tool to cancel)".into());
                return;
            }
            let path = path_sig.get_untracked();
            let ext = path.as_ref().and_then(|p| p.extension()).map(|e| e.to_string_lossy().into_owned());
            let Some(tool) = tools_cfg.by_slot(1, slot, ext.as_deref()).cloned() else {
                status_msg_sig.set(format!("No tool bound to slot {slot}"));
                return;
            };
            let buf = buffer_sig.get_untracked();
            let sel = selection_sig.get_untracked();
            let regions = brouillon::wiring::regions_from_buf(&buf, cursor_sig.get_untracked(), Some((sel.anchor, sel.head)));
            let (line, col) = cursor_sig.get_untracked();
            let selected = regions.first().filter(|r| !r.is_caret()).map(|r| buf.slice(r.start()..r.end()));
            let win = brouillon::wiring::Window::around(&buf, regions[0].head);
            let word = brouillon::cursor::word_range(&win.text, regions[0].head - win.base, "").map(|(s, e)| win.text[s..e].to_string());
            let project = path.as_ref().and_then(|p| p.parent().map(PathBuf::from)).or_else(|| std::env::current_dir().ok());
            let vars = brouillon::tool_vars::VarContext {
                file: path.clone(),
                line: Some(line),
                col: Some(col),
                word,
                selection: selected,
                project: project.clone(),
                ..Default::default()
            };
            let mut save = |policy: brouillon::tools_config::SavePolicy| -> Result<(), String> {
                if policy == brouillon::tools_config::SavePolicy::None {
                    return Ok(());
                }
                match path_sig.get_untracked() {
                    Some(p) if buffer_sig.get_untracked().is_dirty() => {
                        if save_active_to(p) { Ok(()) } else { Err("could not save the current document".into()) }
                    }
                    _ => Ok(()),
                }
            };
            let cwd = project.unwrap_or_default();
            match brouillon::shell_runner::start_tool(&tool, &vars, brouillon::shell_runner::Requester::User, &mut save) {
                Ok(handle) => {
                    output_buf_sig.update(|b| {
                        *b = brouillon::error_patterns::OutputBuffer::new(cwd, brouillon::error_patterns::builtin_patterns());
                        b.push(format!("> {}", tool.name), brouillon::error_patterns::Stream::Info);
                    });
                    tool_footer_sig.set(String::new());
                    tool_running_sig.set(true);
                    output_visible_sig.set(true);
                    active_panel_sig.set("output");
                    *tool_handle.borrow_mut() = Some(handle);
                }
                Err(e) => {
                    status_msg_sig.set(format!("Tool `{}`: {e}", tool.name));
                    eprintln!("[tool] {}: {e}", tool.name);
                }
            }
        }
    };

    // Jump to a (path, 1-based line, 1-based col) target.
    let jump_to = {
        let open_path = open_path.clone();
        move |p: PathBuf, line: usize, col: usize| open_path(p, Some(line.max(1)), Some(col.max(1)))
    };

    // Ctrl+P quick open (BK-071): state shared by the dispatcher (opens it) and the overlay.
    let quick_open_state = brouillon::quick_open_view::QuickOpenState::new();

    // alpha.6 Round 6 : palette / menu / toolbar / shortcut dispatch. Ids are
    // the stable snake_case ones of the registry; legacy dotted ids are
    // translated by `wiring::canonical`. The palette closes after every action.
    // Emmet: expand the abbreviation before the caret (HTML / CSS files). False = nothing happened.
    let emmet_expand_at_caret = move || -> bool {
        let path = path_sig.get_untracked();
        let ext = path.as_deref().and_then(|p| p.extension()).and_then(|e| e.to_str()).unwrap_or("").to_string();
        if brouillon::emmet::Lang::from_ext(&ext).is_none() || !selection_sig.get_untracked().is_empty() || !extra_carets_sig.get_untracked().is_empty() {
            return false;
        }
        let buf = buffer_sig.get_untracked();
        if buf.is_lazy() || buf.byte_len() > 4 * 1024 * 1024 {
            return false;
        }
        let text = buf.text();
        let regions = brouillon::wiring::regions_from(&text, cursor_sig.get_untracked(), None);
        let Some((edit, caret)) = brouillon::wiring::emmet_edit(&text, regions[0].head, &ext) else { return false };
        let out = brouillon::contract::CmdOutcome {
            edits: brouillon::contract::EditSet(vec![edit]),
            selection: Some(vec![brouillon::contract::Region::caret(caret)]),
            ..Default::default()
        };
        apply_ui_outcome(out, regions);
        true
    };

    // Macros: one recorder, one saved macro (macro.toml in the data folder).
    let macro_rec: Rc<RefCell<brouillon::macros::Recorder>> = Rc::new(RefCell::new(Default::default()));
    let macro_slot: Rc<RefCell<brouillon::macros::Macro>> = Rc::new(RefCell::new(brouillon::macros::Macro::load(startup().paths.root()).unwrap_or_default()));
    let dispatch_cell: Rc<RefCell<Option<Arc<dyn Fn(&str)>>>> = Rc::new(RefCell::new(None));
    let play_macro = {
        let slot = macro_slot.clone();
        let dispatch_cell = dispatch_cell.clone();
        move || {
            let m = slot.borrow().clone();
            if m.is_empty() {
                status_msg_sig.set("No macro recorded yet (Macro: Record / Stop)".into());
                return;
            }
            if buffer_sig.get_untracked().is_lazy() {
                status_msg_sig.set("Macros are not available in large file mode".into());
                return;
            }
            let state = || {
                let text = buffer_sig.get_untracked().text();
                let sel = selection_sig.get_untracked();
                let regions = brouillon::wiring::regions_with_extras(&text, cursor_sig.get_untracked(), Some((sel.anchor, sel.head)), &extra_carets_sig.get_untracked());
                (text, regions)
            };
            for step in &m.steps {
                if let Some(id) = &step.cmd {
                    let d = dispatch_cell.borrow().clone();
                    if let Some(d) = d {
                        d(id);
                    }
                } else if let Some(s) = &step.text {
                    let (_, regions) = state();
                    let (edits, after) = brouillon::cursor::insert_text(&regions, s);
                    apply_ui_outcome(brouillon::contract::CmdOutcome { edits, selection: Some(after), ..Default::default() }, regions);
                } else if let Some(k) = &step.key {
                    let (text, regions) = state();
                    let motion = match k.as_str() {
                        "left" => Some(brouillon::cursor::Motion::Left),
                        "right" => Some(brouillon::cursor::Motion::Right),
                        "up" => Some(brouillon::cursor::Motion::Up),
                        "down" => Some(brouillon::cursor::Motion::Down),
                        "home" => Some(brouillon::cursor::Motion::Home),
                        "end" => Some(brouillon::cursor::Motion::End),
                        _ => None,
                    };
                    if let Some(mo) = motion {
                        let moved = brouillon::cursor::move_regions(&text, &regions, mo, false);
                        let (a, h, ex) = brouillon::wiring::split_regions(&text, &moved);
                        cursor_sig.set(h);
                        selection_sig.set(Selection::new(a, h, false));
                        extra_carets_sig.set(ex);
                    } else if k == "enter" {
                        let path = path_sig.get_untracked();
                        let ctx = brouillon::contract::CmdCtx { text: &text, selection: &regions, path: path.as_deref(), args: &[] };
                        if let Some(out) = brouillon::wiring::run("newline_auto_indent", &ctx) {
                            apply_ui_outcome(out, regions);
                        }
                    } else if k == "backspace" || k == "delete" {
                        let (edits, after) = brouillon::cursor::delete_chars(&text, &regions, k == "delete");
                        if !edits.is_empty() {
                            apply_ui_outcome(brouillon::contract::CmdOutcome { edits, selection: Some(after), ..Default::default() }, regions);
                        }
                    }
                }
            }
            status_msg_sig.set(format!("Macro played ({} steps)", m.steps.len()));
        }
    };

    // Last visited tab (Tab: Last Visited Tab) and the comparison view.
    let tab_mru: Rc<std::cell::RefCell<brouillon::tab_mru::TabMru>> = Rc::new(std::cell::RefCell::new(Default::default()));
    {
        let tab_mru = tab_mru.clone();
        create_effect(move |_| {
            let key = tabs_sig.with(|tm| {
                let t = tm.current();
                brouillon::tab_mru::tab_key(t.path.as_deref(), &t.title)
            });
            tab_mru.borrow_mut().visit(&key);
        });
    }
    let diff_ui = brouillon::diff_view::DiffUi::new();
    // Hex mode over the real bytes of the active file (editable, with inspector and regions). An unsaved
    // or modified text buffer keeps the read-only text view. Byte edits never touch the text buffer.
    let hex_ui = brouillon::hex_editor::HexUi::new(Scope::current(), Some(startup().paths.hex_regions_dir()));
    // Quit / window close with unsaved hex byte edits: ask (Save / Don't save / Cancel). Text tabs are journaled
    // instead, so only hex sessions can lose data. `false` = keep the app open.
    let try_quit: Rc<dyn Fn() -> bool> = Rc::new({
        let hex_ui = hex_ui.clone();
        move || {
            let n = hex_ui.dirty_count();
            if n == 0 {
                return true;
            }
            match brouillon::confirm::confirm_quit_dirty(n) {
                brouillon::confirm::ConfirmResult::Save => match hex_ui.save_all_dirty() {
                    Ok(_) => true,
                    Err(e) => {
                        eprintln!("[hex] save before quit failed: {e}");
                        hex_ui.message.set(format!("Save failed: {e}"));
                        false
                    }
                },
                brouillon::confirm::ConfirmResult::Discard => true,
                brouillon::confirm::ConfirmResult::Cancel => false,
            }
        }
    });
    let try_quit_keys = try_quit.clone();
    floem::set_close_guard({
        let try_quit = try_quit.clone();
        move || try_quit()
    });
    let hex_sync: Rc<dyn Fn()> = Rc::new({
        let hex_ui = hex_ui.clone();
        move || {
            let want = hex_mode_sig.get_untracked();
            let path = path_sig.get_untracked();
            let text_dirty = tabs_sig.with_untracked(|tm| tm.current().dirty);
            match (want, path) {
                (true, Some(p)) if !text_dirty && p.is_file() => {
                    if hex_ui.active_path_now().as_deref() != Some(p.as_path()) {
                        if let Err(e) = hex_ui.activate(&p) {
                            eprintln!("[hex] cannot open {} as bytes: {e}", p.display());
                            hex_ui.deactivate();
                        }
                    }
                }
                _ => {
                    if hex_ui.is_active() {
                        hex_ui.deactivate();
                    }
                }
            }
        }
    });
    {
        let hex_sync = hex_sync.clone();
        create_effect(move |_| {
            let _ = hex_mode_sig.get();
            let _ = path_sig.get();
            hex_sync();
        });
    }
    let dispatch_scope = Scope::current();
    let sftp_state_for_dispatch = sftp_state.clone();
    let dispatch: Arc<dyn Fn(&str) + 'static> = Arc::new({
        let tool_handle = tool_handle.clone();
        let tools_cfg = tools_cfg.clone();
        let jump_to = jump_to.clone();
        let open_path = open_path.clone();
        let run_lot_command = run_lot_command.clone();
        let run_tool_slot = run_tool_slot.clone();
        let run_find_in_files = run_find_in_files.clone();
        let on_new_tab = on_new_tab.clone();
        let on_close = on_close.clone();
        let on_switch = on_switch.clone();
        let link_kill = link_actions.kill.clone();
        let macro_rec = macro_rec.clone();
        let macro_slot = macro_slot.clone();
        let play_macro = play_macro.clone();
        let remote_ui = remote_ui.clone();
        let hex_ui = hex_ui.clone();
        let hex_sync = hex_sync.clone();
        move |raw_id: &str| {
        let id_owned = brouillon::wiring::canonical(raw_id);
        let id = id_owned.as_str();
        macro_rec.borrow_mut().record_command(id);
        // Hex commands switch the hex view on when needed; while a hex session is active the shared
        // commands (save, undo, redo, copy, select all) act on the bytes, never on the hidden text.
        if id.starts_with("hex_") && !hex_ui.is_active() {
            hex_mode_sig.set(true);
            hex_sync();
            if !hex_ui.is_active() {
                eprintln!("[hex] {id}: this document has no file on disk or has unsaved text edits");
                palette_for_dispatch.close();
                return;
            }
        }
        if hex_ui.command(id) {
            // the early return skips the common tail below: close the palette here (it stayed open after every Hex command)
            palette_for_dispatch.close();
            return;
        }
        match id {
            "macro_record_toggle" => {
                let stopped = {
                    let mut r = macro_rec.borrow_mut();
                    if r.is_recording() {
                        Some(r.stop())
                    } else {
                        r.start();
                        None
                    }
                };
                match stopped {
                    Some(m) => {
                        let n = m.steps.len();
                        if let Err(e) = m.save(startup().paths.root()) {
                            eprintln!("[macro] cannot save macro.toml: {e}");
                        }
                        *macro_slot.borrow_mut() = m;
                        status_msg_sig.set(format!("Macro recorded: {n} step(s). Macro: Play replays it"));
                    }
                    None => status_msg_sig.set("Recording macro... run Macro: Record / Stop again to finish".into()),
                }
            }
            "macro_play" => {
                if macro_rec.borrow().is_recording() {
                    status_msg_sig.set("Stop recording first (Macro: Record / Stop)".into());
                } else {
                    play_macro();
                }
            }
            "emmet_expand" => {
                if !emmet_expand_at_caret() {
                    status_msg_sig.set("Emmet: no abbreviation to expand here (HTML / CSS files)".into());
                }
            }
            "markers_list" => {
                use brouillon::search_files::{FileResult, Hit};
                let live = buffer_sig.get_untracked();
                let groups: Vec<FileResult> = tabs_sig.with_untracked(|tm| {
                    let active = tm.active_idx();
                    tm.iter()
                        .enumerate()
                        .filter_map(|(i, t)| {
                            let path = t.path.clone()?;
                            if t.meta.markers.is_empty() {
                                return None;
                            }
                            let buf = if i == active { &live } else { &t.buffer };
                            let hits = t
                                .meta
                                .markers
                                .iter()
                                .filter(|&&l| l < buf.line_count())
                                .map(|&l| Hit { line: l, col: 0, start_byte: 0, end_byte: 0, line_text: buf.nth_line(l).unwrap_or_default().trim_end().to_string(), matched: String::new() })
                                .collect::<Vec<_>>();
                            Some(FileResult { path, hits, from_buffer: t.dirty })
                        })
                        .collect()
                });
                let total: usize = groups.iter().map(|g| g.hits.len()).sum();
                if total == 0 {
                    status_msg_sig.set("No bookmarks in the open files (Bookmarks: Toggle adds one)".into());
                } else {
                    results_model_sig.update(|m| {
                        m.begin_search("Bookmarks");
                        for g in &groups {
                            m.add_file(g);
                        }
                    });
                    results_visible_sig.set(true);
                    active_panel_sig.set("results");
                    status_msg_sig.set(format!("{total} bookmark(s)"));
                }
            }
            "link_kill" => link_kill(),
            // Built-in help documents are read-only: refuse commands that would edit them.
            _ if tabs_sig.with_untracked(|tm| tm.current().help_doc.is_some()) && brouillon::startup_doc::command_blocked_on_help(id) => {
                status_msg_sig.set("This help page is read-only. File > Save As keeps an editable copy.".into());
            }
            "help_welcome" | "help_docs" | "help_release_notes" => {
                if let Some(doc) = brouillon::startup_doc::HelpDoc::from_command_id(id) {
                    open_help(doc);
                }
            }
            "view_theme_cycle" => {
                brouillon::theme_mode::cycle();
            }
            "theme_dark" => brouillon::theme_mode::set_mode(brouillon::theme::ThemeMode::Dark),
            "theme_light" => brouillon::theme_mode::set_mode(brouillon::theme::ThemeMode::Light),
            "theme_system" => brouillon::theme_mode::set_mode(brouillon::theme::ThemeMode::System),
            "view_toggle_mode" => {
                preset_sig.update(|p| *p = p.flipped());
                eprintln!("[palette] preset -> {}", preset_sig.get_untracked().name());
            }
            "view_toggle_crayon" => {
                crayon_mode_sig.update(|c| *c = !*c);
                eprintln!("[palette] crayon mode -> {}", crayon_mode_sig.get_untracked());
            }
            "view_toggle_preview" => {
                markdown_preview_sig.update(|m| *m = !*m);
                eprintln!("[palette] markdown preview -> {}", markdown_preview_sig.get_untracked());
            }
            "view_toggle_hex" => hex_mode_sig.update(|h| *h = !*h),
            "toggle_overwrite" => overwrite_sig.update(|o| *o = !*o),
            "view_toggle_ai" => {
                ai_visible_sig.update(|v| *v = !*v);
                eprintln!("[palette] AI panel -> {}", ai_visible_sig.get_untracked());
            }
            "project_open_folder" | "project_load" => {
                if let Some(dir) = rfd::FileDialog::new().set_title("Open Folder").pick_folder() {
                    remote_ui.browse_local(&dir);
                    status_msg_sig.set(format!("Folder: {}", dir.display()));
                }
            }
            "view_toggle_tree" | "browser_toggle" => remote_ui.toggle_browser(),
            "browser_switch_source" => {
                remote_ui.set_browser_visible(true);
                remote_ui.show_source_menu();
            }
            "browser_focus" => {
                remote_ui.set_browser_visible(true);
                remote_ui.kbd_event(brouillon::kbd_focus::Ev::FocusRows);
            }
            "browser_go_up" => {
                remote_ui.set_browser_visible(true);
                remote_ui.go_up();
            }
            "browser_reveal" => {
                let (path, origin) = tabs_sig.with_untracked(|tm| {
                    let t = tm.current();
                    (t.path.clone(), t.remote_origin.clone())
                });
                match (origin, path) {
                    (Some(o), _) => {
                        if !remote_ui.reveal_origin(&o) {
                            status_msg_sig.set("That site is not open. Reconnect it from the Browser source menu.".into());
                        }
                    }
                    (None, Some(p)) => remote_ui.reveal_local(&p),
                    (None, None) => status_msg_sig.set("This tab is not a file yet: nothing to reveal.".into()),
                }
            }
            "remote_connect" => dialogs.remote_connect.set(true),
            "remote_sites" => dialogs.remote_sites.set(true),
            "remote_demo" => remote_ui.connect_demo(),
            "remote_refresh" => remote_ui.refresh(),
            "remote_disconnect" => {
                if let Some(id) = remote_ui.active.get_untracked() {
                    remote_ui.close_site(&id);
                }
            }
            "remote_recover" => remote_ui.open_recover(),
            "credentials_show" => remote_ui.open_credentials(),
            "credentials_panic" => {
                remote_ui.lock_credentials();
                status_msg_sig.set("Saved passwords locked and everything typed but not yet used forgotten. They open again when needed.".into());
            }
            "remote_save_again" => {
                // Same path as Ctrl+S: re-saves the CURRENT text (the saver re-checks the server first).
                let origin = tabs_sig.with_untracked(|tm| tm.current().remote_origin.clone());
                match origin {
                    Some(o) => remote_ui.save_doc(&o, buffer_sig.get_untracked().text().into_bytes()),
                    None => status_msg_sig.set("This document is not on a server.".into()),
                }
            }
            "remote_status" => {
                let note = tabs_sig
                    .with_untracked(|tm| tm.current().remote_origin.clone())
                    .and_then(|o| remote_ui.doc_state(&o))
                    .map(|st| st.chip().label + &st.note.map(|n| format!(" - {n}")).unwrap_or_default());
                status_msg_sig.set(note.unwrap_or_else(|| "This document is not on a server.".into()));
            }
            "view_toggle_sftp" => {
                sftp_visible_sig.update(|v| *v = !*v);
                eprintln!("[palette] SFTP panel -> {}", sftp_visible_sig.get_untracked());
            }
            "preset_moldu" => preset_sig.set(Preset::Moldu),
            "preset_geek" => preset_sig.set(Preset::Geek),
            "tools_infinite" => {
                infinite_layout_sig.update(|inf| *inf = !*inf);
                eprintln!("[palette] infinite layout -> {}", infinite_layout_sig.get_untracked());
            }
            "search_open_find" | "search_open_replace" => fr_for_dispatch.visible.set(true),
            "view_toggle_line_numbers" => {
                let on = !line_numbers_sig.get_untracked();
                line_numbers_sig.set(on);
                let file = startup().paths.settings_file();
                let text = std::fs::read_to_string(&file).unwrap_or_default();
                let text = brouillon::settings::set_raw_key(&text, "editor", "line_numbers", if on { "true" } else { "false" });
                if let Err(e) = std::fs::write(&file, text) {
                    eprintln!("[settings] cannot save editor.line_numbers: {e}");
                }
                status_msg_sig.set(format!("Line numbers {}", if on { "on" } else { "off" }));
            }
            "view_toggle_indent_guides" | "view_toggle_ruler" | "view_toggle_minimap" => {
                let (key, raw, msg) = match id {
                    "view_toggle_indent_guides" => {
                        let on = !editor_opts.indent_guides.get_untracked();
                        editor_opts.indent_guides.set(on);
                        ("indent_guides", on.to_string(), format!("Indent guides {}", if on { "on" } else { "off" }))
                    }
                    "view_toggle_minimap" => {
                        let on = !editor_opts.minimap.get_untracked();
                        editor_opts.minimap.set(on);
                        ("minimap", on.to_string(), format!("Minimap {}", if on { "on" } else { "off" }))
                    }
                    _ => {
                        // Off -> the column set in the settings file (80 when none), on -> off.
                        let cur = editor_opts.ruler_column.get_untracked();
                        let next = if cur > 0 { 0 } else { ruler_default_sig.get_untracked().max(1) };
                        editor_opts.ruler_column.set(next);
                        ("ruler_column", next.to_string(), if next > 0 { format!("Column ruler at {next}") } else { "Column ruler off".to_string() })
                    }
                };
                let file = startup().paths.settings_file();
                let text = std::fs::read_to_string(&file).unwrap_or_default();
                if let Err(e) = std::fs::write(&file, brouillon::settings::set_raw_key(&text, "editor", key, &raw)) {
                    eprintln!("[settings] cannot save editor.{key}: {e}");
                }
                status_msg_sig.set(msg);
            }
            "view_distraction_free" => {
                let on = !distraction_sig.get_untracked();
                distraction_sig.set(on);
                status_msg_sig.set(if on { "Distraction-free mode (Ctrl+Alt+Z to leave)".to_string() } else { String::new() });
            }
            "quick_open" | "goto_function" => {
                if id == "quick_open" && quick_open_state.visible.get_untracked() {
                    quick_open_state.close();
                } else {
                    let root = path_sig
                        .get_untracked()
                        .and_then(|p| p.parent().map(PathBuf::from))
                        .filter(|p| !p.as_os_str().is_empty())
                        .unwrap_or_else(|| std::env::current_dir().unwrap_or_default());
                    let buf = buffer_sig.get_untracked();
                    // The outline walks the whole document: not in large file mode.
                    let symbols = if buf.is_lazy() { Vec::new() } else { brouillon::quick_open_view::extract_symbols_rope(buf.rope(), path_sig.get_untracked().as_deref()) };
                    let files = if id == "goto_function" { Vec::new() } else { brouillon::quick_open_view::list_files(&root) };
                    let data = brouillon::quick_open::QuickOpen::new(recent_files_sig.get_untracked(), files, symbols);
                    quick_open_state.open(data, root, buf.line_count().max(1));
                    if id == "goto_function" {
                        // Go to Function = the quick-open picker pre-filled with the `@` outline prefix.
                        quick_open_state.query.set("@".to_string());
                    }
                }
            }
            "go_to" => {
                goto_input.set(String::new());
                goto_visible.set(true);
            }
            "app_quit" => {
                if try_quit() {
                    tabs_sig.with_untracked(persist_unsaved_on_exit);
                    std::process::exit(0)
                }
            }
            "help_about" => dialogs.about.set(true),
            "file_import_editplus" => dialogs.import.set(true),
            "help_first_run" => {
                wizard.screen.set(0);
                dialogs.firstrun.set(true);
            }
            "help_try_agent" => {
                start_connect.set(true);
                dialogs.settings.set(true);
            }
            "view_command_palette" => palette_for_dispatch.toggle(),
            // The palette lists every command with its effective chord (the
            // dispatcher closes it after each action, so only point to it).
            "help_shortcuts" => status_msg_sig.set("Shortcuts: Ctrl+K opens the command palette, which lists every command with its chord".into()),
            "file_open" => {
                let open_path = open_path.clone();
                let apply = create_ext_action(dispatch_scope, move |p: PathBuf| open_path(p, None, None));
                std::thread::spawn(move || {
                    if let Some(p) = tabs::show_open_dialog() {
                        apply(p);
                    } else {
                        eprintln!("[file] open dialog cancelled");
                    }
                });
            }
            "file_save" => {
                let remote_opt = remote_path_sig.get_untracked();
                // Only a remote save needs the text as one piece; a local save streams the rope.
                let text = || buffer_sig.get_untracked().text();
                let origin_opt = tabs_sig.with_untracked(|tm| tm.current().remote_origin.clone());
                if let Some(origin) = origin_opt {
                    // A document that lives on a server: the verified save engine (temp upload, check, atomic
                    // swap, conflict refusal) on a worker thread; never a local file. The modified marker is
                    // cleared by `RemoteHost::saved` only after the upload was verified.
                    remote_ui.save_doc(&origin, text().into_bytes());
                } else if let Some(rpath) = remote_opt {
                    if let Ok(c) = sftp_state_for_dispatch.client.lock() {
                        if c.is_connected() {
                            match c.write_file(&rpath, &text()) {
                                Ok(()) => {
                                    let mut nb = (*buffer_sig.get_untracked()).clone();
                                    nb.mark_saved();
                                    buffer_sig.set(Arc::new(nb));
                                    tabs_sig.update(|tm| {
                                        tm.current_mut().dirty = false;
                                    });
                                    eprintln!("[sftp] saved remote file: {}", rpath);
                                }
                                Err(e) => eprintln!("[sftp] save error: {e}"),
                            }
                        } else {
                            eprintln!("[sftp] save error: SFTP is not connected");
                        }
                    }
                } else if let Some(p) = path_sig.get_untracked() {
                    save_active_to(p);
                } else {
                    // Untitled: Save As.
                    let apply_save = create_ext_action(dispatch_scope, move |p: PathBuf| {
                        save_active_to(p);
                    });
                    std::thread::spawn(move || {
                        if let Some(p) = tabs::show_save_dialog(Some("untitled.txt")) {
                            apply_save(p);
                        }
                    });
                }
            }
            "file_save_as" => {
                let apply_save = create_ext_action(dispatch_scope, move |p: PathBuf| {
                    save_active_to(p);
                });
                std::thread::spawn(move || {
                    if let Some(p) = tabs::show_save_dialog(Some("untitled.txt")) {
                        apply_save(p);
                    }
                });
            }
            "file_print" => {
                let title = tabs_sig.with_untracked(|tm| tm.current().title.clone());
                let buf = buffer_sig.get_untracked();
                let text = if buf.is_lazy() { String::new() } else { buf.text() };
                let msg = if buf.is_lazy() { "Printing is not available in large file mode".to_string() } else { match brouillon::print_doc::print_text(&title, &text) {
                    Ok(m) | Err(m) => m,
                } };
                status_msg_sig.set(msg);
            }
            "file_new" => (on_new_tab)(),
            "file_close" => {
                let active = tabs_sig.with_untracked(|tm: &TabManager| tm.active_idx());
                (on_close)(active);
            }
            "tab_next" | "tab_previous" => {
                let (n, cur) = tabs_sig.with_untracked(|tm| (tm.count(), tm.active_idx()));
                if n > 1 {
                    let next = if id == "tab_next" { (cur + 1) % n } else { (cur + n - 1) % n };
                    (on_switch)(next);
                }
            }
            "tab_last_visited" | "diff_with_last_tab" => {
                let found = tabs_sig.with_untracked(|tm| {
                    let keys: Vec<String> = tm.iter().map(|t| brouillon::tab_mru::tab_key(t.path.as_deref(), &t.title)).collect();
                    let mru = tab_mru.borrow();
                    let want = mru.last_visited(&keys)?.to_string();
                    keys.iter().position(|k| *k == want)
                });
                match (found, id) {
                    (None, _) => status_msg_sig.set("No other tab has been visited yet".into()),
                    (Some(idx), "tab_last_visited") => (on_switch)(idx),
                    (Some(idx), _) => {
                        // Never copy a big document just to learn that it is too big to compare.
                        let small = |b: &Buffer| b.byte_len() <= DIFF_MAX_BYTES;
                        let (lt, ltext) = tabs_sig.with_untracked(|tm| tm.iter().nth(idx).map(|t| (t.title.clone(), if small(&t.buffer) { t.buffer.text() } else { "x".repeat(DIFF_MAX_BYTES + 1) })).unwrap_or_default());
                        let (rt, rtext) = tabs_sig.with_untracked(|tm| (tm.current().title.clone(), if small(&tm.current().buffer) { tm.current().buffer.text() } else { "x".repeat(DIFF_MAX_BYTES + 1) }));
                        if ltext.len().max(rtext.len()) > DIFF_MAX_BYTES {
                            status_msg_sig.set("These documents are too large to compare here".into());
                        } else {
                            let d = brouillon::diff_view::DiffData::new(&lt, &rt, &ltext, &rtext);
                            status_msg_sig.set(format!("Compare: {}", d.summary()));
                            diff_ui.show(d);
                        }
                    }
                }
            }
            "diff_with_disk" => {
                let path = path_sig.get_untracked();
                match path.as_deref().map(std::fs::read) {
                    Some(Ok(bytes)) => {
                        let disk = String::from_utf8_lossy(&bytes).into_owned();
                        let curbuf = buffer_sig.get_untracked();
                        let cur = if curbuf.byte_len() <= DIFF_MAX_BYTES { curbuf.text() } else { String::new() };
                        if disk.len().max(curbuf.byte_len()) > DIFF_MAX_BYTES {
                            status_msg_sig.set("This file is too large to compare here".into());
                        } else {
                            let name = path.as_deref().and_then(|p| p.file_name()).map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                            let d = brouillon::diff_view::DiffData::new(&format!("{name} (on disk)"), &format!("{name} (editor)"), &disk, &cur);
                            status_msg_sig.set(format!("Compare: {}", d.summary()));
                            diff_ui.show(d);
                        }
                    }
                    Some(Err(e)) => status_msg_sig.set(format!("Cannot read the file on disk: {e}")),
                    None => status_msg_sig.set("This document has no file on disk yet".into()),
                }
            }
            "diff_close" => diff_ui.close(),
            "edit_undo" | "edit_redo" => {
                let redo = id == "edit_redo";
                if crayon_mode_sig.get_untracked() {
                    sketch_doc_sig.update(|doc| {
                        if redo { doc.active_page_mut().redo_stroke(); } else { doc.active_page_mut().undo_stroke(); }
                    });
                } else {
                    let mut nb = (*buffer_sig.get_untracked()).clone();
                    let restored = if redo { nb.redo_restoring() } else { nb.undo_restoring() };
                    if let Some(sel) = restored {
                        let nb = Arc::new(nb);
                        buffer_sig.set(nb.clone());
                        match sel.filter(|r| !r.is_empty()) {
                            Some(regions) => {
                                let (a, h, extras) = brouillon::wiring::split_regions_buf(&nb, &regions);
                                cursor_sig.set(h);
                                selection_sig.set(Selection::new(a, h, false));
                                extra_carets_sig.set(extras);
                            }
                            None => {
                                extra_carets_sig.set(Vec::new());
                                let last = buffer_sig.get_untracked().line_count().max(1);
                                cursor_sig.update(|c| c.0 = c.0.min(last));
                                selection_sig.set(Selection::new((1, 1), (1, 1), false));
                            }
                        }
                    }
                }
            }
            "edit_undo_all" | "edit_undo_to_save" => {
                let mut nb = (*buffer_sig.get_untracked()).clone();
                let changed = if id == "edit_undo_all" { nb.undo_all() > 0 } else { nb.undo_to_save() };
                if changed {
                    buffer_sig.set(Arc::new(nb));
                    let last = buffer_sig.get_untracked().line_count().max(1);
                    cursor_sig.update(|c| c.0 = c.0.min(last));
                }
            }
            "edit_copy" | "edit_cut" | "select_all" | "edit_paste" => {
                let buf = buffer_sig.get_untracked();
                let sel = selection_sig.get_untracked();
                let regions = brouillon::wiring::regions_with_extras_buf(&buf, cursor_sig.get_untracked(), Some((sel.anchor, sel.head)), &extra_carets_sig.get_untracked());
                let r = *regions.last().unwrap();
                match id {
                    "select_all" => {
                        extra_carets_sig.set(Vec::new());
                        let end = brouillon::wiring::line_col_of_buf(&buf, buf.byte_len());
                        selection_sig.set(Selection::new((1, 1), end, false));
                        cursor_sig.set(end);
                    }
                    "edit_copy" | "edit_cut" => {
                        if !r.is_caret() {
                            if let Ok(mut cb) = arboard::Clipboard::new() {
                                let _ = cb.set_text(buf.slice(r.start()..r.end()));
                            }
                            if id == "edit_cut" {
                                let out = brouillon::contract::CmdOutcome {
                                    edits: brouillon::contract::EditSet(vec![brouillon::contract::TextEdit::delete(r.start(), r.end())]),
                                    selection: Some(vec![brouillon::contract::Region::caret(r.start())]),
                                    ..Default::default()
                                };
                                apply_ui_outcome(out, regions);
                            }
                        }
                    }
                    _ => {
                        if let Ok(clip) = arboard::Clipboard::new().and_then(|mut cb| cb.get_text()) {
                            let (edits, sel_after) = brouillon::cursor::paste(&regions, &clip);
                            let out = brouillon::contract::CmdOutcome { edits, selection: Some(sel_after), ..Default::default() };
                            apply_ui_outcome(out, regions);
                        }
                    }
                }
            }
            "bookmark_toggle" | "bookmark_clear" | "bookmark_invert" => {
                let (line0, count) = (cursor_sig.get_untracked().0.saturating_sub(1), buffer_sig.get_untracked().line_count());
                tabs_sig.update(|tm| {
                    let meta = &mut tm.current_mut().meta;
                    let mut m = brouillon::markers::Markers::from_lines(meta.markers.iter().copied());
                    match id {
                        "bookmark_toggle" => {
                            m.toggle(line0);
                        }
                        "bookmark_clear" => m.clear(),
                        _ => m.invert(count),
                    }
                    meta.markers = m.to_vec();
                });
            }
            "tools_toggle_output" => {
                output_visible_sig.update(|v| *v = !*v);
                active_panel_sig.set(if output_visible_sig.get_untracked() { "output" } else { "editor" });
            }
            "tools_stop" => {
                if let Some(h) = tool_handle.borrow().as_ref() {
                    h.stop();
                }
            }
            "tools_next_error" | "tools_prev_error" => {
                let fwd = id == "tools_next_error";
                if let Some((p, l, c)) = output_buf_sig.try_update(|b| b.step(fwd)).flatten() {
                    jump_to(p, l, c);
                }
            }
            "tools_goto_file_at_selection" if buffer_sig.get_untracked().is_lazy() => {
                status_msg_sig.set("Go to file at selection is not available in large file mode".into());
            }
            "tools_goto_file_at_selection" => {
                let text = buffer_sig.get_untracked().text();
                let sel = selection_sig.get_untracked();
                let r = brouillon::wiring::regions_from(&text, cursor_sig.get_untracked(), Some((sel.anchor, sel.head)))[0];
                let base = path_sig.get_untracked().and_then(|p| p.parent().map(PathBuf::from)).unwrap_or_default();
                match brouillon::error_patterns::goto_file_at(&text[r.start()..r.end()], &base, &brouillon::error_patterns::builtin_patterns()) {
                    Some((p, l, c)) => jump_to(p, l, c),
                    None => status_msg_sig.set("The selection is not a file location".into()),
                }
            }
            "results_next_hit" | "results_prev_hit" => {
                let next = id == "results_next_hit";
                let req = results_model_sig.try_update(|m| if next { m.next() } else { m.prev() }).flatten();
                if let Some(r) = req {
                    jump_to(r.path, r.line + 1, r.col + 1);
                }
                results_visible_sig.set(true);
                active_panel_sig.set("results");
            }
            "find_in_files" => run_find_in_files(),
            "replace_in_files" => {
                status_msg_sig.set("Replace in Files needs its confirmation dialog (not wired yet); use Find in Files".into());
            }
            _ if id.starts_with("tools_run_slot_") => {
                let slot = id.trim_start_matches("tools_run_slot_").parse::<u8>().unwrap_or(0);
                let _ = &tools_cfg;
                run_tool_slot(slot);
            }
            _ if brouillon::wiring::shell_implements(id) => eprintln!("[palette] `{id}` is shell-level but has no arm"),
            _ => run_lot_command(id),
        }
        palette_for_dispatch.close();
    }});

    // Brouillon Link, late-bound hooks: `run_command` (only the allowlisted ids ever get here, see
    // `link_host::SAFE_COMMANDS`) and `save` (no dialog ever; the external-change gate applies).
    {
        let d = dispatch.clone();
        *link_hooks.run.borrow_mut() = Some(Rc::new(move |id: &str| d(id)));
        let save: brouillon::link_host::SaveHook = Rc::new(move |idx: usize, force: bool| {
            use brouillon::link_host::SaveFail;
            let (path, meta, buf, active) = tabs_sig
                .with_untracked(|tm| {
                    let active = idx == tm.active_idx();
                    tm.iter().nth(idx).map(|t| (t.path.clone(), t.meta.clone(), if active { buffer_sig.get_untracked() } else { t.buffer.clone() }, active))
                })
                .ok_or_else(|| SaveFail::Io("no such tab".into()))?;
            let path = path.ok_or_else(|| SaveFail::Io("the document has no file".into()))?;
            if buf.is_lazy() {
                return Err(SaveFail::Io("this document is open in large file mode: agents cannot save it".into()));
            }
            brouillon::link_host::save_for_agent(&path, buf.rope(), &meta, force, &startup().backup)?;
            let mut nb = (*buf).clone();
            nb.mark_saved();
            let nb = Arc::new(nb);
            if active {
                buffer_sig.set(nb.clone());
            }
            tabs_sig.update(|tm| {
                if let Some(t) = tm.tab_mut(idx) {
                    if !active {
                        t.buffer = nb.clone();
                    }
                    t.dirty = t.sketch_unsaved();
                    t.meta.external_state = tabs::ExternalState::InSync;
                }
            });
            status_msg_sig.set(format!("Saved {} (requested by an agent)", path.display()));
            eprintln!("[file] saved {} for an agent", path.display());
            Ok(())
        });
        *link_hooks.save.borrow_mut() = Some(save);
    }

    // 10 Hz heartbeat (background thread -> GUI trigger). The effect below does
    // the periodic work on the GUI thread: forwarded IPC requests (single
    // instance), streaming tool output, and the crash-recovery autosave.
    {
        let tick = floem::reactive::Trigger::new();
        std::thread::spawn(move || loop {
            std::thread::sleep(Duration::from_millis(100));
            floem::ext_event::register_ext_trigger(tick);
        });
        let autosaver = RefCell::new(brouillon::recovery::AutoSaver::new(brouillon::recovery::DEFAULT_INTERVAL));
        let journal = brouillon::recovery::Journal::new(startup().paths.journal_dir());
        let tool_handle = tool_handle.clone();
        let open_path = open_path.clone();
        let sync = sync_active_to_mirrors;
        create_effect(move |_| {
            tick.track();
            // 1. requests forwarded by another `brouillon ...` invocation
            let mut reqs = Vec::new();
            if let Some(server) = IPC_SERVER.lock().unwrap().as_ref() {
                while let Some(r) = server.try_recv() {
                    reqs.push(r);
                }
            }
            for req in reqs {
                match req {
                    brouillon::ipc::Request::Open { path, line, col, .. } => open_path(path, line, col),
                    other => {
                        let pos = tabs_sig.try_update(|tm| brouillon::wiring::apply_request(tm, &other)).flatten();
                        sync();
                        if let Some(c) = pos {
                            cursor_sig.set(c);
                        }
                    }
                }
            }
            // 2. tool output
            let mut finished = false;
            if let Some(h) = tool_handle.borrow().as_ref() {
                while let Ok(ev) = h.events.try_recv() {
                    match ev {
                        brouillon::shell_runner::RunEvent::Line { stream, text } => output_buf_sig.update(|b| b.push(text, stream)),
                        brouillon::shell_runner::RunEvent::Finished { exit_code, stopped, elapsed } => {
                            tool_footer_sig.set(brouillon::shell_runner::format_footer(elapsed, exit_code, stopped));
                            finished = true;
                        }
                    }
                }
            }
            if finished {
                *tool_handle.borrow_mut() = None;
                tool_running_sig.set(false);
            }
            // 3. autosave journal
            if autosaver.borrow_mut().due(std::time::Instant::now()) {
                if let Err(e) = tabs_sig.with_untracked(|tm| journal.snapshot_tabs(tm)) {
                    eprintln!("[recovery] autosave failed: {e}");
                }
            }
        });
    }

    // Files named on the command line: open them (or activate the tab already
    // restored from the session) and place the caret.
    for (p, line, col) in &startup().targets {
        open_path(p.clone(), *line, *col);
    }

    // alpha.13 — Moldu UI chrome wire-up. Menu bar + toolbar sit between
    // the header brand stripe and the tab bar. Both are gated on the
    // Moldu preset via `dyn_container` — in Geek they collapse to a
    // zero-height `empty()` so the layout reflows cleanly. Toolbar
    // callbacks pipe straight into the palette dispatcher (action ids
    // were intentionally aligned). Menu items just log for alpha.13 —
    // dropdowns are M1 polish.
    let toolbar_dispatch = dispatch.clone();
    let menu_on_click: Arc<dyn Fn(&str) + 'static> = Arc::new({
        let active_menu_dropdown = active_menu_dropdown;
        move |id: &str| {
            let static_id = match id {
                "file" => Some("file"),
                "edit" => Some("edit"),
                "search" => Some("search"),
                "view" => Some("view"),
                "tools" => Some("tools"),
                "help" => Some("help"),
                _ => None,
            };
            active_menu_dropdown.update(|cur| {
                if *cur == static_id {
                    *cur = None;
                } else {
                    *cur = static_id;
                }
            });
            eprintln!("[menu] toggled dropdown: {:?}", static_id);
        }
    });

    let menu_bar_view_gated = floem::views::dyn_container(
        move || preset_sig.get().config().show_menu_bar,
        {
            let cb = menu_on_click.clone();
            move |show| {
                if show {
                    menu_bar::menu_bar_view(theme_idx, cb.clone()).into_any()
                } else {
                    empty().into_any()
                }
            }
        },
    );
    let toolbar_view_gated = floem::views::dyn_container(
        move || preset_sig.get().config().show_menu_bar,
        {
            let cb = toolbar_dispatch.clone();
            let crayon_mode_sig = crayon_mode_sig;
            move |show| {
                if show {
                    toolbar::toolbar_view(theme_idx, crayon_mode_sig, cb.clone()).into_any()
                } else {
                    empty().into_any()
                }
            }
        },
    );

    let hex_ui_body = hex_ui.clone();
    let body_view_gated = floem::views::dyn_container(
        move || (crayon_mode_sig.get(), hex_mode_sig.get()),
        {
            let hex_ui_body = hex_ui_body.clone();
            let buffer_sig = buffer_sig;
            let cursor_sig = cursor_sig;
            let theme_idx = theme_idx;
            let highlighter_sig = highlighter_sig;
            let sketch_doc_sig = sketch_doc_sig;
            let active_tool_sig = active_tool_sig;
            let active_color_sig = active_color_sig;
            let active_width_sig = active_width_sig;
            let infinite_layout_sig = infinite_layout_sig;
            let tabs_sig = tabs_sig;
            let sync_mirrors = Arc::new(sync_active_to_mirrors.clone());
            let fold_state_sig = fold_state_sig;
            let ac_visible = ac_visible;
            let ac_completions = ac_completions;
            let ac_index = ac_index;
            let markdown_preview_sig = markdown_preview_sig;
            move |(is_crayon, is_hex)| {
                if is_crayon {
                    brouillon::sketch_view::sketch_view(
                        crayon_mode_sig,
                        sketch_doc_sig,
                        active_tool_sig,
                        active_color_sig,
                        active_width_sig,
                        infinite_layout_sig,
                        theme_idx,
                        tabs_sig,
                        sync_mirrors.clone(),
                    ).into_any()
                } else if is_hex {
                    let hex_ui = hex_ui_body.clone();
                    let hex_ui_inner = hex_ui_body.clone();
                    floem::views::dyn_container(
                        move || hex_ui.active_path(),
                        move |active| {
                            if active.is_some() {
                                brouillon::hex_editor::hex_editor_panel(hex_ui_inner.clone(), theme_idx).into_any()
                            } else {
                                brouillon::hex_view::hex_view_panel(buffer_sig, theme_idx).into_any()
                            }
                        },
                    )
                    .style(|s| s.size_full())
                    .into_any()
                } else {
                    let buffer_sig = buffer_sig;
                    let cursor_sig = cursor_sig;
                    let theme_idx = theme_idx;
                    let highlighter_sig = highlighter_sig;
                    let bracket_paint_sig = bracket_paint_sig;
                    let selection_sig = selection_sig;
                    let is_dragging_selection = is_dragging_selection;
                    let fold_state_sig = fold_state_sig;
                    let ac_visible = ac_visible;
                    let ac_completions = ac_completions;
                    let ac_index = ac_index;
                    let markdown_preview_sig = markdown_preview_sig;

                    floem::views::dyn_container(
                        move || (markdown_preview_sig.get(), help_active_sig.get()),
                        move |(show, help_only)| {
                            if show && help_only {
                                // Help page: the rendered page alone (raw text next to it was clipped).
                                return brouillon::markdown_view::markdown_preview_panel(buffer_sig, theme_idx)
                                    .style(|s| s.size_full())
                                    .into_any();
                            }
                            let editor = brouillon::editor_view::editor_body(
                                buffer_sig,
                                cursor_sig,
                                theme_idx,
                                highlighter_sig,
                                bracket_paint_sig,
                                selection_sig,
                                is_dragging_selection,
                                fold_state_sig,
                                ac_visible,
                                ac_completions,
                                ac_index,
                                extra_carets_sig,
                                editor_opts,
                                brouillon::link_view::NotesView {
                                    notes: link_ui.line_notes,
                                    remove: Rc::new(move |id| {
                                        link_ui.notes.update(|v| v.retain(|n| n.id != id));
                                        brouillon::link_host::refresh_notes(link_ui, tabs_sig);
                                    }),
                                },
                            );
                            if show {
                                stack((
                                    editor.style(|s| s.flex_grow(1.0).width_pct(50.0)),
                                    brouillon::markdown_view::markdown_preview_panel(
                                        buffer_sig,
                                        theme_idx,
                                    )
                                    .style(move |s| {
                                        let t = current_theme(theme_idx.get());
                                        s.width_pct(50.0)
                                            .border_left(1.0)
                                            .border_color(t.border)
                                    }),
                                ))
                                .style(|s| s.flex_row().size_full())
                                .into_any()
                            } else {
                                editor.into_any()
                            }
                        }
                    )
                    // Fill the column (a bare dyn_container is as wide as its content).
                    .style(|s| s.size_full())
                    .into_any()
                }
            }
        }
    )
    // The editor column takes ALL the space between the side panels. Without this the
    // dyn_container shrank to its content width: clicks right of the text hit nothing
    // and the AI panel sat ~290 px from the sidebar with a blank area after it.
    .style(|s| s.flex_grow(1.0).flex_basis(0.0).min_width(0.0).min_height(0.0).height_full());

    let on_open_remote_file = {
        let tabs_sig = tabs_sig.clone();
        let buffer_sig = buffer_sig.clone();
        let path_sig = path_sig.clone();
        let remote_path_sig = remote_path_sig.clone();
        let encoding_sig = encoding_sig.clone();
        let eol_sig = eol_sig.clone();
        move |rpath: String, content: String| {
            tabs_sig.update(|tm| {
                tm.open_remote(rpath.clone(), content);
            });
            let (buf, p, rp, enc, eol) = tabs_sig.with(|tm| {
                let active = tm.current();
                (active.buffer.clone(), active.path.clone(), active.remote_path.clone(), active.encoding, active.eol)
            });
            buffer_sig.set(buf);
            path_sig.set(p);
            remote_path_sig.set(rp);
            encoding_sig.set(enc);
            eol_sig.set(eol);
            eprintln!("[sftp] opened remote file: {rpath}");
        }
    };

    // Wrap sidebar + editor body in a horizontal row
    let ai_panel_view_gated = brouillon::ai_panel::ai_panel_view(
        ai_visible_sig,
        ai_messages_sig,
        ai_input_sig,
        ai_privacy_sig,
        theme_idx,
        on_send_message.clone(),
    );

    let sftp_panel_view_gated = brouillon::sftp_panel::sftp_panel_view(
        sftp_visible_sig,
        sftp_state.clone(),
        theme_idx,
        on_open_remote_file,
    );

    // Remote explorer host: how a server file becomes a tab, and how a finished upload clears the marker.
    remote_ui.set_host(brouillon::remote_view::RemoteHost {
        open_doc: Rc::new({
            let sync_active = sync_active_to_mirrors.clone();
            move |origin: brouillon::remote::RemoteOrigin, bytes: Vec<u8>| {
                // Only valid UTF-8 text is opened: saving a lossy decoding would corrupt the server file.
                let Ok(text) = String::from_utf8(bytes) else {
                    status_msg_sig.set(format!("{} is not UTF-8 text; editing other encodings on a server comes later.", origin.path));
                    return;
                };
                tabs_sig.update(|tm| {
                    tm.open_remote_doc(origin.clone(), &text);
                });
                sync_active();
                status_msg_sig.set(format!("Opened {} from {}", origin.path, origin.site_label));
            }
        }),
        saved: Rc::new(move |origin: brouillon::remote::RemoteOrigin, sent: Vec<u8>| {
            // Only clear the marker if nothing was typed since the bytes that were uploaded.
            let current = buffer_sig.get_untracked().text();
            let is_active = tabs_sig.with_untracked(|tm| tm.current().remote_origin.as_ref() == Some(&origin));
            if is_active && current.as_bytes() == sent.as_slice() {
                let mut nb = (*buffer_sig.get_untracked()).clone();
                nb.mark_saved();
                buffer_sig.set(Arc::new(nb));
                tabs_sig.update(|tm| tm.current_mut().dirty = false);
            }
        }),
        say: Rc::new(move |m: String| status_msg_sig.set(m)),
        open_local: Rc::new({
            let open_path = open_path.clone();
            move |p: PathBuf| open_path(p, None, None)
        }),
        replace_doc: Rc::new({
            let sync_active = sync_active_to_mirrors.clone();
            move |origin: brouillon::remote::RemoteOrigin, bytes: Vec<u8>, modified: bool| {
                let Ok(text) = String::from_utf8(bytes) else {
                    status_msg_sig.set(format!("{} is not UTF-8 text.", origin.path));
                    return;
                };
                tabs_sig.update(|tm| {
                    tm.replace_remote_doc(origin, &text, modified);
                });
                sync_active();
            }
        }),
    });
    remote_ui.announce_recoverable();

    // Cliptext panel: an entry is inserted at the caret like Ctrl+Alt+N does (one undo step).
    let cliptext_panel_view = brouillon::cliptext_view::cliptext_side_panel(dialogs.cliptext, brouillon::cliptext_view::ClipHost {
        dir: startup().paths.cliptext_dir(),
        insert: Rc::new(move |clip: brouillon::cliptext::Clip| {
            if tabs_sig.with_untracked(|tm| tm.current().help_doc.is_some()) {
                status_msg_sig.set("This help page is read-only. File > Save As keeps an editable copy.".into());
                return;
            }
            if buffer_sig.get_untracked().is_lazy() {
                status_msg_sig.set("Cliptext is not available in large file mode".into());
                return;
            }
            let text = buffer_sig.get_untracked().text();
            let sel = selection_sig.get_untracked();
            let regions = brouillon::wiring::regions_from(&text, cursor_sig.get_untracked(), Some((sel.anchor, sel.head)));
            let path = path_sig.get_untracked();
            let ctx = brouillon::contract::CmdCtx { text: &text, selection: &regions, path: path.as_deref(), args: &[] };
            let out = brouillon::cliptext::expand(&clip, &ctx, &brouillon::cliptext::today());
            apply_ui_outcome(out, regions);
        }),
        selection: Rc::new(move || {
            let buf = buffer_sig.get_untracked();
            let sel = selection_sig.get_untracked();
            let r = brouillon::wiring::regions_from_buf(&buf, cursor_sig.get_untracked(), Some((sel.anchor, sel.head)))[0];
            buf.slice(r.start()..r.end())
        }),
        open_file: Rc::new({
            let open_path = open_path.clone();
            move |p| open_path(p, None, None)
        }),
    });

    // The left column: Open Files (Moldu preset, as before) and the Browser (Local, sites, cloud folders)
    // in one sidebar. A click in the editor gives the keyboard back.
    let tree_panel = brouillon::browser_view::sidebar(
        remote_ui.clone(),
        doc_entries_sig,
        theme_idx,
        {
            let on_switch_sidebar = on_switch.clone();
            move |idx| (on_switch_sidebar)(idx)
        },
        move || preset_sig.get().config().show_sidebar,
    );
    let body_view_gated = body_view_gated.on_event_cont(EventListener::PointerDown, {
        let remote_ui = remote_ui.clone();
        move |_| remote_ui.kbd_event(brouillon::kbd_focus::Ev::EditorPointer)
    });
    // The comparison view covers the editor column (not the side panels) until closed.
    let body_view_gated = stack((
        body_view_gated,
        brouillon::diff_view::diff_overlay(diff_ui, theme_idx, Rc::new(move || diff_ui.close())),
    ))
    .style(|s| s.flex_row().flex_grow(1.0).flex_basis(0.0).min_width(0.0).min_height(0.0).height_full());

    // Distraction-free mode hides every piece of chrome around the text.
    let zen = move |s: Style| if distraction_sig.get() { s.hide() } else { s };
    let tree_panel = tree_panel.style(zen);
    let sftp_panel_view_gated = sftp_panel_view_gated.style(zen);
    let ai_panel_view_gated = ai_panel_view_gated.style(zen);
    let cliptext_panel_view = cliptext_panel_view.style(zen);

    // Wrap sidebar (Open Files + Browser) + SFTP panel + editor body + AI Assistant panel + Cliptext panel in a horizontal row
    let body_with_sidebar = stack((
        tree_panel,
        sftp_panel_view_gated,
        body_view_gated,
        ai_panel_view_gated,
        cliptext_panel_view,
    )).style(|s| s.flex_row().flex_grow(1.0).min_height(0.0).height_full());


    // Round 15 — keep doc_entries_sig in sync with tabs_sig
    create_effect(move |_| {
        let entries = tabs_sig.with(|tm| {
            let titles: Vec<(String, bool)> = tm.iter()
                .map(|t| (t.title.clone(), t.dirty))
                .collect();
            brouillon::doc_selector::snapshot_entries(&titles, tm.active_idx())
        });
        doc_entries_sig.set(entries);
    });

    *dispatch_cell.borrow_mut() = Some(dispatch.clone());
    let macro_rec_keys = macro_rec.clone();
    // Ctrl+Q quits unless a keymap (Classic: Macro record) binds it to something.
    let ctrl_q_is_bound = {
        let q = brouillon::keymap::parse_chord("ctrl+q");
        key_bindings.iter().any(|b| q.as_ref() == Some(&b.chord))
    };
    let dispatch_for_keys = dispatch.clone();
    let dispatcher_for_keys = dispatcher.clone();
    let dispatch_for_status = dispatch.clone();

    // Status bar panes (BK-011) rebuilt from the active document.
    let panes_sig: RwSignal<Vec<brouillon::status_bar::Pane>> = create_rw_signal(Vec::new());
    let remote_ui_status = remote_ui.clone();
    create_effect(move |_| {
        let buf = buffer_sig.get();
        let cur = cursor_sig.get();
        let overwrite = overwrite_sig.get();
        let meta = tabs_sig.with(|tm| tm.current().meta.clone());
        let remote = tabs_sig.with(|tm| tm.current().remote_origin.clone()).and_then(|o| remote_ui_status.doc_state(&o));
        let mut panes = brouillon::wiring::status_panes_remote(&buf, cur, &meta, overwrite, remote);
        // Huge file still being indexed: "~N lines" and an "Indexing 63%" chip (nothing when exact).
        brouillon::viewport::decorate_panes(&mut panes, &editor_opts.vp.snapshot.get(), editor_opts.vp.pending_goto.get());
        panes_sig.set(panes);
    });

    // Huge file still being indexed: while a lazy index runs, a worker thread wakes the UI about ten
    // times a second (ext trigger, no UI timer); the UI publishes whole-percent progress to the
    // viewport signals ("Indexing 63%", estimated scroll extent). It stops by itself once exact.
    {
        let vp = editor_opts.vp;
        let tick = Scope::current().create_trigger();
        let running = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let running_ui = running.clone();
        create_effect(move |_| {
            tick.track();
            let buf = buffer_sig.get_untracked();
            vp.sync_from(&*buf);
            if vp.snapshot.get_untracked().exact {
                running_ui.store(false, std::sync::atomic::Ordering::SeqCst);
            }
        });
        create_effect(move |_| {
            let buf = buffer_sig.get();
            let snap = vp.snapshot.get_untracked();
            let exact = brouillon::viewport::IndexSnapshot::of(&*buf).exact;
            if exact {
                if !snap.exact {
                    vp.reset_from(&*buf);
                }
                return;
            }
            // A different document that is still being indexed: publish it and make sure the watcher runs.
            if snap.exact {
                vp.reset_from(&*buf);
            }
            if !running.swap(true, std::sync::atomic::Ordering::SeqCst) {
                let running = running.clone();
                std::thread::spawn(move || {
                    while running.load(std::sync::atomic::Ordering::SeqCst) {
                        std::thread::sleep(Duration::from_millis(100));
                        floem::ext_event::register_ext_trigger(tick);
                    }
                });
            }
        });
    }

    // Bottom panels: Find in Files results and streaming tool output.
    let results_panel_gated = floem::views::dyn_container(
        move || results_visible_sig.get(),
        {
            let jump_to = jump_to.clone();
            move |show| {
                if show {
                    let jump_to = jump_to.clone();
                    brouillon::results_panel::results_panel(results_model_sig, theme_idx, move |r: brouillon::results_model::OpenRequest| {
                        jump_to(r.path, r.line + 1, r.col + 1)
                    })
                    .into_any()
                } else {
                    empty().into_any()
                }
            }
        },
    );
    let output_panel_gated = floem::views::dyn_container(
        move || output_visible_sig.get(),
        {
            let tool_handle = tool_handle.clone();
            let jump_to = jump_to.clone();
            move |show| {
                if show {
                    let tool_handle = tool_handle.clone();
                    let jump_to = jump_to.clone();
                    let on_stop: Arc<dyn Fn() + 'static> = Arc::new(move || {
                        if let Some(h) = tool_handle.borrow().as_ref() {
                            h.stop();
                        }
                    });
                    let on_jump: Arc<dyn Fn(PathBuf, usize, usize) + 'static> = Arc::new(move |p, l, c| jump_to(p, l, c));
                    brouillon::output_view::output_stream_view(output_buf_sig, tool_running_sig, tool_footer_sig, on_stop, on_jump, theme_idx).into_any()
                } else {
                    empty().into_any()
                }
            }
        },
    );

    stack((
        // floem implements ViewTuple up to 16 elements: the three stacked chrome rows are grouped.
        stack((
            header_bar(fps_sig, theme_idx, toolbar_view_gated, brouillon::link_view::link_chip(link_ui, link_actions.clone())),
            menu_bar_view_gated,
            tab_bar::tab_bar_view(tabs_sig, on_switch, on_close, on_new_tab),
        ))
        .style(|s| s.flex_col().width_full())
        .style(zen),
        stack((
        brouillon::external_ui::banner_view(
            ext_ctx,
            Rc::new(move |p| {
                save_active_to(p);
            }),
            {
                let d = dispatch.clone();
                Rc::new(move || d("file_save_as"))
            },
        ),
        brouillon::update_ui::banner_view(update_ui_ctl.clone(), theme_idx),
        brouillon::module_ui::offer_bar(ext_ctx, module_as_text),
        ))
        .style(|s| s.flex_col().width_full()),
        body_with_sidebar,
        // Find in Files results, then streaming tool output (Stop, click-to-jump, F4).
        // Grouped: floem implements ViewTuple up to 16 elements.
        stack((results_panel_gated, output_panel_gated)).style(|s| s.flex_col().width_full()).style(zen),
        status_line(total_bytes, panes_sig, status_msg_sig, ram_sig, ram_detail_sig, preset_sig, theme_idx, path_sig, buffer_sig, bracket_paint_sig, language_sig, move |cmd| dispatch_for_status(cmd)).style(zen),
        // Find/Replace overlay paints last so its `Style::absolute()` floats
        // over the editor body. The dyn_container inside collapses to empty()
        // when `visible.get() == false` — no stale paint.
        find_replace::find_replace_overlay(fr_for_overlay, buffer_sig)
            .into_any(),
        palette_view::palette_overlay(palette_for_overlay, dispatch.clone()).into_any(),
        brouillon::quick_open_view::quick_open_overlay(
            quick_open_state,
            {
                let open_path = open_path.clone();
                let fr = find_replace_state.clone();
                Rc::new(move |hit: brouillon::quick_open::Hit| {
                    use brouillon::quick_open::Hit;
                    let goto = |line: usize, col: usize| {
                        let last = buffer_sig.get_untracked().line_count().max(1);
                        cursor_sig.set((line.clamp(1, last), col.max(1)));
                        selection_sig.set(Selection::new((1, 1), (1, 1), false));
                    };
                    match hit {
                        Hit::File(p) => open_path(p, None, None),
                        Hit::Goto { line, col } => goto(line, col.unwrap_or(1)),
                        Hit::Symbol(s) => goto(s.line, 1),
                        Hit::Text(t) => {
                            fr.query.set(t);
                            fr.visible.set(true);
                        }
                    }
                })
            },
            Rc::new(move |m: Option<String>| status_msg_sig.set(m.unwrap_or_default())),
        )
        .into_any(),
        // alpha.11 : Go-to-line dialog (Ctrl-G or palette edit.go-to-line).
        brouillon::goto_view::go_to_line_overlay(goto_visible, goto_input, cursor_sig, buffer_sig, theme_idx, editor_opts.vp).into_any(),
        // Encoding picker: choosing an encoding sets the codepage used by the next save.
        floem::views::dyn_container(
            move || encoding_picker_visible.get(),
            move |show| {
                if !show {
                    return floem::views::empty().into_any();
                }
                let on_pick = move |id: &'static str| {
                    match brouillon::codepages::supported(id) {
                        Ok(cp) => {
                            tabs_sig.update(|tm| tm.current_mut().meta.codepage = cp.id);
                            status_msg_sig.set(format!("Next save will encode as {} (the text on screen is not re-decoded)", cp.label));
                        }
                        Err(_) => status_msg_sig.set(format!("Encoding `{id}` is not supported")),
                    }
                    encoding_picker_visible.set(false);
                };
                floem::views::stack((
                    floem::views::text_input(encoding_query)
                        .placeholder("Filter encodings...")
                        .request_focus(|| {})
                        .style(move |s| {
                            let t = current_theme(theme_idx.get());
                            s.width(280.0).padding(6.0).background(t.bg_chrome).color(t.fg)
                                .border(1.0).border_color(t.accent).border_radius(2.0)
                        }),
                    brouillon::encoding_picker::encoding_picker_view(encoding_query, theme_idx, on_pick),
                ))
                .style(|s| s.flex_col().gap(4.0))
                .into_any()
            },
        )
        .style(|s| s.absolute().inset_top(112.0).inset_left(300.0))
        .into_any(),
        // Round 14 : floating top-bar dropdown popovers Tokyo-Night styled.
        brouillon::menu_dropdown::menu_dropdown_overlay(active_menu_dropdown, theme_idx, dispatch).into_any(),
        // Preferences dialog (settings.toml) and its effects that need no relaunch.
        brouillon::settings_editor::settings_overlay(dialogs.settings, startup().paths.settings_file(), Rc::new({
            let rebind = rebind.clone();
            let link_start = link_start.clone();
            let reload_settings = reload_settings.clone();
            move |key: &str| match key {
                "theme.mode" | "theme.name" => brouillon::theme_mode::reload(),
                "keymap.classic" => rebind(),
                k if k.starts_with("editor.") || k.starts_with("files.") => reload_settings(),
                "mcp.enabled" => {
                    let text = std::fs::read_to_string(startup().paths.settings_file()).unwrap_or_default();
                    link_start(&brouillon_mcp::settings::McpSettings::from_toml(&text));
                }
                _ => {}
            }
        }), Some(link_connect.clone())).into_any(),
        // Keyboard Shortcuts dialog (keymap.toml); a saved change swaps the live chord table.
        brouillon::keymap_view::shortcuts_overlay(dialogs.shortcuts, brouillon::keymap_view::KeymapInit {
            commands: brouillon::wiring::command_list(&registry),
            base: {
                let registry = registry.clone();
                Rc::new(move || brouillon::wiring::base_bindings(&registry, read_classic()))
            },
            file: startup().paths.keymap_file(),
            on_saved: rebind.clone(),
        }).into_any(),
        // Brouillon Link: consent for a new client, and the review panel for proposed edits.
        brouillon::link_view::consent_modal(link_ui, link_actions.clone()).into_any(),
        brouillon::link_view::proposal_panel(link_ui, link_actions.clone()).into_any(),
        // First-run screens, EditPlus import and About: one wrapper that only exists while one of them is up
        // (floem implements ViewTuple up to 16 elements).
        stack((
            brouillon::remote_view::connect_modal(remote_ui.clone()),
            brouillon::remote_view::sites_modal(remote_ui.clone()),
            brouillon::remote_dialogs::remote_dialogs(remote_ui.clone()),
            brouillon::onboarding_view::firstrun_overlay(wizard, wizard_host.clone()),
            brouillon::onboarding_view::import_overlay(dialogs.import, import_host.clone()),
            brouillon::onboarding_view::about_overlay(
                dialogs.about,
                brouillon::onboarding::about_rows(),
                brouillon::onboarding::about_notes(),
                Rc::new(|t: String| {
                    if let Ok(mut cb) = arboard::Clipboard::new() {
                        let _ = cb.set_text(t);
                    }
                }),
            ),
        ))
        .style(move |s| {
            if dialogs.firstrun.get() || dialogs.import.get() || dialogs.about.get() || dialogs.remote_connect.get() || dialogs.remote_sites.get() || dialogs.remote_dialog.get() {
                s.absolute().inset_left(0.0).inset_top(0.0).size_full()
            } else {
                s.display(floem::style::Display::None)
            }
        })
        .into_any(),
    ))
    .style(move |s| {
        let t = current_theme(theme_idx.get());
        brouillon::theme_ctx::inherited(s, &t)
            .size_full()
            .flex_col()
            .background(t.bg)
            .color(t.fg)
            .font_family("Consolas, Menlo, DejaVu Sans Mono, monospace".to_string())
            .font_size(13.0)
    })
    // Arrow keys + Home/End/PageUp/PageDown — cursor movement. KeyDown (not
    // KeyUp) so OS key-repeat fires per held tick. Find/Replace shortcuts
    // are handled here first (Ctrl-F toggles, Ctrl-H opens replace, Escape
    // closes) so they intercept before cursor movement consumes the event.
    .on_event_stop(EventListener::KeyDown, {
        move |event| {
        tick_frame();
        if let Event::KeyDown(ke) = event {
            if std::env::var_os("BROUILLON_TRACE_KEYS").is_some() {
                eprintln!("[TRACE-KEY] root keydown {:?} phys={:?} repeat={} text={:?} mods={:?}", ke.key.logical_key, ke.key.physical_key, ke.key.repeat, ke.key.text, ke.modifiers);
            }
            // Kill switch of Brouillon Link (Ctrl+Alt+K, left Alt): works whatever is on screen.
            if ke.modifiers.control() && ke.modifiers.alt() && !ke.modifiers.altgr() && !ke.modifiers.shift()
                && chord_key_name(&ke.key.logical_key, &ke.key.physical_key, true, false).as_deref() == Some("k")
            {
                (link_kill_key)();
                return;
            }
            // A consent question owns the keyboard: Enter = read only, Escape = deny, any other
            // key is swallowed so nothing is typed into the document behind the dimmed window.
            if brouillon::link_view::consent_key(
                link_ui,
                &link_actions_key,
                ke.key.logical_key == Key::Named(NamedKey::Enter),
                ke.key.logical_key == Key::Named(NamedKey::Escape),
            ) {
                return;
            }
            // First-run screens: arrows / Enter / letters drive them; the import dialog opened from
            // them (above) takes its own keys below.
            if dialogs.firstrun.get_untracked() && !dialogs.import.get_untracked() && !dialogs.about.get_untracked() {
                brouillon::onboarding_view::handle_key(&wizard, &wizard_host_keys, &ke.key.logical_key);
                return;
            }
            // A preference dialog owns the keyboard: nothing reaches the document or the chord table.
            if dialogs.modal_open() {
                if ke.key.logical_key == Key::Named(NamedKey::Escape) {
                    dialogs.close_modals();
                    remote_ui.cancel_dialogs();
                }
                return;
            }
            // ── Browser pane owns the arrows / Enter / F2 / F5 / Delete / Backspace / Alt+arrows / Ctrl+L while it
            // has the keyboard (a click in a row gives it, a click in the editor or in a text field takes it back) ──
            if remote_ui.rows_focused() && remote_ui.browser_visible() && remote_ui.active.get_untracked().is_some() {
                let (ctrl, alt) = (ke.modifiers.control(), ke.modifiers.alt());
                let k = match (&ke.key.logical_key, ctrl, alt) {
                    (Key::Named(NamedKey::ArrowLeft), false, true) => Some("back"),
                    (Key::Named(NamedKey::ArrowRight), false, true) => Some("forward"),
                    (Key::Named(NamedKey::ArrowUp), false, true) => Some("up_dir"),
                    (Key::Character(c), true, false) if c.eq_ignore_ascii_case("l") => Some("path"),
                    (Key::Named(NamedKey::ArrowUp), false, false) => Some("up"),
                    (Key::Named(NamedKey::ArrowDown), false, false) => Some("down"),
                    (Key::Named(NamedKey::ArrowLeft), false, false) => Some("left"),
                    (Key::Named(NamedKey::ArrowRight), false, false) => Some("right"),
                    (Key::Named(NamedKey::Enter), false, false) => Some("enter"),
                    (Key::Named(NamedKey::F5), false, false) => Some("f5"),
                    (Key::Named(NamedKey::F2), false, false) => Some("f2"),
                    (Key::Named(NamedKey::Delete), false, false) => Some("delete"),
                    (Key::Named(NamedKey::Backspace), false, false) => Some("backspace"),
                    _ => None,
                };
                if let Some(k) = k {
                    remote_ui.key(k);
                    return;
                }
                if ke.key.logical_key == Key::Named(NamedKey::Escape) {
                    // First Escape clears the filter, the next one gives the keyboard back.
                    if remote_ui.filter.get_untracked().is_empty() {
                        remote_ui.kbd_event(brouillon::kbd_focus::Ev::Escape);
                    } else {
                        remote_ui.filter.set(String::new());
                    }
                    return;
                }
                // Type-to-filter: a printable key narrows the rows already loaded.
                if !ctrl && !alt {
                    if let Key::Character(c) = &ke.key.logical_key {
                        if !c.is_empty() && c.chars().all(|ch| !ch.is_control() && ch != ' ') {
                            remote_ui.type_char(c);
                            return;
                        }
                    }
                    // Nothing typed or edited while the pane owns the keyboard may reach the document
                    // (a space, Tab, Home/End/Page keys...); function keys and Ctrl/Alt shortcuts stay free.
                    let text_or_edit = matches!(
                        &ke.key.logical_key,
                        Key::Character(_) | Key::Named(NamedKey::Space | NamedKey::Tab | NamedKey::Home | NamedKey::End | NamedKey::PageUp | NamedKey::PageDown | NamedKey::Insert)
                    );
                    if brouillon::kbd_focus::pane_swallows_plain_key(brouillon::kbd_focus::Base::Rows, text_or_edit) {
                        return;
                    }
                }
            }
            // Hex grid keys (bytes, not text) while the hex view is shown and no overlay owns the keyboard.
            // The grid also handles them itself when it has the focus; this catches the rest so a typed digit
            // can never reach the hidden text buffer. Text fields of the hex view (goto, region name) are respected.
            if hex_mode_sig.get_untracked()
                && hex_ui.is_active()
                && !palette_for_keys.visible.get_untracked()
                && !fr_for_keys.visible.get_untracked()
                && !goto_visible.get_untracked()
                && !quick_open_state.visible.get_untracked()
                && hex_ui.handle_key(&ke.key.logical_key, ke.modifiers.control(), ke.modifiers.shift(), ke.modifiers.alt())
            {
                return;
            }
            if diff_ui.is_open() && ke.key.logical_key == Key::Named(NamedKey::Escape) {
                diff_ui.close();
                return;
            }
            // ── Auto-complete Popup navigation ──
            if ac_visible.get_untracked() {
                // The popup opens by itself while typing, so plain arrows must keep moving the
                // caret (they close it); Ctrl+Up/Down choose a suggestion.
                let ctrl = ke.modifiers.control();
                if !ctrl
                    && matches!(
                        ke.key.logical_key,
                        Key::Named(NamedKey::ArrowDown | NamedKey::ArrowUp | NamedKey::ArrowLeft | NamedKey::ArrowRight)
                    )
                {
                    ac_suppress.set(Arc::as_ptr(&buffer_sig.get_untracked()) as usize);
                    ac_visible.set(false);
                }
                if ctrl && ke.key.logical_key == Key::Named(NamedKey::ArrowDown) {
                    ac_index.update(|idx| {
                        let total = ac_completions.get_untracked().len().max(1);
                        *idx = (*idx + 1) % total;
                    });
                    return;
                }
                if ctrl && ke.key.logical_key == Key::Named(NamedKey::ArrowUp) {
                    ac_index.update(|idx| {
                        let total = ac_completions.get_untracked().len();
                        if total > 0 {
                            *idx = if *idx == 0 { total - 1 } else { *idx - 1 };
                        }
                    });
                    return;
                }
                if ke.key.logical_key == Key::Named(NamedKey::Escape) {
                    ac_visible.set(false);
                    return;
                }
                // Only Tab accepts. Enter must stay a newline: the popup opens by itself
                // while typing, and swallowing Enter replaced the word just typed.
                if ke.key.logical_key == Key::Named(NamedKey::Tab) {
                    accept_completion();
                    return;
                }
                if ke.key.logical_key == Key::Named(NamedKey::Enter) {
                    ac_visible.set(false);
                }
            }

            // ── Modifier-key shortcuts ──
            // Order matters: capture our bindings BEFORE handing off to
            // cursor navigation, otherwise key-repeat consumes them.
            if ke.modifiers.control() {
                // Find/Replace (Round 2 wire-up).
                if ke.key.logical_key == Key::Character("f".into()) {
                    fr_for_keys.visible.update(|v| *v = !*v);
                    return;
                }
                if ke.key.logical_key == Key::Character("h".into()) {
                    fr_for_keys.visible.set(true);
                    return;
                }
                // Ctrl-T : cycle theme. The header_bar reads theme_idx
                // reactively, so the brand stripe + FPS color flip on bump.
                if ke.key.logical_key == Key::Character("t".into()) {
                    brouillon::theme_mode::cycle();
                    return;
                }
                // Ctrl-K : toggle command palette (visibility only — overlay
                // UI is Round 4 ; for now we log the toggle so we can see
                // bindings work end-to-end).
                if ke.key.logical_key == Key::Character("k".into()) {
                    palette_for_keys.toggle();
                    let st = if palette_for_keys.visible.get_untracked() { "open" } else { "closed" };
                    eprintln!("[palette] {st}");
                    return;
                }
                // Ctrl-O / S / Shift-S / N / W / Z / Shift-Z / Y: same code paths as the
                // palette and the menus (file_io + backup, atomic edit groups, undo API).
                if let Key::Character(ref c) = ke.key.logical_key {
                    let shift = ke.modifiers.shift();
                    let id = match (c.to_lowercase().as_str(), shift, ke.modifiers.alt()) {
                        ("o", false, false) => Some("file_open"),
                        ("s", false, false) => Some("file_save"),
                        ("s", true, false) => Some("file_save_as"),
                        ("n", false, false) => Some("file_new"),
                        ("w", false, false) => Some("file_close"),
                        ("z", false, false) => Some("edit_undo"),
                        ("z", true, false) | ("y", _, false) => Some("edit_redo"),
                        _ => None,
                    };
                    if let Some(id) = id {
                        dispatch_for_keys(id);
                        return;
                    }
                }
                // alpha.11 : Ctrl-G — toggle go-to-line dialog. Resets the
                // input field each time it opens.
                if ke.key.logical_key == Key::Character("g".into()) {
                    goto_visible.update(|v| *v = !*v);
                    if goto_visible.get_untracked() {
                        goto_input.set(String::new());
                    }
                    return;
                }
                if ke.key.logical_key == Key::Named(NamedKey::PageUp) {
                    dispatch_for_keys("tab_previous");
                    return;
                }
                if ke.key.logical_key == Key::Named(NamedKey::PageDown) {
                    dispatch_for_keys("tab_next");
                    return;
                }
                // Ctrl-Shift-M : flip Moldu / Geek preset. status_line reads
                // preset_sig reactively, so the bottom bar reflects the flip
                // immediately.
                if ke.modifiers.shift()
                    && (ke.key.logical_key == Key::Character("m".into())
                        || ke.key.logical_key == Key::Character("M".into()))
                {
                    preset_sig.update(|p| *p = p.flipped());
                    let p = preset_sig.get_untracked();
                    let cfg = p.config();
                    eprintln!(
                        "[preset] switched to {} (font={:.1}, menus={}, palette_on_startup={})",
                        p.name(),
                        cfg.default_font_size,
                        cfg.show_menu_bar,
                        cfg.palette_on_startup
                    );
                    return;
                }
                // M2 : Ctrl-Shift-H : toggle hex view mode
                if ke.modifiers.shift()
                    && (ke.key.logical_key == Key::Character("h".into())
                        || ke.key.logical_key == Key::Character("H".into()))
                {
                    hex_mode_sig.update(|h| *h = !*h);
                    eprintln!("[hex] hex mode -> {}", hex_mode_sig.get_untracked());
                    return;
                }
                // M2/M3 : Ctrl-Shift-V : toggle Markdown Preview panel
                if ke.modifiers.shift()
                    && (ke.key.logical_key == Key::Character("v".into())
                        || ke.key.logical_key == Key::Character("V".into()))
                {
                    markdown_preview_sig.update(|m| *m = !*m);
                    eprintln!("[markdown] preview -> {}", markdown_preview_sig.get_untracked());
                    return;
                }
                // M2 : Ctrl-Shift-[ : fold all
                if ke.modifiers.shift()
                    && (ke.key.logical_key == Key::Character("[".into())
                        || ke.key.logical_key == Key::Character("{".into())
                        || ke.key.physical_key == PhysicalKey::Code(KeyCode::BracketLeft))
                {
                    fold_state_sig.update(|fs| fs.fold_all());
                    // A caret inside a block that just got hidden moves to the header that hides it.
                    let (l, c) = cursor_sig.get_untracked();
                    let shown = fold_state_sig.get_untracked().nearest_visible(l.saturating_sub(1)) + 1;
                    if shown != l {
                        cursor_sig.set((shown, c));
                        selection_sig.set(Selection::new((shown, c), (shown, c), false));
                    }
                    eprintln!("[fold] fold all");
                    return;
                }
                // M2 : Ctrl-Shift-] : unfold all
                if ke.modifiers.shift()
                    && (ke.key.logical_key == Key::Character("]".into())
                        || ke.key.logical_key == Key::Character("}".into())
                        || ke.key.physical_key == PhysicalKey::Code(KeyCode::BracketRight))
                {
                    fold_state_sig.update(|fs| fs.unfold_all());
                    eprintln!("[fold] unfold all");
                    return;
                }
                // Ctrl+1..0 run the tool bound to that slot: see tools_run_slot_N in the chord table.
                // M2 Sprint 3 : Ctrl-` : toggle output panel
                if ke.key.logical_key == Key::Character("`".into()) {
                    output_visible_sig.update(|v| *v = !*v);
                    eprintln!("[output] panel -> {}", output_visible_sig.get_untracked());
                    return;
                }
                // Ctrl+Alt+A (AI panel) and Ctrl+Alt+S (SFTP panel) are registry chords
                // (view_toggle_ai / view_toggle_sftp): they used to be matched here on the
                // logical character, which Windows never reports for Ctrl+Alt+letter.
            }
            if ke.key.logical_key == Key::Named(NamedKey::Escape) {
                active_menu_dropdown.set(None);
                encoding_picker_visible.set(false);
                fr_for_keys.visible.set(false);
                palette_for_keys.close();
                quick_open_state.close();
                goto_visible.set(false);
                output_visible_sig.set(false);
                results_visible_sig.set(false);
                active_panel_sig.set("editor");
                selection_sig.set(Selection::new((1, 1), (1, 1), false));
                extra_carets_sig.set(Vec::new());
                return;
            }

            // ── Registry chords: F-keys, Ctrl/Alt combinations, Classic overlay, user keymap ──
            // Runs after the fixed shell shortcuts above so those keep priority,
            // and before text entry so a bound chord is never typed.
            let is_fkey = matches!(
                ke.key.logical_key,
                Key::Named(NamedKey::F1 | NamedKey::F2 | NamedKey::F3 | NamedKey::F4 | NamedKey::F5 | NamedKey::F6
                    | NamedKey::F7 | NamedKey::F8 | NamedKey::F9 | NamedKey::F10 | NamedKey::F11 | NamedKey::F12)
            );
            if ke.modifiers.control() || ke.modifiers.alt() || is_fkey {
                if let Some(name) = chord_key_name(&ke.key.logical_key, &ke.key.physical_key, ke.modifiers.control(), ke.modifiers.altgr()) {
                    let combo = brouillon::wiring::combo(ke.modifiers.control(), ke.modifiers.shift(), ke.modifiers.alt(), &name);
                    let resolution = {
                        let mut d = dispatcher_for_keys.borrow_mut();
                        d.ctx.set_str("active_panel", active_panel_sig.get_untracked());
                        d.press(combo, now_millis())
                    };
                    match resolution {
                        brouillon::keymap::Resolution::Command(id) => {
                            dispatch_for_keys(&id);
                            return;
                        }
                        brouillon::keymap::Resolution::Pending => return,
                        brouillon::keymap::Resolution::NoMatch => {}
                    }
                }
            }

            // A built-in help page is read-only: keys that would type or delete text do nothing.
            if brouillon::startup_doc::is_editing_key(&ke.key.logical_key, ke.modifiers.control(), ke.modifiers.alt())
                && tabs_sig.with_untracked(|tm| tm.current().help_doc.is_some())
            {
                return;
            }

            // ── Emmet: Tab right after an abbreviation in an HTML / CSS file expands it ──
            if ke.key.logical_key == Key::Named(NamedKey::Tab) && !ke.modifiers.control() && !ke.modifiers.alt() && !ke.modifiers.shift() && emmet_expand_at_caret() {
                macro_rec_keys.borrow_mut().record_command("emmet_expand");
                return;
            }

            // ── Macro recording: typed text and the editing keys (commands are recorded by `dispatch`) ──
            if macro_rec_keys.borrow().is_recording() && !ke.modifiers.control() && !ke.modifiers.alt() {
                let name = match &ke.key.logical_key {
                    Key::Named(NamedKey::Enter) => Some("enter"),
                    Key::Named(NamedKey::Backspace) => Some("backspace"),
                    Key::Named(NamedKey::Delete) => Some("delete"),
                    Key::Named(NamedKey::ArrowLeft) if !ke.modifiers.shift() => Some("left"),
                    Key::Named(NamedKey::ArrowRight) if !ke.modifiers.shift() => Some("right"),
                    Key::Named(NamedKey::ArrowUp) if !ke.modifiers.shift() => Some("up"),
                    Key::Named(NamedKey::ArrowDown) if !ke.modifiers.shift() => Some("down"),
                    Key::Named(NamedKey::Home) if !ke.modifiers.shift() => Some("home"),
                    Key::Named(NamedKey::End) if !ke.modifiers.shift() => Some("end"),
                    _ => None,
                };
                match name {
                    Some(k) => macro_rec_keys.borrow_mut().record_key(k),
                    None => {
                        if let Some(s) = typed_text(&ke.key.logical_key) {
                            macro_rec_keys.borrow_mut().record_text(&s);
                        }
                    }
                }
            }

            let (line_1b, col_1b) = cursor_sig.get_untracked();
            let now_ms = now_millis();

            // ── Multi-caret session: typing, Backspace, Delete and the arrows act at EVERY caret
            //    as ONE edit set (one undo step). Esc / a plain click collapses (see above). ──
            if !extra_carets_sig.get_untracked().is_empty() && !ke.modifiers.control() && !buffer_sig.get_untracked().is_lazy() {
                let text = buffer_sig.get_untracked().text();
                let sel = selection_sig.get_untracked();
                let regions = brouillon::wiring::regions_with_extras(&text, cursor_sig.get_untracked(), Some((sel.anchor, sel.head)), &extra_carets_sig.get_untracked());
                let named = match &ke.key.logical_key { Key::Named(n) => Some(*n), _ => None };
                let motion = match named {
                    Some(NamedKey::ArrowLeft) => Some(brouillon::cursor::Motion::Left),
                    Some(NamedKey::ArrowRight) => Some(brouillon::cursor::Motion::Right),
                    Some(NamedKey::ArrowUp) => Some(brouillon::cursor::Motion::Up),
                    Some(NamedKey::ArrowDown) => Some(brouillon::cursor::Motion::Down),
                    Some(NamedKey::Home) => Some(brouillon::cursor::Motion::Home),
                    Some(NamedKey::End) => Some(brouillon::cursor::Motion::End),
                    _ => None,
                };
                let edit = if let Some(m) = motion {
                    let moved = brouillon::cursor::move_regions(&text, &regions, m, ke.modifiers.shift());
                    let (a, h, ex) = brouillon::wiring::split_regions(&text, &moved);
                    cursor_sig.set(h);
                    selection_sig.set(Selection::new(a, h, false));
                    extra_carets_sig.set(ex);
                    return;
                } else if named == Some(NamedKey::Backspace) {
                    Some(brouillon::cursor::delete_chars(&text, &regions, false))
                } else if named == Some(NamedKey::Delete) {
                    Some(brouillon::cursor::delete_chars(&text, &regions, true))
                } else if named == Some(NamedKey::Enter) {
                    Some(brouillon::cursor::insert_text(&regions, "
"))
                } else if let Some(ref s) = typed_text(&ke.key.logical_key) {
                    if !ke.modifiers.alt() { Some(brouillon::cursor::insert_text(&regions, s)) } else { None }
                } else {
                    None
                };
                if let Some((edits, after)) = edit {
                    let out = brouillon::contract::CmdOutcome { edits, selection: Some(after), ..Default::default() };
                    apply_ui_outcome(out, regions);
                    return;
                }
            }

            // ── Delete (forward): the editor had no handler for it ──
            if ke.key.logical_key == Key::Named(NamedKey::Delete) && !ke.modifiers.control() && !ke.modifiers.shift() {
                if delete_selection() {
                    return;
                }
                let buf = buffer_sig.get_untracked();
                let regions = brouillon::wiring::regions_from_buf(&buf, cursor_sig.get_untracked(), None);
                let win = brouillon::wiring::Window::around(&buf, regions[0].head);
                let (edits, after) = brouillon::cursor::delete_chars(&win.text, &win.rel(&regions), true);
                let (edits, after) = (win.abs_edits(edits), win.abs_regions(after));
                if !edits.is_empty() {
                    let out = brouillon::contract::CmdOutcome { edits, selection: Some(after), ..Default::default() };
                    apply_ui_outcome(out, regions);
                }
                return;
            }

            // ── Enter (newline with smart auto-indent, one undo group) ──
            if ke.key.logical_key == Key::Named(NamedKey::Enter) {
                delete_selection();
                let buf = buffer_sig.get_untracked();
                let regions = brouillon::wiring::regions_from_buf(&buf, cursor_sig.get_untracked(), None);
                let win = brouillon::wiring::Window::around(&buf, regions[0].head);
                let text = &win.text;
                let path = path_sig.get_untracked();
                let prefs = prefs_sig.get_untracked();
                if !prefs.auto_indent {
                    // `editor.auto_indent = false`: a plain newline.
                    let eol = if text.contains("\r\n") { "\r\n" } else { "\n" };
                    let (edits, after) = brouillon::cursor::insert_text(&regions, eol);
                    apply_ui_outcome(brouillon::contract::CmdOutcome { edits, selection: Some(after), ..Default::default() }, regions);
                } else {
                    let lang = path.as_deref().and_then(|p| p.extension()).map(|e| e.to_string_lossy().into_owned()).unwrap_or_default();
                    let mut profile = brouillon::autoindent::IndentProfile::for_language(&lang);
                    // Tab-indented languages (Go, Makefile) keep their tab unless the user asked for tabs everywhere.
                    if !prefs.insert_spaces {
                        profile.unit = "\t".into();
                    } else if profile.unit != "\t" {
                        profile.unit = prefs.indent_unit();
                    }
                    let out = brouillon::autoindent::on_enter(text, regions.first().map_or(0, |r| r.head - win.base), &profile);
                    apply_ui_outcome(win.abs_outcome(out), regions);
                }
                return;
            }

            // ── Tab: spaces to the next tab stop, or a tab character (`editor.insert_spaces`, `editor.tab_size`) ──
            if ke.key.logical_key == Key::Named(NamedKey::Tab) && !ke.modifiers.control() && !ke.modifiers.alt() && !ke.modifiers.shift() {
                delete_selection();
                let buf = buffer_sig.get_untracked();
                let cur = cursor_sig.get_untracked();
                let before: String = buf.nth_line(cur.0.saturating_sub(1)).unwrap_or_default().chars().take(cur.1.saturating_sub(1)).collect();
                let ins = prefs_sig.get_untracked().tab_text(&before);
                let regions = brouillon::wiring::regions_from_buf(&buf, cur, None);
                let (edits, after) = brouillon::cursor::insert_text(&regions, &ins);
                apply_ui_outcome(brouillon::contract::CmdOutcome { edits, selection: Some(after), ..Default::default() }, regions);
                return;
            }

            // ── Insert: toggle INS / OVR ──
            if ke.key.logical_key == Key::Named(NamedKey::Insert) && !ke.modifiers.control() && !ke.modifiers.shift() {
                overwrite_sig.update(|o| *o = !*o);
                return;
            }

            // ── Backspace (coalesced into the current typing run) ──
            if ke.key.logical_key == Key::Named(NamedKey::Backspace) {
                if delete_selection() {
                    return;
                }
                let buf = buffer_sig.get_untracked();
                let mut nb = (*buf).clone();
                let old_buf = buf.clone();
                if col_1b > 1 {
                    let byte_pos = line_col_to_byte(&nb, line_1b, col_1b);
                    let line_str = nb.nth_line(line_1b.saturating_sub(1)).unwrap_or_default();
                    let col_idx = col_1b.saturating_sub(1);
                    if let Some(c) = line_str.chars().nth(col_idx - 1) {
                        let range = byte_pos - c.len_utf8()..byte_pos;
                        nb.typing_delete(range.clone(), false, now_ms);
                        shift_markers(&old_buf, &brouillon::contract::EditSet(vec![brouillon::contract::TextEdit::delete(range.start, range.end)]));
                        buffer_sig.set(Arc::new(nb));
                        cursor_sig.set((line_1b, col_1b - 1));
                    }
                } else if line_1b > 1 {
                    let prev_line_str = nb.nth_line(line_1b - 2).unwrap_or_default();
                    let prev_line_len = prev_line_str.trim_end_matches(&['\r', '\n']).chars().count();
                    let cur_start = nb.byte_of_line(line_1b - 1);
                    let tail = nb.slice(cur_start.saturating_sub(2)..cur_start);
                    let eol_len = if tail == "\r\n" { 2 } else { 1 };
                    let range = cur_start - eol_len..cur_start;
                    nb.typing_delete(range.clone(), false, now_ms);
                    shift_markers(&old_buf, &brouillon::contract::EditSet(vec![brouillon::contract::TextEdit::delete(range.start, range.end)]));
                    buffer_sig.set(Arc::new(nb));
                    cursor_sig.set((line_1b - 1, prev_line_len + 1));
                }
                selection_sig.set(Selection::new((1, 1), (1, 1), false));
                return;
            }

            // ── Standard character insertion (AltGr arrives as Ctrl+Alt on Windows) ──
            if let Some(ref s) = typed_text(&ke.key.logical_key) {
                let altgr = ke.modifiers.control() && ke.modifiers.alt();
                if (!ke.modifiers.control() && !ke.modifiers.alt()) || altgr {
                    let sel = selection_sig.get_untracked();
                    if !sel.is_empty() || overwrite_sig.get_untracked() {
                        let buf = buffer_sig.get_untracked();
                        let regions = brouillon::wiring::regions_from_buf(&buf, cursor_sig.get_untracked(), Some((sel.anchor, sel.head)));
                        let (edits, after) = if overwrite_sig.get_untracked() && sel.is_empty() {
                            let win = brouillon::wiring::Window::around(&buf, regions[0].head);
                            let (e, a) = brouillon::cursor::overwrite_text(&win.text, &win.rel(&regions), s);
                            (win.abs_edits(e), win.abs_regions(a))
                        } else {
                            brouillon::cursor::insert_text(&regions, s)
                        };
                        let out = brouillon::contract::CmdOutcome { edits, selection: Some(after), ..Default::default() };
                        apply_ui_outcome(out, regions);
                        return;
                    }
                    let buf = buffer_sig.get_untracked();
                    let mut nb = (*buf).clone();
                    let byte_pos = line_col_to_byte(&nb, line_1b, col_1b);
                    nb.typing_insert(byte_pos, s, now_ms);
                    shift_markers(&buf, &brouillon::contract::EditSet(vec![brouillon::contract::TextEdit::insert(byte_pos, s.to_string())]));
                    buffer_sig.set(Arc::new(nb));
                    cursor_sig.set((line_1b, col_1b + s.chars().count()));
                    selection_sig.set(Selection::new((1, 1), (1, 1), false));
                    return;
                }
            }

            let moved = handle_cursor_key_folded(&ke.key.logical_key, cursor_sig, &buffer_sig.get_untracked(), &fold_state_sig.get_untracked());
            if moved {
                selection_sig.set(Selection::new((1, 1), (1, 1), false));
            }
        }
    } })
    .on_event_stop(EventListener::KeyUp, move |event| {
        tick_frame();
        if let Event::KeyUp(ke) = event {
            // Bare F11 only: Ctrl+F11 is Go to Function (Classic keymap) and must not open the inspector.
            if ke.key.logical_key == Key::Named(NamedKey::F11) && !ke.modifiers.control() && !ke.modifiers.alt() && !ke.modifiers.shift() {
                floem::action::inspect();
            }
            // A focused text field swallows Escape (it only drops the focus): close the floating
            // panels here, or they stay on screen with the keyboard going to the document.
            if ke.key.logical_key == Key::Named(NamedKey::Escape) {
                quick_open_state.close();
                palette_for_keyup.close();
                goto_visible.set(false);
                encoding_picker_visible.set(false);
            }
            if ke.modifiers.control() && !ctrl_q_is_bound && ke.key.logical_key == Key::Character("q".into()) && try_quit_keys() {
                tabs_sig.with_untracked(persist_unsaved_on_exit);
                std::process::exit(0);
            }
        }
    })
    .on_event_cont(EventListener::WindowClosed, move |_| {
        tabs_sig.with_untracked(persist_unsaved_on_exit);
        // Close the Link socket and remove its instance file (no-op when Agents were never on).
        link_rt_close.stop();
    })
    // Leaving the window (Alt+Tab, click on another app) closes an open menu popup.
    .on_event_cont(EventListener::WindowLostFocus, move |_| {
        active_menu_dropdown.set(None);
    })
    // Mouse activity re-arms the idle memory trimmer (key activity does it in tick_frame()).
    .on_event_cont(EventListener::PointerMove, |_| brouillon::memtrim::touch())
    .on_event_cont(EventListener::PointerDown, |_| brouillon::memtrim::touch())
    .on_event_cont(EventListener::PointerWheel, |_| brouillon::memtrim::touch())
}


/// Top header — brand stripe (theme accent), theme name, live FPS readout.
/// Ctrl+digit chords follow the PHYSICAL top-row key: on AZERTY the unshifted key
/// produces `&` `é` `"` ... so the logical key never matched Ctrl+1..0 (user tools).
/// AltGr (Ctrl+Alt on Windows) is left alone: it types characters. A real Ctrl+Alt
/// (left Alt, `altgr == false`) is a chord, so Ctrl+Alt+1..0 reach the clip-text slots.
fn digit_chord_name(physical: &PhysicalKey, ctrl: bool, altgr: bool) -> Option<String> {
    if !ctrl || altgr {
        return None;
    }
    let PhysicalKey::Code(code) = physical else { return None };
    let d = match code {
        KeyCode::Digit1 => "1",
        KeyCode::Digit2 => "2",
        KeyCode::Digit3 => "3",
        KeyCode::Digit4 => "4",
        KeyCode::Digit5 => "5",
        KeyCode::Digit6 => "6",
        KeyCode::Digit7 => "7",
        KeyCode::Digit8 => "8",
        KeyCode::Digit9 => "9",
        KeyCode::Digit0 => "0",
        _ => return None,
    };
    Some(d.to_string())
}

/// Key name of a chord from a key event. AltGr (Windows reports it as Ctrl+Alt, but floem
/// flags `ALTGR` from the AltGraph key) never forms a letter chord: it types characters.
/// A real Ctrl+Alt+letter has NO character on Windows (the layout is queried with AltGr
/// state, e.g. AZERTY has nothing on AltGr+A/S) so winit reports `Unidentified(Windows(vk))`;
/// `key_name` maps that VK back to the letter.
fn chord_key_name(logical: &Key, physical: &PhysicalKey, ctrl: bool, altgr: bool) -> Option<String> {
    if altgr {
        return if matches!(logical, Key::Unidentified(_)) { None } else { key_name(logical) };
    }
    digit_chord_name(physical, ctrl, altgr).or_else(|| key_name(logical))
}

/// Key name used by chords ("f3", "left", "pagedown", "a"...). `None` for keys that never form a chord.
fn key_name(key: &Key) -> Option<String> {
    let named = match key {
        Key::Character(c) => return Some(c.to_lowercase()),
        // Windows virtual-key code of a letter (0x41..=0x5A): layout-aware, unlike the physical key.
        Key::Unidentified(floem::keyboard::NativeKey::Windows(vk)) if (0x41..=0x5A).contains(vk) => {
            return Some(((*vk as u8 + 32) as char).to_string());
        }
        Key::Named(n) => match n {
            NamedKey::ArrowLeft => "left",
            NamedKey::ArrowRight => "right",
            NamedKey::ArrowUp => "up",
            NamedKey::ArrowDown => "down",
            NamedKey::Enter => "enter",
            NamedKey::Tab => "tab",
            NamedKey::Escape => "escape",
            NamedKey::Backspace => "backspace",
            NamedKey::Delete => "delete",
            NamedKey::Insert => "insert",
            NamedKey::Home => "home",
            NamedKey::End => "end",
            NamedKey::PageUp => "pageup",
            NamedKey::PageDown => "pagedown",
            NamedKey::Space => "space",
            NamedKey::F1 => "f1",
            NamedKey::F2 => "f2",
            NamedKey::F3 => "f3",
            NamedKey::F4 => "f4",
            NamedKey::F5 => "f5",
            NamedKey::F6 => "f6",
            NamedKey::F7 => "f7",
            NamedKey::F8 => "f8",
            NamedKey::F9 => "f9",
            NamedKey::F10 => "f10",
            NamedKey::F11 => "f11",
            NamedKey::F12 => "f12",
            _ => return None,
        },
        _ => return None,
    };
    Some(named.to_string())
}

/// Wall-clock milliseconds, the clock fed to typing coalescing and chord timeouts.
fn now_millis() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as u64)
}

fn header_bar(fps: RwSignal<f64>, theme_idx: RwSignal<usize>, tools: impl IntoView + 'static, link_chip: impl IntoView + 'static) -> impl IntoView {
    stack((
        // " brouillon " + reactive theme name. Color comes from the active
        // theme accent — cycling theme (Ctrl-T) repaints both label and
        // text.
        label(move || {
            let t = current_theme(theme_idx.get());
            format!(" brouillon · {} ", t.name)
        })
        .style(move |s| {
            let t = current_theme(theme_idx.get());
            s.padding_horiz(8.0).color(t.accent)
        }),
        // Day-4: idle state. `current_fps()` returns 0.0 when the last tick is
        // older than 750ms (see metrics::FPS_IDLE_AFTER). UI shows "—" then so
        // users don't misread "0.0 fps" as a paint stall.
        label(move || {
            let v = fps.get();
            if v <= 0.0 { "    — fps ".to_string() } else { format!(" {:>5.1} fps ", v) }
        })
        .style(move |s| {
            let t = current_theme(theme_idx.get());
            s.color(t.fps_ok)
        }),
        // Undo / Redo / Clavier / Crayon: right after the fps counter.
        tools,
        floem::views::empty().style(|s| s.flex_grow(1.0)),
        link_chip,
        brouillon::render_badge::render_badge(
            brouillon::render_mode::effective_render_mode(),
            startup().paths.settings_file().display().to_string(),
            theme_idx,
        ),
    ))
    .style(move |s| {
        let t = current_theme(theme_idx.get());
        s.flex_row()
            .items_center()
            .height(24.0)
            .width_full()
            .padding_horiz(6.0)
            .background(t.bg_chrome)
            .border_bottom(1.0)
            .border_color(t.border)
    })
}


/// Bottom status line: line/col, total lines, file size, live RAM, preset, path.
/// alpha.4 reads totals reactively from `buffer_sig` so swaps refresh the bar.
fn status_line(
    _total_bytes_init: usize,
    panes: RwSignal<Vec<brouillon::status_bar::Pane>>,
    message: RwSignal<String>,
    ram: RwSignal<String>,
    ram_detail: RwSignal<String>,
    preset_sig: RwSignal<Preset>,
    theme_idx: RwSignal<usize>,
    path_sig: RwSignal<Option<PathBuf>>,
    buffer_sig: RwSignal<Arc<Buffer>>,
    bracket_paint_sig: RwSignal<BracketPaint>,
    language_sig: RwSignal<brouillon::grammars::LanguageId>,
    on_command: impl Fn(&'static str) + 'static + Clone,
) -> impl IntoView {
    stack((
        // BK-011: Ln/Col, lines, char code, EOL, encoding, BOM, INS/OVR, READ, LARGE.
        brouillon::status_bar::status_bar_view(panes, theme_idx, on_command),
        label(move || format!(" | {} ", format_bytes(buffer_sig.get().byte_len()))),
        // Working set is what Task Manager shows; the tooltip also gives the private bytes (the real footprint).
        label(move || format!(" | RAM {} ", ram.get())).tooltip(move || label(move || ram_detail.get())),
        // alpha.3+ : preset readout (Ctrl-Shift-M flips).
        label(move || format!(" | mode: {} ", preset_sig.get().name())),
        // alpha.13 : bracket-pair indicator.
        label(move || {
            match bracket_paint_sig.get() {
                BracketPaint::Matched { pair_byte, .. } => format!(" | <...> @ b{} ", pair_byte),
                BracketPaint::Unmatched { .. } => " | < x ".to_string(),
                BracketPaint::None => String::new(),
            }
        }),
        // alpha.4 : current file path + dirty indicator.
        label(move || {
            let dirty = buffer_sig.get().is_dirty();
            let dirty_glyph = if dirty { "\u{2022}" } else { " " };
            match path_sig.get() {
                Some(p) => {
                    let name = p.file_name().and_then(|s| s.to_str()).unwrap_or("?");
                    format!(" | {dirty_glyph} {name} ")
                }
                None => format!(" | {dirty_glyph} <untitled> "),
            }
        }),
        // M2: language indicator
        label(move || {
            format!(" | {} ", language_sig.get().display_name())
        }),
        // Last command message (search wrapped, "3 replaced", save result...).
        label(move || {
            let m = message.get();
            if m.is_empty() { String::new() } else { format!(" | {m} ") }
        }),
    ))
    .style(move |s| {
        let t = current_theme(theme_idx.get());
        s.flex_row()
            .items_center()
            .height(22.0)
            .width_full()
            .padding_horiz(8.0)
            .background(t.bg_chrome)
            .border_top(1.0)
            .border_color(t.border)
            .color(t.fg_status)
            .font_size(12.0)
    })
}

fn format_bytes(b: usize) -> String {
    let bf = b as f64;
    if b < 1024 {
        format!("{b} B")
    } else if b < 1024 * 1024 {
        format!("{:.1} KB", bf / 1024.0)
    } else {
        format!("{:.1} MB", bf / 1024.0 / 1024.0)
    }
}

/// Spawn a background sampler that publishes FPS and RAM into the GUI
/// signals every 500 ms.
///
/// Sentinel issue #1 (alpha.3 QA): the previous implementation called
/// `RwSignal::set` directly from a non-GUI thread, which Floem 0.2 does
/// not guarantee safe. Closed in alpha.5 via
/// `ext_event_helpers::spawn_periodic`, which routes values through an
/// `mpsc` channel and a `Trigger` so only the GUI thread ever touches
/// the signals. See module docs in `ext_event_helpers.rs` for the
/// design.
fn spawn_metrics_pump(fps: RwSignal<f64>, ram: RwSignal<String>, ram_detail: RwSignal<String>) {
    let sampler = Arc::new(Mutex::new(RamSampler::new()));
    brouillon::ext_event_helpers::spawn_periodic(
        Scope::current(),
        Duration::from_millis(500),
        move || {
            let cur_fps = current_fps();
            let m = brouillon::memtrim::mem_stats();
            if m.working_set > 0 {
                let detail = format!(
                    "Working set {} (resident, what Task Manager shows)
Private bytes {} (committed, the real footprint)
Freed pages are handed back after a quiet period ([perf] idle_trim)",
                    brouillon::memtrim::format_mb(m.working_set),
                    brouillon::memtrim::format_mb(m.private_bytes),
                );
                (cur_fps, brouillon::memtrim::format_mb(m.working_set), detail, m.private_bytes)
            } else {
                let rss = sampler.lock().ok().map(|mut s| s.rss_bytes()).unwrap_or(0);
                (cur_fps, format!("{:.1} MB", rss as f64 / 1048576.0), format!("Resident set {:.1} MB", rss as f64 / 1048576.0), rss)
            }
        },
        {
            // Second trigger, same trim: after a big transient (open, reload, replace all, diff) memory grew and
            // then stood still, even if the user keeps working. Gated by [perf] idle_trim like the idle trigger.
            let growth = std::cell::RefCell::new(brouillon::memtrim::GrowthTrim::default());
            move |(cur_fps, cur_ram, detail, private): (f64, String, String, u64)| {
                fps.set(cur_fps);
                ram.set(cur_ram);
                ram_detail.set(detail);
                if private > 0 && brouillon::memtrim::enabled() && growth.borrow_mut().tick(private) {
                    brouillon::memtrim::trim_now(false);
                }
            }
        },
    );
}

/// Text a plain key press types. winit reports the space bar as `Named(Space)`, not as a
/// character, so matching only `Key::Character` silently dropped every space.
fn typed_text(key: &Key) -> Option<String> {
    match key {
        Key::Character(s) => Some(s.to_string()),
        Key::Named(NamedKey::Space) => Some(" ".to_string()),
        _ => None,
    }
}

// Style helper alias kept around in case day-2 needs a custom style fn type.
#[allow(dead_code)]
fn _style_helper_marker(_s: Style) {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn space_bar_types_a_space() {
        assert_eq!(typed_text(&Key::Named(NamedKey::Space)).as_deref(), Some(" "));
        assert_eq!(typed_text(&Key::Character("e".into())).as_deref(), Some("e"));
        assert_eq!(typed_text(&Key::Named(NamedKey::Enter)), None);
    }

    #[test]
    fn ctrl_digit_follows_the_physical_key() {
        let one = PhysicalKey::Code(KeyCode::Digit1);
        assert_eq!(digit_chord_name(&one, true, false).as_deref(), Some("1"));
        assert_eq!(digit_chord_name(&one, false, false), None, "no Ctrl: plain typing");
        assert_eq!(digit_chord_name(&one, true, true), None, "AltGr types characters");
        assert_eq!(digit_chord_name(&PhysicalKey::Code(KeyCode::KeyA), true, false), None);
    }

    /// CPU first, GPU optional: the default must stay the driver-free renderer
    /// (the 80 MB budget is only reachable without a GPU backend).
    #[test]
    fn render_backend_defaults_to_cpu() {
        assert_eq!(DEFAULT_RENDER_BACKEND, "cpu");
    }

    #[test]
    fn ctrl_alt_letter_is_a_chord_on_azerty_but_altgr_is_not() {
        use floem::keyboard::NativeKey;
        // Traced on AZERTY: Ctrl+Alt+A -> Unidentified(Windows(0x41)) on the physical Q key.
        let a = Key::Unidentified(NativeKey::Windows(0x41));
        let q_pos = PhysicalKey::Code(KeyCode::KeyQ);
        assert_eq!(chord_key_name(&a, &q_pos, true, false).as_deref(), Some("a"));
        let s = Key::Unidentified(NativeKey::Windows(0x53));
        assert_eq!(chord_key_name(&s, &PhysicalKey::Code(KeyCode::KeyS), true, false).as_deref(), Some("s"));
        // AltGr + a key with no character is not a chord; AltGr + a character types it.
        assert_eq!(chord_key_name(&a, &q_pos, true, true), None);
        let euro = Key::Character("\u{20ac}".into());
        assert_eq!(chord_key_name(&euro, &PhysicalKey::Code(KeyCode::KeyE), true, true).as_deref(), Some("\u{20ac}"));
        // Ctrl+Alt+digit follows the physical digit key (AZERTY digits need Shift).
        let one = PhysicalKey::Code(KeyCode::Digit1);
        assert_eq!(chord_key_name(&Key::Character("&".into()), &one, true, false).as_deref(), Some("1"));
        // Non-letter virtual keys never form a chord.
        assert_eq!(key_name(&Key::Unidentified(NativeKey::Windows(0x20))), None);
    }
}
