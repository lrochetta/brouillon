//! External-change guard: what the file looked like when a document was opened or
//! last saved, and what to do when the disk no longer agrees.
//!
//! Cheap by design: no thread per file. A stat (mtime + size) is compared with the
//! remembered stamp; the content is read only when the stamp changed AND a save is
//! about to overwrite it (and even then the size is compared first, so a huge file is
//! never read just to learn that it differs).
//!
//! The decisions are pure functions (`on_poll`, `on_save`) so the whole table is unit tested.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::SystemTime;

use crate::file_io::SaveSpec;

/// What a stat of the file looks like (read-only flag deliberately ignored here).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Stamp {
    pub modified: Option<SystemTime>,
    pub len: u64,
}

impl Stamp {
    pub fn of(path: &Path) -> Option<Self> {
        let m = fs::metadata(path).ok()?;
        Some(Self { modified: m.modified().ok(), len: m.len() })
    }
}

/// The disk compared with the remembered stamp.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Disk {
    Unchanged,
    Changed,
    /// Deleted, renamed away, or no longer readable.
    Gone,
}

#[derive(Default)]
struct Registry {
    known: HashMap<PathBuf, Stamp>,
    /// Disk stamp the user chose to ignore: no banner while the disk stays like this.
    dismissed: HashMap<PathBuf, Stamp>,
}

fn registry() -> std::sync::MutexGuard<'static, Registry> {
    static R: OnceLock<Mutex<Registry>> = OnceLock::new();
    R.get_or_init(Default::default).lock().unwrap_or_else(|e| e.into_inner())
}

/// The document now matches the disk (opened, saved or reloaded): remember its stamp.
pub fn note(path: &Path) {
    let mut r = registry();
    r.dismissed.remove(path);
    match Stamp::of(path) {
        Some(s) => {
            r.known.insert(path.to_path_buf(), s);
        }
        None => {
            r.known.remove(path);
        }
    }
}

/// The document no longer maps to this path.
pub fn forget(path: &Path) {
    let mut r = registry();
    r.known.remove(path);
    r.dismissed.remove(path);
}

/// "Ignore" on the banner: stop warning about this disk state (saving still checks).
pub fn dismiss(path: &Path) {
    let mut r = registry();
    match Stamp::of(path) {
        Some(s) => {
            r.dismissed.insert(path.to_path_buf(), s);
        }
        None => {
            r.dismissed.insert(path.to_path_buf(), Stamp { modified: None, len: u64::MAX });
        }
    }
}

fn compare(known: Option<Stamp>, now: Option<Stamp>) -> Disk {
    match (known, now) {
        (None, _) => Disk::Unchanged, // never seen: cannot tell
        (Some(_), None) => Disk::Gone,
        (Some(k), Some(n)) if k == n => Disk::Unchanged,
        (Some(_), Some(_)) => Disk::Changed,
    }
}

/// Disk state against the remembered stamp (always checked, used before a save).
pub fn disk_state(path: &Path) -> Disk {
    let known = registry().known.get(path).copied();
    compare(known, Stamp::of(path))
}

/// Same, but `Unchanged` when the user already dismissed this very disk state (banner use).
pub fn disk_state_for_banner(path: &Path) -> Disk {
    let (known, dismissed) = {
        let r = registry();
        (r.known.get(path).copied(), r.dismissed.get(path).copied())
    };
    let now = Stamp::of(path);
    let missing = Stamp { modified: None, len: u64::MAX };
    match (dismissed, now) {
        (Some(d), Some(n)) if d == n => Disk::Unchanged,
        (Some(d), None) if d == missing => Disk::Unchanged,
        _ => compare(known, now),
    }
}

/// What the periodic check tells the UI to do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Notice {
    Nothing,
    /// Not modified locally: reload quietly and say so.
    Reload,
    /// Show "the file changed on disk" with Reload / Ignore (local edits are kept).
    Changed,
    /// Show "the file is gone from disk".
    Gone,
}

/// Decision table for the periodic / focus check.
pub fn on_poll(local_modified: bool, disk: Disk, auto_reload_unmodified: bool) -> Notice {
    match (disk, local_modified) {
        (Disk::Unchanged, _) => Notice::Nothing,
        (Disk::Changed, false) if auto_reload_unmodified => Notice::Reload,
        (Disk::Changed, _) => Notice::Changed,
        (Disk::Gone, _) => Notice::Gone,
    }
}

/// What a save may do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SaveGate {
    Write,
    /// The disk holds something else than what we know: ask first, write nothing.
    Conflict,
}

/// Decision table for a save. `same_content` = the bytes on disk already equal what we would write.
pub fn on_save(disk: Disk, same_content: bool) -> SaveGate {
    match disk {
        Disk::Unchanged => SaveGate::Write,
        // Deleted: writing recreates the file and destroys nothing.
        Disk::Gone => SaveGate::Write,
        Disk::Changed if same_content => SaveGate::Write,
        Disk::Changed => SaveGate::Conflict,
    }
}

/// Before writing `text` to `path`: may the save go ahead?
pub fn save_gate(path: &Path, text: &str, spec: &SaveSpec) -> SaveGate {
    save_gate_pieces(path, || std::iter::once(text), spec)
}

