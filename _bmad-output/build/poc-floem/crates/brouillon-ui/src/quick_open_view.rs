//! Quick open panel (Ctrl+P, BK-071): the floating palette over the engine in
//! `quick_open.rs`. The model (`QuickOpenModel`) is plain data and unit-tested;
//! the view is a thin floem layer in the same style as the command palette.
//!
//! Keyboard contract: the field takes the focus when the panel is built and
//! swallows every key (`on_event_stop`), so nothing typed while it is open
//! reaches the document. Enter accepts, Up/Down move.
//!
//! Escape: floem's `text_input` handles it itself (drops the focus, no `FocusLost`,
//! the key never reaches `on_event_stop`), so the shell closes the panel on the
//! Escape key-up it still receives (`QuickOpenState::close`, same for the palette).

use std::path::{Path, PathBuf};
use std::rc::Rc;

use floem::event::{Event, EventListener};
use floem::keyboard::{Key, NamedKey};
use floem::reactive::{create_effect, create_rw_signal, RwSignal, SignalGet, SignalUpdate, SignalWith};
use floem::views::{container, dyn_container, dyn_stack, empty, label, stack, text_input, Decorators};
use floem::IntoView;

use crate::quick_open::{Hit, QuickOpen, Symbol};
use crate::theme_ctx::active_theme;

const ROW_HEIGHT: f64 = 28.0;
const PANEL_WIDTH: f64 = 600.0;
const FIELD_HEIGHT: f64 = 32.0;
/// Rows shown at once (the list does not scroll).
pub const MAX_ROWS: usize = 12;
/// Upper bound of files listed from the folder, so a huge tree cannot stall the open.
pub const MAX_FILES: usize = 5000;

/// One displayed row: a main text and a dimmed complement.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Row {
    pub title: String,
    pub detail: String,
}

/// Results, selection and everything the rows need. Built when the panel opens.
#[derive(Clone)]
pub struct QuickOpenModel {
    data: Rc<QuickOpen>,
    root: PathBuf,
    total_lines: usize,
    hits: Vec<Hit>,
    selected: usize,
}

impl QuickOpenModel {
    pub fn new(data: QuickOpen, root: PathBuf, total_lines: usize) -> Self {
        let mut m = Self { data: Rc::new(data), root, total_lines, hits: Vec::new(), selected: 0 };
        m.set_query("");
        m
    }

    pub fn empty() -> Self {
        Self::new(QuickOpen::new(Vec::new(), Vec::new(), Vec::new()), PathBuf::new(), 1)
    }

    /// Recompute the results for the raw input; the selection goes back to the first row.
    /// An empty input lists the recent files, then the folder's files.
    pub fn set_query(&mut self, input: &str) {
        self.selected = 0;
        self.hits = if input.trim().is_empty() {
            let mut v: Vec<PathBuf> = Vec::new();
            for p in self.data.mru.iter().chain(self.data.files.iter()) {
                if !v.contains(p) {
                    v.push(p.clone());
                }
                if v.len() >= MAX_ROWS {
                    break;
                }
            }
            v.into_iter().map(Hit::File).collect()
        } else {
            self.data.search(input, MAX_ROWS)
        };
    }

    pub fn hits(&self) -> &[Hit] {
        &self.hits
    }

    pub fn selected(&self) -> usize {
        self.selected
    }

    /// Move the selection by `delta`, clamped to the list.
    pub fn move_selection(&mut self, delta: i32) {
        if self.hits.is_empty() {
            self.selected = 0;
            return;
        }
        self.selected = (self.selected as i32 + delta).clamp(0, self.hits.len() as i32 - 1) as usize;
    }

    pub fn set_selected(&mut self, i: usize) {
        if i < self.hits.len() {
            self.selected = i;
        }
    }

    /// The selected result, if any.
    pub fn accept(&self) -> Option<Hit> {
        self.hits.get(self.selected).cloned()
    }

    /// Text announced in the status bar (and shown in the footer).
    pub fn announcement(&self) -> String {
        match self.hits.len() {
            0 => "Quick open: no result".to_string(),
            1 => "Quick open: 1 result".to_string(),
            n => format!("Quick open: {n} results"),
        }
    }

    pub fn rows(&self) -> Vec<Row> {
        self.hits.iter().map(|h| describe(h, &self.root, self.total_lines)).collect()
    }
}

