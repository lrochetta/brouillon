//! Regions: named, coloured byte ranges of a document (a bookmark is a region of 1 byte).
//!
//! * Stored per document in a small sidecar under the settings directory, keyed by a hash of the
//!   document path, NEVER next to the file and never inside it. A sidecar holds names, colours and
//!   offsets only (no file bytes), is capped in size and count, and is ignored when oversized.
//! * Regions follow edits ([`RegionSet::apply_edit`]): offsets shift on inserts and deletes before
//!   them; a region whose bytes were all deleted becomes "orphaned" (kept in the list, greyed by the
//!   UI), never silently removed.
//! * Hex ranges and regions are not exposed to agents: nothing here is reachable from the MCP
//!   surface (tests in `brouillon-mcp` pin that).

use std::ops::Range;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// Maximum regions per document.
pub const MAX_REGIONS: usize = 10_000;
/// Maximum characters of a region name.
pub const MAX_NAME_CHARS: usize = 80;
/// Largest sidecar file read back (anything bigger is ignored).
pub const MAX_SIDECAR_BYTES: u64 = 2 << 20;

/// Eight palette tokens; the UI maps them to theme colours (and always shows the name too).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum RegionColor {
    #[default]
    Blue,
    Green,
    Orange,
    Red,
    Purple,
    Teal,
    Yellow,
    Gray,
}

