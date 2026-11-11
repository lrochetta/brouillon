#![allow(dead_code)]

//! Command Palette — Cmd-K homepage (Rachel UX vision §3).
//!
//! Data-layer only ; main.rs wires the overlay + key binding (Round 3 merge).
//!
//! Design notes (G2 — simplicity first):
//!   - State is built around `RwSignal<T>` so the overlay view re-renders
//!     reactively as the user types and presses arrow keys. The non-reactive
//!     `entries: Vec<PaletteEntry>` is owned data — registration happens once
//!     at startup (default actions) and possibly again at runtime (recent
//!     files, dynamic symbols) ; mutation is exclusive (the merger stashes a
//!     `RefCell<PaletteState>` or `Arc<Mutex<PaletteState>>` if needed).
//!   - `filtered()` allocates a `Vec<&PaletteEntry>` on each call. With the
//!     ≤200-entry budget specified by the round brief this is a few KB and
//!     well below a frame's worth of allocator pressure. Caching is a
//!     premature optimisation here.
//!   - `fuzzy_score` is hand-rolled to avoid pulling in `nucleo` or
//!     `fuzzy-matcher` — both would land deps in a crate that already keeps
//!     a tight inventory. The algorithm is documented inline above the fn.

use floem::reactive::{create_rw_signal, RwSignal, SignalGet, SignalUpdate};

/// The kind of palette entry — drives icon + behavior at the wire-up layer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PaletteKind {
    Action,
    File,
    Symbol,
    Theme,
    Setting,
}

/// One palette item — registered at startup or dynamically (e.g. open files).
#[derive(Debug, Clone)]
pub struct PaletteEntry {
    /// Stable id used by the action dispatcher (e.g. `"file.open"`).
    pub id: &'static str,
    /// User-visible primary text.
    pub title: String,
    /// Optional dimmed hint shown at the right of the row (e.g. `"Ctrl-F"`).
    pub shortcut_hint: Option<String>,
    pub kind: PaletteKind,
}

impl PaletteEntry {
    fn action(id: &'static str, title: &str, shortcut: &str) -> Self {
        Self {
            id,
            title: title.to_string(),
            shortcut_hint: Some(shortcut.to_string()),
            kind: PaletteKind::Action,
        }
    }
}

/// Shared, reactive palette state. Construct via `new()` and clone the
/// signals into the view layer ; the entry list itself is owned here.
pub struct PaletteState {
    pub visible: RwSignal<bool>,
    pub query: RwSignal<String>,
    /// Index into the *filtered* list (0-based). Reset to 0 on open and
    /// clamped on every `move_selection` / `accept` call.
    pub selected: RwSignal<usize>,
    entries: Vec<PaletteEntry>,
}

impl Default for PaletteState {
    fn default() -> Self {
        Self::new()
    }
}

impl PaletteState {
    pub fn new() -> Self {
        Self {
            visible: create_rw_signal(false),
            query: create_rw_signal(String::new()),
            selected: create_rw_signal(0),
            entries: default_actions(),
        }
    }

    /// Replace every entry (e.g. with `entries_from_registry`).
    pub fn set_entries(&mut self, entries: Vec<PaletteEntry>) {
        self.entries = entries;
    }

    pub fn register(&mut self, entry: PaletteEntry) {
        self.entries.push(entry);
    }

    pub fn register_many(&mut self, entries: impl IntoIterator<Item = PaletteEntry>) {
        self.entries.extend(entries);
    }

    /// All entries, unfiltered. Insertion-ordered.
    pub fn all(&self) -> &[PaletteEntry] {
        &self.entries
    }

    /// Entries matching the current query, in match-quality order
    /// (highest score first). Empty query returns all entries unchanged,
    /// preserving insertion order so the user sees a stable list on open.
    pub fn filtered(&self) -> Vec<&PaletteEntry> {
        let q = self.query.get();
        if q.is_empty() {
            return self.entries.iter().collect();
        }
        let mut scored: Vec<(i32, &PaletteEntry)> = self
            .entries
            .iter()
            .filter_map(|e| fuzzy_score(&q, &e.title).map(|s| (s, e)))
            .collect();
        // Sort by score descending; ties broken by insertion order
        // (sort_by is stable, so equal scores keep their original order).
        scored.sort_by(|a, b| b.0.cmp(&a.0));
        scored.into_iter().map(|(_, e)| e).collect()
    }

