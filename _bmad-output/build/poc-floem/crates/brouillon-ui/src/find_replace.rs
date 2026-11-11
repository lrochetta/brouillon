//! Find/Replace overlay. M1 alpha feature — uses the search engine in
//! `crate::search`. Single overlay state per editor (no multi-pane yet).
//!
//! Layout (top to bottom, when visible):
//!   +------------------------------------------------------+
//!   |  [Find: ______________________]  [aA] [.*]  3/27  Up Down X |
//!   |  [Repl: ______________________]  [Replace] [Replace All]    |
//!   +------------------------------------------------------+
//!
//! Design (G2 — simplicity first):
//!   - State is a bundle of `RwSignal`s on the heap (`Arc<FindReplaceState>`)
//!     so we can clone the handle into both the keyboard closure (main.rs)
//!     and the overlay view callbacks without juggling lifetimes.
//!   - The view is a `dyn_container` keyed on `visible` so the overlay
//!     vanishes from the layout tree when off (no stale paint), but the
//!     state outlives it (query/replacement text is preserved across toggles).
//!   - We materialize the regex on every action (refresh / next / prev /
//!     replace_*) rather than caching a compiled `Search`. The compile cost
//!     is microseconds on the small patterns people type and the
//!     simpler API is worth it pre-M1. Day-2 can cache on the state if a
//!     profile shows it matters.
//!   - `prev()` is implemented by collecting all matches (cheap on the
//!     materialized rope: one alloc, no per-match line/col math because the
//!     engine already does it once). `search.rs` doesn't expose a reverse
//!     search and the regex crate's NFA can't run backward. This is
//!     acceptable for M1 — wrap-around is rare and ergonomic on PoC sizes.
//!
//! Buffer mutation is deliberately NOT in this module. `replace_current` /
//! `replace_all` return the new string; the caller (eventually a buffer
//! undo group) decides when to commit. Today nothing commits — the view
//! buttons are wired but mutate signals only. The merger lands the buffer
//! mutation hook in a follow-up so the four scopes stay disjoint.

#![allow(dead_code)]

use std::cell::RefCell;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use floem::keyboard::{Key, NamedKey};
use floem::event::{Event, EventListener, EventPropagation};
use crate::theme_ctx::active_theme;
use floem::reactive::{create_effect, create_rw_signal, RwSignal, SignalGet, SignalUpdate, Trigger};
use floem::views::{
    button, container, dyn_container, empty, label, stack, text_input, Decorators,
};
use floem::IntoView;

use brouillon_editor_engine::buffer::Buffer;
use brouillon_editor_engine::search::live::{self, Direction, Job};
use brouillon_editor_engine::search::{to_match, to_match_hay, CancelToken, Match, Matcher, Search, SearchMode, SearchOptions};

/// Shared mutable state for the Find/Replace overlay. All fields are
/// `RwSignal` so the view re-renders reactively. Construct via `new()`
/// and stash in an `Arc` — the same handle is shared by the key binding
/// in main.rs and the overlay view callbacks.
pub struct FindReplaceState {
    pub visible: RwSignal<bool>,
    pub query: RwSignal<String>,
    pub replacement: RwSignal<String>,
    /// false = wraps query in `(?i)` before compiling.
    pub case_sensitive: RwSignal<bool>,
    /// false = treats query as a literal (regex metacharacters are escaped).
    pub regex_mode: RwSignal<bool>,
    pub current_match: RwSignal<Option<Match>>,
    pub match_count: RwSignal<usize>,
    /// Round-4 alpha.4 wire-up: the Replace / Replace All buttons set this
    /// signal with the post-mutation text. main.rs subscribes via a
    /// `create_effect` and applies it to the active buffer (swaps the
    /// `RwSignal<Arc<Buffer>>`), then clears the slot back to None.
    /// Decoupled this way so find_replace.rs stays unaware of how the
    /// host stores the buffer state.
    pub pending_replacement: RwSignal<Option<String>>,
    /// True while a big-buffer count is still running: the label shows "N..." instead of "?/N".
    pub searching: RwSignal<bool>,
    live: Arc<Live>,
    trigger: Trigger,
    /// (query, case, regex, buffer length) of the last search started, so a key-up that changed
    /// nothing (Shift, arrows, Enter) does not restart a scan.
    last_key: RefCell<Option<(String, bool, bool, usize)>>,
}

/// Buffers up to this size are searched inline exactly as before; bigger ones go to a worker
/// thread so typing and painting never wait for a scan.
pub const SYNC_LIMIT: usize = 2 << 20;
/// Pause after the first match of a query before its full count starts.
pub const DEBOUNCE: Duration = Duration::from_millis(100);

