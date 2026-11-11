//! BK-046 — backup-on-save and atomic write.
//!
//! `save_with_backup` copies the previous bytes to a `.bak` file (options:
//! same dir / dedicated dir / dated names, with count and age caps), then
//! replaces the target atomically (temp file in the same dir + fsync + rename).
//! A failure at any point leaves the original file untouched.

use std::fmt;
use std::fs::{self, File};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// Backup naming / retention options. `max_count` / `max_age_days` of 0 mean
/// "unlimited". Caps only matter for dated backups (plain `.bak` is overwritten).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct BackupOptions {
    pub enabled: bool,
    /// Directory for backups; `None` = next to the original.
    pub dir: Option<PathBuf>,
    /// `name.ext.YYYYMMDD-HHMMSS.bak` (UTC) instead of `name.ext.bak`.
    pub dated: bool,
    pub max_count: usize,
    pub max_age_days: u64,
}

/// Save failure with a message suitable for a failure dialog.
#[derive(Debug)]
pub enum SaveError {
    ReadOnly(PathBuf),
    Backup { path: PathBuf, source: io::Error },
    Write { path: PathBuf, source: io::Error },
}

impl fmt::Display for SaveError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SaveError::ReadOnly(p) => write!(f, "{} is read-only; nothing was written.", p.display()),
            SaveError::Backup { path, source } => write!(
                f,
                "Could not create the backup for {} ({source}); the original is unchanged.",
                path.display()
            ),
            SaveError::Write { path, source } => write!(
                f,
                "Could not save {} ({source}); the original is unchanged.",
                path.display()
            ),
        }
    }
}

impl std::error::Error for SaveError {}

/// Write via `fill` into a temp file next to `path`, fsync, then rename over
/// `path` (see `durable::durable_write_with`). If `fill` or any step fails the
/// temp file is removed and `path` keeps its previous content.
pub fn atomic_write_with<F>(path: &Path, fill: F) -> io::Result<()>
where
    F: FnMut(&mut File) -> io::Result<()>,
{
    crate::durable::durable_write_with(path, fill)
}

/// Atomically replace `path` with `bytes`.
pub fn atomic_write(path: &Path, bytes: &[u8]) -> io::Result<()> {
    crate::durable::durable_write(path, bytes)
}

/// Save `bytes` to `path`, first backing up the previous content when
/// `opts.enabled` and the file exists. Returns the backup path if one was made.
pub fn save_with_backup(
    path: &Path,
    bytes: &[u8],
    opts: &BackupOptions,
) -> Result<Option<PathBuf>, SaveError> {
    save_with_backup_with(path, opts, |f| f.write_all(bytes))
}

/// Same as [`save_with_backup`] with the content produced by `fill` straight into the temp file
/// (restartable: it may run twice if the in-place fallback kicks in). Atomic-write semantics unchanged.
pub fn save_with_backup_with<F>(path: &Path, opts: &BackupOptions, fill: F) -> Result<Option<PathBuf>, SaveError>
where
    F: FnMut(&mut File) -> io::Result<()>,
{
    if let Ok(meta) = fs::metadata(path) {
        if meta.permissions().readonly() {
            return Err(SaveError::ReadOnly(path.to_path_buf()));
        }
    }
    let made = make_backup(path, opts)?;
    atomic_write_with(path, fill).map_err(|source| SaveError::Write { path: path.to_path_buf(), source })?;
    Ok(made)
}

/// The backup half of a save: copy the current content of `path` aside (when backups are enabled and the
/// file exists) and prune old dated copies. `Ok(None)` = nothing to back up.
pub fn make_backup(path: &Path, opts: &BackupOptions) -> Result<Option<PathBuf>, SaveError> {
    if !(opts.enabled && path.is_file()) {
        return Ok(None);
    }
    let bak = backup_path(path, opts, SystemTime::now());
    let copy = || -> io::Result<()> {
        if let Some(parent) = bak.parent() {
            fs::create_dir_all(parent)?;
        }
        crate::durable::durable_write_with(&bak, |out| io::copy(&mut File::open(path)?, out).map(|_| ()))
    };
    copy().map_err(|source| SaveError::Backup { path: path.to_path_buf(), source })?;
    let _ = prune_backups(path, opts, SystemTime::now());
    Ok(Some(bak))
}

/// Where the backup of `path` goes under `opts` at time `now`.
pub fn backup_path(path: &Path, opts: &BackupOptions, now: SystemTime) -> PathBuf {
    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let file = if opts.dated {
        format!("{name}.{}.bak", stamp(now))
    } else {
        format!("{name}.bak")
    };
    match &opts.dir {
        Some(d) => d.join(file),
        None => path.with_file_name(file),
    }
}

