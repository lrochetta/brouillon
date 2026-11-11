//! Lot 4 — Replace in Files (BK-025, decision D7).
//!
//! Flow: `build_plan` (re-reads files, computes every replacement, nothing is
//! written) -> UI shows `FilePlan::preview_diff` with per-file / per-hit
//! checkboxes -> `apply_plan(.., ApplyMode::Confirmed, ..)`. A `DryRun`
//! changes nothing on disk. Each file is backed up through a [`BackupSink`]
//! first, then written via temp file + rename. One failing file never aborts
//! the others; everything is reported in a [`ReplaceReport`].
//!
//! Hits that came from an unsaved open buffer are never written here: the
//! shell must apply them to the buffer (Lot 3 / Lot 1 `EditSet` path).

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

use brouillon_core::contract::{EditSet, TextEdit};

use crate::search_files::{decode_bytes, FileResult, TextMatcher};

/// Backup request issued before a file is overwritten (D7). Lot 7's backup
/// manager can implement this; [`CopyBackup`] is the built-in fallback.
pub trait BackupSink {
    fn backup(&self, path: &Path) -> io::Result<()>;
}

/// Copies each file into `dir` under a unique name before it is modified.
pub struct CopyBackup {
    pub dir: PathBuf,
    counter: AtomicUsize,
}

impl CopyBackup {
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        Self { dir: dir.into(), counter: AtomicUsize::new(0) }
    }
}