/// What a worker last reported for one lane.
struct Update {
    target: Option<Match>,
    target_known: bool,
    count: usize,
    done: bool,
}

/// One cancellable job lane: only the newest job of a lane may publish.
#[derive(Default)]
struct Slot {
    gen: AtomicU64,
    cancel: Mutex<CancelToken>,
    inbox: Mutex<Option<Update>>,
}

#[derive(Default)]
struct Live {
    /// Query-change job: first match + running total count.
    count: Arc<Slot>,
    /// Next / previous job: one match, no count.
    nav: Arc<Slot>,
    /// A next/previous ran after the last refresh: its match wins over the refresh's first match.
    nav_since_refresh: AtomicBool,
}

impl Slot {
    fn cancel_now(&self) {
        self.gen.fetch_add(1, Ordering::SeqCst);
        self.cancel.lock().unwrap().cancel();
        *self.inbox.lock().unwrap() = None;
    }

    /// Start `job` on a worker; whatever ran before in this lane is cancelled.
    fn launch<H: live::Hay + Clone + Send + 'static>(self: &Arc<Self>, trigger: Trigger, rope: H, m: Matcher, job: Job) {
        let gen = self.gen.fetch_add(1, Ordering::SeqCst) + 1;
        let cancel = CancelToken::new();
        std::mem::replace(&mut *self.cancel.lock().unwrap(), cancel.clone()).cancel();
        *self.inbox.lock().unwrap() = None;
        let slot = self.clone();
        let rope_for_lines = rope.clone();
        live::spawn(rope, Arc::new(m), job, cancel, move |p| {
            if slot.gen.load(Ordering::SeqCst) != gen {
                return;
            }
            *slot.inbox.lock().unwrap() = Some(Update {
                target: p.target.clone().map(|r| to_match_hay(&rope_for_lines, r)),
                target_known: p.target_known,
                count: p.count,
                done: p.done,
            });
            floem::ext_event::register_ext_trigger(trigger);
        });
    }
}

impl Default for FindReplaceState {
    fn default() -> Self {
        Self::new()
    }
}

impl FindReplaceState {
    pub fn new() -> Self {
        let live: Arc<Live> = Arc::default();
        let trigger = Trigger::new();
        let current_match = create_rw_signal(None);
        let match_count = create_rw_signal(0);
        let searching = create_rw_signal(false);
        {
            // UI side of the workers: they store an update and wake this effect; nothing polls.
            let live = live.clone();
            create_effect(move |_| {
                trigger.track();
                if let Some(u) = live.count.inbox.lock().unwrap().take() {
                    if u.target_known && !live.nav_since_refresh.load(Ordering::SeqCst) {
                        current_match.set(u.target);
                    }
                    match_count.set(u.count);
                    searching.set(!u.done);
                }
                if let Some(u) = live.nav.inbox.lock().unwrap().take() {
                    current_match.set(u.target);
                }
            });
        }
        Self {
            visible: create_rw_signal(false),
            query: create_rw_signal(String::new()),
            replacement: create_rw_signal(String::new()),
            case_sensitive: create_rw_signal(false),
            regex_mode: create_rw_signal(false),
            current_match,
            match_count,
            pending_replacement: create_rw_signal(None),
            searching,
            live,
            trigger,
            last_key: RefCell::new(None),
        }
    }

    fn is_big(buffer: &Buffer) -> bool {
        // A large-file-mode (lazy) buffer is always searched on a worker, over a snapshot of its pieces.
        buffer.is_lazy() || buffer.byte_len() > SYNC_LIMIT
    }

    /// Re-run the search against the buffer with the current query + flags.
    /// Updates `match_count` and (if non-empty) `current_match` to the first
    /// match. An empty query or invalid regex sets count=0, current=None.
    ///
    /// Small buffers are searched inline. Big ones (see [`SYNC_LIMIT`]) are searched on a worker
    /// thread after a [`DEBOUNCE`] pause: the first match shows at once, `match_count` climbs while
    /// `searching` is true, and a newer query cancels the older scan.
    pub fn refresh(&self, buffer: &Buffer) {
        if self.query.get().is_empty() {
            self.cancel_all();
            self.match_count.set(0);
            self.current_match.set(None);
            return;
        }
        if Self::is_big(buffer) {
            self.refresh_async(buffer);
            return;
        }
        self.cancel_all();
        match self.compiled() {
            Ok(s) => {
                self.match_count.set(s.count_matches(buffer));
                self.current_match.set(s.find_next(buffer, 0));
            }
            Err(_) => {
                // Invalid regex — show 0/0. Future UI surfaces the error
                // string under the find box.
                self.match_count.set(0);
                self.current_match.set(None);
            }
        }
    }

