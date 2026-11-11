//! Dialogs of remote editing that are not the connect form: the ask-once question (password,
//! passphrase, host-key confirmation), the "identity changed" stop, the save-conflict choice and the
//! "recover unsaved changes" list. Wording follows internal design notes (not published)
//! (sections 5 to 7 and the copy deck): plain sentences, the safe button has focus, trusting a new
//! key is never the default, a changed key needs the typed site name.
//!
//! The password question itself has no "save" choice: "use it this time only" is the default (decision Q2).
//! Keeping it is asked AFTER a successful connect, once per site (`keep_body`), and the read-only
//! "Credentials" list (`credentials_body`) shows metadata only. The typed secret lives in a plain
//! `Rc<RefCell<String>>` (never a signal, never persisted), is moved to the helper once and dropped; the
//! engine's `Credentials` holds a zeroizing copy only until the connect result is known.
//!
//! Never seen on screen by the author of this file: these views compile and their logic is unit
//! tested (`name_matches`, dialog state), but nobody has looked at them in the running editor.

use std::cell::RefCell;
use std::rc::Rc;

use brouillon_editor_engine::remote::{save_as_suggestion, AskKind, AskView, ConflictChoice, ConflictKind, KeepOffer, RecoverItem, RemoteOrigin, SavedEntry};
use floem::event::{Event, EventListener};
use floem::keyboard::{Key, NamedKey};
use floem::reactive::{create_rw_signal, RwSignal, SignalGet, SignalUpdate, SignalWith};
use floem::style::CursorStyle;
use floem::views::{container, dyn_container, dyn_stack, empty, label, scroll, stack, stack_from_iter, text_input, Decorators};
use floem::{AnyView, IntoView, View};

use crate::remote_view::RemoteUi;
use crate::theme_ctx::active_theme;
use crate::ui_kit::{button_style, field_style, modal, ring_button, FocusRing};

#[derive(Clone)]
pub struct ConflictState {
    pub origin: RemoteOrigin,
    pub kind: ConflictKind,
    /// The buffer that was being saved (kept for Overwrite / Save as).
    pub local: Rc<Vec<u8>>,
}

/// The "Keep this password?" card: who it is about and what the offer is. Never carries the password.
#[derive(Clone, Debug, PartialEq)]
pub struct KeepAsk {
    pub site_id: String,
    pub site_name: String,
    /// `user@host`
    pub who: String,
    pub offer: KeepOffer,
    /// Plain FTP: the card adds the "not encrypted" warning.
    pub unencrypted: bool,
}

/// One line of the read-only "Credentials" view: metadata only, no value by construction.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CredRow {
    pub id_hex: String,
    pub label: String,
    pub kind: &'static str,
    pub site: String,
    pub created: String,
    pub last_used: String,
}

impl CredRow {
    pub fn of(e: &SavedEntry, site_name: impl Fn(&str) -> String) -> Self {
        CredRow {
            id_hex: format!("{:032x}", e.id.as_u128()),
            label: e.label.clone(),
            kind: e.kind,
            site: e.site_id.as_deref().map(site_name).unwrap_or_default(),
            created: date_text(e.created),
            last_used: if e.last_used == 0 { "never".into() } else { date_text(e.last_used) },
        }
    }
}

