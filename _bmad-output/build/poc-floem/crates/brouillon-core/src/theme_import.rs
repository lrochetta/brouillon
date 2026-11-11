//! Theme importers (BK-060): EditPlus colour INI, Notepad++ style XML and
//! TextMate/Sublime colour schemes (JSON, `.sublime-color-scheme`, plist
//! `.tmTheme`) -> a Brouillon theme TOML (`name`, `is_dark`, `[colors]` with the
//! `Theme` slot names). Anything not understood is listed in the report.
//!
//! Assumption (EditPlus colour INI): lines are `Item = value` or
//! `Item Foreground|Background = value`; a value is `#rrggbb`, `0xBBGGRR`
//! hex, a bare hex with letters, or an all-digit decimal COLORREF (`0xBBGGRR`).

use serde::Serialize;
use std::collections::BTreeMap;

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize)]
pub struct ImportedTheme {
    pub name: String,
    pub is_dark: bool,
    /// Slot name (`bg`, `fg`, `syn_keyword`, ...) -> `#rrggbb`.
    pub colors: BTreeMap<String, String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ImportReport {
    /// Items/scopes/names that did not map to any slot (deduplicated).
    pub unknown: Vec<String>,
}

impl ImportReport {
    fn note(&mut self, what: &str) {
        if !self.unknown.iter().any(|u| u == what) {
            self.unknown.push(what.to_string());
        }
    }
}

impl ImportedTheme {
    pub fn to_toml(&self) -> String {
        toml::to_string(self).unwrap_or_default()
    }

    /// First value wins; also derives `is_dark` from the background.
    fn set(&mut self, slot: &str, color: Option<String>) {
        if let Some(c) = color {
            self.colors.entry(slot.to_string()).or_insert(c);
        }
    }

    fn finish(mut self) -> Self {
        if let Some(bg) = self.colors.get("bg") {
            let v = |i: usize| u32::from_str_radix(&bg[i..i + 2], 16).unwrap_or(0);
            self.is_dark = (v(1) * 299 + v(3) * 587 + v(5) * 114) / 1000 < 128;
        }
        self
    }
}

/// `#rgb`, `#rrggbb`, `#rrggbbaa`, bare `rrggbb` -> `#rrggbb` (alpha dropped).
pub fn parse_color(s: &str) -> Option<String> {
    let t = s.trim().trim_start_matches('#');
    if !t.chars().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    match t.len() {
        3 => Some(format!("#{}", t.chars().flat_map(|c| [c, c]).collect::<String>().to_lowercase())),
        6 | 8 => Some(format!("#{}", t[..6].to_lowercase())),
        _ => None,
    }
}

fn colorref(n: u32) -> String {
    format!("#{:02x}{:02x}{:02x}", n & 0xff, (n >> 8) & 0xff, (n >> 16) & 0xff)
}

// ---------------------------------------------------------------- EditPlus

fn editplus_value(v: &str) -> Option<String> {
    let v = v.trim();
    if let Some(h) = v.strip_prefix("0x").or_else(|| v.strip_prefix("0X")) {
        return u32::from_str_radix(h, 16).ok().map(colorref);
    }
    if !v.is_empty() && v.chars().all(|c| c.is_ascii_digit()) && !v.starts_with('#') {
        return v.parse::<u32>().ok().map(colorref);
    }
    parse_color(v)
}

pub fn from_editplus_ini(text: &str, name: &str) -> (ImportedTheme, ImportReport) {
    let mut t = ImportedTheme { name: name.into(), ..Default::default() };
    let mut rep = ImportReport::default();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with(';') || line.starts_with('[') {
            continue;
        }
        let Some((k, v)) = line.split_once('=') else { continue };
        let key = k.trim().to_lowercase();
        let (item, is_bg) = if let Some(i) = key.strip_suffix(" background").or_else(|| key.strip_suffix(" bg")) {
            (i.trim().to_string(), true)
        } else {
            let i = key.strip_suffix(" foreground").or_else(|| key.strip_suffix(" fg")).unwrap_or(&key);
            (i.trim().to_string(), false)
        };
        let color = editplus_value(v);
        let slot = match (item.as_str(), is_bg) {
            ("text", false) => "fg",
            ("text", true) => "bg",
            ("active line", true) => "bg_current_line",
            ("line number", false) => "fg_dim",
            ("keyword 1" | "keyword1", false) => "syn_keyword",
            ("keyword 2" | "keyword2", false) => "syn_type",
            ("line comment 1" | "line comment" | "block comment 1" | "block comment", false) => "syn_comment",
            ("quotation 1" | "quotation", false) => "syn_string",
            ("number", false) => "syn_number",
            ("matching brace", false) => "accent",
            _ => {
                rep.note(k.trim());
                continue;
            }
        };
        if color.is_none() {
            rep.note(k.trim());
        }
        t.set(slot, color);
    }
    (t.finish(), rep)
}

