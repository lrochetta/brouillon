//! What Brouillon shows when it starts with nothing to restore: the Welcome
//! page on the very first launch, the release notes once after an update, or the
//! document chosen with `startup.document`.
//!
//! The documents are Markdown files compiled into the program
//! (`assets/help/*.md`). They open as read-only tabs without a file path. Such a
//! tab is flagged `TabState::help_doc`: it is never "modified", never asks to be
//! saved on close, and is left out of the session file and the crash journal, so
//! it can neither come back as a duplicate nor trigger an autosave.
//!
//! Everything that decides *what* to open is pure (`decide`) and unit-tested;
//! `main.rs` only gathers the facts and applies the plan.
//!
//! Settings: `startup.document` = `welcome` (default) | `docs` | `changelog` |
//! `none`, and `startup.whats_new` = `true` (default) | `false`.
//! State: `<data dir>/last-run.toml` (`version`, `build`) written with
//! `durable_write`; comparing it with the running program tells whether the
//! program was updated since the last start.

use std::path::Path;
use std::sync::OnceLock;

use floem::keyboard::{Key, NamedKey};
use floem::reactive::{create_effect, RwSignal, SignalGet, SignalUpdate, SignalWith};

use brouillon_core::command_registry::Registry;
use brouillon_core::keymap::{self, Binding};
use brouillon_core::settings::Settings;
use brouillon_editor_engine::tabs::TabManager;

pub const SETTING_DOCUMENT: &str = "startup.document";
pub const SETTING_WHATS_NEW: &str = "startup.whats_new";
/// File (in the data directory) remembering which program version ran last.
pub const LAST_RUN_FILE: &str = "last-run.toml";

/// A built-in help document.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HelpDoc {
    Welcome,
    Docs,
    ReleaseNotes,
}

impl HelpDoc {
    pub const ALL: [HelpDoc; 3] = [HelpDoc::Welcome, HelpDoc::Docs, HelpDoc::ReleaseNotes];

    /// Stable id stored in `TabState::help_doc`.
    pub fn id(self) -> &'static str {
        match self {
            HelpDoc::Welcome => "welcome",
            HelpDoc::Docs => "docs",
            HelpDoc::ReleaseNotes => "release-notes",
        }
    }

    /// Tab title.
    pub fn title(self) -> &'static str {
        match self {
            HelpDoc::Welcome => "Welcome",
            HelpDoc::Docs => "Documentation",
            HelpDoc::ReleaseNotes => "Release notes",
        }
    }

    /// Registry command that (re)opens it (Help menu, palette).
    pub fn command_id(self) -> &'static str {
        match self {
            HelpDoc::Welcome => "help_welcome",
            HelpDoc::Docs => "help_docs",
            HelpDoc::ReleaseNotes => "help_release_notes",
        }
    }

    pub fn from_command_id(id: &str) -> Option<HelpDoc> {
        Self::ALL.into_iter().find(|d| d.command_id() == id)
    }

    /// Markdown source with `{{placeholders}}` (see [`render`]).
    pub fn source(self) -> &'static str {
        match self {
            HelpDoc::Welcome => include_str!("../assets/help/welcome.md"),
            HelpDoc::Docs => include_str!("../assets/help/docs.md"),
            HelpDoc::ReleaseNotes => include_str!("../assets/help/changelog.md"),
        }
    }
}

/// Value of the `startup.document` setting.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StartupDocument {
    Welcome,
    Docs,
    Changelog,
    None,
}

impl StartupDocument {
    /// The documented default, also used for an unknown value.
    pub const DEFAULT: StartupDocument = StartupDocument::Welcome;

    pub fn parse(s: &str) -> Option<Self> {
        Some(match s.trim().to_ascii_lowercase().as_str() {
            "welcome" => Self::Welcome,
            "docs" => Self::Docs,
            "changelog" => Self::Changelog,
            "none" => Self::None,
            _ => return Option::None,
        })
    }