/// `YYYY-MM-DD` (UTC) of a Unix time. Day granularity keeps the view calm and the metadata coarse.
pub fn date_text(secs: u64) -> String {
    let z = (secs / 86_400) as i64 + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!("{y:04}-{m:02}-{d:02}")
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChangedKey {
    pub site_id: String,
    pub site_name: String,
    pub host: String,
    pub port: u16,
}

/// Replacing a pinned identity needs the exact site name, typed (case and surrounding spaces ignored).
pub fn name_matches(typed: &str, site_name: &str) -> bool {
    !site_name.trim().is_empty() && typed.trim().eq_ignore_ascii_case(site_name.trim())
}

#[derive(Clone)]
pub struct DialogState {
    pub ask: RwSignal<Option<AskView>>,
    pub ask_host_open: RwSignal<bool>,
    pub ask_secret_open: RwSignal<bool>,
    /// What the user types in the masked field. NOT a signal.
    typed: Rc<RefCell<String>>,
    pub typed_len: RwSignal<usize>,
    pub conflict: RwSignal<Option<ConflictState>>,
    pub conflict_open: RwSignal<bool>,
    pub save_as: RwSignal<String>,
    pub changed_key: RwSignal<Option<ChangedKey>>,
    pub changed_open: RwSignal<bool>,
    pub changed_typed: RwSignal<String>,
    pub recover_open: RwSignal<bool>,
    pub recover_items: RwSignal<Vec<RecoverItem>>,
    pub keep: RwSignal<Option<KeepAsk>>,
    pub keep_open: RwSignal<bool>,
    /// The radio of the keep card: false = "use it this time only" (the default).
    pub keep_save: RwSignal<bool>,
    pub creds_open: RwSignal<bool>,
    pub creds_items: RwSignal<Vec<CredRow>>,
    pub creds_note: RwSignal<String>,
    /// Id of the row whose delete is waiting for confirmation.
    pub creds_confirm: RwSignal<Option<String>>,
}

impl DialogState {
    /// `any_open` mirrors "one of these dialogs is on screen", so the host's key handler leaves the document alone.
    pub fn new(any_open: RwSignal<bool>) -> Self {
        let d = Self {
            ask: create_rw_signal(None),
            ask_host_open: create_rw_signal(false),
            ask_secret_open: create_rw_signal(false),
            typed: Rc::new(RefCell::new(String::new())),
            typed_len: create_rw_signal(0),
            conflict: create_rw_signal(None),
            conflict_open: create_rw_signal(false),
            save_as: create_rw_signal(String::new()),
            changed_key: create_rw_signal(None),
            changed_open: create_rw_signal(false),
            changed_typed: create_rw_signal(String::new()),
            recover_open: create_rw_signal(false),
            recover_items: create_rw_signal(Vec::new()),
            keep: create_rw_signal(None),
            keep_open: create_rw_signal(false),
            keep_save: create_rw_signal(false),
            creds_open: create_rw_signal(false),
            creds_items: create_rw_signal(Vec::new()),
            creds_note: create_rw_signal(String::new()),
            creds_confirm: create_rw_signal(None),
        };
        let dd = d.clone();
        floem::reactive::create_effect(move |_| {
            any_open.set(dd.ask_host_open.get() || dd.ask_secret_open.get() || dd.conflict_open.get() || dd.changed_open.get() || dd.recover_open.get() || dd.keep_open.get() || dd.creds_open.get());
        });
        d
    }

    pub fn show_ask(&self, v: Option<AskView>) {
        // A different question replaces the one on screen: close it first so the new body is built from the new text.
        if v.is_some() && self.ask.get_untracked().is_some() {
            self.hide_ask();
        }
        self.typed.borrow_mut().clear();
        self.typed_len.set(0);
        match &v {
            Some(v) if v.kind == AskKind::HostKey => {
                self.ask_secret_open.set(false);
                self.ask.set(Some(v.clone()));
                self.ask_host_open.set(true);
            }
            Some(v) => {
                self.ask_host_open.set(false);
                self.ask.set(Some(v.clone()));
                self.ask_secret_open.set(true);
            }
            None => self.hide_ask(),
        }
    }

    pub fn hide_ask(&self) {
        // Clear the flags first: the effect that treats a closed window as "cancel" only fires while `ask` is still set.
        self.ask.set(None);
        self.ask_host_open.set(false);
        self.ask_secret_open.set(false);
        self.typed.borrow_mut().clear();
        self.typed_len.set(0);
    }

    /// The typed text, moved out (the field is empty afterwards).
    pub fn take_typed(&self) -> String {
        self.typed_len.set(0);
        std::mem::take(&mut *self.typed.borrow_mut())
    }

    pub fn show_keep(&self, k: KeepAsk) {
        self.keep_save.set(false);
        self.keep.set(Some(k));
        self.keep_open.set(true);
    }

    pub fn hide_keep(&self) {
        self.keep.set(None);
        self.keep_open.set(false);
    }

    pub fn show_conflict(&self, c: ConflictState) {
        self.save_as.set(save_as_suggestion(&c.origin.path));
        self.conflict.set(Some(c));
        self.conflict_open.set(true);
    }

    pub fn show_changed_key(&self, k: ChangedKey) {
        self.changed_typed.set(String::new());
        self.changed_key.set(Some(k));
        self.changed_open.set(true);
    }
}

// ---------------------------------------------------------------- small widgets

fn button(text: &'static str, selected: bool, on: impl Fn() + 'static) -> impl IntoView {
    label(move || text.to_string()).on_click_stop(move |_| on()).style(move |s| button_style(s, selected))
}

/// A button of a dialog: part of its Tab ring, Enter and Space press it, Escape runs `esc`.
fn ring_btn(ring: &FocusRing, text: &'static str, selected: bool, esc: &Rc<dyn Fn()>, on: impl Fn() + 'static) -> impl IntoView {
    ring_button(ring, text, selected, on, esc.clone())
}

/// The safe button: it also takes the keyboard focus when the dialog opens.
fn focused_btn(ring: &FocusRing, text: &'static str, esc: &Rc<dyn Fn()>, on: impl Fn() + 'static) -> impl IntoView {
    ring_button(ring, text, true, on, esc.clone()).request_focus(|| {})
}

fn dim(text: String) -> impl IntoView {
    label(move || text.clone()).style(|s| s.font_size(11.0).color(active_theme().fg_dim).width_full())
}

fn body_text(text: String) -> impl IntoView {
    label(move || text.clone()).style(|s| s.font_size(12.0).width_full().flex_wrap(floem::taffy::FlexWrap::Wrap))
}

/// Masked entry: shows one `*` per character, takes the keyboard, never echoes. Enter submits, Escape cancels,
/// Ctrl+V pastes (first line only).
fn masked_field(d: DialogState, ring: &FocusRing, on_enter: impl Fn() + 'static, on_cancel: impl Fn() + 'static) -> impl IntoView {
    let (buf, len) = (d.typed.clone(), d.typed_len);
    let ring = ring.clone();
    let field = container(label(move || if len.get() == 0 { "type here".to_string() } else { "*".repeat(len.get().min(64)) }).style(move |s| {
        let t = active_theme();
        s.color(if len.get() == 0 { t.fg_dim } else { t.fg })
    }))
    .keyboard_navigable()
    .request_focus(|| {});
    let idx = ring.register(field.id());
    field
    .style(|s| field_style(s).width_full().min_height(24.0).cursor(CursorStyle::Text))
    .on_event_stop(EventListener::KeyDown, move |e| {
        let Event::KeyDown(ke) = e else { return };
        let ctrl = ke.modifiers.control() || ke.modifiers.alt() || ke.modifiers.meta();
        match &ke.key.logical_key {
            Key::Named(NamedKey::Tab) if !ctrl => ring.go(idx, ke.modifiers.shift()),
            Key::Named(NamedKey::Enter) => on_enter(),
            Key::Named(NamedKey::Escape) => on_cancel(),
            Key::Named(NamedKey::Backspace) => {
                buf.borrow_mut().pop();
                len.set(buf.borrow().chars().count());
            }
            Key::Named(NamedKey::Space) => {
                buf.borrow_mut().push(' ');
                len.set(buf.borrow().chars().count());
            }
            Key::Character(c) if ctrl && c.eq_ignore_ascii_case("v") => {
                if let Ok(t) = arboard::Clipboard::new().and_then(|mut c| c.get_text()) {
                    buf.borrow_mut().push_str(t.lines().next().unwrap_or(""));
                    len.set(buf.borrow().chars().count());
                }
            }
            Key::Character(c) if !ctrl => {
                buf.borrow_mut().push_str(c);
                len.set(buf.borrow().chars().count());
            }
            _ => {}
        }
    })
}

// ---------------------------------------------------------------- the dialogs

fn host_key_body(ui: RemoteUi) -> AnyView {
    let Some(v) = ui.dlg.ask.get_untracked() else { return empty().into_any() };
    let (cancel, trust) = (ui.clone(), ui.clone());
    let copy_text = v.text.clone();
    let copy = ui.clone();
    let ring = FocusRing::new();
    let esc: Rc<dyn Fn()> = {
        let u = ui.clone();
        Rc::new(move || u.cancel_dialogs())
    };
    stack((
        body_text(format!("Brouillon has not connected to {} before, or wants to confirm it. If this does not match what your hosting provider shows, cancel.", v.site_name)),
        scroll(label(move || v.text.clone()).style(|s| s.font_size(11.0).width_full().flex_wrap(floem::taffy::FlexWrap::Wrap))).style(|s| s.width_full().height(110.0).padding(6.0).border(1.0).border_color(active_theme().border)),
        dim("To find the real fingerprint, run on the server: ssh-keygen -lf /etc/ssh/ssh_host_ed25519_key.pub".into()),
        stack((
            focused_btn(&ring, " Cancel ", &esc, move || cancel.answer_ask(None)),
            ring_btn(&ring, " Trust and connect ", false, &esc, move || trust.answer_ask(Some("yes".into()))),
            ring_btn(&ring, " Copy ", false, &esc, move || {
                let _ = arboard::Clipboard::new().and_then(|mut c| c.set_text(copy_text.clone()));
                copy.dlg.typed_len.set(0);
            }),
        ))
        .style(|s| s.flex_row().gap(8.0).justify_end().width_full()),
    ))
    .style(|s| s.flex_col().gap(8.0).width_full())
    .into_any()
}

fn secret_body(ui: RemoteUi) -> AnyView {
    let Some(v) = ui.dlg.ask.get_untracked() else { return empty().into_any() };
    let (ok, cancel, field_ok, field_cancel) = (ui.clone(), ui.clone(), ui.clone(), ui.clone());
    let what = match v.kind {
        AskKind::Passphrase => "key passphrase",
        AskKind::Password => "password",
        _ => "answer",
    };
    let submit = move |u: &RemoteUi| {
        let typed = u.dlg.take_typed();
        u.answer_ask(Some(typed));
    };
    let submit_btn = submit.clone();
    let ring = FocusRing::new();
    let esc: Rc<dyn Fn()> = {
        let u = ui.clone();
        Rc::new(move || u.cancel_dialogs())
    };
    stack((
        body_text(format!("{} asks for the {what}.", v.site_name)),
        dim(v.text.clone()),
        masked_field(ui.dlg.clone(), &ring, move || submit(&field_ok), move || field_cancel.answer_ask(None)),
        dim("Brouillon asks once, after the connection works, whether to keep it.".into()),
        stack((ring_btn(&ring, " Cancel ", false, &esc, move || cancel.answer_ask(None)), ring_btn(&ring, " Continue ", true, &esc, move || submit_btn(&ok)))).style(|s| s.flex_row().gap(8.0).justify_end().width_full()),
    ))
    .style(|s| s.flex_col().gap(8.0).width_full())
    .into_any()
}

fn keep_body(ui: RemoteUi) -> AnyView {
    let Some(k) = ui.dlg.keep.get_untracked() else { return empty().into_any() };
    let save = ui.dlg.keep_save;
    let what = if k.offer.kind == AskKind::Passphrase { "key passphrase" } else { "password" };
    let body = if k.offer.replace {
        format!("The saved {what} for {} ({}) was not accepted, but the one you just typed was. Replace the saved one?", k.site_name, k.who)
    } else {
        format!("{} ({}) accepted it. Do you want Brouillon to remember it?", k.site_name, k.who)
    };
    let save_text: &'static str = if k.offer.replace { "Replace the saved one" } else { "Save on this computer" };
    let ring = FocusRing::new();
    let esc: Rc<dyn Fn()> = {
        let u = ui.clone();
        // Escape (and the close button) = use it this time only.
        Rc::new(move || u.resolve_keep(false))
    };
    let radio = |ring: &FocusRing, text: &'static str, value: bool, esc: &Rc<dyn Fn()>| {
        let set: Rc<dyn Fn()> = Rc::new(move || save.set(value));
        let click = set.clone();
        ring.control(label(move || format!("{} {text}", if save.get() == value { "(o)" } else { "( )" })).on_click_stop(move |_| click()), None, Some(set), esc.clone())
            .style(move |s| s.padding_vert(2.0).cursor(CursorStyle::Pointer).font_size(12.0).color(if save.get() == value { active_theme().accent } else { active_theme().fg }))
    };
    let this_time = radio(&ring, "Use it this time only", false, &esc);
    let on_computer = radio(&ring, save_text, true, &esc);
    let (go, no) = (ui.clone(), ui.clone());
    let mut rows: Vec<AnyView> = vec![
        body_text(body).into_any(),
        stack((this_time, on_computer)).style(|s| s.flex_col().gap(2.0).width_full()).into_any(),
        dim("Saved passwords are scrambled and tied to your Windows account. You can delete them any time: command palette, Credentials: Show.".into()).into_any(),
    ];
    if k.unencrypted {
        rows.push(dim("This connection is not encrypted. Anyone on the network between you and the server can read the password. Use SFTP or FTPS if your server offers it.".into()).into_any());
    }
    rows.push(
        stack((
            focused_btn(&ring, " Continue ", &esc, move || {
                let v = go.dlg.keep_save.get_untracked();
                go.resolve_keep(v)
            }),
            ring_btn(&ring, " Cancel ", false, &esc, move || no.resolve_keep(false)),
        ))
        .style(|s| s.flex_row().gap(8.0).justify_end().width_full())
        .into_any(),
    );
    stack_from_iter(rows).style(|s| s.flex_col().gap(8.0).width_full()).into_any()
}

