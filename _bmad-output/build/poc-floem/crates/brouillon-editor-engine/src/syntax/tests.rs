use super::*;
use crate::highlight::SyntaxKind as K;
use std::path::Path;
use std::sync::{Arc, OnceLock};
use std::time::Instant;

fn reg() -> Arc<Registry> {
    static R: OnceLock<Arc<Registry>> = OnceLock::new();
    R.get_or_init(|| Arc::new(Registry::build(&[]))).clone()
}

fn doc(lang: &str, src: &str) -> SyntaxDoc {
    let r = reg();
    let i = r.by_name(lang).unwrap_or_else(|| panic!("no language {lang}"));
    SyntaxDoc::new(r, i, src)
}

/// (text, kind) of every coloured span, in order.
fn spans(lang: &str, src: &str) -> Vec<(String, K)> {
    let d = doc(lang, src);
    d.spans_in_range(0..src.len())
        .into_iter()
        .map(|s| (src[s.byte_range.clone()].to_string(), s.kind))
        .collect()
}

/// Assert the coloured spans of `src` equal `expected` exactly.
fn golden(lang: &str, src: &str, expected: &[(&str, K)]) {
    let got = spans(lang, src);
    let got_s: Vec<(&str, K)> = got.iter().map(|(t, k)| (t.as_str(), *k)).collect();
    assert_eq!(got_s, expected, "golden mismatch for {lang}");
}

// ---------------------------------------------------------------------------
// Loading
// ---------------------------------------------------------------------------

#[test]
fn builtin_definitions_load_without_errors() {
    let r = reg();
    assert!(r.errors().is_empty(), "{:?}", r.errors());
    for n in [
        "C", "C++", "C#", "Java", "PHP", "HTML", "CSS", "JavaScript", "TypeScript", "SQL", "XML", "Python", "Ruby",
        "Perl", "Visual Basic", "Go", "Shell", "PowerShell", "Batch", "YAML", "INI", "Lua", "JSON", "TOML", "Markdown",
        "Dockerfile", "Makefile", "Diff", "Log",
    ] {
        assert!(r.by_name(n).is_some(), "missing {n}");
    }
    // nested-language links resolved
    let php = r.def(r.by_name("PHP").unwrap());
    assert_eq!(php.host, r.by_name("HTML"));
    let html = r.def(r.by_name("HTML").unwrap());
    let rt = &html.markup.as_ref().unwrap().raw_text;
    assert_eq!(rt[0].def, r.by_name("JavaScript"));
    assert_eq!(rt[1].def, r.by_name("CSS"));
}

#[test]
fn extends_inherits_and_merges_groups() {
    let r = reg();
    let c = r.def(r.by_name("C").unwrap());
    let cpp = r.def(r.by_name("C++").unwrap());
    assert!(cpp.word_count() > c.word_count());
    assert_eq!(cpp.word_kind("printf"), Some(K::Keyword3)); // inherited
    assert_eq!(cpp.word_kind("namespace"), Some(K::Keyword)); // added
    assert_eq!(cpp.word_kind("vector"), Some(K::Type)); // merged into inherited "Types"
    let ts = r.def(r.by_name("TypeScript").unwrap());
    assert_eq!(ts.word_kind("interface"), Some(K::Keyword));
    assert_eq!(ts.word_kind("function"), Some(K::Keyword)); // from JavaScript
}

// ---------------------------------------------------------------------------
// Golden samples (synthetic), P0 languages
// ---------------------------------------------------------------------------

#[test]
fn golden_c() {
    golden(
        "C",
        "#include <stdio.h>\nint main(void) { // entry\n    char *s = \"hi\\n\";\n    return 0x1F + 42;\n}\n",
        &[
            ("#include", K::Preprocessor),
            ("<stdio.h>", K::String),
            ("int", K::Type),
            ("main", K::Function),
            ("void", K::Type),
            ("// entry", K::Comment),
            ("char", K::Type),
            ("*", K::Operator),
            ("=", K::Operator),
            ("\"hi", K::String),
            ("\\n", K::Escape),
            ("\"", K::String),
            ("return", K::Keyword),
            ("0x1F", K::Number),
            ("+", K::Operator),
            ("42", K::Number),
        ],
    );
}

#[test]
fn golden_cpp() {
    golden(
        "C++",
        "class A : public B {\n  std::string s = R\"(raw\n text)\";\n  void f() noexcept { return nullptr; }\n};\n",
        &[
            ("class", K::Keyword),
            (":", K::Operator),
            ("public", K::Keyword),
            ("::", K::Operator),
            ("string", K::Type),
            ("=", K::Operator),
            ("R\"(raw", K::String),
            (" text)\"", K::String),
            ("void", K::Type),
            ("f", K::Function),
            ("noexcept", K::Keyword),
            ("return", K::Keyword),
            ("nullptr", K::Keyword4),
        ],
    );
}

#[test]
fn golden_csharp() {
    golden(
        "C#",
        "#region X\nstring s = @\"a\n\"\"b\"; var t = $\"x{y}\";\nawait Foo();\n",
        &[
            ("#region", K::Preprocessor),
            ("string", K::Type),
            ("=", K::Operator),
            ("@\"a", K::String),
            ("\"\"b\"", K::String),
            ("var", K::Keyword),
            ("=", K::Operator),
            ("$\"x{y}\"", K::String),
            ("await", K::Keyword),
            ("Foo", K::Function),
        ],
    );
}

#[test]
fn golden_java() {
    golden(
        "Java",
        "@Override\npublic static void main(String[] a) { int x = 10L; }\n",
        &[
            ("@Override", K::Attribute),
            ("public", K::Keyword),
            ("static", K::Keyword),
            ("void", K::Type),
            ("main", K::Function),
            ("String", K::Type),
            ("int", K::Type),
            ("=", K::Operator),
            ("10L", K::Number),
        ],
    );
}

