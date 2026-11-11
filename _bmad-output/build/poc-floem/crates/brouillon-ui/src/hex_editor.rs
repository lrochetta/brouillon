//! Editable hex mode over a real file: the grid (selection, nibble editing, undo/redo), the Data
//! inspector panel, the Regions panel, the Goto-offset prompt, Copy as, hashes with progress.
//!
//! Every rule lives in `brouillon_editor_engine::hex_session` (unit-tested, no GUI). This file is the
//! thin Floem layer: signals, views and the key mapping.
//!
//! Floem constraints honoured: rows come from a `virtual_stack`; a row is a few styled runs, not a
//! widget per byte; the panels are `stack_from_iter` lists (no 16-tuple limit); there is no timer
//! and no repaint chain: the hash worker pushes throttled progress through one channel and one
//! trigger, drained by an effect on the GUI thread. Keyboard: the grid handles keys only while it
//! has the focus (or the host forwards them while nothing else owns the keyboard), and never while
//! a text field of this view (goto, region name) has the focus, see `kbd_focus`.
//!
//! Hex bytes and regions are local: nothing here is exposed to agents.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::mpsc::{channel, Receiver, Sender};
use std::time::{Duration, Instant};

use floem::event::{Event, EventListener, EventPropagation};
use floem::ext_event::register_ext_trigger;
use floem::keyboard::{Key, NamedKey};
use floem::kurbo::{Point, Rect};
use floem::peniko::Color;
use floem::reactive::{create_rw_signal, RwSignal, Scope, SignalGet, SignalUpdate, SignalWith, Trigger};
use floem::style::CursorStyle;
use floem::views::{
    button, container, dyn_container, empty, label, scroll, stack, stack_from_iter, text_input, virtual_stack, Decorators, VirtualDirection,
    VirtualItemSize, VirtualVector,
};
use floem::{AnyView, IntoView};

use brouillon_core::theme::{current_theme, Theme};
use brouillon_editor_engine::byte_source::Ctl;
use brouillon_editor_engine::copy_as::CopyFormat;
use brouillon_editor_engine::hex_doc::{read_page, BYTES_PER_ROW};
use brouillon_editor_engine::hex_hashes::{hash_range, HashAlgo};
use brouillon_editor_engine::hex_inspect::rows_for_selection_len;
use brouillon_editor_engine::hex_regions::{Region, RegionColor};
use brouillon_editor_engine::hex_session::{CellKind, HexSession, Move};
use brouillon_editor_engine::search::CancelToken;

use crate::editor_view::{char_width, editor_font, line_height};
use crate::hex_view::{ascii_runs, byte_at_col, hex_runs, Run};

/// Left padding of a grid row, in px (the first text column starts here).
const ROW_PAD: f64 = 10.0;

/// Palette commands of the hex mode: (id, caption). The host maps them to [`HexUi::command`].
pub const COMMANDS: &[(&str, &str)] = &[
    ("hex_goto", "Hex: Go to Offset..."),
    ("hex_toggle_inspector", "Hex: Toggle Data Inspector"),
    ("hex_toggle_regions", "Hex: Toggle Regions Panel"),
    ("hex_add_region", "Hex: Add Region from Selection"),
    ("hex_toggle_endian", "Hex: Toggle Little/Big Endian"),
    ("hex_hash_selection", "Hex: Hash Selection (CRC-32, MD5, SHA-1, SHA-256)"),
    ("hex_copy_as_hex", "Hex: Copy as hex string"),
    ("hex_copy_as_spaced_hex", "Hex: Copy as spaced hex"),
    ("hex_copy_as_c_array", "Hex: Copy as C array"),
    ("hex_copy_as_rust_array", "Hex: Copy as Rust array"),
    ("hex_copy_as_python_bytes", "Hex: Copy as Python bytes"),
    ("hex_copy_as_base64", "Hex: Copy as Base64"),
    ("hex_copy_as_ascii", "Hex: Copy as ASCII text"),
];

// ------------------------------------------------------------------------------------------ pure parts

/// What a key does in the hex grid.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HexKey {
    Cursor(Move, bool),
    Digit(char),
    Undo,
    Redo,
    Save,
    Goto,
    SelectAll,
    Copy,
    CollapseSelection,
}

/// Map a key press to a hex action. `None`: not ours (the caller lets it through). Ctrl+Alt (AltGr)
/// chords never type digits; Ctrl chords that are not listed pass through to the global shortcuts.
pub fn map_key(key: &Key, ctrl: bool, shift: bool, alt: bool) -> Option<HexKey> {
    if let Key::Named(n) = key {
        let m = |mv: Move| Some(HexKey::Cursor(mv, shift));
        return match (n, ctrl) {
            (NamedKey::ArrowLeft, false) => m(Move::Left),
            (NamedKey::ArrowRight, false) => m(Move::Right),
            (NamedKey::ArrowUp, false) => m(Move::Up),
            (NamedKey::ArrowDown, false) => m(Move::Down),
            (NamedKey::PageUp, false) => m(Move::PageUp),
            (NamedKey::PageDown, false) => m(Move::PageDown),
            (NamedKey::Home, false) => m(Move::LineStart),
            (NamedKey::End, false) => m(Move::LineEnd),
            (NamedKey::Home, true) => m(Move::DocStart),
            (NamedKey::End, true) => m(Move::DocEnd),
            (NamedKey::Escape, false) => Some(HexKey::CollapseSelection),
            _ => None,
        };
    }
    let Key::Character(c) = key else { return None };
    if c.chars().count() != 1 || alt {
        return None;
    }
    let ch = c.chars().next()?;
    if ctrl {
        return match (ch.to_ascii_lowercase(), shift) {
            ('z', false) => Some(HexKey::Undo),
            ('z', true) | ('y', false) => Some(HexKey::Redo),
            ('s', false) => Some(HexKey::Save),
            ('g', false) => Some(HexKey::Goto),
            ('a', false) => Some(HexKey::SelectAll),
            ('c', false) => Some(HexKey::Copy),
            _ => None,
        };
    }
    ch.is_ascii_hexdigit().then_some(HexKey::Digit(ch))
}

/// The one-line status of the hex view.
pub fn status_line(s: &HexSession) -> String {
    let mut out = format!("Offset 0x{:08X}", s.cursor());
    if let Some(r) = s.selection() {
        out.push_str(&format!(" | Sel 0x{:X}-0x{:X} ({} B)", r.start, r.end - 1, r.end - r.start));
    }
    out.push_str(&format!(" | {} | {} B", s.endian.label(), s.len()));
    if s.low_nibble_pending() {
        out.push_str(" | type the low digit");
    }
    if s.is_dirty() {
        out.push_str(" | unsaved byte edits (Ctrl+S)");
    }
    out
}

/// "Hashing... 42%".
pub fn progress_text(done: u64, total: u64) -> String {
    if total == 0 {
        return "Hashing...".to_string();
    }
    format!("Hashing... {}%", (done.min(total) as u128 * 100 / total as u128) as u64)
}

