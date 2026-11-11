//! Remote explorer UI: the source strip (`Local | one tab per site | +`), the remote tree
//! (`virtual_stack`, loading / error rows, client-side filter, context menu), the connect modal and
//! the site manager. All state lives in [`brouillon_editor_engine::remote::Explorer`]; this file is
//! glue between it and floem signals.
//!
//! Rules this file keeps:
//! * The UI thread never touches a provider. Every network call goes through the hub; its answers
//!   come back through `register_ext_trigger` + `RemoteUi::pump` (no timers).
//! * No secret is typed, kept or written here. There is deliberately NO password field yet: floem
//!   0.2 has no masked input, and the vault (`SecretRef`) is not built. Password sign-in is
//!   announced as "asked by the secure helper" and is not usable until that exists.
//! * Nothing here is started until a site is used: `RemoteUi::new` only creates signals, an empty
//!   explorer and one effect; the hub starts its worker threads on the first request.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::{Arc, Mutex};

use brouillon_editor_engine::browser::{BrowserPrefs, Crumb, Place, Source, SyncedFolder};
use brouillon_editor_engine::remote::{
    view_of, AuthMethod, Credentials, SecretRef, ConflictChoice, ConnState, Explorer, ExplorerNotice, PromptBroker, ProviderFactory, RemoteDocState, RemoteErrorKind, RemoteHub, RemoteOrigin, RemotePath, RemoteRow, RemoteRowKind,
    RemoteSaveState, ResolvedView, SaveEvent, SaveService, SiteProfile, SiteStore, Submit, LOCAL_SITE_ID,
};
use floem::event::{Event, EventListener};
use floem::ext_event::register_ext_trigger;
use floem::keyboard::{Key, NamedKey};
use floem::menu::{Menu, MenuItem};
use floem::reactive::{create_effect, create_rw_signal, RwSignal, SignalGet, SignalUpdate, SignalWith, Trigger};
use floem::style::CursorStyle;
use floem::views::{dyn_container, dyn_stack, empty, label, scroll, stack, text_input, Decorators};
use floem::{AnyView, IntoView};

use crate::remote_dialogs::{ChangedKey, ConflictState, CredRow, DialogState, KeepAsk};
use crate::theme_ctx::active_theme;
use crate::ui_kit::{button_style, field_style, modal, ring_button, FocusRing};

/// Smoke-test trace: with `BROUILLON_REMOTE_DEBUG=1` the remote UI writes one `[remote-dbg]` line per
/// observable state change on stderr (strip, rows, selection, chips, dialogs, messages) so a GUI script
/// can assert state without reading pixels. Off by default; carries no secret (never logs typed text
/// of a password field, only the site address typed in the connect form).
pub(crate) fn dbg_on() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var("BROUILLON_REMOTE_DEBUG").map(|v| v == "1").unwrap_or(false)) || brouillon_core::probe::enabled()
}

pub(crate) fn dbg(msg: impl FnOnce() -> String) {
    if dbg_on() {
        let line = format!("[remote-dbg] {}", msg());
        brouillon_core::probe::push(&line);
        eprintln!("{line}");
    }
}

thread_local! {
    static LAST_SYNC_LINE: RefCell<String> = const { RefCell::new(String::new()) };
}

pub(crate) const ROW_H: f64 = 20.0;
pub const PANE_W: f64 = 240.0;

/// What the host (main.rs) provides: open a document, flag it saved, say something in the status line.
#[derive(Clone)]
pub struct RemoteHost {
    pub open_doc: Rc<dyn Fn(RemoteOrigin, Vec<u8>)>,
    /// The buffer now equals the server copy (clear the modified marker).
    pub saved: Rc<dyn Fn(RemoteOrigin, Vec<u8>)>,
    pub say: Rc<dyn Fn(String)>,
    /// Open a file of this computer as a normal tab (the Local source of the browser).
    pub open_local: Rc<dyn Fn(PathBuf)>,
    /// Put `bytes` in the tab of `origin` (opening it if needed). `true` = the tab counts as modified
    /// (recovered unsaved changes); `false` = it equals the server copy (after a reload).
    pub replace_doc: Rc<dyn Fn(RemoteOrigin, Vec<u8>, bool)>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Prompt {
    NewFile(RemotePath),
    NewFolder(RemotePath),
    Rename(RemotePath),
    Delete { path: RemotePath, is_dir: bool },
}

/// One tab of the source strip.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StripTab {
    pub id: String,
    pub label: String,
    /// A word, never colour alone.
    pub state: &'static str,
    pub active: bool,
    pub unencrypted: bool,
}

/// What the tree area shows instead of rows while the site is not connected.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct PaneStatus {
    pub text: String,
    pub can_retry: bool,
}

/// What the navigation buttons need to know (one signal, recomputed by `sync`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct NavFlags {
    pub back: bool,
    pub forward: bool,
    pub up: bool,
    /// The shown folder is pinned in the favorites.
    pub pinned: bool,
}

#[derive(Clone)]
pub struct RemoteUi {
    pub(crate) ex: Rc<RefCell<Explorer>>,
    pub(crate) host: Rc<RefCell<Option<RemoteHost>>>,
    sites_path: Rc<PathBuf>,
    /// Favorites, recent folders, layout (split, collapsed) and sort: `browser.toml` next to `sites.toml`.
    pub prefs: RwSignal<BrowserPrefs>,
    pub(crate) prefs_path: Rc<PathBuf>,
    pub crumbs: RwSignal<Vec<Crumb>>,
    pub nav: RwSignal<NavFlags>,
    /// Cloud folders the OS already syncs (found once, off the UI thread, when the browser is first shown).
    pub synced: RwSignal<Vec<SyncedFolder>>,
    pub(crate) synced_slot: Arc<Mutex<Option<Vec<SyncedFolder>>>>,
    pub(crate) synced_started: Rc<Cell<bool>>,
    /// The path bar is a text field (Ctrl+L) instead of clickable segments.
    pub path_edit: RwSignal<bool>,
    pub path_text: RwSignal<String>,
    /// Height of the sidebar, to turn a divider drag into a split ratio.
    pub split_h: RwSignal<f64>,
    pub rows: RwSignal<im::Vector<RemoteRow>>,
    pub strip: RwSignal<Vec<StripTab>>,
    pub active: RwSignal<Option<String>>,
    pub pane_status: RwSignal<PaneStatus>,
    pub header: RwSignal<String>,
    pub selected: RwSignal<Option<RemotePath>>,
    /// Who owns the keyboard on the pane/editor side (see `kbd_focus`).
    pub kbd: RwSignal<crate::kbd_focus::Base>,
    /// The command palette is open (set by the host; only used for ownership).
    pub palette_open: RwSignal<bool>,
    /// A preference-style modal (settings, shortcuts, import, about, first run) is open (set by the host).
    pub other_modal: RwSignal<bool>,
    pub filter: RwSignal<String>,
    pub prompt: RwSignal<Option<Prompt>>,
    pub prompt_text: RwSignal<String>,
    pub message: RwSignal<String>,
    /// State chip per open remote document, keyed `(site id, path)`. The save engine publishes here.
    pub doc_states: RwSignal<HashMap<(String, String), RemoteDocState>>,
    pub saved_sites: RwSignal<Vec<SiteProfile>>,
    pub connect_open: RwSignal<bool>,
    pub sites_open: RwSignal<bool>,
    f_address: RwSignal<String>,
    f_folder: RwSignal<String>,
    f_auth: RwSignal<u8>,
    f_remember: RwSignal<bool>,
    f_error: RwSignal<String>,
    /// Bytes handed to the last upload of each document, so "saved" is only claimed if nothing was typed since.
    sent: Rc<RefCell<HashMap<(String, String), Vec<u8>>>>,
    pub(crate) trigger: Trigger,
    prompts: Rc<RefCell<Option<Arc<PromptBroker>>>>,
    /// Saved passwords (the vault behind a small service). `None` in tests: every question is then asked.
    creds: Rc<RefCell<Option<Arc<Credentials>>>>,
    saves: Rc<RefCell<Option<Arc<SaveService>>>>,
    /// Ask-once, conflict, changed-key and recovery dialogs (state only; views are in `remote_dialogs`).
    pub dlg: DialogState,
}

