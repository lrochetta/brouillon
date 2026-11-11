//! Tree-sitter incremental highlighting, viewport-scoped.
//!
//! Day-2 scope: parse the full buffer once, then on each viewport scroll
//! run the canonical `tree-sitter-rust` highlights query against the
//! visible byte range. Captures are mapped to a small `SyntaxKind` palette
//! whose variants line up 1:1 with the `theme::Theme::syn_*` color slots.
//!
//! NO incremental edit support yet — that requires `Parser::parse` with
//! the previous tree and `InputEdit`s, which we'll wire on day-3 once
//! buffer edits exist.
//!
//! The default grammar is `tree-sitter-rust` 0.24, which exports the
//! `LANGUAGE: LanguageFn` constant (`set_language` wants `&Language`,
//! hence the `.into()` conversion below).
//!
//! Query source: canonical `tree-sitter-rust/queries/highlights.scm` with
//! a single capture rename — `(integer_literal)` and `(float_literal)`
//! are tagged `@number` instead of the upstream `@constant.builtin` so
//! the brief's `Number` slot has a clean target. `(boolean_literal)`
//! keeps `@constant.builtin` (it falls into `Default`, which is correct
//! since the theme has no boolean slot today).

use std::ops::Range;
use std::sync::LazyLock;
use regex::Regex;
use tree_sitter::{Parser, Query, QueryCursor, StreamingIterator, Tree};
use crate::grammars::LanguageId;

/// A syntax-class category. Maps to `theme::Theme::syn_*` color slots.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SyntaxKind {
    /// Reserved words — `fn`, `let`, `if`, `match`, etc.
    Keyword,
    /// Quoted literals — `"..."`, `'c'`, `r#"..."#`.
    String,
    /// Line and block comments — `// ...`, `/* ... */`.
    Comment,
    /// Primitive types + user types via capture.
    Type,
    /// Numeric literals — `42`, `3.14`, `0xff`.
    Number,
    /// Delimiters, brackets, operators.
    Punct,
    /// Generic identifier fallback — variables, functions, properties.
    Identifier,
    /// Unmatched / fallback color slot.
    Default,
    /// Matching bracket pair character.
    BracketMatch,
    /// Unmatched bracket character.
    BracketUnmatched,
    /// Detected URL or email link.
    Link,
    // --- Table-driven lexer categories (`syntax` module). Appended after the
    // original variants so existing code keeps compiling unchanged.
    /// Keyword group 2 ("keyword 2"). Group 1 is `Keyword`.
    Keyword2,
    /// Keyword group 3.
    Keyword3,
    /// Keyword group 4.
    Keyword4,
    /// Keyword group 5 (also used for groups 6..10).
    Keyword5,
    /// Preprocessor / directive — `#include`, `<?php`, `<!DOCTYPE`.
    Preprocessor,
    /// Operator characters — `+ - == &&`.
    Operator,
    /// Function name at a call site (`name(`).
    Function,
    /// Markup tag — `<div`, `</p>`.
    Tag,
    /// Markup attribute / object key / annotation.
    Attribute,
    /// Escape sequence or entity (backslash-n, &amp;).
    Escape,
    /// Variable sigil word — `$var`, `@ivar`, `%VAR%`.
    Variable,
}

impl SyntaxKind {
    /// Alias for the first keyword group (`Keyword`).
    #[allow(non_upper_case_globals)]
    pub const Keyword1: SyntaxKind = SyntaxKind::Keyword;

    /// Keyword kind for a 1-based group index (up to 10 groups are allowed; the
    /// palette has 5 colours, groups 5..10 share the last one).
    pub fn keyword_group(n: usize) -> SyntaxKind {
        match n {
            0 | 1 => SyntaxKind::Keyword,
            2 => SyntaxKind::Keyword2,
            3 => SyntaxKind::Keyword3,
            4 => SyntaxKind::Keyword4,
            _ => SyntaxKind::Keyword5,
        }
    }

    /// Parse the kind names used in language definition files.
    pub fn from_name(name: &str) -> Option<SyntaxKind> {
        Some(match name.trim().to_ascii_lowercase().as_str() {
            "keyword" | "keyword1" => SyntaxKind::Keyword,
            "keyword2" => SyntaxKind::Keyword2,
            "keyword3" => SyntaxKind::Keyword3,
            "keyword4" => SyntaxKind::Keyword4,
            "keyword5" => SyntaxKind::Keyword5,
            "string" => SyntaxKind::String,
            "comment" => SyntaxKind::Comment,
            "type" => SyntaxKind::Type,
            "number" => SyntaxKind::Number,
            "punct" => SyntaxKind::Punct,
            "identifier" => SyntaxKind::Identifier,
            "preprocessor" | "directive" => SyntaxKind::Preprocessor,
            "operator" => SyntaxKind::Operator,
            "function" => SyntaxKind::Function,
            "tag" => SyntaxKind::Tag,
            "attribute" => SyntaxKind::Attribute,
            "escape" | "special" => SyntaxKind::Escape,
            "variable" => SyntaxKind::Variable,
            _ => return None,
        })
    }
}

/// A range of bytes tagged with a syntax class. `byte_range` is
/// inclusive-exclusive, matching `Range<usize>` semantics everywhere else
/// in the codebase.
#[derive(Debug, Clone)]
pub struct HighlightSpan {
    pub byte_range: Range<usize>,
    pub kind: SyntaxKind,
}

/// Canonical tree-sitter-rust highlights query, modified to tag numeric
/// literals with `@number` (upstream uses `@constant.builtin`, which is
/// shared with booleans — splitting them lets `Number` map cleanly).
///
/// Anything not handled here falls through to `SyntaxKind::Default` via
/// the `capture_index_to_kind` lookup at construction time.
const RUST_HIGHLIGHTS_QUERY: &str = r#"
; Identifiers

(type_identifier) @type
(primitive_type) @type.builtin
(field_identifier) @property

; Identifier conventions