/// Delete dated backups of `path` beyond the age / count caps. Returns the
/// number of files removed.
pub fn prune_backups(path: &Path, opts: &BackupOptions, now: SystemTime) -> io::Result<usize> {
    if !opts.dated {
        return Ok(0);
    }
    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let dir = match &opts.dir {
        Some(d) => d.clone(),
        None => path.parent().map(Path::to_path_buf).unwrap_or_else(|| PathBuf::from(".")),
    };
    let prefix = format!("{name}.");
    // Dated names sort chronologically; the plain `name.bak` has no stamp, skip it.
    let mut baks: Vec<(String, PathBuf)> = fs::read_dir(&dir)?
        .filter_map(|e| e.ok())
        .filter_map(|e| {
            let n = e.file_name().to_string_lossy().into_owned();
            let mid = n.strip_prefix(&prefix)?.strip_suffix(".bak")?;
            (mid.len() == 15 && mid.as_bytes()[8] == b'-').then(|| (mid.to_string(), e.path()))
        })
        .collect();
    baks.sort();
    let mut removed = 0;
    if opts.max_age_days > 0 {
        let cutoff = stamp(now - Duration::from_secs(opts.max_age_days * 86_400));
        baks.retain(|(s, p)| {
            if *s < cutoff && fs::remove_file(p).is_ok() {
                removed += 1;
                false
            } else {
                true
            }
        });
    }
    if opts.max_count > 0 && baks.len() > opts.max_count {
        for (_, p) in &baks[..baks.len() - opts.max_count] {
            if fs::remove_file(p).is_ok() {
                removed += 1;
            }
        }
    }
    Ok(removed)
}

/// `YYYYMMDD-HHMMSS` in UTC.
fn stamp(t: SystemTime) -> String {
    let secs = t.duration_since(UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0);
    let (days, rem) = (secs.div_euclid(86_400), secs.rem_euclid(86_400));
    // Civil-from-days (proleptic Gregorian).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!("{y:04}{m:02}{d:02}-{:02}{:02}{:02}", rem / 3600, rem % 3600 / 60, rem % 60)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn on() -> BackupOptions {
        BackupOptions { enabled: true, ..Default::default() }
    }

    #[test]
    fn backup_holds_previous_bytes() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("a.txt");
        fs::write(&p, "old").unwrap();
        let bak = save_with_backup(&p, b"new", &on()).unwrap().unwrap();
        assert_eq!(fs::read_to_string(&p).unwrap(), "new");
        assert_eq!(fs::read_to_string(bak).unwrap(), "old");
    }

    #[test]
    fn no_backup_when_disabled_or_new_file() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("a.txt");
        assert!(save_with_backup(&p, b"x", &on()).unwrap().is_none());
        assert!(save_with_backup(&p, b"y", &BackupOptions::default()).unwrap().is_none());
        assert!(!d.path().join("a.txt.bak").exists());
    }

    #[test]
    fn failed_write_leaves_original_intact() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("a.txt");
        fs::write(&p, "original").unwrap();
        let r = atomic_write_with(&p, |f| {
            f.write_all(b"half")?;
            Err(io::Error::other("killed midway"))
        });
        assert!(r.is_err());
        assert_eq!(fs::read_to_string(&p).unwrap(), "original");
        assert_eq!(fs::read_dir(d.path()).unwrap().count(), 1, "temp file cleaned up");
    }

    #[test]
    fn backup_dir_option() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("a.txt");
        fs::write(&p, "old").unwrap();
        let opts = BackupOptions { dir: Some(d.path().join("bk")), ..on() };
        let bak = save_with_backup(&p, b"new", &opts).unwrap().unwrap();
        assert_eq!(bak, d.path().join("bk").join("a.txt.bak"));
    }

    #[test]
    fn readonly_target_is_reported() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("a.txt");
        fs::write(&p, "old").unwrap();
        let mut perm = fs::metadata(&p).unwrap().permissions();
        perm.set_readonly(true);
        fs::set_permissions(&p, perm.clone()).unwrap();
        let r = save_with_backup(&p, b"new", &on());
        perm.set_readonly(false);
        fs::set_permissions(&p, perm).unwrap();
        assert!(matches!(r, Err(SaveError::ReadOnly(_))));
        assert_eq!(fs::read_to_string(&p).unwrap(), "old");
    }

    #[test]
    fn stamp_known_date() {
        assert_eq!(stamp(UNIX_EPOCH + Duration::from_secs(1_700_000_000)), "20231114-221320");
    }

    #[test]
    fn dated_backups_pruned_by_count_and_age() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("a.txt");
        fs::write(&p, "x").unwrap();
        for s in ["20200101-000000", "20200102-000000", "20200103-000000"] {
            fs::write(d.path().join(format!("a.txt.{s}.bak")), "b").unwrap();
        }
        let opts = BackupOptions { dated: true, max_count: 2, ..on() };
        let now = UNIX_EPOCH + Duration::from_secs(1_580_000_000); // 2020-01-26
        assert_eq!(prune_backups(&p, &opts, now).unwrap(), 1);
        assert!(!d.path().join("a.txt.20200101-000000.bak").exists());
        let opts = BackupOptions { dated: true, max_age_days: 24, ..on() };
        assert_eq!(prune_backups(&p, &opts, now).unwrap(), 1); // 01-02 is older than 24 d
        assert!(d.path().join("a.txt.20200103-000000.bak").exists());
    }
}
