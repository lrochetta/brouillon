//! Brouillon Link surfaces: the "Agents" status chip with its always-visible stop button, the
//! consent modal for a new client, the proposal review panel (per hunk), and the Connect tab of
//! Preferences. Pure view-models plus floem views; the host supplies the actions, so this module
//! knows nothing about the MCP crate.

use std::path::Path;
use std::rc::Rc;

use floem::reactive::{create_rw_signal, RwSignal, SignalGet, SignalUpdate, SignalWith};
use floem::style::{CursorStyle, Display};
use floem::views::{container, dyn_container, empty, label, scroll, stack, stack_from_iter, Decorators};
use floem::{AnyView, IntoView};

use crate::theme_ctx::active_theme;
use crate::ui_kit::button_style;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HunkUi {
    pub start_line: usize,
    pub old_text: String,
    pub new_text: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProposalUi {
    pub id: String,
    pub client: String,
    pub summary: String,
    pub hunks: Vec<HunkUi>,
}

/// What the user answered in the consent modal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Consent {
    /// Read open files and propose edits (each one reviewed).
    Write,
    ReadOnly,
    Deny,
}

/// Someone needs the user's answer: a new client, a client whose program changed, or a client that
/// touches a folder opened after it was allowed.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ConsentReq {
    pub client: String,
    pub exe_path: Option<String>,
    pub exe_hash: Option<String>,
    pub folder: Option<String>,
    pub exe_changed: bool,
}

impl ConsentReq {
    pub fn new(client: &str) -> Self {
        Self { client: client.to_string(), ..Default::default() }
    }
}

/// A note an agent placed on a line (owned by that agent, removable by the user with one click).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NoteUi {
    pub id: u64,
    pub doc: String,
    pub owner: String,
    pub line: usize,
    pub kind: String,
    pub text: String,
    /// Monotonic deadline.
    pub expires: std::time::Instant,
}

/// What a text row needs to draw a note (the notes of the active document, already filtered).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LineNote {
    pub id: u64,
    pub owner: String,
    pub line: usize,
    pub kind: String,
    pub text: String,
}

/// Handed to the editor body: the notes to draw and how to remove one.
#[derive(Clone)]
pub struct NotesView {
    pub notes: RwSignal<Vec<LineNote>>,
    pub remove: Rc<dyn Fn(u64)>,
}

impl NotesView {
    /// No notes ever (tests, and builds without Link).
    pub fn none() -> Self {
        Self { notes: create_rw_signal(vec![]), remove: Rc::new(|_| {}) }
    }
}

/// Reactive state shared by the host (which fills it from the link threads, on the UI thread)
/// and the views.
#[derive(Clone, Copy)]
pub struct LinkUi {
    /// The server is running (chip visible).
    pub on: RwSignal<bool>,
    /// Questions waiting for the user (first one is shown).
    pub consent: RwSignal<Vec<ConsentReq>>,
    /// Pending proposals (first one is shown).
    pub proposals: RwSignal<Vec<ProposalUi>>,
    /// Result line of the Test button.
    pub test_result: RwSignal<String>,
    /// Every live agent note, all documents.
    pub notes: RwSignal<Vec<NoteUi>>,
    /// The notes of the active document, for the rows.
    pub line_notes: RwSignal<Vec<LineNote>>,
    /// `Some(url)` while the loopback HTTP listener runs (Connect tab).
    pub http_url: RwSignal<Option<String>>,
}

impl LinkUi {
    pub fn new() -> Self {
        Self {
            on: create_rw_signal(false),
            consent: create_rw_signal(vec![]),
            proposals: create_rw_signal(vec![]),
            test_result: create_rw_signal(String::new()),
            notes: create_rw_signal(vec![]),
            line_notes: create_rw_signal(vec![]),
            http_url: create_rw_signal(None),
        }
    }

    pub fn add_consent(&self, client: &str) {
        self.add_consent_req(ConsentReq::new(client));
    }

    /// One question per (client, folder): asking twice does not stack two modals.
    pub fn add_consent_req(&self, req: ConsentReq) {
        self.consent.update(|v| {
            if !v.iter().any(|c| c.client == req.client && c.folder == req.folder) {
                v.push(req);
            }
        });
    }

