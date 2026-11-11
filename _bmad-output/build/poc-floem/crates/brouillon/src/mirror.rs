//! The "mirror signals": buffer / cursor / path / ... of the ACTIVE tab, kept
//! in sync with `TabManager` in both directions.
//!
//! * `pull_from_active` copies the active tab into the mirrors (tab switch,
//!   new tab, close, open). It is BATCHED: the push effect below must run once,
//!   after every mirror holds the new tab's value. Unbatched, the effect ran
//!   between `buffer.set(..)` and `path.set(..)`, saw the NEW tab with the OLD
//!   tab's path and renamed it: a fresh scratch tab became "three.txt" (and
//!   was then saved to the session under that title, reopening as a duplicate).
//! * `install` runs the push effect: whenever a mirror changes, the new values
//!   are written back into the active tab (text, cursor, drawing, encoding...).

use std::path::PathBuf;
use std::sync::Arc;

use floem::reactive::{batch, create_effect, RwSignal, SignalGet, SignalUpdate, SignalWith};

use crate::buffer::Buffer;
use crate::encoding::{DocumentEncoding, EolMode};
use crate::sketch::SketchDocument;
use crate::tabs::TabManager;

#[derive(Clone, Copy)]
pub struct Mirrors {
    pub buffer: RwSignal<Arc<Buffer>>,
    pub cursor: RwSignal<(usize, usize)>,
    pub path: RwSignal<Option<PathBuf>>,
    pub remote_path: RwSignal<Option<String>>,
    pub sketch: RwSignal<SketchDocument>,
    pub encoding: RwSignal<DocumentEncoding>,
    pub eol: RwSignal<EolMode>,
}

impl Mirrors {
    /// Active tab -> mirrors, as one batch (see the module doc).
    pub fn pull_from_active(&self, tabs: RwSignal<TabManager>) {
        let (buf, cur, p, rp, sketch, enc, eol) = tabs.with_untracked(|tm: &TabManager| {
            let t = tm.current();
            (t.buffer.clone(), t.cursor, t.path.clone(), t.remote_path.clone(), t.sketch.clone(), t.encoding, t.eol)
        });
        batch(|| {
            self.buffer.set(buf);
            self.cursor.set(cur);
            self.path.set(p);
            self.remote_path.set(rp);
            self.sketch.set(sketch);
            self.encoding.set(enc);
            self.eol.set(eol);
        });
    }

    /// Mirrors -> active tab. Reads (and therefore tracks) the mirror signals.
    pub fn push_to_active(&self, tm: &mut TabManager) {
        let buf = self.buffer.get();
        let cur = self.cursor.get();
        let p = self.path.get();
        let sketch = self.sketch.get();
        let enc = self.encoding.get();
        let eol = self.eol.get();
        let t = tm.current_mut();
        t.sketch = sketch;
        // Modified = unsaved text OR unsaved strokes (a drawing-only tab has a
        // clean text buffer: it used to close with no prompt and lose the strokes).
        // A built-in help document is never "modified" (no prompt on close, no journal).
        t.dirty = t.help_doc.is_none() && (buf.is_dirty() || t.sketch_unsaved());
        t.buffer = buf;
        t.cursor = cur;
        t.encoding = enc;
        t.eol = eol;
        // Title follows the path when it changes (Save As); untitled tabs keep
        // their generated name.
        if t.path != p {
            // Saved to a file: it is an ordinary document from now on.
            if p.is_some() {
                t.help_doc = None;
                t.meta.readonly = false;
                // Saved As a local file: it no longer belongs to a server.
                t.remote_origin = None;
            }
            if let Some(name) = p.as_ref().and_then(|pp| pp.file_name()).and_then(|s| s.to_str()) {
                t.title = name.to_string();
            }
            t.path = p;
        }
    }

