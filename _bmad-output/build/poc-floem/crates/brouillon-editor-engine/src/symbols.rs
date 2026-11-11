//! Outline: functions, types and headings of a document, by line heuristics per language family
//! (picked from the file extension). Feeds the quick-open `@symbol` picker and `goto_function`.
//!
//! This is not a parser: it finds declarations that start a line (after modifiers). It is cheap
//! (one pass, no allocation per non-matching line) and wrong only in the usual ways of a
//! heuristic (a declaration split over several lines before its name, macros).

use std::path::Path;
use std::sync::OnceLock;

use regex::Regex;

/// One entry of the outline.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Symbol {
    pub name: String,
    /// 1-based line of the declaration.
    pub line: usize,
    /// `fn`, `class`, `heading`, `table`, ...
    pub kind: &'static str,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Family {
    Markdown,
    Rust,
    CLike,   // C, C++, C#, Java
    Script,  // JS, TS
    Python,
    Ruby,
    Php,
    Go,
    Sql,
    Shell,
    Perl,
    Lua,
    PowerShell,
    Vb,
    Batch,
    Ini,
    Make,
    Generic,
}

fn family_of(path: Option<&Path>) -> Family {
    let ext = path.and_then(|p| p.extension()).and_then(|e| e.to_str()).map(str::to_ascii_lowercase).unwrap_or_default();
    let name = path.and_then(|p| p.file_name()).and_then(|e| e.to_str()).map(str::to_ascii_lowercase).unwrap_or_default();
    if name == "makefile" || name == "gnumakefile" {
        return Family::Make;
    }
    match ext.as_str() {
        "md" | "markdown" | "mdx" => Family::Markdown,
        "rs" => Family::Rust,
        "c" | "h" | "cc" | "cpp" | "cxx" | "hpp" | "hh" | "hxx" | "cs" | "java" | "kt" | "swift" | "m" => Family::CLike,
        "js" | "jsx" | "mjs" | "cjs" | "ts" | "tsx" => Family::Script,
        "py" | "pyw" => Family::Python,
        "rb" | "rake" => Family::Ruby,
        "php" | "phtml" => Family::Php,
        "go" => Family::Go,
        "sql" => Family::Sql,
        "sh" | "bash" | "zsh" => Family::Shell,
        "pl" | "pm" => Family::Perl,
        "lua" => Family::Lua,
        "ps1" | "psm1" => Family::PowerShell,
        "vb" | "vbs" | "bas" => Family::Vb,
        "bat" | "cmd" => Family::Batch,
        "ini" | "toml" | "cfg" => Family::Ini,
        "mk" => Family::Make,
        _ => Family::Generic,
    }
}

macro_rules! re {
    ($name:ident, $pat:expr) => {
        fn $name() -> &'static Regex {
            static R: OnceLock<Regex> = OnceLock::new();
            R.get_or_init(|| Regex::new($pat).expect("symbols regex"))
        }
    };
}

