//! Import of a user's own EditPlus folder (BK-059): runtime, read-only on the
//! source. Cliptext (`*.ctl`), abbreviations (`*.acp`), `template.*`, and the
//! `workspace*`, `tool*`, `regex*` INI files become Brouillon TOML under a
//! destination folder. Secrets are never copied; `reg_u.ini` is never opened.

use brouillon_editor_engine::{acp, cliptext};
use std::fs;
use std::io;
use std::path::Path;

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Report {
    /// Relative output paths produced.
    pub converted: Vec<String>,
    /// `file: [section] key` for every key dropped as a secret.
    pub skipped_keys: Vec<String>,
    /// Source files left alone (not part of the import, or never read).
    pub skipped_files: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ImportPlan {
    /// (relative path, content)
    pub outputs: Vec<(String, String)>,
    pub report: Report,
}

pub use crate::hardening::is_secret_key;
use crate::hardening::{safe_label, value_has_credential, MAX_ENTRIES, MAX_FILES, MAX_LINE};

pub(crate) fn decode_any(bytes: &[u8]) -> String {
    let utf16 = |rest: &[u8], le: bool| {
        let units: Vec<u16> = rest.chunks_exact(2).map(|c| if le { u16::from_le_bytes([c[0], c[1]]) } else { u16::from_be_bytes([c[0], c[1]]) }).collect();
        String::from_utf16_lossy(&units)
    };
    if let Some(rest) = bytes.strip_prefix(&[0xFF, 0xFE]) {
        return utf16(rest, true);
    }
    if let Some(rest) = bytes.strip_prefix(&[0xFE, 0xFF]) {
        return utf16(rest, false);
    }
    // UTF-16 without a BOM: mostly ASCII text means a zero byte in every other position.
    if bytes.len() >= 4 && bytes.len() % 2 == 0 {
        let sample = &bytes[..bytes.len().min(512) & !1];
        let (even0, odd0) = sample.chunks_exact(2).fold((0, 0), |(e, o), c| (e + (c[0] == 0) as usize, o + (c[1] == 0) as usize));
        let pairs = sample.len() / 2;
        if odd0 * 10 >= pairs * 7 && even0 == 0 {
            return utf16(bytes, true);
        }
        if even0 * 10 >= pairs * 7 && odd0 == 0 {
            return utf16(bytes, false);
        }
    }
    cliptext::decode_text(bytes)
}

/// Parse INI text into a TOML table (all values strings), dropping secret keys.
pub(crate) fn ini_to_table(text: &str, file: &str, report: &mut Report) -> toml::Table {
    let mut root = toml::Table::new();
    let mut section = String::from("_root");
    let mut entries = 0usize;
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with(';') || line.len() > MAX_LINE {
            continue;
        }
        if let Some(s) = line.strip_prefix('[').and_then(|l| l.strip_suffix(']')) {
            section = if crate::hardening::section_is_credential(s) { "_hostile".to_string() } else { s.to_string() };
            continue;
        }
        let Some((k, v)) = line.split_once('=') else { continue };
        let k = k.trim();
        if section == "_hostile" {
            continue;
        }
        entries += 1;
        if entries > MAX_ENTRIES {
            break;
        }
        if is_secret_key(k) {
            report.skipped_keys.push(format!("{file}: [{}] {}", safe_label(&section), safe_label(k)));
            continue;
        }
        if value_has_credential(k, v) {
            report.skipped_keys.push(format!("{file}: [{}] {} (credential-like value, not copied)", safe_label(&section), safe_label(k)));
            continue;
        }
        if let toml::Value::Table(t) = root.entry(section.clone()).or_insert_with(|| toml::Value::Table(toml::Table::new())) {
            t.insert(k.to_string(), toml::Value::String(v.trim().to_string()));
        }
    }
    root
}

