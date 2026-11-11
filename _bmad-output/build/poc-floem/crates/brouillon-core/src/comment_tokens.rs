//! BK-032 comment token table: language id or file extension -> comment syntax.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CommentTokens {
    pub line: Option<&'static str>,
    pub block: Option<(&'static str, &'static str)>,
}

const C_LIKE: CommentTokens = CommentTokens { line: Some("//"), block: Some(("/*", "*/")) };
const HASH: CommentTokens = CommentTokens { line: Some("#"), block: None };
const DASH: CommentTokens = CommentTokens { line: Some("--"), block: None };
const MARKUP: CommentTokens = CommentTokens { line: None, block: Some(("<!--", "-->")) };

/// Look up by language name or extension (case-insensitive, leading dot optional).
pub fn for_language(key: &str) -> Option<CommentTokens> {
    let k = key.trim_start_matches('.').to_ascii_lowercase();
    Some(match k.as_str() {
        "rust" | "rs" | "c" | "h" | "cpp" | "cc" | "cxx" | "hpp" | "java" | "js" | "javascript" | "jsx" | "ts" | "typescript" | "tsx" | "go" | "swift" | "kt" | "kotlin" | "cs" | "csharp" | "scala" | "php" | "dart" | "jsonc" | "glsl" | "wgsl" => C_LIKE,
        "py" | "python" | "sh" | "bash" | "zsh" | "rb" | "ruby" | "toml" | "yaml" | "yml" | "pl" | "perl" | "r" | "mk" | "makefile" | "dockerfile" | "conf" | "gitignore" | "ps1" | "powershell" => HASH,
        "sql" | "lua" | "hs" | "haskell" | "ada" | "elm" => DASH,
        "html" | "htm" | "xml" | "svg" | "xhtml" | "md" | "markdown" | "vue" => MARKUP,
        "css" | "scss" | "less" => CommentTokens { line: None, block: Some(("/*", "*/")) },
        "ini" | "cfg" | "lisp" | "el" | "clj" | "asm" => CommentTokens { line: Some(";"), block: None },
        "bat" | "cmd" => CommentTokens { line: Some("REM"), block: None },
        "vb" | "vbs" | "bas" => CommentTokens { line: Some("'"), block: None },
        "tex" | "latex" | "erl" | "m" => CommentTokens { line: Some("%"), block: None },
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lookup_by_name_and_extension() {
        assert_eq!(for_language("rs").unwrap().line, Some("//"));
        assert_eq!(for_language(".PY").unwrap().line, Some("#"));
        assert_eq!(for_language("html").unwrap().block, Some(("<!--", "-->")));
        assert!(for_language("html").unwrap().line.is_none());
        assert!(for_language("zzz").is_none());
    }
}