    /// Keep the active tab in sync with the mirrors from now on.
    pub fn install(&self, tabs: RwSignal<TabManager>) {
        let m = *self;
        create_effect(move |_| {
            // Read the mirrors before taking the tabs write lock.
            tabs.update(|tm| m.push_to_active(tm));
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use floem::reactive::create_rw_signal;

    fn fixture(files: &[(&str, &str)]) -> (Mirrors, RwSignal<TabManager>, Vec<PathBuf>) {
        let dir = std::env::temp_dir().join(format!("brouillon-mirror-{}-{:?}", std::process::id(), std::thread::current().id()));
        std::fs::create_dir_all(&dir).unwrap();
        let mut paths = Vec::new();
        let mut tm = TabManager::new();
        for (name, text) in files {
            let p = dir.join(name);
            std::fs::write(&p, text).unwrap();
            tm.open_file(&p).unwrap();
            paths.push(p);
        }
        let active = tm.current();
        let m = Mirrors {
            buffer: create_rw_signal(active.buffer.clone()),
            cursor: create_rw_signal(active.cursor),
            path: create_rw_signal(active.path.clone()),
            remote_path: create_rw_signal(None),
            sketch: create_rw_signal(SketchDocument::new()),
            encoding: create_rw_signal(active.encoding),
            eol: create_rw_signal(active.eol),
        };
        let tabs = create_rw_signal(tm);
        m.install(tabs);
        (m, tabs, paths)
    }

    /// Regression (scenario d): Ctrl+N on a file tab must give an untitled
    /// scratch tab, not a tab renamed after the previous tab's file.
    #[test]
    fn new_scratch_keeps_its_untitled_title_and_no_path() {
        let (m, tabs, _) = fixture(&[("one.txt", "1\n"), ("two.txt", "2\n"), ("three.txt", "three\n")]);
        tabs.update(|tm| {
            tm.new_scratch();
        });
        m.pull_from_active(tabs);
        tabs.with_untracked(|tm| {
            let t = tm.current();
            assert!(t.path.is_none(), "scratch tab must not inherit a path");
            assert!(t.title.starts_with("untitled-"), "scratch title was {:?}", t.title);
            let titles: Vec<_> = tm.iter().map(|t| t.title.clone()).collect();
            assert_eq!(titles[..3], ["one.txt", "two.txt", "three.txt"]);
        });
        // switching away and back keeps everything as is
        tabs.update(|tm| tm.switch_to(2));
        m.pull_from_active(tabs);
        tabs.update(|tm| tm.switch_to(3));
        m.pull_from_active(tabs);
        tabs.with_untracked(|tm| {
            assert!(tm.current().path.is_none());
            assert!(tm.current().title.starts_with("untitled-"));
            assert_eq!(tm.iter().filter(|t| t.title == "three.txt").count(), 1);
        });
    }

    fn stroke() -> crate::sketch::Stroke {
        use crate::sketch::{SketchColor, SketchTool, Stroke, StrokePoint};
        let pt = |x: f32, y: f32| StrokePoint { x, y, pressure: 1.0, tilt_x: 0.0, tilt_y: 0.0 };
        Stroke {
            points: vec![pt(1.0, 2.0), pt(30.0, 40.0)],
            tool: SketchTool::Ballpoint,
            color: SketchColor::Black,
            width: 2.0,
            layer_idx: 0,
            is_eraser: false,
        }
    }

    /// Regression (crayon mode): a tab whose only change is drawn strokes is
    /// modified, so closing it asks first; saving the drawing clears the flag.
    #[test]
    fn strokes_alone_make_the_tab_modified() {
        let (m, tabs, _) = fixture(&[("a.txt", "alpha\n")]);
        assert!(!tabs.with_untracked(|tm| tm.current().dirty));
        let mut doc = SketchDocument::new();
        doc.active_page_mut().strokes.push(stroke());
        m.sketch.set(doc);
        tabs.with_untracked(|tm| {
            let t = tm.current();
            assert!(t.dirty, "unsaved strokes must mark the tab modified");
            assert!(!t.buffer.is_dirty(), "the text itself is untouched");
        });
        // "saved": the stroke count on disk now matches
        tabs.update(|tm| {
            let t = tm.current_mut();
            t.sketch_saved = t.sketch.stroke_count();
        });
        m.sketch.update(|d| d.add_page()); // any later change re-runs the sync
        tabs.with_untracked(|tm| assert!(!tm.current().dirty, "saved drawing is clean"));
        // erasing it all again after a save is a change as well
        m.sketch.set(SketchDocument::new());
        tabs.with_untracked(|tm| assert!(tm.current().dirty));
    }

    #[test]
    fn save_as_renames_the_tab_after_the_new_path() {
        let (m, tabs, paths) = fixture(&[("one.txt", "1\n")]);
        tabs.update(|tm| {
            tm.new_scratch();
        });
        m.pull_from_active(tabs);
        m.path.set(Some(paths[0].with_file_name("saved-as.txt")));
        tabs.with_untracked(|tm| assert_eq!(tm.current().title, "saved-as.txt"));
    }
}