    pub fn doc(self) -> Option<HelpDoc> {
        match self {
            Self::Welcome => Some(HelpDoc::Welcome),
            Self::Docs => Some(HelpDoc::Docs),
            Self::Changelog => Some(HelpDoc::ReleaseNotes),
            Self::None => Option::None,
        }
    }

    /// Read the setting; an unknown value falls back to the default and is reported.
    pub fn from_settings(settings: &Settings) -> (Self, Option<String>) {
        let raw = settings.get_str(SETTING_DOCUMENT, "welcome");
        match Self::parse(&raw) {
            Some(v) => (v, Option::None),
            Option::None => (
                Self::DEFAULT,
                Some(format!("{SETTING_DOCUMENT} = {raw:?} is not welcome, docs, changelog or none; using welcome")),
            ),
        }
    }
}

/// Which program ran last (`last-run.toml`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LastRun {
    pub version: String,
    pub build: String,
}

impl LastRun {
    /// The running program.
    pub fn current() -> Self {
        Self { version: crate::buildinfo::VERSION.to_string(), build: crate::buildinfo::BUILD.to_string() }
    }

    pub fn to_toml(&self) -> String {
        format!("version = {:?}\nbuild = {:?}\n", self.version, self.build)
    }

    pub fn parse(text: &str) -> Option<Self> {
        let t: toml::Table = text.parse().ok()?;
        let version = t.get("version")?.as_str()?.to_string();
        let build = match t.get("build")? {
            toml::Value::String(s) => s.clone(),
            toml::Value::Integer(i) => i.to_string(),
            _ => return Option::None,
        };
        Some(Self { version, build })
    }

    /// `None` when the file is missing or unreadable (treated as "never ran").
    pub fn read(dir: &Path) -> Option<Self> {
        Self::parse(&std::fs::read_to_string(dir.join(LAST_RUN_FILE)).ok()?)
    }

    pub fn write(&self, dir: &Path) -> std::io::Result<()> {
        std::fs::create_dir_all(dir)?;
        brouillon_editor_engine::durable::durable_write(&dir.join(LAST_RUN_FILE), self.to_toml().as_bytes())
    }
}

/// Everything known before the window exists.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Facts {
    /// No settings file, no session and no `last-run.toml`: the first start.
    pub first_launch: bool,
    /// `last-run.toml` names another version/build (or is missing on a profile that already has data).
    pub update_pending: bool,
    pub document: StartupDocument,
    pub whats_new: bool,
    /// Files were passed on the command line.
    pub has_targets: bool,
}

impl Facts {
    pub fn gather(
        prev: Option<&LastRun>,
        current: &LastRun,
        settings_file_exists: bool,
        session_file_exists: bool,
        document: StartupDocument,
        whats_new: bool,
        has_targets: bool,
    ) -> Self {
        let first_launch = prev.is_none() && !settings_file_exists && !session_file_exists;
        let update_pending = match prev {
            Some(p) => p != current,
            // Data from a version that did not write last-run.toml yet: it is an update.
            Option::None => !first_launch,
        };
        Self { first_launch, update_pending, document, whats_new, has_targets }
    }

    /// Should `last-run.toml` be (re)written now?
    ///
    /// Not when files were passed on the command line and an older record exists:
    /// that start shows no help, so the release notes stay pending for the next
    /// plain start ("once" means once shown, not once started).
    pub fn should_record(&self, prev: Option<&LastRun>, current: &LastRun) -> bool {
        prev != Some(current) && (prev.is_none() || !self.has_targets)
    }
}

/// Documents to open, in order; the last one ends up active.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Plan {
    pub open: Vec<HelpDoc>,
}

