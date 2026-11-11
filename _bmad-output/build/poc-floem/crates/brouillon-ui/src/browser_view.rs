//! The left sidebar: an "Open files" section and a "Browser" section in ONE column, with a draggable
//! divider, collapsible headers and a browser that reads Local (this computer), saved sites (SFTP now)
//! and, for the cloud drives, either the folder their desktop app already syncs or an honest
//! "Not connected, coming soon" entry.
//!
//! The listing machinery is the remote explorer's (`remote_view` / `Explorer` / `ListingCache`): the
//! local disk is one more source of it, listed on the hub's worker thread, with loading rows and a
//! "N more" cap. Nothing here starts a thread until the browser is shown (the scan for synced cloud
//! folders is one short-lived thread, and only then).
//!
//! Privacy: nothing in this file reaches the agent bridge. The browser is a view of the disk and the
//! sites; an agent only sees documents that are open in tabs, under the usual consent rules.

use std::path::Path;

use brouillon_editor_engine::browser::{detect_synced_system, drag_split, fold_crumbs, parse_typed_path, CloudKind, Place, Source, SortKey, SortMode};
use brouillon_editor_engine::remote::local::{display_path, from_native, to_native};
use brouillon_editor_engine::remote::{ConnState, RemoteOrigin, RemotePath, RemoteRow, LOCAL_SITE_ID};
use floem::event::{Event, EventListener};
use floem::ext_event::register_ext_trigger;
use floem::keyboard::{Key, NamedKey};
use floem::menu::{Menu, MenuItem};
use floem::reactive::{create_rw_signal, RwSignal, SignalGet, SignalUpdate, SignalWith};
use floem::style::{CursorStyle, Display};
use floem::views::{container, dyn_container, dyn_stack, empty, label, scroll, stack, text_input, virtual_stack, Decorators, VirtualDirection, VirtualItemSize};
use floem::{AnyView, IntoView};

use crate::doc_selector::{doc_selector_items, DocSelectorEntry};
use crate::remote_view::{dbg, prompt_bar, row_key, row_view, small_button, PaneStatus, RemoteUi, PANE_W, ROW_H};
use crate::theme_ctx::active_theme;
use crate::ui_kit::field_style;

/// Longest breadcrumb trail drawn before the head is folded into "...".
const MAX_CRUMBS: usize = 4;

fn home_remote() -> Option<RemotePath> {
    dirs::home_dir().map(|h| from_native(&h))
}

// ====================================================================== model glue

impl RemoteUi {
    pub fn browser_visible(&self) -> bool {
        self.prefs.with_untracked(|p| p.visible)
    }

    /// Write `browser.toml` (favorites, recents, split, collapsed flags, sort). Paths and ids only.
    pub fn save_prefs(&self) {
        let p = self.prefs.get_untracked();
        if let Err(e) = p.save(&self.prefs_path) {
            eprintln!("[browser] could not save the browser settings: {e}");
        }
    }

    pub fn update_prefs(&self, save: bool, f: impl FnOnce(&mut brouillon_editor_engine::browser::BrowserPrefs)) {
        self.prefs.update(f);
        if save {
            self.save_prefs();
        }
    }

    pub fn active_is_local(&self) -> bool {
        self.ex.borrow().active_site().is_some_and(|s| s.profile.id == LOCAL_SITE_ID)
    }

    pub fn set_browser_visible(&self, on: bool) {
        if on {
            self.ensure_started();
            // Showing the browser also opens its section: a collapsed header would look like nothing happened.
            self.update_prefs(true, |p| {
                p.visible = true;
                p.browser_collapsed = false;
            });
        } else {
            self.update_prefs(true, |p| p.visible = false);
            self.kbd_event(crate::kbd_focus::Ev::ToEditor);
        }
        self.sync();
    }

    pub fn toggle_browser(&self) {
        self.set_browser_visible(!self.browser_visible());
    }

