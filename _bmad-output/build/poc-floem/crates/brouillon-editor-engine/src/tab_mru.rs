//! "Last visited tab" (Tab: Last Visited Tab, F12 in Classic): remembers the tab that was active
//! before the current one, by a stable key (file path, or the title of an unsaved tab) so that
//! closing or reordering tabs cannot make it point at the wrong document.

/// Stable identity of a tab: its file path, or the title of an unsaved tab.
pub fn tab_key(path: Option<&std::path::Path>, title: &str) -> String {
    path.map(|p| p.to_string_lossy().into_owned()).unwrap_or_else(|| format!("untitled:{title}"))
}

#[derive(Debug, Default, Clone)]
pub struct TabMru {
    /// Most recent first; `order[0]` is the current tab.
    order: Vec<String>,
}

impl TabMru {
    /// Record that `key` is now the active tab (no-op when it already is).
    pub fn visit(&mut self, key: &str) {
        if self.order.first().map(String::as_str) == Some(key) {
            return;
        }
        self.order.retain(|k| k != key);
        self.order.insert(0, key.to_string());
        self.order.truncate(32);
    }

    /// A tab was closed or renamed away.
    pub fn forget(&mut self, key: &str) {
        self.order.retain(|k| k != key);
    }

    /// The tab visited before the current one, if it still exists among `existing`.
    pub fn last_visited<'a>(&'a self, existing: &[String]) -> Option<&'a str> {
        self.order.iter().skip(1).find(|k| existing.iter().any(|e| e == *k)).map(String::as_str)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn toggles_between_the_two_most_recent_tabs() {
        let mut m = TabMru::default();
        let all = vec!["a".to_string(), "b".to_string(), "c".to_string()];
        m.visit("a");
        assert_eq!(m.last_visited(&all), None);
        m.visit("b");
        assert_eq!(m.last_visited(&all), Some("a"));
        m.visit("c");
        assert_eq!(m.last_visited(&all), Some("b"));
        m.visit("b"); // jump to the last visited
        assert_eq!(m.last_visited(&all), Some("c"));
        m.visit("b");
        assert_eq!(m.last_visited(&all), Some("c"));
    }

    #[test]
    fn keys_are_the_path_or_the_unsaved_title() {
        assert_eq!(tab_key(Some(std::path::Path::new("/a/b.rs")), "b.rs"), "/a/b.rs");
        assert_eq!(tab_key(None, "untitled-2"), "untitled:untitled-2");
    }

    #[test]
    fn closed_tabs_are_skipped() {
        let mut m = TabMru::default();
        m.visit("a");
        m.visit("b");
        m.visit("c");
        assert_eq!(m.last_visited(&["a".into(), "c".into()]), Some("a"));
        m.forget("b");
        assert_eq!(m.last_visited(&["a".into(), "c".into()]), Some("a"));
    }
}
