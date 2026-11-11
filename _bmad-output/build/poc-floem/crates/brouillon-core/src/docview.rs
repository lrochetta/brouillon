//! `DocView`: the plug-in seam between the tab/window code and viewers of non-text kinds.
//!
//! Toolkit-neutral on purpose: no floem types cross this trait, only data (status text, text ranges, byte
//! counts). The UI maps a view to widgets. Viewers that parse hostile input live in an optional module
//! (separate process); light ones may be registered in-process. Nothing here is built at startup: the
//! registry exists from the first file open of a routed kind.
//! Design: `docs-formats/04` section 2 and `05` section 2.4.

use crate::kind::{Detection, FileKind};
use std::path::Path;
use std::sync::{Mutex, OnceLock};

/// Lifecycle of an optional module, as seen by routing. The transitions are owned by `brouillon-modules`.
/// absent -> offered -> installing -> ready -> loaded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ModuleState {
    /// Not installed, nothing offered yet.
    Absent,
    /// The offer bar was shown; no answer yet.
    Offered,
    /// Download or install in progress (only after an explicit click).
    Installing,
    /// Installed and verified on disk; never started this session. Costs nothing.
    Ready,
    /// Loaded at its first request; stays loaded until the application exits.
    Loaded,
    /// The last install or load failed (plain reason); back to an offer on an explicit retry.
    Failed(String),
}

/// Which lens shows a document right now.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ViewId {
    Text,
    Hex,
    /// A registered viewer (factory id).
    Viewer(&'static str),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LoadState {
    Ready,
    /// 0..=100
    Loading(u8),
    Failed(String),
    /// The viewer needs this module and it is not loaded.
    ModuleMissing(&'static str),
}

#[derive(Debug, Clone, Default)]
pub struct Caps {
    pub editable: bool,
    pub read_only_reason: Option<&'static str>,
    pub text_search: bool,
    pub text_for_agent: bool,
    pub selectable_text: bool,
    pub streaming: bool,
    /// Above this size the factory refuses (0 = no cap declared).
    pub max_bytes: u64,
    pub needs_helper: bool,
}

pub trait DocView: Send {
    fn kind(&self) -> FileKind;
    fn state(&self) -> LoadState;
    fn caps(&self) -> Caps;
    /// One-line status for the status bar (page, zoom, ...).
    fn status(&self) -> String;
    /// Bytes held by this view, for the memory budget.
    fn mem_bytes(&self) -> u64;
    /// Drop caches under memory pressure; keep the model and the position.
    fn trim(&mut self);
    /// Text for search and agents (only when `caps().text_for_agent`).
    fn text_range(&self, _start: usize, _end: usize) -> Option<String> {
        None
    }
    fn close(self: Box<Self>);
}

pub trait DocViewFactory: Send + Sync {
    fn id(&self) -> &'static str;
    fn kinds(&self) -> &'static [FileKind];
    /// The module that must be Ready/Loaded for this factory to be used; `None` = in-process.
    fn module(&self) -> Option<&'static str>;
    /// 0 = not for these bytes, higher = better.
    fn probe(&self, d: &Detection) -> u8 {
        if self.kinds().contains(&d.kind) {
            10
        } else {
            0
        }
    }
    /// Must not block the UI thread for long: implementations hand heavy work to a worker.
    fn open(&self, path: &Path, d: &Detection) -> Result<Box<dyn DocView>, String>;
}

/// Registered factories. Built lazily; empty until a viewer or module registers one.
#[derive(Default)]
pub struct KindRegistry {
    factories: Vec<Box<dyn DocViewFactory>>,
}

impl KindRegistry {
    pub fn register(&mut self, f: Box<dyn DocViewFactory>) {
        self.factories.push(f);
    }

    pub fn factory_for(&self, d: &Detection) -> Option<&dyn DocViewFactory> {
        self.factories.iter().map(|f| (f.probe(d), f)).filter(|(s, _)| *s > 0).max_by_key(|(s, _)| *s).map(|(_, f)| f.as_ref())
    }

    pub fn len(&self) -> usize {
        self.factories.len()
    }

    pub fn is_empty(&self) -> bool {
        self.factories.is_empty()
    }
}

/// Process-wide registry, created on first use (never at startup).
pub fn registry() -> &'static Mutex<KindRegistry> {
    static R: OnceLock<Mutex<KindRegistry>> = OnceLock::new();
    R.get_or_init(|| Mutex::new(KindRegistry::default()))
}

