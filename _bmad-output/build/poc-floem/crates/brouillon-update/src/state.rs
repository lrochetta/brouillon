//! Persistent state (`<data>/update-state.toml`): replay guard, backoff, install attempt marker.

use serde::{Deserialize, Serialize};
use std::path::Path;

pub const BACKOFF_FIRST_SECS: i64 = 24 * 3600;
pub const BACKOFF_NEXT_SECS: i64 = 7 * 24 * 3600;

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct UpdateState {
    /// `published_utc` (epoch) of the last manifest that passed every check.
    pub last_published_epoch: i64,
    pub fail_count: u32,
    /// No network attempt before this instant (epoch seconds).
    pub next_attempt_epoch: i64,
    /// Installer launched for this build (0 = none). Used to detect success or failure at next start.
    pub attempt_build: u64,
    pub attempt_version: String,
    /// Signed-manifest facts of the launched installer, to explain a failure later.
    pub attempt_file: String,
    pub attempt_sha256: String,
    pub attempt_url: String,
    /// Build whose automatic install failed: never retried automatically (no loop).
    pub failed_build: u64,
    pub failed_version: String,
    pub failed_file: String,
    pub failed_sha256: String,
    pub failed_url: String,
    /// Last build the user was told about in notify-only mode (avoid repeating).
    pub notified_build: u64,
    /// Last time a check was started (epoch seconds): the `check_interval_hours` throttle.
    pub last_check_epoch: i64,
    /// Build the user chose to skip ("Skip this version"); a mandatory update ignores it.
    pub skipped_build: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AttemptOutcome {
    /// We are now running the build that was installed.
    Succeeded { version: String },
    /// The installer ran (or was killed) but the build did not change.
    Failed { version: String },
}

impl UpdateState {
    /// A missing or unreadable file gives the default state (never blocks startup).
    pub fn load(path: &Path) -> Self {
        std::fs::read_to_string(path).ok().and_then(|t| toml::from_str(&t).ok()).unwrap_or_default()
    }

    pub fn save(&self, path: &Path, write: &dyn Fn(&Path, &[u8]) -> std::io::Result<()>) -> std::io::Result<()> {
        let text = toml::to_string(self).map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e))?;
        write(path, text.as_bytes())
    }

    pub fn may_attempt(&self, now: i64) -> bool {
        now >= self.next_attempt_epoch
    }

    /// Throttle: at most one check per `interval_hours` (0 = every launch). A clock that went back
    /// (`now < last_check`) does not block checking forever.
    pub fn check_due(&self, now: i64, interval_hours: u64) -> bool {
        let wait = (interval_hours.min(24 * 365) as i64) * 3600;
        now < self.last_check_epoch || now - self.last_check_epoch >= wait
    }

    pub fn record_failure(&mut self, now: i64) {
        self.fail_count = self.fail_count.saturating_add(1);
        let wait = if self.fail_count <= 1 { BACKOFF_FIRST_SECS } else { BACKOFF_NEXT_SECS };
        self.next_attempt_epoch = now + wait;
    }

    pub fn record_ok(&mut self) {
        self.fail_count = 0;
        self.next_attempt_epoch = 0;
    }

    pub fn record_attempt(&mut self, build: u64, version: &str) {
        self.attempt_build = build;
        self.attempt_version = version.to_string();
    }

    pub fn record_attempt_files(&mut self, file: &str, sha256: &str, url: &str) {
        self.attempt_file = file.to_string();
        self.attempt_sha256 = sha256.to_string();
        self.attempt_url = url.to_string();
    }

    /// The installer failed (seen at once, or at the next start): remember this build as failed.
    pub fn mark_failed_now(&mut self, now: i64) {
        self.failed_build = self.attempt_build;
        self.failed_version = std::mem::take(&mut self.attempt_version);
        self.failed_file = std::mem::take(&mut self.attempt_file);
        self.failed_sha256 = std::mem::take(&mut self.attempt_sha256);
        self.failed_url = std::mem::take(&mut self.attempt_url);
        self.attempt_build = 0;
        self.record_failure(now);
    }

    /// Called at startup: did the last launched installer take effect? Clears the marker.
    /// A failure also arms the backoff, so a broken install can never loop.
    pub fn resolve_attempt(&mut self, current_build: u64, now: i64) -> Option<AttemptOutcome> {
        if self.attempt_build == 0 {
            return None;
        }
        let version = std::mem::take(&mut self.attempt_version);
        let target = std::mem::take(&mut self.attempt_build);
        if current_build >= target {
            self.record_ok();
            Some(AttemptOutcome::Succeeded { version })
        } else {
            self.attempt_build = target;
            self.attempt_version = version.clone();
            self.mark_failed_now(now);
            Some(AttemptOutcome::Failed { version })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backoff_is_24h_then_7d() {
        let mut s = UpdateState::default();
        assert!(s.may_attempt(0));
        s.record_failure(1000);
        assert_eq!(s.next_attempt_epoch, 1000 + 86400);
        assert!(!s.may_attempt(1000 + 86399) && s.may_attempt(1000 + 86400));
        s.record_failure(5000);
        assert_eq!(s.next_attempt_epoch, 5000 + 7 * 86400);
        s.record_failure(9000);
        assert_eq!(s.next_attempt_epoch, 9000 + 7 * 86400);
        s.record_ok();
        assert!(s.may_attempt(0));
    }

    #[test]
    fn throttle_by_interval() {
        let mut s = UpdateState::default();
        assert!(s.check_due(1_000_000, 24), "never checked");
        s.last_check_epoch = 1_000_000;
        assert!(!s.check_due(1_000_000 + 24 * 3600 - 1, 24));
        assert!(s.check_due(1_000_000 + 24 * 3600, 24));
        assert!(s.check_due(1_000_000, 0), "0 = every launch");
        assert!(s.check_due(999_000, 24), "clock moved back");
    }

    #[test]
    fn attempt_marker_success_and_failure() {
        let mut s = UpdateState::default();
        assert_eq!(s.resolve_attempt(80, 0), None);
        s.record_attempt(90, "0.9.0");
        assert_eq!(s.resolve_attempt(90, 10), Some(AttemptOutcome::Succeeded { version: "0.9.0".into() }));
        assert_eq!(s.attempt_build, 0);
        s.record_attempt(91, "0.9.1");
        assert_eq!(s.resolve_attempt(90, 10), Some(AttemptOutcome::Failed { version: "0.9.1".into() }));
        assert_eq!(s.next_attempt_epoch, 10 + 86400, "a failed install arms the backoff");
        assert_eq!(s.resolve_attempt(90, 10), None, "marker cleared: no loop");
        assert_eq!(s.failed_build, 91, "the failed build is remembered: never retried automatically");
        assert_eq!(s.failed_version, "0.9.1");
    }

    #[test]
    fn roundtrip_and_corrupt_file() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("update-state.toml");
        let mut s = UpdateState::default();
        s.last_published_epoch = 1234;
        s.record_failure(1);
        s.save(&p, &|path, b| std::fs::write(path, b)).unwrap();
        assert_eq!(UpdateState::load(&p), s);
        std::fs::write(&p, "{{{ not toml").unwrap();
        assert_eq!(UpdateState::load(&p), UpdateState::default());
        assert_eq!(UpdateState::load(&d.path().join("missing.toml")), UpdateState::default());
    }
}