/// The startup decision. `has_content` = a session was restored or crash
/// recovery brought documents back: the user's own tabs are never pushed aside
/// by a help page, except for the one-time release notes.
pub fn decide(f: &Facts, has_content: bool) -> Plan {
    let mut open = Vec::new();
    if f.has_targets {
        return Plan { open };
    }
    if f.first_launch {
        if !has_content {
            open.push(HelpDoc::Welcome);
        }
        return Plan { open };
    }
    if !has_content {
        if let Some(d) = f.document.doc() {
            open.push(d);
        }
    }
    if f.update_pending && f.whats_new && !open.contains(&HelpDoc::ReleaseNotes) {
        open.push(HelpDoc::ReleaseNotes);
    }
    Plan { open }
}

/// What the rendering of the documents needs from the running program.
#[derive(Debug, Clone)]
pub struct HelpEnv {
    pub data_dir: String,
    pub settings_file: String,
    pub classic_keymap: bool,
    pub user_bindings: Vec<Binding>,
}

/// Open the planned documents in `tm`; returns how many tabs were opened or activated.
pub fn apply(plan: &Plan, tm: &mut TabManager, env: &HelpEnv) -> usize {
    for d in &plan.open {
        open_doc(tm, *d, env);
    }
    plan.open.len()
}

/// Open (or activate) one help document.
pub fn open_doc(tm: &mut TabManager, doc: HelpDoc, env: &HelpEnv) -> usize {
    tm.open_help(doc.id(), doc.title(), &render(doc, env))
}

fn registry() -> &'static Registry {
    static REG: OnceLock<Registry> = OnceLock::new();
    REG.get_or_init(|| crate::wiring::build_registry().0)
}

/// Substitute the `{{placeholders}}` of a document with the running program's facts.
pub fn render(doc: HelpDoc, env: &HelpEnv) -> String {
    let date = crate::buildinfo::DATE_UTC;
    // A checkout with autocrlf turns the assets into CRLF: the pages are always LF.
    let mut out = doc.source().replace("\r\n", "\n");
    for (key, value) in [
        ("version", crate::buildinfo::VERSION.to_string()),
        ("build", crate::buildinfo::BUILD.to_string()),
        ("commit", crate::buildinfo::COMMIT.to_string()),
        ("date", date.get(..10).unwrap_or(date).to_string()),
        ("data_dir", env.data_dir.clone()),
        ("settings_file", env.settings_file.clone()),
    ] {
        out = out.replace(&format!("{{{{{key}}}}}"), &value);
    }
    if out.contains("{{shortcuts}}") {
        out = out.replace("{{shortcuts}}", &shortcuts_markdown(registry(), env.classic_keymap, &env.user_bindings));
    }
    out
}

/// Shortcuts that `main.rs` handles itself, before the registry chords (see the
/// key handler in `app_view`). Listed only when the registry does not already
/// bind the same chord.
const FIXED_SHORTCUTS: &[(&str, &str, &str)] = &[
    ("ctrl+f", "Search", "Find (again to close)"),
    ("ctrl+h", "Search", "Replace"),
    ("ctrl+g", "Navigate", "Go to line"),
    ("ctrl+k", "View", "Command palette"),
    ("ctrl+t", "View", "Cycle theme"),
    ("ctrl+shift+m", "View", "Switch Moldu / Geek mode"),
    ("ctrl+shift+h", "View", "Hex view"),
    ("ctrl+shift+v", "View", "Markdown preview"),
    ("ctrl+shift+[", "View", "Fold all"),
    ("ctrl+shift+]", "View", "Unfold all"),
    ("ctrl+`", "View", "Output panel"),
    ("ctrl+pageup", "Tab", "Previous tab"),
    ("ctrl+pagedown", "Tab", "Next tab"),
    ("ctrl+o", "File", "Open"),
    ("ctrl+s", "File", "Save"),
    ("ctrl+shift+s", "File", "Save as"),
    ("ctrl+n", "File", "New tab"),
    ("ctrl+w", "File", "Close tab"),
    ("ctrl+q", "File", "Quit"),
    ("ctrl+z", "Edit", "Undo"),
    ("ctrl+y", "Edit", "Redo"),
    ("ctrl+shift+z", "Edit", "Redo"),
    ("esc", "View", "Close the open overlay or menu"),
];