fn credentials_body(ui: RemoteUi) -> AnyView {
    let u = ui.clone();
    let list = dyn_stack(
        move || u.dlg.creds_items.get(),
        |r: &CredRow| r.id_hex.clone(),
        {
            let ui = ui.clone();
            move |r: CredRow| {
                let (rep, del) = (ui.clone(), ui.clone());
                let (i1, i2) = (r.id_hex.clone(), r.id_hex.clone());
                let caption = format!("{}  {}  {}  created {}  last used {}", r.label, r.kind, r.site, r.created, r.last_used);
                stack((
                    label(move || caption.clone()).style(|s| s.flex_grow(1.0).min_width(0.0).text_ellipsis().font_size(11.0)),
                    button(" Replace ", false, move || rep.replace_credential(&i1)),
                    button(" Delete ", false, move || del.dlg.creds_confirm.set(Some(i2.clone()))),
                ))
                .style(|s| s.flex_row().items_center().gap(4.0).width_full().padding_vert(2.0))
            }
        },
    )
    .style(|s| s.flex_col().width_full());
    let u = ui.clone();
    let empty_note = label(move || if u.dlg.creds_items.with(|v| v.is_empty()) { "Nothing saved yet. Passwords you choose to save will show up here.".to_string() } else { String::new() }).style(|s| s.font_size(11.0).color(active_theme().fg_dim));
    let d = ui.dlg.clone();
    let note = label(move || d.creds_note.get()).style(|s| s.font_size(11.0).color(active_theme().fg_dim));
    let u = ui.clone();
    let confirm = dyn_container(
        move || u.dlg.creds_confirm.get(),
        {
            let ui = ui.clone();
            move |c: Option<String>| {
                let Some(id) = c else { return empty().into_any() };
                let name = ui.dlg.creds_items.with_untracked(|v| v.iter().find(|r| r.id_hex == id).map(|r| r.label.clone())).unwrap_or_default();
                let (yes, no) = (ui.clone(), ui.clone());
                stack((
                    body_text(format!("Delete the saved password for {name}? Brouillon will forget it. The server and your files are not touched. You will be asked for the password next time.")),
                    stack((button(" Cancel ", true, move || no.dlg.creds_confirm.set(None)), button(" Delete ", false, move || yes.delete_credential(&id))))
                        .style(|s| s.flex_row().gap(8.0).justify_end().width_full()),
                ))
                .style(|s| s.flex_col().gap(6.0).width_full())
                .into_any()
            }
        },
    );
    stack((
        dim("Standard protection. Only your Windows account on this computer can open what is saved here. Brouillon never shows a saved value again: you can replace or delete it.".into()),
        scroll(list).style(|s| s.width_full().height(180.0)),
        empty_note,
        note,
        confirm,
    ))
    .style(|s| s.flex_col().gap(6.0).width_full())
    .into_any()
}