/// Pixel position in the grid to a byte offset (None on a gap, the offset column, or past the end).
pub fn hit_offset(pos: Point, scroll_y: f64, line_h: f64, cell_w: f64, len: u64) -> Option<u64> {
    if line_h <= 0.0 || cell_w <= 0.0 {
        return None;
    }
    let row = ((pos.y + scroll_y) / line_h).floor();
    let col = ((pos.x - ROW_PAD) / cell_w).floor();
    if row < 0.0 || col < 0.0 {
        return None;
    }
    let b = byte_at_col(col as usize, BYTES_PER_ROW)?;
    let off = row as u64 * BYTES_PER_ROW as u64 + b as u64;
    (off < len).then_some(off)
}

fn region_rgb(c: RegionColor) -> Color {
    let (r, g, b) = c.rgb();
    Color::rgb8(r, g, b)
}

/// Background and text colour of a run.
fn run_colors(t: &Theme, kind: CellKind) -> (Option<Color>, Color) {
    match kind {
        CellKind::Plain => (None, t.fg),
        CellKind::Region(c) => (Some(region_rgb(c).multiply_alpha(0.32)), t.fg),
        CellKind::Selected => (Some(t.accent.multiply_alpha(0.35)), t.fg),
        CellKind::Cursor => (Some(t.accent), t.bg),
    }
}

// ------------------------------------------------------------------------------------------ state

enum HashMsg {
    Progress(u64, u64, u64),
    Done(u64, Vec<(HashAlgo, String)>),
    Cancelled(u64),
}

/// The hex editor state shared by the view and the host (`main.rs`). Cheap to clone.
#[derive(Clone)]
pub struct HexUi {
    sessions: Rc<RefCell<HashMap<PathBuf, HexSession>>>,
    active: RwSignal<Option<PathBuf>>,
    /// Bumped after every change: rows and panels re-read the session.
    pub rev: RwSignal<u64>,
    /// Cursor offset the grid scrolls to (set by keyboard moves, goto and region jumps).
    reveal: RwSignal<u64>,
    pub inspector_on: RwSignal<bool>,
    pub regions_on: RwSignal<bool>,
    pub goto_open: RwSignal<bool>,
    pub goto_text: RwSignal<String>,
    pub goto_err: RwSignal<String>,
    pub region_name: RwSignal<String>,
    region_sel: RwSignal<Option<u32>>,
    pub message: RwSignal<String>,
    pub hash_rows: RwSignal<Vec<(String, String)>>,
    pub hash_progress: RwSignal<String>,
    /// A text field of this view has the focus: the grid must not take its keys.
    pub field_focus: RwSignal<bool>,
    regions_dir: Option<PathBuf>,
    hash_tx: Sender<HashMsg>,
    hash_trigger: Trigger,
    hash_job: Rc<Cell<u64>>,
    hash_cancel: Rc<RefCell<Option<CancelToken>>>,
}

fn copy_to_clipboard(text: &str) -> bool {
    arboard::Clipboard::new().and_then(|mut cb| cb.set_text(text.to_string())).is_ok()
}

impl HexUi {
    /// Create inside a reactive scope. `regions_dir` is the settings-side folder of the regions
    /// sidecars (`Paths::hex_regions_dir`); None keeps regions in memory only.
    pub fn new(cx: Scope, regions_dir: Option<PathBuf>) -> Self {
        let trigger = Trigger::new();
        let (tx, rx): (Sender<HashMsg>, Receiver<HashMsg>) = channel();
        let ui = Self {
            sessions: Rc::new(RefCell::new(HashMap::new())),
            active: create_rw_signal(None),
            rev: create_rw_signal(0),
            reveal: create_rw_signal(0),
            inspector_on: create_rw_signal(true),
            regions_on: create_rw_signal(false),
            goto_open: create_rw_signal(false),
            goto_text: create_rw_signal(String::new()),
            goto_err: create_rw_signal(String::new()),
            region_name: create_rw_signal(String::new()),
            region_sel: create_rw_signal(None),
            message: create_rw_signal(String::new()),
            hash_rows: create_rw_signal(Vec::new()),
            hash_progress: create_rw_signal(String::new()),
            field_focus: create_rw_signal(false),
            regions_dir,
            hash_tx: tx,
            hash_trigger: trigger,
            hash_job: Rc::new(Cell::new(0)),
            hash_cancel: Rc::new(RefCell::new(None)),
        };
        // The worker never touches a signal: it sends and registers the trigger; this effect runs on
        // the GUI thread and applies what arrived.
        let me = ui.clone();
        cx.create_effect(move |_| {
            trigger.track();
            while let Ok(m) = rx.try_recv() {
                me.apply_hash_msg(m);
            }
        });
        ui
    }

    // ------------------------------------------------------------------------------ sessions

    /// Open (or switch to) the hex session of `path`. A session with unsaved byte edits stays in
    /// memory while another file is shown.
    pub fn activate(&self, path: &Path) -> Result<(), String> {
        if !self.sessions.borrow().contains_key(path) {
            let s = HexSession::open(path, self.regions_dir.as_deref()).map_err(|e| e.to_string())?;
            self.sessions.borrow_mut().insert(path.to_path_buf(), s);
        }
        self.active.set(Some(path.to_path_buf()));
        self.goto_open.set(false);
        self.hash_rows.set(Vec::new());
        self.hash_progress.set(String::new());
        self.message.set(String::new());
        self.bump();
        Ok(())
    }
    /// Leave the hex view. A clean session is dropped (memory), a dirty one is kept.
    pub fn deactivate(&self) {
        if let Some(p) = self.active.get_untracked() {
            let mut m = self.sessions.borrow_mut();
            if m.get(&p).is_some_and(|s| !s.is_dirty()) {
                m.remove(&p);
            }
        }
        self.cancel_hash();
        self.active.set(None);
    }
    pub fn is_active(&self) -> bool {
        self.active.with_untracked(|a| a.is_some())
    }
    /// The path of the active session, without tracking.
    pub fn active_path_now(&self) -> Option<PathBuf> {
        self.active.get_untracked()
    }
    /// Reactive: the path of the active session.
    pub fn active_path(&self) -> Option<PathBuf> {
        self.active.get()
    }
    /// Unsaved byte edits in any session (the host should warn before quitting).
    pub fn any_dirty(&self) -> bool {
        self.sessions.borrow().values().any(|s| s.is_dirty())
    }
    /// Number of sessions with unsaved byte edits.
    pub fn dirty_count(&self) -> usize {
        self.sessions.borrow().values().filter(|s| s.is_dirty()).count()
    }
    /// Save every dirty session (quit path). Stops at the first failure and returns its message,
    /// so the host keeps the app open instead of losing bytes.
    pub fn save_all_dirty(&self) -> Result<usize, String> {
        let token = CancelToken::new();
        let mut n = 0;
        for (p, s) in self.sessions.borrow_mut().iter_mut().filter(|(_, s)| s.is_dirty()) {
            s.save(&mut Ctl::new(&token)).map_err(|e| format!("{}: {e}", p.display()))?;
            n += 1;
        }
        self.bump();
        Ok(n)
    }
    pub fn is_dirty_path(&self, p: &Path) -> bool {
        self.sessions.borrow().get(p).is_some_and(|s| s.is_dirty())
    }

