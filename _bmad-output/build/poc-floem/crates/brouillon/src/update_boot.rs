//! Automatic update: glue between `brouillon-update` and the UI.
//!
//! One call from `app_view` (`init`) and one from the exit path (`on_exit`).
//! Design: internal design notes (not published).
//! Nothing runs before the window exists; the whole network pass is a background thread that sleeps
//! `START_DELAY` first. The UI thread only reads two small local files.
//!
//! What the request carries: a plain GET of `latest.json` and `latest.json.sig`, then of the installer.
//! No query string, no cookie, no identifier, generic `User-Agent: Brouillon-update/1` (a protocol
//! revision, not the program version, and no OS detail).

use crate::paths::Paths;
use crate::settings::{Layer, Settings};
use brouillon_update::flow::{run_once, Ctx, Outcome};
use brouillon_update::install::{is_installed_copy, is_portable, launch_installer, verify_installer, watch_early, EarlyState};
use brouillon_update::keys::{self, TrustedKey};
use brouillon_update::manifest::Manifest;
use brouillon_update::msg::{detect_lang, render, Lang, Msg};
use brouillon_update::net::{DirTransport, Transport, UreqTransport};
use brouillon_update::policy::{decide, Action};
use brouillon_update::state::{AttemptOutcome, UpdateState};
use brouillon_update::timeutil::now_epoch;
use brouillon_update::{UpdateError, DEFAULT_MANIFEST_URL};
use floem::action::exec_after;
use floem::ext_event::create_signal_from_channel;
use floem::reactive::{create_effect, create_rw_signal, RwSignal, SignalGet, SignalUpdate};
use std::cell::{Cell, RefCell};
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Mutex;
use std::time::Duration;

/// Delay before the network pass: the window is long drawn and startup budgets are untouched.
const START_DELAY: Duration = Duration::from_millis(1500);
/// Grace period between "Ready" and the automatic start (explicit buttons stay available).
const INSTALL_NOW_DELAY: Duration = Duration::from_secs(3);
/// After starting the installer we stay alive this long: an installer that dies at once failed.
const EARLY_WINDOW: Duration = Duration::from_secs(3);
/// Countdown of the mandatory update banner.
const MANDATORY_SECS: u32 = 30;
const DOWNLOAD_PAGE: &str = "https://brouillon.rochetta.fr/download/";

struct Pending {
    installer: PathBuf,
    manifest: Manifest,
    state_path: PathBuf,
}

/// Verified installer waiting for the normal exit (or the user's choice).
static PENDING: Mutex<Option<Pending>> = Mutex::new(None);