/// [`save_gate`] for a text that lives in pieces (rope chunks): when the disk changed, the encoded
/// pieces are compared with the file as they are produced, so neither the text nor its encoding is
/// ever materialised. `pieces` is only called when the disk differs from the remembered stamp.
pub fn save_gate_pieces<'a, I: Iterator<Item = &'a str>>(path: &Path, pieces: impl FnOnce() -> I, spec: &SaveSpec) -> SaveGate {
    use std::io::Read;
    let disk = disk_state(path);
    if disk != Disk::Changed {
        return on_save(disk, false);
    }
    let same = (|| {
        let want_len = Stamp::of(path)?.len;
        let mut file = std::io::BufReader::with_capacity(256 << 10, fs::File::open(path).ok()?);
        let mut seen = 0u64;
        let mut disk_buf = Vec::new();
        let mut sink = |b: &[u8]| -> std::io::Result<()> {
            disk_buf.resize(b.len(), 0);
            file.read_exact(&mut disk_buf)?;
            seen += b.len() as u64;
            if disk_buf == b {
                Ok(())
            } else {
                Err(std::io::ErrorKind::InvalidData.into())
            }
        };
        crate::file_io::encode_stream(pieces(), spec, &mut sink).ok()?;
        Some(seen == want_len)
    })()
    .unwrap_or(false);
    on_save(disk, same)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    const SPEC: SaveSpec<'static> = SaveSpec { codepage: "utf-8", bom: false, eol: None };

    #[test]
    fn poll_table() {
        use Disk as D;
        use Notice as N;
        // (local_modified, disk, auto) -> notice
        let rows = [
            (false, D::Unchanged, true, N::Nothing),
            (true, D::Unchanged, true, N::Nothing),
            (false, D::Changed, true, N::Reload),
            (false, D::Changed, false, N::Changed),
            (true, D::Changed, true, N::Changed), // never a silent reload over local edits
            (true, D::Changed, false, N::Changed),
            (false, D::Gone, true, N::Gone),
            (true, D::Gone, true, N::Gone),
        ];
        for (m, d, a, want) in rows {
            assert_eq!(on_poll(m, d, a), want, "modified={m} disk={d:?} auto={a}");
        }
    }

    #[test]
    fn save_table() {
        use Disk::*;
        assert_eq!(on_save(Unchanged, false), SaveGate::Write);
        assert_eq!(on_save(Gone, false), SaveGate::Write);
        assert_eq!(on_save(Changed, false), SaveGate::Conflict);
        assert_eq!(on_save(Changed, true), SaveGate::Write);
    }

    fn set_mtime_ahead(p: &Path) {
        let f = fs::OpenOptions::new().write(true).open(p).unwrap();
        f.set_modified(SystemTime::now() + Duration::from_secs(5)).unwrap();
    }

    #[test]
    fn real_files_follow_the_table() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("a.txt");
        fs::write(&p, "one").unwrap();
        // Never noted: nothing to compare with, saving is allowed.
        assert_eq!(save_gate(&p, "mine", &SPEC), SaveGate::Write);
        note(&p);
        assert_eq!(disk_state(&p), Disk::Unchanged);
        assert_eq!(save_gate(&p, "mine", &SPEC), SaveGate::Write);

        // Another program changes it (same size, newer mtime).
        fs::write(&p, "two").unwrap();
        set_mtime_ahead(&p);
        assert_eq!(disk_state(&p), Disk::Changed);
        assert_eq!(save_gate(&p, "mine", &SPEC), SaveGate::Conflict);
        assert_eq!(fs::read_to_string(&p).unwrap(), "two", "the gate never writes");
        // Our text equals what is on disk: no conflict.
        assert_eq!(save_gate(&p, "two", &SPEC), SaveGate::Write);

        // Dismissed: no banner, but the save gate still guards.
        dismiss(&p);
        assert_eq!(disk_state_for_banner(&p), Disk::Unchanged);
        assert_eq!(save_gate(&p, "mine", &SPEC), SaveGate::Conflict);

        // Accepting the disk state (reload / overwrite choice) re-syncs.
        note(&p);
        assert_eq!(save_gate(&p, "mine", &SPEC), SaveGate::Write);

        // Deleted.
        fs::remove_file(&p).unwrap();
        assert_eq!(disk_state(&p), Disk::Gone);
        assert_eq!(save_gate(&p, "mine", &SPEC), SaveGate::Write);
        forget(&p);
        assert_eq!(disk_state(&p), Disk::Unchanged);
    }

    #[test]
    fn pieces_gate_equals_whole_text_gate() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("pieces.txt");
        fs::write(&p, "one two three").unwrap();
        note(&p);
        fs::write(&p, "one two three").unwrap();
        set_mtime_ahead(&p);
        assert_eq!(disk_state(&p), Disk::Changed);
        let pieces = |s: &'static str| move || s.split(' ').map(|w| w).collect::<Vec<_>>().into_iter();
        // Pieces glued without the spaces differ from the disk; the real split text is equal only if rejoined.
        assert_eq!(save_gate_pieces(&p, pieces("one two three"), &SPEC), SaveGate::Conflict);
        assert_eq!(save_gate_pieces(&p, || ["one t", "wo thr", "ee"].into_iter(), &SPEC), SaveGate::Write);
        assert_eq!(save_gate_pieces(&p, || ["one t", "wo thr", "eX"].into_iter(), &SPEC), SaveGate::Conflict);
        assert_eq!(save_gate_pieces(&p, || ["one t", "wo thr"].into_iter(), &SPEC), SaveGate::Conflict, "shorter");
        assert_eq!(save_gate_pieces(&p, || ["one t", "wo three", "!"].into_iter(), &SPEC), SaveGate::Conflict, "longer");
        for text in ["one two three", "one two threX", "one two", "one two three!"] {
            assert_eq!(save_gate(&p, text, &SPEC), if text == "one two three" { SaveGate::Write } else { SaveGate::Conflict }, "{text}");
        }
    }

    #[test]
    fn different_size_is_decided_without_reading() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("big.txt");
        fs::write(&p, "x").unwrap();
        note(&p);
        fs::write(&p, "xxxxxxxx").unwrap();
        set_mtime_ahead(&p);
        assert_eq!(save_gate(&p, "short", &SPEC), SaveGate::Conflict);
    }
}