// ------------------------------------------------------------- Notepad++ XML

fn attrs(tag: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let mut rest = tag;
    while let Some(eq) = rest.find("=\"") {
        let key = rest[..eq].rsplit(char::is_whitespace).next().unwrap_or("").to_string();
        let after = &rest[eq + 2..];
        let Some(end) = after.find('"') else { break };
        out.push((key, after[..end].to_string()));
        rest = &after[end + 1..];
    }
    out
}

pub fn from_notepadpp_xml(text: &str, name: &str) -> (ImportedTheme, ImportReport) {
    let mut t = ImportedTheme { name: name.into(), ..Default::default() };
    let mut rep = ImportReport::default();
    for part in text.split('<').skip(1) {
        let tag = part.split('>').next().unwrap_or("");
        let kind = tag.split_whitespace().next().unwrap_or("");
        if kind != "WidgetStyle" && kind != "WordsStyle" {
            continue;
        }
        let a = attrs(tag);
        let get = |k: &str| a.iter().find(|(n, _)| n == k).map(|(_, v)| v.as_str());
        let n = get("name").unwrap_or("");
        let fg = get("fgColor").and_then(parse_color);
        let bg = get("bgColor").and_then(parse_color);
        match n.to_lowercase().as_str() {
            "default style" => {
                t.set("fg", fg);
                t.set("bg", bg);
            }
            "current line background colour" => t.set("bg_current_line", bg),
            "line number margin" => t.set("fg_dim", fg),
            "caret colour" => t.set("accent", fg),
            "instruction word" | "keyword" | "keywords" => t.set("syn_keyword", fg),
            "type word" | "type" | "keywords2" => t.set("syn_type", fg),
            "comment" | "comment line" | "comment doc" | "commentline" => t.set("syn_comment", fg),
            "string" | "double quoted string" | "stringdq" => t.set("syn_string", fg),
            "number" => t.set("syn_number", fg),
            "operator" => t.set("syn_punct", fg),
            _ => rep.note(n),
        }
    }
    (t.finish(), rep)
}

// ------------------------------------------------- TextMate / Sublime schemes

#[derive(Debug, Clone)]
enum J {
    Str(String),
    Arr(Vec<J>),
    Obj(Vec<(String, J)>),
    Other,
}

impl J {
    fn get(&self, k: &str) -> Option<&J> {
        match self {
            J::Obj(v) => v.iter().find(|(n, _)| n == k).map(|(_, v)| v),
            _ => None,
        }
    }
    fn str(&self) -> Option<&str> {
        match self {
            J::Str(s) => Some(s),
            _ => None,
        }
    }
}

/// Tolerant JSON (comments and trailing commas allowed, as in VS Code themes).
struct JsonParser {
    c: Vec<char>,
    i: usize,
}

