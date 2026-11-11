//! Cliptext libraries (BK-054): titled entries, `^` insertion escapes, own TOML
//! format plus a tolerant reader/writer for the EditPlus `.ctl` text format.
//!
//! Escapes in a body: `^!` caret marker (wraps the selection when there is
//! one), `^^` literal `^`, `^#` literal `#`, `^@` current date, `^%hh` byte
//! `hh`. Any other `^x` stays literal. A title-only entry inserts its title.

use brouillon_core::contract::{CmdCtx, CmdOutcome, CommandSpec, EditSet, PanelId, Region, TextEdit};
use serde::{Deserialize, Serialize};
use std::path::Path;

/// One cliptext entry. Empty `body` = title-only entry (the title is inserted).
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct Clip {
    pub title: String,
    #[serde(default)]
    pub body: String,
}

/// A cliptext library.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct Library {
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub info: String,
    #[serde(default)]
    pub sorted: bool,
    #[serde(default, rename = "entry")]
    pub entries: Vec<Clip>,
}

impl Library {
    pub fn add(&mut self, clip: Clip) {
        self.entries.push(clip);
        if self.sorted {
            self.sort();
        }
    }
    pub fn remove(&mut self, index: usize) -> Option<Clip> {
        (index < self.entries.len()).then(|| self.entries.remove(index))
    }
    pub fn modify(&mut self, index: usize, clip: Clip) -> bool {
        match self.entries.get_mut(index) {
            Some(slot) => {
                *slot = clip;
                true
            }
            None => false,
        }
    }
    /// Case-insensitive stable sort by title.
    pub fn sort(&mut self) {
        self.entries.sort_by_key(|c| c.title.to_lowercase());
    }
    /// Entries whose title starts with `prefix` (case-insensitive).
    pub fn search(&self, prefix: &str) -> Vec<&Clip> {
        let p = prefix.to_lowercase();
        self.entries.iter().filter(|c| c.title.to_lowercase().starts_with(&p)).collect()
    }
}

/// Result of expanding a clip body.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Expanded {
    pub text: String,
    /// Byte offset of the caret inside `text` (end of text when no `^!`).
    pub caret: usize,
}

/// Expand the escapes of `body`. With `selection`, the text is wrapped around it
/// at the first `^!` and the caret lands right after the selection; with no
/// `^!` the clip simply replaces the selection. `date` is what `^@` inserts.
pub fn expand_text(body: &str, selection: Option<&str>, date: &str) -> Expanded {
    let chars: Vec<char> = body.chars().collect();
    let mut out = String::with_capacity(body.len());
    let mut caret: Option<usize> = None;
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        i += 1;
        if c != '^' {
            out.push(c);
            continue;
        }
        match chars.get(i).copied() {
            Some('^') => { out.push('^'); i += 1; }
            Some('#') => { out.push('#'); i += 1; }
            Some('@') => { out.push_str(date); i += 1; }
            Some('!') => {
                i += 1;
                if caret.is_none() {
                    out.push_str(selection.unwrap_or(""));
                    caret = Some(out.len());
                }
            }
            Some('%') => {
                let hex: String = chars.get(i + 1..i + 3).map(|s| s.iter().collect()).unwrap_or_default();
                match u8::from_str_radix(&hex, 16) {
                    Ok(b) if hex.len() == 2 && hex.is_ascii() => {
                        out.push(char::from(b));
                        i += 3;
                    }
                    _ => out.push('^'),
                }
            }
            _ => out.push('^'),
        }
    }
    let caret = caret.unwrap_or(out.len());
    Expanded { text: out, caret }
}

/// Build the outcome of inserting `clip` over the first selection region.
pub fn expand(clip: &Clip, ctx: &CmdCtx, date: &str) -> CmdOutcome {
    let region = ctx.selection.first().copied().unwrap_or_default();
    let (start, end) = (region.start(), region.end());
    let Some(sel) = ctx.text.get(start..end) else {
        return CmdOutcome { message: Some("cliptext: selection out of range".into()), ..Default::default() };
    };
    let ex = if clip.body.is_empty() {
        Expanded { text: clip.title.clone(), caret: clip.title.len() }
    } else {
        expand_text(&clip.body, (!sel.is_empty()).then_some(sel), date)
    };
    CmdOutcome {
        edits: EditSet(vec![TextEdit::new(start, end, ex.text)]),
        selection: Some(vec![Region::caret(start + ex.caret)]),
        ..Default::default()
    }
}

/// Decode legacy 8-bit or UTF-8 bytes (BOM stripped; Windows-1252 fallback).
pub fn decode_text(bytes: &[u8]) -> String {
    let bytes = bytes.strip_prefix(&[0xEF, 0xBB, 0xBF]).unwrap_or(bytes);
    match std::str::from_utf8(bytes) {
        Ok(s) => s.to_string(),
        Err(_) => encoding_rs::WINDOWS_1252.decode(bytes).0.into_owned(),
    }
}