    fn local_start(&self) -> RemotePath {
        let last = self.prefs.with_untracked(|p| p.last_local.clone());
        if !last.is_empty() {
            let rp = RemotePath::new(&last);
            if let Ok(Some(n)) = to_native(&rp, cfg!(windows)) {
                if n.is_dir() {
                    return rp;
                }
            }
        }
        home_remote().unwrap_or_else(RemotePath::root)
    }

    /// Create the Local source (no thread, no I/O: its first listing happens when it is drawn) and, once,
    /// look for the folders the cloud apps sync.
    pub fn ensure_started(&self) {
        let start = self.local_start();
        {
            let mut ex = self.ex.borrow_mut();
            let first = ex.site_index(LOCAL_SITE_ID).is_none();
            ex.add_local(start);
            if first && ex.active.is_none() {
                ex.activate(Some(LOCAL_SITE_ID));
            }
        }
        self.start_synced_scan();
    }

    fn start_synced_scan(&self) {
        if self.synced_started.replace(true) {
            return;
        }
        let (slot, trigger) = (self.synced_slot.clone(), self.trigger);
        let spawned = std::thread::Builder::new().name("brouillon-sync-scan".into()).spawn(move || {
            let found = detect_synced_system();
            if let Ok(mut g) = slot.lock() {
                *g = Some(found);
            }
            register_ext_trigger(trigger);
        });
        if spawned.is_err() {
            self.synced_started.set(false);
        }
    }

    fn source_key(&self) -> Option<String> {
        let ex = self.ex.borrow();
        ex.active_site().map(|s| if s.profile.id == LOCAL_SITE_ID { Source::Local.key() } else { Source::Site(s.profile.id.clone()).key() })
    }

    /// After any move: remember the folder (recent list, last local folder), refresh the signals.
    fn after_nav(&self) {
        self.filter.set(String::new());
        self.path_edit.set(false);
        let root = self.ex.borrow().current_root();
        if let (Some(src), Some(root)) = (self.source_key(), root) {
            let local = src == Source::Local.key();
            self.update_prefs(true, |p| {
                if local {
                    p.last_local = root.to_string();
                }
                if !root.is_root() {
                    p.push_recent(Place { source: src.clone(), path: root.to_string() });
                }
            });
            let (b, f) = {
                let ex = self.ex.borrow();
                (ex.can_back(), ex.can_forward())
            };
            dbg(|| format!("browser source={src} root={root} back={b} forward={f}"));
        }
        self.sync();
    }

    pub fn enter(&self, p: &RemotePath) {
        self.kbd_event(crate::kbd_focus::Ev::RowPointer);
        self.ex.borrow_mut().navigate(p.clone());
        self.after_nav();
    }

    pub fn go_up(&self) {
        let moved = self.ex.borrow_mut().up();
        if moved {
            self.after_nav();
        }
    }

    pub fn go_back(&self) {
        let moved = self.ex.borrow_mut().back();
        if moved {
            self.after_nav();
        }
    }

    pub fn go_forward(&self) {
        let moved = self.ex.borrow_mut().forward();
        if moved {
            self.after_nav();
        }
    }

    /// Home: the user's folder for Local, the site's start folder for a server.
    pub fn go_home(&self) {
        let target = {
            let ex = self.ex.borrow();
            match ex.active_site() {
                Some(s) if s.profile.id == LOCAL_SITE_ID => home_remote(),
                Some(s) => Some(RemotePath::new(&s.profile.root_path)),
                None => None,
            }
        };
        if let Some(t) = target {
            self.enter(&t);
        }
    }

    /// Show the Local source on `dir` (a synced cloud folder, a pinned folder, File > Open Folder).
    pub fn browse_local(&self, dir: &Path) {
        self.set_browser_visible(true);
        {
            let mut ex = self.ex.borrow_mut();
            ex.activate(Some(LOCAL_SITE_ID));
            ex.navigate(from_native(dir));
        }
        self.after_nav();
    }

    pub fn use_local(&self) {
        self.set_browser_visible(true);
        self.ex.borrow_mut().activate(Some(LOCAL_SITE_ID));
        self.after_nav();
    }