impl RegionColor {
    pub const ALL: [RegionColor; 8] = [
        RegionColor::Blue,
        RegionColor::Green,
        RegionColor::Orange,
        RegionColor::Red,
        RegionColor::Purple,
        RegionColor::Teal,
        RegionColor::Yellow,
        RegionColor::Gray,
    ];
    pub fn label(self) -> &'static str {
        match self {
            RegionColor::Blue => "Blue",
            RegionColor::Green => "Green",
            RegionColor::Orange => "Orange",
            RegionColor::Red => "Red",
            RegionColor::Purple => "Purple",
            RegionColor::Teal => "Teal",
            RegionColor::Yellow => "Yellow",
            RegionColor::Gray => "Gray",
        }
    }
    /// Next colour in the palette (for the "colour" button).
    pub fn next(self) -> Self {
        Self::ALL[(self as usize + 1) % Self::ALL.len()]
    }
    /// (r, g, b) of the token; chosen to keep at least 3:1 contrast against both light and dark
    /// grid backgrounds when painted as a tint behind text at reduced alpha.
    pub fn rgb(self) -> (u8, u8, u8) {
        match self {
            RegionColor::Blue => (0x3B, 0x82, 0xF6),
            RegionColor::Green => (0x22, 0xA5, 0x5E),
            RegionColor::Orange => (0xF9, 0x73, 0x16),
            RegionColor::Red => (0xDC, 0x26, 0x26),
            RegionColor::Purple => (0x8B, 0x5C, 0xF6),
            RegionColor::Teal => (0x0D, 0x94, 0x88),
            RegionColor::Yellow => (0xCA, 0x8A, 0x04),
            RegionColor::Gray => (0x6B, 0x72, 0x80),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Region {
    pub id: u32,
    pub start: u64,
    pub len: u64,
    pub name: String,
    pub color: RegionColor,
    /// All bytes were deleted: kept in the list, greyed.
    #[serde(default)]
    pub orphaned: bool,
}

impl Region {
    pub fn range(&self) -> Range<u64> {
        self.start..self.start + self.len
    }
    pub fn contains(&self, off: u64) -> bool {
        !self.orphaned && off >= self.start && off < self.start + self.len
    }
}

/// One edit of the document: `removed` bytes at `pos` were replaced by `inserted` bytes.
/// Undo and redo are reported as the inverse / replay delta.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EditDelta {
    pub pos: u64,
    pub removed: u64,
    pub inserted: u64,
}

#[derive(Debug, PartialEq, Eq)]
pub enum RegionError {
    Full,
    Empty,
    OutOfRange,
}

impl std::fmt::Display for RegionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RegionError::Full => write!(f, "Too many regions (limit {MAX_REGIONS})"),
            RegionError::Empty => write!(f, "A region needs at least one byte"),
            RegionError::OutOfRange => write!(f, "Region is outside the file"),
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RegionSet {
    /// Sorted by (start, id).
    regions: Vec<Region>,
    next_id: u32,
}

fn clean_name(n: &str) -> String {
    n.chars().filter(|c| !c.is_control()).take(MAX_NAME_CHARS).collect::<String>().trim().to_string()
}

impl RegionSet {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn len(&self) -> usize {
        self.regions.len()
    }
    pub fn is_empty(&self) -> bool {
        self.regions.is_empty()
    }
    pub fn all(&self) -> &[Region] {
        &self.regions
    }
    pub fn get(&self, id: u32) -> Option<&Region> {
        self.regions.iter().find(|r| r.id == id)
    }

    fn sort(&mut self) {
        self.regions.sort_by_key(|r| (r.start, r.id));
    }

    /// Add a region over `range` (document length `doc_len` bounds it). Returns its id.
    pub fn add(&mut self, range: Range<u64>, name: &str, color: RegionColor, doc_len: u64) -> Result<u32, RegionError> {
        if range.end <= range.start {
            return Err(RegionError::Empty);
        }
        if range.end > doc_len {
            return Err(RegionError::OutOfRange);
        }
        if self.regions.len() >= MAX_REGIONS {
            return Err(RegionError::Full);
        }
        self.next_id += 1;
        let id = self.next_id;
        let mut name = clean_name(name);
        if name.is_empty() {
            name = format!("Region {id}");
        }
        self.regions.push(Region { id, start: range.start, len: range.end - range.start, name, color, orphaned: false });
        self.sort();
        Ok(id)
    }
    pub fn remove(&mut self, id: u32) -> bool {
        let n = self.regions.len();
        self.regions.retain(|r| r.id != id);
        n != self.regions.len()
    }
    pub fn rename(&mut self, id: u32, name: &str) -> bool {
        let name = clean_name(name);
        match self.regions.iter_mut().find(|r| r.id == id) {
            Some(r) if !name.is_empty() => {
                r.name = name;
                true
            }
            _ => false,
        }
    }
    pub fn set_color(&mut self, id: u32, c: RegionColor) -> bool {
        self.regions.iter_mut().find(|r| r.id == id).map(|r| r.color = c).is_some()
    }
    /// Regions covering byte `off`, smallest first (the smallest paints on top).
    pub fn at(&self, off: u64) -> Vec<&Region> {
        let mut v: Vec<&Region> = self.regions.iter().filter(|r| r.contains(off)).collect();
        v.sort_by_key(|r| (r.len, r.id));
        v
    }
    /// The colour to paint byte `off` with, if any.
    pub fn color_at(&self, off: u64) -> Option<RegionColor> {
        self.regions.iter().filter(|r| r.contains(off)).min_by_key(|r| (r.len, r.id)).map(|r| r.color)
    }
    /// Regions overlapping `range` (binary search on the sorted starts is not valid with overlaps,
    /// so this is linear in the region count, capped at [`MAX_REGIONS`]).
    pub fn in_range(&self, range: Range<u64>) -> impl Iterator<Item = &Region> {
        self.regions.iter().filter(move |r| !r.orphaned && r.start < range.end && r.start + r.len > range.start)
    }

    /// Keep regions in step with an edit of the document.
    pub fn apply_edit(&mut self, d: EditDelta) {
        if d.removed == d.inserted {
            return; // overwrite: offsets do not move
        }
        let rem_end = d.pos + d.removed;
        let map_start = |p: u64| -> u64 {
            if p < d.pos {
                p
            } else if p >= rem_end {
                p - d.removed + d.inserted
            } else {
                d.pos // start inside the removed part: the region keeps what was inserted
            }
        };
        let map_end = |p: u64| -> u64 {
            if p <= d.pos {
                p
            } else if p >= rem_end {
                p - d.removed + d.inserted
            } else {
                d.pos + d.inserted
            }
        };
        for r in self.regions.iter_mut() {
            if r.orphaned {
                r.start = map_start(r.start);
                continue;
            }
            let (s, e) = (r.start, r.start + r.len);
            // an insertion exactly at the end does not extend the region; at the start it shifts it
            let (ns, ne) = (map_start(s), map_end(e));
            if ne <= ns {
                r.orphaned = true;
                r.start = ns;
                r.len = 0;
            } else {
                r.start = ns;
                r.len = ne - ns;
            }
        }
        self.sort();
    }

    // ------------------------------------------------------------------------------ persistence

    pub fn to_json(&self) -> String {
        #[derive(Serialize)]
        struct Out<'a> {
            v: u32,
            regions: &'a [Region],
        }
        serde_json::to_string(&Out { v: 1, regions: &self.regions }).unwrap_or_default()
    }

    pub fn from_json(s: &str) -> Result<Self, String> {
        #[derive(Deserialize)]
        struct In {
            v: u32,
            regions: Vec<Region>,
        }
        let i: In = serde_json::from_str(s).map_err(|e| e.to_string())?;
        if i.v != 1 {
            return Err(format!("unknown regions version {}", i.v));
        }
        if i.regions.len() > MAX_REGIONS {
            return Err("too many regions".into());
        }
        let mut set = RegionSet::new();
        for mut r in i.regions {
            r.name = clean_name(&r.name);
            if r.start.checked_add(r.len).is_none() || (r.len == 0 && !r.orphaned) {
                continue;
            }
            set.next_id = set.next_id.max(r.id);
            set.regions.push(r);
        }
        set.sort();
        Ok(set)
    }
}

