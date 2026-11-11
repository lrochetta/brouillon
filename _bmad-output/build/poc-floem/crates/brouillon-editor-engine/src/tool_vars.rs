//! User-tool variables (BK-063): `${file} ${dir} ${name} ${ext} ${filename}
//! ${line} ${col} ${word} ${selection} ${project} ${env:X} ${prompt:Label}`.
//!
//! Syntax is Brouillon's own (decision D10): `${var}`, `${var:default}`,
//! `$$` for a literal `$`. Values can be quoted for a target shell so a path
//! with spaces or metacharacters never changes the command structure.

use std::collections::HashMap;
use std::fmt;
use std::path::PathBuf;

/// How substituted values are escaped.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Quote {
    /// Raw value (use for direct execution: each argument is its own string).
    None,
    /// POSIX `sh`: single quotes.
    Posix,
    /// Windows `cmd.exe`: double quotes; values that cannot be quoted safely are refused.
    Cmd,
}

/// Editor state a template can read.
#[derive(Debug, Clone, Default)]
pub struct VarContext {
    pub file: Option<PathBuf>,
    /// 1-based caret line / column.
    pub line: Option<usize>,
    pub col: Option<usize>,
    pub word: Option<String>,
    pub selection: Option<String>,
    pub project: Option<PathBuf>,
    /// `${env:X}` overrides, consulted before the process environment.
    pub env: HashMap<String, String>,
    /// Answers to `${prompt:Label}`, keyed by label.
    pub prompts: HashMap<String, String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VarError {
    Unknown(String),
    /// Known variable with no value and no default.
    Missing(String),
    /// A `${prompt:Label}` the caller has not answered yet.
    PromptRequired(String),
    Unterminated,
    /// Value cannot be quoted safely for the target shell.
    Unsafe { var: String },
}

impl fmt::Display for VarError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unknown(v) => write!(f, "unknown variable ${{{v}}}"),
            Self::Missing(v) => write!(f, "variable ${{{v}}} has no value here"),
            Self::PromptRequired(l) => write!(f, "prompt not answered: {l}"),
            Self::Unterminated => write!(f, "unterminated ${{ in template"),
            Self::Unsafe { var } => write!(f, "value of ${{{var}}} cannot be quoted safely for this shell"),
        }
    }
}
impl std::error::Error for VarError {}

/// Quote one value for `q`. `Err(())` when the shell cannot represent it safely.
pub fn quote_value(value: &str, q: Quote) -> Result<String, ()> {
    match q {
        Quote::None => Ok(value.to_string()),
        Quote::Posix => {
            if value.contains('\0') {
                return Err(());
            }
            Ok(format!("'{}'", value.replace('\'', "'\\''")))
        }
        Quote::Cmd => {
            if value.chars().any(|c| matches!(c, '"' | '%' | '\r' | '\n' | '\0')) {
                return Err(());
            }
            // A trailing run of backslashes would escape the closing quote.
            let trailing = value.chars().rev().take_while(|&c| c == '\\').count();
            Ok(format!("\"{value}{}\"", "\\".repeat(trailing)))
        }
    }
}

/// Labels of every `${prompt:Label}` in `template`, in order, deduplicated.
pub fn prompts_in(template: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for tok in tokens(template).into_iter().flatten() {
        if let Some(label) = tok.strip_prefix("prompt:") {
            if !out.iter().any(|l| l == label) {
                out.push(label.to_string());
            }
        }
    }
    out
}

/// Raw `${...}` bodies, one per occurrence (`None` entries are unterminated).
fn tokens(template: &str) -> Vec<Option<String>> {
    let mut out = Vec::new();
    let mut rest = template;
    while let Some(i) = rest.find('$') {
        let after = &rest[i + 1..];
        if let Some(a) = after.strip_prefix('$') {
            rest = a;
        } else if let Some(a) = after.strip_prefix('{') {
            match a.find('}') {
                Some(end) => {
                    out.push(Some(a[..end].to_string()));
                    rest = &a[end + 1..];
                }
                None => {
                    out.push(None);
                    break;
                }
            }
        } else {
            rest = after;
        }
    }
    out
}

/// Expand `template`. Literal text is copied verbatim (it is the tool
/// author's); only substituted values are quoted.
pub fn expand(template: &str, ctx: &VarContext, quote: Quote) -> Result<String, VarError> {
    let mut out = String::with_capacity(template.len());
    let mut rest = template;
    while let Some(i) = rest.find('$') {
        out.push_str(&rest[..i]);
        let after = &rest[i + 1..];
        if let Some(a) = after.strip_prefix('$') {
            out.push('$');
            rest = a;
        } else if let Some(a) = after.strip_prefix('{') {
            let end = a.find('}').ok_or(VarError::Unterminated)?;
            let value = resolve(&a[..end], ctx)?;
            let quoted = quote_value(&value, quote).map_err(|_| VarError::Unsafe { var: a[..end].to_string() })?;
            out.push_str(&quoted);
            rest = &a[end + 1..];
        } else {
            out.push('$');
            rest = after;
        }
    }
    out.push_str(rest);
    Ok(out)
}

fn resolve(body: &str, ctx: &VarContext) -> Result<String, VarError> {
    if let Some(label) = body.strip_prefix("prompt:") {
        return ctx.prompts.get(label).cloned().ok_or_else(|| VarError::PromptRequired(label.to_string()));
    }
    // `name:default`; for env the name itself is `env:X` so split after it.
    let (name, default) = if let Some(r) = body.strip_prefix("env:") {
        match r.split_once(':') {
            Some((n, d)) => (format!("env:{n}"), Some(d)),
            None => (body.to_string(), None),
        }
    } else {
        match body.split_once(':') {
            Some((n, d)) => (n.to_string(), Some(d)),
            None => (body.to_string(), None),
        }
    };
    let value = lookup(&name, ctx)?;
    match (value, default) {
        (Some(v), _) => Ok(v),
        (None, Some(d)) => Ok(d.to_string()),
        (None, None) => Err(VarError::Missing(name)),
    }
}

