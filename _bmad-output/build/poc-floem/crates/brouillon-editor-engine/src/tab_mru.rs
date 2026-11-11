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

    /// Indices into `existing`, most recently visited first (the current tab leads); tabs never visited follow in tab order.
    pub fn ordered(&self, existing: &[String]) -> Vec<usize> {
        let mut out: Vec<usize> = self.order.iter().filter_map(|k| existing.iter().position(|e| e == k)).collect();
        out.dedup();
        let rest: Vec<usize> = (0..existing.len()).filter(|i| !out.contains(i)).collect();
        out.extend(rest);
        out
    }
}

/// The Ctrl+Tab popup: tab indices in MRU order and the highlighted row. Pure state; the shell commits on Ctrl release.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Switcher {
    pub items: Vec<usize>,
    pub sel: usize,
}

impl Switcher {
    /// First press: `forward` highlights the previously visited tab, `!forward` the least recent one.
    pub fn open(items: Vec<usize>, forward: bool) -> Self {
        let mut s = Self { items, sel: 0 };
        s.step(if forward { 1 } else { -1 });
        s
    }
    /// Move the highlight, wrapping.
    pub fn step(&mut self, delta: isize) {
        let n = self.items.len() as isize;
        if n > 0 {
            self.sel = (self.sel as isize + delta).rem_euclid(n) as usize;
        }
    }
    /// The tab index to activate on commit.
    pub fn chosen(&self) -> Option<usize> {
        self.items.get(self.sel).copied()
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
    fn ordered_lists_tabs_most_recent_first_then_unvisited() {
        let all: Vec<String> = ["a", "b", "c", "d"].iter().map(|s| s.to_string()).collect();
        let mut m = TabMru::default();
        m.visit("b");
        m.visit("d");
        m.visit("a");
        assert_eq!(m.ordered(&all), vec![0, 3, 1, 2]);
        assert_eq!(m.ordered(&all[..2]), vec![0, 1]);
    }

    #[test]
    fn switcher_opens_on_the_previous_tab_wraps_and_commits_the_highlight() {
        let mut s = Switcher::open(vec![2, 0, 1], true);
        assert_eq!(s.chosen(), Some(0));
        s.step(1);
        assert_eq!(s.chosen(), Some(1));
        s.step(1);
        assert_eq!(s.chosen(), Some(2), "wraps to the current tab");
        let back = Switcher::open(vec![2, 0, 1], false);
        assert_eq!(back.chosen(), Some(1), "Ctrl+Shift+Tab opens on the least recent");
        assert_eq!(Switcher::open(vec![], true).chosen(), None);
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
