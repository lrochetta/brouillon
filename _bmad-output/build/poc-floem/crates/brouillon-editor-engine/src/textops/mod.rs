//! Lot 5: text commands as pure edit generators (text + regions in, `CmdOutcome` out).
//!
//! Lines are `\n` or `\r\n` terminated; a trailing eol yields a final empty line.
//! Commands act on the lines touched by the selection (caret line when empty);
//! sort / trim / filters act on the whole buffer when nothing is selected.

use brouillon_core::contract::{CmdCtx, CmdOutcome, CommandSpec, EditSet, Region, TextEdit};

pub mod align;
pub mod case;
pub mod comment;
pub mod lines;
pub mod sort;
pub mod transform;
pub mod whitespace;

/// Commands this module contributes to the registry.
pub fn commands() -> Vec<CommandSpec> {
    let mut v = lines::commands();
    v.extend(case::commands());
    v.extend(whitespace::commands());
    v.extend(sort::commands());
    v.extend(align::commands());
    v.extend(comment::commands());
    v.extend(transform::commands());
    v
}

/// Run command `id`; `None` = not mine.
pub fn run(id: &str, ctx: &CmdCtx) -> Option<CmdOutcome> {
    lines::run(id, ctx)
        .or_else(|| case::run(id, ctx))
        .or_else(|| whitespace::run(id, ctx))
        .or_else(|| sort::run(id, ctx))
        .or_else(|| align::run(id, ctx))
        .or_else(|| comment::run(id, ctx))
        .or_else(|| transform::run(id, ctx))
}

pub(crate) fn spec(id: &str, caption: &str, category: &str) -> CommandSpec {
    CommandSpec { id: id.into(), caption: caption.into(), category: category.into(), ..Default::default() }
}

/// One line: `start..end` is the content, `end..next` the eol, `next` the following line start.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Line {
    pub start: usize,
    pub end: usize,
    pub next: usize,
}

pub fn split_lines(text: &str) -> Vec<Line> {
    let mut out = Vec::new();
    let mut start = 0;
    for (i, b) in text.bytes().enumerate() {
        if b == b'\n' {
            let end = if i > start && text.as_bytes()[i - 1] == b'\r' { i - 1 } else { i };
            out.push(Line { start, end, next: i + 1 });
            start = i + 1;
        }
    }
    out.push(Line { start, end: text.len(), next: text.len() });
    out
}

pub fn eol_of(text: &str) -> &'static str {
    if text.contains("\r\n") { "\r\n" } else { "\n" }
}

pub fn line_of(lines: &[Line], offset: usize) -> usize {
    lines.partition_point(|l| l.start <= offset).saturating_sub(1)
}

/// Sorted unique line indexes touched by `sel` (caret line when empty). A selection ending
/// at column 0 does not include that last line.
pub fn selected_lines(lines: &[Line], sel: &[Region]) -> Vec<usize> {
    if sel.is_empty() {
        return vec![0];
    }
    let mut out = Vec::new();
    for r in sel {
        let a = line_of(lines, r.start());
        let mut b = line_of(lines, r.end());
        if r.end() > r.start() && b > a && lines[b].start == r.end() {
            b -= 1;
        }
        out.extend(a..=b);
    }
    out.sort_unstable();
    out.dedup();
    out
}

/// Contiguous `(first, last)` line range; the whole buffer when `whole` and nothing is selected.
pub fn target_range(text: &str, lines: &[Line], sel: &[Region], whole: bool) -> (usize, usize) {
    if whole && sel.iter().all(|r| r.is_caret()) {
        let mut last = lines.len() - 1;
        if last > 0 && lines[last].start == text.len() {
            last -= 1;
        }
        return (0, last);
    }
    let s = selected_lines(lines, sel);
    (s[0], *s.last().unwrap())
}

/// Replace the content of lines `first..=last` (eols kept) by `new` joined with the file eol.
pub fn replace_lines(text: &str, lines: &[Line], first: usize, last: usize, new: &[String]) -> TextEdit {
    TextEdit::new(lines[first].start, lines[last].end, new.join(eol_of(text)))
}

