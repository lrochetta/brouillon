//! UI side of the optional modules: the non-modal offer bar under the tabs, the Install click, the exit hook.
//!
//! Nothing here runs at startup beyond storing a path. The manager (and the modules folder) is first touched
//! when the active tab is a file kind that has a module (a PDF, a Word file...). The only network access is the
//! thread started by the Install button.

use std::path::PathBuf;
use std::rc::Rc;
use std::sync::{Mutex, MutexGuard, OnceLock};

use floem::ext_event::create_ext_action;
use floem::reactive::{create_memo, RwSignal, Scope, SignalGet, SignalUpdate, SignalWith};
use floem::style::CursorStyle;
use floem::views::{button, dyn_container, empty, label, stack, Decorators};
use floem::IntoView;

use brouillon_core::docview::{route, ModuleState, Route};
use brouillon_core::kind::{self, Detection, FileKind};
use brouillon_modules::{Consent, Entry, ModuleError, ModuleManager, DEFAULT_BASE_URL};
use brouillon_update::manifest::UrlPolicy;

use crate::external_ui::Ctx;
use crate::theme::current_theme;

static ROOT: OnceLock<PathBuf> = OnceLock::new();
static MANAGER: OnceLock<Mutex<ModuleManager>> = OnceLock::new();

/// Remember where modules live (`<data folder>/modules`) and where the saved "always open as text" list is read.
/// Stores a path and a function pointer only: no I/O.
pub fn configure(modules_dir: PathBuf) {
    let _ = ROOT.set(modules_dir);
    kind::set_always_text_loader(|| manager().map(|mut m| m.always_text()).unwrap_or_default());
}

fn manager<'a>() -> Option<MutexGuard<'a, ModuleManager>> {
    let root = ROOT.get()?;
    let m = MANAGER.get_or_init(|| {
        let build = crate::buildinfo::BUILD.parse::<u64>().unwrap_or(0);
        Mutex::new(ModuleManager::new(root.clone(), brouillon_update::keys::embedded(), build))
    });
    Some(m.lock().unwrap_or_else(|e| e.into_inner()))
}

/// Application exit: release every loaded module (no module process exists yet; the host asks them to quit first).
pub fn on_exit() {
    if let Some(m) = MANAGER.get() {
        m.lock().unwrap_or_else(|e| e.into_inner()).shutdown_all();
    }
}

fn module_title(id: &str) -> &'static str {
    match id {
        "docs" => "Documents module",
        _ => "optional module",
    }
}

