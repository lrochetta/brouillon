//! Markdown to styled text renderer for the live preview pane.
//!
//! Converts Markdown source to a flat list of styled segments (text + style).
//! No HTML rendering — this is a terminal/editor-style renderer that produces
//! styled text spans for Floem's text rendering pipeline.
//!
//! Supported elements: headings, bold, italic, code, code blocks, links,
//! lists (ordered/unordered), blockquotes, horizontal rules.

/// Style attributes for a text segment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MdStyle {
    pub bold: bool,
    pub italic: bool,
    pub code: bool,
    pub heading_level: u8, // 0 = not a heading, 1-6 = h1-h6
    pub blockquote: bool,
    pub link_url: Option<String>,
    pub list_indent: u8,
}

impl MdStyle {
    pub fn plain() -> Self {
        Self {
            bold: false,
            italic: false,
            code: false,
            heading_level: 0,
            blockquote: false,
            link_url: None,
            list_indent: 0,
        }
    }

    pub fn heading(level: u8) -> Self {
        Self {
            bold: true,
            heading_level: level.min(6),
            ..Self::plain()
        }
    }

    pub fn bold() -> Self {
        Self { bold: true, ..Self::plain() }
    }

    pub fn italic() -> Self {
        Self { italic: true, ..Self::plain() }
    }

    pub fn code() -> Self {
        Self { code: true, ..Self::plain() }
    }

    pub fn blockquote() -> Self {
        Self { blockquote: true, ..Self::plain() }
    }
}

/// A rendered segment of markdown text.
#[derive(Debug, Clone)]
pub struct MdSegment {
    pub text: String,
    pub style: MdStyle,
}

/// Render markdown text to a list of styled segments.
///
/// Each segment represents a contiguous run of text with the same style.
/// Newlines are preserved as-is in the segments.
pub fn render_markdown(source: &str) -> Vec<MdSegment> {
    let mut segments = Vec::new();
    let mut in_code_block = false;

    for line in source.lines() {
        if line.starts_with("```") {
            in_code_block = !in_code_block;
            if in_code_block {
                // Don't add the opening fence line content
            }
            continue;
        }

        if in_code_block {
            segments.push(MdSegment {
                text: format!("{}\n", line),
                style: MdStyle::code(),
            });
            continue;
        }

        // Heading detection
        if let Some(level) = detect_heading(line) {
            let text = line.trim_start_matches('#').trim_start();
            segments.push(MdSegment {
                text: format!("{}\n", text),
                style: MdStyle::heading(level),
            });
            continue;
        }

        // Horizontal rule
        if is_horizontal_rule(line) {
            segments.push(MdSegment {
                text: "────────────────────────────────\n".to_string(),
                style: MdStyle::plain(),
            });
            continue;
        }

        // Blockquote
        if line.starts_with("> ") || line == ">" {
            let text = if line.len() > 2 { &line[2..] } else { "" };
            segments.push(MdSegment {
                text: format!("│ {}\n", text),
                style: MdStyle::blockquote(),
            });
            continue;
        }

        // List items
        if let Some((indent, bullet_text)) = detect_list_item(line) {
            let prefix = "  ".repeat(indent as usize);
            segments.push(MdSegment {
                text: format!("{}• {}\n", prefix, bullet_text),
                style: MdStyle { list_indent: indent + 1, ..MdStyle::plain() },
            });
            continue;
        }

        // Regular paragraph — parse inline styles
        let inline_segments = parse_inline(line);
        segments.extend(inline_segments);
        segments.push(MdSegment {
            text: "\n".to_string(),
            style: MdStyle::plain(),
        });
    }

    segments
}

/// Detect heading level from a line. Returns None if not a heading.
fn detect_heading(line: &str) -> Option<u8> {
    let trimmed = line.trim_start();
    if trimmed.starts_with('#') {
        let level = trimmed.chars().take_while(|c| *c == '#').count();
        if level <= 6 && trimmed.len() > level && trimmed.as_bytes()[level] == b' ' {
            return Some(level as u8);
        }
    }
    None
}

/// Check if a line is a horizontal rule (---, ***, ___).
fn is_horizontal_rule(line: &str) -> bool {
    let trimmed = line.trim();
    if trimmed.len() < 3 {
        return false;
    }
    let first = trimmed.chars().next().unwrap();
    (first == '-' || first == '*' || first == '_')
        && trimmed.chars().all(|c| c == first || c == ' ')
        && trimmed.chars().filter(|c| *c == first).count() >= 3
}