    /// A favorite or recent folder. A server that is not open is reconnected first (the folder is shown
    /// as soon as the connection is up).
    pub fn go_place(&self, place: &Place) {
        let path = RemotePath::new(&place.path);
        match Source::from_key(&place.source) {
            Some(Source::Local) => {
                self.set_browser_visible(true);
                {
                    let mut ex = self.ex.borrow_mut();
                    ex.activate(Some(LOCAL_SITE_ID));
                    ex.navigate(path);
                }
                self.after_nav();
            }
            Some(Source::Site(id)) => {
                let open = self.ex.borrow().site_index(&id).is_some();
                if !open {
                    let saved = self.saved_sites.with_untracked(|v| v.iter().find(|s| s.id == id).cloned());
                    match saved {
                        Some(p) => self.connect_profile(p, false),
                        None => {
                            self.message.set("That site is no longer saved.".into());
                            return;
                        }
                    }
                } else {
                    self.ex.borrow_mut().activate(Some(&id));
                }
                self.ex.borrow_mut().navigate(path);
                self.after_nav();
            }
            None => {}
        }
    }

    fn place_of(&self, path: &RemotePath) -> Option<Place> {
        self.source_key().map(|source| Place { source, path: path.to_string() })
    }

    pub fn toggle_pin_of(&self, path: &RemotePath) {
        if let Some(p) = self.place_of(path) {
            let now = std::cell::Cell::new(false);
            self.update_prefs(true, |prefs| now.set(prefs.toggle_pin(p)));
            self.say_pin(now.get(), path);
            self.sync();
        }
    }

    fn say_pin(&self, pinned: bool, path: &RemotePath) {
        self.message.set(format!("{} {}", if pinned { "Pinned" } else { "Unpinned" }, path));
    }

    pub fn toggle_pin_here(&self) {
        if let Some(root) = self.ex.borrow().current_root() {
            self.toggle_pin_of(&root);
        }
    }

    pub fn set_sort(&self, key: SortKey) {
        let mode: SortMode = self.prefs.with_untracked(|p| p.sort).pick(key);
        self.update_prefs(true, |p| p.sort = mode);
        self.ex.borrow_mut().set_sort(mode);
        self.sync();
    }

    // ------------------------------------------------------------------ path bar

    pub fn begin_path_edit(&self) {
        let text = {
            let ex = self.ex.borrow();
            match ex.active_site() {
                Some(s) if s.profile.id == LOCAL_SITE_ID => display_path(&s.tree.root, cfg!(windows)),
                Some(s) => s.tree.root.to_string(),
                None => return,
            }
        };
        self.path_text.set(text);
        self.path_edit.set(true);
        self.kbd_event(crate::kbd_focus::Ev::PathFocused);
    }

    pub fn submit_path(&self) {
        let text = self.path_text.get_untracked();
        let (cur, local) = {
            let ex = self.ex.borrow();
            (ex.current_root(), ex.active_site().is_some_and(|s| s.profile.id == LOCAL_SITE_ID))
        };
        let home = dirs::home_dir();
        match cur.and_then(|c| parse_typed_path(&text, &c, local, home.as_deref())) {
            Some(target) => {
                self.ex.borrow_mut().navigate(target);
                self.after_nav();
            }
            None => self.path_edit.set(false),
        }
    }

    // ------------------------------------------------------------------ reveal

    /// Palette "Browser: Reveal Active File": show the file of a local tab in the Local source.
    pub fn reveal_local(&self, file: &Path) {
        self.set_browser_visible(true);
        self.ex.borrow_mut().reveal(LOCAL_SITE_ID, &from_native(file));
        self.kbd_event(crate::kbd_focus::Ev::FocusRows);
        self.after_nav();
    }

    /// Same for a file opened from a server. `false` when that site is not open any more.
    pub fn reveal_origin(&self, origin: &RemoteOrigin) -> bool {
        let found = self.ex.borrow_mut().reveal(&origin.site, &RemotePath::new(&origin.path));
        if found {
            self.set_browser_visible(true);
            self.kbd_event(crate::kbd_focus::Ev::FocusRows);
            self.after_nav();
        }
        found
    }

