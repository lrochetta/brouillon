//! `brouillon --import-editplus <dir> [--dry-run]`: one-shot EditPlus import
//! without opening a window. Reads the source folder, writes into the settings
//! directory (`--settings-dir`, portable data dir or `~/.brouillon`) and prints
//! the imported / approximated / skipped report.

use std::path::Path;

use brouillon_import::editplus_full::{apply, detect_dirs, plan_import_with_defaults, Plan, Status};
use brouillon_ui::onboarding_view as ui;

/// Build the plan for `src` (read-only). `defaults` = EditPlus program folder (shipped files are not re-imported).
pub fn plan(src: &Path, defaults: Option<&Path>, dest: &Path, known: &dyn Fn(&str) -> bool) -> Result<Plan, String> {
    if !src.is_dir() {
        return Err(format!("{} is not a folder", src.display()));
    }
    let existing = std::fs::read_to_string(dest.join("settings.toml")).unwrap_or_default();
    plan_import_with_defaults(src, defaults.filter(|d| d.is_dir() && *d != src), known, &existing).map_err(|e| e.to_string())
}

/// The program folder of EditPlus when `src` is its per-user folder (so shipped defaults are skipped).
pub fn auto_defaults(src: &Path) -> Option<std::path::PathBuf> {
    let (user, program) = detect_dirs();
    user.filter(|u| u == src).and(program)
}

/// The dialog's view of a plan.
pub fn to_preview(plan: &Plan) -> ui::ImportPreview {
    let st = |s| match s {
        Status::Imported => ui::Status::Imported,
        Status::Approximated => ui::Status::Approximated,
        Status::Skipped => ui::Status::Skipped,
    };
    ui::ImportPreview {
        rows: plan.report.items.iter().map(|i| ui::Row { status: st(i.status), category: i.category.to_string(), name: i.name.clone(), detail: i.detail.clone() }).collect(),
        files: plan.outputs.len(),
        unopened: plan.report.unopened.clone(),
    }
}

/// Which settings an import wrote that the editor does not read yet (precise list for the user).
pub fn ignored_notes(plan: &Plan) -> Vec<String> {
    let written = plan.outputs.iter().find(|(r, _)| r == "settings.toml").map(|(_, t)| t.as_str()).unwrap_or("");
    let table: toml::Table = written.parse().unwrap_or_default();
    brouillon_core::editor_prefs::IGNORED_KEYS
        .iter()
        .filter(|k| k.split_once('.').and_then(|(s, key)| table.get(s)?.as_table()?.get(key)).is_some())
        .map(|k| k.to_string())
        .collect()
}

/// What the user is told after an apply: what is live, what needs a restart, what is stored only.
pub fn apply_message(plan: &Plan) -> String {
    let has = |p: &str| plan.outputs.iter().any(|(r, _)| r == p || r.starts_with(&format!("{p}/")));
    let restart: Vec<&str> = [("tools.toml", "user tools"), ("cliptext", "cliptext"), ("abbrev", "auto-completion"), ("syntax", "syntax files"), ("templates", "templates")]
        .into_iter()
        .filter(|(p, _)| has(p))
        .map(|(_, n)| n)
        .collect();
    let mut m = format!("Imported {} file(s). Applied now: settings, theme, shortcuts, recent files.", plan.outputs.len());
    if !restart.is_empty() {
        m.push_str(&format!(" Restart Brouillon to load: {}.", restart.join(", ")));
    }
    let ignored = ignored_notes(plan);
    if !ignored.is_empty() {
        m.push_str(&format!(" Stored but not used yet: {}.", ignored.join(", ")));
    }
    m
}

/// Run the import. Returns the text to print and the process exit code
/// (0 = done, 1 = the source folder could not be read or the write failed).
pub fn run(src: &Path, dest: &Path, dry_run: bool) -> (String, i32) {
    run_with(src, None, dest, dry_run)
}

/// [`run`] with the EditPlus program folder detected from the environment when `src` is the per-user folder.
pub fn run_auto(src: &Path, dest: &Path, dry_run: bool) -> (String, i32) {
    run_with(src, auto_defaults(src).as_deref(), dest, dry_run)
}