#[test]
fn golden_php_in_html() {
    golden(
        "PHP",
        "<html>\n<?php\n$a = 'x'; // c ?> tail\n<?php /* multi\nline */ echo \"hi\"; $b = <<<EOT\nHello $a\n  EOT;\n?>\n<p class=\"x\"><?= $a ?></p>\n",
        &[
            ("<html>", K::Tag),
            ("<?php", K::Preprocessor),
            ("$a", K::Variable),
            ("=", K::Operator),
            ("'x'", K::String),
            ("// c ", K::Comment),
            ("?>", K::Preprocessor),
            ("<?php", K::Preprocessor),
            ("/* multi", K::Comment),
            ("line */", K::Comment),
            ("echo", K::Keyword),
            ("\"hi\"", K::String),
            ("$b", K::Variable),
            ("=", K::Operator),
            ("<<<EOT", K::Preprocessor),
            ("Hello $a", K::String),
            ("EOT", K::Preprocessor),
            ("?>", K::Preprocessor),
            ("<p", K::Tag),
            ("class", K::Attribute),
            ("\"x\"", K::String),
            (">", K::Tag),
            ("<?=", K::Preprocessor),
            ("$a", K::Variable),
            ("?>", K::Preprocessor),
            ("</p>", K::Tag),
        ],
    );
}

#[test]
fn golden_html_with_embedded_js_and_css() {
    golden(
        "HTML",
        "<!-- c -->\n<div class=\"a\" id=b>&amp; t</div>\n<script>\nvar x = /re[/]g/i; // hi\n</script>\n<style>\n.a { color: #fff; margin: -2px; }\n</style>\n",
        &[
            ("<!-- c -->", K::Comment),
            ("<div", K::Tag),
            ("class", K::Attribute),
            ("\"a\"", K::String),
            ("id", K::Attribute),
            ("b", K::String),
            (">", K::Tag),
            ("&amp;", K::Escape),
            ("</div>", K::Tag),
            ("<script>", K::Tag),
            ("var", K::Keyword),
            ("=", K::Operator),
            ("/re[/]g/i", K::String),
            ("// hi", K::Comment),
            ("</script>", K::Tag),
            ("<style>", K::Tag),
            (".a", K::Type),
            ("color", K::Keyword),
            ("#fff", K::Attribute),
            ("margin", K::Keyword),
            ("-2px", K::Number),
            ("</style>", K::Tag),
        ],
    );
}

#[test]
fn golden_css() {
    golden(
        "CSS",
        "@media (max-width: 10px) {\n  .box > a { color: red; margin: 1.5em 0; background: url(x.png) !important; }\n}\n/* c */\n#id { }\n",
        &[
            ("@media", K::Preprocessor),
            ("max-width", K::Keyword),
            ("10px", K::Number),
            (".box", K::Type),
            (">", K::Operator),
            ("a", K::Tag),
            ("color", K::Keyword),
            ("red", K::Keyword2),
            ("margin", K::Keyword),
            ("1.5em", K::Number),
            ("0", K::Number),
            ("background", K::Keyword),
            ("url", K::Function),
            ("important", K::Keyword2),
            ("/* c */", K::Comment),
            ("#id", K::Attribute),
        ],
    );
}

#[test]
fn golden_javascript() {
    golden(
        "JavaScript",
        "const f = (a) => a / 2; // c\nconst r = /ab+c/gi.test(s);\nlet t = `a\nb`;\n",
        &[
            ("const", K::Keyword),
            ("=", K::Operator),
            ("=>", K::Operator),
            ("/", K::Operator),
            ("2", K::Number),
            ("// c", K::Comment),
            ("const", K::Keyword),
            ("=", K::Operator),
            ("/ab+c/gi", K::String),
            ("test", K::Function),
            ("let", K::Keyword),
            ("=", K::Operator),
            ("`a", K::String),
            ("b`", K::String),
        ],
    );
}

#[test]
fn golden_typescript() {
    golden(
        "TypeScript",
        "interface A { x: number; }\nconst v: Record<string, unknown> = {};\n",
        &[
            ("interface", K::Keyword),
            (":", K::Operator),
            ("number", K::Type),
            ("const", K::Keyword),
            (":", K::Operator),
            ("Record", K::Type),
            ("<", K::Operator),
            ("string", K::Type),
            ("unknown", K::Type),
            (">", K::Operator),
            ("=", K::Operator),
        ],
    );
}

#[test]
fn golden_sql() {
    golden(
        "SQL",
        "SELECT id, COUNT(*) FROM users -- c\nWHERE name = 'O''Brien' AND age >= 18 /* x */ ORDER BY id;\n",
        &[
            ("SELECT", K::Keyword),
            ("COUNT", K::Keyword3),
            ("*", K::Operator),
            ("FROM", K::Keyword),
            ("-- c", K::Comment),
            ("WHERE", K::Keyword),
            ("=", K::Operator),
            ("'O''Brien'", K::String),
            ("AND", K::Keyword),
            (">=", K::Operator),
            ("18", K::Number),
            ("/* x */", K::Comment),
            ("ORDER", K::Keyword),
            ("BY", K::Keyword),
        ],
    );
}

#[test]
fn golden_xml() {
    golden(
        "XML",
        "<?xml version=\"1.0\"?>\n<!-- c -->\n<root a=\"1\"><![CDATA[ x\n y ]]><b>&lt;t&gt;</b></root>\n",
        &[
            ("<?xml version=\"1.0\"?>", K::Preprocessor),
            ("<!-- c -->", K::Comment),
            ("<root", K::Tag),
            ("a", K::Attribute),
            ("\"1\"", K::String),
            (">", K::Tag),
            ("<![CDATA[ x", K::String),
            (" y ]]>", K::String),
            ("<b>", K::Tag),
            ("&lt;", K::Escape),
            ("&gt;", K::Escape),
            ("</b></root>", K::Tag),
        ],
    );
}

// ---------------------------------------------------------------------------
// Multi-line state, nesting
// ---------------------------------------------------------------------------