    /// Type-to-filter: a printable key typed while the browser has the keyboard narrows the loaded rows.
    pub fn type_char(&self, c: &str) {
        self.filter.update(|f| f.push_str(c));
    }

    // ------------------------------------------------------------------ menus

    fn source_label(&self) -> String {
        self.strip.with(|tabs| tabs.iter().find(|t| t.active).map(|t| if t.id == LOCAL_SITE_ID { "Local".to_string() } else { format!("{} ({})", t.label, t.state.to_lowercase()) })).unwrap_or_else(|| "Local".into())
    }

    /// Menu of every place the browser can read: Local, sites, cloud drives. Native popup at the pointer.
    pub fn show_source_menu(&self) {
        let item = |title: String, enabled: bool, f: Box<dyn Fn()>| MenuItem::new(title).enabled(enabled).action(f);
        let mut m = Menu::new("");
        let u = self.clone();
        m = m.entry(item("Local (this computer)".into(), true, Box::new(move || u.use_local())));
        // Cloud drives: the folder the vendor's app already syncs, or an honest placeholder. No network, no login.
        m = m.separator();
        let synced = self.synced.get_untracked();
        for kind in CloudKind::ALL {
            let mine: Vec<_> = synced.iter().filter(|f| f.kind == kind).collect();
            if mine.is_empty() {
                m = m.entry(item(format!("{} - Not connected (coming soon)", kind.label()), false, Box::new(|| {})));
            }
            for f in mine {
                let (u, path) = (self.clone(), f.path.clone());
                m = m.entry(item(format!("{} - synced by {} ({})", kind.label(), kind.sync_app(), f.path.display()), true, Box::new(move || u.browse_local(&path))));
            }
        }
        // Sites: the open ones first, then the saved ones that are not open.
        m = m.separator();
        let open: Vec<(String, String, bool)> = {
            let ex = self.ex.borrow();
            ex.sites.iter().filter(|s| s.profile.id != LOCAL_SITE_ID).map(|s| (s.profile.id.clone(), s.profile.name.clone(), matches!(s.conn, ConnState::Connected))).collect()
        };
        for (id, name, up) in &open {
            let u = self.clone();
            let id2 = id.clone();
            m = m.entry(item(format!("{name} ({})", if *up { "connected" } else { "not connected" }), true, Box::new(move || {
                u.set_browser_visible(true);
                u.activate(Some(id2.clone()));
                u.after_nav();
            })));
        }
        for s in self.saved_sites.get_untracked() {
            if open.iter().any(|(id, _, _)| id == &s.id) {
                continue;
            }
            let u = self.clone();
            let p = s.clone();
            m = m.entry(item(format!("{} - connect", s.name), true, Box::new(move || {
                u.set_browser_visible(true);
                u.connect_profile(p.clone(), false);
            })));
        }
        let u = self.clone();
        m = m.entry(item("Add a site...".into(), true, Box::new(move || u.connect_open.set(true))));
        let u = self.clone();
        m = m.entry(item("Manage sites...".into(), true, Box::new(move || u.sites_open.set(true))));
        floem::action::show_context_menu(m, None);
    }

    fn place_caption(&self, p: &Place) -> String {
        match Source::from_key(&p.source) {
            Some(Source::Local) => display_path(&RemotePath::new(&p.path), cfg!(windows)),
            Some(Source::Site(id)) => format!("{}: {}", self.site_display_name(&id), p.path),
            None => p.path.clone(),
        }
    }