    /// The user answered (or dismissed) the question about `req`.
    pub fn remove_consent(&self, req: &ConsentReq) {
        self.consent.update(|v| v.retain(|c| !(c.client == req.client && c.folder == req.folder)));
    }

    pub fn add_proposal(&self, p: ProposalUi) {
        self.proposals.update(|v| {
            if !v.iter().any(|x| x.id == p.id) {
                v.push(p);
            }
        });
    }

    pub fn close_proposal(&self, id: &str) {
        self.proposals.update(|v| v.retain(|p| p.id != id));
    }

    /// Everything the user could still be asked about goes away (kill switch, server off).
    pub fn clear_pending(&self) {
        self.consent.set(vec![]);
        self.proposals.set(vec![]);
        self.notes.set(vec![]);
        self.line_notes.set(vec![]);
    }
}

/// Notes of `doc` that are still alive at `now` (pure, so it is testable without a window).
pub fn live_notes_for(notes: &[NoteUi], doc: Option<&str>, now: std::time::Instant) -> Vec<LineNote> {
    let Some(doc) = doc else { return vec![] };
    notes
        .iter()
        .filter(|n| n.doc == doc && n.expires > now)
        .map(|n| LineNote { id: n.id, owner: n.owner.clone(), line: n.line, kind: n.kind.clone(), text: n.text.clone() })
        .collect()
}

impl Default for LinkUi {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Clone)]
pub struct LinkActions {
    /// Kill switch: stop the server now AND switch `mcp.enabled` off, so it stays off at the next start.
    pub kill: Rc<dyn Fn()>,
    /// Stop now, for this session only: `mcp.enabled` is left as it is.
    pub kill_session: Rc<dyn Fn()>,
    pub consent: Rc<dyn Fn(ConsentReq, Consent)>,
    /// `None` = accept every hunk, `Some(list)` = only those (empty = reject).
    pub resolve: Rc<dyn Fn(String, Option<Vec<usize>>)>,
}

/// Host side of the Connect tab.
#[derive(Clone)]
pub struct ConnectHost {
    pub ui: LinkUi,
    pub snippet: Rc<dyn Fn() -> String>,
    pub copy: Rc<dyn Fn(String)>,
    pub test: Rc<dyn Fn()>,
    /// Set by "Try an agent": the next time Preferences opens it starts on this tab with the Claude Code line highlighted.
    pub start_connect: RwSignal<bool>,
}

// ---------------------------------------------------------------- pure helpers

pub fn chip_label(pending: usize) -> String {
    if pending == 0 {
        "Agents on".to_string()
    } else {
        format!("Agents on - {pending} to review")
    }
}

/// At most `max_lines` lines of `text`, each cut to `max_chars`; an ellipsis marks anything dropped.
pub fn preview(text: &str, max_lines: usize, max_chars: usize) -> String {
    let mut out: Vec<String> = Vec::new();
    let mut cut = false;
    for (i, l) in text.lines().enumerate() {
        if i >= max_lines {
            cut = true;
            break;
        }
        if l.chars().count() > max_chars {
            out.push(format!("{}...", l.chars().take(max_chars).collect::<String>()));
        } else {
            out.push(l.to_string());
        }
    }
    if out.is_empty() {
        return "(nothing)".to_string();
    }
    if cut {
        out.push("...".to_string());
    }
    out.join("\n")
}

pub fn picked_indices(picked: &[bool]) -> Vec<usize> {
    picked.iter().enumerate().filter(|(_, p)| **p).map(|(i, _)| i).collect()
}

fn quote(s: &str) -> String {
    if s.contains(' ') || s.contains('\t') {
        format!("\"{s}\"")
    } else {
        s.to_string()
    }
}

/// The line to paste in a terminal to register Brouillon in Claude Code. `link_dir` only when it is
/// not the default location the relay already looks at.
pub fn claude_snippet(relay: &Path, link_dir: Option<&Path>) -> String {
    let mut s = format!("claude mcp add brouillon -- {} --client claude-code", quote(&relay.display().to_string()));
    if let Some(d) = link_dir {
        s.push_str(&format!(" --link-dir {}", quote(&d.display().to_string())));
    }
    s
}

// ---------------------------------------------------------------- views