    fn cancel_all(&self) {
        self.live.count.cancel_now();
        self.live.nav.cancel_now();
        *self.last_key.borrow_mut() = None;
        self.searching.set(false);
    }

    fn refresh_async(&self, buffer: &Buffer) {
        let key = (self.query.get(), self.case_sensitive.get(), self.regex_mode.get(), buffer.byte_len());
        if self.last_key.borrow().as_ref() == Some(&key) {
            return; // same search as the one already started or finished
        }
        *self.last_key.borrow_mut() = Some(key);
        let Ok(m) = self.matcher() else {
            self.live.count.cancel_now();
            self.searching.set(false);
            self.match_count.set(0);
            self.current_match.set(None);
            return;
        };
        self.live.nav_since_refresh.store(false, Ordering::SeqCst);
        self.match_count.set(0);
        self.searching.set(true);
        // First match at once; the count starts after DEBOUNCE unless a newer query cancels it.
        let job = Job { dir: Direction::Next(0), count: true, count_delay: DEBOUNCE };
        self.live.count.launch(self.trigger, buffer.snapshot(), m, job);
    }

    /// Jump to the next match (wraps around at EOF).
    pub fn next(&self, buffer: &Buffer) {
        let from = self.current_match.get().map(|m| m.end_byte).unwrap_or(0);
        if Self::is_big(buffer) {
            self.nav_async(buffer, Direction::Next(from));
            return;
        }
        let Ok(s) = self.compiled() else { return };
        let next = s.find_next(buffer, from).or_else(|| {
            // Wrap to BOF if we ran off the end.
            s.find_next(buffer, 0)
        });
        self.current_match.set(next);
    }

    /// Jump to the previous match (wraps around at BOF). Scans backward from the current match
    /// and stops at the first hit: no full collection of the matches.
    pub fn prev(&self, buffer: &Buffer) {
        let before = self.current_match.get().map(|m| m.start_byte).unwrap_or(usize::MAX);
        if Self::is_big(buffer) {
            self.nav_async(buffer, Direction::Prev(before));
            return;
        }
        let Ok(s) = self.compiled() else { return };
        let rope = buffer.rope();
        let found = s
            .matcher()
            .find_prev(rope, before, true, &CancelToken::new())
            .ok()
            .flatten()
            .map(|f| to_match(rope, f.range));
        self.current_match.set(found);
    }

    fn nav_async(&self, buffer: &Buffer, dir: Direction) {
        let Ok(m) = self.matcher() else { return };
        self.live.nav_since_refresh.store(true, Ordering::SeqCst);
        self.live.nav.launch(self.trigger, buffer.snapshot(), m, Job { dir, count: false, count_delay: Duration::ZERO });
    }

    /// Replace the `current_match` with the replacement string. Returns
    /// the modified text — the caller decides whether to commit to the
    /// buffer. (Buffer mutation API is a follow-up.) Returns `None` if
    /// there is no current match.
    pub fn replace_current(&self, buffer: &Buffer) -> Option<String> {
        let m = self.current_match.get()?;
        if buffer.is_lazy() {
            // Large file mode: the replaced text of a whole document is never built (it would be a full copy).
            return None;
        }
        let text = buffer.text();
        if m.end_byte > text.len() {
            return None;
        }
        let repl = self.replacement.get();
        // Splice: text[..start] + replacement + text[end..]. Pre-allocates
        // an exact-fit String to avoid grow-and-copy.
        let mut out = String::with_capacity(text.len() + repl.len());
        out.push_str(&text[..m.start_byte]);
        out.push_str(&repl);
        out.push_str(&text[m.end_byte..]);
        Some(out)
    }

    /// Replace all matches in the buffer. Returns `(new_text, count)`.
    /// On empty query or invalid regex, returns the original text and 0.
    pub fn replace_all(&self, buffer: &Buffer) -> (String, usize) {
        if buffer.is_lazy() {
            return (String::new(), 0); // large file mode: see `replace_current`
        }
        if self.query.get().is_empty() {
            return (buffer.text(), 0);
        }
        match self.compiled() {
            Ok(s) => s.replace_all(buffer, &self.replacement.get()),
            Err(_) => (buffer.text(), 0),
        }
    }

    /// Resolve the active regex from query + flags.
    ///
    /// - When `regex_mode` is false, escapes metacharacters via `regex::escape`
    ///   so the query is treated as a literal substring.
    /// - When `case_sensitive` is false, prepends `(?i)` so the inline flag
    ///   turns on case folding for the whole pattern.
    fn compiled(&self) -> Result<Search, String> {
        Search::new(&self.pattern()).map_err(|e| e.to_string())
    }