impl JsonParser {
    fn ws(&mut self) {
        loop {
            match self.c.get(self.i) {
                Some(c) if c.is_whitespace() => self.i += 1,
                Some('/') if self.c.get(self.i + 1) == Some(&'/') => {
                    while self.c.get(self.i).is_some_and(|&c| c != '\n') { self.i += 1 }
                }
                Some('/') if self.c.get(self.i + 1) == Some(&'*') => {
                    self.i += 2;
                    while self.i < self.c.len() && !(self.c[self.i] == '*' && self.c.get(self.i + 1) == Some(&'/')) { self.i += 1 }
                    self.i += 2;
                }
                _ => break,
            }
        }
    }
    fn string(&mut self) -> Option<String> {
        self.i += 1; // opening quote
        let mut s = String::new();
        while let Some(&c) = self.c.get(self.i) {
            self.i += 1;
            match c {
                '"' => return Some(s),
                '\\' => {
                    let e = *self.c.get(self.i)?;
                    self.i += 1;
                    match e {
                        'n' => s.push('\n'),
                        't' => s.push('\t'),
                        'u' => {
                            let h: String = self.c.get(self.i..self.i + 4)?.iter().collect();
                            s.push(char::from_u32(u32::from_str_radix(&h, 16).ok()?).unwrap_or('?'));
                            self.i += 4;
                        }
                        o => s.push(o),
                    }
                }
                c => s.push(c),
            }
        }
        None
    }
    fn value(&mut self) -> Option<J> {
        self.ws();
        match *self.c.get(self.i)? {
            '"' => self.string().map(J::Str),
            '[' => {
                self.i += 1;
                let mut v = Vec::new();
                loop {
                    self.ws();
                    if self.c.get(self.i) == Some(&']') { self.i += 1; return Some(J::Arr(v)); }
                    v.push(self.value()?);
                    self.ws();
                    if self.c.get(self.i) == Some(&',') { self.i += 1; }
                }
            }
            '{' => {
                self.i += 1;
                let mut v = Vec::new();
                loop {
                    self.ws();
                    match self.c.get(self.i)? {
                        '}' => { self.i += 1; return Some(J::Obj(v)); }
                        '"' => {}
                        _ => return None,
                    }
                    let k = self.string()?;
                    self.ws();
                    if self.c.get(self.i) != Some(&':') { return None; }
                    self.i += 1;
                    v.push((k, self.value()?));
                    self.ws();
                    if self.c.get(self.i) == Some(&',') { self.i += 1; }
                }
            }
            _ => {
                let start = self.i;
                while self.c.get(self.i).is_some_and(|c| !",]} \t\r\n".contains(*c)) { self.i += 1 }
                (self.i > start).then_some(J::Other)
            }
        }
    }
}

/// Minimal Apple plist XML reader (dict/array/string; other scalars -> `Other`).
fn plist_value(s: &str, pos: &mut usize) -> Option<J> {
    fn next_tag<'a>(s: &'a str, pos: &mut usize) -> Option<(&'a str, &'a str)> {
        loop {
            let open = s[*pos..].find('<')? + *pos;
            let close = s[open..].find('>')? + open;
            *pos = close + 1;
            let tag = &s[open + 1..close];
            if !(tag.starts_with('?') || tag.starts_with('!')) {
                return Some((tag, &s[close + 1..]));
            }
        }
    }
    let (tag, _) = next_tag(s, pos)?;
    let name = tag.trim_end_matches('/').trim();
    match name {
        n if n.starts_with("plist") => plist_value(s, pos),
        "dict" if !tag.ends_with('/') => {
            let mut v = Vec::new();
            loop {
                let save = *pos;
                let (t, _) = next_tag(s, pos)?;
                if t == "/dict" { return Some(J::Obj(v)); }
                if t != "key" { *pos = save; return None; }
                let end = s[*pos..].find("</key>")? + *pos;
                let key = s[*pos..end].trim().to_string();
                *pos = end + 6;
                v.push((key, plist_value(s, pos)?));
            }
        }
        "array" if !tag.ends_with('/') => {
            let mut v = Vec::new();
            loop {
                let save = *pos;
                let (t, _) = next_tag(s, pos)?;
                if t == "/array" { return Some(J::Arr(v)); }
                *pos = save;
                v.push(plist_value(s, pos)?);
            }
        }
        "string" if !tag.ends_with('/') => {
            let end = s[*pos..].find("</string>")? + *pos;
            let text = s[*pos..end].replace("&amp;", "&").replace("&lt;", "<").replace("&gt;", ">");
            *pos = end + 9;
            Some(J::Str(text))
        }
        "dict" | "array" | "string" => Some(J::Other),
        _ => {
            // <true/>, <integer>..</integer>, etc.
            if !tag.ends_with('/') {
                let end = s[*pos..].find(&format!("</{name}>"))? + *pos;
                *pos = end + name.len() + 3;
            }
            Some(J::Other)
        }
    }
}

/// Scope prefixes -> slot, most specific first.
const SCOPE_SLOTS: &[(&str, &str)] = &[
    ("comment", "syn_comment"),
    ("string", "syn_string"),
    ("constant.numeric", "syn_number"),
    ("keyword.operator", "syn_punct"),
    ("punctuation", "syn_punct"),
    ("entity.name.type", "syn_type"),
    ("entity.name.class", "syn_type"),
    ("support.type", "syn_type"),
    ("support.class", "syn_type"),
    ("storage.type", "syn_type"),
    ("keyword", "syn_keyword"),
    ("storage", "syn_keyword"),
];