/// Build the import plan for `dir` (non-recursive). Nothing is written.
pub fn plan_from_folder(dir: &Path) -> io::Result<ImportPlan> {
    let mut plan = ImportPlan::default();
    let mut entries: Vec<_> = fs::read_dir(dir)?.filter_map(Result::ok).filter(|e| e.path().is_file()).collect();
    entries.sort_by_key(|e| e.file_name());
    entries.truncate(MAX_FILES);
    let mut templates = Vec::new();
    for e in entries {
        let name = e.file_name().to_string_lossy().into_owned();
        let lower = name.to_lowercase();
        let (stem, ext) = lower.rsplit_once('.').unwrap_or((&lower, ""));
        let out = |plan: &mut ImportPlan, rel: String, content: String| {
            plan.report.converted.push(rel.clone());
            plan.outputs.push((rel, content));
        };
        match ext {
            _ if stem != "reg_u" && crate::hardening::too_large(&e.path()) => plan.report.skipped_files.push(format!("{name} (larger than 4 MiB, not read)")),
            "ctl" => {
                let lib = cliptext::parse_ctl(&decode_any(&fs::read(e.path())?));
                out(&mut plan, format!("cliptext/{stem}.toml"), cliptext::to_toml(&lib));
            }
            "acp" => {
                let f = acp::parse_acp(&decode_any(&fs::read(e.path())?));
                out(&mut plan, format!("abbrev/{stem}.toml"), acp::to_toml(&f));
            }
            "ini" if stem == "reg_u" => plan.report.skipped_files.push(name), // licence: never opened
            "ini" if ["workspace", "tool", "regex"].iter().any(|p| stem.starts_with(p)) => {
                let text = decode_any(&fs::read(e.path())?);
                let table = ini_to_table(&text, &name, &mut plan.report);
                out(&mut plan, format!("ini/{stem}.toml"), toml::to_string(&table).unwrap_or_default());
            }
            _ if stem.starts_with("template") && !ext.is_empty() && ext != "ini" => {
                let body = decode_any(&fs::read(e.path())?);
                out(&mut plan, format!("templates/{name}"), body);
                templates.push((ext.to_string(), name));
            }
            _ => plan.report.skipped_files.push(name),
        }
    }
    if !templates.is_empty() {
        use brouillon_editor_engine::templates::{Template, TemplateList};
        let mut list = TemplateList::default();
        for (ext, file) in templates {
            let _ = list.add(Template { name: format!("New .{ext}"), file });
        }
        plan.report.converted.push("templates/templates.toml".into());
        plan.outputs.push(("templates/templates.toml".into(), list.to_toml()));
    }
    Ok(plan)
}

/// Write a plan under `dest` (creates folders; never touches the source).
pub fn write_plan(plan: &ImportPlan, dest: &Path) -> io::Result<()> {
    for (rel, content) in &plan.outputs {
        let p = dest.join(rel);
        if let Some(parent) = p.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(p, content)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> crate::testutil::TempDir {
        let d = crate::testutil::TempDir::new();
        let w = |n: &str, c: &[u8]| fs::write(d.path().join(n), c).unwrap();
        w("mini.ctl", b"#TITLE=Mini\r\n#T=bold\r\n<b>^!</b>\r\n#\r\n");
        w("mini.acp", b"#TITLE=A\r\n#CASE=y\r\n#T=if\r\nif (^!)\r\n#\r\n");
        w("template.html", b"<p>^!</p>\r\n");
        w("tool_u.ini", b"[Tool1]\nCommand=make\nArgument=-j4\nRemote Password=hunter2\n");
        w("workspace_u.ini", b"[Workspace]\nCount=1\n[Workspace\\1]\nFile=C:\\x\\a.txt\nLine=3\n");
        w("reg_u.ini", b"Name=me\nCode=SECRET-LICENCE\n");
        w("ftp_u.ini", b"Password=abc\n");
        d
    }

    #[test]
    fn converts_fixture_to_expected_toml() {
        let d = fixture();
        let plan = plan_from_folder(d.path()).unwrap();
        let get = |p: &str| plan.outputs.iter().find(|(r, _)| r == p).map(|(_, c)| c.clone()).unwrap();
        let ctl = cliptext::from_toml(&get("cliptext/mini.toml")).unwrap();
        assert_eq!(ctl.entries[0].body, "<b>^!</b>");
        let a = acp::from_toml(&get("abbrev/mini.toml")).unwrap();
        assert!(a.case_sensitive && a.abbrevs[0].abbr == "if");
        assert!(get("templates/templates.toml").contains("template.html"));
        let ws: toml::Table = get("ini/workspace_u.toml").parse().unwrap();
        assert_eq!(ws["Workspace\\1"]["Line"].as_str(), Some("3"));
        let tool: toml::Table = get("ini/tool_u.toml").parse().unwrap();
        assert_eq!(tool["Tool1"]["Command"].as_str(), Some("make"));
    }

    #[test]
    fn password_keys_are_dropped_and_reported() {
        let d = fixture();
        let plan = plan_from_folder(d.path()).unwrap();
        let all: String = plan.outputs.iter().map(|(_, c)| c.as_str()).collect();
        assert!(!all.contains("hunter2") && !all.contains("Password") && !all.contains("SECRET-LICENCE"));
        assert_eq!(plan.report.skipped_keys, vec!["tool_u.ini: [Tool1] Remote Password"]);
        assert!(plan.report.skipped_files.contains(&"reg_u.ini".to_string()));
        assert!(plan.report.skipped_files.contains(&"ftp_u.ini".to_string()));
    }

    #[test]
    fn write_plan_creates_files_and_leaves_source() {
        let d = fixture();
        let out = crate::testutil::TempDir::new();
        let plan = plan_from_folder(d.path()).unwrap();
        write_plan(&plan, out.path()).unwrap();
        assert!(out.path().join("cliptext/mini.toml").is_file());
        assert!(d.path().join("mini.ctl").is_file());
    }

    #[test]
    fn utf16_ini_is_decoded() {
        let mut b = vec![0xFF, 0xFE];
        for u in "[S]\nK=v\n".encode_utf16() {
            b.extend_from_slice(&u.to_le_bytes());
        }
        assert!(decode_any(&b).contains("K=v"));
    }
}