    /// Same pattern as [`Self::compiled`], as an owned [`Matcher`] for a worker thread.
    fn matcher(&self) -> Result<Matcher, String> {
        let opts = SearchOptions { mode: SearchMode::Regex, ..SearchOptions::default() };
        Matcher::new(&self.pattern(), &opts).map_err(|e| e.to_string())
    }

    fn pattern(&self) -> String {
        let raw = self.query.get();
        let body = if self.regex_mode.get() {
            raw
        } else {
            regex::escape(&raw)
        };
        if self.case_sensitive.get() {
            body
        } else {
            format!("(?i){body}")
        }
    }
}

// --------------------------------------------------------------------------
// View
// --------------------------------------------------------------------------

/// Build the overlay view. Caller is responsible for stacking it above the
/// editor body — typically by passing it as the last child of the root
/// `stack(...)` so it paints last and (via `Style::absolute()`) floats over
/// the editor body.
pub fn find_replace_overlay(
    state: Arc<FindReplaceState>,
    buffer_sig: RwSignal<Arc<Buffer>>,
) -> impl IntoView {
    let visible = state.visible;

    // `dyn_container` swaps its child reactively. When `visible == false`
    // we render `empty()`, which has zero size and renders nothing — so
    // the overlay completely disappears from the layout tree.
    dyn_container(
        move || visible.get(),
        move |is_visible| {
            if is_visible {
                overlay_body(state.clone(), buffer_sig).into_any()
            } else {
                empty().into_any()
            }
        },
    )
    .style(|s| {
        s.absolute()
            .inset_top(OVERLAY_TOP)
            .inset_right(12.0)
    })
}

/// Top of the overlay: below the whole chrome (header stripe ~19 + menu bar +
/// toolbar + tab strip, measured 107 px at 175 % DPI). At 28 px it covered the
/// toolbar's Crayon button.
pub const OVERLAY_TOP: f64 = 112.0;

/// The visible overlay: two rows (find + replace) inside an accent-bordered
/// rounded card. Pure view; all reactive wiring lives in the signal updates.
fn overlay_body(
    state: Arc<FindReplaceState>,
    buffer_sig: RwSignal<Arc<Buffer>>,
) -> impl IntoView {
    let s = state.clone();
    let find_row = build_find_row(s, buffer_sig);

    let s = state.clone();
    let replace_row = build_replace_row(s, buffer_sig);

    container(
        stack((find_row, replace_row))
            .style(|s| s.flex_col().gap(6.0)),
    )
    .style(|s| {
        let t = active_theme();
        s.padding(8.0)
            .background(t.bg_chrome)
            .color(t.fg)
            .border(1.0)
            .border_color(t.accent)
            .border_radius(4.0)
            // Catch Escape inside the overlay so users can dismiss it
            // even when a text input is focused.
            .min_width(420.0)
    })
    .on_event_stop(EventListener::KeyDown, {
        let visible = state.visible;
        move |event| {
            if let Event::KeyDown(ke) = event {
                if ke.key.logical_key == Key::Named(NamedKey::Escape) {
                    visible.set(false);
                }
            }
        }
    })
}