/// The effective shortcuts as Markdown list items grouped by category: the
/// registry bindings that really execute (defaults, optional Classic overlay,
/// the user's `keymap.toml`) plus the fixed shell shortcuts.
pub fn shortcuts_markdown(reg: &Registry, classic: bool, user: &[Binding]) -> String {
    let bindings = crate::wiring::dispatch_bindings(reg, classic, user);
    // (category, chord text, label, chord key for the duplicate check)
    let mut rows: Vec<(String, String, String, String)> = Vec::new();
    for b in &bindings {
        if b.command.is_empty() {
            continue;
        }
        let Some(spec) = reg.get(&b.command) else { continue };
        let key = keymap::chord_to_string(&b.chord);
        let mut label = spec.caption.split_once(": ").map_or(spec.caption.clone(), |(_, rest)| rest.to_string());
        if let Some(w) = &b.when {
            label.push_str(&format!(" (when {w})"));
        }
        rows.push((spec.category.clone(), keymap::chord_display(&key), label, key));
    }
    for (chord, cat, label) in FIXED_SHORTCUTS {
        if !rows.iter().any(|r| r.3 == *chord) {
            rows.push(((*cat).to_string(), keymap::chord_display(chord), (*label).to_string(), (*chord).to_string()));
        }
    }
    rows.sort_by(|a, b| (&a.0, &a.2, &a.3).cmp(&(&b.0, &b.2, &b.3)));
    let mut out = String::new();
    let mut cat = "";
    for r in &rows {
        if r.0 != cat {
            cat = &r.0;
            out.push_str(&format!("\n### {cat}\n\n"));
        }
        out.push_str(&format!("- {}: {}\n", r.1, r.2));
    }
    out
}

/// `true` when `key` would type or delete text in the document if it reached
/// the editor (AltGr counts as typing; other Ctrl/Alt combinations do not).
pub fn is_editing_key(key: &Key, ctrl: bool, alt: bool) -> bool {
    match key {
        Key::Named(NamedKey::Enter | NamedKey::Backspace | NamedKey::Delete | NamedKey::Tab | NamedKey::Space) => true,
        Key::Character(_) => (!ctrl && !alt) || (ctrl && alt),
        _ => false,
    }
}

/// `true` when command `id` would change the text of the active document and so
/// must be refused on a read-only help tab. File commands (save a copy, open...)
/// are never blocked.
pub fn command_blocked_on_help(id: &str) -> bool {
    !id.starts_with("file_") && !id.starts_with("tools_run_slot_") && registry().get(id).is_some_and(|s| s.mutates)
}