impl BackupSink for CopyBackup {
    fn backup(&self, path: &Path) -> io::Result<()> {
        std::fs::create_dir_all(&self.dir)?;
        let n = self.counter.fetch_add(1, Ordering::Relaxed);
        let ts = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_millis());
        let name = path.file_name().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
        std::fs::copy(path, self.dir.join(format!("{ts}-{n}-{name}.bak"))).map(|_| ())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlanItem {
    pub checked: bool,
    pub line: usize,
    pub start_byte: usize,
    pub end_byte: usize,
    pub matched: String,
    pub replacement: String,
    /// Full line before / after this single replacement (no EOL).
    pub before: String,
    pub after: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FilePlan {
    pub path: PathBuf,
    pub items: Vec<PlanItem>,
    /// `Some(reason)` = this file cannot be written (unsaved buffer, changed, unreadable).
    pub skip: Option<String>,
    fingerprint: (usize, u64),
}

fn fingerprint(bytes: &[u8]) -> (usize, u64) {
    let mut h = DefaultHasher::new();
    bytes.hash(&mut h);
    (bytes.len(), h.finish())
}

impl FilePlan {
    pub fn checked_count(&self) -> usize {
        self.items.iter().filter(|i| i.checked).count()
    }

    pub fn set_all(&mut self, checked: bool) {
        self.items.iter_mut().for_each(|i| i.checked = checked);
    }

    pub fn toggle(&mut self, index: usize) {
        if let Some(i) = self.items.get_mut(index) {
            i.checked = !i.checked;
        }
    }

    /// Edits of the checked items, in original coordinates.
    pub fn edit_set(&self) -> EditSet {
        EditSet(
            self.items
                .iter()
                .filter(|i| i.checked)
                .map(|i| TextEdit::new(i.start_byte, i.end_byte, i.replacement.clone()))
                .collect(),
        )
    }

    /// Human-readable diff of the checked items (1-based line numbers).
    pub fn preview_diff(&self) -> String {
        let mut out = format!("--- {}\n", self.path.display());
        for i in self.items.iter().filter(|i| i.checked) {
            out.push_str(&format!("@@ line {} @@\n- {}\n+ {}\n", i.line + 1, i.before, i.after));
        }
        out
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ReplacePlan {
    pub files: Vec<FilePlan>,
}

impl ReplacePlan {
    pub fn total_checked(&self) -> usize {
        self.files.iter().filter(|f| f.skip.is_none()).map(FilePlan::checked_count).sum()
    }
}

/// Compute (but do not write) the replacement for every hit.
pub fn build_plan(results: &[FileResult], matcher: &dyn TextMatcher) -> ReplacePlan {
    let files = results.iter().map(|r| plan_file(r, matcher)).collect();
    ReplacePlan { files }
}

fn plan_file(r: &FileResult, matcher: &dyn TextMatcher) -> FilePlan {
    let mut plan = FilePlan { path: r.path.clone(), items: Vec::new(), skip: None, fingerprint: (0, 0) };
    if r.from_buffer {
        plan.skip = Some("unsaved open buffer: replace inside the editor".into());
        return plan;
    }
    let Ok(bytes) = std::fs::read(&r.path) else {
        plan.skip = Some("cannot read file".into());
        return plan;
    };
    plan.fingerprint = fingerprint(&bytes);
    let Some(dec) = decode_bytes(&bytes) else {
        plan.skip = Some("binary or undecodable file".into());
        return plan;
    };
    let text = &dec.text;
    for h in &r.hits {
        if text.get(h.start_byte..h.end_byte) != Some(h.matched.as_str()) {
            plan.items.clear();
            plan.skip = Some("file changed since the search".into());
            return plan;
        }
        let range = h.start_byte..h.end_byte;
        let replacement = matcher.replacement(text, &range);
        let ls = text[..h.start_byte].rfind('\n').map_or(0, |i| i + 1);
        let le = text[h.end_byte..].find('\n').map_or(text.len(), |i| h.end_byte + i);
        let after = format!("{}{}{}", &text[ls..h.start_byte], replacement, &text[h.end_byte..le]);
        plan.items.push(PlanItem {
            checked: true,
            line: h.line,
            start_byte: h.start_byte,
            end_byte: h.end_byte,
            matched: h.matched.clone(),
            replacement,
            before: h.line_text.clone(),
            after: after.trim_end_matches('\r').to_string(),
        });
    }
    plan
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApplyMode {
    /// Run every check, write nothing, back nothing up.
    DryRun,
    /// The user explicitly confirmed: write the checked hits.
    Confirmed,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FileStatus {
    Applied(usize),
    WouldApply(usize),
    Skipped(String),
    Failed(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReplaceReport {
    pub files: Vec<(PathBuf, FileStatus)>,
}

impl ReplaceReport {
    pub fn replacements(&self) -> usize {
        self.files
            .iter()
            .map(|(_, s)| match s {
                FileStatus::Applied(n) | FileStatus::WouldApply(n) => *n,
                _ => 0,
            })
            .sum()
    }
    pub fn failures(&self) -> usize {
        self.files.iter().filter(|(_, s)| matches!(s, FileStatus::Failed(_))).count()
    }
    /// One-line summary for the status bar.
    pub fn summary(&self) -> String {
        let done = self.files.iter().filter(|(_, s)| matches!(s, FileStatus::Applied(_) | FileStatus::WouldApply(_))).count();
        let skipped = self.files.iter().filter(|(_, s)| matches!(s, FileStatus::Skipped(_))).count();
        format!("{} replacement(s) in {} file(s), {} skipped, {} failed", self.replacements(), done, skipped, self.failures())
    }
}

/// Apply the checked hits of every file. Nothing is written unless
/// `mode == Confirmed`.
pub fn apply_plan(plan: &ReplacePlan, mode: ApplyMode, backup: &dyn BackupSink) -> ReplaceReport {
    let files = plan
        .files
        .iter()
        .map(|f| {
            let status = match apply_file(f, mode, backup) {
                Ok(s) => s,
                Err(e) => FileStatus::Failed(e),
            };
            (f.path.clone(), status)
        })
        .collect();
    ReplaceReport { files }
}

fn apply_file(f: &FilePlan, mode: ApplyMode, backup: &dyn BackupSink) -> Result<FileStatus, String> {
    if let Some(reason) = &f.skip {
        return Ok(FileStatus::Skipped(reason.clone()));
    }
    let n = f.checked_count();
    if n == 0 {
        return Ok(FileStatus::Skipped("nothing selected".into()));
    }
    let bytes = std::fs::read(&f.path).map_err(|e| format!("read: {e}"))?;
    if fingerprint(&bytes) != f.fingerprint {
        return Err("file changed on disk since the preview".into());
    }
    let dec = decode_bytes(&bytes).ok_or("undecodable file")?;
    let new_text = f.edit_set().apply_to_str(&dec.text).map_err(|e| e.to_string())?;
    let new_bytes = dec.encode(&new_text)?;
    if mode == ApplyMode::DryRun {
        return Ok(FileStatus::WouldApply(n));
    }
    backup.backup(&f.path).map_err(|e| format!("backup failed, file untouched: {e}"))?;
    write_atomic(&f.path, &new_bytes).map_err(|e| format!("write: {e}"))?;
    Ok(FileStatus::Applied(n))
}

fn write_atomic(path: &Path, bytes: &[u8]) -> io::Result<()> {
    crate::durable::durable_write(path, bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::search_files::{search_files_collect, CancelToken, FindOptions, RegexMatcher};
    use std::fs;

    struct NoBackup;
    impl BackupSink for NoBackup {
        fn backup(&self, _: &Path) -> io::Result<()> {
            Ok(())
        }
    }
    struct FailBackup;
    impl BackupSink for FailBackup {
        fn backup(&self, _: &Path) -> io::Result<()> {
            Err(io::Error::new(io::ErrorKind::Other, "disk full"))
        }
    }

    fn setup() -> (tempfile::TempDir, RegexMatcher, ReplacePlan) {
        let d = tempfile::tempdir().unwrap();
        fs::write(d.path().join("a.txt"), "foo 1\r\nfoo 2\r\n").unwrap();
        fs::write(d.path().join("b.txt"), b"\xEF\xBB\xBFfoo bar\n").unwrap();
        fs::write(d.path().join("c.txt"), "nothing\n").unwrap();
        let m = RegexMatcher::new(r"foo (\w+)", true, true, false).unwrap().with_replacement("bar $1!");
        let (res, _) = search_files_collect(&FindOptions::new(d.path()), &m, &CancelToken::new()).unwrap();
        let plan = build_plan(&res, &m);
        (d, m, plan)
    }

    fn read(d: &tempfile::TempDir, n: &str) -> Vec<u8> {
        fs::read(d.path().join(n)).unwrap()
    }

    #[test]
    fn dry_run_changes_nothing() {
        let (d, _, plan) = setup();
        assert_eq!(plan.total_checked(), 3);
        let rep = apply_plan(&plan, ApplyMode::DryRun, &FailBackup);
        assert_eq!(rep.replacements(), 3);
        assert_eq!(read(&d, "a.txt"), b"foo 1\r\nfoo 2\r\n");
        assert_eq!(read(&d, "b.txt"), b"\xEF\xBB\xBFfoo bar\n");
    }

    #[test]
    fn apply_only_checked_preserves_eol_and_bom() {
        let (d, _, mut plan) = setup();
        plan.files[0].toggle(1); // a.txt: keep "foo 2"
        let diff = plan.files[0].preview_diff();
        assert!(diff.contains("- foo 1") && diff.contains("+ bar 1!") && !diff.contains("foo 2"));
        let rep = apply_plan(&plan, ApplyMode::Confirmed, &NoBackup);
        assert_eq!(rep.failures(), 0);
        assert_eq!(read(&d, "a.txt"), b"bar 1!\r\nfoo 2\r\n");
        assert_eq!(read(&d, "b.txt"), b"\xEF\xBB\xBFbar bar!\n");
        assert_eq!(rep.replacements(), 2);
        assert!(rep.summary().contains("2 replacement(s) in 2 file(s)"));
    }

    #[test]
    fn one_failure_does_not_abort_and_backup_is_made() {
        let (d, _, plan) = setup();
        fs::write(d.path().join("a.txt"), "changed meanwhile foo 9\n").unwrap();
        let bdir = d.path().join("bk");
        let rep = apply_plan(&plan, ApplyMode::Confirmed, &CopyBackup::new(&bdir));
        assert_eq!(rep.failures(), 1);
        assert_eq!(read(&d, "a.txt"), b"changed meanwhile foo 9\n");
        assert_eq!(read(&d, "b.txt"), b"\xEF\xBB\xBFbar bar!\n");
        let backups: Vec<_> = fs::read_dir(&bdir).unwrap().collect();
        assert_eq!(backups.len(), 1);
        assert_eq!(fs::read(backups[0].as_ref().unwrap().path()).unwrap(), b"\xEF\xBB\xBFfoo bar\n");
    }

    #[test]
    fn backup_failure_leaves_file_untouched() {
        let (d, _, plan) = setup();
        let rep = apply_plan(&plan, ApplyMode::Confirmed, &FailBackup);
        assert_eq!(rep.failures(), 2);
        assert_eq!(read(&d, "a.txt"), b"foo 1\r\nfoo 2\r\n");
    }

    #[test]
    fn unsaved_buffer_hits_are_skipped() {
        let d = tempfile::tempdir().unwrap();
        fs::write(d.path().join("x.txt"), "foo 1\n").unwrap();
        let m = RegexMatcher::new("foo", false, true, false).unwrap().with_replacement("bar");
        let mut o = FindOptions::new(d.path());
        o.buffers.insert(d.path().join("x.txt"), "foo 1\nfoo 2\n".into());
        let (res, _) = search_files_collect(&o, &m, &CancelToken::new()).unwrap();
        let plan = build_plan(&res, &m);
        let rep = apply_plan(&plan, ApplyMode::Confirmed, &NoBackup);
        assert!(matches!(rep.files[0].1, FileStatus::Skipped(_)));
        assert_eq!(fs::read(d.path().join("x.txt")).unwrap(), b"foo 1\n");
    }

    #[test]
    fn edit_set_matches_checked_items() {
        let (_, _, mut plan) = setup();
        plan.files[0].set_all(false);
        assert!(plan.files[0].edit_set().is_empty());
        plan.files[0].toggle(0);
        let es = plan.files[0].edit_set();
        assert_eq!(es.apply_to_str("foo 1\r\nfoo 2\r\n").unwrap(), "bar 1!\r\nfoo 2\r\n");
    }
}