/// What the bar shows for the active tab.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Bar {
    None,
    Offer { module: &'static str, kind: FileKind },
    Installing { module: &'static str },
    Failed { module: &'static str, kind: FileKind, why: String },
    Loaded { module: &'static str, version: String },
}

fn bar_for(kind: FileKind, dismissed: &[&'static str]) -> Bar {
    let Some(module) = kind.module() else { return Bar::None };
    if kind::is_always_text(kind) {
        return Bar::None;
    }
    let Some(mut m) = manager() else { return Bar::None };
    let det = Detection { kind, mislabelled: false, evidence: "tab" };
    let reg = brouillon_core::docview::registry().lock().unwrap_or_else(|e| e.into_inner());
    let st = m.state(module);
    let r = route(&det, &reg, false, &|_| st.clone());
    drop(reg);
    match st {
        ModuleState::Installing => Bar::Installing { module },
        ModuleState::Failed(why) if !dismissed.contains(&module) => Bar::Failed { module, kind, why },
        ModuleState::Ready | ModuleState::Loaded if kind.routed_to_viewer() => match m.request(module) {
            Ok(l) => Bar::Loaded { module, version: l.version },
            Err(e) => Bar::Failed { module, kind, why: e.to_string() },
        },
        _ => match r {
            Route::Fallback { offer: true, .. } if !dismissed.contains(&module) => {
                m.offer(module);
                Bar::Offer { module, kind }
            }
            _ => Bar::None,
        },
    }
}

fn bar_button(ctx: Ctx, caption: &'static str, primary: bool, on: impl Fn() + 'static) -> impl IntoView {
    button(label(move || caption.to_string()))
        .action(on)
        .style(move |s| {
            let t = current_theme(ctx.theme.get());
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

/// The offer bar. `on_always_text` re-opens the active file as plain text (the shell's reload).
pub fn offer_bar(ctx: Ctx, on_always_text: Rc<dyn Fn()>) -> impl IntoView {
    let scope = Scope::current();
    let rev: RwSignal<u32> = RwSignal::new(0);
    let dismissed: RwSignal<Vec<&'static str>> = RwSignal::new(Vec::new());
    // Only a change of the active tab's kind rebuilds the bar (a memo compares values); typing does not.
    let active = create_memo(move |_| ctx.tabs.with(|tm| (tm.current().kind, tm.active_idx())));
    dyn_container(
        move || (active.get(), rev.get(), dismissed.get()),
        move |((kind, _), _, dism)| {
            let bar = bar_for(kind, &dism);
            let on_always_text = on_always_text.clone();
            let (msg, buttons): (String, Vec<floem::AnyView>) = match bar {
                Bar::None => return empty().into_any(),
                Bar::Offer { module, kind } => {
                    let name = module_title(module);
                    let install = move || {
                        // The only code path that may reach the network: an explicit click.
                        let consent = Consent::explicit_click();
                        let job = match manager().and_then(|mut m| m.begin_install(consent, module).ok()) {
                            Some(j) => j,
                            None => return,
                        };
                        rev.update(|r| *r += 1);
                        let done = create_ext_action(scope, move |res: Result<Entry, ModuleError>| {
                            let msg = match &res {
                                Ok(_) => format!("{} installed.", module_title(module)),
                                Err(e) => e.to_string(),
                            };
                            if let Some(mut m) = manager() {
                                m.finish_install(module, res);
                            }
                            ctx.status.set(msg);
                            rev.update(|r| *r += 1);
                        });
                        std::thread::spawn(move || {
                            let policy = UrlPolicy::production("brouillon.rochetta.fr");
                            done(job.download_and_install(consent, DEFAULT_BASE_URL, &policy, false));
                        });
                    };
                    let not_now = move || {
                        if let Some(mut m) = manager() {
                            m.decline(module);
                        }
                        dismissed.update(|d| d.push(module));
                    };
                    let as_text = move || {
                        if let Some(mut m) = manager() {
                            let _ = m.set_always_text(kind.key(), true);
                            let keys = m.always_text();
                            kind::set_always_text(&keys.iter().map(|s| s.as_str()).collect::<Vec<_>>());
                        }
                        on_always_text();
                        rev.update(|r| *r += 1);
                    };
                    (
                        format!("{}. Its viewer is an optional {name} (about 9 MB, signed).", kind.label()),
                        vec![
                            bar_button(ctx, "Install", true, install).into_any(),
                            bar_button(ctx, "Not now", false, not_now).into_any(),
                            bar_button(ctx, "Always open as text", false, as_text).into_any(),
                        ],
                    )
                }
                Bar::Installing { module } => (format!("Installing the {}... the file stays open as raw bytes meanwhile.", module_title(module)), vec![]),
                Bar::Failed { module, kind, why } => {
                    let retry = move || {
                        if let Some(mut m) = manager() {
                            m.decline(module);
                        }
                        rev.update(|r| *r += 1);
                    };
                    let as_text = {
                        let on_always_text = on_always_text.clone();
                        move || {
                            if let Some(mut m) = manager() {
                                let _ = m.set_always_text(kind.key(), true);
                                let keys = m.always_text();
                                kind::set_always_text(&keys.iter().map(|s| s.as_str()).collect::<Vec<_>>());
                            }
                            on_always_text();
                            rev.update(|r| *r += 1);
                        }
                    };
                    (why, vec![bar_button(ctx, "Back to the offer", true, retry).into_any(), bar_button(ctx, "Open as text", false, as_text).into_any()])
                }
                Bar::Loaded { module, version } => (
                    format!("The {} {version} is installed and loaded. Its viewer is not part of this version yet: showing the raw bytes.", module_title(module)),
                    vec![],
                ),
            };
            stack((
                label(move || msg.clone()).style(move |s| {
                    let t = current_theme(ctx.theme.get());
                    s.flex_grow(1.0).flex_shrink(1.0).min_width(0.0).color(t.fg).padding_left(8.0)
                }),
                floem::views::stack_from_iter(buttons).style(|s| s.flex_row().items_center()),
            ))
            .style(move |s| {
                let t = current_theme(ctx.theme.get());
                s.flex_row().items_center().width_full().padding_vert(5.0).padding_horiz(6.0).background(t.bg_chrome).border_bottom(1.0).border_color(t.border)
            })
            .into_any()
        },
    )
    .style(|s| s.width_full())
}