impl RemoteUi {
    /// `factory` turns a profile into a provider (dev flag: the built-in fake server).
    /// Must be called on the UI thread inside the reactive scope.
    pub fn new(factory: Arc<dyn ProviderFactory>, sites_path: PathBuf, connect_open: RwSignal<bool>, sites_open: RwSignal<bool>, dialog_open: RwSignal<bool>) -> Self {
        let trigger = Trigger::new();
        let hub = RemoteHub::new(move || register_ext_trigger(trigger), factory);
        let prefs_path = sites_path.with_file_name("browser.toml");
        let prefs = BrowserPrefs::load(&prefs_path);
        let mut explorer = Explorer::new(hub);
        explorer.set_sort(prefs.sort);
        let ui = Self {
            ex: Rc::new(RefCell::new(explorer)),
            host: Rc::new(RefCell::new(None)),
            prefs: create_rw_signal(prefs),
            prefs_path: Rc::new(prefs_path),
            crumbs: create_rw_signal(Vec::new()),
            nav: create_rw_signal(NavFlags::default()),
            synced: create_rw_signal(Vec::new()),
            synced_slot: Arc::new(Mutex::new(None)),
            synced_started: Rc::new(Cell::new(false)),
            path_edit: create_rw_signal(false),
            path_text: create_rw_signal(String::new()),
            split_h: create_rw_signal(600.0),
            saved_sites: create_rw_signal(SiteStore::load(&sites_path).sites),
            sites_path: Rc::new(sites_path),
            rows: create_rw_signal(im::Vector::new()),
            strip: create_rw_signal(Vec::new()),
            active: create_rw_signal(None),
            pane_status: create_rw_signal(PaneStatus::default()),
            header: create_rw_signal(String::new()),
            selected: create_rw_signal(None),
            kbd: create_rw_signal(crate::kbd_focus::Base::Editor),
            palette_open: create_rw_signal(false),
            other_modal: create_rw_signal(false),
            filter: create_rw_signal(String::new()),
            prompt: create_rw_signal(None),
            prompt_text: create_rw_signal(String::new()),
            message: create_rw_signal(String::new()),
            doc_states: create_rw_signal(HashMap::new()),
            connect_open,
            sites_open,
            f_address: create_rw_signal(String::new()),
            f_folder: create_rw_signal(String::new()),
            f_auth: create_rw_signal(0),
            f_remember: create_rw_signal(true),
            f_error: create_rw_signal(String::new()),
            sent: Rc::new(RefCell::new(HashMap::new())),
            trigger,
            prompts: Rc::new(RefCell::new(None)),
            creds: Rc::new(RefCell::new(None)),
            saves: Rc::new(RefCell::new(None)),
            dlg: DialogState::new(dialog_open),
        };
        let me = ui.clone();
        create_effect(move |_| {
            trigger.track();
            me.pump();
        });
        let me = ui.clone();
        create_effect(move |_| {
            // Closing the "Keep this password?" card (x, Escape) is "use it this time only".
            if !me.dlg.keep_open.get() && me.dlg.keep.get_untracked().is_some() {
                me.resolve_keep(false);
            }
        });
        let me = ui.clone();
        create_effect(move |_| {
            // Closing the question window (x, Escape) is a cancel: the helper must never wait on a hidden dialog.
            let open = me.dlg.ask_host_open.get() || me.dlg.ask_secret_open.get();
            if !open && me.dlg.ask.get_untracked().is_some() {
                me.answer_ask(None);
            }
        });
        let me = ui.clone();
        create_effect(move |_| {
            if !me.dlg.conflict_open.get() && me.dlg.conflict.get_untracked().is_some() {
                me.dlg.conflict.set(None);
            }
        });
        let me = ui.clone();
        create_effect(move |_| {
            let f = me.filter.get();
            me.ex.borrow_mut().set_filter(&f);
            me.sync();
        });
        if dbg_on() {
            // Chip changes, one line each (the chip is a pure view of this map).
            let me = ui.clone();
            create_effect(move |prev: Option<HashMap<(String, String), String>>| {
                let cur: HashMap<(String, String), String> = me.doc_states.with(|m| m.iter().map(|(k, v)| (k.clone(), v.chip().label)).collect());
                for (k, label) in &cur {
                    if prev.as_ref().and_then(|p| p.get(k)) != Some(label) {
                        dbg(|| format!("chip {} | {}", k.1, label));
                    }
                }
                cur
            });
            // Which windows are open.
            let me = ui.clone();
            create_effect(move |prev: Option<String>| {
                let line = format!(
                    "dialogs connect={} sites={} conflict={} ask_host={} ask_secret={} changed={} recover={}",
                    me.connect_open.get(),
                    me.sites_open.get(),
                    me.dlg.conflict_open.get(),
                    me.dlg.ask_host_open.get(),
                    me.dlg.ask_secret_open.get(),
                    me.dlg.changed_open.get(),
                    me.dlg.recover_open.get()
                );
                if prev.as_ref() != Some(&line) {
                    dbg(|| line.clone());
                }
                line
            });
            // Who owns the keyboard (one line per change): editor | browser | path | filter | modal | palette.
            let me = ui.clone();
            create_effect(move |prev: Option<&'static str>| {
                let o = me.kbd_owner().as_str();
                if prev != Some(o) {
                    dbg(|| format!("kbd-owner={o}"));
                }
                o
            });
        }
        ui
    }

    /// The current keyboard owner, overlays included.
    pub fn kbd_owner(&self) -> crate::kbd_focus::Owner {
        let modal = self.connect_open.get() || self.sites_open.get() || self.dlg.conflict_open.get() || self.dlg.ask_host_open.get() || self.dlg.ask_secret_open.get() || self.dlg.changed_open.get() || self.dlg.recover_open.get() || self.other_modal.get();
        crate::kbd_focus::owner(self.kbd.get(), modal, self.palette_open.get())
    }

    /// The browser rows have the keyboard (the root key handler routes arrows / Enter / type-to-filter to the pane).
    pub fn rows_focused(&self) -> bool {
        self.kbd.get_untracked() == crate::kbd_focus::Base::Rows
    }

    /// Feed one event to the focus machine; drop the real text-field focus when the event says so.
    pub fn kbd_event(&self, ev: crate::kbd_focus::Ev) {
        let cur = self.kbd.get_untracked();
        let next = crate::kbd_focus::step(cur, ev);
        if next != cur {
            self.kbd.set(next);
        }
        if crate::kbd_focus::drops_real_focus(ev) {
            floem::action::clear_app_focus();
        }
    }

    pub fn set_host(&self, h: RemoteHost) {
        *self.host.borrow_mut() = Some(h);
    }

    /// Plug in the helper-side services: the question broker (password / passphrase / host key) and
    /// the save engine. Both wake the UI through the same trigger as the hub.
    pub fn attach_services(&self, prompts: Arc<PromptBroker>, saves: Arc<SaveService>) {
        let t = self.trigger;
        prompts.set_waker(move || register_ext_trigger(t));
        let t = self.trigger;
        saves.set_waker(move || register_ext_trigger(t));
        *self.prompts.borrow_mut() = Some(prompts);
        *self.saves.borrow_mut() = Some(saves);
    }

    /// Plug in the saved-password service: password and passphrase questions of a site with a saved
    /// password are answered from the vault without a dialog.
    pub fn attach_credentials(&self, c: Arc<Credentials>) {
        if let Some(b) = self.prompts.borrow().as_ref() {
            b.set_vault(c.clone());
        }
        *self.creds.borrow_mut() = Some(c);
        self.sync_saved_sites();
    }

    fn credentials(&self) -> Option<Arc<Credentials>> {
        self.creds.borrow().clone()
    }

    /// Tell the service which sites have a saved password (their `secret_ref` in `sites.toml`).
    fn sync_saved_sites(&self) {
        if let Some(c) = self.credentials() {
            let ids: Vec<String> = self.saved_sites.with_untracked(|v| v.iter().filter(|s| s.secret_ref.0.is_some()).map(|s| s.id.clone()).collect());
            c.set_saved_sites(ids.iter().map(|s| s.as_str()));
        }
    }

    /// Change one saved profile and write `sites.toml`.
    fn edit_saved(&self, id: &str, f: impl FnOnce(&mut SiteProfile)) {
        self.saved_sites.update(|v| {
            let mut st = SiteStore { sites: std::mem::take(v) };
            if let Some(p) = st.sites.iter_mut().find(|p| p.id == id) {
                f(p);
            }
            if let Err(e) = st.save(&self.sites_path) {
                eprintln!("[remote] could not save the site list: {e}");
            }
            *v = st.sites;
        });
        self.sync_saved_sites();
    }

    /// At launch: tell the user, once, that unsaved remote changes were found on disk.
    pub fn announce_recoverable(&self) {
        let n = self.saves.borrow().as_ref().map(|s| s.recoverable().len()).unwrap_or(0);
        if n > 0 {
            self.say(format!("{n} unsaved remote change{} found from an earlier session. Command palette: Remote: Recover unsaved changes.", if n == 1 { "" } else { "s" }));
        }
    }

    fn say(&self, msg: impl Into<String>) {
        let msg = msg.into();
        dbg(|| format!("say {msg}"));
        self.message.set(msg.clone());
        if let Some(h) = self.host.borrow().as_ref() {
            (h.say)(msg);
        }
    }

    /// Any site open (the source strip is only drawn then).
    pub fn any_site(&self) -> bool {
        self.strip.with(|s| !s.is_empty())
    }

    /// Recompute every signal from the explorer. Cheap: cache reads only.
    pub fn sync(&self) {
        if self.ex.borrow().sites.is_empty() && self.strip.with_untracked(|s| s.is_empty()) {
            return;
        }
        let mut ex = self.ex.borrow_mut();
        let rows = ex.rows();
        let strip: Vec<StripTab> = ex
            .sites
            .iter()
            .enumerate()
            .map(|(i, s)| StripTab { id: s.profile.id.clone(), label: s.profile.name.clone(), state: s.conn.word(), active: ex.active == Some(i), unencrypted: s.profile.is_unencrypted() })
            .collect();
        let (active, header, sel, status) = match ex.active_site() {
            Some(s) => {
                let status = match &s.conn {
                    ConnState::Connected => PaneStatus::default(),
                    ConnState::Connecting => PaneStatus { text: "Connecting...".into(), can_retry: false },
                    ConnState::Failed(e) => PaneStatus { text: e.plain(), can_retry: true },
                    ConnState::Disconnected => PaneStatus { text: "Not connected".into(), can_retry: true },
                };
                let shown = if s.profile.id == LOCAL_SITE_ID { brouillon_editor_engine::remote::local::display_path(&s.tree.root, cfg!(windows)) } else { s.tree.root.to_string() };
                (Some(s.profile.id.clone()), format!("{}:{}", s.profile.name, shown), s.tree.selected.clone(), status)
            }
            None => (None, String::new(), None, PaneStatus::default()),
        };
        let local = ex.active_site().is_some_and(|s| s.profile.id == LOCAL_SITE_ID);
        let root = ex.current_root();
        let (nav_back, nav_fwd) = (ex.can_back(), ex.can_forward());
        drop(ex);
        let crumbs = root.as_ref().map(|r| brouillon_editor_engine::browser::breadcrumbs(r, local && cfg!(windows))).unwrap_or_default();
        let pinned = match (&root, &active) {
            (Some(r), Some(id)) => {
                let src = if local { Source::Local } else { Source::Site(id.clone()) };
                self.prefs.with_untracked(|p| p.is_pinned(&Place { source: src.key(), path: r.to_string() }))
            }
            _ => false,
        };
        self.nav.set(NavFlags { back: nav_back, forward: nav_fwd, up: root.as_ref().is_some_and(|r| !r.is_root()), pinned });
        self.crumbs.set(crumbs);
        if dbg_on() {
            let loading = rows.iter().filter(|r| matches!(r.kind, RemoteRowKind::Loading)).count();
            let errors = rows.iter().filter(|r| matches!(r.kind, RemoteRowKind::Error(_))).count();
            let names: Vec<String> = rows.iter().filter(|r| r.is_entry()).map(|r| format!("{}{}", r.path, if r.is_dir { "/" } else { "" })).collect();
            let line = format!(
                "tree strip=[{}] active={} selected={} loading={} errors={} entries=[{}] pane=\"{}\"",
                strip.iter().map(|t| format!("{}:{}{}", t.label, t.state, if t.active { "*" } else { "" })).collect::<Vec<_>>().join(","),
                active.clone().unwrap_or_default(),
                sel.as_ref().map(|p| p.to_string()).unwrap_or_default(),
                loading,
                errors,
                names.join(","),
                status.text
            );
            LAST_SYNC_LINE.with(|l| {
                if *l.borrow() != line {
                    dbg(|| line.clone());
                    *l.borrow_mut() = line;
                }
            });
        }
        self.rows.set(rows.into());
        self.strip.set(strip);
        self.active.set(active);
        self.header.set(header);
        self.selected.set(sel);
        self.pane_status.set(status);
    }

    /// UI thread, after the hub woke us: apply answers and tell the user.
    pub fn pump(&self) {
        let notices = self.ex.borrow_mut().pump();
        for n in notices {
            self.notice(n);
        }
        let events = self.saves.borrow().as_ref().map(|s| s.drain()).unwrap_or_default();
        for e in events {
            self.save_event(e);
        }
        let current = self.prompts.borrow().as_ref().and_then(|b| b.current());
        if current != self.dlg.ask.get_untracked() {
            self.dlg.show_ask(current);
        }
        if let Some(found) = self.synced_slot.lock().ok().and_then(|mut g| g.take()) {
            self.synced.set(found);
        }
        self.sync();
    }

    fn notice(&self, n: ExplorerNotice) {
        match n {
            ExplorerNotice::Connected { site } => {
                self.say(format!("Connected to {}", self.site_name(&site)));
                self.after_connect(&site);
                // Crash recovery for this site's journal (server side only: it never uploads a buffer).
                let provider = self.ex.borrow().hub.provider(&site);
                if let (Some(saves), Some(p)) = (self.saves.borrow().clone(), provider) {
                    saves.recover(&site, p);
                }
            }
            ExplorerNotice::ConnectFailed { site, error } => {
                if error.kind == RemoteErrorKind::HostKeyChanged {
                    let host = {
                        let ex = self.ex.borrow();
                        ex.site_index(&site).map(|i| (ex.sites[i].profile.host.clone(), ex.sites[i].profile.port))
                    };
                    if let Some((host, port)) = host {
                        self.dlg.show_changed_key(ChangedKey { site_id: site.clone(), site_name: self.site_name(&site), host, port });
                    }
                }
                let saved_failed = self.credentials().map(|c| c.failed(&site)).unwrap_or(false);
                if saved_failed {
                    self.say(format!("{}: the saved password was not accepted. Brouillon will ask for a new one: connect again.", self.site_name(&site)));
                } else {
                    self.say(format!("{}: {}", self.site_name(&site), error.plain()))
                }
            }
            ExplorerNotice::FileLoaded { origin, bytes } => {
                // A safe provider also hands over the server stamp it saw at read time: the base of later saves.
                let path = RemotePath::new(&origin.path);
                let provider = self.ex.borrow().hub.provider(&origin.site);
                let saves = self.saves.borrow().clone();
                if let (Some(p), Some(saves)) = (provider, saves) {
                    let key = (origin.site.clone(), origin.path.clone());
                    let safe = p.safe_fs().is_some();
                    match p.take_open_stamp(&path) {
                        Some(stamp) if safe => saves.attach(key, &bytes, stamp),
                        // A safe provider without a stamp must NOT fall back to the plain overwrite.
                        _ if safe => saves.attach_unstamped(key, &bytes),
                        _ => saves.forget(&key),
                    }
                    dbg(|| format!("save-tracking {} safe={} tracked={}", origin.path, safe, saves.is_tracked(&(origin.site.clone(), origin.path.clone()))));
                }
                let host = self.host.borrow().clone();
                dbg(|| format!("file-loaded {} on {} ({} bytes)", origin.path, origin.site_label, bytes.len()));
                if let Some(h) = host {
                    self.doc_states.update(|m| {
                        m.insert((origin.site.clone(), origin.path.clone()), RemoteDocState::synced(origin.site_label.clone()));
                    });
                    (h.open_doc)(origin, bytes);
                }
            }
            ExplorerNotice::OpenFailed { origin, error } => self.say(format!("Could not open {}: {}", origin.path, error.plain())),
            ExplorerNotice::Written { origin, result, .. } => {
                let key = (origin.site.clone(), origin.path.clone());
                match result {
                    Ok(()) => {
                        self.doc_states.update(|m| {
                            m.insert(key, RemoteDocState::synced(origin.site_label.clone()));
                        });
                        if let Some(h) = self.host.borrow().clone() {
                            let sent = self.sent.borrow_mut().remove(&(origin.site.clone(), origin.path.clone())).unwrap_or_default();
                            (h.saved)(origin.clone(), sent);
                        }
                        self.say(format!("Saved {} to {}", origin.path, origin.site_label));
                    }
                    Err(e) => {
                        let mut st = RemoteDocState::synced(origin.site_label.clone()).with(RemoteSaveState::Pending);
                        st.note = Some(e.plain());
                        self.doc_states.update(|m| {
                            m.insert(key, st);
                        });
                        self.say(format!("Not saved ({}). Your text is safe in the editor.", e.plain()));
                    }
                }
            }
            ExplorerNotice::Changed { what, .. } => self.say(what),
            ExplorerNotice::ChangeFailed { what, error, .. } => self.say(format!("{what} failed: {}", error.plain())),
            ExplorerNotice::WentOffline { site, error } => {
                self.doc_states.update(|m| {
                    for ((s, _), st) in m.iter_mut() {
                        if *s == site {
                            st.save = RemoteSaveState::Offline;
                            st.note = Some(error.plain());
                        }
                    }
                });
                self.say(format!("{}: {}", self.site_name(&site), error.plain()));
            }
        }
    }

    fn site_name(&self, id: &str) -> String {
        let ex = self.ex.borrow();
        ex.site_index(id).map(|i| ex.sites[i].profile.name.clone()).unwrap_or_else(|| id.to_string())
    }

    // ---------------------------------------------------------------- connections

    pub fn connect_profile(&self, p: SiteProfile, remember: bool) {
        let mut p = p;
        if remember {
            self.saved_sites.update(|v| {
                let mut st = SiteStore { sites: std::mem::take(v) };
                // Keep what is already known about this site (a saved password, the answered question).
                if let Some(old) = st.get(&p.id) {
                    p.secret_ref = old.secret_ref.clone();
                    p.keep_asked = old.keep_asked;
                }
                st.upsert(p.clone());
                if let Err(e) = st.save(&self.sites_path) {
                    eprintln!("[remote] could not save the site list: {e}");
                }
                *v = st.sites;
            });
        }
        dbg(|| format!("connect name={} host={} root={}", p.name, p.host, p.root_path));
        self.ex.borrow_mut().connect(p);
        self.kbd_event(crate::kbd_focus::Ev::ToEditor);
        // A server is browsed in the browser section: show it.
        self.set_browser_visible(true);
        self.sync();
    }

    /// Dev command: the built-in fake server, no network.
    pub fn connect_demo(&self) {
        if let Some(p) = SiteProfile::from_quick("demo@demo", AuthMethod::Agent, "/var/www") {
            let mut p = p;
            p.name = "demo".into();
            self.connect_profile(p, false);
        }
    }

    pub fn activate(&self, id: Option<String>) {
        let retry = {
            let mut ex = self.ex.borrow_mut();
            ex.activate(id.as_deref());
            id.filter(|id| ex.site_index(id).is_some_and(|i| matches!(ex.sites[i].conn, ConnState::Failed(_) | ConnState::Disconnected)))
        };
        if let Some(id) = retry {
            self.ex.borrow_mut().reconnect(&id);
        }
        self.prompt.set(None);
        self.sync();
    }

    pub fn retry_active(&self) {
        let id = self.active.get_untracked();
        if let Some(id) = id {
            self.ex.borrow_mut().reconnect(&id);
            self.sync();
        }
    }

    pub fn close_site(&self, id: &str) {
        dbg(|| format!("close-site {id}"));
        self.ex.borrow_mut().close_site(id);
        self.doc_states.update(|m| {
            for ((s, _), st) in m.iter_mut() {
                if s == id {
                    st.save = RemoteSaveState::Offline;
                    st.note = Some("The site was closed. Reconnect to save.".into());
                }
            }
        });
        self.sync();
    }

    pub fn remove_saved(&self, id: &str) {
        // A site that is removed takes its saved password with it.
        if let Some(c) = self.credentials() {
            let _ = c.forget(id);
        }
        self.saved_sites.update(|v| {
            let mut st = SiteStore { sites: std::mem::take(v) };
            st.remove(id);
            let _ = st.save(&self.sites_path);
            *v = st.sites;
        });
    }

    pub fn toggle_flag(&self, id: &str, production: bool) {
        self.saved_sites.update(|v| {
            if let Some(s) = v.iter_mut().find(|s| s.id == id) {
                if production {
                    s.production = !s.production;
                } else {
                    s.read_only = !s.read_only;
                }
            }
            let _ = SiteStore { sites: v.clone() }.save(&self.sites_path);
        });
    }

    // ---------------------------------------------------------------- the tree

    pub fn click(&self, row: &RemoteRow) {
        self.kbd_event(crate::kbd_focus::Ev::RowPointer);
        if !row.is_entry() {
            if matches!(row.kind, RemoteRowKind::Error(_)) {
                self.ex.borrow_mut().refresh_dir(&row.path);
            }
            self.sync();
            return;
        }
        {
            let mut ex = self.ex.borrow_mut();
            ex.select(&row.path);
            if row.is_dir && !row.is_symlink {
                ex.toggle(&row.path);
            }
        }
        self.sync();
    }

    /// Double-click / Enter on a file.
    pub fn open(&self, path: &RemotePath) {
        self.kbd_event(crate::kbd_focus::Ev::FileOpened);
        if self.active_is_local() {
            // The local disk is read by the editor's normal open path (encodings, large files, recents), not the hub.
            let native = brouillon_editor_engine::remote::local::to_native(path, cfg!(windows)).ok().flatten();
            let host = self.host.borrow().clone();
            if let (Some(p), Some(h)) = (native, host) {
                (h.open_local)(p);
            }
            return;
        }
        if self.ex.borrow_mut().open_file(path).is_some() {
            self.say(format!("Opening {path}..."));
        }
    }

    /// Keyboard while the pane has focus: `up down left right enter`, `f5`, `delete`, `f2`.
    pub fn key(&self, k: &str) {
        let rows: Vec<RemoteRow> = self.rows.with_untracked(|r| r.iter().cloned().collect());
        let sel = self.selected.get_untracked();
        let cur = sel.as_ref().and_then(|s| rows.iter().find(|r| r.is_entry() && &r.path == s)).cloned();
        match k {
            "up" | "down" => {
                if let Some(s) = self.ex.borrow_mut().active_site_mut() {
                    s.tree.move_selection(&rows, if k == "up" { -1 } else { 1 });
                }
            }
            "right" => {
                if let Some(r) = cur.filter(|r| r.is_dir && !r.is_symlink && !r.expanded) {
                    self.ex.borrow_mut().toggle(&r.path);
                }
            }
            "left" => {
                if let Some(r) = cur {
                    let mut ex = self.ex.borrow_mut();
                    if r.is_dir && r.expanded {
                        ex.toggle(&r.path);
                    } else if let Some(parent) = r.path.parent() {
                        let root = ex.active_site().map(|s| s.tree.root.clone());
                        if root.as_ref() != Some(&r.path) && rows.iter().any(|x| x.is_entry() && x.path == parent) {
                            ex.select(&parent);
                        }
                    }
                }
            }
            "enter" => {
                if let Some(r) = cur {
                    if r.is_dir {
                        // Enter goes INTO the folder (Right arrow expands it in place).
                        self.enter(&r.path);
                        return;
                    } else {
                        self.open(&r.path);
                    }
                }
            }
            "f5" => self.ex.borrow_mut().refresh_selected(&rows),
            "backspace" => {
                // With text in the filter, Backspace edits it; otherwise it goes up one folder.
                if self.filter.with_untracked(|f| f.is_empty()) {
                    self.go_up();
                } else {
                    self.filter.update(|f| {
                        f.pop();
                    });
                }
                return;
            }
            "up_dir" => {
                self.go_up();
                return;
            }
            "back" => {
                self.go_back();
                return;
            }
            "forward" => {
                self.go_forward();
                return;
            }
            "path" => {
                self.begin_path_edit();
                return;
            }
            "home" => {
                self.go_home();
                return;
            }
            "f2" => {
                if let Some(r) = cur {
                    self.begin_prompt(Prompt::Rename(r.path));
                }
            }
            "delete" => {
                if let Some(r) = cur {
                    self.begin_prompt(Prompt::Delete { path: r.path, is_dir: r.is_dir });
                }
            }
            _ => {}
        }
        self.sync();
    }

    pub fn refresh(&self) {
        let rows: Vec<RemoteRow> = self.rows.with_untracked(|r| r.iter().cloned().collect());
        self.ex.borrow_mut().refresh_selected(&rows);
        self.sync();
    }

    pub fn begin_prompt(&self, p: Prompt) {
        self.prompt_text.set(match &p {
            Prompt::Rename(path) => path.file_name().unwrap_or("").to_string(),
            _ => String::new(),
        });
        self.prompt.set(Some(p));
    }

    pub fn submit_prompt(&self) {
        let Some(p) = self.prompt.get_untracked() else { return };
        let text = self.prompt_text.get_untracked();
        let r = {
            let mut ex = self.ex.borrow_mut();
            match &p {
                Prompt::NewFile(dir) => ex.create_file(dir, &text),
                Prompt::NewFolder(dir) => ex.create_folder(dir, &text),
                Prompt::Rename(path) => ex.rename(path, &text),
                Prompt::Delete { path, is_dir } => ex.delete(path, *is_dir),
            }
        };
        match r {
            Ok(_) => self.prompt.set(None),
            Err(msg) => self.say(msg),
        }
    }

    /// Folder a "new file" should go to: the selected folder, the selected file's folder, or the root.
    fn target_dir(&self, row: Option<&RemoteRow>) -> RemotePath {
        match row {
            Some(r) if r.is_dir => r.path.clone(),
            Some(r) => r.path.parent().unwrap_or_else(RemotePath::root),
            None => self.ex.borrow().active_site().map(|s| s.tree.root.clone()).unwrap_or_else(RemotePath::root),
        }
    }

    pub fn copy_path(&self, path: &RemotePath) {
        match arboard::Clipboard::new().and_then(|mut c| c.set_text(path.to_string())) {
            Ok(()) => self.say(format!("Copied {path}")),
            Err(_) => self.say("Could not reach the clipboard."),
        }
    }

    fn row_menu(&self, row: RemoteRow) -> Menu {
        let read_only = self.ex.borrow().active_site().is_some_and(|s| s.profile.read_only);
        let item = |title: &str, enabled: bool, f: Box<dyn Fn()>| MenuItem::new(title).enabled(enabled).action(f);
        let ui = self.clone();
        let mut m = Menu::new("");
        let r = row.clone();
        if row.is_dir {
            let (u, r2) = (ui.clone(), r.clone());
            m = m.entry(item("Open / close", true, Box::new(move || u.click(&r2))));
            let (u, p) = (ui.clone(), r.path.clone());
            m = m.entry(item("Open this folder here", true, Box::new(move || u.enter(&p))));
            let (u, p) = (ui.clone(), r.path.clone());
            m = m.entry(item("Pin / unpin this folder", true, Box::new(move || u.toggle_pin_of(&p))));
        } else {
            let (u, p) = (ui.clone(), r.path.clone());
            m = m.entry(item("Open", true, Box::new(move || u.open(&p))));
        }
        m = m.separator();
        let (u, r2) = (ui.clone(), r.clone());
        m = m.entry(item("New file...", !read_only, Box::new(move || u.begin_prompt(Prompt::NewFile(u.target_dir(Some(&r2)))))));
        let (u, r2) = (ui.clone(), r.clone());
        m = m.entry(item("New folder...", !read_only, Box::new(move || u.begin_prompt(Prompt::NewFolder(u.target_dir(Some(&r2)))))));
        let (u, p) = (ui.clone(), r.path.clone());
        m = m.entry(item("Rename...", !read_only, Box::new(move || u.begin_prompt(Prompt::Rename(p.clone())))));
        let (u, p, d) = (ui.clone(), r.path.clone(), r.is_dir);
        m = m.entry(item("Delete...", !read_only, Box::new(move || u.begin_prompt(Prompt::Delete { path: p.clone(), is_dir: d }))));
        m = m.separator();
        let (u, p) = (ui.clone(), r.path.clone());
        m = m.entry(item("Copy remote path", true, Box::new(move || u.copy_path(&p))));
        let u = ui.clone();
        m.entry(item("Refresh", true, Box::new(move || u.refresh())))
    }

    // ---------------------------------------------------------------- documents

    /// Publish the state chip of a document. THE hook for the save engine: whoever runs the verified
    /// upload calls this on the UI thread as the document moves between Synced / Saving / Pending /
    /// Conflict / Offline. The status bar and tab chip are pure views of it.
    pub fn set_doc_state(&self, origin: &RemoteOrigin, st: RemoteDocState) {
        self.doc_states.update(|m| {
            m.insert((origin.site.clone(), origin.path.clone()), st);
        });
    }

    /// Tracked read for the status-bar effect.
    pub fn doc_state(&self, origin: &RemoteOrigin) -> Option<RemoteDocState> {
        self.doc_states.with(|m| m.get(&(origin.site.clone(), origin.path.clone())).cloned())
    }

    /// Ctrl+S on a remote document. With a safe provider (the SFTP helper) this runs the SafeSaver on a
    /// worker thread: upload to a temp name, verify, atomic swap, refuse on conflict. Only the demo
    /// server (no safe provider) keeps the plain upload.
    pub fn save_doc(&self, origin: &RemoteOrigin, bytes: Vec<u8>) {
        let key = (origin.site.clone(), origin.path.clone());
        let read_only = {
            let ex = self.ex.borrow();
            ex.site_index(&origin.site).is_some_and(|i| ex.sites[i].profile.read_only)
        };
        if read_only {
            self.say("This site is read-only. Nothing was saved.");
            return;
        }
        let saves = self.saves.borrow().clone();
        let provider = self.ex.borrow().hub.provider(&origin.site);
        if let (Some(saves), Some(p)) = (saves.clone(), provider) {
            let submitted = saves.save(&key, bytes.clone(), p);
            dbg(|| format!("save-submit {} {:?} bytes={}", origin.path, submitted, bytes.len()));
            match submitted {
                Submit::Started => {
                    self.set_doc_state(origin, RemoteDocState::synced(origin.site_label.clone()).with(RemoteSaveState::Saving));
                    return;
                }
                Submit::Busy => {
                    self.say("Still saving the previous version. Press Ctrl+S again in a moment.");
                    return;
                }
                Submit::NotTracked => {} // plain upload below
            }
        } else if saves.is_some() && !self.ex.borrow().hub.is_connected(&origin.site) {
            // Offline: the buffer stays modified in the tab; nothing was lost.
            let mut st = RemoteDocState::synced(origin.site_label.clone()).with(RemoteSaveState::Pending);
            st.note = Some("Not connected. Your text is safe here. Reconnect, then save again.".into());
            self.set_doc_state(origin, st);
            self.say("Not saved: the server is not connected. Your text is safe in the editor.");
            return;
        }
        let path = RemotePath::new(&origin.path);
        self.sent.borrow_mut().insert((origin.site.clone(), origin.path.clone()), bytes.clone());
        let r = self.ex.borrow_mut().save(&origin.site, &path, bytes);
        match r {
            Ok(_) => self.set_doc_state(origin, RemoteDocState::synced(origin.site_label.clone()).with(RemoteSaveState::Saving)),
            Err(e) => {
                let mut st = RemoteDocState::synced(origin.site_label.clone()).with(RemoteSaveState::Offline);
                st.note = Some(e.plain());
                self.set_doc_state(origin, st);
                self.say(format!("Not saved: {}. Your text is safe in the editor.", e.plain()));
            }
        }
    }

    fn origin_of(&self, key: &(String, String)) -> Option<RemoteOrigin> {
        self.ex.borrow().origin(&key.0, &RemotePath::new(&key.1))
    }

    /// A finished save job, on the UI thread.
    fn save_event(&self, ev: SaveEvent) {
        match ev {
            SaveEvent::Outcome { key, sent, outcome } => self.apply_outcome(&key, sent, &outcome),
            SaveEvent::Resolved { key, local, result } => {
                let Some(origin) = self.origin_of(&key) else { return };
                match result {
                    Err(msg) => self.say(format!("Could not carry that out ({msg}). Nothing was changed on the server.")),
                    Ok(ResolvedView::Saved(o)) => self.apply_outcome(&key, local, &o),
                    Ok(ResolvedView::SavedAs { path, outcome }) => {
                        let v = view_of(&outcome);
                        if v.saved {
                            self.say(format!("Your version was saved on the server as {path}. This tab still differs from {}.", origin.path));
                        } else {
                            self.say(v.note.unwrap_or_else(|| "Could not save your version under the new name.".into()));
                        }
                    }
                    Ok(ResolvedView::Reloaded(bytes)) => {
                        let host = self.host.borrow().clone();
                        if let Some(h) = host {
                            (h.replace_doc)(origin.clone(), bytes, false);
                        }
                        self.set_doc_state(&origin, RemoteDocState::synced(origin.site_label.clone()));
                        self.say(format!("Reloaded {} from the server. Your edits were kept for 7 days in the local journal.", origin.path));
                    }
                    Ok(ResolvedView::Cancelled) => {
                        let mut st = RemoteDocState::synced(origin.site_label.clone()).with(RemoteSaveState::Conflict);
                        st.note = Some("Not saved. The file on the server is different from the one you opened.".into());
                        self.set_doc_state(&origin, st);
                    }
                    Ok(ResolvedView::Rebased(_)) | Ok(ResolvedView::Diff { .. }) => {}
                }
            }
            SaveEvent::Recovery { site, cleaned, repaired, failed } => {
                if cleaned + repaired.len() + failed.len() > 0 {
                    self.say(format!(
                        "{}: checked the files left by an interrupted save ({} already complete, {} repaired{}).",
                        self.site_name(&site),
                        cleaned,
                        repaired.len(),
                        if failed.is_empty() { String::new() } else { format!(", {} could not be checked", failed.len()) }
                    ));
                }
            }
        }
    }

    fn apply_outcome(&self, key: &(String, String), sent: Vec<u8>, outcome: &brouillon_editor_engine::remote::SaveOutcome) {
        let Some(origin) = self.origin_of(key) else { return };
        let v = view_of(outcome);
        dbg(|| format!("save-outcome {} {}", origin.path, format!("{outcome:?}").chars().take(60).collect::<String>()));
        let mut st = RemoteDocState::synced(origin.site_label.clone()).with(v.state);
        st.note = v.note.clone();
        self.set_doc_state(&origin, st);
        if v.saved {
            if let Some(dir) = RemotePath::new(&origin.path).parent() {
                self.ex.borrow_mut().cache.invalidate(&origin.site, &dir);
            }
            // Only now, after the verified upload, may the host clear the modified marker.
            if let Some(h) = self.host.borrow().clone() {
                (h.saved)(origin.clone(), sent);
            }
            self.say(v.note.unwrap_or_else(|| format!("Saved {} to {}", origin.path, origin.site_label)));
            return;
        }
        if let Some(kind) = v.conflict {
            self.dlg.show_conflict(ConflictState { origin: origin.clone(), kind, local: Rc::new(sent) });
        }
        self.say(v.note.unwrap_or_else(|| "Not saved. Your text is safe in the editor.".into()));
    }

    /// Pick of the conflict window. The buffer stays as it is on every path except Reload.
    pub fn resolve_conflict(&self, choice: ConflictChoice) {
        let Some(c) = self.dlg.conflict.get_untracked() else { return };
        dbg(|| format!("resolve-conflict {choice:?}"));
        self.dlg.conflict_open.set(false);
        self.dlg.conflict.set(None);
        let key = (c.origin.site.clone(), c.origin.path.clone());
        let saves = self.saves.borrow().clone();
        let provider = self.ex.borrow().hub.provider(&key.0);
        let (Some(saves), Some(p)) = (saves, provider) else {
            self.say("Not connected: nothing was changed. Reconnect and save again.");
            return;
        };
        if matches!(choice, ConflictChoice::Cancel) {
            let mut st = RemoteDocState::synced(c.origin.site_label.clone()).with(RemoteSaveState::Conflict);
            st.note = Some("Not saved. The file on the server is different from the one you opened.".into());
            self.set_doc_state(&c.origin, st);
            return;
        }
        if saves.resolve(&key, (*c.local).clone(), choice, p) == Submit::Busy {
            self.say("Still working on this file. Try again in a moment.");
        } else {
            self.set_doc_state(&c.origin, RemoteDocState::synced(c.origin.site_label.clone()).with(RemoteSaveState::Saving));
        }
    }

    /// The user answered (or cancelled) the question on screen. The text goes to the helper once and is dropped.
    pub fn answer_ask(&self, answer: Option<String>) {
        let Some(v) = self.dlg.ask.get_untracked() else { return };
        self.dlg.hide_ask();
        if let Some(b) = self.prompts.borrow().as_ref() {
            b.answer(v.id, answer);
        }
        // The next queued question, if any.
        self.pump();
    }

    /// Escape on top of everything: cancel what is asked, close the rest.
    pub fn cancel_dialogs(&self) {
        dbg(|| "cancel-dialogs (Escape)".to_string());
        self.answer_ask(None);
        self.dlg.conflict_open.set(false);
        self.dlg.changed_open.set(false);
        self.dlg.recover_open.set(false);
        self.dlg.keep_open.set(false);
        self.dlg.creds_open.set(false);
    }

    /// "Replace and connect" of the changed-key stop: only acts after the site name was typed.
    pub fn replace_changed_key(&self) {
        let Some(k) = self.dlg.changed_key.get_untracked() else { return };
        if !crate::remote_dialogs::name_matches(&self.dlg.changed_typed.get_untracked(), &k.site_name) {
            return;
        }
        self.dlg.changed_open.set(false);
        match brouillon_editor_engine::remote::forget_host_key(&k.host, k.port) {
            Ok(()) => {
                self.say(format!("The old identity of {} was removed. Connecting again: read the new fingerprint carefully.", k.host));
                self.ex.borrow_mut().reconnect(&k.site_id);
                self.sync();
            }
            Err(e) => self.say(format!("Could not remove the old identity: {e}")),
        }
    }

    // ---------------------------------------------------------------- saved passwords

    /// The connect worked: if the user typed a password for it, ask (once per site) whether to keep it.
    fn after_connect(&self, site: &str) {
        let Some(c) = self.credentials() else { return };
        let Some(offer) = c.connected(site) else { return };
        let profile = self.saved_sites.with_untracked(|v| v.iter().find(|p| p.id == site).cloned());
        // Only a site in the saved list can hold a saved password; a quick connect stays "this time only".
        let Some(p) = profile.filter(|p| offer.replace || !p.keep_asked) else {
            c.discard(site);
            return;
        };
        self.dlg.show_keep(KeepAsk { site_id: p.id.clone(), site_name: p.name.clone(), who: format!("{}@{}", p.user, p.host), offer, unencrypted: p.is_unencrypted() });
    }

    /// The "Keep this password?" card was answered. `false` (also Esc and the close button) = use it this time only.
    pub fn resolve_keep(&self, save: bool) {
        let Some(k) = self.dlg.keep.get_untracked() else { return };
        self.dlg.hide_keep();
        let Some(c) = self.credentials() else { return };
        if !save {
            c.discard(&k.site_id);
            self.edit_saved(&k.site_id, |p| p.keep_asked = true);
            return;
        }
        match c.keep(&k.site_id, &k.site_name) {
            Ok(id) => {
                let r = SecretRef(Some(format!("{:032x}", id.as_u128())));
                self.edit_saved(&k.site_id, |p| {
                    p.secret_ref = r;
                    p.keep_asked = true;
                });
                self.say(format!("Password for {} saved on this computer. Forget it in Sites.", k.site_name));
            }
            Err(e) => {
                self.edit_saved(&k.site_id, |p| p.keep_asked = true);
                let why = if e.means_ask_every_time() { c.unavailable_note().unwrap_or("Brouillon will ask each time and keep nothing.") } else { "It could not be saved. Brouillon will ask each time." };
                self.say(why);
            }
        }
    }

    /// "Forget this password" in the site manager.
    pub fn forget_password(&self, id: &str) {
        let Some(c) = self.credentials() else { return };
        match c.forget(id) {
            Ok(_) => {
                self.edit_saved(id, |p| {
                    p.secret_ref = SecretRef::default();
                    p.keep_asked = false;
                });
                self.say(format!("Forgot the saved password for {}. You will be asked next time.", self.site_name(id)));
            }
            Err(_) => self.say("Brouillon could not open the saved passwords. Nothing was changed."),
        }
    }

    /// Palette: `Credentials: Show`. Metadata only; the vault opens now (lazy unlock), not at startup.
    pub fn open_credentials(&self) {
        self.refresh_credentials();
        self.dlg.creds_confirm.set(None);
        self.dlg.creds_open.set(true);
    }

    fn refresh_credentials(&self) {
        let Some(c) = self.credentials() else {
            self.dlg.creds_note.set("Saved passwords are not available here.".into());
            self.dlg.creds_items.set(Vec::new());
            return;
        };
        match c.entries() {
            Ok(items) => {
                self.dlg.creds_note.set(c.unavailable_note().map(String::from).unwrap_or_default());
                self.dlg.creds_items.set(items.into_iter().map(|e| CredRow::of(&e, |id| self.site_name(id))).collect());
            }
            Err(_) => {
                self.dlg.creds_note.set("Brouillon cannot open the saved passwords on this computer or account. They were probably created somewhere else.".into());
                self.dlg.creds_items.set(Vec::new());
            }
        }
    }

    /// "Replace": forget the value now; the next connect asks for the new one and offers to keep it.
    pub fn replace_credential(&self, id_hex: &str) {
        self.remove_credential(id_hex, "Forgot it. Connect again to enter the new password and keep it.");
    }

    pub fn delete_credential(&self, id_hex: &str) {
        self.dlg.creds_confirm.set(None);
        self.remove_credential(id_hex, "Deleted. The server and your files are not touched.");
    }

    fn remove_credential(&self, id_hex: &str, done: &str) {
        let Some(c) = self.credentials() else { return };
        let Ok(raw) = u128::from_str_radix(id_hex, 16) else { return };
        match c.delete_entry(brouillon_editor_engine::remote::SecretId::from_u128(raw)) {
            Ok(site) => {
                if let Some(site) = site.filter(|s| !c.is_saved(s)) {
                    self.edit_saved(&site, |p| {
                        p.secret_ref = SecretRef::default();
                        p.keep_asked = false;
                    });
                }
                self.say(done);
            }
            Err(_) => self.say("Brouillon could not open the saved passwords. Nothing was changed."),
        }
        self.refresh_credentials();
    }

    /// Panic / kill switch: lock the vault and drop every typed-but-unproven password.
    pub fn lock_credentials(&self) {
        if let Some(c) = self.credentials() {
            c.lock();
        }
    }

    // ---------------------------------------------------------------- recovery of unsaved changes

    pub fn open_recover(&self) {
        let items = self.saves.borrow().as_ref().map(|s| s.recoverable()).unwrap_or_default();
        self.dlg.recover_items.set(items);
        self.dlg.recover_open.set(true);
    }

    pub fn recover_open_item(&self, site: &str, id: &str) {
        let saves = self.saves.borrow().clone();
        let provider = self.ex.borrow().hub.provider(site);
        let Some(saves) = saves else { return };
        if provider.is_none() {
            self.say(format!("Connect to {} first (Remote: Connect), then open the recovered text again.", self.site_name(site)));
            return;
        }
        let Some((path, bytes)) = saves.adopt(site, id) else {
            self.say("That recovered text could not be read.");
            return;
        };
        let origin = self.ex.borrow().origin(site, &RemotePath::new(&path));
        let Some(origin) = origin else { return };
        if let Some(h) = self.host.borrow().clone() {
            (h.replace_doc)(origin.clone(), bytes, true);
        }
        let mut st = RemoteDocState::synced(origin.site_label.clone()).with(RemoteSaveState::Pending);
        st.note = Some("Recovered unsaved changes. Save to upload them; if the server file changed meanwhile you will be asked first.".into());
        self.set_doc_state(&origin, st);
        self.dlg.recover_open.set(false);
        self.say(format!("Opened the recovered text of {path}. Nothing was uploaded."));
    }

    pub fn recover_discard_item(&self, site: &str, id: &str) {
        if let Some(s) = self.saves.borrow().as_ref() {
            s.discard(site, id);
        }
        let items = self.saves.borrow().as_ref().map(|s| s.recoverable()).unwrap_or_default();
        self.dlg.recover_items.set(items);
        self.say("Put aside. The text stays in the local journal for 7 days.");
    }

    pub fn site_display_name(&self, id: &str) -> String {
        self.site_name(id)
    }

    // ---------------------------------------------------------------- connect form

    fn reset_form(&self) {
        self.f_address.set(String::new());
        self.f_folder.set(String::new());
        self.f_auth.set(0);
        self.f_error.set(String::new());
    }

    fn submit_form(&self) {
        let auth = match self.f_auth.get_untracked() {
            0 => AuthMethod::Agent,
            1 => AuthMethod::KeyFile { path: "~/.ssh/id_ed25519".into() },
            _ => AuthMethod::Password,
        };
        dbg(|| format!("connect-form address=\"{}\" folder=\"{}\" auth={}", self.f_address.get_untracked(), self.f_folder.get_untracked(), self.f_auth.get_untracked()));
        match SiteProfile::from_quick(&self.f_address.get_untracked(), auth, &self.f_folder.get_untracked()) {
            Some(p) => {
                self.f_error.set(String::new());
                self.connect_open.set(false);
                self.connect_profile(p, self.f_remember.get_untracked());
            }
            None => self.f_error.set("Type the address as user@host (for example deploy@vps.example.org).".into()),
        }
    }
}