    /// Run `f` on the active session. Never nest calls (the session is borrowed mutably).
    pub fn with<R>(&self, f: impl FnOnce(&mut HexSession) -> R) -> Option<R> {
        let p = self.active.get_untracked()?;
        self.sessions.borrow_mut().get_mut(&p).map(f)
    }
    fn bump(&self) {
        self.rev.update(|r| *r += 1);
    }
    fn bump_reveal(&self) {
        if let Some(c) = self.with(|s| s.cursor()) {
            self.reveal.set(c);
        }
        self.bump();
    }
    fn row_total(&self) -> u64 {
        let _ = self.rev.get();
        self.with(|s| brouillon_editor_engine::hex_doc::row_count(s.len(), BYTES_PER_ROW)).unwrap_or(0)
    }

    // ------------------------------------------------------------------------------ actions

    fn submit_goto(&self) {
        let text = self.goto_text.get_untracked();
        match self.with(|s| s.goto(&text)) {
            Some(Ok(_)) => {
                self.goto_open.set(false);
                // the goto field is torn down without a FocusLost: a stale `true` here froze every grid key
                // (arrows, digits, Ctrl+G) until the next click in the grid
                self.field_focus.set(false);
                self.goto_err.set(String::new());
                self.bump_reveal();
            }
            Some(Err(e)) => self.goto_err.set(e.to_string()),
            None => self.goto_open.set(false),
        }
    }
    pub fn open_goto(&self) {
        self.goto_text.set(String::new());
        self.goto_err.set(String::new());
        self.goto_open.set(true);
    }
    fn copy_as(&self, f: CopyFormat) {
        match self.with(|s| s.copy_as(f)) {
            Some(Ok(text)) => {
                let n = text.len();
                if copy_to_clipboard(&text) {
                    self.message.set(format!("Copied as {} ({n} characters)", f.label()));
                } else {
                    self.message.set("Could not reach the clipboard".to_string());
                }
            }
            Some(Err(e)) => self.message.set(e.to_string()),
            None => {}
        }
    }
    fn save(&self) {
        let token = CancelToken::new();
        match self.with(|s| s.save(&mut Ctl::new(&token))) {
            Some(Ok(())) => self.message.set("Saved".to_string()),
            Some(Err(e)) => self.message.set(format!("Save failed: {e}")),
            None => {}
        }
        self.bump();
    }
    fn add_region(&self) {
        let name = self.region_name.get_untracked();
        let color = self.with(|s| RegionColor::ALL[s.regions().len() % RegionColor::ALL.len()]).unwrap_or_default();
        match self.with(|s| s.add_region(&name, color)) {
            Some(Ok(_)) => {
                self.region_name.set(String::new());
                self.regions_on.set(true);
                self.message.set("Region added".to_string());
            }
            Some(Err(e)) => self.message.set(e.to_string()),
            None => {}
        }
        self.bump();
    }

    /// Start hashing the selection (or the cursor byte) on a worker thread.
    pub fn start_hash(&self) {
        let Some((snap, range)) = self.with(|s| (s.snapshot(), s.effective_range())) else { return };
        self.cancel_hash();
        let token = CancelToken::new();
        *self.hash_cancel.borrow_mut() = Some(token.clone());
        let job = self.hash_job.get() + 1;
        self.hash_job.set(job);
        self.hash_rows.set(Vec::new());
        self.hash_progress.set(progress_text(0, range.end - range.start));
        let (tx, trig) = (self.hash_tx.clone(), self.hash_trigger);
        std::thread::spawn(move || {
            let mut last = Instant::now();
            let mut cb = |done: u64, total: u64| {
                // throttled to about 10 Hz; the last tick always goes through
                if last.elapsed() >= Duration::from_millis(100) || done >= total {
                    last = Instant::now();
                    let _ = tx.send(HashMsg::Progress(job, done, total));
                    register_ext_trigger(trig);
                }
            };
            let res = {
                let mut ctl = Ctl::new(&token).with_progress(&mut cb);
                hash_range(&snap, range, &HashAlgo::ALL, &mut ctl)
            };
            let _ = tx.send(match res {
                Ok(v) => HashMsg::Done(job, v),
                Err(_) => HashMsg::Cancelled(job),
            });
            register_ext_trigger(trig);
        });
    }
    fn cancel_hash(&self) {
        if let Some(t) = self.hash_cancel.borrow_mut().take() {
            t.cancel();
        }
    }
    fn apply_hash_msg(&self, m: HashMsg) {
        let cur = self.hash_job.get();
        match m {
            HashMsg::Progress(j, d, t) if j == cur => self.hash_progress.set(progress_text(d, t)),
            HashMsg::Done(j, v) if j == cur => {
                self.hash_progress.set(String::new());
                self.hash_rows.set(v.into_iter().map(|(a, h)| (a.label().to_string(), h)).collect());
            }
            HashMsg::Cancelled(j) if j == cur => self.hash_progress.set("Hash cancelled".to_string()),
            _ => {}
        }
    }

    /// A key press. Returns true when it was consumed.
    pub fn handle_key(&self, key: &Key, ctrl: bool, shift: bool, alt: bool) -> bool {
        if !self.is_active() || self.field_focus.get_untracked() || self.goto_open.get_untracked() {
            return false;
        }
        let Some(k) = map_key(key, ctrl, shift, alt) else { return false };
        self.message.set(String::new());
        match k {
            HexKey::Cursor(m, ext) => {
                self.with(|s| s.move_cursor(m, ext));
                self.bump_reveal();
            }
            HexKey::Digit(c) => {
                match self.with(|s| s.type_hex_digit(c)) {
                    Some(Err(e)) => self.message.set(format!("Cannot edit: {e}")),
                    _ => {}
                }
                self.bump_reveal();
            }
            HexKey::Undo => {
                if self.with(|s| s.undo()) == Some(true) {
                    self.bump_reveal();
                }
            }
            HexKey::Redo => {
                if self.with(|s| s.redo()) == Some(true) {
                    self.bump_reveal();
                }
            }
            HexKey::Save => self.save(),
            HexKey::Goto => self.open_goto(),
            HexKey::SelectAll => {
                self.with(|s| s.select_all());
                self.bump();
            }
            HexKey::Copy => self.copy_as(CopyFormat::HexSpaced),
            HexKey::CollapseSelection => {
                if self.with(|s| s.selection().is_some()) != Some(true) {
                    return false;
                }
                self.with(|s| {
                    let c = s.cursor();
                    s.click(c, false)
                });
                self.bump();
            }
        }
        true
    }