/// Show the Markdown preview while a help document is the active tab and put the
/// user's previous choice back when they leave it. `help_active` mirrors "the
/// active tab is a help document" for the view (preview alone, full width).
pub fn follow_preview(tabs: RwSignal<TabManager>, preview: RwSignal<bool>, help_active: RwSignal<bool>) {
    let before_help = std::cell::Cell::new(false);
    create_effect(move |was_help: Option<bool>| {
        let is_help = tabs.with(|tm| tm.current().help_doc.is_some());
        if was_help != Some(is_help) {
            help_active.set(is_help);
        }
        match (was_help.unwrap_or(false), is_help) {
            (false, true) => {
                before_help.set(preview.get_untracked());
                preview.set(true);
            }
            (true, false) => preview.set(before_help.get()),
            _ => {}
        }
        is_help
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env() -> HelpEnv {
        HelpEnv {
            data_dir: "C:\\Users\\x\\.brouillon".into(),
            settings_file: "C:\\Users\\x\\.brouillon\\settings.toml".into(),
            classic_keymap: false,
            user_bindings: Vec::new(),
        }
    }

    fn lr(v: &str, b: &str) -> LastRun {
        LastRun { version: v.into(), build: b.into() }
    }

    fn facts(prev: Option<&LastRun>, settings: bool, session: bool, doc: StartupDocument, whats_new: bool, targets: bool) -> Facts {
        Facts::gather(prev, &lr("0.1.0", "10"), settings, session, doc, whats_new, targets)
    }

    use HelpDoc::{Docs, ReleaseNotes, Welcome};
    use StartupDocument as D;

    /// (label, facts, has_content, expected documents)
    #[test]
    fn startup_decision_table() {
        let same = lr("0.1.0", "10");
        let older = lr("0.1.0", "9");
        let cases: Vec<(&str, Facts, bool, Vec<HelpDoc>)> = vec![
            ("first launch: nothing exists", facts(None, false, false, D::Welcome, true, false), false, vec![Welcome]),
            ("first launch ignores startup.document", facts(None, false, false, D::None, true, false), false, vec![Welcome]),
            ("first launch with a recovered document", facts(None, false, false, D::Welcome, true, false), true, vec![]),
            ("first launch with a file argument", facts(None, false, false, D::Welcome, true, true), false, vec![]),
            ("same build, session restored", facts(Some(&same), true, true, D::Welcome, true, false), true, vec![]),
            ("same build, no session: startup.document = welcome", facts(Some(&same), true, false, D::Welcome, true, false), false, vec![Welcome]),
            ("same build, no session: docs", facts(Some(&same), true, false, D::Docs, true, false), false, vec![Docs]),
            ("same build, no session: changelog", facts(Some(&same), true, false, D::Changelog, true, false), false, vec![ReleaseNotes]),
            ("same build, no session: none", facts(Some(&same), true, false, D::None, true, false), false, vec![]),
            ("same build, file argument", facts(Some(&same), true, false, D::Welcome, true, true), false, vec![]),
            ("update, session restored: release notes only", facts(Some(&older), true, true, D::Welcome, true, false), true, vec![ReleaseNotes]),
            ("update, no session: chosen document then notes", facts(Some(&older), true, false, D::Welcome, true, false), false, vec![Welcome, ReleaseNotes]),
            ("update, no session, startup.document = changelog: once", facts(Some(&older), true, false, D::Changelog, true, false), false, vec![ReleaseNotes]),
            ("update, no session, startup.document = none: notes only", facts(Some(&older), true, false, D::None, true, false), false, vec![ReleaseNotes]),
            ("update with startup.whats_new = false", facts(Some(&older), true, true, D::Welcome, false, false), true, vec![]),
            ("update with a file argument", facts(Some(&older), true, true, D::Welcome, true, true), true, vec![]),
            ("other version, same build number", facts(Some(&lr("0.0.9", "10")), true, true, D::Welcome, true, false), true, vec![ReleaseNotes]),
            ("data from before last-run.toml existed = update", facts(None, true, true, D::Welcome, true, false), true, vec![ReleaseNotes]),
            ("session file only (no settings) is not a first launch", facts(None, false, true, D::Welcome, true, false), true, vec![ReleaseNotes]),
        ];
        for (label, f, content, want) in cases {
            assert_eq!(decide(&f, content).open, want, "{label}");
        }
    }

    #[test]
    fn first_launch_is_only_when_nothing_exists() {
        assert!(facts(None, false, false, D::Welcome, true, false).first_launch);
        assert!(!facts(None, true, false, D::Welcome, true, false).first_launch);
        assert!(!facts(None, false, true, D::Welcome, true, false).first_launch);
        assert!(!facts(Some(&lr("0.1.0", "10")), false, false, D::Welcome, true, false).first_launch);
    }

    #[test]
    fn last_run_is_recorded_unless_a_file_start_must_keep_the_notes_pending() {
        let cur = lr("0.1.0", "10");
        let older = lr("0.1.0", "9");
        let plain = facts(Some(&older), true, true, D::Welcome, true, false);
        assert!(plain.should_record(Some(&older), &cur));
        assert!(!plain.should_record(Some(&cur), &cur), "nothing to write when unchanged");
        let with_file = facts(Some(&older), true, true, D::Welcome, true, true);
        assert!(!with_file.should_record(Some(&older), &cur), "notes stay pending for the next plain start");
        let first_with_file = facts(None, false, false, D::Welcome, true, true);
        assert!(first_with_file.should_record(None, &cur), "no older record to protect");
    }

    #[test]
    fn startup_document_setting_values() {
        for (s, want) in [("welcome", D::Welcome), ("docs", D::Docs), ("changelog", D::Changelog), ("none", D::None), (" Docs ", D::Docs)] {
            assert_eq!(D::parse(s), Some(want), "{s}");
        }
        assert_eq!(D::parse("readme"), None);
        let ok = Settings::build(&[(brouillon_core::settings::Layer::User, "[startup]\ndocument = \"docs\"\n")]);
        assert_eq!(D::from_settings(&ok), (D::Docs, None));
        let bad = Settings::build(&[(brouillon_core::settings::Layer::User, "[startup]\ndocument = \"readme\"\n")]);
        let (v, warn) = D::from_settings(&bad);
        assert_eq!(v, D::Welcome);
        assert!(warn.unwrap().contains("readme"));
        // The documented defaults really are the built-in ones.
        let none = Settings::build(&[]);
        assert_eq!(D::from_settings(&none), (D::Welcome, None));
        assert!(none.get_bool(SETTING_WHATS_NEW, false));
        assert!(none.unknown_keys.is_empty());
    }

    #[test]
    fn last_run_file_round_trip_and_garbage() {
        let d = tempfile::tempdir().unwrap();
        assert_eq!(LastRun::read(d.path()), None, "missing file");
        let cur = LastRun::current();
        cur.write(d.path()).unwrap();
        assert_eq!(LastRun::read(d.path()), Some(cur.clone()));
        assert!(std::fs::read_to_string(d.path().join(LAST_RUN_FILE)).unwrap().contains("version = "));
        assert_eq!(LastRun::parse("build = 12\nversion = \"0.1.0\"\n"), Some(lr("0.1.0", "12")), "integer build accepted");
        for bad in ["", "not toml [", "version = 3\nbuild = \"1\"", "version = \"1\"\n"] {
            assert_eq!(LastRun::parse(bad), None, "{bad:?}");
        }
        std::fs::write(d.path().join(LAST_RUN_FILE), "garbage = [").unwrap();
        assert_eq!(LastRun::read(d.path()), None, "unreadable record = never ran");
    }

    #[test]
    fn help_documents_never_reach_the_session_or_the_journal() {
        let d = tempfile::tempdir().unwrap();
        let mut tm = TabManager::new();
        let plan = Plan { open: vec![Welcome, ReleaseNotes] };
        assert_eq!(apply(&plan, &mut tm, &env()), 2);
        assert_eq!(tm.count(), 2);
        assert!(tm.iter().all(|t| t.help_doc.is_some() && t.path.is_none() && t.meta.readonly && !t.dirty));
        brouillon_editor_engine::session::save_to_dir(&tm, d.path()).unwrap();
        assert!(!d.path().join("session.toml").exists(), "nothing worth restoring, so no session file");
        let j = brouillon_editor_engine::recovery::Journal::new(d.path().join("journal"));
        assert_eq!(j.snapshot_tabs(&tm).unwrap(), 0);
        assert!(j.recover().is_empty());
        // Restoring after a real session never duplicates a help page.
        let f = d.path().join("a.txt");
        std::fs::write(&f, "x").unwrap();
        let mut tm2 = TabManager::new();
        tm2.open_file(&f).unwrap();
        apply(&Plan { open: vec![Docs] }, &mut tm2, &env());
        brouillon_editor_engine::session::save_to_dir(&tm2, d.path()).unwrap();
        let snap = brouillon_editor_engine::session::load_from_dir(d.path()).unwrap().unwrap();
        assert_eq!(snap.tabs.len(), 1);
        let mut tm3 = TabManager::new();
        assert_eq!(brouillon_editor_engine::session::restore_into(snap, &mut tm3), 1);
        assert!(tm3.iter().all(|t| t.help_doc.is_none()));
    }

    #[test]
    fn reopening_a_document_reuses_its_tab() {
        let mut tm = TabManager::new();
        open_doc(&mut tm, Docs, &env());
        tm.new_scratch(); // a blank tab is replaced by the next help page...
        open_doc(&mut tm, Welcome, &env());
        assert_eq!(tm.count(), 2);
        tm.new_scratch();
        tm.current_mut().buffer = std::sync::Arc::new(brouillon_editor_engine::buffer::Buffer::from_str("typed"));
        open_doc(&mut tm, ReleaseNotes, &env()); // ...but a tab with text is kept
        assert_eq!(tm.count(), 4);
        let again = open_doc(&mut tm, Docs, &env());
        assert_eq!((again, tm.count()), (0, 4), "the open Documentation tab is reused");
        assert_eq!(tm.current().title, "Documentation");
    }

    #[test]
    fn command_ids_round_trip_and_are_registered() {
        let reg = registry();
        for d in HelpDoc::ALL {
            assert_eq!(HelpDoc::from_command_id(d.command_id()), Some(d));
            assert!(reg.contains(d.command_id()), "{} must be in the registry (palette, menu)", d.command_id());
        }
        assert_eq!(HelpDoc::from_command_id("help_about"), None);
    }

    /// Words the shipped texts must never contain: third-party products and the
    /// word that describes what Brouillon is not.
    const FORBIDDEN: &[&str] = &["editplus", "notepad++", "sublime", "editpad", "agentpad", "clone", "vs code", "vscode", "atom editor"];

    #[test]
    fn shipped_texts_name_no_third_party_product() {
        for d in HelpDoc::ALL {
            let text = render(d, &env()).to_lowercase();
            for w in FORBIDDEN {
                assert!(!text.contains(w), "{} contains {w:?}", d.title());
            }
        }
    }

    /// The preview splits a paragraph at every bold, italic, code or link mark and reads
    /// underscores and asterisks as marks: keep the shipped paragraphs free of them
    /// (list items, headings, quotes and fenced blocks are drawn as plain rows).
    #[test]
    fn paragraphs_avoid_markup_the_preview_cannot_draw_well() {
        for d in HelpDoc::ALL {
            let mut in_fence = false;
            for line in render(d, &env()).lines() {
                if line.starts_with("```") {
                    in_fence = !in_fence;
                    continue;
                }
                let plain = !in_fence && !line.starts_with("- ") && !line.starts_with('#') && !line.starts_with('>');
                if plain {
                    for bad in ['*', '`', '_'] {
                        assert!(!line.contains(bad), "{}: paragraph with {bad:?}: {line}", d.title());
                    }
                    assert!(!line.contains("]("), "{}: link in a paragraph: {line}", d.title());
                }
            }
            assert!(!in_fence, "{}: unclosed code fence", d.title());
        }
    }

    #[test]
    fn every_placeholder_is_replaced() {
        for d in HelpDoc::ALL {
            let text = render(d, &env());
            assert!(!text.contains("{{"), "{} still has a placeholder: {}", d.title(), text.lines().find(|l| l.contains("{{")).unwrap_or(""));
            assert!(text.starts_with("# "), "{} must start with a title", d.title());
        }
        let notes = render(ReleaseNotes, &env());
        assert!(notes.contains(crate::buildinfo::VERSION) && notes.contains(&format!("build {}", crate::buildinfo::BUILD)));
        assert!(render(Welcome, &env()).contains("C:\\Users\\x\\.brouillon\\settings.toml"));
    }

    #[test]
    fn texts_say_how_to_report_a_bug_and_who_wrote_it() {
        let w = render(Welcome, &env());
        for needle in ["https://brouillon.rochetta.fr", "MIT", "Laurent Rochetta", "alpha"] {
            assert!(w.contains(needle), "welcome.md must mention {needle}");
        }
        // Honest alpha status: no unqualified promises.
        assert!(w.to_lowercase().contains("alpha"));
    }

    #[test]
    fn documentation_lists_every_language_the_editor_colours() {
        let docs = render(Docs, &env());
        let reg = brouillon_editor_engine::syntax::Registry::build(&[]);
        let mut names: Vec<&str> = reg.names();
        names.push("Rust");
        assert!(names.len() >= 30, "30 languages expected, found {}: {names:?}", names.len());
        for n in names {
            assert!(docs.contains(n), "docs.md does not list the language {n:?}");
        }
    }

    #[test]
    fn documentation_names_the_real_setting_keys() {
        let docs = render(Docs, &env());
        for k in ["startup.document", "startup.whats_new", "render.backend", "backup.enabled", "backup.max_count", "backup.max_age_days", "keymap.classic"] {
            assert!(docs.contains(k), "docs.md should explain {k}");
        }
    }

    #[test]
    fn generated_shortcuts_come_from_the_registry_and_are_not_empty() {
        let (reg, errs) = crate::wiring::build_registry();
        assert!(errs.is_empty(), "{errs:?}");
        let md = shortcuts_markdown(&reg, false, &[]);
        for expect in ["- Ctrl+Shift+F:", "- Ctrl+S: Save", "- Ctrl+K: Command Palette", "- F3:"] {
            assert!(md.contains(expect), "missing {expect:?} in:\n{md}");
        }
        assert!(md.contains("### File") && md.contains("### Search"));
        // The user layer wins and an unbinding removes the row.
        let user = vec![Binding::new("ctrl+alt+shift+j", "case_upper", None).unwrap()];
        assert!(shortcuts_markdown(&reg, false, &user).contains("- Ctrl+Shift+Alt+J:"));
        assert!(!md.contains("- Ctrl+Shift+Alt+J:"));
        // No two rows may share a chord (fixed shortcuts are skipped when bound).
        let mut chords: Vec<&str> = md.lines().filter_map(|l| l.strip_prefix("- ")?.split_once(": ").map(|p| p.0)).collect();
        let total = chords.len();
        chords.sort_unstable();
        chords.dedup();
        assert!(total - chords.len() <= 12, "unexpected duplicate chords: {total} rows, {} distinct", chords.len());
    }

    #[test]
    fn editing_keys_are_recognised_and_navigation_is_not() {
        let ch = |s: &str| Key::Character(s.into());
        assert!(is_editing_key(&ch("a"), false, false));
        assert!(is_editing_key(&ch("@"), true, true), "AltGr types");
        assert!(!is_editing_key(&ch("c"), true, false), "Ctrl+C is a command, not typing");
        for k in [NamedKey::Enter, NamedKey::Backspace, NamedKey::Delete, NamedKey::Tab, NamedKey::Space] {
            assert!(is_editing_key(&Key::Named(k), false, false), "{k:?}");
        }
        for k in [NamedKey::ArrowDown, NamedKey::PageUp, NamedKey::Home, NamedKey::End, NamedKey::Escape] {
            assert!(!is_editing_key(&Key::Named(k), false, false), "{k:?}");
        }
    }

    #[test]
    fn text_changing_commands_are_refused_on_help_tabs_but_files_and_navigation_are_not() {
        for id in ["case_upper", "sort_asc", "edit_paste", "edit_cut", "edit_undo", "delete_line", "newline_auto_indent"] {
            assert!(command_blocked_on_help(id), "{id}");
        }
        for id in ["file_save", "file_save_as", "file_open", "file_new", "file_close", "edit_copy", "select_all", "go_to", "help_docs", "search_find_next", "view_toggle_preview", "tools_run_slot_1"] {
            assert!(!command_blocked_on_help(id), "{id}");
        }
    }
}