/// Title and detail of a result row.
pub fn describe(hit: &Hit, root: &Path, total_lines: usize) -> Row {
    match hit {
        Hit::File(p) => {
            let rel = p.strip_prefix(root).unwrap_or(p);
            let name = p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            let dir = rel.parent().map(|d| d.to_string_lossy().replace('\\', "/")).unwrap_or_default();
            Row { title: name, detail: dir }
        }
        Hit::Goto { line, col } => {
            let title = match col {
                Some(c) => format!("Go to line {line}, column {c}"),
                None => format!("Go to line {line}"),
            };
            Row { title, detail: format!("document has {total_lines} line(s)") }
        }
        Hit::Symbol(s) => Row { title: s.name.clone(), detail: format!("line {}", s.line) },
        Hit::Text(t) => Row { title: format!("Search this document for \"{t}\""), detail: String::new() },
    }
}

/// Files of `root` (`.gitignore` respected, hidden files skipped), at most `MAX_FILES`.
pub fn list_files(root: &Path) -> Vec<PathBuf> {
    let mut v = Vec::new();
    for e in ignore::WalkBuilder::new(root).build().flatten() {
        if e.file_type().is_some_and(|t| t.is_file()) {
            v.push(e.into_path());
            if v.len() >= MAX_FILES {
                break;
            }
        }
    }
    v.sort();
    v
}

/// Declarations of the document for `@symbol` / Go to Function (engine-side outline, language
/// picked from the file extension).
pub fn extract_symbols(text: &str, path: Option<&std::path::Path>) -> Vec<Symbol> {
    brouillon_editor_engine::symbols::extract(text, path)
        .into_iter()
        .map(|s| Symbol { name: s.name, line: s.line })
        .collect()
}

/// [`extract_symbols`] straight from the document's rope (no whole-text String).
pub fn extract_symbols_rope(rope: &crop::Rope, path: Option<&std::path::Path>) -> Vec<Symbol> {
    brouillon_editor_engine::symbols::extract_rope(rope, path)
        .into_iter()
        .map(|s| Symbol { name: s.name, line: s.line })
        .collect()
}

/// Reactive state shared between the shell (opens it) and the view.
#[derive(Clone, Copy)]
pub struct QuickOpenState {
    pub visible: RwSignal<bool>,
    pub query: RwSignal<String>,
    pub model: RwSignal<QuickOpenModel>,
}

impl QuickOpenState {
    pub fn new() -> Self {
        Self {
            visible: create_rw_signal(false),
            query: create_rw_signal(String::new()),
            model: create_rw_signal(QuickOpenModel::empty()),
        }
    }

    /// Open with fresh data (recent files, folder files, symbols of the active document).
    pub fn open(&self, data: QuickOpen, root: PathBuf, total_lines: usize) {
        self.query.set(String::new());
        self.model.set(QuickOpenModel::new(data, root, total_lines));
        self.visible.set(true);
    }

    pub fn close(&self) {
        self.visible.set(false);
    }
}

impl Default for QuickOpenState {
    fn default() -> Self {
        Self::new()
    }
}

/// The floating panel. `on_hit` receives the accepted result (the panel has closed);
/// `announce(Some(text))` while open and `announce(None)` once closed.
pub fn quick_open_overlay(
    state: QuickOpenState,
    on_hit: Rc<dyn Fn(Hit)>,
    announce: Rc<dyn Fn(Option<String>)>,
) -> impl IntoView {
    let visible = state.visible;
    let announce_close = announce.clone();
    create_effect(move |was_open: Option<bool>| {
        let v = visible.get();
        if was_open == Some(true) && !v {
            announce_close(None);
        }
        v
    });
    dyn_container(
        move || visible.get(),
        move |is_visible| {
            if is_visible {
                panel_body(state, on_hit.clone(), announce.clone()).into_any()
            } else {
                empty().into_any()
            }
        },
    )
    .style(|s| s.absolute().inset_top(80.0).inset_left_pct(50.0).margin_left((-PANEL_WIDTH / 2.0) as f32))
}