    /// A palette / menu command. True when consumed: the `hex_*` ids, and while a hex session is
    /// active the shared ones whose meaning changes (save, undo, redo, copy, select all) so they act
    /// on the bytes and never on the hidden text buffer.
    pub fn command(&self, id: &str) -> bool {
        if let Some(f) = CopyFormat::from_command_id(id) {
            self.copy_as(f);
            return true;
        }
        match id {
            "hex_goto" => self.open_goto(),
            "hex_toggle_inspector" => self.inspector_on.update(|v| *v = !*v),
            "hex_toggle_regions" => {
                self.regions_on.update(|v| *v = !*v);
                self.field_focus.set(false); // the region name field disappears with the panel
            }
            "hex_add_region" => self.add_region(),
            "hex_toggle_endian" => {
                self.with(|s| s.endian = s.endian.toggled());
                self.bump();
            }
            "hex_hash_selection" => self.start_hash(),
            "file_save" | "edit_undo" | "edit_redo" | "edit_copy" | "edit_select_all" if self.is_active() => {
                let (key, ctrl, shift) = match id {
                    "file_save" => ("s", true, false),
                    "edit_undo" => ("z", true, false),
                    "edit_redo" => ("y", true, false),
                    "edit_copy" => ("c", true, false),
                    _ => ("a", true, false),
                };
                return self.handle_key(&Key::Character(key.into()), ctrl, shift, false);
            }
            _ => return false,
        }
        true
    }
}

// ------------------------------------------------------------------------------------------ views

struct Rows {
    rows: u64,
    version: u64,
}
impl VirtualVector<(u64, u64)> for Rows {
    fn total_len(&self) -> usize {
        self.rows.min(usize::MAX as u64) as usize
    }
    fn slice(&mut self, range: std::ops::Range<usize>) -> impl Iterator<Item = (u64, u64)> {
        let end = (range.end as u64).min(self.rows);
        let version = self.version;
        ((range.start as u64).min(end)..end).map(move |i| (i, version))
    }
}

struct RegionRows(Vec<Region>);
impl VirtualVector<Region> for RegionRows {
    fn total_len(&self) -> usize {
        self.0.len()
    }
    fn slice(&mut self, range: std::ops::Range<usize>) -> impl Iterator<Item = Region> {
        let end = range.end.min(self.0.len());
        self.0[range.start.min(end)..end].to_vec().into_iter()
    }
}

fn run_label(r: Run, theme_idx: RwSignal<usize>) -> AnyView {
    let kind = r.kind;
    label(move || r.text.clone())
        .style(move |s| {
            let t = current_theme(theme_idx.get());
            let (bg, fg) = run_colors(&t, kind);
            let s = s.color(fg);
            match bg {
                Some(b) => s.background(b),
                None => s,
            }
        })
        .into_any()
}

fn plain_label(text: String, theme_idx: RwSignal<usize>, dim: bool) -> AnyView {
    label(move || text.clone())
        .style(move |s| {
            let t = current_theme(theme_idx.get());
            s.color(if dim { t.fg_dim } else { t.fg })
        })
        .into_any()
}

fn row_view(ui: &HexUi, idx: u64, theme_idx: RwSignal<usize>) -> impl IntoView {
    let first = idx * BYTES_PER_ROW as u64;
    let (hex, ascii) = ui
        .with(|s| {
            let page = read_page(s.doc(), idx, 1, BYTES_PER_ROW);
            let kind = |o: u64| s.cell_kind(o);
            (hex_runs(&page, first, BYTES_PER_ROW, &kind), ascii_runs(&page, first, BYTES_PER_ROW, &kind))
        })
        .unwrap_or_default();
    let mut views: Vec<AnyView> = Vec::with_capacity(hex.len() + ascii.len() + 2);
    views.push(plain_label(format!("{first:08X}  "), theme_idx, true));
    views.extend(hex.into_iter().map(|r| run_label(r, theme_idx)));
    views.push(plain_label("  ".to_string(), theme_idx, false));
    views.extend(ascii.into_iter().map(|r| run_label(r, theme_idx)));
    stack_from_iter(views).style(move |s| {
        let (fam, size) = editor_font();
        s.flex_row().height(line_height()).padding_horiz(ROW_PAD).font_family(fam).font_size(size)
    })
}

fn tool_button(text: impl Fn() -> String + 'static, theme_idx: RwSignal<usize>, action: impl Fn() + 'static) -> impl IntoView {
    button(label(move || format!(" {} ", text()))).action(action).style(move |s| {
        let t = current_theme(theme_idx.get());
        s.color(t.fg).background(t.bg).border(1.0).border_color(t.border).border_radius(3.0).padding_horiz(4.0).margin_right(4.0).cursor(CursorStyle::Pointer)
    })
}

fn toolbar(ui: &HexUi, theme_idx: RwSignal<usize>) -> impl IntoView {
    let (a, b, c, d, e) = (ui.clone(), ui.clone(), ui.clone(), ui.clone(), ui.clone());
    stack((
        tool_button(move || if a.inspector_on.get() { "Inspector: on".to_string() } else { "Inspector: off".to_string() }, theme_idx, {
            let a = ui.clone();
            move || a.inspector_on.update(|v| *v = !*v)
        }),
        tool_button(move || if b.regions_on.get() { "Regions: on".to_string() } else { "Regions: off".to_string() }, theme_idx, {
            let b = ui.clone();
            move || b.regions_on.update(|v| *v = !*v)
        }),
        tool_button(
            move || {
                let _ = c.rev.get();
                c.with(|s| s.endian.label()).unwrap_or("LE").to_string()
            },
            theme_idx,
            {
                let c = ui.clone();
                move || {
                    c.command("hex_toggle_endian");
                }
            },
        ),
        tool_button(|| "Go to".to_string(), theme_idx, {
            let d = d.clone();
            move || d.open_goto()
        }),
        tool_button(|| "Hash selection".to_string(), theme_idx, {
            let e = e.clone();
            move || e.start_hash()
        }),
    ))
    .style(move |s| {
        let t = current_theme(theme_idx.get());
        s.flex_row().items_center().padding(4.0).background(t.bg_chrome).border_bottom(1.0).border_color(t.border)
    })
}