/// Detect a list item. Returns (indent_level, text) if it's a list item.
fn detect_list_item(line: &str) -> Option<(u8, &str)> {
    let indent = line.chars().take_while(|c| *c == ' ' || *c == '\t').count();
    let indent_level = (indent / 2) as u8;
    let rest = line.trim_start();

    // Unordered: - item, * item, + item
    if rest.starts_with("- ") || rest.starts_with("* ") || rest.starts_with("+ ") {
        return Some((indent_level, &rest[2..]));
    }

    // Ordered: 1. item, 2. item
    let num_end = rest.chars().take_while(|c| c.is_ascii_digit()).count();
    if num_end > 0 && rest.len() > num_end + 1 && &rest[num_end..num_end + 2] == ". " {
        return Some((indent_level, &rest[num_end + 2..]));
    }

    None
}

/// Parse inline markdown formatting (bold, italic, code, links).
fn parse_inline(text: &str) -> Vec<MdSegment> {
    let mut segments = Vec::new();
    let chars: Vec<char> = text.chars().collect();
    let mut i = 0;
    let mut current_text = String::new();

    while i < chars.len() {
        // Inline code: `code`
        if chars[i] == '`' {
            if !current_text.is_empty() {
                segments.push(MdSegment { text: current_text.clone(), style: MdStyle::plain() });
                current_text.clear();
            }
            i += 1;
            let mut code = String::new();
            while i < chars.len() && chars[i] != '`' {
                code.push(chars[i]);
                i += 1;
            }
            if i < chars.len() { i += 1; } // skip closing `
            segments.push(MdSegment { text: code, style: MdStyle::code() });
            continue;
        }

        // Bold: **text** or __text__
        if i + 1 < chars.len() && ((chars[i] == '*' && chars[i + 1] == '*') || (chars[i] == '_' && chars[i + 1] == '_')) {
            let delim = chars[i];
            if !current_text.is_empty() {
                segments.push(MdSegment { text: current_text.clone(), style: MdStyle::plain() });
                current_text.clear();
            }
            i += 2;
            let mut bold_text = String::new();
            while i + 1 < chars.len() && !(chars[i] == delim && chars[i + 1] == delim) {
                bold_text.push(chars[i]);
                i += 1;
            }
            if i + 1 < chars.len() { i += 2; } // skip closing **
            segments.push(MdSegment { text: bold_text, style: MdStyle::bold() });
            continue;
        }

        // Italic: *text* or _text_ (single)
        if (chars[i] == '*' || chars[i] == '_') && (i + 1 < chars.len() && chars[i + 1] != chars[i]) {
            let delim = chars[i];
            if !current_text.is_empty() {
                segments.push(MdSegment { text: current_text.clone(), style: MdStyle::plain() });
                current_text.clear();
            }
            i += 1;
            let mut italic_text = String::new();
            while i < chars.len() && chars[i] != delim {
                italic_text.push(chars[i]);
                i += 1;
            }
            if i < chars.len() { i += 1; } // skip closing *
            segments.push(MdSegment { text: italic_text, style: MdStyle::italic() });
            continue;
        }

        // Link: [text](url)
        if chars[i] == '[' {
            if let Some((link_text, url, consumed)) = parse_link(&chars[i..]) {
                if !current_text.is_empty() {
                    segments.push(MdSegment { text: current_text.clone(), style: MdStyle::plain() });
                    current_text.clear();
                }
                segments.push(MdSegment {
                    text: link_text,
                    style: MdStyle { link_url: Some(url), ..MdStyle::plain() },
                });
                i += consumed;
                continue;
            }
        }

        current_text.push(chars[i]);
        i += 1;
    }

    if !current_text.is_empty() {
        segments.push(MdSegment { text: current_text, style: MdStyle::plain() });
    }

    segments
}