#[test]
fn block_comment_over_three_lines_carries_state() {
    let src = "a /* one\ntwo\nthree */ b\nint x;\n";
    let d = doc("C", src);
    let sp: Vec<(String, K)> = d
        .spans_in_range(0..src.len())
        .into_iter()
        .map(|s| (src[s.byte_range].to_string(), s.kind))
        .collect();
    assert_eq!(
        sp,
        vec![
            ("/* one".to_string(), K::Comment),
            ("two".to_string(), K::Comment),
            ("three */".to_string(), K::Comment),
            ("int".to_string(), K::Type),
        ]
    );
    // Asking only for the last line still knows we are outside the comment.
    let last = src.find("int").unwrap();
    let only = d.spans_in_range(last..src.len());
    assert_eq!(&src[only[0].byte_range.clone()], "int");
    // ...and the middle line alone knows it is inside one.
    let mid = src.find("two").unwrap();
    let m = d.spans_in_range(mid..mid + 3);
    assert_eq!(m[0].kind, K::Comment);
}

#[test]
fn multiline_string_carries_state() {
    let src = "var s = `first\nsecond\nthird`; var n = 1;\n";
    let sp = spans("JavaScript", src);
    assert!(sp.contains(&("`first".to_string(), K::String)));
    assert!(sp.contains(&("second".to_string(), K::String)));
    assert!(sp.contains(&("third`".to_string(), K::String)));
    assert!(sp.contains(&("1".to_string(), K::Number)));
    assert_eq!(sp.iter().filter(|(t, k)| t == "var" && *k == K::Keyword).count(), 2);
}

#[test]
fn unterminated_single_line_string_stops_at_eol() {
    let sp = spans("C", "char *s = \"oops;\nint y;\n");
    assert!(sp.contains(&("\"oops;".to_string(), K::String)));
    assert!(sp.contains(&("int".to_string(), K::Type)));
}

#[test]
fn shell_heredoc_body_is_string() {
    let sp = spans("Shell", "cat <<EOF\nif x # not a comment\nEOF\necho done\n");
    assert!(sp.contains(&("<<EOF".to_string(), K::Preprocessor)));
    assert!(sp.contains(&("if x # not a comment".to_string(), K::String)));
    assert!(sp.contains(&("EOF".to_string(), K::Preprocessor)));
    assert!(sp.contains(&("echo".to_string(), K::Keyword2)));
}

#[test]
fn php_open_tag_inside_javascript_block() {
    let src = "<script>\nvar a = <?php echo 1; ?>;\nvar b = 2;\n</script>\n<?php $c = 3; ?>\n";
    let sp = spans("PHP", src);
    // JS keyword before and after the PHP island
    assert_eq!(sp.iter().filter(|(t, k)| t == "var" && *k == K::Keyword).count(), 2);
    assert!(sp.contains(&("echo".to_string(), K::Keyword)));
    assert!(sp.contains(&("$c".to_string(), K::Variable)));
}

#[test]
fn script_end_tag_inside_js_string_still_closes_script() {
    let sp = spans("HTML", "<script>var s = \"a</script><b>\n");
    assert!(sp.contains(&("</script><b>".to_string(), K::Tag)), "{sp:?}");
}

// ---------------------------------------------------------------------------
// Detection
// ---------------------------------------------------------------------------

#[test]
fn detection_by_name_extension_and_first_line() {
    let r = reg();
    let name = |p: &str, fl: Option<&str>| r.detect(Path::new(p), fl).map(|i| r.def(i).name);
    assert_eq!(name("a/b/main.c", None), Some("C"));
    assert_eq!(name("x.HPP", None), Some("C++"));
    assert_eq!(name("Program.cs", None), Some("C#"));
    assert_eq!(name("index.php", None), Some("PHP"));
    assert_eq!(name("Dockerfile", None), Some("Dockerfile"));
    assert_eq!(name("Dockerfile.dev", None), Some("Dockerfile"));
    assert_eq!(name("Makefile", None), Some("Makefile"));
    assert_eq!(name("deploy", Some("#!/usr/bin/env python3")), Some("Python"));
    assert_eq!(name("run", Some("#!/bin/bash -e")), Some("Shell"));
    assert_eq!(name("t", Some("#!/usr/bin/perl")), Some("Perl"));
    assert_eq!(name("feed", Some("<?xml version=\"1.0\"?>")), Some("XML"));
    assert_eq!(name("page", Some("<!DOCTYPE html>")), Some("HTML"));
    assert_eq!(name("unknown.xyz", None), None);
    assert_eq!(name("noext", Some("just text")), None);
    // extension beats first line
    assert_eq!(name("x.rb", Some("#!/bin/sh")), Some("Ruby"));
}

#[test]
fn language_id_mapping_keeps_legacy_variants() {
    use crate::grammars::{detect_language, detect_language_with_first_line, syntax_index, LanguageId};
    assert_eq!(detect_language(Path::new("a.js")), LanguageId::JavaScript);
    assert_eq!(detect_language(Path::new("a.rs")), LanguageId::Rust);
    assert_eq!(detect_language(Path::new("a.zzz")), LanguageId::PlainText);
    let c = detect_language(Path::new("a.c"));
    assert!(matches!(c, LanguageId::Defined(_)));
    assert_eq!(c.display_name(), "C");
    assert_eq!(detect_language(Path::new("Makefile")).display_name(), "Makefile");
    let sh = detect_language_with_first_line(Path::new("run"), Some("#!/bin/sh"));
    assert_eq!(sh.display_name(), "Shell");
    assert!(syntax_index(LanguageId::Python).is_some());
    assert!(syntax_index(LanguageId::Rust).is_none());
    assert!(syntax_index(LanguageId::PlainText).is_none());
}

// ---------------------------------------------------------------------------
// User definitions
// ---------------------------------------------------------------------------

fn user(files: &[(&str, &str)]) -> Registry {
    let v: Vec<(String, String)> = files.iter().map(|(a, b)| (a.to_string(), b.to_string())).collect();
    Registry::build(&v)
}

