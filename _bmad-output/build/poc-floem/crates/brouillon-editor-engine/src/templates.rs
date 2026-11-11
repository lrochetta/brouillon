//! New-from-template (BK-058): up to 20 templates (menu name + file), `^!`
//! caret marker (same escapes as cliptext, no selection).

use crate::cliptext::{decode_text, expand_text, today};
use brouillon_core::contract::{CmdCtx, CmdOutcome, CommandSpec, EditSet, Region, TextEdit};
use serde::{Deserialize, Serialize};
use std::path::Path;

pub const MAX_TEMPLATES: usize = 20;

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct Template {
    /// File > New menu text.
    pub name: String,
    /// Template file (relative to the list file's directory, or absolute).
    pub file: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct TemplateList {
    #[serde(default, rename = "template")]
    pub templates: Vec<Template>,
}

impl TemplateList {
    pub fn add(&mut self, t: Template) -> Result<(), String> {
        if self.templates.len() >= MAX_TEMPLATES {
            return Err(format!("at most {MAX_TEMPLATES} templates"));
        }
        self.templates.push(t);
        Ok(())
    }
    pub fn from_toml(text: &str) -> Result<Self, String> {
        let mut l: Self = toml::from_str(text).map_err(|e| e.to_string())?;
        l.templates.truncate(MAX_TEMPLATES);
        Ok(l)
    }
    pub fn to_toml(&self) -> String {
        toml::to_string(self).unwrap_or_default()
    }
}

/// Expand a template body: text for the new document and the caret offset.
pub fn instantiate(body: &str) -> (String, usize) {
    let e = expand_text(body, None, &today());
    (e.text, e.caret)
}

pub fn commands() -> Vec<CommandSpec> {
    vec![CommandSpec {
        id: "file_new_from_template".into(),
        caption: "File: New from template".into(),
        category: "File".into(),
        default_chord: None,
        when: None, ..Default::default()
    }]
}

/// `file_new_from_template`: `args[0]` = template file. Meant to run on the
/// freshly created empty document: inserts the expansion and places the caret.
pub fn run(id: &str, ctx: &CmdCtx) -> Option<CmdOutcome> {
    if id != "file_new_from_template" {
        return None;
    }
    let fail = |m: String| Some(CmdOutcome { message: Some(m), ..Default::default() });
    let Some(path) = ctx.args.first() else { return fail("template: no file".into()) };
    let body = match std::fs::read(Path::new(path)) {
        Ok(b) => decode_text(&b),
        Err(e) => return fail(format!("{path}: {e}")),
    };
    let (text, caret) = instantiate(&body);
    Some(CmdOutcome {
        edits: EditSet(vec![TextEdit::insert(0, text)]),
        selection: Some(vec![Region::caret(caret)]),
        ..Default::default()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn caret_marker_places_caret() {
        let (t, c) = instantiate("int main() {\n\t^!\n}\n");
        assert_eq!(t, "int main() {\n\t\n}\n");
        assert_eq!(c, 14);
    }

    #[test]
    fn run_inserts_template_with_caret() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("template.html");
        std::fs::write(&p, "<p>^!</p>").unwrap();
        let args = [p.to_string_lossy().into_owned()];
        let ctx = CmdCtx { text: "", selection: &[], args: &args, ..Default::default() };
        let o = run("file_new_from_template", &ctx).unwrap();
        assert_eq!(o.edits.apply_to_str("").unwrap(), "<p></p>");
        assert_eq!(o.selection.unwrap()[0].head, 3);
        assert!(run("x", &ctx).is_none());
    }

    #[test]
    fn list_capped_at_twenty_and_roundtrips() {
        let mut l = TemplateList::default();
        for i in 0..20 {
            l.add(Template { name: format!("t{i}"), file: format!("f{i}") }).unwrap();
        }
        assert!(l.add(Template::default()).is_err());
        assert_eq!(TemplateList::from_toml(&l.to_toml()).unwrap(), l);
    }
}