    /// Favorites, recent folders and the sort choice.
    pub fn show_places_menu(&self) {
        let item = |title: String, enabled: bool, f: Box<dyn Fn()>| MenuItem::new(title).enabled(enabled).action(f);
        let prefs = self.prefs.get_untracked();
        let mut m = Menu::new("");
        let u = self.clone();
        let pinned = self.nav.get_untracked().pinned;
        m = m.entry(item(if pinned { "Unpin this folder".into() } else { "Pin this folder".into() }, true, Box::new(move || u.toggle_pin_here())));
        m = m.separator();
        m = m.entry(item("Favorites".into(), false, Box::new(|| {})));
        if prefs.favorites.is_empty() {
            m = m.entry(item("  (none yet)".into(), false, Box::new(|| {})));
        }
        for f in &prefs.favorites {
            let (u, p) = (self.clone(), f.clone());
            m = m.entry(item(format!("  {}", self.place_caption(f)), true, Box::new(move || u.go_place(&p))));
        }
        m = m.separator();
        m = m.entry(item("Recent folders".into(), false, Box::new(|| {})));
        if prefs.recents.is_empty() {
            m = m.entry(item("  (none yet)".into(), false, Box::new(|| {})));
        }
        for r in prefs.recents.iter().take(8) {
            let (u, p) = (self.clone(), r.clone());
            m = m.entry(item(format!("  {}", self.place_caption(r)), true, Box::new(move || u.go_place(&p))));
        }
        m = m.separator();
        for key in [SortKey::Name, SortKey::Size, SortKey::Modified, SortKey::Kind] {
            let u = self.clone();
            let cur = prefs.sort.key == key;
            let arrow = if cur { if prefs.sort.descending { " (descending)" } else { " (ascending)" } } else { "" };
            m = m.entry(item(format!("{}Sort by {}{arrow}", if cur { "* " } else { "  " }, key.label()), true, Box::new(move || u.set_sort(key))));
        }
        floem::action::show_context_menu(m, None);
    }
}

// ====================================================================== views

fn nav_button(text: &'static str, enabled: impl Fn() -> bool + Clone + 'static, on: impl Fn() + 'static) -> impl IntoView {
    let en = enabled.clone();
    label(move || text.to_string()).on_click_stop(move |_| {
        if en() {
            on();
        }
    }).style(move |s| {
        let t = active_theme();
        let s = s.padding_horiz(6.0).padding_vert(2.0).font_size(11.0).border_radius(3.0).border(1.0).border_color(t.border);
        if enabled() {
            s.color(t.fg).cursor(CursorStyle::Pointer).hover(move |h| h.background(t.fg.multiply_alpha(0.08)))
        } else {
            s.color(t.fg_dim.multiply_alpha(0.45))
        }
    })
}

fn breadcrumb_bar(ui: RemoteUi) -> impl IntoView {
    let u = ui.clone();
    dyn_container(
        move || u.path_edit.get(),
        move |editing| {
            if editing {
                let (u_key, u_blur) = (ui.clone(), ui.clone());
                return text_input(ui.path_text)
                    .placeholder("Type a path and press Enter")
                    .request_focus(|| {})
                    .on_event_stop(EventListener::KeyDown, move |e| {
                        if let Event::KeyDown(ke) = e {
                            match ke.key.logical_key {
                                Key::Named(NamedKey::Enter) => u_key.submit_path(),
                                Key::Named(NamedKey::Escape) => {
                                u_key.path_edit.set(false);
                                u_key.kbd_event(crate::kbd_focus::Ev::Escape);
                            }
                                _ => {}
                            }
                        }
                    })
                    .on_event_cont(EventListener::FocusLost, move |_| {
                        u_blur.path_edit.set(false);
                        u_blur.kbd_event(crate::kbd_focus::Ev::PathBlurred);
                    })
                    .on_event_cont(EventListener::FocusGained, {
                        let u = ui.clone();
                        move |_| u.kbd_event(crate::kbd_focus::Ev::PathFocused)
                    })
                    .style(|s| field_style(s).width_full())
                    .into_any();
            }
            let u = ui.clone();
            dyn_stack(
                move || fold_crumbs(u.crumbs.get(), MAX_CRUMBS).into_iter().enumerate().collect::<Vec<_>>(),
                |(i, c): &(usize, brouillon_editor_engine::browser::Crumb)| (*i, c.label.clone(), c.path.clone()),
                {
                    let ui = ui.clone();
                    move |(i, c): (usize, brouillon_editor_engine::browser::Crumb)| {
                        let u = ui.clone();
                        let path = c.path.clone();
                        let text = if i == 0 { c.label.clone() } else { format!("> {}", c.label) };
                        label(move || text.clone()).on_click_stop(move |_| u.enter(&path)).style(|s| {
                            let t = active_theme();
                            s.font_size(11.0).color(t.fg_dim).padding_vert(1.0).padding_right(3.0).cursor(CursorStyle::Pointer).hover(move |h| h.color(t.accent))
                        })
                    }
                },
            )
            .style(|s| s.flex_row().flex_wrap(floem::taffy::FlexWrap::Wrap).items_center().width_full())
            .into_any()
        },
    )
    .style(|s| s.width_full())
}

