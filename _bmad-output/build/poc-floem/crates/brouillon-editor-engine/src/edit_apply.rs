//! Lot 1: pure helpers around `EditSet` + the undo commands.
//!
//! - [`map_pos`] / [`map_splice`]: shift a byte position through edits
//!   (anchors, carets, folds, markers).
//! - [`diff_edits`]: turn "old text -> new text" into a minimal local `EditSet`
//!   (BK-004: replace-all must not swap the whole buffer).
//! - `commands()` declares the undo family; the shell runs them on the
//!   `Buffer` (`undo`, `redo`, `undo_all`, `undo_to_save`), so `run` returns
//!   `None` (they produce no `EditSet`).

use brouillon_core::contract::{CmdCtx, CmdOutcome, CommandSpec, EditSet, TextEdit};
use similar::{ChangeTag, TextDiff};

/// Which side an anchor sticks to when text is inserted exactly at its position.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Bias {
    /// Stay before inserted text.
    #[default]
    Left,
    /// Move after inserted text (a typing caret).
    Right,
}

/// Map `pos` through ONE splice that replaced `start..end` with `new_len` bytes.
///
/// - before `start`: unchanged; at/after a non-empty `end`: shifted;
/// - strictly inside a replaced range: collapses to `start` (`Left`) or to the
///   end of the new text (`Right`) - deterministic clamp, never out of range;
/// - exactly at `start`: unchanged for a non-empty range; for a pure insert
///   `Left` stays, `Right` moves after the text.
pub fn map_splice(pos: usize, start: usize, end: usize, new_len: usize, bias: Bias) -> usize {
    if pos < start {
        return pos;
    }
    if start == end {
        return if pos == start && bias == Bias::Left { pos } else { pos + new_len };
    }
    if pos == start {
        return pos;
    }
    if pos >= end {
        return pos - (end - start) + new_len;
    }
    match bias {
        Bias::Left => start,
        Bias::Right => start + new_len,
    }
}

/// Map `pos` (original coordinates) through a whole `EditSet`.
pub fn map_pos(edits: &EditSet, pos: usize, bias: Bias) -> usize {
    let mut pos = pos;
    let mut delta: isize = 0;
    for e in &edits.0 {
        let s = (e.start_byte as isize + delta) as usize;
        let en = (e.end_byte as isize + delta) as usize;
        pos = map_splice(pos, s, en, e.text.len(), bias);
        delta += e.text.len() as isize - (e.end_byte - e.start_byte) as isize;
    }
    pos
}

/// Minimal local edits turning `old` into `new`: line diff, then each changed
/// hunk is trimmed to its differing middle (common prefix / suffix removed,
/// on char boundaries). `apply_to_str(old) == new` always holds.
pub fn diff_edits(old: &str, new: &str) -> EditSet {
    if old == new {
        return EditSet::new();
    }
    let diff = TextDiff::from_lines(old, new);
    let mut edits: Vec<TextEdit> = Vec::new();
    // Hunk accumulators: byte ranges in old / new of the current run of changes.
    let (mut o, mut n) = (0usize, 0usize); // running offsets
    let mut hunk: Option<(usize, usize, usize, usize)> = None; // (old_s, old_e, new_s, new_e)
    let flush = |hunk: &mut Option<(usize, usize, usize, usize)>, edits: &mut Vec<TextEdit>| {
        if let Some((os, oe, ns, ne)) = hunk.take() {
            let (a, b) = (&old[os..oe], &new[ns..ne]);
            let mut pre = a.bytes().zip(b.bytes()).take_while(|(x, y)| x == y).count();
            while !(a.is_char_boundary(pre) && b.is_char_boundary(pre)) {
                pre -= 1;
            }
            let (a, b) = (&a[pre..], &b[pre..]);
            let mut suf = a.bytes().rev().zip(b.bytes().rev()).take_while(|(x, y)| x == y).count();
            while !(a.is_char_boundary(a.len() - suf) && b.is_char_boundary(b.len() - suf)) {
                suf -= 1;
            }
            let (a, b) = (&a[..a.len() - suf], &b[..b.len() - suf]);
            if !a.is_empty() || !b.is_empty() {
                let start = os + pre;
                edits.push(TextEdit::new(start, start + a.len(), b));
            }
        }
    };
    for change in diff.iter_all_changes() {
        let len = change.value().len();
        match change.tag() {
            ChangeTag::Equal => {
                flush(&mut hunk, &mut edits);
                o += len;
                n += len;
            }
            ChangeTag::Delete => {
                let h = hunk.get_or_insert((o, o, n, n));
                h.1 = o + len;
                o += len;
            }
            ChangeTag::Insert => {
                let h = hunk.get_or_insert((o, o, n, n));
                h.3 = n + len;
                n += len;
            }
        }
    }
    flush(&mut hunk, &mut edits);
    EditSet(edits)
}