pub fn run_with(src: &Path, defaults: Option<&Path>, dest: &Path, dry_run: bool) -> (String, i32) {
    let (reg, _) = crate::wiring::build_registry();
    let plan = match plan(src, defaults, dest, &|id| reg.contains(id)) {
        Ok(p) => p,
        Err(e) => return (format!("import failed: {e}\n"), 1),
    };
    let files: Vec<String> = plan.outputs.iter().map(|(r, _)| r.clone()).collect();
    if !dry_run {
        if let Err(e) = apply(&plan, dest) {
            return (format!("import failed while writing under {}: {e}\n", dest.display()), 1);
        }
    }
    (plan.report.render(dry_run, &dest.display().to_string(), &files), 0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(tag: &str) -> std::path::PathBuf {
        let p = std::env::temp_dir().join(format!("brouillon-impcli-{}-{tag}", std::process::id()));
        let _ = std::fs::remove_dir_all(&p);
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    fn source(tag: &str) -> std::path::PathBuf {
        let d = tmp(tag);
        std::fs::write(
            d.join("editplus.ini"),
            "[Settings]\nTabSize=2\n[Keyboard]\nSave As=Ctrl+Alt+S\nFrobnicate=Ctrl+F12\n[User Tools]\nMenu1=Build\nCommand1=make\n[Recent File List]\nFile1=C:\\a.txt\n",
        )
        .unwrap();
        std::fs::write(d.join("snip.ctl"), "#TITLE=S\n#T=x\nbody\n#\n").unwrap();
        std::fs::write(d.join("reg_u.ini"), "Code=SECRET").unwrap();
        d
    }

    #[test]
    fn dry_run_prints_report_and_writes_nothing() {
        let (src, dest) = (source("src1"), tmp("dry"));
        let (out, code) = run(&src, &dest, true);
        assert_eq!(code, 0);
        assert!(out.contains("dry run") && out.contains("Would write") && out.contains("tab_size = 2"), "{out}");
        assert!(out.contains("file_save_as") && out.contains("no Brouillon command"), "{out}");
        assert_eq!(std::fs::read_dir(&dest).unwrap().count(), 0);
    }

    #[test]
    fn real_run_writes_files_that_the_editor_loads() {
        let (src, dest) = (source("src2"), tmp("real"));
        let (out, code) = run(&src, &dest, false);
        assert_eq!(code, 0, "{out}");
        let (reg, _) = crate::wiring::build_registry();
        let text = std::fs::read_to_string(dest.join("settings.toml")).unwrap();
        let settings = crate::settings::Settings::build(&[(crate::settings::Layer::User, text.as_str())]);
        assert_eq!(settings.get_i64("editor.tab_size", 0), 2);
        assert!(settings.get_bool("keymap.classic", false));
        let (user, warns) = crate::keymap::parse_bindings_toml(&std::fs::read_to_string(dest.join("keymap.toml")).unwrap());
        assert!(warns.is_empty() && user.len() == 1 && reg.contains(&user[0].command));
        let tools = brouillon_editor_engine::tools_config::load(&dest.join("tools.toml"));
        assert_eq!(tools.tools.len(), 1);
        assert_eq!(crate::session::load_recent_from_dir(&dest), vec![std::path::PathBuf::from("C:\\a.txt")]);
        assert!(dest.join("cliptext/snip.toml").is_file());
        assert!(!out.contains("SECRET"));
    }

    #[test]
    fn gui_preview_apply_and_the_settings_the_editor_now_reads() {
        let src = tmp("gui-src");
        std::fs::write(
            src.join("editplus.ini"),
            "[Settings]\nTabSize=3\nIndentSize=2\nUse spaces for tab=0\nAuto indent=0\nLine number=0\nWord wrap=1\nWrap column=90\n[Font]\nName=Consolas\nSize=-13\n[Files]\nDefault encoding=windows-1252\n",
        )
        .unwrap();
        let dest = tmp("gui-dest");
        let (reg, _) = crate::wiring::build_registry();
        let plan = plan(&src, None, &dest, &|id| reg.contains(id)).unwrap();
        // dry run: the dialog's rows, nothing written
        let pv = to_preview(&plan);
        assert!(pv.count(ui::Status::Imported) >= 10, "{}", pv.summary());
        assert_eq!(std::fs::read_dir(&dest).unwrap().count(), 0);
        let row = |needle: &str| pv.rows.iter().find(|r| r.name.contains(needle)).unwrap_or_else(|| panic!("no row {needle}"));
        for k in ["TabSize", "IndentSize", "Use spaces", "Auto indent", "Line number", "Default encoding", "Word wrap", "Wrap column", "[Font] Name", "[Font] Size"] {
            assert_eq!(row(k).status, ui::Status::Imported, "{k} is read by the editor");
        }
        // apply, then the editor's own reader sees the values
        apply(&plan, &dest).unwrap();
        let text = std::fs::read_to_string(dest.join("settings.toml")).unwrap();
        let p = brouillon_core::editor_prefs::EditorPrefs::from_settings(&crate::onboarding::settings_of(&text));
        assert_eq!((p.tab_size, p.indent_size, p.insert_spaces, p.auto_indent), (3, 2, false, false));
        assert_eq!(p.default_encoding, "windows-1252");
        assert!(!crate::onboarding::settings_of(&text).get_bool("editor.line_numbers", true));
        // every imported key is read now: nothing is announced as "stored only"
        let m = apply_message(&plan);
        assert!(!m.contains("Stored but not used"), "{m}");
        assert!(ignored_notes(&plan).is_empty());
        let st = crate::onboarding::settings_of(&text);
        assert_eq!((st.get_str("editor.font_family", ""), st.get_i64("editor.font_size", 0)), ("Consolas".to_string(), 13));
        assert!(st.get_bool("editor.word_wrap", false) && st.get_i64("editor.wrap_column", 0) == 90);
        assert!(!m.contains("Restart"), "no tools/cliptext in this folder: {m}");
    }

    #[test]
    fn apply_message_asks_for_a_restart_when_tools_or_cliptext_are_present() {
        let (src, dest) = (source("src-msg"), tmp("dest-msg"));
        let (reg, _) = crate::wiring::build_registry();
        let m = apply_message(&plan(&src, None, &dest, &|id| reg.contains(id)).unwrap());
        assert!(m.contains("Restart Brouillon to load: user tools, cliptext."), "{m}");
    }

    #[test]
    fn missing_source_is_an_error_not_a_panic() {
        let (out, code) = run(Path::new("/definitely/not/here"), &tmp("x"), true);
        assert_eq!(code, 1);
        assert!(out.contains("not a folder"));
    }
}