fn goto_row(ui: &HexUi, theme_idx: RwSignal<usize>) -> impl IntoView {
    let ui = ui.clone();
    dyn_container(
        move || ui.goto_open.get(),
        {
            let ui = ui.clone();
            move |open: bool| {
                if !open {
                    return empty().into_any();
                }
                let submit = ui.clone();
                let esc = ui.clone();
                let (fg, fl) = (ui.clone(), ui.clone());
                let err = ui.goto_err;
                let field = text_input(ui.goto_text)
                    .placeholder("0x1F40   +0x20   end-8   1k   sel.start+4")
                    // built when it opens: take the keyboard so typing lands here, not in the grid
                    .request_focus(|| {})
                    .style(move |s| {
                        let t = current_theme(theme_idx.get());
                        s.width(300.0).padding_horiz(6.0).padding_vert(3.0).background(t.bg).border(1.0).border_color(t.border).border_radius(2.0).color(t.fg)
                    })
                    .on_event_cont(EventListener::FocusGained, move |_| fg.field_focus.set(true))
                    .on_event_cont(EventListener::FocusLost, move |_| fl.field_focus.set(false))
                    .on_event_stop(EventListener::KeyDown, move |event| {
                        if let Event::KeyDown(ke) = event {
                            if ke.key.logical_key == Key::Named(NamedKey::Enter) {
                                submit.submit_goto();
                            } else if ke.key.logical_key == Key::Named(NamedKey::Escape) {
                                esc.goto_open.set(false);
                                esc.field_focus.set(false);
                            }
                        }
                    });
                stack((
                    label(|| " Go to offset ".to_string()).style(move |s| s.color(current_theme(theme_idx.get()).fg_dim)),
                    field,
                    label(move || err.get()).style(move |s| s.color(current_theme(theme_idx.get()).error).margin_left(8.0)),
                ))
                .style(move |s| {
                    let t = current_theme(theme_idx.get());
                    s.flex_row().items_center().padding(4.0).background(t.bg_chrome).border_bottom(1.0).border_color(t.accent)
                })
                .into_any()
            }
        },
    )
}

fn grid(ui: &HexUi, theme_idx: RwSignal<usize>) -> impl IntoView {
    let viewport: RwSignal<Rect> = create_rw_signal(Rect::ZERO);
    let dragging = create_rw_signal(false);
    let indices = {
        let ui = ui.clone();
        move || Rows { rows: ui.row_total(), version: ui.rev.get() }
    };
    let body = scroll(
        virtual_stack(
            VirtualDirection::Vertical,
            VirtualItemSize::Fixed(Box::new(line_height)),
            indices,
            move |k: &(u64, u64)| *k,
            {
                let ui = ui.clone();
                move |(idx, _v): (u64, u64)| row_view(&ui, idx, theme_idx)
            },
        )
        .style(|s| s.flex_col().width_full()),
    )
    .on_scroll(move |r| viewport.set(r))
    .scroll_to({
        let ui = ui.clone();
        move || {
            // Tracks only the reveal offset: tracking the viewport would snap the view back to the
            // cursor on every manual scroll.
            let off = ui.reveal.get();
            let vp = viewport.get_untracked();
            if vp.height() <= 0.0 {
                return None;
            }
            let lh = line_height();
            let y = (off / BYTES_PER_ROW as u64) as f64 * lh;
            if y < vp.y0 {
                Some(Point::new(vp.x0, y))
            } else if y + lh > vp.y1 {
                Some(Point::new(vp.x0, y + lh - vp.height()))
            } else {
                None
            }
        }
    })
    .style(move |s| {
        let t = current_theme(theme_idx.get());
        s.size_full().background(t.bg)
    });
    let (d, m, u) = (ui.clone(), ui.clone(), ui.clone());
    let at = move |ui: &HexUi, pos: Point| -> Option<u64> {
        let len = ui.with(|s| s.len())?;
        hit_offset(pos, viewport.get_untracked().y0, line_height(), char_width(), len)
    };
    let at2 = at.clone();
    let ui_key = ui.clone();
    container(body)
        .style(|s| s.flex_grow(1.0).min_width(0.0).height_full())
        .keyboard_navigable()
        .on_event_stop(EventListener::PointerDown, move |event| {
            // a press in the grid gives the keyboard to the editor side (kbd_focus rule)
            crate::kbd_focus::editor_pointer();
            if let Event::PointerDown(pe) = event {
                if let Some(off) = at(&d, pe.pos) {
                    d.with(|s| s.click(off, pe.modifiers.shift()));
                    dragging.set(true);
                    d.bump();
                }
            }
        })
        .on_event_stop(EventListener::PointerMove, move |event| {
            if let Event::PointerMove(pe) = event {
                if dragging.get_untracked() {
                    if let Some(off) = at2(&m, pe.pos) {
                        let moved = m.with(|s| {
                            let was = (s.cursor(), s.anchor());
                            s.drag_to(off);
                            was != (s.cursor(), s.anchor())
                        });
                        if moved == Some(true) {
                            m.bump();
                        }
                    }
                }
            }
        })
        .on_event_stop(EventListener::PointerUp, move |_| {
            dragging.set(false);
            u.field_focus.set(false);
        })
        .on_event(EventListener::KeyDown, move |event| {
            if let Event::KeyDown(ke) = event {
                if ui_key.handle_key(&ke.key.logical_key, ke.modifiers.control(), ke.modifiers.shift(), ke.modifiers.alt()) {
                    return EventPropagation::Stop;
                }
            }
            EventPropagation::Continue
        })
        .request_focus({
            let ui = ui.clone();
            move || {
                // take the keyboard when a document is shown (and again when the file changes)
                let _ = ui.active_path();
            }
        })
}

fn inspector_panel(ui: &HexUi, theme_idx: RwSignal<usize>) -> impl IntoView {
    let ui = ui.clone();
    dyn_container(
        move || (ui.rev.get(), ui.inspector_on.get()),
        {
            let ui = ui.clone();
            move |(_, on): (u64, bool)| {
                if !on {
                    return empty().into_any();
                }
                let (rows, emph) = ui
                    .with(|s| (s.inspector(), s.selection().and_then(|r| rows_for_selection_len(r.end - r.start))))
                    .unwrap_or_default();
                let mut views: Vec<AnyView> = Vec::with_capacity(rows.len() + 1);
                views.push(
                    label(|| "Data inspector".to_string())
                        .style(move |s| s.color(current_theme(theme_idx.get()).fg_dim).margin_bottom(4.0))
                        .into_any(),
                );
                for r in rows {
                    let emphasised = emph.is_some_and(|(a, b)| r.id == a || r.id == b);
                    let value = r.value.clone();
                    let shown = value.clone().unwrap_or_else(|| "n/a".to_string());
                    let known = value.is_some();
                    let name = r.label;
                    views.push(
                        stack((
                            label(move || name.to_string()).style(move |s| s.width(92.0).color(current_theme(theme_idx.get()).fg_dim)),
                            label(move || shown.clone()).style(move |s| {
                                let t = current_theme(theme_idx.get());
                                s.color(if known { t.fg } else { t.fg_dim.multiply_alpha(0.6) })
                            }),
                        ))
                        .style(move |s| {
                            let t = current_theme(theme_idx.get());
                            let s = s.flex_row().padding_vert(1.0).cursor(CursorStyle::Pointer);
                            if emphasised { s.background(t.accent.multiply_alpha(0.18)) } else { s }
                        })
                        .on_click_stop({
                            let ui = ui.clone();
                            move |_| {
                                if let Some(v) = &value {
                                    if copy_to_clipboard(v) {
                                        ui.message.set(format!("Copied {v}"));
                                    }
                                }
                            }
                        })
                        .into_any(),
                    );
                }
                stack_from_iter(views)
                    .style(move |s| {
                        let (fam, size) = editor_font();
                        let t = current_theme(theme_idx.get());
                        s.flex_col()
                            .width(300.0)
                            .padding(8.0)
                            .background(t.bg_chrome)
                            .border_left(1.0)
                            .border_color(t.border)
                            .font_family(fam)
                            .font_size(size)
                    })
                    .into_any()
            }
        },
    )
}

