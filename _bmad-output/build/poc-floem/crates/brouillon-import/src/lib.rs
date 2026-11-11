//! Lot 8: one-shot importers (BK-059/060/061). EditPlus folder -> Brouillon TOML,
//! theme files -> theme TOML, `.stx` -> keyword table. Nothing here runs at edit time.

pub mod editplus;
pub mod editplus_full;
pub mod hardening;
pub mod others;
pub mod stx;

use brouillon_core::contract::{CmdCtx, CmdOutcome, CommandSpec};
use brouillon_core::theme_import;
use std::path::Path;

pub fn commands() -> Vec<CommandSpec> {
    let spec = |id: &str, caption: &str| CommandSpec {
        id: id.into(),
        caption: caption.into(),
        category: "Import".into(),
        default_chord: None,
        when: None, ..Default::default()
    };
    vec![
        spec("import_editplus_folder", "Import: EditPlus folder"),
        spec("import_theme", "Import: Theme file"),
        spec("import_stx", "Import: EditPlus .stx to keyword table"),
    ]
}

fn msg(m: String) -> Option<CmdOutcome> {
    Some(CmdOutcome { message: Some(m), ..Default::default() })
}

/// All commands take `args = [source, destination]` (destination folder for
/// `import_editplus_folder`, output file otherwise).
pub fn run(id: &str, ctx: &CmdCtx) -> Option<CmdOutcome> {
    if !matches!(id, "import_editplus_folder" | "import_theme" | "import_stx") {
        return None;
    }
    let [src, dest, ..] = ctx.args else { return msg("import: expected source and destination".into()) };
    let (src, dest) = (Path::new(src), Path::new(dest));
    let res = match id {
        "import_editplus_folder" => import_folder(src, dest),
        "import_theme" => import_theme(src, dest),
        _ => import_stx(src, dest),
    };
    msg(res.unwrap_or_else(|e| format!("import failed: {e}")))
}

fn import_folder(src: &Path, dest: &Path) -> Result<String, String> {
    let plan = editplus::plan_from_folder(src).map_err(|e| e.to_string())?;
    editplus::write_plan(&plan, dest).map_err(|e| e.to_string())?;
    Ok(format!(
        "imported {} files, {} secret keys dropped, {} files skipped",
        plan.report.converted.len(),
        plan.report.skipped_keys.len(),
        plan.report.skipped_files.len()
    ))
}

fn import_theme(src: &Path, dest: &Path) -> Result<String, String> {
    let text = brouillon_editor_engine::cliptext::decode_text(&std::fs::read(src).map_err(|e| e.to_string())?);
    let name = src.file_stem().and_then(|s| s.to_str()).unwrap_or("imported");
    let ext = src.extension().and_then(|e| e.to_str()).unwrap_or("").to_lowercase();
    let (theme, report) = match ext.as_str() {
        "ini" => theme_import::from_editplus_ini(&text, name),
        "xml" => theme_import::from_notepadpp_xml(&text, name),
        _ => theme_import::from_textmate(&text, name)?,
    };
    std::fs::write(dest, theme.to_toml()).map_err(|e| e.to_string())?;
    Ok(format!("theme '{name}' imported, {} unknown items: {}", report.unknown.len(), report.unknown.join(", ")))
}

fn import_stx(src: &Path, dest: &Path) -> Result<String, String> {
    let bytes = std::fs::read(src).map_err(|e| e.to_string())?;
    let (table, warnings) = stx::convert_stx_bytes(&bytes)?;
    std::fs::write(dest, stx::to_definition_toml(&table)).map_err(|e| e.to_string())?;
    Ok(format!("{} keyword groups, {} warnings", table.groups.len(), warnings.len()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn commands_route_and_report() {
        assert_eq!(commands().len(), 3);
        let d = crate::testutil::TempDir::new();
        let src = d.path().join("t.stx");
        std::fs::write(&src, "#TITLE=X\n#KEYWORD=k\nfoo\n").unwrap();
        let out = d.path().join("o.toml");
        let args = [src.to_string_lossy().into_owned(), out.to_string_lossy().into_owned()];
        let ctx = CmdCtx { args: &args, ..Default::default() };
        let o = run("import_stx", &ctx).unwrap();
        assert_eq!(o.message.as_deref(), Some("1 keyword groups, 0 warnings"));
        assert!(std::fs::read_to_string(out).unwrap().contains("foo"));
        assert!(run("nope", &ctx).is_none());
        let bad = CmdCtx::default();
        assert!(run("import_theme", &bad).unwrap().message.unwrap().contains("expected"));
    }
}

/// Test-only scratch dir (this crate does not depend on `tempfile`).
#[cfg(test)]
pub(crate) mod testutil {
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicUsize, Ordering};

    static N: AtomicUsize = AtomicUsize::new(0);

    pub struct TempDir(PathBuf);

    impl TempDir {
        pub fn new() -> Self {
            let n = N.fetch_add(1, Ordering::SeqCst);
            let p = std::env::temp_dir().join(format!("brouillon-import-{}-{n}", std::process::id()));
            std::fs::create_dir_all(&p).expect("temp dir");
            Self(p)
        }
        pub fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
}