; Assume all-caps names are constants
((identifier) @constant
 (#match? @constant "^[A-Z][A-Z\\d_]+$"))

; Assume uppercase names are enum constructors
((identifier) @constructor
 (#match? @constructor "^[A-Z]"))

; Assume that uppercase names in paths are types
((scoped_identifier
  path: (identifier) @type)
 (#match? @type "^[A-Z]"))
((scoped_identifier
  path: (scoped_identifier
    name: (identifier) @type))
 (#match? @type "^[A-Z]"))
((scoped_type_identifier
  path: (identifier) @type)
 (#match? @type "^[A-Z]"))
((scoped_type_identifier
  path: (scoped_identifier
    name: (identifier) @type))
 (#match? @type "^[A-Z]"))

(struct_pattern
  type: (scoped_type_identifier
    name: (type_identifier) @constructor))

; Function calls

(call_expression
  function: (identifier) @function)
(call_expression
  function: (field_expression
    field: (field_identifier) @function.method))
(call_expression
  function: (scoped_identifier
    "::"
    name: (identifier) @function))

(generic_function
  function: (identifier) @function)
(generic_function
  function: (scoped_identifier
    name: (identifier) @function))
(generic_function
  function: (field_expression
    field: (field_identifier) @function.method))

(macro_invocation
  macro: (identifier) @function.macro
  "!" @function.macro)

; Function definitions

(function_item (identifier) @function)
(function_signature_item (identifier) @function)

(line_comment) @comment
(block_comment) @comment

(line_comment (doc_comment)) @comment.documentation
(block_comment (doc_comment)) @comment.documentation

"(" @punctuation.bracket
")" @punctuation.bracket
"[" @punctuation.bracket
"]" @punctuation.bracket
"{" @punctuation.bracket
"}" @punctuation.bracket

(type_arguments
  "<" @punctuation.bracket
  ">" @punctuation.bracket)
(type_parameters
  "<" @punctuation.bracket
  ">" @punctuation.bracket)

"::" @punctuation.delimiter
":" @punctuation.delimiter
"." @punctuation.delimiter
"," @punctuation.delimiter
";" @punctuation.delimiter

(parameter (identifier) @variable.parameter)

(lifetime (identifier) @label)

"as" @keyword
"async" @keyword
"await" @keyword
"break" @keyword
"const" @keyword
"continue" @keyword
"default" @keyword
"dyn" @keyword
"else" @keyword
"enum" @keyword
"extern" @keyword
"fn" @keyword
"for" @keyword
"gen" @keyword
"if" @keyword
"impl" @keyword
"in" @keyword
"let" @keyword
"loop" @keyword
"macro_rules!" @keyword
"match" @keyword
"mod" @keyword
"move" @keyword
"pub" @keyword
"raw" @keyword
"ref" @keyword
"return" @keyword
"static" @keyword
"struct" @keyword
"trait" @keyword
"type" @keyword
"union" @keyword
"unsafe" @keyword
"use" @keyword
"where" @keyword
"while" @keyword
"yield" @keyword
(crate) @keyword
(mutable_specifier) @keyword
(use_list (self) @keyword)
(scoped_use_list (self) @keyword)
(scoped_identifier (self) @keyword)
(super) @keyword

(self) @variable.builtin

(char_literal) @string
(string_literal) @string
(raw_string_literal) @string

(boolean_literal) @constant.builtin
(integer_literal) @number
(float_literal) @number

(escape_sequence) @escape

(attribute_item) @attribute
(inner_attribute_item) @attribute

"*" @operator
"&" @operator
"'" @operator
"#;

/// Translate a capture name from the highlights query into a coarse
/// `SyntaxKind`. The grammar uses dotted sub-categories (`type.builtin`,
/// `function.method`, …) — we collapse those into the broader bucket.
///
/// Mapping table (brief-mandated):
/// - `keyword`                                           → `Keyword`
/// - `string`                                            → `String`
/// - `comment`, `comment.documentation`                  → `Comment`
/// - `type`, `type.builtin`                              → `Type`
/// - `number`                                            → `Number`
/// - `punctuation.bracket`, `punctuation.delimiter`,
///   `operator`                                          → `Punct`
/// - `variable`, `function`, `function.method`,
///   `property`                                          → `Identifier`
/// - everything else (`constant`, `constant.builtin`,
///   `constructor`, `escape`, `attribute`, `label`,
///   `function.macro`, `variable.parameter`,
///   `variable.builtin`)                                 → `Default`
fn capture_name_to_kind(name: &str) -> SyntaxKind {
    match name {
        "keyword" => SyntaxKind::Keyword,
        "string" => SyntaxKind::String,
        "comment" | "comment.documentation" => SyntaxKind::Comment,
        "type" | "type.builtin" => SyntaxKind::Type,
        "number" => SyntaxKind::Number,
        "punctuation.bracket" | "punctuation.delimiter" | "operator" => SyntaxKind::Punct,
        "variable" | "function" | "function.method" | "property" => SyntaxKind::Identifier,
        _ => SyntaxKind::Default,
    }
}

pub struct Highlighter {
    parser: Option<Parser>,
    tree: Option<Tree>,
    /// Compiled query — built once at construction so per-frame
    /// `spans_in_range` calls avoid re-parsing the ~180-line .scm.
    query: Option<Query>,
    /// Capture index → SyntaxKind lookup. The query exposes capture
    /// indices as `u32`s; we precompute the kind for each so the hot
    /// path is a single `Vec` index instead of a string match.
    capture_kinds: Vec<SyntaxKind>,
    /// Cached source text. Required by `QueryCursor::captures`'s
    /// `TextProvider` so predicates like `#match?` can evaluate against
    /// the original bytes. Cleared on `parse` re-entry.
    source: String,
    /// Non-Rust languages: spans of the whole document, computed once in
    /// `parse` (background thread). Sorted by start and non-overlapping
    /// (regex matches), so a range query is a binary search instead of a
    /// regex scan of the entire document per rendered line.
    lexical: Vec<HighlightSpan>,
    lang: LanguageId,
    /// Table-driven lexer for every non-Rust language that has a definition.
    doc: Option<crate::syntax::SyntaxDoc>,
}

/// Above this document size the lexical spans are not cached (24 bytes per
/// span would reach hundreds of MB); queries scan a window around the range.
const LEXICAL_CACHE_MAX_BYTES: usize = 16 * 1024 * 1024;
/// Half-width of the window scanned for documents above the cache limit.
const LEXICAL_WINDOW_BYTES: usize = 64 * 1024;

impl Highlighter {
    /// Build a highlighter pre-loaded with the specific language grammar.
    pub fn new(lang: LanguageId) -> Self {
        if lang == LanguageId::Rust {
            let mut parser = Parser::new();
            let language = tree_sitter_rust::LANGUAGE;
            let language_fn: tree_sitter::Language = language.into();
            parser
                .set_language(&language_fn)
                .expect("tree-sitter-rust grammar incompatible with tree-sitter runtime");

            let query = Query::new(&language_fn, RUST_HIGHLIGHTS_QUERY)
                .expect("brouillon Rust highlights query failed to compile");
            let capture_kinds: Vec<SyntaxKind> = query
                .capture_names()
                .iter()
                .map(|name| capture_name_to_kind(name))
                .collect();

            Self {
                parser: Some(parser),
                tree: None,
                query: Some(query),
                capture_kinds,
                source: String::new(),
                lexical: Vec::new(),
                lang,
                doc: None,
            }
        } else {
            let doc = crate::grammars::syntax_index(lang).map(|i| {
                let reg = crate::syntax::registry();
                crate::syntax::SyntaxDoc::new(reg, i, "")
            });
            Self {
                parser: None,
                tree: None,
                query: None,
                capture_kinds: Vec::new(),
                source: String::new(),
                lexical: Vec::new(),
                lang,
                doc,
            }
        }
    }

    /// Language this highlighter was built for.
    pub fn language(&self) -> LanguageId {
        self.lang
    }

    /// Incremental update after a single contiguous edit: `new_source` is the
    /// whole text after replacing `old_len` bytes at `start` with `new_len`
    /// bytes. Only the table-driven lexer is incremental (re-lexes from the
    /// edited line and stops when the line state converges); the Rust
    /// tree-sitter path falls back to a full re-parse.
    pub fn apply_edit(&mut self, new_source: &str, start: usize, old_len: usize, new_len: usize) {
        match self.doc.as_mut() {
            Some(doc) => doc.apply_edit(new_source, start, old_len, new_len),
            None => self.parse(new_source),
        }
    }

    /// `true` when [`Self::apply_edit`] is truly incremental (table-driven
    /// lexer); the Rust tree-sitter path would re-parse everything.
    pub fn is_incremental(&self) -> bool {
        self.doc.is_some()
    }

    /// Update to `new_source` without knowing the edit: the changed span is
    /// found by trimming the common prefix and suffix against the previous
    /// text, then [`Self::apply_edit`] runs on it. Only meaningful when
    /// [`Self::is_incremental`]; otherwise it is a full [`Self::parse`].
    pub fn apply_text(&mut self, new_source: &str) {
        let Some(doc) = self.doc.as_ref() else {
            self.parse(new_source);
            return;
        };
        let old = doc.text();
        let (a, b) = (old.as_bytes(), new_source.as_bytes());
        let max = a.len().min(b.len());
        let mut start = 0;
        while start < max && a[start] == b[start] {
            start += 1;
        }
        while start > 0 && !(old.is_char_boundary(start) && new_source.is_char_boundary(start)) {
            start -= 1;
        }
        let mut suffix = 0;
        while suffix < max - start && a[a.len() - 1 - suffix] == b[b.len() - 1 - suffix] {
            suffix += 1;
        }
        while suffix > 0 && !(old.is_char_boundary(a.len() - suffix) && new_source.is_char_boundary(b.len() - suffix)) {
            suffix -= 1;
        }
        let (old_len, new_len) = (a.len() - start - suffix, b.len() - start - suffix);
        self.apply_edit(new_source, start, old_len, new_len);
    }

    /// Parse a rope. The table-driven lexer reads the (shared) rope in place: no copy of the document;
    /// plain text keeps nothing; the other languages (Rust, tree-sitter) still need one contiguous `String`.
    pub fn parse_rope(&mut self, rope: &crop::Rope) {
        if let Some(doc) = self.doc.as_mut() {
            doc.set_rope(rope.clone());
        } else if self.lang == LanguageId::PlainText {
            self.parse(""); // plain text has no spans and keeps no source
        } else {
            self.parse(&rope.to_string());
        }
    }

    /// [`Self::apply_text`] for a rope: the changed span is found by comparing the old and new text piece by
    /// piece, so a keystroke re-lexes one line and allocates nothing proportional to the document.
    pub fn apply_rope(&mut self, rope: &crop::Rope) {
        match self.doc.as_mut() {
            Some(doc) => doc.apply_rope(rope.clone()),
            None => self.parse_rope(rope),
        }
    }

    /// Build a highlighter pre-loaded with the Rust grammar.
    pub fn new_rust() -> Self {
        Self::new(LanguageId::Rust)
    }

    /// Parse (or re-parse) the full source.
    pub fn parse(&mut self, source: &str) {
        if let Some(doc) = self.doc.as_mut() {
            doc.set_text(source);
            return;
        }
        if self.lang == LanguageId::Rust {
            if let Some(parser) = self.parser.as_mut() {
                self.tree = parser.parse(source, None);
            }
        }
        // The copy of the source is only read by tree-sitter queries (Rust)
        // and by the windowed lexical scan of very large documents. Every
        // other case (plain text, lexical languages up to the cache limit)
        // would keep a useless copy of the whole file in memory.
        let lexical_lang = !matches!(self.lang, LanguageId::Rust | LanguageId::PlainText);
        if self.lang == LanguageId::Rust || (lexical_lang && source.len() > LEXICAL_CACHE_MAX_BYTES) {
            // Replace in-place to reuse the existing allocation when the
            // new source is the same length-class.
            self.source.clear();
            self.source.push_str(source);
        } else {
            self.source = String::new();
        }
        self.lexical = if self.lang != LanguageId::Rust && source.len() <= LEXICAL_CACHE_MAX_BYTES {
            highlight_lexical(source, self.lang, 0..source.len())
        } else {
            Vec::new()
        };
    }

    /// `true` if we have a parsed tree.
    pub fn is_parsed(&self) -> bool {
        if self.lang == LanguageId::Rust {
            self.tree.is_some()
        } else {
            true // Lexical highlights don't rely on parsed tree, considered ready.
        }
    }

    /// Returns highlight spans clipped to `byte_range`.
    ///
    /// Returns an empty `Vec` if no parse tree exists, or if no spans
    /// fall in range. Spans whose byte_range only partially overlaps
    /// `byte_range` are still returned (the caller is responsible for
    /// further clipping if exact-pixel alignment matters).
    ///
    /// Performance: `O(log n)` tree walk to the range entry point via
    /// `QueryCursor::set_byte_range`, then `O(matches in range)` capture
    /// emission. A fresh `QueryCursor` is allocated per call — the
    /// allocation is small (a single C struct + a few growable buffers)
    /// and tree-sitter recommends this pattern over long-lived cursors
    /// because internal state isn't reset on `set_byte_range`.
    pub fn spans_in_range(&self, byte_range: Range<usize>) -> Vec<HighlightSpan> {
        if let Some(doc) = self.doc.as_ref() {
            return doc.spans_in_range(byte_range);
        }
        if self.lang != LanguageId::Rust {
            if self.source.len() <= LEXICAL_CACHE_MAX_BYTES {
                // Same result as a fresh `highlight_lexical` over the whole
                // source: keep the cached spans that intersect the range.
                let first = self.lexical.partition_point(|s| s.byte_range.end <= byte_range.start);
                return self.lexical[first..]
                    .iter()
                    .take_while(|s| s.byte_range.start < byte_range.end)
                    .cloned()
                    .collect();
            }
            // Very large document: scan a window around the range only.
            let src = &self.source;
            let mut lo = byte_range.start.saturating_sub(LEXICAL_WINDOW_BYTES);
            let mut hi = (byte_range.end + LEXICAL_WINDOW_BYTES).min(src.len());
            while lo > 0 && !src.is_char_boundary(lo) {
                lo -= 1;
            }
            while hi < src.len() && !src.is_char_boundary(hi) {
                hi += 1;
            }
            let mut spans = highlight_lexical(&src[lo..hi], self.lang, byte_range.start - lo..byte_range.end - lo);
            for s in &mut spans {
                s.byte_range = s.byte_range.start + lo..s.byte_range.end + lo;
            }
            return spans;
        }
        let Some(tree) = self.tree.as_ref() else {
            return Vec::new();
        };
        let Some(query) = self.query.as_ref() else {
            return Vec::new();
        };
        if byte_range.is_empty() {
            return Vec::new();
        }

        let mut cursor = QueryCursor::new();
        cursor.set_byte_range(byte_range.clone());

        let source_bytes = self.source.as_bytes();
        let mut captures = cursor.captures(query, tree.root_node(), source_bytes);

        // Pre-size based on a rough heuristic: roughly one span per
        // ~12 bytes of viewport (a token-density floor for code).
        // Bounds the realloc count without over-committing memory.
        let mut out: Vec<HighlightSpan> = Vec::with_capacity(byte_range.len() / 12 + 8);

        while let Some((m, capture_idx)) = captures.next() {
            let capture = m.captures[*capture_idx];
            let kind = self
                .capture_kinds
                .get(capture.index as usize)
                .copied()
                .unwrap_or(SyntaxKind::Default);
            let node = capture.node;
            let start = node.start_byte();
            let end = node.end_byte();
            // Filter out captures whose entire node sits outside the
            // requested window. `set_byte_range` already prunes the
            // grammar walk but tree-sitter can still surface ancestor
            // captures that span past the edge — clip defensively.
            if end <= byte_range.start || start >= byte_range.end {
                continue;
            }
            out.push(HighlightSpan {
                byte_range: start..end,
                kind,
            });
        }
        out
    }

    /// Tree-sitter-aware bracket matcher. Uses the parse tree to find
    /// the matching pair, ignoring brackets inside strings/comments.
    /// Falls back to the text scan if the highlighter has no parse tree.
    ///
    /// The `text` parameter MUST match what was parsed (passing a stale
    /// text returns garbage). Callers should pass the live rope text
    /// matching the tree.
    ///
    /// Algorithm:
    /// 1. Read the byte at `cursor_byte`; if not a recognised bracket,
    ///    return `NotOnBracket`.
    /// 2. Locate the smallest tree-sitter node covering that byte. In
    ///    `tree-sitter-rust`, individual brackets surface as anonymous
    ///    leaf nodes whose `kind()` is the literal char (`"("`, `")"`,
    ///    `"["`, `"]"`, `"{"`, `"}"`) — see
    ///    `tree-sitter-rust/src/node-types.json`.
    /// 3. Walk that node's parent's named/anonymous children and pick
    ///    the sibling whose `kind()` is the matching bracket char. That
    ///    sibling's start byte is the match position.
    /// 4. If the cursor sits inside a `string_literal` / `raw_string_literal`
    ///    / `char_literal` / `line_comment` / `block_comment` node, the
    ///    bracket is NOT a real grammatical bracket — fall back to the
    ///    naive text scan so the user can still navigate inside the
    ///    literal.
    /// 5. Any unexpected tree shape → defer to `matching_bracket_text`.
    pub fn matching_bracket(&self, text: &str, cursor_byte: usize) -> BracketMatch {
        // Cheap gate first: text-mode pre-check catches `NotOnBracket`
        // without descending the tree.
        let bytes = text.as_bytes();
        if cursor_byte >= bytes.len() {
            return BracketMatch::NotOnBracket;
        }
        let c = bytes[cursor_byte];
        if !is_bracket_byte(c) {
            return BracketMatch::NotOnBracket;
        }

        // Without a parse tree we can't be smarter than the text scan.
        let Some(tree) = self.tree.as_ref() else {
            return matching_bracket_text(text, cursor_byte);
        };

        let root = tree.root_node();
        // `descendant_for_byte_range` with start == end pinpoints the
        // smallest node containing that byte.
        let Some(node) = root.descendant_for_byte_range(cursor_byte, cursor_byte + 1) else {
            return matching_bracket_text(text, cursor_byte);
        };

        // If the node lives inside a string/comment, the bracket is not
        // grammatically a bracket — text-scan instead.
        let mut walker = node;
        loop {
            let kind = walker.kind();
            if matches!(
                kind,
                "string_literal"
                    | "raw_string_literal"
                    | "char_literal"
                    | "line_comment"
                    | "block_comment"
            ) {
                return matching_bracket_text(text, cursor_byte);
            }
            match walker.parent() {
                Some(p) => walker = p,
                None => break,
            }
        }

        // For the bracket-on-cursor to be a real grammatical bracket,
        // its node kind should be one of the six literals.
        let node_kind = node.kind();
        let want = match node_kind {
            "(" => ")",
            ")" => "(",
            "[" => "]",
            "]" => "[",
            "{" => "}",
            "}" => "{",
            // The cursor byte was a bracket char but the deepest node
            // covering it isn't a bracket leaf (e.g. inside a token
            // tree macro body that doesn't tokenise it). Fall back.
            _ => return matching_bracket_text(text, cursor_byte),
        };

        let Some(parent) = node.parent() else {
            return matching_bracket_text(text, cursor_byte);
        };

        // Scan siblings under the same parent for the matching kind.
        // The parent (e.g. `block`, `arguments`, `array_expression`)
        // owns BOTH brackets, so the matching kind sits among its
        // direct children. We iterate via `child(i)` to also pick up
        // anonymous nodes which `named_children` skips.
        // `child_count` is a usize but `child(i)` takes u32 in tree-sitter
        // 0.26 (anything beyond ~4B children would be a grammar bug).
        let child_count: u32 = parent.child_count() as u32;
        for i in 0..child_count {
            let Some(child) = parent.child(i) else { continue };
            if child.id() == node.id() {
                continue;
            }
            if child.kind() == want {
                return BracketMatch::At(child.start_byte());
            }
        }

        // Parent didn't expose the partner (malformed parse / error
        // recovery). Last resort: naive text scan.
        matching_bracket_text(text, cursor_byte)
    }
}

/// Bracket-matching result. Returns the byte position of the matching
/// bracket, OR a hint that the cursor is on an unmatched bracket.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BracketMatch {
    /// The matching bracket is at this byte position.
    At(usize),
    /// Cursor is on an opener with no close, or a close with no open.
    Unmatched,
    /// Cursor is not on a bracket character.
    NotOnBracket,
}

/// True for any of `(){}[]`.
#[inline]
fn is_bracket_byte(b: u8) -> bool {
    matches!(b, b'(' | b')' | b'[' | b']' | b'{' | b'}')
}

/// Returns the matching close byte for an open bracket, or `None`.
#[inline]
fn matching_close(open: u8) -> Option<u8> {
    match open {
        b'(' => Some(b')'),
        b'[' => Some(b']'),
        b'{' => Some(b'}'),
        _ => None,
    }
}

/// Returns the matching open byte for a close bracket, or `None`.
#[inline]
fn matching_open(close: u8) -> Option<u8> {
    match close {
        b')' => Some(b'('),
        b']' => Some(b'['),
        b'}' => Some(b'{'),
        _ => None,
    }
}

/// Text-based bracket matcher. Walks the text from `cursor_byte` in the
/// appropriate direction, counting nesting. Returns the matching
/// position or a state hint.
///
/// Recognised pairs : (), [], {}. Quote pairs ('' "" ``) NOT included
/// because they're non-directional and need lex context.
///
/// Limitations : naive scan ; counts brackets INSIDE strings and
/// comments. For tree-sitter-aware matching that ignores strings/
/// comments, use `Highlighter::matching_bracket(...)`.
///
/// Complexity : O(n) where n is the distance from `cursor_byte` to the
/// matching bracket (or to the buffer boundary on an unmatched case).
/// Pure byte iteration — bracket chars are ASCII so we sidestep UTF-8
/// decoding entirely.
pub fn matching_bracket_text(text: &str, cursor_byte: usize) -> BracketMatch {
    let bytes = text.as_bytes();
    if cursor_byte >= bytes.len() {
        return BracketMatch::NotOnBracket;
    }
    let c = bytes[cursor_byte];
    if !is_bracket_byte(c) {
        return BracketMatch::NotOnBracket;
    }

    // Forward case: cursor sits on an opener, count forward to find
    // the matching close.
    if let Some(close) = matching_close(c) {
        let open = c;
        let mut depth: usize = 1;
        let mut i = cursor_byte + 1;
        while i < bytes.len() {
            let b = bytes[i];
            if b == open {
                depth += 1;
            } else if b == close {
                depth -= 1;
                if depth == 0 {
                    return BracketMatch::At(i);
                }
            }
            i += 1;
        }
        return BracketMatch::Unmatched;
    }

    // Backward case: cursor sits on a closer, count backward.
    if let Some(open) = matching_open(c) {
        let close = c;
        let mut depth: usize = 1;
        // `cursor_byte` is guaranteed >= 1 if any bracket precedes us;
        // when it's 0 the loop body never runs and we return Unmatched.
        let mut i = cursor_byte;
        while i > 0 {
            i -= 1;
            let b = bytes[i];
            if b == close {
                depth += 1;
            } else if b == open {
                depth -= 1;
                if depth == 0 {
                    return BracketMatch::At(i);
                }
            }
        }
        return BracketMatch::Unmatched;
    }

    // Unreachable: the bracket gate above is exhaustive over the 6
    // chars and both branches handle their halves.
    BracketMatch::NotOnBracket
}

static JS_TOKEN_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?x)
        (?P<comment>//.*|/\*(?s:.*?)\*/)|
        (?P<string>r?\x22[^\x22\\]*(?:\\.[^\x22\\]*)*\x22|r?'[^\'\\]*(?:\\.[^\'\\]*)*'|`[^`\\]*(?:\\.[^`\\]*)*`)|
        (?P<keyword>\b(?:break|case|catch|class|const|continue|debugger|default|delete|do|else|export|extends|finally|for|function|if|import|in|instanceof|new|return|super|switch|this|throw|try|typeof|var|void|while|with|yield|let|await|async)\b)|
        (?P<type>\b(?:number|string|boolean|any|unknown|never|void|true|false|null|undefined|Array|Map|Set|Promise)\b)|
        (?P<number>\b\d+(?:\.\d+)?\b)|
        (?P<punct>[{}()\[\].,:;!&|<>=\+\-/\*%^])
    ").unwrap()
});

static PY_TOKEN_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?x)
        (?P<comment>\#.*)|
        (?P<string>\x22\x22\x22(?s:.*?)\x22\x22\x22|'''(?s:.*?)'''|r?\x22[^\x22\\]*(?:\\.[^\x22\\]*)*\x22|r?'[^\'\\]*(?:\\.[^\'\\]*)*')|
        (?P<keyword>\b(?:False|None|True|and|as|assert|async|await|break|class|continue|def|del|elif|else|except|finally|for|from|global|if|import|in|is|lambda|nonlocal|not|or|pass|raise|return|try|while|with|yield)\b)|
        (?P<type>\b(?:int|float|str|bool|list|dict|set|tuple|object|Option|List|Dict|Union)\b)|
        (?P<number>\b\d+(?:\.\d+)?\b)|
        (?P<punct>[{}()\[\].,:;!&|<>=\+\-/\*%^])
    ").unwrap()
});

static HTML_TOKEN_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?x)
        (?P<comment><!--(?s:.*?)-->)|
        (?P<keyword></?[a-zA-Z0-9:-]+>?)|
        (?P<type>[a-zA-Z0-9:-]+)\s*=|
        (?P<string>\x22[^\x22]*\x22|'[^\']*')
    ").unwrap()
});

static CSS_TOKEN_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?x)
        (?P<comment>/\*(?s:.*?)\*/)|
        (?P<type>\.[a-zA-Z0-9_-]+|\#[a-zA-Z0-9_-]+|\b(?:body|html|div|span|p|a|ul|li|h1|h2|h3|h4|h5|h6|section|header|footer|nav|button|input)\b)|
        (?P<keyword>\b(?:color|background|margin|padding|border|display|position|width|height|font|flex|align|justify|opacity|transition|transform|grid)\b)|
        (?P<number>\b\d+(?:px|em|rem|%|s|ms|deg|pt|vh|vw)?\b)|
        (?P<string>\x22[^\x22]*\x22|'[^\']*')
    ").unwrap()
});

static JSON_TOKEN_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?x)
        (?P<keyword>\x22[^\x22]*\x22)\s*:|
        (?P<string>\x22[^\x22]*\x22)|
        (?P<number>\b\d+(?:\.\d+)?\b)|
        (?P<type>\b(?:true|false|null)\b)
    ").unwrap()
});