/// Parse a markdown link `[text](url)`. Returns (text, url, chars_consumed).
fn parse_link(chars: &[char]) -> Option<(String, String, usize)> {
    if chars.is_empty() || chars[0] != '[' { return None; }

    let mut i = 1;
    let mut text = String::new();
    while i < chars.len() && chars[i] != ']' {
        text.push(chars[i]);
        i += 1;
    }
    if i >= chars.len() { return None; }
    i += 1; // skip ]

    if i >= chars.len() || chars[i] != '(' { return None; }
    i += 1; // skip (

    let mut url = String::new();
    while i < chars.len() && chars[i] != ')' {
        url.push(chars[i]);
        i += 1;
    }
    if i >= chars.len() { return None; }
    i += 1; // skip )

    Some((text, url, i))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn render_heading() {
        let segs = render_markdown("# Hello World");
        assert_eq!(segs.len(), 1);
        assert!(segs[0].style.bold);
        assert_eq!(segs[0].style.heading_level, 1);
        assert!(segs[0].text.contains("Hello World"));
    }

    #[test]
    fn render_heading_levels() {
        for level in 1..=6 {
            let prefix = "#".repeat(level);
            let md = format!("{} Title", prefix);
            let segs = render_markdown(&md);
            assert_eq!(segs[0].style.heading_level, level as u8);
        }
    }

    #[test]
    fn render_bold() {
        let segs = render_markdown("Hello **bold** world");
        assert!(segs.iter().any(|s| s.text == "bold" && s.style.bold));
    }

    #[test]
    fn render_italic() {
        let segs = render_markdown("Hello *italic* world");
        assert!(segs.iter().any(|s| s.text == "italic" && s.style.italic));
    }

    #[test]
    fn render_inline_code() {
        let segs = render_markdown("Use `code` here");
        assert!(segs.iter().any(|s| s.text == "code" && s.style.code));
    }

    #[test]
    fn render_code_block() {
        let md = "```rust\nfn main() {}\n```";
        let segs = render_markdown(md);
        assert!(segs.iter().any(|s| s.style.code && s.text.contains("fn main")));
    }

    #[test]
    fn render_link() {
        let segs = render_markdown("Click [here](https://example.com) now");
        let link_seg = segs.iter().find(|s| s.style.link_url.is_some());
        assert!(link_seg.is_some());
        assert_eq!(link_seg.unwrap().text, "here");
        assert_eq!(link_seg.unwrap().style.link_url.as_deref(), Some("https://example.com"));
    }

    #[test]
    fn render_unordered_list() {
        let segs = render_markdown("- item one\n- item two");
        assert!(segs.iter().any(|s| s.text.contains("item one") && s.style.list_indent > 0));
    }

    #[test]
    fn render_ordered_list() {
        let segs = render_markdown("1. first\n2. second");
        assert!(segs.iter().any(|s| s.text.contains("first")));
    }

    #[test]
    fn render_blockquote() {
        let segs = render_markdown("> quoted text");
        assert!(segs.iter().any(|s| s.style.blockquote && s.text.contains("quoted text")));
    }

    #[test]
    fn render_horizontal_rule() {
        let segs = render_markdown("---");
        assert!(segs.iter().any(|s| s.text.contains("───")));
    }

    #[test]
    fn render_plain_text() {
        let segs = render_markdown("Just plain text");
        assert_eq!(segs.len(), 2); // text + newline
        assert_eq!(segs[0].text, "Just plain text");
        assert_eq!(segs[0].style, MdStyle::plain());
    }

    #[test]
    fn render_empty() {
        let segs = render_markdown("");
        assert!(segs.is_empty());
    }

    #[test]
    fn detect_heading_levels() {
        assert_eq!(detect_heading("# H1"), Some(1));
        assert_eq!(detect_heading("## H2"), Some(2));
        assert_eq!(detect_heading("### H3"), Some(3));
        assert_eq!(detect_heading("Not a heading"), None);
        assert_eq!(detect_heading("#NoSpace"), None);
    }

    #[test]
    fn horizontal_rule_variants() {
        assert!(is_horizontal_rule("---"));
        assert!(is_horizontal_rule("***"));
        assert!(is_horizontal_rule("___"));
        assert!(is_horizontal_rule("- - - -"));
        assert!(!is_horizontal_rule("--"));
        assert!(!is_horizontal_rule("abc"));
    }

    #[test]
    fn list_item_detection() {
        assert!(detect_list_item("- item").is_some());
        assert!(detect_list_item("* item").is_some());
        assert!(detect_list_item("+ item").is_some());
        assert!(detect_list_item("1. item").is_some());
        assert!(detect_list_item("  - nested").is_some());
        assert!(detect_list_item("not a list").is_none());
    }
}