/// Commands this module contributes to the registry. They are executed by the
/// shell directly on the active `Buffer` (see module docs).
pub fn commands() -> Vec<CommandSpec> {
    let spec = |id: &str, caption: &str, chord: Option<&str>| CommandSpec {
        id: id.into(),
        caption: caption.into(),
        category: "Edit".into(),
        default_chord: chord.map(Into::into),
        when: None, ..Default::default()
    };
    vec![
        spec("edit_undo", "Edit: Undo", Some("ctrl+z")),
        spec("edit_redo", "Edit: Redo", Some("ctrl+y")),
        spec("edit_undo_all", "Edit: Undo All", None),
        spec("edit_undo_to_save", "Edit: Undo to Last Save", None),
    ]
}

/// Run command `id`; `None` = not mine / handled on the `Buffer` by the shell.
pub fn run(id: &str, ctx: &CmdCtx) -> Option<CmdOutcome> {
    let _ = (id, ctx);
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn map_splice_rules() {
        // insert 3 bytes at 5
        assert_eq!(map_splice(4, 5, 5, 3, Bias::Left), 4);
        assert_eq!(map_splice(5, 5, 5, 3, Bias::Left), 5);
        assert_eq!(map_splice(5, 5, 5, 3, Bias::Right), 8);
        assert_eq!(map_splice(9, 5, 5, 3, Bias::Left), 12);
        // replace 5..10 with 2 bytes
        assert_eq!(map_splice(5, 5, 10, 2, Bias::Right), 5);
        assert_eq!(map_splice(7, 5, 10, 2, Bias::Left), 5);
        assert_eq!(map_splice(7, 5, 10, 2, Bias::Right), 7);
        assert_eq!(map_splice(10, 5, 10, 2, Bias::Left), 7);
        assert_eq!(map_splice(12, 5, 10, 2, Bias::Left), 9);
    }

    #[test]
    fn map_pos_through_set() {
        let set = EditSet(vec![TextEdit::insert(0, "ab"), TextEdit::delete(4, 6)]);
        assert_eq!(map_pos(&set, 3, Bias::Left), 5);
        assert_eq!(map_pos(&set, 5, Bias::Left), 6); // inside the deleted range: clamps to its start
        assert_eq!(map_pos(&set, 8, Bias::Left), 8);
    }

    fn check(old: &str, new: &str) -> EditSet {
        let set = diff_edits(old, new);
        assert_eq!(set.apply_to_str(old).unwrap(), new, "diff must reproduce new text");
        set
    }

    #[test]
    fn diff_edits_is_local_and_exact() {
        assert!(check("same\n", "same\n").is_empty());
        let old = "one\ntwo foo bar\nthree\nfour foo\n";
        let new = "one\ntwo baz bar\nthree\nfour baz\n";
        let set = check(old, new);
        assert_eq!(set.len(), 2, "one edit per changed line, not a whole swap");
        assert_eq!(set.0[0], TextEdit::new(8, 11, "baz"), "trimmed to the differing word");
        check("", "new");
        check("gone", "");
        check("héllo wörld\n", "héllo wörld!\nplus\n");
        check("a\nb\nc\n", "a\nc\n");
    }

    #[test]
    fn commands_are_declared() {
        let ids: Vec<_> = commands().into_iter().map(|c| c.id).collect();
        assert!(ids.contains(&"edit_undo".to_string()) && ids.contains(&"edit_undo_to_save".to_string()));
    }
}