    /// Move selection by `delta` (negative = up). Clamped to `[0, len-1]`
    /// of the *filtered* list. No-op if the filtered list is empty.
    pub fn move_selection(&self, delta: i32) {
        let len = self.filtered().len();
        if len == 0 {
            self.selected.set(0);
            return;
        }
        let cur = self.selected.get() as i32;
        let next = (cur + delta).max(0).min(len as i32 - 1) as usize;
        self.selected.set(next);
    }

    /// Returns the currently selected entry, if any. Cloned so the caller
    /// can keep it past the next mutation of `entries`.
    pub fn accept(&self) -> Option<PaletteEntry> {
        let list = self.filtered();
        if list.is_empty() {
            return None;
        }
        let idx = self.selected.get().min(list.len() - 1);
        list.get(idx).map(|e| (*e).clone())
    }

    /// Open the palette, reset query + selection so each Cmd-K starts fresh.
    pub fn open(&self) {
        self.query.set(String::new());
        self.selected.set(0);
        self.visible.set(true);
    }

    /// Close the palette. Query is intentionally NOT cleared on close so a
    /// stray Escape doesn't lose the user's typing — `open()` does the reset.
    pub fn close(&self) {
        self.visible.set(false);
    }

    /// Toggle visibility (used by Ctrl-K binding). Opening goes through
    /// `open()` so query + selection are reset ; closing is a plain hide.
    pub fn toggle(&self) {
        if self.visible.get() {
            self.close();
        } else {
            self.open();
        }
    }
}

/// Default action set registered at startup. The wire-up code may add more
/// (recent files, dynamic symbols) at runtime via `register_many`.
///
/// IDs are stable contracts — main.rs dispatches on them. Renaming an id is
/// a breaking change to the action wire-up layer.
pub fn default_actions() -> Vec<PaletteEntry> {
    vec![
        PaletteEntry::action("file.open", "Open File...", "Ctrl-O"),
        PaletteEntry::action("file.save", "Save", "Ctrl-S"),
        PaletteEntry::action("file.save-as", "Save As...", "Ctrl-Shift-S"),
        PaletteEntry::action("file.new-tab", "New Tab", "Ctrl-N"),
        PaletteEntry::action("file.close-tab", "Close Tab", "Ctrl-W"),
        PaletteEntry::action("edit.find", "Find", "Ctrl-F"),
        PaletteEntry::action("edit.replace", "Replace", "Ctrl-H"),
        PaletteEntry::action("edit.go-to-line", "Go to Line...", "Ctrl-G"),
        PaletteEntry::action("view.theme.cycle", "Cycle Theme", "Ctrl-T"),
        PaletteEntry::action("theme.dark", "Theme: Dark", ""),
        PaletteEntry::action("theme.light", "Theme: Light", ""),
        PaletteEntry::action("theme.system", "Theme: Follow System", ""),
        PaletteEntry::action(
            "view.bracket-style.cycle",
            "Cycle Bracket Highlighting Style",
            "Ctrl-Shift-P",
        ),
        PaletteEntry::action(
            "view.toggle-mode",
            "Toggle Moldu / Geek Mode",
            "Ctrl-Shift-M",
        ),
        PaletteEntry::action(
            "view.toggle-crayon",
            "Toggle Clavier / Crayon (Sketch) Mode",
            "Ctrl-Shift-D",
        ),
        PaletteEntry::action(
            "view.toggle-preview",
            "Toggle Markdown Preview",
            "Ctrl-Shift-V",
        ),
        PaletteEntry::action("view.command-palette", "Command Palette", "Ctrl-K"),
        PaletteEntry::action("view.toggle-ai", "Toggle AI Assistant Panel", "Ctrl-Alt-A"),
        PaletteEntry::action("view.toggle-sftp", "Toggle SFTP Remote Panel", "Ctrl-Alt-S"),
        PaletteEntry::action("help.about", "About / Credits...", ""),
        PaletteEntry::action("app.quit", "Quit", "Ctrl-Q"),
    ]
}