fn conflict_body(ui: RemoteUi) -> AnyView {
    let Some(c) = ui.dlg.conflict.get_untracked() else { return empty().into_any() };
    let (headline, detail) = match c.kind {
        ConflictKind::Changed => ("The file changed on the server.", format!("{} on {} was changed after you opened it. Brouillon wrote nothing: their version is intact and your text is still in this tab.", c.origin.path, c.origin.site_label)),
        ConflictKind::Newer => ("The server has a newer version.", format!("{} on {} is newer than the copy you opened. You made no changes, so loading it loses nothing.", c.origin.path, c.origin.site_label)),
        ConflictKind::Gone => ("The file no longer exists on the server.", format!("{} on {} was deleted or moved. Nothing was written.", c.origin.path, c.origin.site_label)),
    };
    let ring = FocusRing::new();
    let esc: Rc<dyn Fn()> = {
        let u = ui.clone();
        Rc::new(move || u.cancel_dialogs())
    };
    // Tab order = visual order: the Save-as field and button first, then Cancel (which holds the focus
    // when the dialog opens), Reload, Overwrite.
    let save_as_row: AnyView = if c.kind == ConflictKind::Newer {
        empty().into_any()
    } else {
        let u = ui.clone();
        let u_enter = ui.clone();
        let sa = ui.dlg.save_as;
        let try_save = move |u: &RemoteUi| {
            let p = sa.get_untracked();
            if p.starts_with('/') && !p.ends_with('/') {
                u.resolve_conflict(ConflictChoice::SaveAs { path: p });
            }
        };
        let try_save2 = try_save.clone();
        let input = ring.control(text_input(sa), Some(Rc::new(move || try_save2(&u_enter))), None, esc.clone()).style(|s| field_style(s).flex_grow(1.0).min_width(0.0));
        stack((input, ring_btn(&ring, " Save mine as... ", false, &esc, move || try_save(&u))))
            .style(|s| s.flex_row().gap(6.0).width_full())
            .into_any()
    };
    let u = ui.clone();
    let cancel = focused_btn(&ring, " Cancel ", &esc, move || u.resolve_conflict(ConflictChoice::Cancel));
    let mut row: Vec<AnyView> = vec![cancel.into_any()];
    if c.kind != ConflictKind::Gone {
        let u = ui.clone();
        row.push(ring_btn(&ring, " Reload from server ", c.kind == ConflictKind::Newer, &esc, move || u.resolve_conflict(ConflictChoice::Reload)).into_any());
    }
    if c.kind != ConflictKind::Newer {
        let u = ui.clone();
        let title: &'static str = if c.kind == ConflictKind::Gone { " Create it again " } else { " Overwrite (theirs is backed up first) " };
        row.push(ring_btn(&ring, title, false, &esc, move || u.resolve_conflict(ConflictChoice::Overwrite)).into_any());
    }
    stack((
        label(move || headline.to_string()).style(|s| s.font_bold().font_size(13.0)),
        body_text(detail),
        save_as_row,
        dim("Reload keeps your edits for 7 days in the local journal. Comparing the two versions side by side is not available here yet.".into()),
        stack_from_iter(row).style(|s| s.flex_row().gap(8.0).justify_end().flex_wrap(floem::taffy::FlexWrap::Wrap).width_full()),
    ))
    .style(|s| s.flex_col().gap(8.0).width_full())
    .into_any()
}

