//! BK-049 / BK-050 — external change detection and tail (follow) mode.
//!
//! `notify` wakes us up; the truth is always a re-stat against the stamp taken
//! when the file was loaded / saved (`FileStamp`). The same re-stat is used on
//! window focus and just before save (`check`).

use std::collections::HashMap;
use std::fs;
use std::io::{self, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{channel, Receiver};
use std::time::SystemTime;

use notify::{RecommendedWatcher, RecursiveMode, Watcher as _};

use crate::tabs::ExternalState;

/// What was observed on disk when the buffer was last synced.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FileStamp {
    pub modified: Option<SystemTime>,
    pub len: u64,
    pub readonly: bool,
}

impl FileStamp {
    /// `None` = file does not exist (deleted).
    pub fn of(path: &Path) -> Option<Self> {
        let m = fs::metadata(path).ok()?;
        Some(Self { modified: m.modified().ok(), len: m.len(), readonly: m.permissions().readonly() })
    }
}

/// Compare the file now against `known`.
pub fn check(path: &Path, known: &FileStamp) -> ExternalState {
    match FileStamp::of(path) {
        None => ExternalState::Deleted,
        Some(now) if now.readonly != known.readonly => ExternalState::ReadonlyFlipped,
        Some(now) if now.modified != known.modified || now.len != known.len => ExternalState::Modified,
        Some(_) => ExternalState::InSync,
    }
}

/// User option for modified-on-disk files.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ExternalPolicy {
    #[default]
    Ask,
    AutoReloadWhenClean,
    Ignore,
}

/// Answer choices when the buffer is dirty and the file changed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConflictChoice {
    Reload,
    Keep,
    Compare,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Reaction {
    Nothing,
    Reload,
    /// Show a prompt offering these choices.
    Prompt(Vec<ConflictChoice>),
}

/// Decide what to do about `state`. A dirty buffer never reloads silently.
pub fn react(state: ExternalState, dirty: bool, policy: ExternalPolicy) -> Reaction {
    if matches!(state, ExternalState::InSync) || policy == ExternalPolicy::Ignore {
        return Reaction::Nothing;
    }
    match (state, dirty, policy) {
        (ExternalState::Modified, false, ExternalPolicy::AutoReloadWhenClean) => Reaction::Reload,
        (ExternalState::Modified, ..) => {
            Reaction::Prompt(vec![ConflictChoice::Reload, ConflictChoice::Keep, ConflictChoice::Compare])
        }
        // Deleted / readonly flip: always inform, never reload.
        _ => Reaction::Prompt(vec![ConflictChoice::Keep]),
    }
}

/// Saving onto a read-only file: the dialog offers these.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReadOnlyChoice {
    MakeWritableAndSave,
    SaveAs,
    Cancel,
}

pub const READONLY_CHOICES: [ReadOnlyChoice; 3] =
    [ReadOnlyChoice::MakeWritableAndSave, ReadOnlyChoice::SaveAs, ReadOnlyChoice::Cancel];

/// Clear the read-only attribute (the "make writable" answer).
pub fn make_writable(path: &Path) -> io::Result<()> {
    let mut perm = fs::metadata(path)?.permissions();
    #[allow(clippy::permissions_set_readonly_false)]
    perm.set_readonly(false);
    fs::set_permissions(path, perm)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WatchEvent {
    pub path: PathBuf,
    pub state: ExternalState,
}

/// Watches a set of files (via their parent dirs, so atomic replace is seen).
pub struct FileWatcher {
    inner: RecommendedWatcher,
    rx: Receiver<notify::Result<notify::Event>>,
    files: HashMap<PathBuf, (FileStamp, ExternalState)>,
}

impl FileWatcher {
    pub fn new() -> notify::Result<Self> {
        let (tx, rx) = channel();
        let inner = notify::recommended_watcher(move |ev| {
            let _ = tx.send(ev);
        })?;
        Ok(Self { inner, rx, files: HashMap::new() })
    }

    /// Start watching `path`, synced as of now.
    pub fn watch(&mut self, path: &Path) -> notify::Result<()> {
        if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
            self.inner.watch(parent, RecursiveMode::NonRecursive)?;
        }
        if let Some(stamp) = FileStamp::of(path) {
            self.files.insert(path.to_path_buf(), (stamp, ExternalState::InSync));
        }
        Ok(())
    }

    pub fn unwatch(&mut self, path: &Path) {
        self.files.remove(path);
    }

    /// The buffer was reloaded / saved: re-sync the stamp.
    pub fn acknowledge(&mut self, path: &Path) {
        if let Some(stamp) = FileStamp::of(path) {
            self.files.insert(path.to_path_buf(), (stamp, ExternalState::InSync));
        }
    }

    /// Drain notifications and report each file whose state changed.
    pub fn poll(&mut self) -> Vec<WatchEvent> {
        let mut touched: Vec<PathBuf> = Vec::new();
        while let Ok(ev) = self.rx.try_recv() {
            if let Ok(ev) = ev {
                for p in ev.paths {
                    if self.files.contains_key(&p) && !touched.contains(&p) {
                        touched.push(p);
                    }
                }
            }
        }
        self.recheck(touched)
    }

    /// Re-stat every watched file (window focus, before save).
    pub fn recheck_all(&mut self) -> Vec<WatchEvent> {
        let all: Vec<PathBuf> = self.files.keys().cloned().collect();
        self.recheck(all)
    }

    fn recheck(&mut self, paths: Vec<PathBuf>) -> Vec<WatchEvent> {
        let mut out = Vec::new();
        for p in paths {
            let Some((stamp, last)) = self.files.get_mut(&p) else { continue };
            let state = check(&p, stamp);
            if state != *last && state != ExternalState::InSync {
                *last = state;
                out.push(WatchEvent { path: p, state });
            }
        }
        out
    }
}