// ====================================================================== views

pub(crate) fn small_button(text: &'static str, selected: bool, on: impl Fn() + 'static) -> impl IntoView {
    label(move || text.to_string()).on_click_stop(move |_| on()).style(move |s| button_style(s, selected))
}

pub(crate) fn row_view(row: RemoteRow, ui: RemoteUi) -> impl IntoView {
    let depth = row.depth;
    let kind = row.kind.clone();
    let is_dir = row.is_dir;
    let path = row.path.clone();
    let is_entry = row.is_entry();
    let glyph = match (&kind, is_dir, row.expanded, row.is_symlink) {
        (RemoteRowKind::Entry, true, true, false) => "\u{25BE}",
        (RemoteRowKind::Entry, true, false, false) => "\u{25B8}",
        (RemoteRowKind::Entry, false, _, _) => brouillon_editor_engine::browser::kind_glyph(&row.name, false, row.is_symlink),
        _ => " ",
    };
    let mut text = match &kind {
        RemoteRowKind::Entry => row.name.clone(),
        _ => row.name.clone(),
    };
    if row.is_symlink {
        text.push_str(" ->");
    }
    if row.locked && is_entry {
        text.push_str("  (read-only)");
    }
    let size = if is_entry && !is_dir { human_size(row.size) } else { String::new() };
    let (u_click, u_dbl, u_menu) = (ui.clone(), ui.clone(), ui.clone());
    let (r_click, r_menu) = (row.clone(), row.clone());
    let dbl_path = row.path.clone();
    let sel_path = path.clone();
    let kind_style = kind.clone();
    let view = stack((label(move || glyph.to_string()).style(|s| s.width(16.0).font_size(10.0)), label(move || text.clone()).style(|s| s.flex_grow(1.0).min_width(0.0).text_ellipsis()), label(move || size.clone()).style(|s| s.font_size(10.0).padding_right(6.0))))
        .on_click_stop(move |_| u_click.click(&r_click))
        .on_event_stop(EventListener::DoubleClick, move |_| {
            if is_entry && !is_dir {
                u_dbl.open(&dbl_path);
            } else if is_entry {
                u_dbl.enter(&dbl_path);
            }
        });
    let view = if is_entry {
        view.on_secondary_click_stop(move |_| {
            u_menu.click_select(&r_menu);
            floem::action::show_context_menu(u_menu.row_menu(r_menu.clone()), None);
        })
        .into_any()
    } else {
        view.into_any()
    };
    view.style(move |s| {
        let t = active_theme();
        let selected = is_entry && ui.selected.get().as_ref() == Some(&sel_path);
        let color = match &kind_style {
            RemoteRowKind::Entry if is_dir => t.fg,
            RemoteRowKind::Entry => t.fg_dim,
            RemoteRowKind::Error(_) => t.error,
            _ => t.fg_dim,
        };
        let base = s
            .flex_row()
            .items_center()
            .width_full()
            .height(ROW_H)
            .padding_left(8.0 + depth as f64 * 12.0)
            .font_size(12.0)
            .color(color)
            .cursor(CursorStyle::Pointer)
            .hover(move |h| h.background(t.fg.multiply_alpha(0.06)));
        if selected {
            base.background(t.accent.multiply_alpha(0.18)).color(t.fg)
        } else {
            base
        }
    })
}

