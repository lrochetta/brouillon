//! Opt-in ring of recent diagnostic lines (the `[remote-dbg]` style state traces). Dormant by default: nothing is
//! stored unless a test harness called [`enable`]. Carries no secret (callers only push lines they already print).

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;

const CAP: usize = 400;
static ON: AtomicBool = AtomicBool::new(false);
static RING: Mutex<VecDeque<String>> = Mutex::new(VecDeque::new());

pub fn enable() {
    ON.store(true, Ordering::Relaxed);
}

pub fn enabled() -> bool {
    ON.load(Ordering::Relaxed)
}

pub fn push(line: &str) {
    if !enabled() {
        return;
    }
    if let Ok(mut r) = RING.lock() {
        if r.len() >= CAP {
            r.pop_front();
        }
        r.push_back(line.to_string());
    }
}

/// The last `n` lines, oldest first.
pub fn recent(n: usize) -> Vec<String> {
    RING.lock().map(|r| r.iter().rev().take(n).rev().cloned().collect()).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    #[test]
    fn dormant_until_enabled_then_keeps_the_tail() {
        super::push("ignored");
        assert!(super::recent(5).is_empty());
        super::enable();
        for i in 0..450 {
            super::push(&format!("l{i}"));
        }
        let r = super::recent(3);
        assert_eq!(r, vec!["l447", "l448", "l449"]);
    }
}