/// Append-only follow position for one file (BK-050).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Tail {
    offset: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TailUpdate {
    Nothing,
    /// New text to append at the end of the buffer.
    Appended(String),
    /// File shrank / was rotated: reload everything from this text.
    Truncated(String),
}

impl Tail {
    /// Start following from the current end of `path`.
    pub fn at_end(path: &Path) -> io::Result<Self> {
        Ok(Self { offset: fs::metadata(path)?.len() })
    }

    /// Read what was appended since the last call. Never splits a UTF-8
    /// sequence: an incomplete tail is left for the next call.
    pub fn read_new(&mut self, path: &Path) -> io::Result<TailUpdate> {
        let len = fs::metadata(path)?.len();
        let truncated = len < self.offset;
        if truncated {
            self.offset = 0;
        }
        if len == self.offset {
            return Ok(TailUpdate::Nothing);
        }
        let mut f = fs::File::open(path)?;
        f.seek(SeekFrom::Start(self.offset))?;
        let mut bytes = Vec::new();
        f.take(len - self.offset).read_to_end(&mut bytes)?;
        let valid = match std::str::from_utf8(&bytes) {
            Ok(_) => bytes.len(),
            Err(e) if e.error_len().is_none() => e.valid_up_to(),
            Err(_) => bytes.len(), // genuinely invalid bytes: take them lossily
        };
        self.offset += valid as u64;
        let text = String::from_utf8_lossy(&bytes[..valid]).into_owned();
        Ok(match (text.is_empty(), truncated) {
            (true, _) => TailUpdate::Nothing,
            (false, true) => TailUpdate::Truncated(text),
            (false, false) => TailUpdate::Appended(text),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use std::time::{Duration, Instant};

    #[test]
    fn touch_yields_modified_event_within_a_second() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("a.txt");
        fs::write(&p, "one").unwrap();
        let mut w = FileWatcher::new().unwrap();
        w.watch(&p).unwrap();
        std::thread::sleep(Duration::from_millis(100));
        fs::write(&p, "one-two").unwrap();
        let t0 = Instant::now();
        let mut got = Vec::new();
        while got.is_empty() && t0.elapsed() < Duration::from_secs(1) {
            got = w.poll();
            std::thread::sleep(Duration::from_millis(20));
        }
        assert_eq!(got, vec![WatchEvent { path: p.clone(), state: ExternalState::Modified }]);
        // Not re-reported until acknowledged and changed again.
        assert!(w.recheck_all().is_empty());
        w.acknowledge(&p);
        fs::remove_file(&p).unwrap();
        assert_eq!(w.recheck_all()[0].state, ExternalState::Deleted);
    }

    #[test]
    fn check_detects_readonly_flip() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("a.txt");
        fs::write(&p, "x").unwrap();
        let stamp = FileStamp::of(&p).unwrap();
        assert_eq!(check(&p, &stamp), ExternalState::InSync);
        let mut perm = fs::metadata(&p).unwrap().permissions();
        perm.set_readonly(true);
        fs::set_permissions(&p, perm).unwrap();
        assert_eq!(check(&p, &stamp), ExternalState::ReadonlyFlipped);
        make_writable(&p).unwrap();
        assert_eq!(check(&p, &stamp), ExternalState::InSync);
    }

    #[test]
    fn dirty_buffer_never_auto_reloads() {
        use ExternalPolicy::*;
        assert_eq!(react(ExternalState::Modified, false, AutoReloadWhenClean), Reaction::Reload);
        assert!(matches!(react(ExternalState::Modified, true, AutoReloadWhenClean), Reaction::Prompt(_)));
        assert!(matches!(react(ExternalState::Modified, false, Ask), Reaction::Prompt(_)));
        assert_eq!(react(ExternalState::Modified, true, Ignore), Reaction::Nothing);
        assert_eq!(react(ExternalState::InSync, true, Ask), Reaction::Nothing);
        let Reaction::Prompt(c) = react(ExternalState::Modified, true, Ask) else { panic!() };
        assert_eq!(c, vec![ConflictChoice::Reload, ConflictChoice::Keep, ConflictChoice::Compare]);
    }

    #[test]
    fn tail_follows_appended_lines_and_handles_truncation() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("log.txt");
        fs::write(&p, "start\n").unwrap();
        let mut t = Tail::at_end(&p).unwrap();
        assert_eq!(t.read_new(&p).unwrap(), TailUpdate::Nothing);
        let mut f = fs::OpenOptions::new().append(true).open(&p).unwrap();
        let chunk: String = (0..100).map(|i| format!("line {i}\n")).collect();
        f.write_all(chunk.as_bytes()).unwrap();
        assert_eq!(t.read_new(&p).unwrap(), TailUpdate::Appended(chunk));
        assert_eq!(t.read_new(&p).unwrap(), TailUpdate::Nothing);
        fs::write(&p, "new\n").unwrap();
        assert_eq!(t.read_new(&p).unwrap(), TailUpdate::Truncated("new\n".into()));
    }

    #[test]
    fn tail_waits_for_complete_utf8_sequence() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("u.txt");
        fs::write(&p, "").unwrap();
        let mut t = Tail::at_end(&p).unwrap();
        let e = "é".as_bytes(); // 2 bytes
        fs::write(&p, [b'a', e[0]]).unwrap();
        assert_eq!(t.read_new(&p).unwrap(), TailUpdate::Appended("a".into()));
        fs::write(&p, ["a".as_bytes(), e].concat()).unwrap();
        assert_eq!(t.read_new(&p).unwrap(), TailUpdate::Appended("é".into()));
    }
}
