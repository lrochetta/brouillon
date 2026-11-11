//! Small value types shared by the tab chip, the status-bar chip and the (separate) save engine.
//!
//! The chip is a PURE VIEW of [`RemoteDocState`]: whoever drives it (today the dev wiring, later
//! the SafeSaver integration) only has to publish a new `RemoteDocState` for the active document.

use super::error::RemoteError;

/// Which server file a document came from. Carried by the tab; contains no secret.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct RemoteOrigin {
    /// Site id (stable key into the site list).
    pub site: String,
    /// Short nickname shown in the `~site` chip.
    pub site_label: String,
    /// Normalised absolute remote path.
    pub path: String,
}

impl RemoteOrigin {
    /// `~vps`, truncated to 8 characters (spec 02, 7.1).
    pub fn chip(&self) -> String {
        let l: String = self.site_label.chars().take(8).collect();
        format!("~{l}")
    }

    /// Full name for a tooltip.
    pub fn display(&self) -> String {
        format!("{}:{}", self.site_label, self.path)
    }
}

/// State of one connection (strip dot + status text).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ConnState {
    Disconnected,
    Connecting,
    Connected,
    Failed(RemoteError),
}

impl ConnState {
    pub fn word(&self) -> &'static str {
        match self {
            ConnState::Disconnected => "Not connected",
            ConnState::Connecting => "Connecting",
            ConnState::Connected => "Connected",
            ConnState::Failed(_) => "Offline",
        }
    }
}

/// Where a remote document stands with respect to the server copy.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum RemoteSaveState {
    /// The server copy equals the buffer as of the last verified upload.
    Synced,
    /// An upload is in flight.
    Saving,
    /// A save is held (offline, or waiting for a re-check of the server file).
    Pending,
    /// The server file changed since it was opened; nothing was overwritten.
    Conflict,
    /// No connection: the buffer is safe locally.
    Offline,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RemoteDocState {
    pub site_label: String,
    pub save: RemoteSaveState,
    pub latency_ms: Option<u32>,
    /// `HH:MM` of the last verified upload, preformatted by the driver (no clock in this crate).
    pub last_sync: Option<String>,
    /// Plain-text reason for Conflict / Offline, shown in the chip tooltip.
    pub note: Option<String>,
}

impl RemoteDocState {
    pub fn synced(site_label: impl Into<String>) -> Self {
        Self { site_label: site_label.into(), save: RemoteSaveState::Synced, latency_ms: None, last_sync: None, note: None }
    }

    pub fn with(mut self, save: RemoteSaveState) -> Self {
        self.save = save;
        self
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ChipTone {
    Ok,
    Busy,
    Warn,
    Error,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RemoteChip {
    pub label: String,
    pub tone: ChipTone,
}

/// Pure view-model of the status-bar chip: always a word, never colour alone.
pub fn chip(s: &RemoteDocState) -> RemoteChip {
    let (word, tone) = match s.save {
        RemoteSaveState::Synced => ("Synced", ChipTone::Ok),
        RemoteSaveState::Saving => ("Saving...", ChipTone::Busy),
        RemoteSaveState::Pending => ("Pending", ChipTone::Warn),
        RemoteSaveState::Conflict => ("Conflict", ChipTone::Error),
        RemoteSaveState::Offline => ("Offline", ChipTone::Error),
    };
    let mut label = s.site_label.clone();
    if let (Some(ms), RemoteSaveState::Synced | RemoteSaveState::Saving) = (s.latency_ms, s.save) {
        label.push_str(&format!("  {ms} ms"));
    }
    label.push_str("  ");
    label.push_str(word);
    if let (RemoteSaveState::Synced, Some(t)) = (s.save, &s.last_sync) {
        label.push(' ');
        label.push_str(t);
    }
    RemoteChip { label, tone }
}

impl RemoteDocState {
    pub fn chip(&self) -> RemoteChip {
        chip(self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chip_origin_is_truncated_to_eight_characters() {
        let o = RemoteOrigin { site: "x".into(), site_label: "production-eu".into(), path: "/a".into() };
        assert_eq!(o.chip(), "~producti");
        assert_eq!(o.display(), "production-eu:/a");
    }

    #[test]
    fn every_save_state_has_a_word_and_a_tone() {
        let base = RemoteDocState::synced("vps");
        let words: Vec<(String, ChipTone)> = [RemoteSaveState::Synced, RemoteSaveState::Saving, RemoteSaveState::Pending, RemoteSaveState::Conflict, RemoteSaveState::Offline]
            .into_iter()
            .map(|st| {
                let c = base.clone().with(st).chip();
                (c.label, c.tone)
            })
            .collect();
        assert_eq!(words[0], ("vps  Synced".to_string(), ChipTone::Ok));
        assert!(words[1].0.ends_with("Saving...") && words[1].1 == ChipTone::Busy);
        assert!(words[2].0.ends_with("Pending") && words[2].1 == ChipTone::Warn);
        assert!(words[3].0.ends_with("Conflict") && words[3].1 == ChipTone::Error);
        assert!(words[4].0.ends_with("Offline") && words[4].1 == ChipTone::Error);
    }

    #[test]
    fn latency_and_last_sync_only_where_they_mean_something() {
        let mut s = RemoteDocState::synced("vps");
        s.latency_ms = Some(42);
        s.last_sync = Some("12:04".into());
        assert_eq!(s.chip().label, "vps  42 ms  Synced 12:04");
        s.save = RemoteSaveState::Offline;
        assert_eq!(s.chip().label, "vps  Offline");
    }
}