/// `Ok(None)` = known variable without a value; `Err` = unknown name.
fn lookup(name: &str, ctx: &VarContext) -> Result<Option<String>, VarError> {
    let path_str = |p: &PathBuf| p.to_string_lossy().into_owned();
    if let Some(var) = name.strip_prefix("env:") {
        return Ok(ctx.env.get(var).cloned().or_else(|| std::env::var(var).ok()));
    }
    Ok(match name {
        "file" => ctx.file.as_ref().map(path_str),
        "dir" => ctx.file.as_ref().and_then(|p| p.parent()).map(|p| p.to_string_lossy().into_owned()),
        "filename" => ctx.file.as_ref().and_then(|p| p.file_name()).map(|s| s.to_string_lossy().into_owned()),
        "name" => ctx.file.as_ref().and_then(|p| p.file_stem()).map(|s| s.to_string_lossy().into_owned()),
        "ext" => ctx.file.as_ref().and_then(|p| p.extension()).map(|s| s.to_string_lossy().into_owned()),
        "line" => ctx.line.map(|n| n.to_string()),
        "col" => ctx.col.map(|n| n.to_string()),
        "word" => ctx.word.clone(),
        "selection" => ctx.selection.clone(),
        "project" => ctx.project.as_ref().map(path_str),
        _ => return Err(VarError::Unknown(name.to_string())),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx() -> VarContext {
        VarContext {
            file: Some(PathBuf::from("C:/My Docs/report v2.txt")),
            line: Some(12),
            col: Some(3),
            word: Some("foo".into()),
            selection: Some("a b".into()),
            project: Some(PathBuf::from("/proj")),
            ..Default::default()
        }
    }

    #[test]
    fn golden_expansion_with_spaces() {
        let c = ctx();
        assert_eq!(expand("${file}", &c, Quote::None).unwrap(), "C:/My Docs/report v2.txt");
        assert_eq!(expand("${dir}|${filename}|${name}|${ext}", &c, Quote::None).unwrap(), "C:/My Docs|report v2.txt|report v2|txt");
        assert_eq!(expand("-n ${line}:${col} ${word}", &c, Quote::None).unwrap(), "-n 12:3 foo");
        assert_eq!(expand("cat ${file}", &c, Quote::Posix).unwrap(), "cat 'C:/My Docs/report v2.txt'");
        assert_eq!(expand("type ${file}", &c, Quote::Cmd).unwrap(), "type \"C:/My Docs/report v2.txt\"");
    }

    #[test]
    fn missing_and_unknown_are_errors() {
        let c = VarContext::default();
        assert_eq!(expand("${file}", &c, Quote::None), Err(VarError::Missing("file".into())));
        assert_eq!(expand("${bogus}", &c, Quote::None), Err(VarError::Unknown("bogus".into())));
        assert_eq!(expand("${file", &c, Quote::None), Err(VarError::Unterminated));
    }

    #[test]
    fn defaults_and_escape() {
        let c = VarContext::default();
        assert_eq!(expand("${file:none.txt}", &c, Quote::None).unwrap(), "none.txt");
        assert_eq!(expand("$${file} $ $x", &c, Quote::None).unwrap(), "${file} $ $x");
        assert_eq!(expand("${line:1}", &ctx(), Quote::None).unwrap(), "12");
    }

    #[test]
    fn env_and_prompt() {
        let mut c = VarContext::default();
        c.env.insert("MYV".into(), "val".into());
        assert_eq!(expand("${env:MYV}", &c, Quote::None).unwrap(), "val");
        assert_eq!(expand("${env:BROUILLON_NOPE_XYZ:dflt}", &c, Quote::None).unwrap(), "dflt");
        assert!(matches!(expand("${env:BROUILLON_NOPE_XYZ}", &c, Quote::None), Err(VarError::Missing(_))));
        assert_eq!(prompts_in("a ${prompt:Branch name:} b ${prompt:Branch name:} ${prompt:Msg}"), vec!["Branch name:", "Msg"]);
        assert_eq!(expand("${prompt:Msg}", &c, Quote::None), Err(VarError::PromptRequired("Msg".into())));
        c.prompts.insert("Msg".into(), "hi".into());
        assert_eq!(expand("${prompt:Msg}", &c, Quote::None).unwrap(), "hi");
    }

    #[test]
    fn injection_is_neutralised() {
        let mut c = ctx();
        c.selection = Some("x'; rm -rf / #".into());
        assert_eq!(expand("echo ${selection}", &c, Quote::Posix).unwrap(), "echo 'x'\\''; rm -rf / #'");
        c.selection = Some("a & del *".into());
        assert_eq!(expand("echo ${selection}", &c, Quote::Cmd).unwrap(), "echo \"a & del *\"");
        for bad in ["a\"b", "%PATH%", "x\ny"] {
            c.selection = Some(bad.into());
            assert!(matches!(expand("${selection}", &c, Quote::Cmd), Err(VarError::Unsafe { .. })), "{bad}");
        }
        c.selection = Some("dir\\".into());
        assert_eq!(expand("${selection}", &c, Quote::Cmd).unwrap(), "\"dir\\\\\"");
    }
}