fn changed_key_body(ui: RemoteUi) -> AnyView {
    let Some(k) = ui.dlg.changed_key.get_untracked() else { return empty().into_any() };
    let typed = ui.dlg.changed_typed;
    let (cancel, replace) = (ui.clone(), ui.clone());
    let name = k.site_name.clone();
    let ok_name = k.site_name.clone();
    let ring = FocusRing::new();
    let esc: Rc<dyn Fn()> = {
        let u = ui.clone();
        Rc::new(move || u.cancel_dialogs())
    };
    let do_replace: Rc<dyn Fn()> = Rc::new(move || {
        if name_matches(&typed.get_untracked(), &ok_name) {
            replace.replace_changed_key();
        }
    });
    stack((
        label(|| "Stop: the server's identity changed.".to_string()).style(|s| s.font_bold().font_size(13.0).color(active_theme().error)),
        body_text(format!("{} used to have a different fingerprint. That is normal if the server was reinstalled. It can also mean someone is sitting between you and the server. Brouillon did not send your password.", k.host)),
        dim(format!("To replace the old identity, type the site name ({name}) and press Replace and connect. Then compare the new fingerprint with your provider's.")),
        ring.control(text_input(typed), Some(do_replace.clone()), None, esc.clone()).style(|s| field_style(s).width_full()),
        stack((
            focused_btn(&ring, " Cancel ", &esc, move || cancel.dlg.changed_open.set(false)),
            ring.control(label(|| " Replace and connect ".to_string()).on_click_stop({
                    let r = do_replace.clone();
                    move |_| r()
                }), None, Some(do_replace.clone()), esc.clone())
                .style(move |s| {
                    let enabled = name_matches(&typed.get(), &k.site_name);
                    let s = button_style(s, false);
                    if enabled {
                        s.color(active_theme().error)
                    } else {
                        s.color(active_theme().fg_dim)
                    }
                }),
        ))
        .style(|s| s.flex_row().gap(8.0).justify_end().width_full()),
    ))
    .style(|s| s.flex_col().gap(8.0).width_full())
    .into_any()
}