/// Source switcher, path bar, navigation, filter, tree, prompt and status line.
fn browser_body(ui: RemoteUi) -> impl IntoView {
    let u = ui.clone();
    let source = label(move || format!("{}  v", u.source_label())).on_click_stop({
        let u = ui.clone();
        move |_| u.show_source_menu()
    }).style(|s| {
        let t = active_theme();
        s.flex_grow(1.0).min_width(0.0).text_ellipsis().padding_horiz(6.0).padding_vert(2.0).font_size(11.0).color(t.fg).border(1.0).border_color(t.border).border_radius(3.0).cursor(CursorStyle::Pointer).hover(move |h| h.background(t.fg.multiply_alpha(0.08)))
    });
    let u = ui.clone();
    let pin = label(move || if u.nav.get().pinned { "Pinned".to_string() } else { "Pin".to_string() }).on_click_stop({
        let u = ui.clone();
        move |_| u.toggle_pin_here()
    }).style({
        let u = ui.clone();
        move |s| {
            let t = active_theme();
            s.padding_horiz(6.0).padding_vert(2.0).font_size(11.0).cursor(CursorStyle::Pointer).color(if u.nav.get().pinned { t.accent } else { t.fg_dim }).hover(move |h| h.color(t.accent))
        }
    });
    let places = nav_button("Places", || true, {
        let u = ui.clone();
        move || u.show_places_menu()
    });
    let row_source = stack((source, pin, places)).style(|s| s.flex_row().items_center().gap(4.0).width_full());

    let (u1, u2, u3, u4, u5, u6) = (ui.clone(), ui.clone(), ui.clone(), ui.clone(), ui.clone(), ui.clone());
    let (e1, e2, e3) = (ui.clone(), ui.clone(), ui.clone());
    let row_nav = stack((
        nav_button(" < ", move || e1.nav.get().back, move || u1.go_back()),
        nav_button(" > ", move || e2.nav.get().forward, move || u2.go_forward()),
        nav_button(" ^ ", move || e3.nav.get().up, move || u3.go_up()),
        nav_button("Home", || true, move || u4.go_home()),
        nav_button("F5", || true, move || u5.refresh()),
        nav_button("Path", || true, move || u6.begin_path_edit()),
    ))
    .style(|s| s.flex_row().items_center().gap(3.0).width_full().flex_wrap(floem::taffy::FlexWrap::Wrap));

    let filter = text_input(ui.filter)
        .placeholder("Filter (type here, Esc clears)")
        .on_event_cont(EventListener::FocusGained, {
            let u = ui.clone();
            move |_| u.kbd_event(crate::kbd_focus::Ev::FilterFocused)
        })
        .on_event_cont(EventListener::FocusLost, {
            let u = ui.clone();
            move |_| u.kbd_event(crate::kbd_focus::Ev::FilterBlurred)
        })
        .on_event_stop(EventListener::KeyDown, {
            let u = ui.clone();
            move |e| {
                if let Event::KeyDown(ke) = e {
                    if ke.key.logical_key == Key::Named(NamedKey::Escape) {
                        // First Escape clears the text, the next one gives the keyboard back to the editor.
                        if u.filter.get_untracked().is_empty() {
                            u.kbd_event(crate::kbd_focus::Ev::Escape);
                        } else {
                            u.filter.set(String::new());
                        }
                    }
                }
            }
        })
        .style(|s| field_style(s).width_full());

    let controls = stack((row_source, row_nav, breadcrumb_bar(ui.clone()), filter)).style(|s| s.flex_col().gap(4.0).padding(6.0).width_full());

    let body = {
        let u = ui.clone();
        dyn_container(
            move || u.pane_status.get(),
            {
                let ui = ui.clone();
                move |st: PaneStatus| {
                    if !st.text.is_empty() {
                        let u = ui.clone();
                        let retry: AnyView = if st.can_retry { small_button(" Retry ", false, move || u.retry_active()).into_any() } else { empty().into_any() };
                        return stack((label(move || st.text.clone()).style(|s| s.width_full()), retry))
                            .style(|s| s.flex_col().gap(6.0).padding(10.0).font_size(12.0).color(active_theme().fg_dim))
                            .into_any();
                    }
                    let u = ui.clone();
                    scroll(
                        virtual_stack(
                            VirtualDirection::Vertical,
                            VirtualItemSize::Fixed(Box::new(|| ROW_H)),
                            move || u.rows.get(),
                            row_key,
                            {
                                let ui = ui.clone();
                                move |r: RemoteRow| row_view(r, ui.clone())
                            },
                        )
                        .style(|s| s.flex_col().width_full()),
                    )
                    .style(|s| s.width_full().flex_grow(1.0).min_height(0.0))
                    .into_any()
                }
            },
        )
        .style(|s| s.flex_col().width_full().flex_grow(1.0).min_height(0.0))
    };
    let msg = {
        let u = ui.clone();
        label(move || u.message.get()).style(|s| {
            let t = active_theme();
            s.font_size(10.0).color(t.fg_dim).padding_horiz(8.0).padding_vert(4.0).width_full().border_top(1.0).border_color(t.border)
        })
    };
    stack((controls, body, prompt_bar(ui.clone()), msg)).style(|s| s.flex_col().size_full().min_height(0.0))
}

