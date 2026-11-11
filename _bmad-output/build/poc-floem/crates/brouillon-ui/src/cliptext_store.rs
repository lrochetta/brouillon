//! Cliptext libraries on disk: the default library (generic, useful snippets, created on first
//! use), the list of libraries in the user's `cliptext` folder, saving, and which library the
//! `Ctrl+Alt+1..0` chords insert from. Pure file logic; the panel view binds to it.
//!
//! Body escapes (see `brouillon_editor_engine::cliptext`): `^!` caret position (wraps the
//! selection), `^^` a literal `^`, `^@` today's date (ISO).

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use brouillon_editor_engine::cliptext::{load_library, to_ctl, to_toml, Clip, Library};
use brouillon_editor_engine::durable::durable_write;

/// File name of the library created for a new user.
pub const DEFAULT_FILE: &str = "default.toml";

fn clip(title: &str, body: &str) -> Clip {
    Clip { title: title.to_string(), body: body.to_string() }
}

const MIT_TEXT: &str = "MIT License

Copyright (c) ^!<year> <copyright holder>

Permission is hereby granted, free of charge, to any person obtaining a copy
of this software and associated documentation files (the \"Software\"), to deal
in the Software without restriction, including without limitation the rights
to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
copies of the Software, and to permit persons to whom the Software is
furnished to do so, subject to the following conditions:

The above copyright notice and this permission notice shall be included in all
copies or substantial portions of the Software.

THE SOFTWARE IS PROVIDED \"AS IS\", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
SOFTWARE.
";

/// The library every new user gets: ten generic entries, so `Ctrl+Alt+1..0` reach all of them.
pub fn default_library() -> Library {
    Library {
        title: "Default".into(),
        info: "Generic snippets. Edit this file freely: ^! marks where the caret lands (and wraps a selection), ^@ is today's date, ^^ is a literal ^.".into(),
        sorted: false,
        entries: vec![
            clip("MIT license header (short)", "// SPDX-License-Identifier: MIT\n// Copyright (c) ^!<year> <copyright holder>\n"),
            clip("MIT license (full text)", MIT_TEXT),
            clip("Date (ISO)", "^@"),
            clip("Block comment", "/*\n * ^!\n */"),
            clip("Section banner", "// ---------- ^! ----------"),
            clip("TODO note", "TODO(^@): ^!"),
            clip("Lorem ipsum (short)", "Lorem ipsum dolor sit amet, consectetur adipiscing elit, sed do eiusmod tempor incididunt ut labore et dolore magna aliqua."),
            clip("Markdown link", "[^!text](https://)"),
            clip("Markdown table (2 columns)", "| ^!Column A | Column B |\n| --- | --- |\n|  |  |\n"),
            clip("HTML5 page", "<!DOCTYPE html>\n<html lang=\"en\">\n<head>\n  <meta charset=\"utf-8\">\n  <title>^!</title>\n</head>\n<body>\n\n</body>\n</html>\n"),
        ],
    }
}

/// Library path used by `Ctrl+Alt+N` and pre-selected in the panel. `None` = the default one.
static ACTIVE: Mutex<Option<PathBuf>> = Mutex::new(None);

pub fn set_active(path: &Path) {
    *ACTIVE.lock().unwrap() = Some(path.to_path_buf());
}

/// The default library file in `dir`, written when missing (a user's edits are never overwritten).
pub fn ensure_default(dir: &Path) -> PathBuf {
    let path = dir.join(DEFAULT_FILE);
    if !path.exists() {
        let _ = std::fs::create_dir_all(dir);
        if let Err(e) = durable_write(&path, to_toml(&default_library()).as_bytes()) {
            eprintln!("[cliptext] could not create {}: {e}", path.display());
        }
    }
    path
}

/// The library `Ctrl+Alt+N` inserts from: the one chosen in the panel, else the default.
pub fn active_library(dir: &Path) -> PathBuf {
    let default = ensure_default(dir);
    match ACTIVE.lock().unwrap().clone() {
        Some(p) if p.is_file() => p,
        _ => default,
    }
}

/// Every library of `dir` (`.toml`, `.ctl`) as `(path, library)`, the default first, then by name.
/// Unreadable files are skipped with a note on stderr.
pub fn list_libraries(dir: &Path) -> Vec<(PathBuf, Library)> {
    let default = ensure_default(dir);
    let mut paths: Vec<PathBuf> = std::fs::read_dir(dir)
        .map(|rd| {
            rd.filter_map(Result::ok)
                .map(|e| e.path())
                .filter(|p| p.is_file() && p.extension().is_some_and(|x| x.eq_ignore_ascii_case("toml") || x.eq_ignore_ascii_case("ctl")))
                .collect()
        })
        .unwrap_or_default();
    paths.sort_by_key(|p| (p != &default, p.file_name().map(|n| n.to_string_lossy().to_lowercase())));
    paths
        .into_iter()
        .filter_map(|p| match load_library(&p) {
            Ok(l) => Some((p, l)),
            Err(e) => {
                eprintln!("[cliptext] {e}");
                None
            }
        })
        .collect()
}

/// Write `lib` back in the format its extension says.
pub fn save_library(path: &Path, lib: &Library) -> Result<(), String> {
    let text = if path.extension().is_some_and(|x| x.eq_ignore_ascii_case("ctl")) { to_ctl(lib) } else { to_toml(lib) };
    durable_write(path, text.as_bytes()).map_err(|e| format!("{}: {e}", path.display()))
}

/// Entry made from selected text: the title is its first line, the body is the text with `^`
/// escaped so it is inserted exactly as it was.
pub fn clip_from_selection(selected: &str) -> Option<Clip> {
    let first = selected.lines().map(str::trim).find(|l| !l.is_empty())?;
    let title: String = first.chars().take(40).collect();
    Some(Clip { title, body: selected.replace('^', "^^") })
}

/// First line of the body for the list preview (escapes shown as they will be inserted).
pub fn preview(c: &Clip) -> String {
    let text = if c.body.is_empty() { c.title.as_str() } else { c.body.as_str() };
    let line = text.lines().map(str::trim).find(|l| !l.is_empty()).unwrap_or("");
    let line = line.replace("^!", "").replace("^@", "<date>").replace("^^", "^");
    line.trim().chars().take(46).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use brouillon_core::contract::{CmdCtx, Region};
    use brouillon_editor_engine::cliptext::{expand, from_toml};

    fn tmp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("brouillon-cliptext-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        d
    }

    fn apply(clip: &Clip, text: &str, sel: Region) -> (String, usize) {
        let s = [sel];
        let ctx = CmdCtx { text, selection: &s, ..Default::default() };
        let o = expand(clip, &ctx, "2026-11-05");
        (o.edits.apply_to_str(text).unwrap(), o.selection.unwrap()[0].head)
    }

    #[test]
    fn default_library_is_useful_generic_and_reaches_every_chord() {
        let lib = default_library();
        assert_eq!(lib.entries.len(), 10, "Ctrl+Alt+1..0 reach all ten");
        let mut titles: Vec<_> = lib.entries.iter().map(|c| c.title.clone()).collect();
        titles.sort();
        titles.dedup();
        assert_eq!(titles.len(), 10, "unique titles");
        // The wanted kinds are there.
        for want in ["MIT license header", "Date", "Block comment", "Lorem ipsum"] {
            assert!(lib.entries.iter().any(|c| c.title.starts_with(want)), "{want}");
        }
        // No third-party product names anywhere.
        let all = to_toml(&lib).to_lowercase();
        for banned in ["editplus", "notepad", "sublime", "vscode", "visual studio", "jetbrains", "emacs"] {
            assert!(!all.contains(banned), "{banned}");
        }
        // The TOML round-trips (it is what is written to the user's folder).
        assert_eq!(from_toml(&to_toml(&lib)).unwrap(), lib);
    }

    #[test]
    fn entries_expand_as_documented() {
        let lib = default_library();
        let by = |t: &str| lib.entries.iter().find(|c| c.title.starts_with(t)).unwrap().clone();
        // Date.
        assert_eq!(apply(&by("Date"), "x", Region::caret(1)).0, "x2026-11-05");
        // MIT header: SPDX id, caret on the year placeholder, to fill in.
        let (t, caret) = apply(&by("MIT license header"), "", Region::caret(0));
        assert!(t.starts_with("// SPDX-License-Identifier: MIT\n// Copyright (c) <year> <copyright holder>"), "{t}");
        assert_eq!(&t[caret..caret + 6], "<year>");
        // Full text keeps the standard wording.
        let (t, _) = apply(&by("MIT license (full"), "", Region::caret(0));
        assert!(t.contains("Permission is hereby granted, free of charge") && t.contains("WITHOUT WARRANTY OF ANY KIND") && !t.contains('^'));
        // Block comment wraps the selection.
        let (t, caret) = apply(&by("Block comment"), "note", Region { anchor: 0, head: 4 });
        assert_eq!(t, "/*\n * note\n */");
        assert_eq!(caret, 10, "right after the wrapped selection");
        // No stray escape is left in any entry.
        for c in &lib.entries {
            let (t, _) = apply(c, "", Region::caret(0));
            assert!(!t.contains('^') && !t.is_empty(), "{}: {t:?}", c.title);
        }
    }

    #[test]
    fn default_file_is_created_once_and_never_overwritten() {
        let dir = tmp("default");
        let p = ensure_default(&dir);
        assert_eq!(p, dir.join(DEFAULT_FILE));
        assert_eq!(load_library(&p).unwrap(), default_library());
        // The user edits it: a later call must keep the edit.
        let mut mine = default_library();
        mine.entries.truncate(2);
        save_library(&p, &mine).unwrap();
        ensure_default(&dir);
        assert_eq!(load_library(&p).unwrap().entries.len(), 2);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn libraries_are_listed_default_first_and_ctl_is_read() {
        let dir = tmp("list");
        ensure_default(&dir);
        std::fs::write(dir.join("a.ctl"), "#TITLE=Mine\n#T=hello\nworld^!\n#\n").unwrap();
        std::fs::write(dir.join("broken.toml"), "entry = [[[").unwrap();
        std::fs::write(dir.join("notes.txt"), "ignored").unwrap();
        let libs = list_libraries(&dir);
        let names: Vec<_> = libs.iter().map(|(p, _)| p.file_name().unwrap().to_string_lossy().into_owned()).collect();
        assert_eq!(names, vec!["default.toml", "a.ctl"], "broken and non-library files are skipped");
        assert_eq!(libs[1].1.title, "Mine");
        // Saving a .ctl keeps the .ctl format.
        save_library(&dir.join("a.ctl"), &libs[1].1).unwrap();
        assert!(std::fs::read_to_string(dir.join("a.ctl")).unwrap().starts_with("#TITLE=Mine"));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn active_library_follows_the_panel_choice_and_falls_back_to_the_default() {
        let dir = tmp("active");
        let default = active_library(&dir);
        assert_eq!(default, dir.join(DEFAULT_FILE));
        std::fs::write(dir.join("b.toml"), "title = \"B\"\n").unwrap();
        set_active(&dir.join("b.toml"));
        assert_eq!(active_library(&dir), dir.join("b.toml"));
        std::fs::remove_file(dir.join("b.toml")).unwrap();
        assert_eq!(active_library(&dir), dir.join(DEFAULT_FILE), "a vanished library falls back");
        *ACTIVE.lock().unwrap() = None;
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn selection_becomes_an_entry_inserted_verbatim() {
        assert!(clip_from_selection("  \n ").is_none());
        let c = clip_from_selection("\n  a ^ b\nsecond").unwrap();
        assert_eq!(c.title, "a ^ b");
        let (t, _) = apply(&c, "", Region::caret(0));
        assert_eq!(t, "\n  a ^ b\nsecond", "escapes round-trip");
        assert_eq!(clip_from_selection(&"x".repeat(100)).unwrap().title.len(), 40);
    }

    #[test]
    fn preview_hides_escapes() {
        assert_eq!(preview(&clip("t", "<b>^!</b>")), "<b></b>");
        assert_eq!(preview(&clip("t", "\n\n  TODO(^@): ^!")), "TODO(<date>):");
        assert_eq!(preview(&clip("only title", "")), "only title");
    }
}