fn recover_body(ui: RemoteUi) -> AnyView {
    let u = ui.clone();
    let list = dyn_stack(
        move || u.dlg.recover_items.get(),
        |i: &RecoverItem| (i.site.clone(), i.id.clone()),
        {
            let ui = ui.clone();
            move |i: RecoverItem| {
                let (o, d) = (ui.clone(), ui.clone());
                let (s1, id1, s2, id2) = (i.site.clone(), i.id.clone(), i.site.clone(), i.id.clone());
                let caption = format!("{}  {}  ({} bytes)", ui.site_display_name(&i.site), i.path, i.bytes);
                stack((
                    label(move || caption.clone()).style(|s| s.flex_grow(1.0).min_width(0.0).text_ellipsis().font_size(11.0)),
                    button(" Open ", false, move || o.recover_open_item(&s1, &id1)),
                    button(" Put aside ", false, move || d.recover_discard_item(&s2, &id2)),
                ))
                .style(|s| s.flex_row().items_center().gap(4.0).width_full().padding_vert(2.0))
            }
        },
    )
    .style(|s| s.flex_col().width_full());
    let u = ui.clone();
    let none = label(move || if u.dlg.recover_items.with(|v| v.is_empty()) { "Nothing to recover.".to_string() } else { String::new() }).style(|s| s.font_size(11.0).color(active_theme().fg_dim));
    stack((
        dim("Text you edited on a server that never reached it. Open puts it back in a tab, modified; nothing is uploaded until you save and the server file is checked first.".into()),
        scroll(list).style(|s| s.width_full().height(200.0)),
        none,
    ))
    .style(|s| s.flex_col().gap(6.0).width_full())
    .into_any()
}