fn region_row(ui: &HexUi, r: Region, theme_idx: RwSignal<usize>) -> AnyView {
    let (id, start, len, orphaned, color) = (r.id, r.start, r.len, r.orphaned, r.color);
    let name = r.name.clone();
    let range_text = if orphaned { "orphaned (bytes deleted)".to_string() } else { format!("0x{start:X} +{len}") };
    let swatch = container(empty()).style(move |s| {
        let c = region_rgb(color);
        let s = s.width(12.0).height(12.0).border_radius(2.0).margin_right(6.0).background(if orphaned { c.multiply_alpha(0.25) } else { c });
        s
    });
    let (go, col, del, pick) = (ui.clone(), ui.clone(), ui.clone(), ui.clone());
    let picked_name = name.clone();
    let small = move |text: &'static str, f: Box<dyn Fn()>| {
        button(label(move || format!(" {text} ")))
            .action(move || f())
            .style(move |s| {
                let t = current_theme(theme_idx.get());
                // an explicit background: the default button face is near-white and the pale theme text vanished on it
                s.color(t.fg).background(t.bg).border(1.0).border_color(t.border).border_radius(3.0).margin_left(3.0).cursor(CursorStyle::Pointer)
            })
            .into_any()
    };
    let buttons: Vec<AnyView> = vec![
        small(
            "Go",
            Box::new(move || {
                go.with(|s| s.jump_to_region(id));
                go.bump_reveal();
            }),
        ),
        small(
            "Colour",
            Box::new(move || {
                col.with(|s| s.recolor_region(id, color.next()));
                col.bump();
            }),
        ),
        small(
            "Delete",
            Box::new(move || {
                del.with(|s| s.remove_region(id));
                del.bump();
            }),
        ),
    ];
    stack((
        swatch,
        stack((
            label(move || name.clone()).style(move |s| s.color(current_theme(theme_idx.get()).fg)),
            label(move || range_text.clone()).style(move |s| s.color(current_theme(theme_idx.get()).fg_dim)),
        ))
        .style(|s| s.flex_col().flex_grow(1.0).min_width(0.0))
        .on_click_stop(move |_| {
            pick.region_sel.set(Some(id));
            pick.region_name.set(picked_name.clone());
        }),
        stack_from_iter(buttons).style(|s| s.flex_row()),
    ))
    .style(move |s| s.flex_row().items_center().padding_vert(2.0).cursor(CursorStyle::Pointer))
    .into_any()
}

fn regions_panel(ui: &HexUi, theme_idx: RwSignal<usize>) -> impl IntoView {
    let ui = ui.clone();
    dyn_container(
        move || ui.regions_on.get(),
        {
            let ui = ui.clone();
            move |on: bool| {
                if !on {
                    return empty().into_any();
                }
                let (fg, fl) = (ui.clone(), ui.clone());
                let name_field = text_input(ui.region_name)
                    .placeholder("Region name")
                    .style(move |s| {
                        let t = current_theme(theme_idx.get());
                        s.width(150.0).padding_horiz(6.0).padding_vert(2.0).background(t.bg).border(1.0).border_color(t.border).border_radius(2.0).color(t.fg)
                    })
                    .on_event_cont(EventListener::FocusGained, move |_| fg.field_focus.set(true))
                    .on_event_cont(EventListener::FocusLost, move |_| fl.field_focus.set(false));
                let (add, ren) = (ui.clone(), ui.clone());
                let controls = stack((
                    name_field,
                    tool_button(|| "Add".to_string(), theme_idx, move || add.add_region()),
                    tool_button(|| "Rename".to_string(), theme_idx, move || {
                        if let Some(id) = ren.region_sel.get_untracked() {
                            let n = ren.region_name.get_untracked();
                            ren.with(|s| s.rename_region(id, &n));
                            ren.bump();
                        }
                    }),
                ))
                .style(|s| s.flex_row().items_center().margin_bottom(4.0));
                let list = {
                    let ui = ui.clone();
                    let items = {
                        let ui = ui.clone();
                        move || {
                            let _ = ui.rev.get();
                            RegionRows(ui.with(|s| s.regions().all().to_vec()).unwrap_or_default())
                        }
                    };
                    scroll(
                        virtual_stack(
                            VirtualDirection::Vertical,
                            VirtualItemSize::Fixed(Box::new(|| 26.0)),
                            items,
                            move |r: &Region| (r.id, r.start, r.len, r.name.clone(), r.color as u8, r.orphaned),
                            move |r: Region| region_row(&ui, r, theme_idx),
                        )
                        .style(|s| s.flex_col().width_full()),
                    )
                    .style(|s| s.height(200.0).width_full())
                };
                stack((
                    label(|| "Regions".to_string()).style(move |s| s.color(current_theme(theme_idx.get()).fg_dim).margin_bottom(4.0)),
                    controls,
                    list,
                ))
                .style(move |s| {
                    let t = current_theme(theme_idx.get());
                    s.flex_col().width(300.0).padding(8.0).background(t.bg_chrome).border_left(1.0).border_color(t.border)
                })
                .into_any()
            }
        },
    )
}

fn hash_panel(ui: &HexUi, theme_idx: RwSignal<usize>) -> impl IntoView {
    let ui = ui.clone();
    dyn_container(
        move || (ui.hash_rows.get(), ui.hash_progress.get()),
        {
            let ui = ui.clone();
            move |(rows, progress): (Vec<(String, String)>, String)| {
                if rows.is_empty() && progress.is_empty() {
                    return empty().into_any();
                }
                let mut views: Vec<AnyView> = Vec::new();
                if !progress.is_empty() {
                    views.push(label(move || progress.clone()).style(move |s| s.color(current_theme(theme_idx.get()).warn)).into_any());
                }
                for (name, value) in rows {
                    let v = value.clone();
                    let ui = ui.clone();
                    views.push(
                        stack((
                            label(move || name.clone()).style(move |s| s.width(70.0).color(current_theme(theme_idx.get()).fg_dim)),
                            label(move || value.clone()).style(move |s| s.color(current_theme(theme_idx.get()).fg)),
                        ))
                        .style(|s| s.flex_row().cursor(CursorStyle::Pointer))
                        .on_click_stop(move |_| {
                            if copy_to_clipboard(&v) {
                                ui.message.set("Copied checksum".to_string());
                            }
                        })
                        .into_any(),
                    );
                }
                stack_from_iter(views)
                    .style(move |s| {
                        let (fam, size) = editor_font();
                        let t = current_theme(theme_idx.get());
                        s.flex_col().padding_horiz(8.0).padding_vert(4.0).background(t.bg_chrome).border_top(1.0).border_color(t.border).font_family(fam).font_size(size)
                    })
                    .into_any()
            }
        },
    )
}