#[derive(Clone)]
pub enum Ev {
    /// A newer build exists and its installer starts downloading.
    Found(Manifest),
    Progress(u32),
    /// Newer build, nothing downloaded (portable, `auto_install = false`, ...).
    Notify(Manifest),
    /// An automatic install of this build already failed on this machine: manual route, no retry.
    KnownFailure(Manifest),
    Staged { manifest: Manifest, installer: PathBuf, mandatory: bool },
    Failed(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BannerKind {
    Downloading,
    /// Verified installer ready: Update now / Later / Skip this version.
    Ready,
    /// Mandatory (min_supported_build): no buttons.
    Required,
    /// Plain message. `skippable`: offer "Skip this version".
    Info { skippable: bool },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UpdateBanner {
    pub kind: BannerKind,
    pub text: String,
}

fn env_is(name: &str, v: &str) -> bool {
    std::env::var(name).map_or(false, |x| x == v)
}

/// Gate 1: the update check runs only when the user has not turned it off (`updates.check`, default ON; off in the
/// first-run screen or Preferences) and `BROUILLON_NO_UPDATE` is not set.
pub fn enabled(settings: &Settings, no_update_env: bool) -> bool {
    settings.get_bool("updates.check", true) && !no_update_env
}

/// Test hooks exist only in a debug build or one built with `--features test-input`: a shipped
/// release exe can never be redirected to another host, key or folder.
pub fn dev_allowed() -> bool {
    cfg!(any(debug_assertions, feature = "test-input"))
}

#[derive(Default, Clone)]
pub struct DevOverride {
    /// `BROUILLON_UPDATE_DEV_DIR`: serve the manifest/installer from a folder (no socket).
    pub dir: Option<PathBuf>,
    /// `BROUILLON_UPDATE_URL`: manifest URL (loopback HTTP allowed in dev).
    pub url: Option<String>,
    /// `BROUILLON_UPDATE_DEV_PUBKEY`: extra trusted key (hex).
    pub pubkey: Option<TrustedKey>,
    /// `BROUILLON_UPDATE_FORCE_INSTALLED=1`: pretend to be an installed copy.
    pub force_installed: bool,
}

impl DevOverride {
    pub fn active(&self) -> bool {
        self.dir.is_some() || self.url.is_some()
    }
}

/// Every override is IGNORED when `allowed` is false.
pub fn dev_override(allowed: bool, get: &dyn Fn(&str) -> Option<String>) -> DevOverride {
    if !allowed {
        return DevOverride::default();
    }
    DevOverride {
        dir: get("BROUILLON_UPDATE_DEV_DIR").filter(|s| !s.is_empty()).map(PathBuf::from),
        url: get("BROUILLON_UPDATE_URL").filter(|s| !s.is_empty()),
        pubkey: get("BROUILLON_UPDATE_DEV_PUBKEY").and_then(|h| keys::key_from_hex(&h)),
        force_installed: get("BROUILLON_UPDATE_FORCE_INSTALLED").as_deref() == Some("1"),
    }
}

/// Everything the background pass needs (all `Send`).
pub struct Plan {
    pub ctx: Ctx,
    pub state: UpdateState,
    pub state_path: PathBuf,
    pub start_delay: Duration,
}

/// The one place a thread is started. Returns at once: no network call happens on the caller's thread.
pub fn spawn_check(plan: Plan, transport: Box<dyn Transport + Send>, on_event: impl Fn(Ev) + Send + 'static) -> std::thread::JoinHandle<()> {
    std::thread::spawn(move || {
        std::thread::sleep(plan.start_delay);
        let Plan { ctx, mut state, state_path, .. } = plan;
        let write = |p: &std::path::Path, b: &[u8]| crate::durable::durable_write(p, b);
        let now = now_epoch();
        let prev_check = state.last_check_epoch;
        state.last_check_epoch = now;
        let mut last_pct = u32::MAX;
        let mut announce = |m: &Manifest| on_event(Ev::Found(m.clone()));
        let mut progress = |done: u64, total: u64| {
            let pct = if total == 0 { 0 } else { ((done.min(total) * 100) / total) as u32 };
            if pct != last_pct {
                last_pct = pct;
                on_event(Ev::Progress(pct));
            }
        };
        let result = run_once(transport.as_ref(), &ctx, &mut state, now, &mut announce, &mut progress);
        let ev = match result {
            Ok(Outcome::UpToDate) | Ok(Outcome::DowngradeIgnored) | Ok(Outcome::Skipped(_)) => {
                state.record_ok();
                None
            }
            Ok(Outcome::Notify(m)) => {
                state.record_ok();
                Some(Ev::Notify(m))
            }
            Ok(Outcome::KnownFailure(m)) => Some(Ev::KnownFailure(m)),
            Ok(Outcome::Staged { manifest, installer, mandatory }) => {
                state.record_ok();
                Some(Ev::Staged { manifest, installer, mandatory })
            }
            // Offline (or the host is unreachable): silent, and the next launch simply tries again.
            Err(UpdateError::Network(e)) => {
                eprintln!("[update] offline: {e}");
                state.last_check_epoch = prev_check;
                None
            }
            Err(e) => {
                eprintln!("[update] {e}");
                state.record_failure(now_epoch());
                Some(Ev::Failed(e.to_string()))
            }
        };
        let _ = state.save(&state_path, &write);
        if let Some(ev) = ev {
            on_event(ev);
        }
    })
}

/// Start the update machinery. `dirty()`: any modified document. `persist()`: write the recovery
/// journal (same function as a normal exit). `journal_ok`: false with `--no-session`.
/// Returns the controller the banner view drives, or `None` when updates are off.
pub fn init(paths: &Paths, status: RwSignal<String>, dirty: impl Fn() -> bool + 'static, persist: impl Fn() + 'static, journal_ok: bool) -> Option<Rc<Ui>> {
    // Gate 1: off means NO request and no file touched.
    let settings_text = std::fs::read_to_string(paths.settings_file()).unwrap_or_default();
    let settings = Settings::build(&[(Layer::User, settings_text.as_str())]);
    if !enabled(&settings, env_is("BROUILLON_NO_UPDATE", "1")) || paths.root().join("no-update").exists() {
        return None;
    }
    let dev = dev_override(dev_allowed(), &|k| std::env::var(k).ok());
    let lang = detect_lang();
    let current_build: u64 = crate::buildinfo::BUILD.parse().unwrap_or(0);
    let state_path = paths.root().join("update-state.toml");
    let write = |p: &std::path::Path, b: &[u8]| crate::durable::durable_write(p, b);
    let banner = create_rw_signal(None);

    // Result of the previous installer run: "Updated to X" once, or a failure (remembered, never retried).
    let mut state = UpdateState::load(&state_path);
    let now = now_epoch();
    let mut cleanup = false;
    if let Some(outcome) = state.resolve_attempt(current_build, now) {
        cleanup = true;
        match &outcome {
            AttemptOutcome::Succeeded { version } => {
                let text = render(&Msg::Updated(version), lang);
                status.update(|s| *s = if s.is_empty() { text } else { format!("{text}  |  {s}") });
            }
            AttemptOutcome::Failed { .. } => show_failure(&state, lang, status, banner),
        }
        let _ = state.save(&state_path, &write);
    }
    let interval_h = settings.get_i64("updates.check_interval_hours", 24).max(0) as u64;
    if !state.may_attempt(now) || !state.check_due(now, interval_h) {
        return Some(Rc::new(Ui::new(state_path, status, banner, lang, dirty, persist, journal_ok, true, false, false)));
    }

    let exe = std::env::current_exe().unwrap_or_default();
    let mut trusted = keys::embedded();
    trusted.extend(dev.pubkey.clone());
    let (manifest_url, dev_http) = match (&dev.dir, &dev.url) {
        (Some(_), _) => ("http://127.0.0.1/updates/latest.json".to_string(), true),
        (None, Some(u)) => (u.clone(), true),
        _ => (DEFAULT_MANIFEST_URL.to_string(), false),
    };
    let can_install = is_installed_copy(&exe) || (dev.force_installed && !is_portable(&exe));
    let auto = settings.get_bool("updates.auto_install", true);
    let ctx = Ctx {
        manifest_url,
        dev: dev_http,
        keys: trusted,
        current_build,
        updates_dir: paths.root().join("updates"),
        auto,
        can_install,
        channel: settings.get_str("updates.channel", ""),
    };
    let ui = Rc::new(Ui::new(state_path.clone(), status, banner, lang, dirty, persist, journal_ok, auto, can_install, is_portable(&exe)));
    // Events arrive from the worker thread through a channel; the effect runs on the UI thread.
    let (tx, rx) = crossbeam_channel::unbounded::<Ev>();
    let ev_sig = create_signal_from_channel(rx);
    {
        let ui = ui.clone();
        create_effect(move |_| {
            if let Some(ev) = ev_sig.get() {
                ui.handle(ev);
            }
        });
    }
    let transport: Box<dyn Transport + Send> = match dev.dir {
        Some(d) => Box::new(DirTransport(d)),
        None => Box::new(UreqTransport { dev: dev_http }),
    };
    let updates_dir = ctx.updates_dir.clone();
    spawn_check(Plan { ctx, state, state_path, start_delay: START_DELAY }, transport, move |ev| {
        let _ = tx.send(ev);
    });
    if cleanup {
        // After a finished install attempt, whatever it left in the data folder goes (off the UI thread).
        std::thread::spawn(move || {
            std::thread::sleep(START_DELAY * 4);
            brouillon_update::install::clean_dir(&updates_dir, None);
            let _ = std::fs::remove_dir_all(updates_dir.join("tmp"));
        });
    }
    Some(ui)
}

/// "The update could not be installed automatically..." from the remembered failed build.
fn show_failure(state: &UpdateState, lang: Lang, status: RwSignal<String>, banner: RwSignal<Option<UpdateBanner>>) {
    let text = render(&Msg::CouldNotInstall { file: &state.failed_file, sha256: &state.failed_sha256, url: DOWNLOAD_PAGE }, lang);
    status.set(text.clone());
    banner.set(Some(UpdateBanner { kind: BannerKind::Info { skippable: false }, text }));
}

pub struct Ui {
    state_path: PathBuf,
    status: RwSignal<String>,
    pub banner: RwSignal<Option<UpdateBanner>>,
    lang: Lang,
    dirty: Box<dyn Fn() -> bool>,
    persist: Box<dyn Fn()>,
    journal_ok: bool,
    auto: bool,
    can_install: bool,
    portable: bool,
    offer: RefCell<Option<Manifest>>,
    /// Set by Later / Skip: a queued automatic start must then not happen.
    cancelled: Cell<bool>,
}

impl Ui {
    #[allow(clippy::too_many_arguments)]
    fn new(state_path: PathBuf, status: RwSignal<String>, banner: RwSignal<Option<UpdateBanner>>, lang: Lang, dirty: impl Fn() -> bool + 'static, persist: impl Fn() + 'static, journal_ok: bool, auto: bool, can_install: bool, portable: bool) -> Ui {
        Ui { state_path, status, banner, lang, dirty: Box::new(dirty), persist: Box::new(persist), journal_ok, auto, can_install, portable, offer: RefCell::new(None), cancelled: Cell::new(false) }
    }

    fn say(&self, m: &Msg) {
        self.status.set(render(m, self.lang));
    }

    fn show(&self, kind: BannerKind, m: &Msg) {
        let text = render(m, self.lang);
        self.status.set(text.clone());
        self.banner.set(Some(UpdateBanner { kind, text }));
    }

    fn save_state(&self, f: impl FnOnce(&mut UpdateState)) {
        let mut st = UpdateState::load(&self.state_path);
        f(&mut st);
        let _ = st.save(&self.state_path, &|p, b| crate::durable::durable_write(p, b));
    }

    pub fn handle(self: &Rc<Self>, ev: Ev) {
        match ev {
            Ev::Failed(e) => self.say(&Msg::Failed(&e)),
            Ev::Found(m) => {
                self.show(BannerKind::Downloading, &Msg::Downloading { version: &m.version, build: m.build, pct: 0 });
                *self.offer.borrow_mut() = Some(m);
            }
            Ev::Progress(pct) => {
                if let Some(m) = self.offer.borrow().as_ref() {
                    self.show(BannerKind::Downloading, &Msg::Downloading { version: &m.version, build: m.build, pct });
                }
            }
            Ev::KnownFailure(m) => {
                let text = render(&Msg::CouldNotInstall { file: &m.installer.file, sha256: &m.installer.sha256, url: DOWNLOAD_PAGE }, self.lang);
                self.status.set(text.clone());
                self.banner.set(Some(UpdateBanner { kind: BannerKind::Info { skippable: false }, text }));
            }
            Ev::Notify(m) => {
                let url = if m.notes_url.is_empty() { DOWNLOAD_PAGE } else { m.notes_url.as_str() };
                let msg = if self.portable {
                    Msg::Portable { version: &m.version, build: m.build, file: &m.installer.file, sha256: &m.installer.sha256, url: DOWNLOAD_PAGE }
                } else {
                    Msg::Available { version: &m.version, url }
                };
                self.show(BannerKind::Info { skippable: true }, &msg);
                *self.offer.borrow_mut() = Some(m);
            }
            Ev::Staged { manifest, installer, mandatory } => {
                let state_path = self.state_path.clone();
                let action = decide(self.auto, self.can_install, self.journal_ok, (self.dirty)(), mandatory);
                let version = manifest.version.clone();
                *self.offer.borrow_mut() = Some(manifest.clone());
                self.cancelled.set(false);
                match action {
                    Action::Notify => self.show(BannerKind::Info { skippable: true }, &Msg::Available { version: &version, url: DOWNLOAD_PAGE }),
                    Action::InstallNow => {
                        *PENDING.lock().unwrap() = Some(Pending { installer, manifest, state_path });
                        self.show(BannerKind::Ready, &Msg::Ready { dirty: false });
                        let me = self.clone();
                        exec_after(INSTALL_NOW_DELAY, move |_| {
                            if me.cancelled.get() || PENDING.lock().unwrap().is_none() {
                                return;
                            }
                            if (me.dirty)() {
                                // The user started typing in the meantime: wait for their choice or the exit.
                                (me.persist)();
                                me.show(BannerKind::Ready, &Msg::Ready { dirty: true });
                            } else {
                                me.update_now();
                            }
                        });
                    }
                    Action::InstallOnExit => {
                        (self.persist)(); // the journal already holds the work; the exit path rewrites it
                        *PENDING.lock().unwrap() = Some(Pending { installer, manifest, state_path });
                        self.show(BannerKind::Ready, &Msg::Ready { dirty: true });
                    }
                    Action::Mandatory => {
                        (self.persist)();
                        *PENDING.lock().unwrap() = Some(Pending { installer, manifest, state_path });
                        self.countdown(MANDATORY_SECS);
                    }
                }
            }
        }
    }

    /// Save everything, start the verified installer, and quit once it is really running.
    pub fn update_now(self: &Rc<Self>) {
        (self.persist)();
        match launch_pending() {
            Launch::Nothing => {}
            Launch::Refused(e) => self.say(&Msg::Failed(&e)),
            Launch::Failed => self.install_failed(),
            Launch::Started(child) => self.watch(child, EARLY_WINDOW.as_millis() as u64),
        }
    }

    /// Poll the child without blocking the UI: dead early = failure, alive after the window = quit.
    fn watch(self: &Rc<Self>, mut child: std::process::Child, left_ms: u64) {
        self.say(&Msg::Installing(&self.offer.borrow().as_ref().map(|m| m.version.clone()).unwrap_or_default()));
        match watch_early(&mut child, Duration::from_millis(0)) {
            EarlyState::Failed | EarlyState::ExitedOk => return self.install_failed(),
            EarlyState::Running => {}
        }
        let me = self.clone();
        exec_after(Duration::from_millis(250), move |_| {
            if left_ms <= 250 {
                std::process::exit(0);
            }
            me.watch(child, left_ms - 250);
        });
    }

    /// The installer died at once (blocked, refused, error code). Remember the build, show the manual route.
    fn install_failed(&self) {
        let now = now_epoch();
        self.save_state(|st| st.mark_failed_now(now));
        let st = UpdateState::load(&self.state_path);
        show_failure(&st, self.lang, self.status, self.banner);
        if let Some(m) = self.offer.borrow().as_ref() {
            let _ = std::fs::remove_file(self.state_path.parent().unwrap_or(std::path::Path::new(".")).join("updates").join(&m.installer.file));
        }
    }

    /// "Later": nothing is installed now or at close; the file is deleted; asked again after the interval.
    pub fn later(&self) {
        self.cancelled.set(true);
        discard_pending();
        self.banner.set(None);
        self.status.set(String::new());
    }

    /// "Skip this version": like Later, and this build is not offered again (a mandatory one still is).
    pub fn skip(&self) {
        if let Some(m) = self.offer.borrow().as_ref() {
            let b = m.build;
            self.save_state(|st| st.skipped_build = b);
        }
        self.later();
    }

    /// Non-dismissable banner: re-asserted every second, journal refreshed every 5 s.
    fn countdown(self: &Rc<Self>, remaining: u32) {
        let text = render(&Msg::Required { secs: remaining }, self.lang);
        self.status.set(text.clone());
        self.banner.set(Some(UpdateBanner { kind: BannerKind::Required, text }));
        if remaining % 5 == 0 {
            (self.persist)();
        }
        let me = self.clone();
        exec_after(Duration::from_secs(1), move |_| {
            if remaining <= 1 {
                me.update_now();
            } else {
                me.countdown(remaining - 1);
            }
        });
    }
}

fn discard_pending() {
    if let Some(p) = PENDING.lock().unwrap().take() {
        let _ = std::fs::remove_file(&p.installer);
    }
}

pub enum Launch {
    Nothing,
    /// Re-check refused the file (changed after staging): deleted, nothing ran.
    Refused(String),
    /// The installer could not even be started.
    Failed,
    Started(std::process::Child),
}

/// Re-verify size/hash/PE right before running, record the attempt (so a failed install can never
/// loop), and start the installer with TMP/TEMP inside the data folder. Never elevates.
pub fn launch_pending() -> Launch {
    let Some(p) = PENDING.lock().unwrap().take() else { return Launch::Nothing };
    if let Err(e) = verify_installer(&p.installer, &p.manifest.installer) {
        eprintln!("[update] installer re-check failed, not running it: {e}");
        let _ = std::fs::remove_file(&p.installer);
        return Launch::Refused(format!("installer check failed: {e}"));
    }
    let mut st = UpdateState::load(&p.state_path);
    st.record_attempt(p.manifest.build, &p.manifest.version);
    st.record_attempt_files(&p.manifest.installer.file, &p.manifest.installer.sha256, &p.manifest.installer.url);
    let write = |path: &std::path::Path, b: &[u8]| crate::durable::durable_write(path, b);
    let _ = st.save(&p.state_path, &write);
    let tmp = p.installer.parent().map(|d| d.join("tmp")).unwrap_or_else(|| PathBuf::from("tmp"));
    match launch_installer(&p.installer, &tmp) {
        Ok(child) => {
            eprintln!("[update] installer started for {}", p.manifest.version);
            Launch::Started(child)
        }
        Err(e) => {
            eprintln!("[update] could not start installer: {e}");
            Launch::Failed
        }
    }
}

/// Exit hook: launch the verified installer if one is pending. Returns true when it was started.
/// Call AFTER the recovery journal has been written. A failure here is reported at the next start
/// (the build number did not change), and that build is then never retried automatically.
pub fn on_exit() -> bool {
    matches!(launch_pending(), Launch::Started(_))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::buffer::Buffer;
    use crate::recovery::Journal;
    use crate::tabs::TabManager;
    use brouillon_update::install::sha256_file;
    use brouillon_update::manifest::Installer;
    use brouillon_update::net::Reply;
    use floem::reactive::SignalGet;
    use std::cell::RefCell;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    // PENDING is process-global: tests that touch it must not run in parallel.
    static SERIAL: Mutex<()> = Mutex::new(());

    fn pe_file(dir: &std::path::Path) -> (PathBuf, Installer) {
        let mut v = vec![0u8; 600];
        v[0] = b'M';
        v[1] = b'Z';
        v[60] = 128;
        v[128..132].copy_from_slice(b"PE\0\0");
        let p = dir.join("B-setup.exe");
        std::fs::write(&p, &v).unwrap();
        let (sha, size) = sha256_file(&p).unwrap();
        (p, Installer { file: "B-setup.exe".into(), url: "https://brouillon.rochetta.fr/downloads/B-setup.exe".into(), size_bytes: size, sha256: sha })
    }

    fn manifest(inst: Installer, min: u64) -> Manifest {
        Manifest {
            schema: 1,
            channel: "t".into(),
            version: "9.9.9".into(),
            build: 99,
            build_date_utc: "2026-11-05T14:32:10Z".into(),
            published_utc: "2026-11-05T14:32:10Z".into(),
            min_supported_build: min,
            installer: inst,
            notes_url: String::new(),
            notes: String::new(),
        }
    }

    /// A tab with unsaved text, the way the editor holds it.
    fn tabs_with_unsaved(text: &str) -> TabManager {
        let mut tm = TabManager::new();
        let t = tm.current_mut();
        let mut b = Buffer::from_str(text);
        b.mark_dirty();
        t.buffer = Arc::new(b);
        t.dirty = true;
        tm
    }

    fn ui_with(status: RwSignal<String>, tm: Rc<RefCell<TabManager>>, journal: Rc<Journal>, persisted: Rc<Cell<u32>>, dirty: bool, dir: &std::path::Path, portable: bool, can_install: bool) -> Rc<Ui> {
        Rc::new(Ui::new(
            dir.join("update-state.toml"),
            status,
            create_rw_signal(None),
            Lang::En,
            move || dirty,
            move || {
                persisted.set(persisted.get() + 1);
                let tm = tm.borrow();
                journal.snapshot_tabs(&tm).and_then(|_| journal.mark_clean_exit()).unwrap();
            },
            true,
            true,
            can_install,
            portable,
        ))
    }

    fn ui(status: RwSignal<String>, tm: Rc<RefCell<TabManager>>, journal: Rc<Journal>, persisted: Rc<Cell<u32>>, dirty: bool, dir: &std::path::Path) -> Rc<Ui> {
        ui_with(status, tm, journal, persisted, dirty, dir, false, true)
    }

    /// Mandatory update with unsaved work: the journal holds the text BEFORE anything is launched,
    /// the banner shows the countdown, the installer is only queued (not started).
    #[test]
    fn mandatory_update_saves_all_work_before_the_installer_can_start() {
        let _g = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
        *PENDING.lock().unwrap() = None;
        let dir = tempfile::tempdir().unwrap();
        let (installer, inst) = pe_file(dir.path());
        let jdir = tempfile::tempdir().unwrap();
        let journal = Rc::new(Journal::new(jdir.path()));
        let tm = Rc::new(RefCell::new(tabs_with_unsaved("precious unsaved text")));
        let persisted = Rc::new(Cell::new(0));
        let status = floem::reactive::create_rw_signal(String::new());
        let ui = ui(status, tm.clone(), journal.clone(), persisted.clone(), true, dir.path());

        ui.handle(Ev::Staged { manifest: manifest(inst, 98), installer: installer.clone(), mandatory: true });

        assert!(persisted.get() >= 1, "journal written");
        let docs = journal.recover();
        assert_eq!(docs.len(), 1);
        assert!(docs[0].text.contains("precious unsaved text"), "no work lost");
        assert!(journal.last_exit_clean());
        let s = status.get_untracked();
        assert!(s.contains("UPDATE REQUIRED") && s.contains("30 s"), "{s}");
        assert!(PENDING.lock().unwrap().is_some(), "queued, not launched");
        assert!(installer.exists());
        *PENDING.lock().unwrap() = None;
    }

    /// Modified document: nothing starts by itself, the journal is written, the banner offers the choice.
    #[test]
    fn modified_document_waits_for_the_user_with_journal_and_buttons() {
        let _g = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
        *PENDING.lock().unwrap() = None;
        let dir = tempfile::tempdir().unwrap();
        let (installer, inst) = pe_file(dir.path());
        let jdir = tempfile::tempdir().unwrap();
        let journal = Rc::new(Journal::new(jdir.path()));
        let tm = Rc::new(RefCell::new(tabs_with_unsaved("draft")));
        let persisted = Rc::new(Cell::new(0));
        let status = floem::reactive::create_rw_signal(String::new());
        let ui = ui(status, tm, journal.clone(), persisted.clone(), true, dir.path());
        ui.handle(Ev::Staged { manifest: manifest(inst, 0), installer, mandatory: false });
        assert_eq!(persisted.get(), 1);
        assert_eq!(journal.recover().len(), 1);
        let b = ui.banner.get_untracked().unwrap();
        assert_eq!(b.kind, BannerKind::Ready);
        assert!(b.text.starts_with("Ready: Brouillon will close, update and restart") && b.text.contains("unsaved work"), "{}", b.text);
        assert!(PENDING.lock().unwrap().is_some());
        *PENDING.lock().unwrap() = None;
    }

    /// Later deletes the file and clears the banner; Skip also remembers the build in the state file.
    #[test]
    fn later_and_skip_discard_the_installer_and_skip_is_remembered() {
        let _g = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
        *PENDING.lock().unwrap() = None;
        let dir = tempfile::tempdir().unwrap();
        let (installer, inst) = pe_file(dir.path());
        let jdir = tempfile::tempdir().unwrap();
        let journal = Rc::new(Journal::new(jdir.path()));
        let tm = Rc::new(RefCell::new(tabs_with_unsaved("x")));
        let status = floem::reactive::create_rw_signal(String::new());
        let ui = ui(status, tm, journal, Rc::new(Cell::new(0)), true, dir.path());
        ui.handle(Ev::Staged { manifest: manifest(inst.clone(), 0), installer: installer.clone(), mandatory: false });
        ui.later();
        assert!(!installer.exists() && PENDING.lock().unwrap().is_none() && ui.banner.get_untracked().is_none());
        assert_eq!(UpdateState::load(&dir.path().join("update-state.toml")).skipped_build, 0);
        let (installer, inst) = pe_file(dir.path());
        ui.handle(Ev::Staged { manifest: manifest(inst, 0), installer: installer.clone(), mandatory: false });
        ui.skip();
        assert!(!installer.exists());
        assert_eq!(UpdateState::load(&dir.path().join("update-state.toml")).skipped_build, 99);
    }

    /// Portable copy: never an installer; the message gives file name, SHA-256 and link.
    #[test]
    fn portable_copy_only_shows_file_hash_and_link() {
        let _g = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
        *PENDING.lock().unwrap() = None;
        let dir = tempfile::tempdir().unwrap();
        let (_, inst) = pe_file(dir.path());
        let jdir = tempfile::tempdir().unwrap();
        let status = floem::reactive::create_rw_signal(String::new());
        let ui = ui_with(status, Rc::new(RefCell::new(TabManager::new())), Rc::new(Journal::new(jdir.path())), Rc::new(Cell::new(0)), false, dir.path(), true, false);
        let sha = inst.sha256.clone();
        ui.handle(Ev::Notify(manifest(inst, 0)));
        let t = ui.banner.get_untracked().unwrap().text;
        assert!(t.contains("Portable copy") && t.contains("B-setup.exe") && t.contains(&sha) && t.contains("https://"), "{t}");
        assert!(PENDING.lock().unwrap().is_none(), "nothing queued to run");
    }

    /// Download progress is shown in the plain words of the request.
    #[test]
    fn found_and_progress_show_the_downloading_message() {
        let dir = tempfile::tempdir().unwrap();
        let (_, inst) = pe_file(dir.path());
        let jdir = tempfile::tempdir().unwrap();
        let status = floem::reactive::create_rw_signal(String::new());
        let ui = ui(status, Rc::new(RefCell::new(TabManager::new())), Rc::new(Journal::new(jdir.path())), Rc::new(Cell::new(0)), false, dir.path());
        ui.handle(Ev::Found(manifest(inst, 0)));
        ui.handle(Ev::Progress(42));
        assert_eq!(status.get_untracked(), "Update 9.9.9 (build 99) is available: downloading... 42%");
        assert_eq!(ui.banner.get_untracked().unwrap().kind, BannerKind::Downloading);
    }

    /// The installer died at once (e.g. execution from the temp folder refused): the build is remembered
    /// as failed, the message shows the manual route with the signed hash, no retry.
    #[test]
    fn installer_failure_shows_manual_route_and_remembers_the_build() {
        let _g = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
        *PENDING.lock().unwrap() = None;
        let dir = tempfile::tempdir().unwrap();
        let (_, inst) = pe_file(dir.path());
        let jdir = tempfile::tempdir().unwrap();
        let status = floem::reactive::create_rw_signal(String::new());
        let ui = ui(status, Rc::new(RefCell::new(TabManager::new())), Rc::new(Journal::new(jdir.path())), Rc::new(Cell::new(0)), false, dir.path());
        let sha = inst.sha256.clone();
        // what launch_pending records before spawning
        let mut st = UpdateState::default();
        st.record_attempt(99, "9.9.9");
        st.record_attempt_files("B-setup.exe", &sha, "https://brouillon.rochetta.fr/downloads/B-setup.exe");
        st.save(&dir.path().join("update-state.toml"), &|p, b| std::fs::write(p, b)).unwrap();
        *ui.offer.borrow_mut() = Some(manifest(inst, 0));
        ui.install_failed();
        let t = ui.banner.get_untracked().unwrap().text;
        assert!(t.starts_with("The update could not be installed automatically. Download the portable zip or run the installer as administrator"), "{t}");
        assert!(t.contains("B-setup.exe") && t.contains(&sha));
        let after = UpdateState::load(&dir.path().join("update-state.toml"));
        assert_eq!((after.failed_build, after.attempt_build), (99, 0));
        assert!(!after.may_attempt(now_epoch()), "backoff armed");
    }

    /// The exit hook re-verifies the file: a swapped installer is deleted and never run.
    #[test]
    fn on_exit_refuses_an_installer_changed_after_staging() {
        let _g = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
        let dir = tempfile::tempdir().unwrap();
        let (installer, inst) = pe_file(dir.path());
        std::fs::write(&installer, b"swapped after verification").unwrap();
        *PENDING.lock().unwrap() = Some(Pending { installer: installer.clone(), manifest: manifest(inst, 0), state_path: dir.path().join("update-state.toml") });
        assert!(!on_exit());
        assert!(!installer.exists());
        assert!(!dir.path().join("update-state.toml").exists(), "no attempt recorded");
    }

    #[test]
    fn dev_overrides_are_ignored_unless_the_build_allows_them() {
        let env = |k: &str| match k {
            "BROUILLON_UPDATE_DEV_DIR" => Some("C:/fake".to_string()),
            "BROUILLON_UPDATE_URL" => Some("http://127.0.0.1:1/x".to_string()),
            "BROUILLON_UPDATE_FORCE_INSTALLED" => Some("1".to_string()),
            _ => None,
        };
        let shipped = dev_override(false, &env);
        assert!(!shipped.active() && !shipped.force_installed && shipped.pubkey.is_none());
        let dev = dev_override(true, &env);
        assert!(dev.active() && dev.force_installed);
    }

    struct Counting(Arc<AtomicUsize>);
    impl Transport for Counting {
        fn get(&self, _url: &str, _t: Duration) -> Result<Reply, UpdateError> {
            self.0.fetch_add(1, Ordering::SeqCst);
            Err(UpdateError::Network("offline".into()))
        }
    }

    /// Startup adds no synchronous network call: the spawn returns first, the request happens later on
    /// the worker thread; offline is silent (no event) and the check time is not consumed.
    #[test]
    fn startup_spawns_a_thread_and_makes_no_call_before_returning_offline_is_silent() {
        let dir = tempfile::tempdir().unwrap();
        let calls = Arc::new(AtomicUsize::new(0));
        let events = Arc::new(AtomicUsize::new(0));
        let ev2 = events.clone();
        let plan = Plan {
            ctx: Ctx {
                manifest_url: DEFAULT_MANIFEST_URL.into(),
                dev: false,
                keys: vec![],
                current_build: 1,
                updates_dir: dir.path().join("updates"),
                auto: true,
                can_install: true,
                channel: String::new(),
            },
            state: UpdateState::default(),
            state_path: dir.path().join("update-state.toml"),
            start_delay: Duration::from_millis(400),
        };
        let t0 = std::time::Instant::now();
        let h = spawn_check(plan, Box::new(Counting(calls.clone())), move |_| {
            ev2.fetch_add(1, Ordering::SeqCst);
        });
        assert!(t0.elapsed() < Duration::from_millis(100), "spawn returns immediately");
        assert_eq!(calls.load(Ordering::SeqCst), 0, "no request on the calling thread");
        h.join().unwrap();
        assert!(calls.load(Ordering::SeqCst) >= 1, "the worker did ask");
        assert_eq!(events.load(Ordering::SeqCst), 0, "offline is silent");
        assert_eq!(UpdateState::load(&dir.path().join("update-state.toml")).last_check_epoch, 0, "a failed attempt does not use up the throttle");
    }
}