impl RemoteUi {
    /// Right-click selects without toggling.
    fn click_select(&self, row: &RemoteRow) {
        self.kbd_event(crate::kbd_focus::Ev::RowPointer);
        self.ex.borrow_mut().select(&row.path);
        self.sync();
    }
}

pub fn human_size(n: u64) -> String {
    const K: f64 = 1024.0;
    let f = n as f64;
    if f < K {
        format!("{n} B")
    } else if f < K * K {
        format!("{:.1} KB", f / K)
    } else if f < K * K * K {
        format!("{:.1} MB", f / (K * K))
    } else {
        format!("{:.1} GB", f / (K * K * K))
    }
}

pub(crate) fn row_key(r: &RemoteRow) -> (String, String, bool, u8, usize) {
    let tag = match r.kind {
        RemoteRowKind::Entry => 0,
        RemoteRowKind::Loading => 1,
        RemoteRowKind::Error(_) => 2,
        RemoteRowKind::Empty => 3,
        RemoteRowKind::More(_) => 4,
    };
    (r.path.to_string(), r.name.clone(), r.expanded, tag, r.depth)
}

pub(crate) fn prompt_bar(ui: RemoteUi) -> impl IntoView {
    let u = ui.clone();
    dyn_container(
        move || u.prompt.get(),
        move |p| {
            let Some(p) = p else { return empty().into_any() };
            let (u_ok, u_cancel, u_key) = (ui.clone(), ui.clone(), ui.clone());
            let is_delete = matches!(p, Prompt::Delete { .. });
            let caption = match &p {
                Prompt::NewFile(d) => format!("New file in {d}"),
                Prompt::NewFolder(d) => format!("New folder in {d}"),
                Prompt::Rename(path) => format!("Rename {}", path.file_name().unwrap_or("")),
                Prompt::Delete { path, is_dir } => {
                    let prod = ui.ex.borrow().active_site().is_some_and(|s| s.profile.production);
                    let who = if ui.active_is_local() { "Your computer has" } else { "The server has" };
                    format!("Delete {} {}? {who} no recycle bin here, this cannot be undone.{}", if *is_dir { "folder" } else { "file" }, path.file_name().unwrap_or(""), if prod { " This is a PRODUCTION site." } else { "" })
                }
            };
            let input: AnyView = if is_delete {
                empty().into_any()
            } else {
                text_input(ui.prompt_text)
                    .request_focus(|| {})
                    .on_event_stop(EventListener::KeyDown, move |e| {
                        if let Event::KeyDown(ke) = e {
                            match ke.key.logical_key {
                                Key::Named(NamedKey::Enter) => u_key.submit_prompt(),
                                Key::Named(NamedKey::Escape) => u_key.prompt.set(None),
                                _ => {}
                            }
                        }
                    })
                    .style(|s| field_style(s).width_full())
                    .into_any()
            };
            let ok_text = if is_delete { " Delete " } else { " OK " };
            stack((
                label(move || caption.clone()).style(|s| s.font_size(11.0).width_full()),
                input,
                stack((small_button(ok_text, false, move || u_ok.submit_prompt()), small_button(" Cancel ", false, move || u_cancel.prompt.set(None)))).style(|s| s.flex_row().gap(6.0)),
            ))
            .style(|s| {
                let t = active_theme();
                s.flex_col().gap(4.0).padding(6.0).width_full().border_top(1.0).border_color(t.border).color(t.fg)
            })
            .into_any()
        },
    )
}