fn section_header(title: impl Fn() -> String + 'static, collapsed: impl Fn() -> bool + 'static, on_toggle: impl Fn() + 'static) -> impl IntoView {
    label(move || format!("{} {}", if collapsed() { "\u{25B8}" } else { "\u{25BE}" }, title())).on_click_stop(move |_| on_toggle()).style(|s| {
        let t = active_theme();
        s.width_full().color(t.fg_dim).font_size(10.0).padding_horiz(8.0).padding_vert(6.0).cursor(CursorStyle::Pointer).hover(move |h| h.color(t.fg))
    })
}

/// Draggable bar between the two sections. The drag is relative to where the bar was grabbed, so the
/// bar follows the pointer while the sections re-flow under it.
fn divider(ui: RemoteUi) -> impl IntoView {
    let dragging = create_rw_signal(false);
    let grab = create_rw_signal(0.0_f64);
    let (u_move, u_up) = (ui.clone(), ui.clone());
    let u_style = ui.clone();
    empty()
        .on_event_stop(EventListener::PointerDown, move |e| {
            if let Event::PointerDown(pe) = e {
                dragging.set(true);
                grab.set(pe.pos.y);
            }
        })
        .on_event_stop(EventListener::PointerMove, move |e| {
            if let (Event::PointerMove(pe), true) = (e, dragging.get_untracked()) {
                let dy = pe.pos.y - grab.get_untracked();
                let total = u_move.split_h.get_untracked();
                u_move.update_prefs(false, |p| p.split = drag_split(p.split, dy, total));
            }
        })
        .on_event_stop(EventListener::PointerUp, move |_| {
            if dragging.get_untracked() {
                dragging.set(false);
                u_up.save_prefs();
            }
        })
        .style(move |s| {
            let t = active_theme();
            let p = u_style.prefs.get();
            // Only between two open sections.
            if p.visible && !p.browser_collapsed && !p.open_files_collapsed {
                s.width_full().height(5.0).background(t.border).cursor(CursorStyle::RowResize).hover(move |h| h.background(t.accent.multiply_alpha(0.5)))
            } else {
                s.display(Display::None)
            }
        })
}

