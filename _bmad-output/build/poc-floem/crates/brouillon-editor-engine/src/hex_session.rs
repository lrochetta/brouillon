//! The state of one document in hex mode, free of any GUI so every rule is unit-tested: cursor and
//! selection, keyboard movement, goto-offset, nibble typing, undo/redo, regions that follow edits,
//! the inspector rows, Copy as.
//!
//! Selection model: `anchor` and `cursor` (both byte offsets); the selection covers both ends
//! inclusive (`min..=max`), so a click is the cursor on one byte and shift+click / drag extend it.
//! Only overwrite editing exists in this slice (typing hex digits); the document length never
//! changes through typing.
//!
//! Hex content and regions are local to this window: nothing here is reachable from the agent (MCP)
//! surface.

use std::ops::Range;
use std::path::{Path, PathBuf};

use crate::byte_source::{ByteSource, Ctl};
use crate::copy_as::{copy_range_as, CopyError, CopyFormat};
use crate::hex_doc::{HexDoc, HexError, HexSnapshot, BYTES_PER_ROW};
use crate::hex_hashes::{hash_range, HashAlgo};
use crate::hex_inspect::{inspect, Endian, InspectRow, WINDOW};
use crate::hex_regions::{self, Region, RegionColor, RegionError, RegionSet};
use crate::offset_expr::{eval_offset, ExprCtx, ExprError};
use crate::search::Cancelled;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Move {
    Left,
    Right,
    Up,
    Down,
    PageUp,
    PageDown,
    LineStart,
    LineEnd,
    DocStart,
    DocEnd,
}

/// How one byte is painted: the cursor wins over the selection, which wins over a region.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CellKind {
    Plain,
    Region(RegionColor),
    Selected,
    Cursor,
}

pub struct HexSession {
    doc: HexDoc,
    path: PathBuf,
    cursor: u64,
    anchor: u64,
    /// The next typed digit goes to the high nibble (false: to the low nibble of the same byte).
    high_next: bool,
    pub endian: Endian,
    regions: RegionSet,
    regions_dir: Option<PathBuf>,
    regions_dirty: bool,
    /// Rows moved by PageUp / PageDown (set from the viewport by the view).
    pub page_rows: u64,
}