fn build_find_row(state: Arc<FindReplaceState>, buffer_sig: RwSignal<Arc<Buffer>>) -> impl IntoView {
    let query = state.query;
    let case_sig = state.case_sensitive;
    let regex_sig = state.regex_mode;
    let count_sig = state.match_count;
    let current_sig = state.current_match;
    let searching_sig = state.searching;
    let visible = state.visible;

    // Refresh on every keystroke. `text_input` writes `query` on each
    // character — but to avoid a borrow loop in the effect (`query.get()`
    // triggers, we call refresh which reads query again) we keep refresh
    // call-driven from the button + arrows + open. The text_input change
    // is wired via on_event_stop to refresh after each key.
    // buffer_sig: RwSignal<Arc<Buffer>> is Copy — captured by move into
    // each action ; reads stay live (post-Find/Replace commits pick up
    // the swapped Arc).
    let s1 = state.clone();
    let find_field = text_input(query)
        .placeholder("Find...")
        // The overlay is built when it opens: take the keyboard so typing lands here, not in the document.
        .request_focus(|| {})
        .style(field_style)
        // Enter must be consumed here: an unhandled Enter falls through to the root
        // key handler and inserts a newline in the document.
        .on_event(EventListener::KeyDown, {
            let s_enter = state.clone();
            move |event| {
                if let Event::KeyDown(ke) = event {
                    if ke.key.logical_key == Key::Named(NamedKey::Enter) {
                        let buf = buffer_sig.get_untracked();
                        s_enter.refresh(&buf);
                        if ke.modifiers.shift() { s_enter.prev(&buf) } else { s_enter.next(&buf) }
                        return EventPropagation::Stop;
                    }
                }
                EventPropagation::Continue
            }
        })
        .on_event_stop(EventListener::KeyUp, move |_| {
            s1.refresh(&buffer_sig.get_untracked());
        });

    let s_case = state.clone();
    let case_btn = button(label(move || {
        if case_sig.get() { " aA ".to_string() } else { " Aa ".to_string() }
    }))
    .action(move || {
        case_sig.update(|v| *v = !*v);
        s_case.refresh(&buffer_sig.get_untracked());
    })
    .style(|s| toggle_button_style(s));

    let s_re = state.clone();
    let regex_btn = button(label(move || {
        if regex_sig.get() { " .* ".to_string() } else { " ab ".to_string() }
    }))
    .action(move || {
        regex_sig.update(|v| *v = !*v);
        s_re.refresh(&buffer_sig.get_untracked());
    })
    .style(|s| toggle_button_style(s));

    let count_label = label(move || {
        let total = count_sig.get();
        if searching_sig.get() {
            // big file: the count is still running, show how far it got
            return format!(" {total}... ");
        }
        let pos = current_sig
            .get()
            .and_then(|m| {
                // The 1-based index of the current match — recompute by
                // counting matches with end <= m.start_byte. Cheap, since
                // count is already small per query.
                if total == 0 { None } else { Some(m) }
            });
        match (pos, total) {
            (Some(_), n) => format!(" ?/{n} "),
            (None, 0) => " 0/0 ".to_string(),
            (None, n) => format!(" 0/{n} "),
        }
    })
    .style(|s| s.color(active_theme().fps_ok).padding_horiz(6.0));

    let s_prev = state.clone();
    let prev_btn = button(label(|| " < ".to_string()))
        .action(move || s_prev.prev(&buffer_sig.get_untracked()))
        .style(|s| toggle_button_style(s));

    let s_next = state.clone();
    let next_btn = button(label(|| " > ".to_string()))
        .action(move || s_next.next(&buffer_sig.get_untracked()))
        .style(|s| toggle_button_style(s));

    let close_btn = button(label(|| " x ".to_string()))
        .action(move || visible.set(false))
        .style(|s| toggle_button_style(s));

    stack((
        find_field,
        case_btn,
        regex_btn,
        count_label,
        prev_btn,
        next_btn,
        close_btn,
    ))
    .style(|s| s.flex_row().items_center().gap(4.0))
}

fn build_replace_row(state: Arc<FindReplaceState>, buffer_sig: RwSignal<Arc<Buffer>>) -> impl IntoView {
    let replacement = state.replacement;

    let replace_field = text_input(replacement)
        .placeholder("Replace with...")
        .style(field_style)
        // Enter replaces the current match (same as the button) instead of typing a newline.
        .on_event(EventListener::KeyDown, {
            let s_enter = state.clone();
            move |event| {
                if let Event::KeyDown(ke) = event {
                    if ke.key.logical_key == Key::Named(NamedKey::Enter) {
                        if let Some(new_text) = s_enter.replace_current(&buffer_sig.get_untracked()) {
                            s_enter.pending_replacement.set(Some(new_text));
                        }
                        return EventPropagation::Stop;
                    }
                }
                EventPropagation::Continue
            }
        });

    // Round-4 alpha.4 wire-up : Replace / Replace All buttons publish
    // their post-mutation text to `state.pending_replacement`. main.rs
    // subscribes to that signal via `create_effect`, swaps the active
    // buffer (Arc::new(Buffer::from_str(&new_text))), and clears the
    // slot. This decouples find_replace.rs from how the host stores
    // buffer state.
    let s_one = state.clone();
    let replace_btn = button(label(|| " Replace ".to_string()))
        .action(move || {
            let cur = buffer_sig.get_untracked();
            if let Some(new_text) = s_one.replace_current(&cur) {
                s_one.pending_replacement.set(Some(new_text));
            }
        })
        .style(|s| toggle_button_style(s));

    let s_all = state.clone();
    let replace_all_btn = button(label(|| " Replace All ".to_string()))
        .action(move || {
            let cur = buffer_sig.get_untracked();
            let (new_text, count) = s_all.replace_all(&cur);
            if count > 0 {
                s_all.pending_replacement.set(Some(new_text));
            }
        })
        .style(|s| toggle_button_style(s));

    stack((replace_field, replace_btn, replace_all_btn))
        .style(|s| s.flex_row().items_center().gap(4.0))
}