/// Header chip: only while the server runs. The stop button is a button, not a menu item: it is
/// always one click away.
pub fn link_chip(ui: LinkUi, actions: LinkActions) -> impl IntoView {
    dyn_container(
        move || ui.on.get(),
        move |on| {
            if !on {
                return empty().into_any();
            }
            let kill = actions.kill.clone();
            let chip = label(move || chip_label(ui.proposals.with(|p| p.len())))
                .style(|s| {
                    let t = active_theme();
                    s.padding_horiz(8.0).padding_vert(1.0).border_radius(8.0).background(t.accent).color(t.bg).font_size(11.0).font_bold()
                });
            let stop = label(|| "Stop agents (Ctrl+Alt+K)".to_string())
                .on_click_stop(move |_| kill())
                .style(|s| {
                    let t = active_theme();
                    s.padding_horiz(8.0).padding_vert(1.0).margin_left(6.0).font_size(11.0).border(1.0).border_radius(3.0).border_color(t.error).color(t.error).cursor(CursorStyle::Pointer).hover(move |h| h.background(t.error).color(t.bg))
                });
            let kill_session = actions.kill_session.clone();
            let session_only = label(|| "this session only".to_string())
                .on_click_stop(move |_| kill_session())
                .style(|s| {
                    let t = active_theme();
                    s.padding_horiz(4.0).padding_vert(1.0).margin_left(4.0).font_size(10.0).color(t.fg_dim).cursor(CursorStyle::Pointer).hover(move |h| h.color(t.error))
                });
            stack((chip, stop, session_only)).style(|s| s.flex_row().items_center().margin_right(8.0)).into_any()
        },
    )
}

fn card_style(s: floem::style::Style, width: f64) -> floem::style::Style {
    let t = active_theme();
    s.flex_col().gap(8.0).padding(12.0).width(width).background(t.bg_chrome).color(t.fg).border(1.0).border_color(t.accent).border_radius(4.0)
}

/// (title, explanation, program line) of the consent modal for `req`. Plain text, no emoji.
pub fn consent_texts(req: &ConsentReq) -> (String, String, Option<String>) {
    let name = if req.client.is_empty() { "An unnamed program".to_string() } else { format!("\"{}\"", req.client) };
    let (title, body) = if let Some(folder) = &req.folder {
        (
            format!("Allow {name} to also use the folder {folder}?"),
            "You opened this folder after allowing this program. Anything it reads there may be sent to that program's provider. Edits still never apply by themselves.".to_string(),
        )
    } else if req.exe_changed {
        (
            format!("{name} is a different program than the one you allowed"),
            "The program file changed since you allowed it (an update, or something else). Allow it again only if you expect that.".to_string(),
        )
    } else {
        (
            format!("Allow {name} to use Brouillon?"),
            "It asked to read your open documents. Anything it reads may be sent to that program's provider; Brouillon cannot see beyond this point. Edits never apply by themselves: each one appears for you to accept or reject, as a single undo step. You can stop it at any time with the Stop button.".to_string(),
        )
    };
    let program = req.exe_path.as_ref().map(|p| {
        let h = req.exe_hash.as_deref().map(|h| format!(" (fingerprint {})", h.chars().take(8).collect::<String>())).unwrap_or_default();
        format!("Program: {p}{h}")
    });
    (title, body, program)
}

/// The modal owns the keyboard while it is up: Enter = read only (the careful choice), Escape = deny.
/// Returns `true` when `ui` has a question waiting (the caller must then swallow the key, whatever it is).
pub fn consent_key(ui: LinkUi, actions: &LinkActions, enter: bool, escape: bool) -> bool {
    let Some(first) = ui.consent.with_untracked(|v| v.first().cloned()) else { return false };
    let answer = if enter {
        Some(Consent::ReadOnly)
    } else if escape {
        Some(Consent::Deny)
    } else {
        None
    };
    if let Some(a) = answer {
        (actions.consent)(first.clone(), a);
        ui.remove_consent(&first);
    }
    true
}