/// What to do with a freshly detected file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Route {
    /// The normal text editor (all text kinds, and "always open as text").
    Text,
    /// A registered viewer serves it.
    Viewer { factory: &'static str },
    /// No viewer yet: open in `fallback` now; when `offer` is set, show the non-modal bar for that module.
    Fallback { fallback: ViewId, module: Option<&'static str>, offer: bool },
}

/// Pure routing decision. `module_state(id)` is only asked for kinds that have a module.
pub fn route(d: &Detection, reg: &KindRegistry, always_text: bool, module_state: &dyn Fn(&str) -> ModuleState) -> Route {
    if always_text || !d.kind.routed_to_viewer() && d.kind.module().is_none() {
        return Route::Text;
    }
    let module = d.kind.module();
    let state = module.map(module_state);
    let usable = matches!(state, None | Some(ModuleState::Ready) | Some(ModuleState::Loaded));
    if usable {
        if let Some(f) = reg.factory_for(d) {
            return Route::Viewer { factory: f.id() };
        }
    }
    if !d.kind.routed_to_viewer() {
        // Text-backed kind with a module (RTF): the text editor is the fallback, the bar is only an offer.
        let offer = matches!(state, Some(ModuleState::Absent) | Some(ModuleState::Offered) | Some(ModuleState::Failed(_)));
        return if offer { Route::Fallback { fallback: ViewId::Text, module, offer } } else { Route::Text };
    }
    let offer = matches!(state, Some(ModuleState::Absent) | Some(ModuleState::Offered) | Some(ModuleState::Failed(_)));
    Route::Fallback { fallback: ViewId::Hex, module, offer }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::kind::detect_kind;

    struct Fake;
    struct FakeView;
    impl DocView for FakeView {
        fn kind(&self) -> FileKind {
            FileKind::Pdf
        }
        fn state(&self) -> LoadState {
            LoadState::Ready
        }
        fn caps(&self) -> Caps {
            Caps { read_only_reason: Some("test"), ..Caps::default() }
        }
        fn status(&self) -> String {
            "page 1".into()
        }
        fn mem_bytes(&self) -> u64 {
            0
        }
        fn trim(&mut self) {}
        fn close(self: Box<Self>) {}
    }
    impl DocViewFactory for Fake {
        fn id(&self) -> &'static str {
            "fake-pdf"
        }
        fn kinds(&self) -> &'static [FileKind] {
            &[FileKind::Pdf]
        }
        fn module(&self) -> Option<&'static str> {
            Some("docs")
        }
        fn open(&self, _p: &Path, _d: &Detection) -> Result<Box<dyn DocView>, String> {
            Ok(Box::new(FakeView))
        }
    }

    fn st(s: ModuleState) -> impl Fn(&str) -> ModuleState {
        move |_| s.clone()
    }

    #[test]
    fn text_kinds_never_leave_the_text_editor() {
        let d = detect_kind("a.md", b"# t");
        assert_eq!(route(&d, &KindRegistry::default(), false, &st(ModuleState::Absent)), Route::Text);
        let d = detect_kind("a.png", &[0x89, b'P', b'N', b'G']);
        assert_eq!(route(&d, &KindRegistry::default(), false, &st(ModuleState::Absent)), Route::Text);
    }

    #[test]
    fn pdf_without_module_falls_back_to_hex_and_offers() {
        let d = detect_kind("a.pdf", b"%PDF-1.7");
        let r = route(&d, &KindRegistry::default(), false, &st(ModuleState::Absent));
        assert_eq!(r, Route::Fallback { fallback: ViewId::Hex, module: Some("docs"), offer: true });
        // installing: still hex, but no second offer
        let r = route(&d, &KindRegistry::default(), false, &st(ModuleState::Installing));
        assert_eq!(r, Route::Fallback { fallback: ViewId::Hex, module: Some("docs"), offer: false });
        // always open as text wins
        assert_eq!(route(&d, &KindRegistry::default(), true, &st(ModuleState::Absent)), Route::Text);
    }

    #[test]
    fn rtf_offers_but_stays_text() {
        let d = detect_kind("a.rtf", b"{\\rtf1 hi}");
        let r = route(&d, &KindRegistry::default(), false, &st(ModuleState::Absent));
        assert_eq!(r, Route::Fallback { fallback: ViewId::Text, module: Some("docs"), offer: true });
        assert_eq!(route(&d, &KindRegistry::default(), false, &st(ModuleState::Ready)), Route::Text);
    }

    #[test]
    fn registered_viewer_is_used_only_when_its_module_is_usable() {
        let mut reg = KindRegistry::default();
        reg.register(Box::new(Fake));
        let d = detect_kind("a.pdf", b"%PDF-1.7");
        assert!(matches!(route(&d, &reg, false, &st(ModuleState::Absent)), Route::Fallback { .. }));
        assert_eq!(route(&d, &reg, false, &st(ModuleState::Ready)), Route::Viewer { factory: "fake-pdf" });
        assert_eq!(route(&d, &reg, false, &st(ModuleState::Loaded)), Route::Viewer { factory: "fake-pdf" });
        let v = reg.factory_for(&d).unwrap().open(Path::new("x.pdf"), &d).unwrap();
        assert_eq!(v.status(), "page 1");
        assert_eq!(v.caps().read_only_reason, Some("test"));
        v.close();
    }

    #[test]
    fn global_registry_starts_empty() {
        assert!(registry().lock().unwrap().is_empty());
    }
}