impl HexSession {
    /// Open `path` as bytes. `regions_dir` is where its regions sidecar lives (None: not persisted).
    pub fn open(path: &Path, regions_dir: Option<&Path>) -> std::io::Result<Self> {
        let mut doc = HexDoc::open(path)?;
        doc.set_track_edits(true);
        let regions = regions_dir.map(|d| hex_regions::load(d, path)).unwrap_or_default();
        Ok(Self {
            doc,
            path: path.to_path_buf(),
            cursor: 0,
            anchor: 0,
            high_next: true,
            endian: Endian::default(),
            regions,
            regions_dir: regions_dir.map(Path::to_path_buf),
            regions_dirty: false,
            page_rows: 16,
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
    pub fn len(&self) -> u64 {
        ByteSource::len(&self.doc)
    }
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
    pub fn version(&self) -> u64 {
        self.doc.version()
    }
    pub fn is_dirty(&self) -> bool {
        self.doc.is_dirty()
    }
    pub fn doc(&self) -> &HexDoc {
        &self.doc
    }
    pub fn snapshot(&self) -> HexSnapshot {
        self.doc.snapshot()
    }
    pub fn cursor(&self) -> u64 {
        self.cursor
    }
    pub fn anchor(&self) -> u64 {
        self.anchor
    }
    pub fn regions(&self) -> &RegionSet {
        &self.regions
    }
    pub fn can_undo(&self) -> bool {
        self.doc.can_undo()
    }
    pub fn can_redo(&self) -> bool {
        self.doc.can_redo()
    }
    /// True while the next typed digit completes a byte (the high nibble was just typed).
    pub fn low_nibble_pending(&self) -> bool {
        !self.high_next
    }

    fn last_byte(&self) -> u64 {
        self.len().saturating_sub(1)
    }

    // ------------------------------------------------------------------------------ selection

    /// The selected bytes (both ends inclusive), only when more than the cursor byte is selected.
    pub fn selection(&self) -> Option<Range<u64>> {
        if self.anchor == self.cursor || self.is_empty() {
            return None;
        }
        let (a, b) = (self.anchor.min(self.cursor), self.anchor.max(self.cursor));
        Some(a..(b + 1).min(self.len()))
    }
    /// The selection, or the byte under the cursor (empty for an empty document).
    pub fn effective_range(&self) -> Range<u64> {
        if self.is_empty() {
            return 0..0;
        }
        self.selection().unwrap_or(self.cursor..self.cursor + 1)
    }
    /// Paint kind of byte `off`.
    pub fn cell_kind(&self, off: u64) -> CellKind {
        if off == self.cursor {
            return CellKind::Cursor;
        }
        if self.selection().is_some_and(|r| r.contains(&off)) {
            return CellKind::Selected;
        }
        match self.regions.color_at(off) {
            Some(c) => CellKind::Region(c),
            None => CellKind::Plain,
        }
    }

    /// A click: the cursor goes to `off`; with `extend` (shift) the anchor stays.
    pub fn click(&mut self, off: u64, extend: bool) {
        self.cursor = off.min(self.last_byte());
        if !extend {
            self.anchor = self.cursor;
        }
        self.high_next = true;
    }
    /// Drag: the same as a shift+click.
    pub fn drag_to(&mut self, off: u64) {
        self.click(off, true);
    }
    pub fn select_all(&mut self) {
        self.anchor = 0;
        self.cursor = self.last_byte();
        self.high_next = true;
    }
    pub fn select_range(&mut self, r: Range<u64>) {
        if r.end <= r.start || self.is_empty() {
            return;
        }
        self.anchor = r.start.min(self.last_byte());
        self.cursor = (r.end - 1).min(self.last_byte());
        self.high_next = true;
    }

    pub fn move_cursor(&mut self, m: Move, extend: bool) {
        let bpr = BYTES_PER_ROW as u64;
        let c = self.cursor;
        let last = self.last_byte();
        let page = self.page_rows.max(1) * bpr;
        let to = match m {
            Move::Left => c.saturating_sub(1),
            Move::Right => (c + 1).min(last),
            Move::Up => c.saturating_sub(bpr),
            Move::Down => (c + bpr).min(last),
            Move::PageUp => c.saturating_sub(page),
            Move::PageDown => (c + page).min(last),
            Move::LineStart => c - c % bpr,
            Move::LineEnd => (c - c % bpr + bpr - 1).min(last),
            Move::DocStart => 0,
            Move::DocEnd => last,
        };
        self.click(to, extend);
    }

    /// Goto: evaluate an offset expression (decimal, `0x` hex, `+`/`-` relative, `end`, arithmetic).
    pub fn goto(&mut self, expr: &str) -> Result<u64, ExprError> {
        let ctx = ExprCtx { cursor: self.cursor, sel: self.selection(), len: self.len() };
        let off = eval_offset(expr, &ctx)?;
        self.click(off, false);
        Ok(self.cursor)
    }

    // ------------------------------------------------------------------------------ editing

    /// Type one hex digit at the cursor (overwrite). The high nibble is written first, then the low
    /// one and the cursor advances; both digits of a byte are one undo step. Returns false when the
    /// character is not a hex digit (nothing happens).
    pub fn type_hex_digit(&mut self, ch: char) -> Result<bool, HexError> {
        let Some(v) = ch.to_digit(16) else { return Ok(false) };
        if self.is_empty() {
            return Ok(false);
        }
        if let Some(sel) = self.selection() {
            self.click(sel.start, false);
        }
        let at = self.cursor;
        self.doc.set_nibble(at, self.high_next, v as u8)?;
        if self.high_next {
            self.high_next = false;
        } else {
            self.high_next = true;
            if at < self.last_byte() {
                self.cursor = at + 1;
            }
            self.anchor = self.cursor;
        }
        self.sync_regions();
        Ok(true)
    }

    /// Fill the selection (or the cursor byte) with `pattern`, one undo step.
    pub fn fill_selection(&mut self, pattern: &[u8]) -> Result<(), HexError> {
        let r = self.effective_range();
        self.doc.fill(r, pattern)?;
        self.high_next = true;
        self.sync_regions();
        Ok(())
    }

    pub fn undo(&mut self) -> bool {
        let ok = self.doc.undo();
        self.after_history(ok);
        ok
    }
    pub fn redo(&mut self) -> bool {
        let ok = self.doc.redo();
        self.after_history(ok);
        ok
    }
    fn after_history(&mut self, changed: bool) {
        self.high_next = true;
        if changed {
            if let Some(d) = self.doc.last_edit() {
                self.cursor = d.pos.min(self.last_byte());
                self.anchor = self.cursor;
            }
        }
        self.sync_regions();
        self.cursor = self.cursor.min(self.last_byte());
        self.anchor = self.anchor.min(self.last_byte());
    }

    /// Atomic save of the bytes (and of the regions sidecar).
    pub fn save(&mut self, ctl: &mut Ctl) -> std::io::Result<()> {
        let path = self.path.clone();
        self.doc.save(&path, ctl)?;
        self.save_regions();
        Ok(())
    }

    // ------------------------------------------------------------------------------ inspector, copy, hash

    /// Inspector rows for the bytes at the cursor.
    pub fn inspector(&self) -> Vec<InspectRow> {
        let mut buf = [0u8; WINDOW];
        // A selection is read from its first byte (the cursor sits on its far end after Shift+Right, which would
        // show the bytes after the selection); without a selection, from the cursor.
        let at = self.selection().map(|r| r.start).unwrap_or(self.cursor);
        let n = self.doc.read_at(at, &mut buf);
        inspect(&buf[..n], self.endian)
    }
    pub fn copy_as(&self, f: CopyFormat) -> Result<String, CopyError> {
        copy_range_as(&self.doc, self.effective_range(), f)
    }
    /// Hash the selection (or the cursor byte) on the calling thread; the view runs this on a
    /// worker over a snapshot with progress and cancel (see `hash_snapshot`).
    pub fn hash_selection(&self, algos: &[HashAlgo], ctl: &mut Ctl) -> Result<Vec<(HashAlgo, String)>, Cancelled> {
        hash_range(&self.doc, self.effective_range(), algos, ctl)
    }

    // ------------------------------------------------------------------------------ regions

    /// Replay the document's edits onto the regions (they follow inserts and deletes).
    fn sync_regions(&mut self) {
        let deltas = self.doc.drain_edits();
        if deltas.is_empty() {
            return;
        }
        if !self.regions.is_empty() {
            deltas.into_iter().for_each(|d| self.regions.apply_edit(d));
            self.regions_dirty = true;
        }
    }
    pub fn add_region(&mut self, name: &str, color: RegionColor) -> Result<u32, RegionError> {
        let r = self.effective_range();
        let id = self.regions.add(r, name, color, self.len())?;
        self.regions_dirty = true;
        self.save_regions();
        Ok(id)
    }
    pub fn remove_region(&mut self, id: u32) -> bool {
        let ok = self.regions.remove(id);
        self.touch_regions(ok);
        ok
    }
    pub fn rename_region(&mut self, id: u32, name: &str) -> bool {
        let ok = self.regions.rename(id, name);
        self.touch_regions(ok);
        ok
    }
    pub fn recolor_region(&mut self, id: u32, c: RegionColor) -> bool {
        let ok = self.regions.set_color(id, c);
        self.touch_regions(ok);
        ok
    }
    fn touch_regions(&mut self, changed: bool) {
        if changed {
            self.regions_dirty = true;
            self.save_regions();
        }
    }
    /// Select the region and put the cursor at its start. False for an unknown or orphaned region.
    pub fn jump_to_region(&mut self, id: u32) -> bool {
        match self.regions.get(id).cloned() {
            Some(Region { orphaned: false, start, len, .. }) => {
                self.select_range(start..start + len);
                // the cursor (what the view scrolls to) goes to the start of the region
                self.anchor = (start + len - 1).min(self.last_byte());
                self.cursor = start.min(self.last_byte());
                true
            }
            _ => false,
        }
    }
    /// Persist the regions if they changed. Errors are not fatal (regions are a convenience).
    pub fn save_regions(&mut self) {
        if !self.regions_dirty {
            return;
        }
        if let Some(dir) = self.regions_dir.clone() {
            if hex_regions::save(&dir, &self.path, &self.regions).is_ok() {
                self.regions_dirty = false;
            }
        } else {
            self.regions_dirty = false;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hex_inspect::{value_of, InspectId};
    use crate::search::CancelToken;

    fn sess(bytes: &[u8]) -> (tempfile::TempDir, HexSession) {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("f.bin");
        std::fs::write(&p, bytes).unwrap();
        let s = HexSession::open(&p, Some(&dir.path().join("reg"))).unwrap();
        (dir, s)
    }
    fn bytes_of(s: &HexSession) -> Vec<u8> {
        let mut v = vec![0u8; s.len() as usize];
        s.doc().read_at(0, &mut v);
        v
    }

    #[test]
    fn click_shift_click_and_drag_select_inclusive() {
        let (_d, mut s) = sess(&[0u8; 100]);
        assert_eq!(s.selection(), None);
        assert_eq!(s.effective_range(), 0..1);
        s.click(10, false);
        assert_eq!((s.cursor(), s.selection()), (10, None));
        s.click(14, true);
        assert_eq!(s.selection(), Some(10..15));
        s.click(5, true);
        assert_eq!(s.selection(), Some(5..11), "anchor stays at 10, both ends inclusive");
        s.click(20, false);
        assert_eq!(s.selection(), None);
        s.drag_to(22);
        assert_eq!(s.selection(), Some(20..23));
        s.click(1000, false);
        assert_eq!(s.cursor(), 99, "clamped to the last byte");
        s.select_all();
        assert_eq!(s.selection(), Some(0..100));
    }

    #[test]
    fn arrows_pages_home_end_with_extend() {
        let (_d, mut s) = sess(&[0u8; 100]);
        s.page_rows = 2;
        s.move_cursor(Move::Right, false);
        s.move_cursor(Move::Down, false);
        assert_eq!(s.cursor(), 17);
        s.move_cursor(Move::Left, false);
        s.move_cursor(Move::Up, false);
        s.move_cursor(Move::Up, false);
        assert_eq!(s.cursor(), 0, "no underflow");
        s.move_cursor(Move::PageDown, false);
        assert_eq!(s.cursor(), 32);
        s.move_cursor(Move::LineEnd, true);
        assert_eq!((s.cursor(), s.selection()), (47, Some(32..48)));
        s.move_cursor(Move::LineStart, false);
        assert_eq!(s.cursor(), 32);
        s.move_cursor(Move::DocEnd, false);
        assert_eq!(s.cursor(), 99);
        s.move_cursor(Move::LineEnd, false);
        assert_eq!(s.cursor(), 99, "last row is short");
        s.move_cursor(Move::Down, false);
        assert_eq!(s.cursor(), 99);
        s.move_cursor(Move::PageUp, true);
        assert_eq!(s.selection(), Some(67..100));
        s.move_cursor(Move::DocStart, false);
        assert_eq!(s.cursor(), 0);
    }

    #[test]
    fn empty_document_is_safe() {
        let (_d, mut s) = sess(&[]);
        s.click(5, false);
        s.move_cursor(Move::Down, true);
        s.select_all();
        assert_eq!((s.cursor(), s.selection(), s.effective_range()), (0, None, 0..0));
        assert_eq!(s.type_hex_digit('A').unwrap(), false);
        assert!(s.inspector().iter().all(|r| r.value.is_none()));
        assert_eq!(s.goto("0"), Ok(0));
        assert_eq!(s.copy_as(CopyFormat::HexPlain).unwrap(), "");
    }

    #[test]
    fn goto_uses_cursor_selection_and_end() {
        let (_d, mut s) = sess(&[0u8; 0x200]);
        assert_eq!(s.goto("0x1F"), Ok(0x1F));
        assert_eq!(s.goto("+0x10"), Ok(0x2F));
        assert_eq!(s.goto("-0x10"), Ok(0x1F));
        assert_eq!(s.goto("end"), Ok(0x1FF), "end is clamped to the last byte for the cursor");
        assert!(matches!(s.goto("0x201"), Err(ExprError::PastEnd(_))));
        assert_eq!(s.cursor(), 0x1FF, "a failed goto does not move");
        s.select_range(0x20..0x28);
        assert_eq!(s.goto("sel.start + 4"), Ok(0x24));
        assert_eq!(s.selection(), None, "goto clears the selection");
    }

    #[test]
    fn typing_digits_edits_nibbles_advances_and_undoes_per_byte() {
        let (_d, mut s) = sess(&[0x00, 0x11, 0x22, 0x33]);
        assert_eq!(s.type_hex_digit('g').unwrap(), false);
        assert!(s.type_hex_digit('a').unwrap());
        assert!(s.low_nibble_pending());
        assert_eq!((bytes_of(&s)[0], s.cursor()), (0xA0, 0));
        assert!(s.type_hex_digit('B').unwrap());
        assert_eq!((bytes_of(&s)[0], s.cursor()), (0xAB, 1), "advanced after the low nibble");
        assert!(!s.low_nibble_pending());
        assert!(s.type_hex_digit('f').unwrap());
        assert!(s.type_hex_digit('e').unwrap());
        assert_eq!(bytes_of(&s), vec![0xAB, 0xFE, 0x22, 0x33]);
        assert!(s.is_dirty());
        assert!(s.undo());
        assert_eq!(bytes_of(&s), vec![0xAB, 0x11, 0x22, 0x33]);
        assert_eq!(s.cursor(), 1, "undo puts the cursor on the change");
        assert!(s.undo());
        assert_eq!(bytes_of(&s), vec![0x00, 0x11, 0x22, 0x33]);
        assert!(!s.undo());
        assert!(s.redo() && s.redo());
        assert_eq!(bytes_of(&s), vec![0xAB, 0xFE, 0x22, 0x33]);
        // at the last byte the cursor stays
        s.click(3, false);
        s.type_hex_digit('1').unwrap();
        s.type_hex_digit('2').unwrap();
        assert_eq!((bytes_of(&s)[3], s.cursor()), (0x12, 3));
        // moving the cursor between the two digits restarts at the high nibble
        s.click(0, false);
        s.type_hex_digit('7').unwrap();
        s.click(2, false);
        assert!(!s.low_nibble_pending());
    }

    #[test]
    fn typing_with_a_selection_starts_at_its_beginning() {
        let (_d, mut s) = sess(&[0u8; 8]);
        s.select_range(3..6);
        s.type_hex_digit('5').unwrap();
        assert_eq!((s.cursor(), s.selection()), (3, None));
        assert_eq!(bytes_of(&s)[3], 0x50);
    }

    #[test]
    fn fill_selection_is_one_undo_step() {
        let (_d, mut s) = sess(&[1, 2, 3, 4, 5, 6]);
        s.select_range(1..5);
        s.fill_selection(&[0xAA, 0xBB]).unwrap();
        assert_eq!(bytes_of(&s), vec![1, 0xAA, 0xBB, 0xAA, 0xBB, 6]);
        assert!(s.undo());
        assert_eq!(bytes_of(&s), vec![1, 2, 3, 4, 5, 6]);
    }

    #[test]
    fn save_writes_the_bytes_and_keeps_undo() {
        let (d, mut s) = sess(&[0, 0, 0]);
        s.type_hex_digit('1').unwrap();
        s.type_hex_digit('2').unwrap();
        let cancel = CancelToken::new();
        s.save(&mut Ctl::new(&cancel)).unwrap();
        assert!(!s.is_dirty());
        assert_eq!(std::fs::read(d.path().join("f.bin")).unwrap(), vec![0x12, 0, 0]);
        assert!(s.undo());
        assert_eq!(bytes_of(&s), vec![0, 0, 0]);
        assert!(s.is_dirty());
    }

    #[test]
    fn inspector_and_copy_follow_cursor_and_selection() {
        let (_d, mut s) = sess(&[0x01, 0x00, 0x00, 0x00, 0xFF, 0xFF]);
        assert_eq!(value_of(&s.inspector(), InspectId::U32), Some("1"));
        s.endian = Endian::Big;
        assert_eq!(value_of(&s.inspector(), InspectId::U32), Some("16777216"));
        s.click(4, false);
        assert_eq!(value_of(&s.inspector(), InspectId::U32), None, "not enough bytes");
        assert_eq!(value_of(&s.inspector(), InspectId::U16), Some("65535"));
        assert_eq!(s.copy_as(CopyFormat::HexPlain).unwrap(), "FF");
        s.select_range(0..4);
        assert_eq!(s.copy_as(CopyFormat::HexSpaced).unwrap(), "01 00 00 00");
        // the inspector reads a selection from its first byte, even when the cursor is on its far end
        s.endian = Endian::Little;
        s.click(0, false);
        for _ in 0..3 {
            s.move_cursor(Move::Right, true);
        }
        assert_eq!(s.cursor(), 3);
        assert_eq!(value_of(&s.inspector(), InspectId::U32), Some("1"));
        let cancel = CancelToken::new();
        let h = s.hash_selection(&[HashAlgo::Crc32], &mut Ctl::new(&cancel)).unwrap();
        assert_eq!(h[0].1.len(), 8);
    }

    #[test]
    fn cell_kinds_cursor_over_selection_over_region() {
        let (_d, mut s) = sess(&[0u8; 64]);
        s.select_range(10..20);
        s.add_region("hdr", RegionColor::Green).unwrap();
        s.click(30, false);
        s.select_range(15..25);
        s.click(40, false);
        assert_eq!(s.cell_kind(12), CellKind::Region(RegionColor::Green));
        assert_eq!(s.cell_kind(40), CellKind::Cursor);
        assert_eq!(s.cell_kind(41), CellKind::Plain);
        s.select_range(11..14);
        s.click(12, true);
        assert!(matches!(s.cell_kind(11), CellKind::Selected | CellKind::Cursor));
        assert_eq!(s.cell_kind(12), CellKind::Cursor);
    }

    #[test]
    fn regions_follow_typing_undo_and_persist() {
        let (d, mut s) = sess(&[0u8; 64]);
        s.select_range(8..16);
        let id = s.add_region("block", RegionColor::Blue).unwrap();
        s.click(8, false);
        s.type_hex_digit('1').unwrap();
        s.type_hex_digit('2').unwrap();
        assert_eq!(s.regions().get(id).unwrap().range(), 8..16, "overwrite does not move regions");
        // persisted in the settings-side dir, reloaded by a new session
        let s2 = HexSession::open(&d.path().join("f.bin"), Some(&d.path().join("reg"))).unwrap();
        assert_eq!(s2.regions().get(id).unwrap().name, "block");
        // jump selects the whole region
        s.click(0, false);
        assert!(s.jump_to_region(id));
        assert_eq!(s.selection(), Some(8..16));
        assert_eq!(s.cursor(), 8);
        // rename / recolor / delete
        assert!(s.rename_region(id, "header"));
        assert!(s.recolor_region(id, RegionColor::Red));
        let s3 = HexSession::open(&d.path().join("f.bin"), Some(&d.path().join("reg"))).unwrap();
        assert_eq!(s3.regions().get(id).unwrap().name, "header");
        assert_eq!(s3.regions().get(id).unwrap().color, RegionColor::Red);
        assert!(s.remove_region(id));
        assert!(!s.jump_to_region(id));
        let s4 = HexSession::open(&d.path().join("f.bin"), Some(&d.path().join("reg"))).unwrap();
        assert!(s4.regions().is_empty());
        // the sidecar is not next to the file
        assert_eq!(std::fs::read_dir(d.path()).unwrap().filter(|e| e.as_ref().unwrap().path().extension().is_some_and(|x| x == "json")).count(), 0);
    }

    #[test]
    fn without_a_regions_dir_nothing_is_written() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("f.bin");
        std::fs::write(&p, [0u8; 8]).unwrap();
        let mut s = HexSession::open(&p, None).unwrap();
        s.add_region("x", RegionColor::Blue).unwrap();
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);
    }
}