/// Consent modal for the first waiting question. It dims and blocks the whole window, and the
/// root key handler routes Enter / Escape to [`consent_key`].
pub fn consent_modal(ui: LinkUi, actions: LinkActions) -> impl IntoView {
    dyn_container(
        move || ui.consent.with(|v| v.first().cloned()),
        move |first| {
            let Some(req) = first else { return empty().into_any() };
            let (title, body, program) = consent_texts(&req);
            let btn = |text: &'static str, c: Consent, primary: bool, req: ConsentReq, actions: LinkActions| {
                label(move || text.to_string())
                    .on_click_stop(move |_| {
                        (actions.consent)(req.clone(), c);
                        ui.remove_consent(&req);
                    })
                    .style(move |s| button_style(s, primary))
            };
            let has_program = program.is_some();
            let program_line = label(move || program.clone().unwrap_or_default()).style(move |s| {
                let s = s.font_size(11.0).max_width(420.0).color(active_theme().fg_dim);
                if has_program { s } else { s.display(Display::None) }
            });
            let card = stack((
                label(move || title.clone()).style(|s| s.font_bold().font_size(14.0).color(active_theme().accent).max_width(420.0)),
                label(move || body.clone()).style(|s| s.font_size(12.0).max_width(420.0).color(active_theme().fg_dim)),
                program_line,
                stack((
                    btn("Allow (read + propose edits)", Consent::Write, false, req.clone(), actions.clone()),
                    btn("Read only  [Enter]", Consent::ReadOnly, true, req.clone(), actions.clone()),
                    btn("Deny  [Esc]", Consent::Deny, false, req, actions.clone()),
                ))
                .style(|s| s.flex_row().gap(8.0)),
            ))
            .style(|s| card_style(s, 460.0))
            .on_click_stop(|_| {});
            container(card)
                // Opaque to the pointer: nothing below the dimmed backdrop can be clicked.
                .on_click_stop(|_| {})
                .on_event_stop(floem::event::EventListener::PointerDown, |_| {})
                .style(|s| s.size_full().flex_col().items_center().padding_top(110.0).background(active_theme().bg.multiply_alpha(0.72)))
                .into_any()
        },
    )
    .style(move |s| {
        if ui.consent.with(|v| v.is_empty()) {
            s.display(Display::None)
        } else {
            s.absolute().inset_left(0.0).inset_top(0.0).size_full()
        }
    })
}


/// Review panel for the first pending proposal: per-hunk toggles, accept selected / all, reject.
pub fn proposal_panel(ui: LinkUi, actions: LinkActions) -> impl IntoView {
    dyn_container(
        move || ui.proposals.with(|v| v.first().cloned()),
        move |first| {
            let Some(p) = first else { return empty().into_any() };
            let n = p.hunks.len();
            let picked: RwSignal<Vec<bool>> = create_rw_signal(vec![true; n]);
            let id = p.id.clone();
            let hunk_views: Vec<AnyView> = p
                .hunks
                .iter()
                .enumerate()
                .map(|(i, h)| {
                    let start_line = h.start_line;
                    let head = label(move || format!("{} Hunk {} - line {}", if picked.with(|v| v[i]) { "[x]" } else { "[ ]" }, i + 1, start_line))
                        .on_click_stop(move |_| picked.update(|v| v[i] = !v[i]))
                        .style(|s| s.font_bold().font_size(12.0).cursor(CursorStyle::Pointer).color(active_theme().accent));
                    let old = label({
                        let t = preview(&h.old_text, 5, 60);
                        move || format!("- {}", t.replace('\n', "\n  "))
                    })
                    .style(|s| {
                        let t = active_theme();
                        s.font_size(12.0).color(t.error).background(t.error.multiply_alpha(0.12)).padding_horiz(4.0).width_full()
                    });
                    let new = label({
                        let t = preview(&h.new_text, 5, 60);
                        move || format!("+ {}", t.replace('\n', "\n  "))
                    })
                    .style(|s| {
                        let t = active_theme();
                        s.font_size(12.0).color(t.fps_ok).background(t.fps_ok.multiply_alpha(0.12)).padding_horiz(4.0).width_full()
                    });
                    stack((head, old, new)).style(|s| s.flex_col().gap(2.0).width_full()).into_any()
                })
                .collect();
            let (a1, a2, a3) = (actions.clone(), actions.clone(), actions.clone());
            let (i1, i2, i3) = (id.clone(), id.clone(), id.clone());
            let buttons = stack((
                label(move || format!("Accept all ({n})")).on_click_stop(move |_| (a1.resolve)(i1.clone(), None)).style(|s| button_style(s, true)),
                label(|| "Accept selected".to_string()).on_click_stop(move |_| (a2.resolve)(i2.clone(), Some(picked_indices(&picked.get_untracked())))).style(|s| button_style(s, false)),
                label(|| "Reject".to_string()).on_click_stop(move |_| (a3.resolve)(i3.clone(), Some(vec![]))).style(|s| button_style(s, false)),
            ))
            .style(|s| s.flex_row().gap(8.0));
            let title = format!("\"{}\" proposes a change", if p.client.is_empty() { "An agent" } else { &p.client });
            let summary = p.summary.clone();
            stack((
                label(move || title.clone()).style(|s| s.font_bold().font_size(13.0).color(active_theme().accent)),
                label(move || summary.clone()).style(|s| s.font_size(11.0).color(active_theme().fg_dim).max_width(480.0)),
                scroll(stack_from_iter(hunk_views).style(|s| s.flex_col().gap(8.0).width_full())).style(|s| s.max_height(260.0).width_full()),
                buttons,
            ))
            .style(|s| card_style(s, 520.0))
            .into_any()
        },
    )
    .style(move |s| {
        if ui.proposals.with(|v| v.is_empty()) {
            s.display(Display::None)
        } else {
            s.absolute().inset_right(14.0).inset_bottom(34.0)
        }
    })
}