#[test]
fn user_definition_adds_a_language() {
    let r = user(&[(
        "mini.toml",
        "name = \"MiniLang\"\nextensions = [\"mini\"]\nline_comments = [\"--\"]\n[[keywords]]\nname = \"K\"\nwords = \"begin end\"\n",
    )]);
    assert!(r.errors().is_empty(), "{:?}", r.errors());
    let i = r.detect(Path::new("x.mini"), None).unwrap();
    assert_eq!(r.def(i).name, "MiniLang");
    let d = SyntaxDoc::new(Arc::new(r), i, "begin -- c\n");
    let sp = d.spans_in_range(0..11);
    assert_eq!(sp[0].kind, K::Keyword);
    assert_eq!(sp[1].kind, K::Comment);
}

#[test]
fn user_definition_overrides_shipped_by_name_and_extension() {
    let r = user(&[(
        "c.toml",
        "name = \"C\"\nextensions = [\"c\", \"h\", \"cx\"]\n[[keywords]]\nname = \"K\"\nwords = \"mykw\"\n",
    )]);
    assert!(r.errors().is_empty(), "{:?}", r.errors());
    let i = r.by_name("C").unwrap();
    assert_eq!(r.def(i).origin, Origin::User);
    assert_eq!(r.def(i).word_kind("mykw"), Some(K::Keyword));
    assert_eq!(r.def(i).word_kind("while"), None); // replaced, not merged
    assert_eq!(r.detect(Path::new("a.cx"), None), Some(i));
    // same index as the shipped one: LanguageId::Defined stays valid
    assert_eq!(Some(i), reg().by_name("C"));
    // a different name claiming a shipped extension wins by priority
    let r2 = user(&[("z.toml", "name = \"Zeta\"\nextensions = [\"c\"]\n")]);
    let z = r2.detect(Path::new("a.c"), None).unwrap();
    assert_eq!(r2.def(z).name, "Zeta");
}

#[test]
fn user_definition_can_extend_the_shipped_one() {
    let r = user(&[(
        "c.toml",
        "name = \"C\"\nextends = \"C\"\n[[keywords]]\nname = \"Keywords\"\nwords = \"myword\"\n",
    )]);
    assert!(r.errors().is_empty(), "{:?}", r.errors());
    let d = r.def(r.by_name("C").unwrap());
    assert_eq!(d.word_kind("myword"), Some(K::Keyword));
    assert_eq!(d.word_kind("while"), Some(K::Keyword));
}

#[test]
fn invalid_toml_is_reported_with_line_and_does_not_break_others() {
    let r = user(&[
        ("bad.toml", "name = \"Bad\"\nextensions = [\"bad\"\nline_comments = 3\n"),
        ("good.toml", "name = \"Good\"\nextensions = [\"good\"]\n"),
    ]);
    assert_eq!(r.errors().len(), 1, "{:?}", r.errors());
    let e = &r.errors()[0];
    assert_eq!(e.source, "bad.toml");
    assert!(e.line.is_some(), "line expected: {e}");
    assert!(e.to_string().starts_with("bad.toml:"));
    assert!(r.by_name("Good").is_some());
    assert!(r.by_name("C").is_some(), "shipped languages survive");
}

#[test]
fn semantic_errors_name_the_line() {
    let r = user(&[(
        "k.toml",
        "name = \"K\"\nextensions = [\"k\"]\n\n[[keywords]]\nname = \"G\"\nkind = \"sparkly\"\nwords = \"a b\"\n",
    )]);
    assert_eq!(r.errors().len(), 1, "{:?}", r.errors());
    let e = &r.errors()[0];
    assert!(e.message.contains("sparkly"), "{e}");
    assert_eq!(e.line, Some(6));
    assert!(r.by_name("K").is_none(), "invalid definition is skipped");

    let r = user(&[("u.toml", "name = \"U\"\nwhat_is_this = 1\n")]);
    assert_eq!(r.errors().len(), 1);
    assert_eq!(r.errors()[0].line, Some(2));

    let r = user(&[("e.toml", "name = \"E\"\nhost = \"Nonexistent\"\nextends = \"Nope\"\n")]);
    assert!(r.errors().iter().any(|e| e.message.contains("Nope")));
}