re!(re_rust, r"^\s*(?:pub(?:\([^)]*\))?\s+)?(?:(?:async|unsafe|const|extern(?:\s+\x22[^\x22]*\x22)?|default)\s+)*(fn|struct|enum|trait|union|mod|type|static|const)\s+(?:mut\s+)?([A-Za-z_][A-Za-z0-9_]*)");
re!(re_rust_impl, r"^\s*(?:unsafe\s+)?impl\b(?:<[^>]*>)?\s*([^{]+?)\s*(?:\{|where\b|$)");
re!(re_py, r"^\s*(?:async\s+)?(def|class)\s+([A-Za-z_][A-Za-z0-9_]*)");
re!(re_ruby, r"^\s*(def|class|module)\s+(?:self\.)?([A-Za-z_][A-Za-z0-9_:?!=]*)");
re!(re_php, r"^\s*(?:(?:public|private|protected|static|final|abstract)\s+)*(function|class|interface|trait)\s+&?\s*([A-Za-z_][A-Za-z0-9_]*)");
re!(re_go, r"^(func)\s+(?:\([^)]*\)\s*)?([A-Za-z_][A-Za-z0-9_]*)|^(type)\s+([A-Za-z_][A-Za-z0-9_]*)\s+(?:struct|interface)");
re!(re_js, r"^\s*(?:export\s+)?(?:default\s+)?(?:async\s+)?(function\*?|class|interface|enum|type)\s+([A-Za-z_$][A-Za-z0-9_$]*)");
re!(re_js_const, r"^\s*(?:export\s+)?(?:const|let|var)\s+([A-Za-z_$][A-Za-z0-9_$]*)\s*(?::[^=]+)?=\s*(?:async\s+)?(?:function\b|\([^)]*\)\s*(?::[^=]+)?=>|[A-Za-z_$][A-Za-z0-9_$]*\s*=>)");
re!(re_sql, r"(?i)^\s*create\s+(?:or\s+replace\s+)?(?:(?:unique|temporary|temp|materialized|clustered)\s+)*(table|view|function|procedure|index|trigger|type|schema)\s+(?:if\s+not\s+exists\s+)?([\[`\x22]?[A-Za-z_][A-Za-z0-9_.\]`\x22\[]*)");
re!(re_shell, r"^\s*(?:function\s+([A-Za-z_][A-Za-z0-9_.:-]*)|([A-Za-z_][A-Za-z0-9_.:-]*)\s*\(\s*\))");
re!(re_perl, r"^\s*(sub)\s+([A-Za-z_][A-Za-z0-9_:]*)");
re!(re_lua, r"^\s*(?:local\s+)?(function)\s+([A-Za-z_][A-Za-z0-9_.:]*)");
re!(re_ps, r"(?i)^\s*(function|filter)\s+([A-Za-z_][A-Za-z0-9_:-]*)");
re!(re_vb, r"(?i)^\s*(?:(?:public|private|friend|protected|shared|static|overrides|overridable)\s+)*(sub|function|class|module)\s+([A-Za-z_][A-Za-z0-9_]*)");
re!(re_batch, r"^\s*:([A-Za-z_][A-Za-z0-9_.-]*)\s*$");
re!(re_ini, r"^\s*\[+([^\]\r\n]+)\]+\s*$");
re!(re_make, r"^([A-Za-z0-9_./-]+)\s*:(?:[^=]|$)");
re!(re_type_decl, r"^\s*(?:(?:public|private|protected|internal|static|final|abstract|sealed|partial|export|extern|unsafe|readonly|data|open|inline)\s+)*(class|struct|enum|interface|namespace|record|union)\s+([A-Za-z_][A-Za-z0-9_.]*)");
// type-and-name followed by a parameter list: `int main(`, `public static void Run<T>(`, `Foo::bar(`, `~Foo(`.
re!(re_c_fn, r"^\s*(?:[A-Za-z_][A-Za-z0-9_<>\[\],.:*&\s]*?[\s*&])?(~?[A-Za-z_][A-Za-z0-9_]*(?:::~?[A-Za-z_][A-Za-z0-9_]*)*)\s*(?:<[^>()]*>)?\s*\(");

const NOT_FUNCTIONS: &[&str] =
    &["if", "for", "while", "switch", "catch", "return", "else", "sizeof", "do", "foreach", "using", "lock", "typeof", "new", "throw", "elif", "defined", "case", "delete", "when", "fixed", "synchronized"];

fn push(out: &mut Vec<Symbol>, name: &str, line: usize, kind: &'static str) {
    if !name.is_empty() {
        out.push(Symbol { name: name.to_string(), line, kind });
    }
}

/// Outline of `text`; the language family comes from `path`'s extension.
pub fn extract(text: &str, path: Option<&Path>) -> Vec<Symbol> {
    extract_lines(text.lines(), path)
}

/// [`extract`] over a rope: one line is materialised at a time (and only when it straddles two
/// chunks), so outlining a 100 MB document no longer copies it into a String plus a line index.
pub fn extract_rope(rope: &crop::Rope, path: Option<&Path>) -> Vec<Symbol> {
    extract_lines(
        rope.lines().map(|l| {
            let mut chunks = l.chunks();
            match (chunks.next(), chunks.next()) {
                (None, _) => std::borrow::Cow::Borrowed(""),
                (Some(a), None) => std::borrow::Cow::Borrowed(a),
                (Some(a), Some(b)) => std::borrow::Cow::Owned([a, b].into_iter().chain(chunks).collect::<String>()),
            }
        }),
        path,
    )
}