/// Connect modal: address, sign-in method, folder. Three fields, nothing secret.
pub fn connect_modal(ui: RemoteUi) -> impl IntoView {
    let visible = ui.connect_open;
    modal(visible, "Connect to a server", 420.0, 330.0, move || {
        ui.reset_form();
        let u = ui.clone();
        let addr_ui = ui.clone();
        let field = |caption: &'static str| label(move || caption.to_string()).style(|s| s.font_size(11.0).color(active_theme().fg_dim));
        let ring = FocusRing::new();
        let esc: Rc<dyn Fn()> = {
            let v = ui.connect_open;
            Rc::new(move || v.set(false))
        };
        let submit: Rc<dyn Fn()> = {
            let u = addr_ui.clone();
            Rc::new(move || u.submit_form())
        };
        // Visual order = Tab order: Address, Folder, Cancel, Connect.
        let address = ring
            .control(text_input(ui.f_address).placeholder("deploy@vps.example.org").request_focus(|| {}), Some(submit.clone()), None, esc.clone())
            .style(|s| field_style(s).width_full());
        let auth = {
            let (a0, a1, a2) = (ui.clone(), ui.clone(), ui.clone());
            let sel = ui.f_auth;
            stack((
                label(|| " SSH agent ".to_string()).on_click_stop(move |_| a0.f_auth.set(0)).style(move |s| button_style(s, sel.get() == 0)),
                label(|| " Key file ".to_string()).on_click_stop(move |_| a1.f_auth.set(1)).style(move |s| button_style(s, sel.get() == 1)),
                label(|| " Password ".to_string()).on_click_stop(move |_| a2.f_auth.set(2)).style(move |s| button_style(s, sel.get() == 2)),
            ))
            .style(|s| s.flex_row().gap(6.0))
        };
        let auth_note = {
            let sel = ui.f_auth;
            label(move || match sel.get() {
                0 => "Uses the keys loaded in your SSH agent. Brouillon handles no secret.".to_string(),
                1 => "Uses ~/.ssh/id_ed25519. Only the path is remembered, never the passphrase.".to_string(),
                _ => "The password is asked by the secure helper at connect time. It is never typed here nor saved.".to_string(),
            })
            .style(|s| s.font_size(10.0).color(active_theme().fg_dim).width_full())
        };
        let folder = ring
            .control(text_input(ui.f_folder).placeholder("/home/deploy  (optional)"), Some(submit.clone()), None, esc.clone())
            .style(|s| field_style(s).width_full());
        let remember = {
            let r = ui.f_remember;
            label(move || if r.get() { "[x] Remember this site (no password is saved)".to_string() } else { "[ ] Remember this site".to_string() })
                .on_click_stop(move |_| r.update(|v| *v = !*v))
                .style(|s| s.font_size(11.0).cursor(CursorStyle::Pointer))
        };
        let err = {
            let e = ui.f_error;
            label(move || e.get()).style(|s| s.font_size(11.0).color(active_theme().error).width_full())
        };
        let buttons = {
            let (c, g) = (ui.clone(), ui.clone());
            stack((
                ring_button(&ring, " Cancel ", false, move || c.connect_open.set(false), esc.clone()),
                ring_button(&ring, " Connect ", true, move || g.submit_form(), esc.clone()),
            ))
            .style(|s| s.flex_row().gap(8.0).justify_end().width_full())
        };
        let _ = u;
        stack((field("Address"), address, field("Sign in"), auth, auth_note, field("Folder"), folder, remember, err, buttons)).style(|s| s.flex_col().gap(5.0).width_full()).into_any()
    })
}