/// Parsed line-oriented `#DIRECTIVE` file shared by `.ctl` and `.acp`.
#[derive(Debug, Default)]
pub(crate) struct RawClipFile {
    pub header: Vec<(String, String)>,
    pub info: String,
    pub clips: Vec<Clip>,
}

fn directive(line: &str) -> Option<(String, &str)> {
    let rest = line.strip_prefix('#')?;
    let (name, value) = match rest.split_once('=') {
        Some((n, v)) => (n, Some(v)),
        None => (rest, None),
    };
    let up = name.trim().to_ascii_uppercase();
    let known = matches!(up.as_str(), "TITLE" | "INFO" | "SORT" | "T" | "CASE")
        || (up.len() == 8 && up.starts_with("BRACKET") && up.ends_with(|c: char| c.is_ascii_digit()));
    (known && (value.is_some() || up == "INFO")).then(|| (up, value.unwrap_or("")))
}

pub(crate) fn parse_clip_file(text: &str) -> RawClipFile {
    #[derive(PartialEq)]
    enum St { Head, Info, Body }
    let mut f = RawClipFile::default();
    let mut st = St::Head;
    let mut body: Vec<&str> = Vec::new();
    let mut info: Vec<&str> = Vec::new();
    fn flush(f: &mut RawClipFile, body: &mut Vec<&str>) {
        if let Some(c) = f.clips.last_mut() {
            c.body = body.join("\n");
        }
        body.clear();
    }
    for line in text.lines() {
        if let Some((name, value)) = directive(line) {
            if st == St::Body { flush(&mut f, &mut body); }
            st = St::Head;
            match name.as_str() {
                "T" => {
                    f.clips.push(Clip { title: value.to_string(), body: String::new() });
                    st = St::Body;
                }
                "INFO" => {
                    if !value.is_empty() { info.push(value); }
                    st = St::Info;
                }
                _ => f.header.push((name, value.to_string())),
            }
        } else if line == "#" {
            if st == St::Body { flush(&mut f, &mut body); }
            st = St::Head;
        } else {
            match st {
                St::Body => body.push(line),
                St::Info if !line.starts_with(';') => info.push(line),
                _ => {}
            }
        }
    }
    if st == St::Body { flush(&mut f, &mut body); }
    f.info = info.join("\n");
    f
}

/// Parse the EditPlus `.ctl` text format (tolerant: unknown lines ignored).
pub fn parse_ctl(text: &str) -> Library {
    let raw = parse_clip_file(text);
    let get = |k: &str| raw.header.iter().find(|(n, _)| n == k).map(|(_, v)| v.trim().to_string());
    Library {
        title: get("TITLE").unwrap_or_default(),
        info: raw.info.clone(),
        sorted: get("SORT").is_some_and(|v| v.eq_ignore_ascii_case("y")),
        entries: raw.clips,
    }
}

/// Write the library back in `.ctl` text format.
pub fn to_ctl(lib: &Library) -> String {
    let mut s = format!("#TITLE={}\n", lib.title);
    if !lib.info.is_empty() {
        s.push_str("#INFO\n");
        s.push_str(&lib.info);
        s.push('\n');
    }
    s.push_str(if lib.sorted { "#SORT=y\n" } else { "#SORT=n\n" });
    for c in &lib.entries {
        s.push_str("#T=");
        s.push_str(&c.title);
        s.push('\n');
        if !c.body.is_empty() {
            s.push_str(&c.body);
            s.push_str("\n#\n");
        }
    }
    s
}

pub fn to_toml(lib: &Library) -> String {
    toml::to_string(lib).unwrap_or_default()
}

pub fn from_toml(text: &str) -> Result<Library, String> {
    toml::from_str(text).map_err(|e| e.to_string())
}

/// Load a library from `.ctl` (EditPlus text) or any other extension as TOML.
pub fn load_library(path: &Path) -> Result<Library, String> {
    let bytes = std::fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let text = decode_text(&bytes);
    if path.extension().is_some_and(|e| e.eq_ignore_ascii_case("ctl")) {
        Ok(parse_ctl(&text))
    } else {
        from_toml(&text)
    }
}

/// `YYYY-MM-DD` (UTC) from the system clock, for `^@`.
pub fn today() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    let z = secs.div_euclid(86_400) + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!("{y:04}-{m:02}-{d:02}")
}

pub fn commands() -> Vec<CommandSpec> {
    let mut v = vec![CommandSpec {
        id: "cliptext_open_panel".into(),
        caption: "Cliptext: Show panel".into(),
        category: "Cliptext".into(),
        default_chord: None,
        when: None, ..Default::default()
    }];
    for n in 1..=10 {
        v.push(CommandSpec {
            id: format!("cliptext_insert_{n}"),
            caption: format!("Cliptext: Insert entry {n}"),
            category: "Cliptext".into(),
            default_chord: Some(format!("ctrl+alt+{}", n % 10)),
            when: None, ..Default::default()
        });
    }
    v
}