/// Sidecar file for a document path: `<dir>/<first 32 hex of sha256(path)>.json`. The path itself
/// is never written to disk, only its hash.
pub fn sidecar_path(dir: &Path, doc_path: &Path) -> PathBuf {
    let mut h = Sha256::new();
    let key = doc_path.to_string_lossy().replace('\\', "/");
    #[cfg(windows)]
    let key = key.to_lowercase();
    h.update(key.as_bytes());
    let d = h.finalize();
    let name: String = d[..16].iter().map(|b| format!("{b:02x}")).collect();
    dir.join(format!("{name}.json"))
}

/// Load the regions of `doc_path`; a missing, oversized or corrupt sidecar gives an empty set.
pub fn load(dir: &Path, doc_path: &Path) -> RegionSet {
    let p = sidecar_path(dir, doc_path);
    match std::fs::metadata(&p) {
        Ok(m) if m.len() <= MAX_SIDECAR_BYTES => {}
        _ => return RegionSet::new(),
    }
    std::fs::read_to_string(&p).ok().and_then(|s| RegionSet::from_json(&s).ok()).unwrap_or_default()
}

/// Save the regions of `doc_path`. An empty set removes the sidecar.
pub fn save(dir: &Path, doc_path: &Path, set: &RegionSet) -> std::io::Result<()> {
    let p = sidecar_path(dir, doc_path);
    if set.is_empty() {
        return match std::fs::remove_file(&p) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e),
            _ => Ok(()),
        };
    }
    std::fs::create_dir_all(dir)?;
    crate::durable::durable_write(&p, set.to_json().as_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn set3() -> (RegionSet, u32, u32, u32) {
        let mut s = RegionSet::new();
        let a = s.add(10..20, "a", RegionColor::Blue, 1000).unwrap();
        let b = s.add(30..40, "b", RegionColor::Green, 1000).unwrap();
        let c = s.add(15..18, "c", RegionColor::Red, 1000).unwrap();
        (s, a, b, c)
    }
    fn span(s: &RegionSet, id: u32) -> (u64, u64, bool) {
        let r = s.get(id).unwrap();
        (r.start, r.len, r.orphaned)
    }

    #[test]
    fn add_validates_and_names() {
        let mut s = RegionSet::new();
        assert_eq!(s.add(5..5, "x", RegionColor::Blue, 100), Err(RegionError::Empty));
        assert_eq!(s.add(5..101, "x", RegionColor::Blue, 100), Err(RegionError::OutOfRange));
        let id = s.add(0..1, "  ", RegionColor::Blue, 100).unwrap();
        assert_eq!(s.get(id).unwrap().name, format!("Region {id}"));
        let id2 = s.add(0..1, &"n\n".repeat(200), RegionColor::Blue, 100).unwrap();
        assert_eq!(s.get(id2).unwrap().name.chars().count(), MAX_NAME_CHARS);
        assert!(!s.get(id2).unwrap().name.contains('\n'));
    }

    #[test]
    fn cap_is_enforced() {
        let mut s = RegionSet::new();
        for i in 0..MAX_REGIONS as u64 {
            s.add(i..i + 1, "r", RegionColor::Gray, u64::MAX).unwrap();
        }
        assert_eq!(s.add(0..1, "r", RegionColor::Gray, u64::MAX), Err(RegionError::Full));
    }

    #[test]
    fn smallest_region_paints_on_top() {
        let (s, _a, _b, c) = set3();
        assert_eq!(s.color_at(16), Some(RegionColor::Red));
        assert_eq!(s.color_at(12), Some(RegionColor::Blue));
        assert_eq!(s.color_at(25), None);
        assert_eq!(s.at(16).first().unwrap().id, c);
        assert_eq!(s.in_range(19..31).count(), 2);
    }

    #[test]
    fn edit_before_shifts_after_unchanged() {
        let (mut s, a, b, _c) = set3();
        s.apply_edit(EditDelta { pos: 0, removed: 0, inserted: 4 });
        assert_eq!(span(&s, a), (14, 10, false));
        assert_eq!(span(&s, b), (34, 10, false));
        s.apply_edit(EditDelta { pos: 100, removed: 5, inserted: 0 });
        assert_eq!(span(&s, b), (34, 10, false));
        s.apply_edit(EditDelta { pos: 0, removed: 4, inserted: 0 });
        assert_eq!(span(&s, a), (10, 10, false));
    }

    #[test]
    fn insert_inside_grows_at_end_does_not() {
        let (mut s, a, ..) = set3();
        s.apply_edit(EditDelta { pos: 12, removed: 0, inserted: 3 });
        assert_eq!(span(&s, a), (10, 13, false));
        s.apply_edit(EditDelta { pos: 23, removed: 0, inserted: 3 });
        assert_eq!(span(&s, a), (10, 13, false), "insertion at the end offset");
        s.apply_edit(EditDelta { pos: 10, removed: 0, inserted: 3 });
        assert_eq!(span(&s, a), (13, 13, false), "insertion at the start shifts");
    }

    #[test]
    fn overwrite_never_moves_anything() {
        let (mut s, a, b, c) = set3();
        let before = s.clone();
        s.apply_edit(EditDelta { pos: 12, removed: 10, inserted: 10 });
        assert_eq!(s, before);
        let _ = (a, b, c);
    }

    #[test]
    fn delete_partial_and_full() {
        let (mut s, a, b, c) = set3();
        // delete 12..32: a keeps 10..12, b loses its first 2 bytes, c is gone
        s.apply_edit(EditDelta { pos: 12, removed: 20, inserted: 0 });
        assert_eq!(span(&s, a), (10, 2, false));
        assert_eq!(span(&s, b), (12, 8, false));
        assert_eq!(span(&s, c), (12, 0, true), "orphaned, not removed");
        assert_eq!(s.len(), 3);
        assert!(s.at(12).iter().all(|r| r.id != c), "an orphan paints nothing");
        assert_eq!(s.in_range(0..100).count(), 2);
    }

    #[test]
    fn undo_round_trip_restores_when_the_deltas_are_inverted() {
        let (mut s, a, b, _c) = set3();
        s.apply_edit(EditDelta { pos: 0, removed: 0, inserted: 8 });
        s.apply_edit(EditDelta { pos: 0, removed: 8, inserted: 0 });
        assert_eq!(span(&s, a), (10, 10, false));
        assert_eq!(span(&s, b), (30, 10, false));
    }

    #[test]
    fn randomized_edits_keep_invariants() {
        let mut seed = 99u64;
        let mut next = move || {
            seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            seed >> 33
        };
        let (mut s, ..) = set3();
        let mut len = 1000u64;
        for _ in 0..5000 {
            let pos = next() % (len + 1);
            let removed = next() % 8.min(len - pos + 1);
            let inserted = next() % 8;
            s.apply_edit(EditDelta { pos, removed, inserted });
            len = len - removed + inserted;
            for r in s.all() {
                assert!(r.start <= len || r.orphaned, "{r:?} len {len}");
                assert!(r.orphaned || r.len > 0);
                assert!(r.start + r.len <= len.max(r.start), "{r:?} len {len}");
            }
            assert!(s.all().windows(2).all(|w| (w[0].start, w[0].id) <= (w[1].start, w[1].id)));
        }
    }

    #[test]
    fn json_round_trip_and_hostile_input() {
        let (mut s, a, ..) = set3();
        s.rename(a, "header \"quoted\"");
        s.set_color(a, RegionColor::Purple);
        let back = RegionSet::from_json(&s.to_json()).unwrap();
        assert_eq!(back, s);
        assert!(RegionSet::from_json("not json").is_err());
        assert!(RegionSet::from_json(r#"{"v":2,"regions":[]}"#).is_err());
        // zero-length live region and overflowing range are dropped
        let j = r#"{"v":1,"regions":[{"id":1,"start":0,"len":0,"name":"z","color":"blue"},{"id":2,"start":18446744073709551615,"len":5,"name":"o","color":"red"},{"id":3,"start":1,"len":1,"name":"ok","color":"gray"}]}"#;
        let r = RegionSet::from_json(j).unwrap();
        assert_eq!(r.len(), 1);
        assert_eq!(r.all()[0].name, "ok");
    }

    #[test]
    fn sidecar_lives_in_the_settings_dir_keyed_by_hash_and_holds_no_path() {
        let dir = std::env::temp_dir().join(format!("brouillon-hexreg-{}", std::process::id()));
        let doc = Path::new("C:/data/secret-firmware.bin");
        let p = sidecar_path(&dir, doc);
        assert!(p.starts_with(&dir));
        assert_ne!(p, sidecar_path(&dir, Path::new("C:/data/other.bin")));
        let (s, ..) = set3();
        save(&dir, doc, &s).unwrap();
        let text = std::fs::read_to_string(&p).unwrap();
        assert!(!text.contains("secret-firmware") && !text.contains("data"), "no path in the sidecar: {text}");
        assert_eq!(load(&dir, doc), s);
        // oversize sidecar is ignored
        std::fs::write(&p, vec![b' '; MAX_SIDECAR_BYTES as usize + 1]).unwrap();
        assert!(load(&dir, doc).is_empty());
        // corrupt sidecar is ignored
        std::fs::write(&p, b"{").unwrap();
        assert!(load(&dir, doc).is_empty());
        // an empty set removes the file
        save(&dir, doc, &RegionSet::new()).unwrap();
        assert!(!p.exists());
        save(&dir, doc, &RegionSet::new()).unwrap();
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn colour_cycle_visits_all() {
        let mut c = RegionColor::Blue;
        let mut seen = std::collections::HashSet::new();
        for _ in 0..8 {
            seen.insert(c);
            c = c.next();
        }
        assert_eq!(seen.len(), 8);
        assert_eq!(c, RegionColor::Blue);
    }
}