/// Body of the Connect tab in Preferences. `enabled` is the persisted `mcp.enabled`; `toggle`
/// writes it (and the host starts or stops the server in reaction).
pub fn connect_tab(host: ConnectHost, enabled: RwSignal<bool>, toggle: Rc<dyn Fn(bool)>, highlight: bool) -> AnyView {
    let ui = host.ui;
    let copied = create_rw_signal(String::new());
    let on_toggle = {
        let toggle = toggle.clone();
        // Enabled in settings but stopped (kill switch): one click starts it again.
        move |_: &floem::event::Event| toggle(if enabled.get_untracked() && !ui.on.get_untracked() { true } else { !enabled.get_untracked() })
    };
    let switch = label(move || if enabled.get() { "[x] Agents (MCP server) on".to_string() } else { "[ ] Agents (MCP server) off".to_string() })
        .on_click_stop(on_toggle)
        .style(move |s| button_style(s, enabled.get()));
    let state = label(move || {
        if ui.on.get() {
            "Running: waiting for a client. Nothing leaves this computer: the server only answers a program on your machine."
        } else if enabled.get() {
            "Enabled in settings, not running (stopped with the kill switch, or it could not start)."
        } else {
            "Off. Nothing listens, no file is written. Default."
        }
        .to_string()
    })
    .style(|s| s.font_size(11.0).color(active_theme().fg_dim).max_width(640.0));
    let snippet = host.snippet.clone();
    let shown = label(move || snippet()).style(move |s| {
        let t = active_theme();
        let s = s.font_size(12.0).padding(8.0).background(t.bg).border(1.0).border_color(t.border).border_radius(3.0).max_width(700.0).width_full();
        if highlight { s.border(2.0).border_color(t.accent).background(t.bg_chrome) } else { s }
    });
    let (snip2, copy) = (host.snippet.clone(), host.copy.clone());
    let copy_btn = label(|| "Copy snippet".to_string())
        .on_click_stop(move |_| {
            copy(snip2());
            copied.set("Copied. Paste it in a terminal.".to_string());
        })
        .style(|s| button_style(s, false));
    let test = host.test.clone();
    let test_btn = label(|| "Test".to_string())
        .on_click_stop(move |_| {
            ui.test_result.set("Testing...".to_string());
            test();
        })
        .style(|s| button_style(s, false));
    stack((
        label(|| "Let an AI program (Claude Code, or any MCP client) read your open documents and propose edits.".to_string()).style(|s| s.font_size(12.0)),
        switch,
        state,
        label(|| "1. Turn it on. 2. Run this in a terminal (it registers Brouillon in Claude Code):".to_string()).style(|s| s.font_size(12.0).margin_top(10.0)),
        label(move || if highlight { "Claude Code: copy this line".to_string() } else { String::new() }).style(move |s| if highlight { s.font_size(12.0).font_bold().color(active_theme().accent) } else { s.hide() }),
        shown,
        stack((
            copy_btn,
            test_btn,
            label(move || copied.get()).style(|s| s.font_size(11.0).color(active_theme().fg_dim)),
        ))
        .style(|s| s.flex_row().items_center().gap(8.0)),
        label(move || ui.test_result.get()).style(|s| s.font_size(12.0).color(active_theme().accent)),
        label(move || match ui.http_url.get() {
            Some(u) => format!("HTTP transport (advanced) is on: {u} - 127.0.0.1 only, bearer token in the instance file of the link folder (never in the URL)."),
            None => String::new(),
        })
        .style(move |s| {
            let s = s.font_size(11.0).color(active_theme().fg_dim).max_width(700.0);
            if ui.http_url.with(|u| u.is_some()) { s } else { s.display(Display::None) }
        }),
        label(|| "3. Ask Claude Code to use Brouillon. The first time, Brouillon asks you to allow that client. Every edit it proposes is shown for you to accept or reject, and is one undo step.".to_string())
            .style(|s| s.font_size(12.0).margin_top(10.0).max_width(700.0)),
    ))
    .style(|s| s.flex_col().gap(8.0).width_full())
    .into_any()
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

    #[test]
    fn chip_text_counts_pending_proposals() {
        assert_eq!(chip_label(0), "Agents on");
        assert_eq!(chip_label(2), "Agents on - 2 to review");
    }

    #[test]
    fn preview_cuts_lines_and_width_and_never_panics_on_multibyte() {
        assert_eq!(preview("", 5, 10), "(nothing)");
        assert_eq!(preview("a\nb", 5, 10), "a\nb");
        assert_eq!(preview("a\nb\nc\nd", 2, 10), "a\nb\n...");
        assert_eq!(preview("0123456789abc", 5, 10), "0123456789...");
        assert_eq!(preview("\u{e9}\u{e9}\u{e9}\u{e9}", 5, 2), "\u{e9}\u{e9}...");
    }

    #[test]
    fn picked_hunks_become_indices() {
        assert_eq!(picked_indices(&[true, false, true]), vec![0, 2]);
        assert!(picked_indices(&[false, false]).is_empty());
        assert!(picked_indices(&[]).is_empty());
    }

    #[test]
    fn snippet_quotes_paths_with_spaces_and_adds_link_dir_only_when_given() {
        let s = claude_snippet(Path::new("C:\\Program Files\\Brouillon\\brouillon-mcp.exe"), None);
        assert_eq!(s, "claude mcp add brouillon -- \"C:\\Program Files\\Brouillon\\brouillon-mcp.exe\" --client claude-code");
        let s = claude_snippet(Path::new("/opt/b/brouillon-mcp"), Some(Path::new("/tmp/my link")));
        assert_eq!(s, "claude mcp add brouillon -- /opt/b/brouillon-mcp --client claude-code --link-dir \"/tmp/my link\"");
    }

    #[test]
    fn link_ui_queues_deduplicate_and_clear() {
        run(|| {
            let ui = LinkUi::new();
            ui.add_consent("claude-code");
            ui.add_consent("claude-code");
            ui.add_consent("other");
            assert_eq!(ui.consent.get_untracked(), vec![ConsentReq::new("claude-code"), ConsentReq::new("other")]);
            ui.add_consent_req(ConsentReq { folder: Some("/w".into()), ..ConsentReq::new("other") });
            assert_eq!(ui.consent.get_untracked().len(), 3, "a folder question is separate from the client question");
            ui.remove_consent(&ConsentReq { folder: Some("/w".into()), ..ConsentReq::new("other") });
            assert_eq!(ui.consent.get_untracked().len(), 2);
            let p = ProposalUi { id: "p1".into(), client: "c".into(), summary: "s".into(), hunks: vec![HunkUi { start_line: 1, old_text: "a".into(), new_text: "b".into() }] };
            ui.add_proposal(p.clone());
            ui.add_proposal(p);
            assert_eq!(ui.proposals.get_untracked().len(), 1);
            ui.close_proposal("p1");
            ui.close_proposal("zz");
            assert!(ui.proposals.get_untracked().is_empty());
            ui.add_consent("x");
            ui.clear_pending();
            assert!(ui.consent.get_untracked().is_empty());
        });
    }

    #[test]
    fn consent_texts_say_why_and_name_the_program() {
        let (t, b, p) = consent_texts(&ConsentReq::new("claude-code"));
        assert!(t.contains("\"claude-code\"") && b.contains("provider") && p.is_none());
        let changed = ConsentReq { exe_changed: true, exe_path: Some("C:\\x\\claude.exe".into()), exe_hash: Some("0123456789abcdef".into()), ..ConsentReq::new("claude-code") };
        let (t, _, p) = consent_texts(&changed);
        assert!(t.contains("different program"));
        assert_eq!(p.as_deref(), Some("Program: C:\\x\\claude.exe (fingerprint 01234567)"));
        let folder = ConsentReq { folder: Some("C:\\later".into()), ..ConsentReq::new("c") };
        assert!(consent_texts(&folder).0.contains("also use the folder C:\\later"));
        assert!(consent_texts(&ConsentReq::new("")).0.contains("unnamed"));
    }

    #[test]
    fn the_modal_grabs_the_keyboard_enter_is_read_only_escape_denies() {
        run(|| {
            let ui = LinkUi::new();
            let got: Rc<std::cell::RefCell<Vec<(String, Consent)>>> = Default::default();
            let g2 = got.clone();
            let actions = LinkActions { kill: Rc::new(|| {}), kill_session: Rc::new(|| {}), consent: Rc::new(move |r, c| g2.borrow_mut().push((r.client, c))), resolve: Rc::new(|_, _| {}) };
            assert!(!consent_key(ui, &actions, true, false), "no question, the key is not ours");
            ui.add_consent("a");
            ui.add_consent("b");
            assert!(consent_key(ui, &actions, false, false), "any other key is swallowed while the modal is up");
            assert!(got.borrow().is_empty() && ui.consent.get_untracked().len() == 2);
            assert!(consent_key(ui, &actions, true, false));
            assert!(consent_key(ui, &actions, false, true));
            assert_eq!(*got.borrow(), vec![("a".to_string(), Consent::ReadOnly), ("b".to_string(), Consent::Deny)]);
            assert!(ui.consent.get_untracked().is_empty());
            assert!(!consent_key(ui, &actions, true, false));
        });
    }

    #[test]
    fn notes_are_filtered_by_document_and_expiry() {
        let now = std::time::Instant::now();
        let n = |id, doc: &str, secs: u64| NoteUi { id, doc: doc.into(), owner: "c".into(), line: 3, kind: "note".into(), text: "t".into(), expires: now + std::time::Duration::from_secs(secs) };
        let mut dead = n(3, "d1", 5);
        dead.expires = now - std::time::Duration::from_secs(1);
        let all = vec![n(1, "d1", 5), n(2, "d2", 5), dead];
        let v = live_notes_for(&all, Some("d1"), now);
        assert_eq!(v.iter().map(|x| x.id).collect::<Vec<_>>(), [1]);
        assert!(live_notes_for(&all, None, now).is_empty());
    }

    #[test]
    fn views_build_with_and_without_pending_items() {
        run(|| {
            let ui = LinkUi::new();
            let actions = LinkActions { kill: Rc::new(|| {}), kill_session: Rc::new(|| {}), consent: Rc::new(|_, _| {}), resolve: Rc::new(|_, _| {}) };
            let _ = link_chip(ui, actions.clone());
            let _ = (consent_modal(ui, actions.clone()), proposal_panel(ui, actions.clone()));
            ui.on.set(true);
            ui.add_consent("claude-code");
            ui.add_proposal(ProposalUi { id: "p1".into(), client: "claude-code".into(), summary: "rename".into(), hunks: vec![HunkUi { start_line: 3, old_text: "a\nb".into(), new_text: "c".into() }] });
            let _ = link_chip(ui, actions.clone());
            let _ = (consent_modal(ui, actions.clone()), proposal_panel(ui, actions));
            let host = ConnectHost { ui, snippet: Rc::new(|| "claude mcp add x".into()), copy: Rc::new(|_| {}), test: Rc::new(|| {}), start_connect: create_rw_signal(false) };
            let _ = connect_tab(host, create_rw_signal(true), Rc::new(|_| {}), true);
        });
    }
}
