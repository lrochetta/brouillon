//! Decisions: is the manifest acceptable, and when do we apply the update.

use crate::manifest::Manifest;
use crate::state::UpdateState;
use crate::timeutil::parse_utc;
use crate::UpdateError;

/// A manifest dated more than this far ahead of the local clock is refused.
pub const MAX_FUTURE_SKEW_SECS: i64 = 24 * 3600;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Offer {
    /// `build` strictly greater than ours.
    Newer,
    UpToDate,
    /// `build` lower than ours: ignored (rollback protection).
    DowngradeRefused,
}

/// Replay and downgrade protection. `Err` = reject the manifest, `Ok` = accepted
/// (the caller then stores `published_utc` as the new replay floor).
pub fn evaluate(m: &Manifest, current_build: u64, state: &UpdateState, now: i64) -> Result<Offer, UpdateError> {
    let published = parse_utc(&m.published_utc).ok_or_else(|| UpdateError::BadManifest("bad date".into()))?;
    if published < state.last_published_epoch {
        return Err(UpdateError::Replay);
    }
    if published > now + MAX_FUTURE_SKEW_SECS {
        return Err(UpdateError::FutureDated);
    }
    Ok(if m.build > current_build {
        Offer::Newer
    } else if m.build == current_build {
        Offer::UpToDate
    } else {
        Offer::DowngradeRefused
    })
}

pub fn is_mandatory(m: &Manifest, current_build: u64) -> bool {
    m.min_supported_build > current_build
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    /// Tell the user a new version exists; install nothing.
    Notify,
    /// Nothing is modified: install right away.
    InstallNow,
    /// Something is modified: keep working, install at normal exit (journal already holds the work).
    InstallOnExit,
    /// `min_supported_build` above ours: countdown banner, save everything, install.
    Mandatory,
}

/// Decision table (unit-tested exhaustively).
/// * `auto`: `updates.auto_install`. `can_install`: installed Windows copy (not portable, not other OS).
/// * `journal_ok`: unsaved work can be journaled (false with `--no-session`): then a modified
///   document forbids any automatic install, because nothing would protect it.
pub fn decide(auto: bool, can_install: bool, journal_ok: bool, dirty: bool, mandatory: bool) -> Action {
    if !auto || !can_install {
        return Action::Notify;
    }
    if dirty && !journal_ok {
        return Action::Notify;
    }
    if mandatory {
        Action::Mandatory
    } else if dirty {
        Action::InstallOnExit
    } else {
        Action::InstallNow
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::manifest::testkit::manifest_json;

    fn m(build: u64, published: &str, min: u64) -> Manifest {
        serde_json::from_str(&manifest_json(build, published, "https://brouillon.rochetta.fr/downloads/a.exe", "a.exe", 10, &"0".repeat(64), min)).unwrap()
    }
    const NOW: i64 = 1_793_889_130; // 2026-11-05T14:32:10Z

    #[test]
    fn newer_same_older_build() {
        let s = UpdateState::default();
        assert_eq!(evaluate(&m(90, "2026-11-05T14:32:10Z", 0), 83, &s, NOW), Ok(Offer::Newer));
        assert_eq!(evaluate(&m(83, "2026-11-05T14:32:10Z", 0), 83, &s, NOW), Ok(Offer::UpToDate));
        assert_eq!(evaluate(&m(82, "2026-11-05T14:32:10Z", 0), 83, &s, NOW), Ok(Offer::DowngradeRefused));
    }

    #[test]
    fn replay_refused_equal_accepted() {
        let mut s = UpdateState::default();
        s.last_published_epoch = NOW;
        assert_eq!(evaluate(&m(90, "2026-11-05T14:32:09Z", 0), 83, &s, NOW), Err(UpdateError::Replay));
        assert_eq!(evaluate(&m(90, "2026-11-05T14:32:10Z", 0), 83, &s, NOW), Ok(Offer::Newer));
    }

    #[test]
    fn far_future_refused_small_skew_accepted() {
        let s = UpdateState::default();
        assert_eq!(evaluate(&m(90, "2026-11-07T14:32:10Z", 0), 83, &s, NOW), Err(UpdateError::FutureDated));
        assert_eq!(evaluate(&m(90, "2026-11-05T20:00:00Z", 0), 83, &s, NOW), Ok(Offer::Newer));
    }

    #[test]
    fn mandatory_only_when_min_supported_above_current() {
        assert!(is_mandatory(&m(90, "2026-11-05T14:32:10Z", 84), 83));
        assert!(!is_mandatory(&m(90, "2026-11-05T14:32:10Z", 83), 83));
        assert!(!is_mandatory(&m(90, "2026-11-05T14:32:10Z", 0), 83));
    }

    #[test]
    fn decision_table() {
        use Action::*;
        // (auto, can_install, journal_ok, dirty, mandatory) -> action
        let rows = [
            (true, true, true, false, false, InstallNow),
            (true, true, true, true, false, InstallOnExit),
            (true, true, true, false, true, Mandatory),
            (true, true, true, true, true, Mandatory),
            (false, true, true, false, false, Notify),
            (false, true, true, true, true, Notify),
            (true, false, true, false, false, Notify), // portable / non-Windows
            (true, false, true, false, true, Notify),
            (true, true, false, true, false, Notify), // --no-session with unsaved work: never auto
            (true, true, false, true, true, Notify),
            (true, true, false, false, false, InstallNow), // nothing to lose
        ];
        for (a, c, j, d, mand, want) in rows {
            assert_eq!(decide(a, c, j, d, mand), want, "auto={a} can={c} journal={j} dirty={d} mandatory={mand}");
        }
    }
}
