//! Compare view: two texts (buffer vs buffer, buffer vs disk) shown inline or side by side over
//! the editor. Read-only; Close (or the `diff_close` command) goes back to the document.
//! All the diffing is `brouillon_editor_engine::diff`; this file only draws it.

use std::rc::Rc;

use brouillon_core::theme::current_theme;
use brouillon_editor_engine::diff::{self, InlineRow, Kind, SideRow};
use floem::peniko::Color;
use floem::reactive::{create_rw_signal, RwSignal, SignalGet, SignalUpdate, SignalWith};
use floem::style::CursorStyle;
use floem::views::{dyn_container, empty, label, scroll, stack, virtual_stack, Decorators, VirtualDirection, VirtualItemSize};
use floem::IntoView;

const ROW_H: f64 = 18.0;

pub struct DiffData {
    pub left_title: String,
    pub right_title: String,
    pub inline: Vec<InlineRow>,
    pub side: Vec<SideRow>,
    pub added: usize,
    pub removed: usize,
    pub hunks: usize,
}

impl DiffData {
    pub fn new(left_title: &str, right_title: &str, old: &str, new: &str) -> Self {
        // One line diff feeds all three views (it used to run three times: 3x the transient memory).
        let inline = diff::inline_rows(old, new);
        let side = diff::side_by_side_from(&inline);
        let (added, removed) = diff::stats_from(&inline);
        let hunks = diff::hunk_starts(&side.iter().map(|r| r.kind).collect::<Vec<_>>()).len();
        Self { left_title: left_title.into(), right_title: right_title.into(), inline, side, added, removed, hunks }
    }

    pub fn is_identical(&self) -> bool {
        self.added == 0 && self.removed == 0
    }

    pub fn summary(&self) -> String {
        if self.is_identical() {
            "identical".to_string()
        } else {
            format!("+{} -{} in {} change{}", self.added, self.removed, self.hunks, if self.hunks == 1 { "" } else { "s" })
        }
    }
}

#[derive(Clone, Copy)]
pub struct DiffUi {
    pub data: RwSignal<Option<Rc<DiffData>>>,
    pub side_by_side: RwSignal<bool>,
}

impl DiffUi {
    pub fn new() -> Self {
        Self { data: create_rw_signal(None), side_by_side: create_rw_signal(true) }
    }
    pub fn show(&self, d: DiffData) {
        self.data.set(Some(Rc::new(d)));
    }
    pub fn close(&self) {
        self.data.set(None);
    }
    pub fn is_open(&self) -> bool {
        self.data.with_untracked(|d| d.is_some())
    }
}

impl Default for DiffUi {
    fn default() -> Self {
        Self::new()
    }
}

fn tint(kind: Kind, left: bool) -> Option<Color> {
    let red = Color::rgba8(240, 90, 90, 46);
    let green = Color::rgba8(80, 200, 120, 46);
    match (kind, left) {
        (Kind::Equal, _) => None,
        (Kind::Delete, _) => Some(red),
        (Kind::Insert, _) => Some(green),
        (Kind::Change, true) => Some(red),
        (Kind::Change, false) => Some(green),
    }
}

fn num(n: Option<usize>) -> String {
    n.map(|n| format!("{n:>5}")).unwrap_or_else(|| "     ".to_string())
}

fn half(no: Option<usize>, text: String, kind: Kind, left: bool, theme_idx: RwSignal<usize>) -> impl IntoView {
    stack((
        label(move || num(no)).style(move |s| s.width(44.0).color(current_theme(theme_idx.get()).fg_dim)),
        label(move || text.clone()).style(|s| s.padding_left(6.0)),
    ))
    .style(move |s| {
        let s = s.flex_row().width_pct(50.0).height(ROW_H).items_center();
        match tint(kind, left) {
            Some(c) => s.background(c),
            None => s,
        }
    })
}

fn inline_row(r: InlineRow, theme_idx: RwSignal<usize>) -> impl IntoView {
    let mark = match r.kind {
        Kind::Insert => "+",
        Kind::Delete => "-",
        _ => " ",
    };
    let (kind, text) = (r.kind, r.text);
    stack((
        label(move || format!("{}{}", num(r.old_no), num(r.new_no))).style(move |s| s.width(92.0).color(current_theme(theme_idx.get()).fg_dim)),
        label(move || mark.to_string()).style(|s| s.width(14.0)),
        label(move || text.clone()),
    ))
    .style(move |s| {
        let s = s.flex_row().width_full().height(ROW_H).items_center();
        match tint(kind, true) {
            Some(c) => s.background(c),
            None => s,
        }
    })
}