/// The whole left column. `show_open_files` is the Moldu-preset gate of today's Open Files list.
pub fn sidebar(
    ui: RemoteUi,
    entries: RwSignal<Vec<DocSelectorEntry>>,
    theme_idx: RwSignal<usize>,
    on_switch: impl Fn(usize) + 'static + Clone,
    show_open_files: impl Fn() -> bool + 'static + Clone,
) -> impl IntoView {
    let (sh1, sh2, sh3) = (show_open_files.clone(), show_open_files.clone(), show_open_files);
    // --- Open files
    let (u_h, u_c) = (ui.clone(), ui.clone());
    let open_header = section_header(
        move || format!("OPEN FILES ({})", entries.with(|v| v.len())),
        move || u_h.prefs.get().open_files_collapsed,
        move || u_c.update_prefs(true, |p| p.open_files_collapsed = !p.open_files_collapsed),
    );
    let u = ui.clone();
    let open_body = scroll(doc_selector_items(entries, theme_idx, on_switch)).style(move |s| {
        if u.prefs.get().open_files_collapsed {
            s.display(Display::None)
        } else {
            s.width_full().flex_grow(1.0).min_height(0.0)
        }
    });
    let (u_s, sh) = (ui.clone(), sh1);
    let open_section = stack((open_header, open_body)).style(move |s| {
        let p = u_s.prefs.get();
        if !sh() {
            return s.display(Display::None);
        }
        let s = s.flex_col().width_full().min_height(0.0);
        let browser_open = p.visible && !p.browser_collapsed;
        if p.open_files_collapsed {
            s
        } else if browser_open {
            s.flex_grow(p.split).flex_basis(0.0)
        } else {
            s.flex_grow(1.0).flex_basis(0.0)
        }
    });

    // --- Browser
    let (u_h, u_c) = (ui.clone(), ui.clone());
    let browser_header = section_header(
        || "BROWSER".to_string(),
        move || u_h.prefs.get().browser_collapsed,
        move || u_c.update_prefs(true, |p| p.browser_collapsed = !p.browser_collapsed),
    );
    let u = ui.clone();
    // Built only when first shown, so a closed browser costs no widgets and no listing.
    let u_gate = ui.clone();
    let body = dyn_container(
        move || u_gate.prefs.get().visible && !u_gate.prefs.get().browser_collapsed,
        {
            let ui = ui.clone();
            move |open| if open { browser_body(ui.clone()).into_any() } else { empty().into_any() }
        },
    )
    .style(move |s| if u.prefs.get().browser_collapsed { s.display(Display::None) } else { s.width_full().flex_grow(1.0).min_height(0.0) });
    let (u_s, sh) = (ui.clone(), sh2);
    let browser_section = stack((browser_header, body)).style(move |s| {
        let p = u_s.prefs.get();
        if !p.visible {
            return s.display(Display::None);
        }
        let s = s.flex_col().width_full().min_height(0.0);
        if p.browser_collapsed {
            s
        } else if sh() && !p.open_files_collapsed {
            s.flex_grow(1.0 - p.split).flex_basis(0.0)
        } else {
            s.flex_grow(1.0).flex_basis(0.0)
        }
    });

    let u_size = ui.clone();
    let u_any = ui.clone();
    container(stack((open_section, divider(ui.clone()), browser_section)).style(|s| s.flex_col().size_full().min_height(0.0)))
        .on_resize(move |r| u_size.split_h.set(r.height()))
        .style(move |s| {
            let t = active_theme();
            let s = s.width(PANE_W).height_full().background(t.bg_chrome).border_right(1.0).border_color(t.border).color(t.fg);
            // No browser and no Open Files list (Geek preset): the column is gone, as before.
            if u_any.prefs.get().visible || sh3() {
                s
            } else {
                s.display(Display::None)
            }
        })
}