/// All the remote dialogs in one wrapper that exists only while one of them is up.
pub fn remote_dialogs(ui: RemoteUi) -> impl IntoView {
    let any = {
        let d = ui.dlg.clone();
        move || d.ask_host_open.get() || d.ask_secret_open.get() || d.conflict_open.get() || d.changed_open.get() || d.recover_open.get() || d.keep_open.get() || d.creds_open.get()
    };
    let (u1, u2, u3, u4, u5) = (ui.clone(), ui.clone(), ui.clone(), ui.clone(), ui.clone());
    let d = ui.dlg.clone();
    // The dialogs rebuild their body each time they open (see `modal`), so they always start from fresh state.
    dyn_container(
        move || (d.ask_host_open.get(), d.ask_secret_open.get(), d.conflict_open.get(), d.changed_open.get(), d.recover_open.get(), d.keep_open.get(), d.creds_open.get()),
        move |(host, secret, conflict, changed, recover, keep, creds)| {
            let host_modal = modal(u1.dlg.ask_host_open, "Is this really your server?", 460.0, 360.0, {
                let u = u1.clone();
                move || host_key_body(u.clone())
            });
            let _ = (&u2, &u3, &u4, &u5);
            if host {
                return host_modal.into_any();
            }
            if secret {
                return modal(u2.dlg.ask_secret_open, "Sign in", 420.0, 250.0, {
                    let u = u2.clone();
                    move || secret_body(u.clone())
                })
                .into_any();
            }
            if conflict {
                return modal(u3.dlg.conflict_open, "Save conflict", 500.0, 330.0, {
                    let u = u3.clone();
                    move || conflict_body(u.clone())
                })
                .into_any();
            }
            if changed {
                return modal(u4.dlg.changed_open, "Server identity changed", 480.0, 330.0, {
                    let u = u4.clone();
                    move || changed_key_body(u.clone())
                })
                .into_any();
            }
            if keep {
                let title = match u1.dlg.keep.get_untracked() {
                    Some(k) if k.offer.replace => "Replace the saved password?",
                    Some(k) if k.offer.kind == AskKind::Passphrase => "Keep this key passphrase?",
                    _ => "Keep this password?",
                };
                return modal(u1.dlg.keep_open, title, 440.0, 280.0, {
                    let u = u1.clone();
                    move || keep_body(u.clone())
                })
                .into_any();
            }
            if creds {
                return modal(u2.dlg.creds_open, "Credentials", 560.0, 380.0, {
                    let u = u2.clone();
                    move || credentials_body(u.clone())
                })
                .into_any();
            }
            if recover {
                return modal(u5.dlg.recover_open, "Recover unsaved changes", 520.0, 360.0, {
                    let u = u5.clone();
                    move || recover_body(u.clone())
                })
                .into_any();
            }
            empty().into_any()
        },
    )
    .style(move |s| if any() { s.absolute().inset_left(0.0).inset_top(0.0).size_full() } else { s.display(floem::style::Display::None) })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dates_are_plain_utc_days() {
        assert_eq!(date_text(0), "1970-01-01");
        assert_eq!(date_text(1_700_000_000), "2023-11-14");
        assert_eq!(date_text(951_782_400), "2000-02-29");
    }

    #[test]
    fn a_credential_row_has_no_value_and_never_shows_one() {
        use brouillon_editor_engine::remote::SecretId;
        let e = SavedEntry { id: SecretId::from_u128(0xab), label: "vps".into(), kind: "password", scope: "remote/s/password".into(), site_id: Some("s".into()), created: 0, last_used: 0 };
        let r = CredRow::of(&e, |id| format!("site {id}"));
        assert_eq!((r.site.as_str(), r.last_used.as_str(), r.id_hex.len()), ("site s", "never", 32));
    }

    #[test]
    fn the_site_name_gate_is_exact_but_forgiving_about_case_and_spaces() {
        assert!(name_matches("vps-prod", "vps-prod"));
        assert!(name_matches("  VPS-Prod ", "vps-prod"));
        assert!(!name_matches("vps", "vps-prod"));
        assert!(!name_matches("", "vps-prod"));
        assert!(!name_matches("", ""), "an empty name can never unlock the replace button");
    }
}