/// Shared style for the small chips/buttons on the overlay row. Pulled out
/// so the find/replace rows stay readable.
fn toggle_button_style(s: floem::style::Style) -> floem::style::Style {
    let t = active_theme();
    s.padding_horiz(4.0)
        .padding_vert(1.0)
        .background(t.bg_current_line)
        .border(1.0)
        .border_color(t.fg_dim)
        .border_radius(2.0)
        .color(t.fg)
}

/// Text field of the overlay: page-coloured, 3:1 border, visible caret.
fn field_style(s: floem::style::Style) -> floem::style::Style {
    let t = active_theme();
    s.width(220.0)
        .padding_horiz(6.0)
        .padding_vert(2.0)
        .background(t.bg)
        .color(t.fg)
        .border(1.0)
        .border_color(t.fg_dim)
        .border_radius(2.0)
}

// --------------------------------------------------------------------------
// Tests
// --------------------------------------------------------------------------
//
// Tests focus on `FindReplaceState` logic, not the view (which needs a
// Floem reactive scope). Each test wraps its body in `Scope::new()` so
// the `RwSignal`s have somewhere to live.

#[cfg(test)]
mod tests {
    use super::*;
    use floem::reactive::{with_scope, Scope};

    #[test]
    fn overlay_sits_below_menu_toolbar_and_tabs() {
        let chrome = crate::menu_bar::MENU_HEIGHT + crate::toolbar::TOOLBAR_HEIGHT + crate::tab_bar::TAB_HEIGHT;
        assert!(OVERLAY_TOP >= chrome + 19.0, "{OVERLAY_TOP} would cover the toolbar or tabs");
    }