fn status_strip(ui: &HexUi, theme_idx: RwSignal<usize>) -> impl IntoView {
    let ui = ui.clone();
    label(move || {
        let _ = ui.rev.get();
        let base = ui.with(|s| status_line(s)).unwrap_or_default();
        let msg = ui.message.get();
        if msg.is_empty() {
            base
        } else {
            format!("{base} | {msg}")
        }
    })
    .style(move |s| {
        let t = current_theme(theme_idx.get());
        s.padding_horiz(8.0).padding_vert(3.0).color(t.fg_status).background(t.bg_chrome).border_top(1.0).border_color(t.border).width_full()
    })
}

/// The hex editor: toolbar, goto prompt, grid with the inspector / regions beside it, hashes, status.
pub fn hex_editor_panel(ui: HexUi, theme_idx: RwSignal<usize>) -> impl IntoView {
    stack((
        toolbar(&ui, theme_idx),
        goto_row(&ui, theme_idx),
        stack((grid(&ui, theme_idx), inspector_panel(&ui, theme_idx), regions_panel(&ui, theme_idx))).style(|s| s.flex_row().flex_grow(1.0).min_height(0.0).width_full()),
        hash_panel(&ui, theme_idx),
        status_strip(&ui, theme_idx),
    ))
    .style(move |s| s.flex_col().size_full().background(current_theme(theme_idx.get()).bg))
}

#[cfg(test)]
mod tests {
    use super::*;
    use floem::reactive::{with_scope, Scope};

