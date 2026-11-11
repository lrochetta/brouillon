//! Non-modal update bar under the tabs (driven by `update_boot::Ui`). Plain text; buttons only when
//! there is a choice: Update now / Later / Skip this version.

use crate::theme::current_theme;
use crate::update_boot::{BannerKind, Ui};
use floem::reactive::{RwSignal, SignalGet};
use floem::style::CursorStyle;
use floem::views::{button, dyn_container, empty, label, stack, Decorators};
use floem::IntoView;
use std::rc::Rc;

fn text_button(theme: RwSignal<usize>, caption: &'static str, primary: bool, on: impl Fn() + 'static) -> impl IntoView {
    button(label(move || caption.to_string()))
        .action(on)
        .style(move |s| {
            let t = current_theme(theme.get());
            s.padding_horiz(10.0)
                .padding_vert(3.0)
                .margin_left(8.0)
                .border(1.0)
                .border_radius(3.0)
                .border_color(t.border)
                .background(if primary { t.accent } else { t.bg })
                .color(if primary { t.bg } else { t.fg })
                .cursor(CursorStyle::Pointer)
        })
}

pub fn banner_view(ui: Option<Rc<Ui>>, theme: RwSignal<usize>) -> impl IntoView {
    let Some(ui) = ui else { return empty().into_any() };
    let banner = ui.banner;
    dyn_container(
        move || banner.get().map(|b| b.kind),
        move |kind| {
            let Some(kind) = kind else { return empty().into_any() };
            let mut buttons: Vec<floem::AnyView> = Vec::new();
            match kind {
                BannerKind::Ready => {
                    let (u, l, s) = (ui.clone(), ui.clone(), ui.clone());
                    buttons.push(text_button(theme, "Update now", true, move || u.update_now()).into_any());
                    buttons.push(text_button(theme, "Later", false, move || l.later()).into_any());
                    buttons.push(text_button(theme, "Skip this version", false, move || s.skip()).into_any());
                }
                BannerKind::Info { skippable } => {
                    let (l, s) = (ui.clone(), ui.clone());
                    buttons.push(text_button(theme, if skippable { "Later" } else { "Dismiss" }, true, move || l.later()).into_any());
                    if skippable {
                        buttons.push(text_button(theme, "Skip this version", false, move || s.skip()).into_any());
                    }
                }
                BannerKind::Downloading | BannerKind::Required => {}
            }
            stack((
                label(move || banner.get().map(|b| b.text).unwrap_or_default()).style(move |s| {
                    let t = current_theme(theme.get());
                    s.flex_grow(1.0).padding_left(8.0).color(t.fg)
                }),
                floem::views::stack_from_iter(buttons).style(|s| s.flex_row().items_center()),
            ))
            .style(move |s| {
                let t = current_theme(theme.get());
                s.flex_row().items_center().width_full().padding_vert(5.0).padding_horiz(6.0).background(t.bg_chrome).border_bottom(1.0).border_color(t.accent)
            })
            .into_any()
        },
    )
    .style(|s| s.width_full())
    .into_any()
}