    /// Run a closure under a fresh reactive Scope so RwSignals have somewhere
    /// to live. `with_scope` (free fn) is the floem_reactive 0.2 spelling —
    /// `Scope::with` doesn't exist there. The closure's return type must be
    /// `'static` (the trait bound on `with_scope`), so we return `()`.
    fn run<F: FnOnce() + 'static>(f: F) {
        let scope = Scope::new();
        with_scope(scope, f);
        scope.dispose();
    }

    #[test]
    fn new_state_is_sane() {
        run(|| {
            let st = FindReplaceState::new();
            assert!(!st.visible.get());
            assert_eq!(st.query.get(), "");
            assert_eq!(st.replacement.get(), "");
            assert!(!st.case_sensitive.get());
            assert!(!st.regex_mode.get());
            assert_eq!(st.match_count.get(), 0);
            assert!(st.current_match.get().is_none());
        });
    }

    #[test]
    fn refresh_empty_query_zero_count() {
        run(|| {
            let buf = Buffer::from_str("alpha beta gamma");
            let st = FindReplaceState::new();
            st.refresh(&buf);
            assert_eq!(st.match_count.get(), 0);
            assert!(st.current_match.get().is_none());
        });
    }

    #[test]
    fn refresh_literal_query_counts_matches() {
        run(|| {
            // 100k-line sample is overkill — small buf proves the wiring.
            let buf = Buffer::from_str("foo bar foo baz foo");
            let st = FindReplaceState::new();
            st.query.set("foo".to_string());
            // case_sensitive defaults to false ; regex_mode defaults to false.
            // Either way "foo" lowercase matches "foo" lowercase three times.
            st.refresh(&buf);
            assert_eq!(st.match_count.get(), 3);
            let m = st.current_match.get().unwrap();
            assert_eq!(m.start_byte, 0);
        });
    }

    #[test]
    fn next_wraps_at_eof() {
        run(|| {
            let buf = Buffer::from_str("foo bar foo");
            let st = FindReplaceState::new();
            st.query.set("foo".to_string());
            st.refresh(&buf);

            // Initial: first "foo" at byte 0.
            assert_eq!(st.current_match.get().unwrap().start_byte, 0);

            // Advance to the second "foo" at byte 8.
            st.next(&buf);
            assert_eq!(st.current_match.get().unwrap().start_byte, 8);

            // Past the last match — wraps back to byte 0.
            st.next(&buf);
            assert_eq!(st.current_match.get().unwrap().start_byte, 0);
        });
    }

    #[test]
    fn prev_wraps_at_bof() {
        run(|| {
            let buf = Buffer::from_str("foo bar foo baz foo");
            let st = FindReplaceState::new();
            st.query.set("foo".to_string());
            st.refresh(&buf);

            // From the first match, prev should wrap to the last match (byte 16).
            st.prev(&buf);
            assert_eq!(st.current_match.get().unwrap().start_byte, 16);

            // One more prev — second-to-last (byte 8).
            st.prev(&buf);
            assert_eq!(st.current_match.get().unwrap().start_byte, 8);
        });
    }

    #[test]
    fn compiled_escapes_metacharacters_when_literal() {
        run(|| {
            let buf = Buffer::from_str("a.b a*b a+b");
            let st = FindReplaceState::new();
            st.query.set("a.b".to_string());
            // regex_mode=false: "." is escaped, so only "a.b" literal matches once.
            st.refresh(&buf);
            assert_eq!(st.match_count.get(), 1);

            // regex_mode=true: "." matches any char, so all three substrings match.
            st.regex_mode.set(true);
            st.refresh(&buf);
            assert_eq!(st.match_count.get(), 3);
        });
    }

    #[test]
    fn compiled_rejects_invalid_pattern() {
        run(|| {
            let buf = Buffer::from_str("anything");
            let st = FindReplaceState::new();
            st.regex_mode.set(true);
            // Unclosed character class — regex crate rejects this.
            st.query.set("[".to_string());
            st.refresh(&buf);
            // Invalid regex: count stays 0, current stays None.
            assert_eq!(st.match_count.get(), 0);
            assert!(st.current_match.get().is_none());
        });
    }

    #[test]
    fn case_sensitivity_toggle() {
        run(|| {
            let buf = Buffer::from_str("Foo foo FOO");
            let st = FindReplaceState::new();
            st.query.set("foo".to_string());

            // Default (case-insensitive): 3 matches.
            st.refresh(&buf);
            assert_eq!(st.match_count.get(), 3);

            // Case-sensitive: only the lowercase "foo".
            st.case_sensitive.set(true);
            st.refresh(&buf);
            assert_eq!(st.match_count.get(), 1);
        });
    }

    #[test]
    fn replace_current_splices_correctly() {
        run(|| {
            let buf = Buffer::from_str("hello world hello");
            let st = FindReplaceState::new();
            st.query.set("hello".to_string());
            st.replacement.set("HOWDY".to_string());
            st.refresh(&buf);
            let out = st.replace_current(&buf).expect("first match");
            assert_eq!(out, "HOWDY world hello");
        });
    }

    #[test]
    fn replace_all_basic() {
        run(|| {
            let buf = Buffer::from_str("hello world hello");
            let st = FindReplaceState::new();
            st.query.set("hello".to_string());
            st.replacement.set("HI".to_string());
            let (out, n) = st.replace_all(&buf);
            assert_eq!(n, 2);
            assert_eq!(out, "HI world HI");
        });
    }

    #[test]
    fn replace_all_with_empty() {
        run(|| {
            let buf = Buffer::from_str("aaa bbb aaa");
            let st = FindReplaceState::new();
            st.query.set("aaa".to_string());
            st.replacement.set(String::new());
            let (out, n) = st.replace_all(&buf);
            assert_eq!(n, 2);
            assert_eq!(out, " bbb ");
        });
    }

    #[test]
    fn replace_with_longer_text() {
        run(|| {
            let buf = Buffer::from_str("x");
            let st = FindReplaceState::new();
            st.query.set("x".to_string());
            st.replacement.set("XXXXX".to_string());
            let (out, n) = st.replace_all(&buf);
            assert_eq!(n, 1);
            assert_eq!(out, "XXXXX");
        });
    }

    #[test]
    fn empty_query_gives_zero_matches() {
        run(|| {
            let buf = Buffer::from_str("anything");
            let st = FindReplaceState::new();
            st.query.set(String::new());
            st.refresh(&buf);
            assert_eq!(st.match_count.get(), 0);
        });
    }

    #[test]
    fn unicode_search() {
        run(|| {
            let buf = Buffer::from_str("café résumé naïve");
            let st = FindReplaceState::new();
            st.query.set("é".to_string());
            st.refresh(&buf);
            assert_eq!(st.match_count.get(), 3, "Should find 3 é characters");
        });
    }

    #[test]
    fn next_advances_current_match() {
        run(|| {
            let buf = Buffer::from_str("a b a b a");
            let st = FindReplaceState::new();
            st.query.set("a".to_string());
            st.refresh(&buf);
            assert_eq!(st.match_count.get(), 3);
            let m0 = st.current_match.get();
            st.next(&buf);
            let m1 = st.current_match.get();
            assert_ne!(m0, m1);
        });
    }

    #[test]
    fn prev_moves_backward() {
        run(|| {
            let buf = Buffer::from_str("a b a b a");
            let st = FindReplaceState::new();
            st.query.set("a".to_string());
            st.refresh(&buf);
            let m_init = st.current_match.get();
            st.next(&buf);
            st.prev(&buf);
            let m_back = st.current_match.get();
            assert_eq!(m_init, m_back);
        });
    }

    #[test]
    fn replace_current_returns_none_on_no_match() {
        run(|| {
            let buf = Buffer::from_str("hello world");
            let st = FindReplaceState::new();
            st.query.set("xyz".to_string());
            st.replacement.set("abc".to_string());
            st.refresh(&buf);
            assert!(st.replace_current(&buf).is_none());
        });
    }

    #[test]
    fn regex_replace_all() {
        run(|| {
            let buf = Buffer::from_str("foo123 bar456");
            let st = FindReplaceState::new();
            st.regex_mode.set(true);
            st.query.set(r"\d+".to_string());
            st.replacement.set("NUM".to_string());
            let (out, n) = st.replace_all(&buf);
            assert_eq!(n, 2);
            assert_eq!(out, "fooNUM barNUM");
        });
    }
    // ---- big buffers: worker thread, progress, cancellation -------------------------------

    fn big_text() -> String {
        let line = "the quick brown fox jumps over the lazy dog\n";
        line.repeat(SYNC_LIMIT / line.len() + 10)
    }

    /// Wait (bounded) until the lane's inbox holds a finished update.
    fn wait_done(slot: &Slot) -> Update {
        for _ in 0..2000 {
            if let Some(u) = slot.inbox.lock().unwrap().take() {
                if u.done {
                    return u;
                }
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        panic!("job did not finish");
    }

    #[test]
    fn big_buffer_refresh_returns_at_once_and_marks_searching() {
        run(|| {
            let buf = Buffer::from_str(&big_text());
            assert!(FindReplaceState::is_big(&buf));
            let st = FindReplaceState::new();
            st.query.set("lazy".to_string());
            let t = std::time::Instant::now();
            st.refresh(&buf);
            assert!(t.elapsed() < Duration::from_millis(50), "refresh must not scan: {:?}", t.elapsed());
            assert!(st.searching.get());
            assert_eq!(st.match_count.get(), 0);
            // same search again (key-up of Shift, Enter...) is a no-op, not a restart
            st.refresh(&buf);
            assert!(st.searching.get());
            // empty query clears everything
            st.query.set(String::new());
            st.refresh(&buf);
            assert!(!st.searching.get());
        });
    }

    #[test]
    fn worker_lane_counts_like_the_inline_search() {
        run(|| {
            let buf = Buffer::from_str(&big_text());
            let st = FindReplaceState::new();
            st.query.set("LAZY".to_string());
            let expected = st.compiled().unwrap().count_matches(&buf);
            st.live.count.launch(st.trigger, buf.rope().clone(), st.matcher().unwrap(), Job { dir: Direction::Next(0), count: true, count_delay: Duration::ZERO });
            let u = wait_done(&st.live.count);
            assert_eq!(u.count, expected);
            let first = u.target.unwrap();
            assert_eq!((first.line, first.col), (1, 36));
        });
    }

    #[test]
    fn newer_job_supersedes_the_older_one() {
        run(|| {
            let buf = Buffer::from_str(&big_text());
            let st = FindReplaceState::new();
            st.query.set("fox".to_string());
            st.live.count.launch(st.trigger, buf.rope().clone(), st.matcher().unwrap(), Job { dir: Direction::Next(0), count: true, count_delay: Duration::ZERO });
            st.query.set("zzz-absent".to_string());
            st.live.count.launch(st.trigger, buf.rope().clone(), st.matcher().unwrap(), Job { dir: Direction::Next(0), count: true, count_delay: Duration::ZERO });
            let u = wait_done(&st.live.count);
            assert_eq!(u.count, 0, "the old job's counts must never surface");
            assert!(u.target.is_none());
            std::thread::sleep(Duration::from_millis(100));
            assert!(st.live.count.inbox.lock().unwrap().is_none());
        });
    }

    #[test]
    fn nav_lane_next_and_prev_on_a_big_buffer() {
        run(|| {
            let buf = Buffer::from_str(&big_text());
            let st = FindReplaceState::new();
            st.query.set("fox".to_string());
            let m = || st.matcher().unwrap();
            st.live.nav.launch(st.trigger, buf.rope().clone(), m(), Job { dir: Direction::Next(100), count: false, count_delay: Duration::ZERO });
            let n = wait_done(&st.live.nav).target.unwrap();
            assert_eq!((n.line, n.col), (3, 17));
            st.live.nav.launch(st.trigger, buf.rope().clone(), m(), Job { dir: Direction::Prev(n.start_byte), count: false, count_delay: Duration::ZERO });
            let p = wait_done(&st.live.nav).target.unwrap();
            assert_eq!((p.line, p.col), (2, 17));
        });
    }
}