    fn run<F: FnOnce() + 'static>(f: F) {
        let scope = Scope::new();
        with_scope(scope, f);
        scope.dispose();
    }

    fn ch(c: &str) -> Key {
        Key::Character(c.into())
    }

    #[test]
    fn keys_map_to_hex_actions() {
        use HexKey::*;
        assert_eq!(map_key(&Key::Named(NamedKey::ArrowDown), false, false, false), Some(Cursor(Move::Down, false)));
        assert_eq!(map_key(&Key::Named(NamedKey::ArrowDown), false, true, false), Some(Cursor(Move::Down, true)));
        assert_eq!(map_key(&Key::Named(NamedKey::End), true, false, false), Some(Cursor(Move::DocEnd, false)));
        assert_eq!(map_key(&Key::Named(NamedKey::Home), false, false, false), Some(Cursor(Move::LineStart, false)));
        assert_eq!(map_key(&Key::Named(NamedKey::Escape), false, false, false), Some(CollapseSelection));
        assert_eq!(map_key(&Key::Named(NamedKey::Tab), false, false, false), None);
        assert_eq!(map_key(&ch("a"), false, false, false), Some(Digit('a')));
        assert_eq!(map_key(&ch("F"), false, true, false), Some(Digit('F')));
        assert_eq!(map_key(&ch("7"), false, false, false), Some(Digit('7')));
        assert_eq!(map_key(&ch("g"), false, false, false), None, "not a hex digit: passes through");
        assert_eq!(map_key(&ch("z"), true, false, false), Some(Undo));
        assert_eq!(map_key(&ch("Z"), true, true, false), Some(Redo));
        assert_eq!(map_key(&ch("y"), true, false, false), Some(Redo));
        assert_eq!(map_key(&ch("s"), true, false, false), Some(Save));
        assert_eq!(map_key(&ch("g"), true, false, false), Some(Goto));
        assert_eq!(map_key(&ch("a"), true, false, false), Some(SelectAll));
        assert_eq!(map_key(&ch("c"), true, false, false), Some(Copy));
        // global shortcuts stay global
        assert_eq!(map_key(&ch("k"), true, false, false), None);
        assert_eq!(map_key(&ch("f"), true, false, false), None);
        assert_eq!(map_key(&ch("h"), true, true, false), None);
        // AltGr (Ctrl+Alt) never types a digit nor triggers an action
        assert_eq!(map_key(&ch("a"), true, false, true), None);
        assert_eq!(map_key(&ch("1"), false, false, true), None);
    }

    #[test]
    fn hit_test_maps_pixels_to_offsets() {
        let (lh, cw) = (18.0, 8.0);
        let x_of = |col: usize| ROW_PAD + col as f64 * cw + 1.0;
        // first hex digit of byte 0 of row 2 (scrolled by 9 px, pointer 20 px from the top)
        let p = Point::new(x_of(crate::hex_view::OFFSET_COLS), 30.0);
        assert_eq!(hit_offset(p, 9.0, lh, cw, 1000), Some(32));
        // a separator, the offset column, left of the text
        assert_eq!(hit_offset(Point::new(x_of(crate::hex_view::OFFSET_COLS + 2), 5.0), 0.0, lh, cw, 1000), None);
        assert_eq!(hit_offset(Point::new(x_of(3), 5.0), 0.0, lh, cw, 1000), None);
        assert_eq!(hit_offset(Point::new(2.0, 5.0), 0.0, lh, cw, 1000), None);
        // past the end of the document
        assert_eq!(hit_offset(p, 9.0, lh, cw, 33), Some(32));
        assert_eq!(hit_offset(p, 9.0, lh, cw, 32), None);
        // ASCII part
        let a0 = crate::hex_view::OFFSET_COLS + crate::hex_view::hex_cols(16) + 2;
        assert_eq!(hit_offset(Point::new(x_of(a0 + 3), 1.0), 0.0, lh, cw, 1000), Some(3));
        assert_eq!(hit_offset(Point::new(x_of(a0), -5.0), 0.0, lh, cw, 1000), None, "above the first row");
    }

    #[test]
    fn save_all_dirty_writes_every_edited_session() {
        run(|| {
            let dir = tempfile::tempdir().unwrap();
            let (a, b) = (dir.path().join("a.bin"), dir.path().join("b.bin"));
            std::fs::write(&a, [0u8; 32]).unwrap();
            std::fs::write(&b, [0u8; 32]).unwrap();
            let ui = HexUi::new(Scope::current(), None);
            assert_eq!(ui.save_all_dirty(), Ok(0), "nothing dirty: nothing written");
            ui.activate(&a).unwrap();
            ui.with(|s| { s.select_range(0..1); s.type_hex_digit('A').unwrap(); s.type_hex_digit('B').unwrap(); });
            ui.deactivate(); // a dirty session survives
            ui.activate(&b).unwrap();
            ui.with(|s| { s.select_range(1..2); s.type_hex_digit('C').unwrap(); s.type_hex_digit('D').unwrap(); });
            assert_eq!(ui.dirty_count(), 2);
            assert!(ui.any_dirty());
            assert_eq!(ui.save_all_dirty(), Ok(2));
            assert!(!ui.any_dirty());
            assert_eq!(std::fs::read(&a).unwrap()[0], 0xAB);
            assert_eq!(std::fs::read(&b).unwrap()[1], 0xCD);
        });
    }

    /// Timing of a 100 MB save (run with --ignored --nocapture).
    #[test]
    #[ignore]
    fn save_100mb_timing() {
        run(|| {
            let dir = tempfile::tempdir().unwrap();
            let p = dir.path().join("big.bin");
            std::fs::write(&p, vec![7u8; 100 << 20]).unwrap();
            let ui = HexUi::new(Scope::current(), None);
            ui.activate(&p).unwrap();
            ui.with(|s| { s.select_range(50_000_000..50_000_001); s.type_hex_digit('A').unwrap(); s.type_hex_digit('B').unwrap(); });
            let t = std::time::Instant::now();
            ui.save_all_dirty().unwrap();
            println!("SAVE_100MB_MS={}", t.elapsed().as_millis());
        });
    }

    #[test]
    fn progress_and_status_text() {
        assert_eq!(progress_text(0, 0), "Hashing...");
        assert_eq!(progress_text(50, 200), "Hashing... 25%");
        assert_eq!(progress_text(300, 200), "Hashing... 100%");
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("a.bin");
        std::fs::write(&p, [0u8; 64]).unwrap();
        let mut s = HexSession::open(&p, None).unwrap();
        assert_eq!(status_line(&s), "Offset 0x00000000 | LE | 64 B");
        s.select_range(4..8);
        s.type_hex_digit('A').unwrap();
        let line = status_line(&s);
        assert!(line.contains("type the low digit") && line.contains("unsaved"), "{line}");
        s.select_range(16..32);
        assert!(status_line(&s).contains("Sel 0x10-0x1F (16 B)"));
    }

    #[test]
    fn command_list_is_consistent_with_copy_formats() {
        for f in CopyFormat::ALL {
            assert!(COMMANDS.iter().any(|(id, _)| *id == f.command_id()), "{:?} missing from COMMANDS", f);
        }
        let mut ids: Vec<&str> = COMMANDS.iter().map(|(i, _)| *i).collect();
        ids.sort();
        ids.dedup();
        assert_eq!(ids.len(), COMMANDS.len(), "duplicate command id");
        assert!(COMMANDS.iter().all(|(id, cap)| id.starts_with("hex_") && cap.starts_with("Hex: ")));
    }

    #[test]
    fn session_flow_through_the_ui_state_and_commands() {
        run(|| {
            let dir = tempfile::tempdir().unwrap();
            let p = dir.path().join("a.bin");
            std::fs::write(&p, (0..=255u8).collect::<Vec<u8>>()).unwrap();
            let ui = HexUi::new(Scope::current(), Some(dir.path().join("reg")));
            assert!(!ui.is_active());
            assert!(!ui.handle_key(&ch("a"), false, false, false), "inactive: not consumed");
            assert!(!ui.command("file_save"), "inactive: shared commands go to the document");
            ui.activate(&p).unwrap();
            assert!(ui.is_active());
            // move, type, undo through the key path
            assert!(ui.handle_key(&Key::Named(NamedKey::ArrowRight), false, false, false));
            assert_eq!(ui.with(|s| s.cursor()), Some(1));
            assert!(ui.handle_key(&ch("f"), false, false, false));
            assert!(ui.handle_key(&ch("f"), false, false, false));
            assert_eq!(ui.with(|s| s.cursor()), Some(2));
            assert!(ui.any_dirty());
            assert!(ui.handle_key(&ch("z"), true, false, false));
            assert_eq!(ui.with(|s| s.cursor()), Some(1));
            assert_eq!(ui.with(|s| s.doc().can_undo()), Some(false));
            // goto prompt: a text field open blocks the grid keys
            ui.open_goto();
            assert!(!ui.handle_key(&ch("a"), false, false, false));
            ui.goto_text.set("0x20 + 4".to_string());
            ui.field_focus.set(true); // what the focused goto field leaves behind
            ui.submit_goto();
            assert!(!ui.goto_open.get_untracked());
            assert!(!ui.field_focus.get_untracked(), "submitting the goto releases the grid keys");
            assert_eq!(ui.with(|s| s.cursor()), Some(0x24));
            assert!(ui.handle_key(&Key::Named(NamedKey::ArrowRight), false, false, false), "arrows work right after a goto");
            assert_eq!(ui.with(|s| s.cursor()), Some(0x25));
            ui.with(|s| s.click(0x24, false));
            ui.open_goto();
            ui.goto_text.set("0x5000".to_string());
            ui.submit_goto();
            assert!(ui.goto_open.get_untracked() && !ui.goto_err.get_untracked().is_empty(), "error shown, prompt stays open");
            ui.goto_open.set(false);
            // field focus (region name) blocks the grid
            ui.field_focus.set(true);
            assert!(!ui.handle_key(&ch("a"), false, false, false));
            ui.field_focus.set(false);
            // region from selection, then a shared command is routed to the bytes
            ui.with(|s| s.select_range(8..16));
            ui.region_name.set("header".to_string());
            ui.command("hex_add_region");
            assert_eq!(ui.with(|s| s.regions().len()), Some(1));
            assert!(ui.command("edit_select_all"));
            assert_eq!(ui.with(|s| s.selection()), Some(Some(0..256)));
            assert!(ui.command("hex_toggle_endian"));
            assert_eq!(ui.with(|s| s.endian.label()), Some("BE"));
            // deactivate drops a clean session, keeps a dirty one
            let clean = HexUi::new(Scope::current(), None);
            clean.activate(&p).unwrap();
            clean.deactivate();
            assert!(!clean.is_active() && !clean.any_dirty());
            ui.deactivate();
            assert!(!ui.is_active());
            assert!(ui.any_dirty(), "a session that was edited is kept");
        });
    }

    #[test]
    fn panel_views_build() {
        run(|| {
            let dir = tempfile::tempdir().unwrap();
            let p = dir.path().join("a.bin");
            std::fs::write(&p, vec![7u8; 100]).unwrap();
            let ui = HexUi::new(Scope::current(), None);
            ui.activate(&p).unwrap();
            ui.regions_on.set(true);
            let theme = create_rw_signal(0);
            let _v = hex_editor_panel(ui.clone(), theme);
            let empty_ui = HexUi::new(Scope::current(), None);
            let _w = hex_editor_panel(empty_ui, theme);
        });
    }
}