static TOML_TOKEN_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?x)
        (?P<comment>\#.*)|
        (?P<type>\[[a-zA-Z0-9._-]+\])|
        (?P<keyword>[a-zA-Z0-9_-]+)\s*=|
        (?P<string>\x22[^\x22]*\x22|'[^\']*')|
        (?P<number>\b\d+(?:\.\d+)?\b)
    ").unwrap()
});

static MD_TOKEN_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?x)
        (?P<type>^\#{1,6}\s+.*$)|
        (?P<string>\*\*.*?\*\*|\*.*?\*|`.*?`)|
        (?P<keyword>\[.*?\]\(.*?\))|
        (?P<comment><!--(?s:.*?)-->)
    ").unwrap()
});

pub fn highlight_lexical(source: &str, lang: LanguageId, byte_range: Range<usize>) -> Vec<HighlightSpan> {
    let re = match lang {
        LanguageId::JavaScript | LanguageId::TypeScript => &*JS_TOKEN_RE,
        LanguageId::Python => &*PY_TOKEN_RE,
        LanguageId::Html => &*HTML_TOKEN_RE,
        LanguageId::Css => &*CSS_TOKEN_RE,
        LanguageId::Json => &*JSON_TOKEN_RE,
        LanguageId::Toml => &*TOML_TOKEN_RE,
        LanguageId::Markdown => &*MD_TOKEN_RE,
        _ => return Vec::new(),
    };

    let mut spans = Vec::new();
    for cap in re.captures_iter(source) {
        let (kind, m) = if let Some(m) = cap.name("comment") {
            (SyntaxKind::Comment, m)
        } else if let Some(m) = cap.name("string") {
            (SyntaxKind::String, m)
        } else if let Some(m) = cap.name("keyword") {
            (SyntaxKind::Keyword, m)
        } else if let Some(m) = cap.name("type") {
            (SyntaxKind::Type, m)
        } else if let Some(m) = cap.name("number") {
            (SyntaxKind::Number, m)
        } else if let Some(m) = cap.name("punct") {
            (SyntaxKind::Punct, m)
        } else {
            continue;
        };

        let start = m.start();
        let end = m.end();

        if end <= byte_range.start || start >= byte_range.end {
            continue;
        }

        spans.push(HighlightSpan {
            byte_range: start..end,
            kind,
        });
    }

    spans
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Smoke test on the day-1 hello-world. Proves end-to-end:
    /// parser loads, query compiles, captures land in the expected
    /// kinds. The exact span count is grammar-dependent so we assert
    /// presence rather than equality.
    #[test]
    fn highlighter_parses_simple_rust() {
        let mut hl = Highlighter::new_rust();
        hl.parse("fn main() {}\n");
        assert!(hl.is_parsed());
        let spans = hl.spans_in_range(0..100);
        assert!(!spans.is_empty(), "expected at least one span");
    }

    /// A freshly-loaded but unparsed buffer must not produce spans —
    /// the renderer relies on this to skip the syntax-paint pass on
    /// the very first frame, before `parse` has run.
    #[test]
    fn empty_buffer_returns_no_spans() {
        let mut hl = Highlighter::new_rust();
        hl.parse("");
        assert!(hl.is_parsed());
        let spans = hl.spans_in_range(0..100);
        assert!(spans.is_empty(), "got {} spans on empty buffer", spans.len());
    }

    /// The `fn` keyword should produce a Keyword span covering bytes
    /// 0..2. The brief mandates this exact contract.
    #[test]
    fn fn_keyword_extracted() {
        let src = "fn main() {}";
        let mut hl = Highlighter::new_rust();
        hl.parse(src);
        let spans = hl.spans_in_range(0..src.len());
        let kw: Vec<&HighlightSpan> = spans
            .iter()
            .filter(|s| s.kind == SyntaxKind::Keyword)
            .collect();
        assert!(
            !kw.is_empty(),
            "expected at least one Keyword span, got {:?}",
            spans
        );
        assert!(
            kw.iter()
                .any(|s| s.byte_range.start == 0 && s.byte_range.end == 2),
            "expected a Keyword span covering 0..2 (fn), got {:?}",
            kw
        );
    }

    /// A `"hello"` literal should produce a String span whose range
    /// covers the surrounding quotes. The exact byte boundaries depend
    /// on the grammar — we only assert at least one String span exists
    /// inside the literal's region.
    #[test]
    fn string_literal_extracted() {
        let src = r#"fn main() { let _x = "hello"; }"#;
        let mut hl = Highlighter::new_rust();
        hl.parse(src);
        let spans = hl.spans_in_range(0..src.len());
        let strings: Vec<&HighlightSpan> = spans
            .iter()
            .filter(|s| s.kind == SyntaxKind::String)
            .collect();
        assert!(
            !strings.is_empty(),
            "expected at least one String span, got kinds {:?}",
            spans.iter().map(|s| s.kind).collect::<Vec<_>>()
        );
        // The `"hello"` literal starts at byte 21 and ends at byte 28.
        let lit_start = src.find('"').unwrap();
        let lit_end = src.rfind('"').unwrap() + 1;
        assert!(
            strings
                .iter()
                .any(|s| s.byte_range.start <= lit_start && s.byte_range.end >= lit_end),
            "no String span covers the literal at {}..{}: {:?}",
            lit_start,
            lit_end,
            strings
        );
    }

    /// A line comment must surface as a Comment span. Block comments
    /// are exercised implicitly by the canonical query but we only
    /// pin the line-comment case here — block tests can come later if
    /// the mapping ever drifts.
    #[test]
    fn line_comment_extracted() {
        let src = "// hi\nfn main() {}";
        let mut hl = Highlighter::new_rust();
        hl.parse(src);
        let spans = hl.spans_in_range(0..src.len());
        let comments: Vec<&HighlightSpan> = spans
            .iter()
            .filter(|s| s.kind == SyntaxKind::Comment)
            .collect();
        assert!(
            !comments.is_empty(),
            "expected at least one Comment span, got {:?}",
            spans
        );
        assert!(
            comments
                .iter()
                .any(|s| s.byte_range.start == 0 && s.byte_range.end >= 5),
            "Comment span doesn't cover 0..=5 (// hi): {:?}",
            comments
        );
    }

    /// A range strictly past the source must yield no spans. This is
    /// the viewport-scroll-past-EOF case — the renderer asks for spans
    /// in a window that doesn't intersect any real token.
    #[test]
    fn range_clipping_excludes_outside_spans() {
        let src = "fn main() {}";
        let mut hl = Highlighter::new_rust();
        hl.parse(src);
        // Request a window well past EOF.
        let spans = hl.spans_in_range(1000..2000);
        assert!(
            spans.is_empty(),
            "expected zero spans past EOF, got {:?}",
            spans
        );
    }

    /// A range that partially overlaps a token must still surface that
    /// token — the renderer will trim glyph-side. Verifies the
    /// defensive clip in `spans_in_range` doesn't drop partial hits.
    #[test]
    fn range_clipping_includes_partial_overlap() {
        // `fn` occupies bytes 0..2. Ask for bytes 1..10 — the token
        // straddles the start of the window.
        let src = "fn main() {}";
        let mut hl = Highlighter::new_rust();
        hl.parse(src);
        let spans = hl.spans_in_range(1..10);
        assert!(
            spans.iter().any(|s| s.kind == SyntaxKind::Keyword
                && s.byte_range.start == 0
                && s.byte_range.end == 2),
            "expected to see the `fn` Keyword span (straddling start), got {:?}",
            spans
        );
    }

    /// Calling `spans_in_range` before `parse` must return an empty
    /// Vec — the renderer relies on this to skip the syntax pass on
    /// frame zero. NO panics, NO partially-initialized state.
    #[test]
    fn unparsed_highlighter_returns_empty() {
        let hl = Highlighter::new_rust();
        assert!(!hl.is_parsed());
        let spans = hl.spans_in_range(0..1000);
        assert!(
            spans.is_empty(),
            "unparsed highlighter must yield 0 spans, got {}",
            spans.len()
        );
    }

    /// An empty buffer can never have a bracket — the cursor byte is
    /// out of range, so `NotOnBracket` is the right answer (vs. e.g.
    /// `Unmatched`, which would be misleading).
    #[test]
    fn matching_bracket_text_empty_text_returns_not_on_bracket() {
        assert_eq!(matching_bracket_text("", 0), BracketMatch::NotOnBracket);
    }

    /// Forward case: cursor on the open `(` of `()` finds the close
    /// at byte 1.
    #[test]
    fn matching_bracket_text_open_paren_finds_close() {
        let src = "()";
        assert_eq!(matching_bracket_text(src, 0), BracketMatch::At(1));
    }

    /// Backward case: cursor on the close `)` of `()` finds the open
    /// at byte 0.
    #[test]
    fn matching_bracket_text_close_paren_finds_open_backward() {
        let src = "()";
        assert_eq!(matching_bracket_text(src, 1), BracketMatch::At(0));
    }

    /// Nested case: in `(())`, the outer `(` at byte 0 must skip the
    /// inner pair and land on the outer `)` at byte 3. Inner `(` at 1
    /// pairs with inner `)` at 2.
    #[test]
    fn matching_bracket_text_nested_pairs_match_correctly() {
        let src = "(())";
        assert_eq!(matching_bracket_text(src, 0), BracketMatch::At(3));
        assert_eq!(matching_bracket_text(src, 1), BracketMatch::At(2));
        assert_eq!(matching_bracket_text(src, 2), BracketMatch::At(1));
        assert_eq!(matching_bracket_text(src, 3), BracketMatch::At(0));
    }

    /// An open with no close in the rest of the buffer must report
    /// `Unmatched`, not panic.
    #[test]
    fn matching_bracket_text_unmatched_open_returns_unmatched() {
        let src = "(abc";
        assert_eq!(matching_bracket_text(src, 0), BracketMatch::Unmatched);
    }

    /// A stray close with nothing before it must also report
    /// `Unmatched`. Backward walk hits byte 0 without zeroing.
    #[test]
    fn matching_bracket_text_unmatched_close_returns_unmatched() {
        let src = ")abc";
        assert_eq!(matching_bracket_text(src, 0), BracketMatch::Unmatched);
    }

    /// Cursor on an alphanumeric char inside a buffer that DOES contain
    /// brackets must still report `NotOnBracket` — the helper never
    /// "snaps" to a nearby bracket.
    #[test]
    fn matching_bracket_text_cursor_on_non_bracket_returns_not_on_bracket() {
        let src = "(x)";
        assert_eq!(matching_bracket_text(src, 1), BracketMatch::NotOnBracket);
    }

    /// All three pair kinds must be handled identically: `()`, `[]`,
    /// `{}`. Mixed pairs do NOT match (a `(` can't pair with `]`).
    #[test]
    fn matching_bracket_text_handles_three_pair_kinds() {
        assert_eq!(matching_bracket_text("()", 0), BracketMatch::At(1));
        assert_eq!(matching_bracket_text("[]", 0), BracketMatch::At(1));
        assert_eq!(matching_bracket_text("{}", 0), BracketMatch::At(1));
        // Mixed: `(]` is an unmatched open, the `]` is also unmatched
        // (counted as a non-event in the forward scan from `(`).
        assert_eq!(matching_bracket_text("(]", 0), BracketMatch::Unmatched);
        assert_eq!(matching_bracket_text("(]", 1), BracketMatch::Unmatched);
    }

    /// Tree-sitter case: in a parsed `fn main() { let _x = 1; }`, the
    /// cursor on the opening `{` of the body must find the closing
    /// `}`. We also verify it works the other direction and that the
    /// result agrees with what the text scan would produce (which it
    /// must in this brackets-free-of-strings example).
    #[test]
    fn matching_bracket_tree_sitter_in_rust_fn_body() {
        let src = "fn main() { let _x = 1; }";
        let mut hl = Highlighter::new_rust();
        hl.parse(src);

        let open = src.find('{').expect("source contains `{`");
        let close = src.rfind('}').expect("source contains `}`");

        // Forward: `{` → `}`.
        assert_eq!(hl.matching_bracket(src, open), BracketMatch::At(close));
        // Backward: `}` → `{`.
        assert_eq!(hl.matching_bracket(src, close), BracketMatch::At(open));

        // Also the `(` / `)` of the empty arg list.
        let paren_open = src.find('(').unwrap();
        let paren_close = src.find(')').unwrap();
        assert_eq!(
            hl.matching_bracket(src, paren_open),
            BracketMatch::At(paren_close)
        );
        assert_eq!(
            hl.matching_bracket(src, paren_close),
            BracketMatch::At(paren_open)
        );
    }

    /// Calling `matching_bracket` before `parse` must NOT panic and
    /// must defer to the text scan — that's the documented fallback so
    /// the renderer can ask without first checking `is_parsed`.
    #[test]
    fn matching_bracket_tree_sitter_falls_back_when_not_parsed() {
        let hl = Highlighter::new_rust();
        assert!(!hl.is_parsed());
        let src = "fn main() {}";
        let open = src.find('{').unwrap();
        let close = src.rfind('}').unwrap();
        // Text scan should still find the pair.
        assert_eq!(hl.matching_bracket(src, open), BracketMatch::At(close));
        // And a non-bracket cursor returns `NotOnBracket`.
        assert_eq!(
            hl.matching_bracket(src, 0),
            BracketMatch::NotOnBracket // byte 0 is `f`
        );
    }

    #[test]
    fn syntax_kind_variants_are_distinct() {
        let kinds = [
            SyntaxKind::Keyword, SyntaxKind::String, SyntaxKind::Comment,
            SyntaxKind::Type, SyntaxKind::Number, SyntaxKind::Punct,
            SyntaxKind::Default,
        ];
        for i in 0..kinds.len() {
            for j in (i+1)..kinds.len() {
                assert_ne!(kinds[i], kinds[j]);
            }
        }
    }

    #[test]
    fn empty_source_parse_succeeds() {
        let mut hl = Highlighter::new_rust();
        hl.parse("");
        assert!(hl.is_parsed());
        let spans = hl.spans_in_range(0..0);
        assert!(spans.is_empty());
    }

    #[test]
    fn highlight_comment_detection() {
        let mut hl = Highlighter::new_rust();
        let src = "// this is a comment\nfn main() {}";
        hl.parse(src);
        let spans = hl.spans_in_range(0..src.len());
        let has_comment = spans.iter().any(|s| s.kind == SyntaxKind::Comment);
        assert!(has_comment, "Should detect comment in Rust source");
    }

    #[test]
    fn highlight_string_detection() {
        let mut hl = Highlighter::new_rust();
        let src = r#"let s = "hello world";"#;
        hl.parse(src);
        let spans = hl.spans_in_range(0..src.len());
        let has_string = spans.iter().any(|s| s.kind == SyntaxKind::String);
        assert!(has_string, "Should detect string literal");
    }

    #[test]
    fn highlight_number_detection() {
        let mut hl = Highlighter::new_rust();
        let src = "let x = 42;";
        hl.parse(src);
        let spans = hl.spans_in_range(0..src.len());
        let has_number = spans.iter().any(|s| s.kind == SyntaxKind::Number);
        assert!(has_number, "Should detect number literal");
    }

    #[test]
    fn highlight_keyword_detection() {
        let mut hl = Highlighter::new_rust();
        let src = "fn main() { let x = 1; }";
        hl.parse(src);
        let spans = hl.spans_in_range(0..src.len());
        let has_keyword = spans.iter().any(|s| s.kind == SyntaxKind::Keyword);
        assert!(has_keyword, "Should detect 'fn' keyword");
    }

    #[test]
    fn bracket_match_variants_debug() {
        let m = BracketMatch::At(42);
        assert_eq!(format!("{:?}", m), "At(42)");
        let m = BracketMatch::Unmatched;
        assert_eq!(format!("{:?}", m), "Unmatched");
        let m = BracketMatch::NotOnBracket;
        assert_eq!(format!("{:?}", m), "NotOnBracket");
    }

    #[test]
    fn test_lexical_highlighter() {
        // Test JS
        let js_spans = highlight_lexical("const x = 42; // comment", LanguageId::JavaScript, 0..100);
        assert!(js_spans.iter().any(|s| s.kind == SyntaxKind::Keyword && s.byte_range == (0..5))); // "const"
        assert!(js_spans.iter().any(|s| s.kind == SyntaxKind::Number && s.byte_range == (10..12))); // "42"
        assert!(js_spans.iter().any(|s| s.kind == SyntaxKind::Comment && s.byte_range == (14..24))); // "// comment"

        // Test Python
        let py_spans = highlight_lexical("def func(): # py\n    return True", LanguageId::Python, 0..100);
        assert!(py_spans.iter().any(|s| s.kind == SyntaxKind::Keyword && s.byte_range == (0..3))); // "def"
        assert!(py_spans.iter().any(|s| s.kind == SyntaxKind::Comment && s.byte_range == (12..16))); // "# py"

        // Test HTML
        let html_spans = highlight_lexical("<html><body class=\"red\"></body></html>", LanguageId::Html, 0..100);
        assert!(html_spans.iter().any(|s| s.kind == SyntaxKind::Keyword && s.byte_range == (0..6))); // "<html>"
        assert!(html_spans.iter().any(|s| s.kind == SyntaxKind::Type && s.byte_range == (12..17))); // "class"
        assert!(html_spans.iter().any(|s| s.kind == SyntaxKind::String && s.byte_range == (18..23))); // "\"red\""

        // Test CSS
        let css_spans = highlight_lexical(".red { color: #ff0000; }", LanguageId::Css, 0..100);
        assert!(css_spans.iter().any(|s| s.kind == SyntaxKind::Type && s.byte_range == (0..4))); // ".red"
        assert!(css_spans.iter().any(|s| s.kind == SyntaxKind::Keyword && s.byte_range == (7..12))); // "color"
    }

    /// The cached lexical spans must answer every range query exactly like a
    /// fresh whole-document scan (the behaviour before the cache existed).
    #[test]
    fn lexical_cache_matches_full_scan() {
        let src = "{\n  \"name\": \"brouillon\", \"n\": 42,\n  \"list\": [true, false, null, 3.14],\n  \"é\": \"ü\"\n}\n".repeat(20);
        let mut hl = Highlighter::new(LanguageId::Json);
        // Json is table-driven since the syntax lot: drop that path so the legacy
        // lexical cache (still used when no definition exists) is what is tested.
        hl.doc = None;
        hl.parse(&src);
        let key = |v: &[HighlightSpan]| v.iter().map(|s| (s.byte_range.clone(), s.kind)).collect::<Vec<_>>();
        for (start, end) in [(0, 0), (0, 10), (5, 5), (40, 120), (100, 101), (src.len() - 7, src.len()), (0, src.len()), (src.len() + 5, src.len() + 9)] {
            let expected = highlight_lexical(&src, LanguageId::Json, start..end);
            assert_eq!(key(&hl.spans_in_range(start..end)), key(&expected), "range {start}..{end}");
        }
    }

    /// `apply_text` (prefix/suffix diff + incremental lexer) must give the
    /// same spans as a fresh parse, including multi-byte text and block comments.
    #[test]
    fn apply_text_matches_fresh_parse() {
        let steps = [
            "int main() {\n  return 0; /* c */\n}\n",
            "int main() {\n  return 0; /* c\n}\n",
            "int main() {\n  r\u{e9}turn 0; /* c\n}\n",
            "int main() {\n  r\u{e9}turn 0; /* c */\n}\n// \u{1F600}\n",
            "",
        ];
        let mut inc = Highlighter::new(crate::grammars::detect_language(std::path::Path::new("x.c")));
        assert!(inc.is_incremental());
        inc.parse(steps[0]);
        for text in &steps[1..] {
            inc.apply_text(text);
            let mut fresh = Highlighter::new(crate::grammars::detect_language(std::path::Path::new("x.c")));
            fresh.parse(text);
            let key = |v: Vec<HighlightSpan>| v.into_iter().map(|s| (s.byte_range, s.kind)).collect::<Vec<_>>();
            assert_eq!(key(inc.spans_in_range(0..text.len())), key(fresh.spans_in_range(0..text.len())), "text {text:?}");
        }
    }
}