fn panel_body(state: QuickOpenState, on_hit: Rc<dyn Fn(Hit)>, announce: Rc<dyn Fn(Option<String>)>) -> impl IntoView {
    let QuickOpenState { visible, query, model } = state;

    create_effect(move |_| {
        let q = query.get();
        model.update(|m| m.set_query(&q));
    });
    create_effect(move |_| {
        let text = model.with(|m| m.announcement());
        announce(Some(text));
    });

    let field = text_input(query)
        .placeholder("File name, :line, @symbol or #text")
        // Built when the panel opens: take the keyboard so typing lands here, not in the document.
        .request_focus(|| {})
        .style(|s| {
            let t = active_theme();
            s.width(PANEL_WIDTH - 16.0)
                .height(FIELD_HEIGHT)
                .padding_horiz(8.0)
                .padding_vert(4.0)
                .background(t.bg)
                .border(1.0)
                .border_color(t.fg_dim)
                .border_radius(2.0)
                .color(t.fg)
        })
        .on_event_stop(EventListener::KeyDown, {
            let on_hit = on_hit.clone();
            move |event| {
                let Event::KeyDown(ke) = event else { return };
                match &ke.key.logical_key {
                    Key::Named(NamedKey::ArrowDown) => model.update(|m| m.move_selection(1)),
                    Key::Named(NamedKey::ArrowUp) => model.update(|m| m.move_selection(-1)),
                    Key::Named(NamedKey::Escape) => visible.set(false),
                    Key::Named(NamedKey::Enter) => {
                        if let Some(hit) = model.with_untracked(|m| m.accept()) {
                            visible.set(false);
                            on_hit(hit);
                        }
                    }
                    Key::Character(c) if ke.modifiers.control() && c.eq_ignore_ascii_case("p") => visible.set(false),
                    _ => {}
                }
            }
        });

    let list = dyn_stack(
        move || model.with(|m| m.rows()).into_iter().enumerate().collect::<Vec<_>>(),
        |(i, r): &(usize, Row)| (*i, r.title.clone(), r.detail.clone()),
        move |(i, r): (usize, Row)| {
            let on_hit = on_hit.clone();
            row_view(i, r, model).on_click_stop(move |_| {
                let hit = model.with_untracked(|m| m.hits().get(i).cloned());
                if let Some(h) = hit {
                    visible.set(false);
                    on_hit(h);
                }
            })
        },
    )
    .style(|s| s.flex_col().width_full());

    let empty_note = label(|| "No match".to_string()).style(move |s| {
        let none = model.with(|m| m.hits().is_empty());
        let s = s.padding_horiz(8.0).height(ROW_HEIGHT).items_center().color(active_theme().fg_dim);
        if none { s } else { s.hide() }
    });
    let footer = label(move || model.with(|m| m.announcement()))
        .style(|s| s.color(active_theme().fg_dim).padding_horiz(8.0).font_size(11.0));

    container(stack((field, list, empty_note, footer)).style(|s| s.flex_col().gap(6.0).width_full()))
        .style(|s| {
            let t = active_theme();
            s.padding(8.0)
                .width(PANEL_WIDTH)
                .background(t.bg_chrome)
                .color(t.fg)
                .border(1.0)
                .border_color(t.accent)
                .border_radius(4.0)
        })
}