/// `cliptext_insert_N` takes `args[0]` = library path (.ctl or .toml).
pub fn run(id: &str, ctx: &CmdCtx) -> Option<CmdOutcome> {
    if id == "cliptext_open_panel" {
        return Some(CmdOutcome { open_panel: Some(PanelId("cliptext".into())), ..Default::default() });
    }
    let n: usize = id.strip_prefix("cliptext_insert_")?.parse().ok()?;
    let fail = |m: String| Some(CmdOutcome { message: Some(m), ..Default::default() });
    let Some(path) = ctx.args.first() else { return fail("cliptext: no library".into()) };
    let lib = match load_library(Path::new(path)) {
        Ok(l) => l,
        Err(e) => return fail(e),
    };
    match n.checked_sub(1).and_then(|i| lib.entries.get(i)) {
        Some(clip) => Some(expand(clip, ctx, &today())),
        None => fail(format!("cliptext: no entry {n}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn clip(t: &str, b: &str) -> Clip {
        Clip { title: t.into(), body: b.into() }
    }

    fn outcome(c: &Clip, text: &str, sel: Region) -> (String, usize) {
        let s = [sel];
        let ctx = CmdCtx { text, selection: &s, ..Default::default() };
        let o = expand(c, &ctx, "2026-01-02");
        (o.edits.apply_to_str(text).unwrap(), o.selection.unwrap()[0].head)
    }

    #[test]
    fn wrap_selection_golden() {
        let (t, caret) = outcome(&clip("bold", "<b>^!</b>"), "x", Region { anchor: 0, head: 1 });
        assert_eq!(t, "<b>x</b>");
        assert_eq!(caret, 4);
    }

    #[test]
    fn caret_marker_without_selection() {
        let (t, caret) = outcome(&clip("p", "<p>^!</p>"), "ab", Region::caret(1));
        assert_eq!(t, "a<p></p>b");
        assert_eq!(caret, 4);
    }

    #[test]
    fn escapes_and_invalid_are_literal() {
        let e = expand_text("a^^b^#c^@d^%41e^%0ag^zh^%zz^", None, "D");
        assert_eq!(e.text, "a^b#cDdAe\ng^zh^%zz^");
        assert_eq!(e.caret, e.text.len());
    }

    #[test]
    fn title_only_inserts_title() {
        let (t, caret) = outcome(&clip("font-family: ", ""), "", Region::caret(0));
        assert_eq!((t.as_str(), caret), ("font-family: ", 13));
    }

    #[test]
    fn ctl_reader_and_roundtrip() {
        let src = "#TITLE=Mini\r\n#INFO\r\nSynthetic lib\r\n#SORT=y\r\n#T=bold\r\n<b>^!</b>\r\n#\r\n#T=a\r\nline1\r\nline2\r\n#T=hash\r\n^#x\r\n#\r\n#T=title only\r\n";
        let lib = parse_ctl(src);
        assert_eq!(lib.title, "Mini");
        assert_eq!(lib.info, "Synthetic lib");
        assert!(lib.sorted);
        assert_eq!(lib.entries.len(), 4);
        assert_eq!(lib.entries[1].body, "line1\nline2");
        assert_eq!(lib.entries[3].body, "");
        assert_eq!(parse_ctl(&to_ctl(&lib)), lib);
        assert_eq!(from_toml(&to_toml(&lib)).unwrap(), lib);
    }

    #[test]
    fn sort_search_edit() {
        let mut lib = Library { entries: vec![clip("str", ""), clip("Alpha", ""), clip("strcpy", "")], ..Default::default() };
        lib.sort();
        assert_eq!(lib.entries[0].title, "Alpha");
        assert_eq!(lib.search("STR").len(), 2);
        assert!(lib.remove(0).is_some() && lib.remove(9).is_none());
        assert!(lib.modify(0, clip("z", "")));
    }

    #[test]
    fn insert_chord_runs_nth_entry() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("l.toml");
        let lib = Library { entries: vec![clip("a", "1"), clip("b", "2"), clip("c", "3^!!")], ..Default::default() };
        std::fs::write(&p, to_toml(&lib)).unwrap();
        let args = [p.to_string_lossy().into_owned()];
        let ctx = CmdCtx { text: "", selection: &[], args: &args, ..Default::default() };
        let o = run("cliptext_insert_3", &ctx).unwrap();
        assert_eq!(o.edits.apply_to_str("").unwrap(), "3!");
        assert!(run("cliptext_insert_9", &ctx).unwrap().message.is_some());
        assert!(run("other", &ctx).is_none());
        assert_eq!(commands().len(), 11);
    }

    #[test]
    fn today_is_iso() {
        let t = today();
        assert_eq!((t.len(), &t[4..5]), (10, "-"));
    }
}
