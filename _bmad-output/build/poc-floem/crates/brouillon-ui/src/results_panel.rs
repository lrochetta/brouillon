//! Lot 4 — Find in Files results panel (BK-024): grouped by file, foldable,
//! click to open at line:col. Rendering only; all logic is in `ResultsModel`.

use floem::reactive::{RwSignal, SignalGet, SignalUpdate, SignalWith};
use floem::style::CursorStyle;
use floem::views::{dyn_stack, label, scroll, stack, Decorators};
use floem::IntoView;

use brouillon_core::theme::current_theme;

use crate::results_model::{OpenRequest, ResultsModel};

/// One rendered line: a file header (`entry == None`) or a hit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PanelRow {
    pub key: String,
    pub group: usize,
    pub entry: Option<usize>,
    pub text: String,
    pub selected: bool,
    pub collapsed: bool,
}

/// Flatten the active search into rows (pure, unit-tested).
pub fn rows(model: &ResultsModel) -> Vec<PanelRow> {
    let Some(set) = model.active() else { return Vec::new() };
    let visible = set.visible();
    let cursor = model.cursor();
    let mut out = Vec::new();
    for (g, grp) in set.groups.iter().enumerate() {
        out.push(PanelRow {
            key: format!("h{g}:{}", grp.collapsed),
            group: g,
            entry: None,
            text: format!("{} {} ({})", if grp.collapsed { "▸" } else { "▾" }, grp.path.display(), grp.entries.len()),
            selected: false,
            collapsed: grp.collapsed,
        });
        for e in visible.iter().filter(|p| p.0 == g).map(|p| p.1) {
            let ent = &grp.entries[e];
            let selected = cursor == Some((g, e));
            out.push(PanelRow {
                key: format!("e{g}:{e}:{selected}"),
                group: g,
                entry: Some(e),
                text: format!("  {:>5}: {}", ent.line + 1, ent.text.trim()),
                selected,
                collapsed: false,
            });
        }
    }
    out
}

fn copy_to_clipboard(text: String) {
    if let Ok(mut c) = arboard::Clipboard::new() {
        let _ = c.set_text(text);
    }
}

fn toolbar_button(
    caption: &'static str,
    theme_idx: RwSignal<usize>,
    on: impl Fn() + 'static,
) -> impl IntoView {
    label(move || caption.to_string())
        .on_click_stop(move |_| on())
        .style(move |s| {
            let t = current_theme(theme_idx.get());
            s.padding_horiz(6.0)
                .font_size(11.0)
                .color(t.fg_status)
                .cursor(CursorStyle::Pointer)
                .hover(move |h| h.color(t.accent))
        })
}

/// The panel. `on_open` is called with the target of a clicked hit; the shell
/// opens the file and moves the caret (it should also call it for F4/Shift+F4
/// results returned by `ResultsModel::next/prev`).
pub fn results_panel(
    model: RwSignal<ResultsModel>,
    theme_idx: RwSignal<usize>,
    on_open: impl Fn(OpenRequest) + Clone + 'static,
) -> impl IntoView {
    let header = stack((
        label(move || model.with(|m| m.status())).style(|s| s.flex_grow(1.0)),
        toolbar_button("Fold all", theme_idx, move || model.update(|m| m.set_all_folded(true))),
        toolbar_button("Unfold", theme_idx, move || model.update(|m| m.set_all_folded(false))),
        toolbar_button("1/line", theme_idx, move || {
            model.update(|m| {
                let on = !m.active().is_some_and(|s| s.one_per_line);
                m.set_one_per_line(on);
            })
        }),
        toolbar_button("Copy", theme_idx, move || copy_to_clipboard(model.with(|m| m.copy_text()))),
        toolbar_button("Clear", theme_idx, move || model.update(|m| m.clear())),
    ))
    .style(move |s| {
        let t = current_theme(theme_idx.get());
        s.width_full()
            .height(22.0)
            .items_center()
            .padding_horiz(8.0)
            .font_size(11.0)
            .color(t.fg_status)
            .background(t.bg_chrome)
            .border_bottom(1.0)
            .border_color(t.border)
    });

    let list = dyn_stack(
        move || model.with(rows),
        |r: &PanelRow| r.key.clone(),
        move |r: PanelRow| {
            let on_open = on_open.clone();
            let (g, e, selected, is_header) = (r.group, r.entry, r.selected, r.entry.is_none());
            label(move || r.text.clone())
                .on_click_stop(move |_| match e {
                    None => model.update(|m| m.toggle_fold(g)),
                    Some(e) => {
                        if let Some(req) = model.try_update(|m| m.select(g, e)).flatten() {
                            on_open(req);
                        }
                    }
                })
                .style(move |s| {
                    let t = current_theme(theme_idx.get());
                    let s = s
                        .width_full()
                        .padding_horiz(8.0)
                        .padding_vert(1.0)
                        .font_size(12.0)
                        .cursor(CursorStyle::Pointer)
                        .color(if is_header { t.fg } else { t.fg_dim })
                        .hover(move |h| h.background(t.fg.multiply_alpha(0.06)));
                    if selected {
                        s.background(t.accent.multiply_alpha(0.15))
                    } else {
                        s
                    }
                })
        },
    )
    .style(|s| s.flex_col().width_full());

    stack((header, scroll(list).style(|s| s.width_full().flex_grow(1.0).min_height(0.0))))
        .style(move |s| {
            let t = current_theme(theme_idx.get());
            s.flex_col().width_full().height(200.0).background(t.bg).border_top(1.0).border_color(t.border)
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use brouillon_editor_engine::search_files::{FileResult, Hit};
    use std::path::PathBuf;

    fn model() -> ResultsModel {
        let mut m = ResultsModel::new();
        m.begin_search("q");
        let h = |line| Hit { line, col: 0, start_byte: 0, end_byte: 0, line_text: "  foo ".into(), matched: "foo".into() };
        m.add_file(&FileResult { path: PathBuf::from("a.rs"), hits: vec![h(0), h(6)], from_buffer: false });
        m
    }

    #[test]
    fn rows_group_headers_and_hits() {
        let mut m = model();
        let r = rows(&m);
        assert_eq!(r.len(), 3);
        assert!(r[0].entry.is_none() && r[0].text.contains("a.rs (2)"));
        assert_eq!(r[2].text, "      7: foo");
        m.next();
        assert!(rows(&m)[1].selected);
        m.toggle_fold(0);
        assert_eq!(rows(&m).len(), 1);
        assert!(rows(&m)[0].collapsed);
    }
}