fn row_view(i: usize, row: Row, model: RwSignal<QuickOpenModel>) -> impl IntoView {
    let (title, detail) = (row.title, row.detail);
    let title_label = label(move || title.clone()).style(|s| s.color(active_theme().fg));
    let detail_label = label(move || detail.clone()).style(|s| s.color(active_theme().fg_dim).padding_left(10.0));
    container(stack((title_label, detail_label)).style(|s| s.flex_row().items_center().width_full()))
        .style(move |s| {
            let base = s.height(ROW_HEIGHT).width_full().padding_horiz(8.0).items_center();
            if model.with(|m| m.selected()) == i {
                base.background(active_theme().bg_current_line)
            } else {
                base
            }
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use floem::reactive::{with_scope, Scope};

    fn p(s: &str) -> PathBuf {
        PathBuf::from(s)
    }

    fn model() -> QuickOpenModel {
        let data = QuickOpen::new(
            vec![p("/w/src/recent.rs")],
            vec![p("/w/src/main.rs"), p("/w/README.md"), p("/w/src/recent.rs"), p("/w/src/util.rs")],
            vec![Symbol { name: "parse_query".into(), line: 10 }],
        );
        QuickOpenModel::new(data, p("/w"), 12)
    }

    #[test]
    fn empty_query_lists_recent_then_folder_files_without_duplicates() {
        let m = model();
        assert_eq!(m.hits()[0], Hit::File(p("/w/src/recent.rs")));
        assert_eq!(m.hits().len(), 4);
        assert_eq!(m.hits().iter().filter(|h| **h == Hit::File(p("/w/src/recent.rs"))).count(), 1);
    }

    #[test]
    fn line_prefix_gives_one_goto_row_and_enter_returns_it() {
        let mut m = model();
        m.set_query(":5");
        assert_eq!(m.accept(), Some(Hit::Goto { line: 5, col: None }));
        m.set_query(":5:3");
        assert_eq!(m.accept(), Some(Hit::Goto { line: 5, col: Some(3) }));
        assert_eq!(m.rows()[0].title, "Go to line 5, column 3");
        assert_eq!(m.rows()[0].detail, "document has 12 line(s)");
    }

    #[test]
    fn file_query_filters_and_text_prefix_searches() {
        let mut m = model();
        m.set_query("main");
        assert_eq!(m.accept(), Some(Hit::File(p("/w/src/main.rs"))));
        m.set_query("#todo");
        assert_eq!(m.accept(), Some(Hit::Text("todo".into())));
        m.set_query("@pq");
        assert_eq!(m.accept(), Some(Hit::Symbol(Symbol { name: "parse_query".into(), line: 10 })));
    }

    #[test]
    fn selection_is_clamped_and_reset_by_a_new_query() {
        let mut m = model();
        m.move_selection(-1);
        assert_eq!(m.selected(), 0);
        m.move_selection(99);
        assert_eq!(m.selected(), 3);
        m.set_selected(1);
        assert_eq!(m.selected(), 1);
        m.set_selected(50);
        assert_eq!(m.selected(), 1, "out of range is ignored");
        m.set_query("rs");
        assert_eq!(m.selected(), 0);
    }

    #[test]
    fn no_result_means_no_accept_and_an_announcement() {
        let mut m = model();
        m.set_query("zzzz");
        assert!(m.accept().is_none());
        assert_eq!(m.announcement(), "Quick open: no result");
        m.move_selection(1);
        assert_eq!(m.selected(), 0);
        m.set_query(":9");
        assert_eq!(m.announcement(), "Quick open: 1 result");
        m.set_query("");
        assert_eq!(m.announcement(), "Quick open: 4 results");
    }

    #[test]
    fn file_rows_show_name_and_relative_folder() {
        let r = describe(&Hit::File(p("/w/src/editor/mod.rs")), &p("/w"), 1);
        assert_eq!(r, Row { title: "mod.rs".into(), detail: "src/editor".into() });
        let r = describe(&Hit::File(p("/elsewhere/a.txt")), &p("/w"), 1);
        assert_eq!(r.title, "a.txt");
    }

    #[test]
    fn symbols_come_from_the_engine_outline_by_extension() {
        let text = "# Title
use x;
pub fn alpha() {}
    def beta(self):
class Gamma:
export async function delta() {}
";
        let names = |p: Option<&str>| -> Vec<(String, usize)> {
            extract_symbols(text, p.map(std::path::Path::new)).into_iter().map(|s| (s.name, s.line)).collect()
        };
        assert_eq!(names(None), vec![("alpha".to_string(), 3), ("beta".to_string(), 4), ("Gamma".to_string(), 5), ("delta".to_string(), 6)]);
        assert_eq!(names(Some("n.md")), vec![("Title".to_string(), 1)]);
    }

    #[test]
    fn list_files_respects_the_cap_and_skips_directories() {
        let dir = std::env::temp_dir().join(format!("brouillon-qo-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("sub")).unwrap();
        std::fs::write(dir.join("a.txt"), "a").unwrap();
        std::fs::write(dir.join("sub").join("b.txt"), "b").unwrap();
        let files = list_files(&dir);
        assert_eq!(files.len(), 2);
        assert!(files.iter().all(|f| f.is_file()));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn state_open_resets_and_close_hides() {
        let scope = Scope::new();
        with_scope(scope, || {
            let st = QuickOpenState::new();
            st.query.set("old".into());
            st.open(QuickOpen::new(vec![p("a.rs")], Vec::new(), Vec::new()), p("."), 3);
            assert!(st.visible.get_untracked());
            assert_eq!(st.query.get_untracked(), "");
            assert_eq!(st.model.with_untracked(|m| m.hits().len()), 1);
            st.close();
            assert!(!st.visible.get_untracked());
            let on_hit: Rc<dyn Fn(Hit)> = Rc::new(|_| {});
            let ann: Rc<dyn Fn(Option<String>)> = Rc::new(|_| {});
            let _ = quick_open_overlay(st, on_hit, ann);
        });
        scope.dispose();
    }
}