/// Drop no-ops, sort, drop overlaps, wrap in an outcome.
pub fn outcome(text: &str, mut edits: Vec<TextEdit>) -> CmdOutcome {
    edits.retain(|e| text.get(e.start_byte..e.end_byte) != Some(e.text.as_str()));
    edits.sort_by_key(|e| (e.start_byte, e.end_byte));
    let mut kept: Vec<TextEdit> = Vec::with_capacity(edits.len());
    for e in edits {
        if kept.last().is_none_or(|p| e.start_byte >= p.end_byte) {
            kept.push(e);
        }
    }
    CmdOutcome { edits: EditSet(kept), ..Default::default() }
}

pub fn message(text: impl Into<String>) -> CmdOutcome {
    CmdOutcome { message: Some(text.into()), ..Default::default() }
}

pub fn is_word(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

/// Byte range of the word containing/touching `offset`.
pub fn word_at(text: &str, offset: usize) -> Option<(usize, usize)> {
    let offset = offset.min(text.len());
    let start = text[..offset].char_indices().rev().take_while(|&(_, c)| is_word(c)).last().map_or(offset, |(i, _)| i);
    let end = offset + text[offset..].char_indices().take_while(|&(_, c)| is_word(c)).last().map_or(0, |(i, c)| i + c.len_utf8());
    (start < end).then_some((start, end))
}

pub fn arg_usize(ctx: &CmdCtx, i: usize, default: usize) -> usize {
    ctx.args.get(i).and_then(|a| a.parse().ok()).unwrap_or(default)
}

/// Char column of byte `offset` within its line.
pub fn column_of(text: &str, lines: &[Line], offset: usize) -> usize {
    let l = lines[line_of(lines, offset)];
    text[l.start..offset.clamp(l.start, l.end)].chars().count()
}

/// Byte offset of char column `col` in `line` (clamped to the line end).
pub fn byte_at_column(text: &str, line: Line, col: usize) -> usize {
    text[line.start..line.end].char_indices().nth(col).map_or(line.end, |(i, _)| line.start + i)
}

/// Test helper: run `id` over `text` with `sel` and apply the result.
#[cfg(test)]
pub(crate) fn apply(id: &str, text: &str, sel: &[Region], args: &[&str]) -> String {
    let args: Vec<String> = args.iter().map(|s| s.to_string()).collect();
    let ctx = CmdCtx { text, selection: sel, path: None, args: &args };
    run(id, &ctx).unwrap_or_else(|| panic!("unknown command {id}")).edits.apply_to_str(text).unwrap()
}

#[cfg(test)]
pub(crate) fn sel(a: usize, b: usize) -> Vec<Region> {
    vec![Region { anchor: a, head: b }]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lines_and_selection() {
        let l = split_lines("ab\r\ncd\n");
        assert_eq!(l.len(), 3);
        assert_eq!((l[0].end, l[0].next), (2, 4));
        // selection ending at column 0 excludes the last line
        assert_eq!(selected_lines(&l, &sel(0, 4)), vec![0]);
        assert_eq!(selected_lines(&l, &sel(0, 5)), vec![0, 1]);
        assert_eq!(word_at("foo bar", 5), Some((4, 7)));
        assert_eq!(word_at("foo bar", 3), Some((0, 3)));
    }

    #[test]
    fn registry_ids_unique_and_every_id_routes() {
        let cmds = commands();
        let mut ids: Vec<_> = cmds.iter().map(|c| c.id.clone()).collect();
        let n = ids.len();
        ids.sort();
        ids.dedup();
        assert_eq!(n, ids.len(), "duplicate command id");
        let ctx = CmdCtx { text: "", selection: &[], path: None, args: &[] };
        assert!(run("nope", &ctx).is_none());
        for id in &ids {
            assert!(run(id, &ctx).is_some(), "{id} not routed");
        }
    }
}