fn scope_slot(scope: &str) -> Option<&'static str> {
    let s = scope.trim();
    SCOPE_SLOTS
        .iter()
        .find(|(p, _)| s == *p || s.strip_prefix(p).is_some_and(|r| r.starts_with('.')))
        .map(|(_, slot)| *slot)
}

/// Import a TextMate-family colour scheme. Accepts VS Code theme JSON
/// (`colors` + `tokenColors`), Sublime `.sublime-color-scheme` (`globals` +
/// `rules`) and plist `.tmTheme`. Format is detected from the first character.
pub fn from_textmate(text: &str, name: &str) -> Result<(ImportedTheme, ImportReport), String> {
    let text = text.trim_start_matches('\u{feff}');
    let root = if text.trim_start().starts_with('<') {
        let mut pos = 0;
        plist_value(text, &mut pos).ok_or("invalid plist")?
    } else {
        JsonParser { c: text.chars().collect(), i: 0 }.value().ok_or("invalid JSON")?
    };
    let mut t = ImportedTheme { name: name.into(), ..Default::default() };
    let mut rep = ImportReport::default();
    let vars = root.get("variables");
    let color = |j: Option<&J>| -> Option<String> {
        let s = j?.str()?;
        if let Some(v) = s.strip_prefix("var(").and_then(|r| r.strip_suffix(')')) {
            return parse_color(vars?.get(v)?.str()?);
        }
        parse_color(s)
    };
    // globals
    if let Some(c) = root.get("colors") {
        t.set("bg", color(c.get("editor.background")));
        t.set("fg", color(c.get("editor.foreground")));
        t.set("bg_current_line", color(c.get("editor.lineHighlightBackground")));
        t.set("fg_dim", color(c.get("editorLineNumber.foreground")));
        t.set("accent", color(c.get("editorCursor.foreground")));
    }
    if let Some(g) = root.get("globals") {
        t.set("bg", color(g.get("background")));
        t.set("fg", color(g.get("foreground")));
        t.set("bg_current_line", color(g.get("line_highlight")));
        t.set("accent", color(g.get("caret")));
    }
    let mut rules: Vec<(String, Option<String>)> = Vec::new();
    let mut add_rule = |scope: Option<&J>, fg: Option<String>| match scope {
        Some(J::Str(s)) => rules.extend(s.split(',').map(|p| (p.trim().to_string(), fg.clone()))),
        Some(J::Arr(a)) => rules.extend(a.iter().filter_map(J::str).map(|p| (p.trim().to_string(), fg.clone()))),
        _ => {}
    };
    if let Some(J::Arr(a)) = root.get("tokenColors") {
        for r in a {
            add_rule(r.get("scope"), color(r.get("settings").and_then(|s| s.get("foreground"))));
        }
    }
    if let Some(J::Arr(a)) = root.get("rules") {
        for r in a {
            add_rule(r.get("scope"), color(r.get("foreground")));
        }
    }
    if let Some(J::Arr(a)) = root.get("settings") {
        for r in a {
            let Some(s) = r.get("settings") else { continue };
            if r.get("scope").is_none() {
                t.set("bg", color(s.get("background")));
                t.set("fg", color(s.get("foreground")));
                t.set("bg_current_line", color(s.get("lineHighlight")));
                t.set("accent", color(s.get("caret")));
            } else {
                add_rule(r.get("scope"), color(s.get("foreground")));
            }
        }
    }
    for (scope, fg) in rules {
        // descendant selectors ("source.js string"): classify by the last token
        let last = scope.split_whitespace().last().unwrap_or("");
        match scope_slot(last) {
            Some(slot) => t.set(slot, fg),
            None if !last.is_empty() => rep.note(last),
            None => {}
        }
    }
    Ok((t.finish(), rep))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn editplus_ini_fixture() {
        let ini = "[Colors]\n; synthetic\nText Foreground=#d0d0d0\nText Background=#101010\nActive line Background=0x00202020\nKeyword 1=255\nQuotation 1=00ff00\nBogus Item=#fff\nNumber=zzz\n";
        let (t, rep) = from_editplus_ini(ini, "Imp");
        assert_eq!(t.colors["fg"], "#d0d0d0");
        assert_eq!(t.colors["bg"], "#101010");
        assert_eq!(t.colors["bg_current_line"], "#202020");
        assert_eq!(t.colors["syn_keyword"], "#ff0000"); // COLORREF 255 = BGR red
        assert_eq!(t.colors["syn_string"], "#00ff00");
        assert!(t.is_dark);
        assert_eq!(rep.unknown, vec!["Bogus Item", "Number"]);
        assert!(t.to_toml().contains("[colors]"));
    }

    #[test]
    fn notepadpp_xml_fixture() {
        let xml = r##"<NotepadPlus><LexerStyles><LexerType name="x"><WordsStyle name="COMMENT" styleID="1" fgColor="008000" bgColor="FFFFFF"/><WordsStyle name="INSTRUCTION WORD" styleID="5" fgColor="0000FF"/><WordsStyle name="WEIRD" styleID="9" fgColor="123456"/></LexerType></LexerStyles>
<GlobalStyles><WidgetStyle name="Default Style" styleID="32" fgColor="000000" bgColor="FFFFFF" /><WidgetStyle name="Caret colour" styleID="2069" fgColor="FF0000" /><WidgetStyle name="Fold" fgColor="000000"/></GlobalStyles></NotepadPlus>"##;
        let (t, rep) = from_notepadpp_xml(xml, "N");
        assert_eq!(t.colors["syn_comment"], "#008000");
        assert_eq!(t.colors["syn_keyword"], "#0000ff");
        assert_eq!(t.colors["bg"], "#ffffff");
        assert_eq!(t.colors["accent"], "#ff0000");
        assert!(!t.is_dark);
        assert_eq!(rep.unknown, vec!["WEIRD", "Fold"]);
    }

    #[test]
    fn vscode_json_with_comments_and_unknown_scope() {
        let json = r##"{
  // synthetic
  "name": "T",
  "colors": {"editor.background": "#112233", "editor.foreground": "#eeeeee",},
  "tokenColors": [
    {"scope": ["comment", "string.quoted"], "settings": {"foreground": "#888"}},
    {"scope": "keyword.operator", "settings": {"foreground": "#ff00ff"}},
    {"scope": "keyword.control, storage", "settings": {"foreground": "#ffaa00"}},
    {"scope": "markup.heading", "settings": {"foreground": "#00aaff"}},
  ]
}"##;
        let (t, rep) = from_textmate(json, "T").unwrap();
        assert_eq!(t.colors["bg"], "#112233");
        assert_eq!(t.colors["syn_comment"], "#888888");
        assert_eq!(t.colors["syn_string"], "#888888");
        assert_eq!(t.colors["syn_punct"], "#ff00ff");
        assert_eq!(t.colors["syn_keyword"], "#ffaa00");
        assert_eq!(rep.unknown, vec!["markup.heading"]);
        assert!(t.is_dark);
    }

    #[test]
    fn sublime_scheme_with_variables() {
        let json = r##"{"variables": {"red": "#ff0000"}, "globals": {"background": "white", "foreground": "var(red)"},
 "rules": [{"scope": "constant.numeric.float", "foreground": "var(red)"}]}"##;
        let (t, _) = from_textmate(json, "S").unwrap();
        assert_eq!(t.colors["fg"], "#ff0000");
        assert_eq!(t.colors["syn_number"], "#ff0000");
        assert!(!t.colors.contains_key("bg")); // "white" is not hex: skipped
    }

    #[test]
    fn tmtheme_plist() {
        let plist = r##"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict><key>name</key><string>P</string><key>settings</key><array>
<dict><key>settings</key><dict><key>background</key><string>#FFFFFF</string><key>foreground</key><string>#000000</string><key>caret</key><string>#0000FF</string></dict></dict>
<dict><key>name</key><string>c</string><key>scope</key><string>comment</string><key>settings</key><dict><key>fontStyle</key><string>italic</string><key>foreground</key><string>#AAAAAA</string></dict></dict>
<dict><key>scope</key><string>support.function</string><key>settings</key><dict><key>foreground</key><string>#112233</string></dict></dict>
</array></dict></plist>"##;
        let (t, rep) = from_textmate(plist, "P").unwrap();
        assert_eq!(t.colors["bg"], "#ffffff");
        assert_eq!(t.colors["accent"], "#0000ff");
        assert_eq!(t.colors["syn_comment"], "#aaaaaa");
        assert_eq!(rep.unknown, vec!["support.function"]);
        assert!(from_textmate("{ nope", "x").is_err());
    }
}