fn side_row(r: SideRow, theme_idx: RwSignal<usize>) -> impl IntoView {
    let (l, rr) = (r.left, r.right);
    stack((
        half(l.as_ref().map(|s| s.no), l.map(|s| s.text).unwrap_or_default(), r.kind, true, theme_idx),
        half(rr.as_ref().map(|s| s.no), rr.map(|s| s.text).unwrap_or_default(), r.kind, false, theme_idx),
    ))
    .style(|s| s.flex_row().width_full().height(ROW_H))
}

fn button(text: &'static str, active: impl Fn() -> bool + 'static, on: impl Fn() + 'static, theme_idx: RwSignal<usize>) -> impl IntoView {
    label(move || text.to_string())
        .on_click_stop(move |_| on())
        .style(move |s| {
            let t = current_theme(theme_idx.get());
            let s = s.padding_horiz(10.0).padding_vert(3.0).margin_left(6.0).cursor(CursorStyle::Pointer).color(t.fg).border(1.0).border_color(t.border).border_radius(3.0);
            if active() {
                s.background(t.accent.multiply_alpha(0.25))
            } else {
                s.hover(move |h| h.background(t.fg.multiply_alpha(0.08)))
            }
        })
}

/// The overlay: absolute, fills its (relative) parent, hidden while no comparison is open.
pub fn diff_overlay(ui: DiffUi, theme_idx: RwSignal<usize>, on_close: Rc<dyn Fn()>) -> impl IntoView {
    dyn_container(
        move || ui.data.get(),
        move |data| {
            let Some(data) = data else { return empty().into_any() };
            let header = {
                let (lt, rt, sum) = (data.left_title.clone(), data.right_title.clone(), data.summary());
                let close = on_close.clone();
                stack((
                    label(move || format!("Compare:  {lt}   <->   {rt}")).style(|s| s.flex_grow(1.0)),
                    label(move || sum.clone()).style(move |s| s.margin_right(12.0).color(current_theme(theme_idx.get()).fg_dim)),
                    button("Inline", move || !ui.side_by_side.get(), move || ui.side_by_side.set(false), theme_idx),
                    button("Side by side", move || ui.side_by_side.get(), move || ui.side_by_side.set(true), theme_idx),
                    button("Close", || false, move || close(), theme_idx),
                ))
                .style(move |s| {
                    let t = current_theme(theme_idx.get());
                    s.flex_row().items_center().width_full().padding(8.0).background(t.bg_chrome).border_bottom(1.0).border_color(t.border)
                })
            };
            let body = {
                let d = data.clone();
                dyn_container(
                    move || ui.side_by_side.get(),
                    move |side| {
                        let d = d.clone();
                        let n = if side { d.side.len() } else { d.inline.len() };
                        let rows: im::Vector<usize> = (0..n).collect();
                        scroll(
                            virtual_stack(
                                VirtualDirection::Vertical,
                                VirtualItemSize::Fixed(Box::new(|| ROW_H)),
                                move || rows.clone(),
                                |i: &usize| *i,
                                move |i: usize| {
                                    if side {
                                        side_row(d.side[i].clone(), theme_idx).into_any()
                                    } else {
                                        inline_row(d.inline[i].clone(), theme_idx).into_any()
                                    }
                                },
                            )
                            .style(|s| s.flex_col().width_full()),
                        )
                        .style(|s| s.size_full())
                        .into_any()
                    },
                )
                .style(|s| s.flex_grow(1.0).min_height(0.0).width_full())
            };
            stack((header, body))
                .style(move |s| {
                    let t = current_theme(theme_idx.get());
                    s.flex_col().size_full().background(t.bg).font_size(13.0).color(t.fg)
                })
                .into_any()
        },
    )
    // Hidden while nothing is open: an invisible full-size overlay would swallow every click
    // meant for the editor underneath (pointer events go to the topmost view only).
    .style(move |s| if ui.data.with(|d| d.is_some()) { s.absolute().inset(0.0).z_index(60) } else { s.hide() })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn summary_counts_changes_and_detects_identical() {
        let d = DiffData::new("a", "b", "x\ny\nz\n", "x\nY\nz\nw\n");
        assert_eq!((d.added, d.removed, d.hunks), (2, 1, 2));
        assert_eq!(d.summary(), "+2 -1 in 2 changes");
        assert!(DiffData::new("a", "b", "same\n", "same\n").is_identical());
        assert_eq!(DiffData::new("a", "b", "same\n", "same\n").summary(), "identical");
    }
}