/// Site manager: saved profiles with Connect / flags / Delete. Non-secret data only.
pub fn sites_modal(ui: RemoteUi) -> impl IntoView {
    let visible = ui.sites_open;
    modal(visible, "Sites", 720.0, 360.0, move || {
        let u = ui.clone();
        let list = dyn_stack(
            move || u.saved_sites.get(),
            |s: &SiteProfile| (s.id.clone(), s.production, s.read_only, s.secret_ref.0.is_some()),
            {
                let ui = ui.clone();
                move |s: SiteProfile| {
                    let (u1, u2, u3, u4) = (ui.clone(), ui.clone(), ui.clone(), ui.clone());
                    let u5 = ui.clone();
                    let (i2, i3, i4, i5) = (s.id.clone(), s.id.clone(), s.id.clone(), s.id.clone());
                    let saved = s.secret_ref.0.is_some();
                    let p = s.clone();
                    let caption = format!("{}  {}:{}", s.name, s.url().replace("sftp://", ""), s.root_path);
                    stack((
                        label(move || caption.clone()).style(|s| s.flex_grow(1.0).min_width(0.0).text_ellipsis().font_size(11.0)),
                        label(|| " production ".to_string()).on_click_stop(move |_| u2.toggle_flag(&i2, true)).style({
                            let on = s.production;
                            move |st| button_style(st, on)
                        }),
                        label(|| " read-only ".to_string()).on_click_stop(move |_| u3.toggle_flag(&i3, false)).style({
                            let on = s.read_only;
                            move |st| button_style(st, on)
                        }),
                        label(move || if saved { "password saved".to_string() } else { String::new() }).style(|s| s.font_size(11.0).color(active_theme().fg_dim)),
                        label(move || if saved { " Forget this password ".to_string() } else { String::new() })
                            .on_click_stop(move |_| u5.forget_password(&i5))
                            .style(move |st| if saved { button_style(st, false) } else { st.display(floem::style::Display::None) }),
                        small_button(" Connect ", false, move || {
                            u1.sites_open.set(false);
                            u1.connect_profile(p.clone(), false);
                        }),
                        small_button(" Delete ", false, move || u4.remove_saved(&i4)),
                    ))
                    .style(|s| s.flex_row().items_center().gap(4.0).width_full().padding_vert(2.0))
                }
            },
        )
        .style(|s| s.flex_col().width_full());
        let empty_note = {
            let u = ui.clone();
            label(move || if u.saved_sites.with(|v| v.is_empty()) { "No saved site yet. Use Connect, and tick \"Remember this site\".".to_string() } else { String::new() })
                .style(|s| s.font_size(11.0).color(active_theme().fg_dim))
        };
        let new = {
            let u = ui.clone();
            small_button(" New... ", false, move || {
                u.sites_open.set(false);
                u.connect_open.set(true);
            })
        };
        stack((scroll(list).style(|s| s.width_full().height(240.0)), empty_note, new)).style(|s| s.flex_col().gap(6.0).width_full()).into_any()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sizes_read_like_a_file_manager() {
        assert_eq!(human_size(12), "12 B");
        assert_eq!(human_size(4300), "4.2 KB");
        assert_eq!(human_size(5 * 1024 * 1024), "5.0 MB");
    }

    #[test]
    fn row_keys_tell_placeholder_rows_from_the_folder_they_belong_to() {
        let mk = |kind| RemoteRow { kind, path: RemotePath::new("/a"), name: "a".into(), depth: 0, is_dir: true, expanded: false, is_symlink: false, locked: false, size: 0 };
        assert_ne!(row_key(&mk(RemoteRowKind::Entry)), row_key(&mk(RemoteRowKind::Loading)));
        let mut open = mk(RemoteRowKind::Entry);
        open.expanded = true;
        assert_ne!(row_key(&mk(RemoteRowKind::Entry)), row_key(&open));
    }
}