#[test]
fn user_dir_loading_reads_toml_files_and_ignores_missing_dir() {
    let dir = std::env::temp_dir().join(format!("brouillon-syntax-test-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("a.toml"), "name = \"FromDisk\"\nextensions = [\"fd\"]\n").unwrap();
    std::fs::write(dir.join("broken.toml"), "name = [").unwrap();
    std::fs::write(dir.join("notes.txt"), "ignored").unwrap();
    let r = Registry::build_with_dir(Some(&dir));
    assert!(r.by_name("FromDisk").is_some());
    assert_eq!(r.errors().len(), 1);
    assert_eq!(r.errors()[0].source, "broken.toml");
    let _ = std::fs::remove_dir_all(&dir);
    let r = Registry::build_with_dir(Some(&dir.join("does-not-exist")));
    assert!(r.errors().is_empty());
    assert!(r.by_name("C").is_some());
}

// ---------------------------------------------------------------------------
// P1 smoke tests
// ---------------------------------------------------------------------------

fn has(sp: &[(String, K)], t: &str, k: K) -> bool {
    sp.iter().any(|(x, kk)| x == t && *kk == k)
}

#[test]
fn p1_languages_color_their_basics() {
    let sp = spans("Python", "@dec\ndef f(self):  # c\n    return \"\"\"doc\nmore\"\"\" + 1\n");
    assert!(has(&sp, "@dec", K::Attribute) && has(&sp, "def", K::Keyword) && has(&sp, "# c", K::Comment), "{sp:?}");
    assert!(has(&sp, "more\"\"\"", K::String) && has(&sp, "self", K::Keyword4), "{sp:?}");

    let sp = spans("Ruby", "=begin\nx\n=end\nputs :sym, @iv # c\n");
    assert!(has(&sp, "x", K::Comment) && has(&sp, ":sym", K::Attribute) && has(&sp, "@iv", K::Variable), "{sp:?}");

    let sp = spans("Perl", "my $x = 'a'; # c\n=pod\ndoc\n=cut\nprint $x;\n");
    assert!(has(&sp, "my", K::Keyword) && has(&sp, "$x", K::Variable) && has(&sp, "doc", K::Comment), "{sp:?}");

    let sp = spans("Visual Basic", "Dim x As Integer ' c\nIf x Then MsgBox(\"a\"\"b\")\n");
    assert!(has(&sp, "Dim", K::Keyword) && has(&sp, "Integer", K::Type) && has(&sp, "' c", K::Comment), "{sp:?}");
    assert!(has(&sp, "\"a\"\"b\"", K::String), "{sp:?}");

    let sp = spans("Go", "func main() { x := `raw\nmore`; _ = 'a' }\n");
    assert!(has(&sp, "func", K::Keyword) && has(&sp, "more`", K::String), "{sp:?}");

    let sp = spans("Shell", "export A=\"$HOME/x\" # c\nif [ -f ${B} ]; then echo $A; fi\n");
    assert!(has(&sp, "export", K::Keyword2) && has(&sp, "# c", K::Comment) && has(&sp, "${B}", K::Variable), "{sp:?}");
    assert!(has(&sp, "$A", K::Variable) && has(&sp, "fi", K::Keyword), "{sp:?}");

    let sp = spans("PowerShell", "<# block\n#> $x = Get-Item -eq 'a' # c\n");
    assert!(has(&sp, "<# block", K::Comment) && has(&sp, "$x", K::Variable) && has(&sp, "Get-Item", K::Keyword2), "{sp:?}");
    assert!(has(&sp, "-eq", K::Keyword3), "{sp:?}");

    let sp = spans("Batch", "REM c\n:label\n@echo %PATH%\nif exist x goto :eof\n");
    assert!(has(&sp, "REM c", K::Comment) && has(&sp, ":label", K::Function) && has(&sp, "echo", K::Keyword), "{sp:?}");
    assert!(has(&sp, "%PATH", K::Variable) && has(&sp, "goto", K::Keyword), "{sp:?}");

    let sp = spans("YAML", "---\nname: \"x\" # c\nitems:\n  - a: 1\n  - &anc true\n");
    assert!(has(&sp, "---", K::Preprocessor) && has(&sp, "name", K::Attribute) && has(&sp, "# c", K::Comment), "{sp:?}");
    assert!(has(&sp, "&anc", K::Variable) && has(&sp, "true", K::Keyword) && has(&sp, "1", K::Number), "{sp:?}");

    let sp = spans("INI", "; c\n[section]\nkey = value\n");
    assert!(has(&sp, "; c", K::Comment) && has(&sp, "[section]", K::Tag) && has(&sp, "key", K::Attribute), "{sp:?}");

    let sp = spans("Lua", "--[[ a\nb ]] local x = [[s\nt]] -- c\n");
    assert!(has(&sp, "--[[ a", K::Comment) && has(&sp, "local", K::Keyword) && has(&sp, "t]]", K::String), "{sp:?}");

    let sp = spans("JSON", "{\"a\": [1, true, \"s\"]}\n");
    assert!(has(&sp, "\"a\"", K::Attribute) && has(&sp, "1", K::Number) && has(&sp, "true", K::Keyword2), "{sp:?}");
    assert!(has(&sp, "\"s\"", K::String), "{sp:?}");

    let sp = spans("TOML", "[pkg]\nname = \"x\" # c\nn = 1_000\n");
    assert!(has(&sp, "[pkg]", K::Tag) && has(&sp, "name", K::Attribute) && has(&sp, "1_000", K::Number), "{sp:?}");

    let sp = spans("Markdown", "# Title\nsome `code` and **bold**\n```rust\nfn main() {}\n```\n> quote\n");
    assert!(has(&sp, "# Title", K::Keyword) && has(&sp, "`code`", K::String) && has(&sp, "**bold**", K::Keyword2), "{sp:?}");
    assert!(has(&sp, "fn main() {}", K::String) && has(&sp, "> quote", K::Comment), "{sp:?}");

    let sp = spans("Dockerfile", "FROM rust:1 AS b\nENV A=${B} # c\nrun echo\n");
    assert!(has(&sp, "FROM", K::Keyword) && has(&sp, "${B}", K::Variable) && has(&sp, "run", K::Keyword), "{sp:?}");

    let sp = spans("Makefile", "CC := gcc # c\nall: main.o\n\t$(CC) -o $@ $<\n");
    assert!(has(&sp, "all", K::Function) && has(&sp, "$(CC)", K::Variable) && has(&sp, "$@", K::Variable), "{sp:?}");

    let sp = spans("Diff", "--- a/x\n+++ b/x\n@@ -1 +1 @@\n-old\n+new\n ctx\n");
    assert!(has(&sp, "--- a/x", K::Keyword) && has(&sp, "@@ -1 +1 @@", K::Preprocessor), "{sp:?}");
    assert!(has(&sp, "-old", K::Tag) && has(&sp, "+new", K::String), "{sp:?}");

    let sp = spans("Log", "2024-01-02 10:00:01 ERROR failed to open \"x\"\n10:00:02 warn slow\n");
    assert!(has(&sp, "ERROR", K::Tag) && has(&sp, "warn", K::Keyword4) && has(&sp, "\"x\"", K::String), "{sp:?}");
}

#[test]
fn ruby_colors_keywords_strings_numbers_and_comments() {
    let sp = spans("Ruby", "# note
class G < Base
  def hi(n = \"w\")
    puts 'x' if n != :none
    @c += 42
  end
end
");
    assert!(has(&sp, "# note", K::Comment), "{sp:?}");
    assert!(has(&sp, "class", K::Keyword) && has(&sp, "def", K::Keyword) && has(&sp, "end", K::Keyword) && has(&sp, "if", K::Keyword), "{sp:?}");
    assert!(has(&sp, "\"w\"", K::String) && has(&sp, "'x'", K::String), "{sp:?}");
    assert!(has(&sp, "42", K::Number) && has(&sp, ":none", K::Attribute) && has(&sp, "@c", K::Variable), "{sp:?}");
}

// ---------------------------------------------------------------------------
// Incremental editing
// ---------------------------------------------------------------------------

fn all_spans(d: &SyntaxDoc) -> Vec<(usize, usize, K)> {
    d.spans_in_range(0..d.text().len()).into_iter().map(|s| (s.byte_range.start, s.byte_range.end, s.kind)).collect()
}

fn apply(d: &mut SyntaxDoc, start: usize, old_len: usize, ins: &str) -> String {
    let mut t = d.text().to_string();
    t.replace_range(start..start + old_len, ins);
    d.apply_edit(&t, start, old_len, ins.len());
    assert!(d.index_is_consistent(), "line index after edit at {start}");
    t
}

#[test]
fn incremental_edit_matches_full_relex() {
    let base = "int a;\n/* c1\nc2 */ int b;\nchar *s = \"x\";\n// tail\nint z;\n";
    let edits: Vec<(usize, usize, &str)> = vec![
        (base.find("int a").unwrap(), 0, "/* "),             // open a comment above everything
        (base.find("c2").unwrap(), 2, "c2 */ /*"),           // shift the end of the comment
        (base.find("\"x\"").unwrap() + 1, 1, "y\nz"),        // split a string across lines
        (base.find("// tail").unwrap(), 7, ""),              // delete a line comment
        (0, base.len(), "int only;\n"),                      // replace everything
        (base.find("char").unwrap(), 0, "\n\n\n"),           // insert blank lines
    ];
    for (start, old, ins) in edits {
        let mut d = doc("C", base);
        let _ = all_spans(&d); // lex everything first so states are cached
        let new_text = apply(&mut d, start, old, ins);
        let fresh = doc("C", &new_text);
        assert_eq!(all_spans(&d), all_spans(&fresh), "edit at {start} (-{old} +{ins:?})");
    }
}

#[test]
fn incremental_edit_stops_once_state_converges() {
    let mut text = String::new();
    for i in 0..2000 {
        text.push_str(&format!("int v{i} = {i}; // line\n"));
    }
    let mut d = doc("C", &text);
    let _ = all_spans(&d);
    let lexed_before = d.lexed_lines();
    assert!(lexed_before >= 2000);
    // a plain edit in the middle keeps the cached states of the lines below
    let at = text.find("v1000").unwrap();
    apply(&mut d, at, 5, "w1000");
    assert!(d.lexed_lines() >= lexed_before - 2, "tail states must be kept, got {}", d.lexed_lines());
    // opening a block comment invalidates what follows
    let at = text.find("v500").unwrap();
    let t2 = apply(&mut d, at, 0, "/*");
    let fresh = doc("C", &t2);
    assert_eq!(all_spans(&d), all_spans(&fresh));
}

fn rope_doc(lang: &str, src: &str) -> SyntaxDoc {
    let r = reg();
    let i = r.by_name(lang).unwrap_or_else(|| panic!("no language {lang}"));
    SyntaxDoc::new_rope(r, i, src.parse().unwrap())
}

/// A rope-backed document (the text is shared with the buffer, never copied) must colour, and re-colour
/// after every kind of edit, exactly like the `String`-backed one and like a fresh document.
#[test]
fn rope_backed_document_equals_the_string_one() {
    let mut text = String::new();
    for i in 0..3000 {
        text.push_str(&format!("int v{i} = {i}; /* c{i}\n more */ // caf\u{e9} \u{20ac}\r\nchar *s = \"x{i}\";\n"));
    }
    assert_eq!(all_spans(&doc("C", &text)), all_spans(&rope_doc("C", &text)));
    let edits: Vec<(usize, usize, &str)> = vec![
        (text.find("v500").unwrap(), 0, "/*"),             // opens a comment: invalidates what follows
        (text.find("v1000").unwrap(), 5, "w1000"),         // plain edit, tail states kept
        (text.find("c7").unwrap(), 2, ""),                 // deletion
        (text.find("\u{e9}").unwrap(), 2, "e\u{301}\u{20ac}"), // multi-byte replaced by multi-byte
        (text.len(), 0, "tail\n"),                         // append
        (0, 0, "// head\n"),                               // prepend
        (0, text.len(), "int only;\n"),                    // replace everything
        (text.len() / 2, 0, ""),                           // no change at all
    ];
    for (start, old, ins) in edits {
        let mut t = text.clone();
        t.replace_range(start..start + old, ins);
        let mut d = rope_doc("C", &text);
        let _ = all_spans(&d); // lex everything first so states are cached
        d.apply_rope(t.parse().unwrap());
        assert!(d.index_is_consistent(), "apply_rope line index, edit at {start}");
        assert_eq!(d.text(), t.as_str());
        assert_eq!(all_spans(&d), all_spans(&doc("C", &t)), "apply_rope: edit at {start} (-{old} +{ins:?})");
        let mut e = rope_doc("C", &text);
        let _ = all_spans(&e);
        e.apply_edit_rope(t.parse().unwrap(), start, old, ins.len());
        assert!(e.index_is_consistent(), "apply_edit_rope line index, edit at {start}");
        assert_eq!(all_spans(&e), all_spans(&doc("C", &t)), "apply_edit_rope: edit at {start} (-{old} +{ins:?})");
    }
    // A String document turned into a rope one keeps working (set_rope / set_text swap the storage).
    let mut d = doc("C", "int a;\n");
    d.set_rope("int b; /* x\n".parse().unwrap());
    assert_eq!(all_spans(&d), all_spans(&doc("C", "int b; /* x\n")));
    d.set_text("int c;\n");
    assert_eq!(all_spans(&d), all_spans(&doc("C", "int c;\n")));
}

#[test]
fn state_runs_behave_like_a_vector_of_states() {
    use lexer::{Inner, Mode};
    let pool: Vec<State> = vec![
        State { mode: Mode::Code, inner: Inner::None },
        State { mode: Mode::Code, inner: Inner::MComment },
        State { mode: Mode::Markup, inner: Inner::None },
        State { mode: Mode::Code, inner: Inner::Block { rule: 1, depth: 2 } },
    ];
    let mut x = 7u32;
    let mut next = || {
        x = x.wrapping_mul(1103515245).wrapping_add(12345);
        (x >> 16) as usize
    };
    let mut runs = StateRuns::default();
    let mut model: Vec<State> = Vec::new();
    for _ in 0..600 {
        let st = pool[if next() % 4 == 0 { next() % pool.len() } else { 0 }].clone();
        runs.push(st.clone());
        model.push(st);
    }
    assert!(runs.runs.len() < model.len() / 2, "mostly-default states must collapse into few runs ({})", runs.runs.len());
    assert_eq!(runs.len(), model.len());
    for (i, m) in model.iter().enumerate() {
        assert_eq!(runs.get(i), m, "get({i})");
    }
    for n in [0usize, 1, 5, 123, 599, 600, 900] {
        let p = runs.prefix(n);
        assert_eq!(p.len(), n.min(model.len()));
        for i in 0..p.len() {
            assert_eq!(p.get(i), &model[i]);
        }
        let mut t = runs.clone();
        t.truncate(n);
        assert_eq!(t.len(), n.min(model.len()));
        for i in 0..t.len() {
            assert_eq!(t.get(i), &model[i]);
        }
    }
    for from in [0usize, 1, 77, 300, 599, 600, 700] {
        let mut tail = runs.prefix(10);
        let mut want: Vec<State> = model[..10].to_vec();
        tail.append_from(&runs, from);
        want.extend(model.iter().skip(from).cloned());
        assert_eq!(tail.len(), want.len(), "append_from({from})");
        for (i, m) in want.iter().enumerate() {
            assert_eq!(tail.get(i), m);
        }
    }
}

#[test]
fn common_prefix_and_suffix_over_pieces_match_the_flat_bytes() {
    let flat_a = b"hello wide world, hello small world".to_vec();
    let flat_b = b"hello wide new world, hello small world".to_vec();
    let split = |v: &[u8], cuts: &[usize]| -> Vec<Vec<u8>> {
        let mut out = Vec::new();
        let mut last = 0;
        for &c in cuts {
            out.push(v[last..c.min(v.len())].to_vec());
            last = c.min(v.len());
        }
        out.push(v[last..].to_vec());
        out
    };
    for cuts_a in [vec![], vec![3], vec![5, 6, 6, 20], vec![1, 2, 3, 4, 5]] {
        for cuts_b in [vec![], vec![9], vec![4, 8, 15, 16], vec![30]] {
            let (pa, pb) = (split(&flat_a, &cuts_a), split(&flat_b, &cuts_b));
            let (ra, rb): (Vec<&[u8]>, Vec<&[u8]>) = (pa.iter().map(|v| v.as_slice()).collect(), pb.iter().map(|v| v.as_slice()).collect());
            let want_p = flat_a.iter().zip(&flat_b).take_while(|(a, b)| a == b).count();
            let want_s = flat_a.iter().rev().zip(flat_b.iter().rev()).take_while(|(a, b)| a == b).count();
            assert_eq!(common_prefix(&ra, &rb), want_p, "{cuts_a:?} {cuts_b:?}");
            assert_eq!(common_suffix(&ra, &rb, usize::MAX), want_s, "{cuts_a:?} {cuts_b:?}");
            assert_eq!(common_suffix(&ra, &rb, 3), want_s.min(3));
        }
    }
    assert_eq!(common_prefix(&[], &[b"abc"]), 0);
    assert_eq!(common_suffix(&[b"abc"], &[], 9), 0);
}

#[test]
fn lazy_lexing_only_lexes_what_is_requested() {
    let mut text = String::new();
    for i in 0..5000 {
        text.push_str(&format!("int v{i};\n"));
    }
    let d = doc("C", &text);
    assert_eq!(d.lexed_lines(), 1);
    let _ = d.spans_in_range(0..20);
    assert!(d.lexed_lines() < 10, "viewport at the top must not lex the file ({})", d.lexed_lines());
}

// ---------------------------------------------------------------------------
// Highlighter integration (what the editor view consumes)
// ---------------------------------------------------------------------------

#[test]
fn highlighter_serves_table_driven_spans() {
    use crate::grammars::{detect_language, LanguageId};
    use crate::highlight::Highlighter;
    let lang = detect_language(Path::new("m.c"));
    assert!(matches!(lang, LanguageId::Defined(_)));
    let mut hl = Highlighter::new(lang);
    let src = "#include <a.h>\nint x = 1; /* c\nd */\n";
    hl.parse(src);
    assert!(hl.is_parsed());
    let sp = hl.spans_in_range(0..src.len());
    assert!(sp.iter().any(|s| s.kind == K::Preprocessor));
    assert!(sp.iter().any(|s| s.kind == K::Comment && src[s.byte_range.clone()].starts_with("/* c")));
    // one line at a time, the way the painter asks
    let l2 = src.find("int").unwrap()..src.find("d */").unwrap();
    assert!(hl.spans_in_range(l2).iter().any(|s| s.kind == K::Type));
    // incremental update through the Highlighter
    let mut new_src = src.to_string();
    new_src.replace_range(0..0, "// ");
    hl.apply_edit(&new_src, 0, 0, 3);
    let sp = hl.spans_in_range(0..new_src.len());
    assert_eq!(sp[0].kind, K::Comment);
    // legacy languages go through the same engine
    let mut js = Highlighter::new(LanguageId::JavaScript);
    js.parse("const x = 42; // c");
    let kinds: Vec<K> = js.spans_in_range(0..18).iter().map(|s| s.kind).collect();
    assert!(kinds.contains(&K::Keyword) && kinds.contains(&K::Number) && kinds.contains(&K::Comment));
}

// ---------------------------------------------------------------------------
// Performance
// ---------------------------------------------------------------------------

/// ~5 MB of C-like source, deterministic.
fn big_source() -> String {
    let block = "#include <stdio.h>\n/* header comment\n   second line */\nstatic int compute(int a, int b) {\n    char *s = \"text with \\n escape\"; // trailing\n    for (int i = 0; i < 100; i++) { a += b * 0x1F + i; }\n    return a > b ? a : b;\n}\n\n";
    let mut s = String::with_capacity(5 * 1024 * 1024 + block.len());
    while s.len() < 5 * 1024 * 1024 {
        s.push_str(block);
    }
    s
}

fn window_spans(d: &SyntaxDoc, at: usize) -> usize {
    let t = d.text();
    let s = t[..at].rfind('\n').map_or(0, |i| i + 1);
    let mut e = s;
    for _ in 0..60 {
        e = t[e..].find('\n').map_or(t.len(), |i| e + i + 1);
    }
    d.spans_in_range(s..e).len()
}

#[test]
fn perf_visible_window_and_relex_after_edit() {
    let text = big_source();
    let mut d = doc("C", &text);
    let mid = text.len() / 2;
    // cold: lex up to the middle of the file for the first time
    let t = Instant::now();
    let n = window_spans(&d, mid);
    let cold = t.elapsed();
    // warm: same window again
    let t = Instant::now();
    let n2 = window_spans(&d, mid);
    let warm = t.elapsed();
    assert_eq!(n, n2);
    // edit in the middle, then re-lex the same window
    let mut t2 = d.text().to_string();
    let pos = t2[..mid].rfind('\n').unwrap() + 1;
    t2.insert_str(pos, "int inserted = 1;\n");
    let t = Instant::now();
    d.apply_edit(&t2, pos, 0, 18);
    let n3 = window_spans(&d, pos);
    let edit = t.elapsed();
    assert!(n3 > 0);
    println!(
        "[perf] 5MB C: cold window@50% = {:?}, warm window = {:?}, edit+relex window = {:?} (profile: {})",
        cold,
        warm,
        edit,
        if cfg!(debug_assertions) { "debug" } else { "release" }
    );
    if !cfg!(debug_assertions) {
        assert!(warm.as_millis() < 16, "warm window too slow: {warm:?}");
        assert!(edit.as_millis() < 16, "edit + relex too slow: {edit:?}");
    }
}

// ---------------------------------------------------------------------------
// Robustness
// ---------------------------------------------------------------------------

/// Deterministic pseudo-random soups of "dangerous" tokens must never panic,
/// loop forever, or yield overlapping / out-of-range / mid-character spans.
#[test]
fn fuzz_every_language_with_token_soup() {
    const TOKENS: &[&str] = &[
        "\"", "'", "`", "\\", "/*", "*/", "//", "#", "--", "<!--", "-->", "<?php", "?>", "<?", "<", ">", "</", "/>",
        "<script>", "</script>", "<style>", "</style>", "<![CDATA[", "]]>", "<<<EOT", "<<EOF", "EOT", "EOF", "=begin",
        "=end", "=pod", "=cut", "--[[", "]]", "[[", "${", "$(", "$", "@", "%", "&", ";", ":", "=", "{", "}", "(", ")",
        "[", "]", "0x", "1e", ".5", "12", "-", "+", " ", "  ", "\t", "\r", "\n", "\n", "\n", "é", "日本", "😀", "a", "foo",
        "if", "end", "select", "class", "function", "REM ", "::", "!", "?", "*", "_",
    ];
    let r = reg();
    let mut seed: u64 = 0x9e37_79b9_7f4a_7c15;
    let mut next = move || {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        seed
    };
    for (i, name) in r.names().into_iter().enumerate() {
        for _round in 0..12 {
            let mut src = String::new();
            for _ in 0..(next() % 120 + 1) {
                src.push_str(TOKENS[(next() % TOKENS.len() as u64) as usize]);
            }
            let d = SyntaxDoc::new(r.clone(), i as u16, &src);
            let sp = d.spans_in_range(0..src.len());
            let mut prev_end = 0;
            for s in &sp {
                let b = &s.byte_range;
                assert!(b.start < b.end && b.end <= src.len(), "{name}: bad range {b:?} in {src:?}");
                assert!(b.start >= prev_end, "{name}: overlapping/unordered {b:?} after {prev_end} in {src:?}");
                assert!(src.is_char_boundary(b.start) && src.is_char_boundary(b.end), "{name}: mid-char span {b:?} in {src:?}");
                prev_end = b.end;
            }
            // line by line (the painter's access pattern) gives the same spans
            let mut by_line = Vec::new();
            let mut off = 0;
            for line in src.split_inclusive('\n') {
                by_line.extend(d.spans_in_range(off..off + line.len()));
                off += line.len();
            }
            let a: Vec<_> = sp.iter().map(|s| (s.byte_range.clone(), s.kind)).collect();
            let b: Vec<_> = by_line.iter().map(|s| (s.byte_range.clone(), s.kind)).collect();
            assert_eq!(a, b, "{name}: whole-range vs per-line spans differ for {src:?}");
        }
    }
}

#[test]
fn registry_build_time_is_reported() {
    let t = Instant::now();
    let r = Registry::build(&[]);
    let el = t.elapsed();
    println!("[perf] Registry::build (29 shipped definitions) = {:?} ({})", el, if cfg!(debug_assertions) { "debug" } else { "release" });
    assert!(r.errors().is_empty());
    if !cfg!(debug_assertions) {
        assert!(el.as_millis() < 100, "registry build too slow: {el:?}");
    }
}