/// Palette entries for every registered command available in `ctx`, with the
/// effective chord as hint. Ids are leaked once (`PaletteEntry::id` is
/// `&'static str`); call at startup / registry change only, not per frame.
pub fn entries_from_registry(
    reg: &brouillon_core::command_registry::Registry,
    bindings: &[brouillon_core::keymap::Binding],
    ctx: &brouillon_core::keymap::Context,
) -> Vec<PaletteEntry> {
    use brouillon_core::keymap::{chord_display, chords_for};
    reg.available(ctx)
        .into_iter()
        .map(|s| PaletteEntry {
            id: Box::leak(s.id.clone().into_boxed_str()),
            title: s.caption.clone(),
            shortcut_hint: chords_for(bindings, &s.id).first().map(|c| chord_display(c)),
            kind: PaletteKind::Action,
        })
        .collect()
}

/// Fuzzy match score — higher is better. `None` means no match.
///
/// Algorithm: case-insensitive character-subsequence match. We walk both
/// strings in tandem ; each query char must appear in order somewhere in the
/// candidate. Score accumulates +1 per matched char, +3 bonus when the
/// previous candidate char was also matched (consecutive run), and +2 bonus
/// when the match is at the start of the candidate. Worst case is
/// `O(query_len * candidate_len)` but in practice it's a single linear scan
/// since we never backtrack — early-exit on first non-match returns `None`.
pub fn fuzzy_score(query: &str, candidate: &str) -> Option<i32> {
    if query.is_empty() {
        // Empty query is handled by `filtered()` — defensively return Some(0)
        // so any direct caller treats an empty query as "matches anything".
        return Some(0);
    }
    let q: Vec<char> = query.chars().flat_map(|c| c.to_lowercase()).collect();
    let c: Vec<char> = candidate.chars().flat_map(|c| c.to_lowercase()).collect();

    let mut qi = 0usize;
    let mut score: i32 = 0;
    let mut prev_matched = false;
    let mut first_match_idx: Option<usize> = None;

    for (i, ch) in c.iter().enumerate() {
        if qi < q.len() && *ch == q[qi] {
            score += 1;
            if prev_matched {
                score += 3;
            }
            if first_match_idx.is_none() {
                first_match_idx = Some(i);
            }
            qi += 1;
            prev_matched = true;
        } else {
            prev_matched = false;
        }
    }

    if qi < q.len() {
        return None;
    }
    if first_match_idx == Some(0) {
        score += 2;
    }
    Some(score)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registry_entries_carry_effective_chord_hint() {
        use brouillon_core::{command_registry::Registry, contract::CommandSpec, keymap::*};
        let mut reg = Registry::new();
        let spec = |id: &str, when: Option<&str>| CommandSpec {
            id: id.into(),
            caption: format!("File: {id}"),
            category: "File".into(),
            default_chord: None,
            when: when.map(Into::into), ..Default::default()
        };
        reg.register_all(vec![spec("file_save", None), spec("hidden", Some("never"))]);
        let b = vec![Binding::new("ctrl+shift+s", "file_save", None).unwrap()];
        let entries = entries_from_registry(&reg, &b, &Context::new());
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].id, "file_save");
        assert_eq!(entries[0].shortcut_hint.as_deref(), Some("Ctrl+Shift+S"));
    }

    #[test]
    fn new_state_default_is_closed_and_empty_query() {
        let s = PaletteState::new();
        assert!(!s.visible.get(), "palette should start closed");
        assert!(s.query.get().is_empty(), "query should start empty");
        assert_eq!(s.selected.get(), 0, "selection should start at 0");
    }

    #[test]
    fn default_actions_contains_known_ids() {
        let actions = default_actions();
        let must_have = [
            "file.open",
            "file.save",
            "file.save-as",
            "file.new-tab",
            "file.close-tab",
            "edit.find",
            "edit.replace",
            "edit.go-to-line",
            "view.theme.cycle",
            "view.toggle-mode",
            "view.toggle-preview",
            "view.command-palette",
            "app.quit",
        ];
        assert!(
            actions.len() >= 12,
            "expected >=12 default actions, got {}",
            actions.len()
        );
        for id in must_have {
            assert!(
                actions.iter().any(|e| e.id == id),
                "missing default action: {id}"
            );
        }
    }

    #[test]
    fn empty_query_returns_all_entries() {
        let s = PaletteState::new();
        let all = s.all().len();
        let filtered = s.filtered().len();
        assert_eq!(all, filtered, "empty query must return every entry");
    }

    #[test]
    fn fuzzy_score_finds_subsequence() {
        // "save" is a strict subsequence of "Save As..."
        assert!(fuzzy_score("save", "Save As...").is_some());
        // Non-contiguous subsequence still matches.
        assert!(fuzzy_score("sva", "Save As...").is_some());
    }

    #[test]
    fn fuzzy_score_is_case_insensitive() {
        let a = fuzzy_score("SAVE", "save as...").unwrap();
        let b = fuzzy_score("save", "Save As...").unwrap();
        assert_eq!(a, b, "case must not affect score");
    }

    #[test]
    fn fuzzy_score_returns_none_for_non_match() {
        assert!(fuzzy_score("xyz", "Save As...").is_none());
        // Order matters — chars must appear in sequence.
        assert!(fuzzy_score("eva", "Save").is_none());
    }

    #[test]
    fn filtered_orders_by_score_descending() {
        let s = PaletteState::new();
        s.query.set("save".to_string());
        let list = s.filtered();
        assert!(!list.is_empty(), "'save' should match at least one entry");
        // "Save" (exact prefix, contiguous) must outrank "Save As..." which
        // outranks anything that merely contains s-a-v-e as a subsequence.
        let top = list[0];
        assert_eq!(
            top.id, "file.save",
            "'Save' should win over 'Save As...' (shorter + contiguous start)"
        );
    }

    #[test]
    fn move_selection_clamps_to_filtered_list() {
        let s = PaletteState::new();
        // No query -> all default actions in the filtered list.
        let len = s.filtered().len();
        assert!(len >= 12);

        s.move_selection(-5); // would go negative
        assert_eq!(s.selected.get(), 0, "selection must clamp at 0");

        s.move_selection((len as i32) + 100); // would overshoot
        assert_eq!(
            s.selected.get(),
            len - 1,
            "selection must clamp at len-1"
        );

        s.move_selection(-1);
        assert_eq!(s.selected.get(), len - 2);
    }

    #[test]
    fn accept_returns_none_when_filtered_empty() {
        let s = PaletteState::new();
        s.query.set("zzz-no-match-zzz".to_string());
        assert!(
            s.filtered().is_empty(),
            "sanity: query should match nothing"
        );
        assert!(s.accept().is_none(), "accept on empty list must be None");
    }

    #[test]
    fn accept_returns_selected_entry() {
        let s = PaletteState::new();
        // Empty query -> filtered == all entries, in insertion order.
        let first_id = s.all()[0].id;
        let accepted = s.accept().expect("first entry must be accept-able");
        assert_eq!(accepted.id, first_id);
    }

    #[test]
    fn toggle_flips_visibility() {
        let s = PaletteState::new();
        assert!(!s.visible.get());
        s.toggle();
        assert!(s.visible.get(), "toggle should open from closed");
        s.toggle();
        assert!(!s.visible.get(), "toggle should close from open");
    }

    #[test]
    fn open_resets_query_and_selection() {
        let s = PaletteState::new();
        s.query.set("stale".to_string());
        s.selected.set(5);
        s.open();
        assert!(s.visible.get());
        assert!(s.query.get().is_empty(), "open() must clear query");
        assert_eq!(s.selected.get(), 0, "open() must reset selection");
    }

    #[test]
    fn register_appends_entries() {
        let mut s = PaletteState::new();
        let before = s.all().len();
        s.register(PaletteEntry {
            id: "test.entry",
            title: "Test Entry".to_string(),
            shortcut_hint: None,
            kind: PaletteKind::Action,
        });
        assert_eq!(s.all().len(), before + 1);
    }

    #[test]
    fn register_many_appends_entries() {
        let mut s = PaletteState::new();
        let before = s.all().len();
        s.register_many(vec![
            PaletteEntry {
                id: "test.entry1",
                title: "Test Entry 1".to_string(),
                shortcut_hint: None,
                kind: PaletteKind::Action,
            },
            PaletteEntry {
                id: "test.entry2",
                title: "Test Entry 2".to_string(),
                shortcut_hint: None,
                kind: PaletteKind::Action,
            },
        ]);
        assert_eq!(s.all().len(), before + 2);
    }

    #[test]
    fn fuzzy_score_exact_match_wins() {
        let exact = fuzzy_score("open", "Open File...").unwrap();
        let substring = fuzzy_score("open", "Some open element").unwrap();
        assert!(exact > substring, "exact prefix match should outscore substring");
    }

    #[test]
    fn fuzzy_score_empty_query() {
        assert_eq!(fuzzy_score("", "anything"), Some(0));
    }
}