/// The outline of consecutive `lines` (no line terminators); one line of look-ahead is kept.
pub fn extract_lines<L: AsRef<str>>(lines: impl Iterator<Item = L>, path: Option<&Path>) -> Vec<Symbol> {
    let fam = family_of(path);
    let mut out = Vec::new();
    let mut in_fence = false;
    let mut in_block_comment = false;
    let mut lines = lines;
    let mut next = lines.next();
    let mut line_no = 0usize;
    while let Some(cur) = next.take() {
        next = lines.next();
        line_no += 1;
        let raw: &str = cur.as_ref();
        let trimmed = raw.trim_start();
        match fam {
            Family::Markdown => {
                if trimmed.starts_with("```") || trimmed.starts_with("~~~") {
                    in_fence = !in_fence;
                    continue;
                }
                if in_fence || raw.len() - trimmed.len() > 3 {
                    continue;
                }
                let hashes = trimmed.bytes().take_while(|b| *b == b'#').count();
                if (1..=6).contains(&hashes) {
                    let title = trimmed[hashes..].trim().trim_end_matches('#').trim();
                    if trimmed[hashes..].starts_with(' ') && !title.is_empty() {
                        push(&mut out, title, line_no, "heading");
                    }
                } else if let Some(next) = next.as_ref().map(|n| n.as_ref()) {
                    // setext heading: a text line underlined with === or ---
                    let n = next.trim();
                    if !trimmed.is_empty() && !n.is_empty() && (n.bytes().all(|b| b == b'=') || (n.len() >= 3 && n.bytes().all(|b| b == b'-'))) && !trimmed.starts_with(['-', '*', '>', '|']) {
                        push(&mut out, trimmed, line_no, "heading");
                    }
                }
            }
            Family::Rust => {
                if trimmed.starts_with("//") {
                    continue;
                }
                if let Some(c) = re_rust().captures(raw) {
                    let kind = match &c[1] {
                        "fn" => "fn",
                        "struct" | "union" => "struct",
                        "enum" => "enum",
                        "trait" => "trait",
                        "mod" => "mod",
                        "type" => "type",
                        _ => "const",
                    };
                    if !(matches!(&c[1], "const" | "static") && raw.starts_with(char::is_whitespace)) {
                        push(&mut out, &c[2], line_no, kind);
                    }
                } else if let Some(c) = re_rust_impl().captures(raw) {
                    push(&mut out, &format!("impl {}", c[1].trim()), line_no, "impl");
                }
            }
            Family::Python => {
                if let Some(c) = re_py().captures(raw) {
                    push(&mut out, &c[2], line_no, if &c[1] == "def" { "fn" } else { "class" });
                }
            }
            Family::Ruby => {
                if let Some(c) = re_ruby().captures(raw) {
                    push(&mut out, &c[2], line_no, match &c[1] { "def" => "fn", "class" => "class", _ => "module" });
                }
            }
            Family::Php => {
                if let Some(c) = re_php().captures(raw) {
                    push(&mut out, &c[2], line_no, if &c[1] == "function" { "fn" } else { "class" });
                }
            }
            Family::Go => {
                if let Some(c) = re_go().captures(raw) {
                    if let Some(n) = c.get(2) {
                        push(&mut out, n.as_str(), line_no, "fn");
                    } else if let Some(n) = c.get(4) {
                        push(&mut out, n.as_str(), line_no, "type");
                    }
                }
            }
            Family::Script => {
                if trimmed.starts_with("//") {
                    continue;
                }
                if let Some(c) = re_js().captures(raw) {
                    push(&mut out, &c[2], line_no, if c[1].starts_with("function") { "fn" } else { "class" });
                } else if let Some(c) = re_js_const().captures(raw) {
                    push(&mut out, &c[1], line_no, "fn");
                }
            }
            Family::Sql => {
                if trimmed.starts_with("--") {
                    continue;
                }
                if let Some(c) = re_sql().captures(raw) {
                    let name = c[2].trim_matches(|ch| matches!(ch, '[' | ']' | '`' | '"'));
                    push(&mut out, name, line_no, "sql");
                }
            }
            Family::Shell => {
                if trimmed.starts_with('#') {
                    continue;
                }
                if let Some(c) = re_shell().captures(raw) {
                    let n = c.get(1).or_else(|| c.get(2)).map(|m| m.as_str()).unwrap_or("");
                    if !matches!(n, "if" | "while" | "for" | "case" | "until") {
                        push(&mut out, n, line_no, "fn");
                    }
                }
            }
            Family::Perl => {
                if let Some(c) = re_perl().captures(raw) {
                    push(&mut out, &c[2], line_no, "fn");
                }
            }
            Family::Lua => {
                if let Some(c) = re_lua().captures(raw) {
                    push(&mut out, &c[2], line_no, "fn");
                }
            }
            Family::PowerShell => {
                if let Some(c) = re_ps().captures(raw) {
                    push(&mut out, &c[2], line_no, "fn");
                }
            }
            Family::Vb => {
                if let Some(c) = re_vb().captures(raw) {
                    // `End Sub` / `Exit Function` lines do not start with the keyword, so no false hit.
                    push(&mut out, &c[2], line_no, "fn");
                }
            }
            Family::Batch => {
                if let Some(c) = re_batch().captures(raw) {
                    push(&mut out, &c[1], line_no, "label");
                }
            }
            Family::Ini => {
                if let Some(c) = re_ini().captures(raw) {
                    push(&mut out, c[1].trim(), line_no, "section");
                }
            }
            Family::Make => {
                if !raw.starts_with('\t') && !raw.starts_with('#') {
                    if let Some(c) = re_make().captures(raw) {
                        push(&mut out, &c[1], line_no, "target");
                    }
                }
            }
            Family::CLike => {
                // Skip block comments and preprocessor / line comments.
                if in_block_comment {
                    if raw.contains("*/") {
                        in_block_comment = false;
                    }
                    continue;
                }
                if trimmed.starts_with("/*") {
                    in_block_comment = !raw.contains("*/");
                    continue;
                }
                if trimmed.starts_with("//") || trimmed.starts_with('#') || trimmed.starts_with('*') || trimmed.starts_with('@') {
                    continue;
                }
                if let Some(c) = re_type_decl().captures(raw) {
                    push(&mut out, &c[2], line_no, if &c[1] == "namespace" { "namespace" } else { "class" });
                    continue;
                }
                if trimmed.ends_with(';') || trimmed.ends_with(',') || trimmed.contains(" = ") || trimmed.starts_with("return ") {
                    continue;
                }
                if let Some(c) = re_c_fn().captures(raw) {
                    let name = &c[1];
                    let base = name.rsplit("::").next().unwrap_or(name);
                    if NOT_FUNCTIONS.contains(&base) {
                        continue;
                    }
                    // A call statement has no return type before the name and is not a definition:
                    // require either a type before the name, or a `Class::name(` qualified name.
                    let before = &raw[..c.get(1).unwrap().start()];
                    let has_type = before.trim().len() > 0 && before.trim() != "new";
                    let ctor_like = name.contains("::") || name.starts_with('~');
                    // Definitions open a body on this line or the next ("{"), or end with ")" (K&R / Allman).
                    let opens = trimmed.ends_with('{')
                        || trimmed.ends_with(')')
                        || trimmed.ends_with("const")
                        || trimmed.ends_with("override")
                        || next.as_ref().map(|n| n.as_ref().trim_start().starts_with('{')).unwrap_or(false)
                        || trimmed.contains("throws");
                    if (has_type || ctor_like) && opens && !before.contains('=') && !before.contains('(') {
                        push(&mut out, name, line_no, "fn");
                    }
                }
            }
            Family::Generic => {
                // Keyword-led declarations of the common languages, plus Markdown-looking headings.
                let mut words = trimmed.split_whitespace().filter(|w| !matches!(*w, "pub" | "public" | "private" | "async" | "export" | "default" | "static"));
                let (Some(kw), Some(name)) = (words.next(), words.next()) else { continue };
                if matches!(kw, "fn" | "def" | "function" | "func" | "class" | "struct" | "interface" | "sub" | "proc") {
                    let name: String = name.chars().take_while(|c| c.is_alphanumeric() || *c == '_' || *c == '$').collect();
                    push(&mut out, &name, line_no, "fn");
                }
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    #[test]
    fn rope_and_line_iterator_give_the_same_outline_as_the_whole_text() {
        let samples: [(&str, &str); 4] = [
            ("a.md", "# Title\n\nSetext\n======\n```\n# not\n```\n## Two\n"),
            ("a.rs", "pub fn alpha() {}\nstruct S;\nimpl S {\n    fn beta(&self) {}\n}\n// fn nope\n"),
            ("a.cpp", "/* c */\nint main()\n{\n  return 0;\n}\nclass Foo {};\nvoid Foo::bar(int x) {\n}\n"),
            ("a.py", "def f():\r\n    pass\r\nclass C:\r\n    def m(self): pass\r\n"),
        ];
        for (file, text) in samples {
            // A big filler of the same flavour makes the rope span many chunks.
            let big = format!("{text}{}{text}", text.repeat(300));
            for t in [text.to_string(), big] {
                let whole = extract(&t, Some(Path::new(file)));
                let rope = crop::Rope::from(t.as_str());
                let streamed = extract_rope(&rope, Some(Path::new(file)));
                assert_eq!(whole, streamed, "{file}");
                assert!(!whole.is_empty(), "{file} should have symbols");
            }
        }
    }

    use super::*;

    fn names(text: &str, file: &str) -> Vec<(String, usize)> {
        extract(text, Some(Path::new(file))).into_iter().map(|s| (s.name, s.line)).collect()
    }
    fn n(v: &[(&str, usize)]) -> Vec<(String, usize)> {
        v.iter().map(|(a, b)| (a.to_string(), *b)).collect()
    }

    #[test]
    fn markdown_headings_skip_code_fences() {
        let t = "# Title\ntext\n```\n# not a heading\n```\n## Sub\n\nSetext\n======\n";
        assert_eq!(names(t, "a.md"), n(&[("Title", 1), ("Sub", 6), ("Setext", 8)]));
    }

    #[test]
    fn rust_items_and_impl() {
        let t = "pub struct A;\nimpl A {\n    pub fn new() -> Self { A }\n    const X: u8 = 1;\n}\npub(crate) async fn go() {}\n// fn commented()\nimpl<T> Tr for B<T> where T: Copy {\n}\n";
        assert_eq!(names(t, "x.rs"), n(&[("A", 1), ("impl A", 2), ("new", 3), ("go", 6), ("impl Tr for B<T>", 8)]));
    }

    #[test]
    fn c_functions_not_calls_or_control_flow() {
        let t = "#include <x.h>\nstatic int add(int a, int b)\n{\n    if (a) { return foo(a); }\n    return a + b;\n}\nvoid run(void) {\n    helper(1);\n    x = compute(2);\n}\nstruct Point {\n};\n";
        assert_eq!(names(t, "m.c"), n(&[("add", 2), ("run", 7), ("Point", 11)]));
    }

    #[test]
    fn cpp_qualified_and_destructor() {
        let t = "Foo::Foo(int x) : a(x) {\n}\nvoid Foo::bar() const {\n}\nFoo::~Foo() {\n}\nnamespace ns {\n}\n";
        assert_eq!(names(t, "f.cpp"), n(&[("Foo::Foo", 1), ("Foo::bar", 3), ("Foo::~Foo", 5), ("ns", 7)]));
    }

    #[test]
    fn java_and_csharp_methods() {
        let t = "public class App {\n    public static void main(String[] args) {\n        run(args);\n    }\n    private int Count<T>(List<T> xs) {\n    }\n}\n";
        assert_eq!(names(t, "App.java"), n(&[("App", 1), ("main", 2), ("Count", 5)]));
        let cs = "namespace N {\n  public sealed class C {\n    public void Go() {\n    }\n    public int P { get; set; }\n  }\n}\n";
        assert_eq!(names(cs, "C.cs"), n(&[("N", 1), ("C", 2), ("Go", 3)]));
    }

    #[test]
    fn php_ruby_python_go() {
        assert_eq!(names("<?php\nclass A {\n  public static function f($x) {}\n}\nfunction g() {}\n", "a.php"), n(&[("A", 2), ("f", 3), ("g", 5)]));
        assert_eq!(names("module M\n  class K\n    def self.make; end\n    def run?; end\n  end\nend\n", "a.rb"), n(&[("M", 1), ("K", 2), ("make", 3), ("run?", 4)]));
        assert_eq!(names("class A:\n    async def f(self): pass\ndef g(): pass\n", "a.py"), n(&[("A", 1), ("f", 2), ("g", 3)]));
        assert_eq!(names("func (s *S) Run() {}\nfunc main() {}\ntype T struct {\n}\n", "a.go"), n(&[("Run", 1), ("main", 2), ("T", 3)]));
    }

    #[test]
    fn js_sql_shell_ini() {
        assert_eq!(names("export async function a() {}\nconst b = (x) => x;\nclass C {}\nconst d = 5;\n", "a.js"), n(&[("a", 1), ("b", 2), ("C", 3)]));
        assert_eq!(names("-- create table no\nCREATE TABLE IF NOT EXISTS users (id int);\ncreate or replace function f() returns int as $$ $$;\n", "a.sql"), n(&[("users", 2), ("f", 3)]));
        assert_eq!(names("#!/bin/sh\nbuild() {\n  if (x) ; then :; fi\n}\nfunction deploy {\n}\n", "a.sh"), n(&[("build", 2), ("deploy", 5)]));
        assert_eq!(names("[core]\nx=1\n[net.http]\n", "a.ini"), n(&[("core", 1), ("net.http", 3)]));
    }

    #[test]
    fn unknown_extension_uses_keyword_fallback_and_empty_text_is_empty() {
        assert_eq!(names("def foo\nclass Bar\n", "a.zzz"), n(&[("foo", 1), ("Bar", 2)]));
        assert!(extract("", None).is_empty());
    }
}
