//! Emmet-style abbreviation expansion for HTML and CSS (a documented subset, written from the
//! public abbreviation syntax; no third-party Emmet code).
//!
//! HTML subset
//!   * `tag`, `#id`, `.class` (several), `tag#id.a.b`; no tag name -> implied (`div`, `li` in
//!     `ul`/`ol`, `tr` in `table`/`tbody`/`thead`, `td` in `tr`, `option` in `select`, `span` in
//!     inline parents such as `p`, `a`, `em`)
//!   * `[a=1 b="x y" disabled]` attributes, `{text}` content
//!   * `>` child, `+` sibling, `^` climb (`^^` climbs twice), `( ... )` groups, `*N` repeat
//!   * `$` item number inside a repeat, `$$` zero-padded (`li.item$$*3` -> `item01`..`item03`)
//!   * built-ins: `a` (href), `img` (src alt), `link` (rel stylesheet + href), `script:src`,
//!     `input`, `btn` (button), `!` / `html:5` (HTML5 skeleton); void tags have no closing tag
//! CSS subset
//!   * `prop[:]value` with the usual short names (`m`, `mt`, `p`, `w`, `h`, `d`, `pos`, `fz`,
//!     `bg`, `bd`, `ta`, ... see `CSS_PROPS`); a bare number means `px` (`m10` -> `margin: 10px;`),
//!     `10p` `%`, `2e` `em`, `1r` `rem`, `-` separates several values (`p10-20` -> `10px 20px`),
//!     a leading `-` is a negative number, `!` suffix adds `!important`, keyword shorts
//!     (`d:n` -> `none`, `pos:a` -> `absolute`, `ta:c` -> `center`, ...)
//! Not supported: filters (`|c`), `lorem`, custom snippets, multiplication of CSS, `@` numbering
//! options, XML mode. An abbreviation that does not parse expands to nothing (the caller then
//! inserts a normal Tab).

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Lang {
    Html,
    Css,
}

impl Lang {
    /// From a file extension (without dot), case-insensitive.
    pub fn from_ext(ext: &str) -> Option<Lang> {
        match ext.to_ascii_lowercase().as_str() {
            "html" | "htm" | "xhtml" | "php" | "vue" | "svelte" | "erb" | "twig" | "jsx" | "tsx" => Some(Lang::Html),
            "css" | "scss" | "sass" | "less" => Some(Lang::Css),
            _ => None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Expansion {
    /// Expanded text; continuation lines are indented with the given unit only (the caller adds
    /// the base indentation of the line).
    pub text: String,
    /// Byte offset of the caret inside `text`.
    pub cursor: usize,
}

const MARK: char = '\u{1}';

fn finish(s: String) -> Expansion {
    match s.find(MARK) {
        Some(i) => Expansion { text: s.replace(MARK, ""), cursor: i },
        None => {
            let n = s.len();
            Expansion { text: s, cursor: n }
        }
    }
}

/// Expand `abbr`; `indent` is one indentation unit ("\t" or spaces).
pub fn expand(abbr: &str, lang: Lang, indent: &str) -> Option<Expansion> {
    let abbr = abbr.trim();
    if abbr.is_empty() {
        return None;
    }
    match lang {
        Lang::Html => expand_html(abbr, indent),
        Lang::Css => expand_css(abbr),
    }
}

/// Find the abbreviation that ends at the end of `line_before_caret` (text of the line up to the
/// caret) and expand it: returns the byte index where the abbreviation starts and the expansion.
pub fn expand_before_caret(line_before_caret: &str, lang: Lang, indent: &str) -> Option<(usize, Expansion)> {
    let start = abbreviation_start(line_before_caret, lang);
    let abbr = &line_before_caret[start..];
    expand(abbr, lang, indent).map(|e| (start, e))
}

fn abbreviation_start(s: &str, lang: Lang) -> usize {
    let mut depth_sq = 0i32;
    let mut depth_br = 0i32;
    let mut start = s.len();
    for (i, c) in s.char_indices().rev() {
        let inside = depth_sq > 0 || depth_br > 0;
        match c {
            ']' if depth_br == 0 => depth_sq += 1,
            '[' if depth_sq > 0 => depth_sq -= 1,
            '}' if depth_sq == 0 => depth_br += 1,
            '{' if depth_br > 0 => depth_br -= 1,
            _ if inside => {}
            c if c.is_alphanumeric() || "#.>+^*$()-_:!@%".contains(c) => {}
            ',' if lang == Lang::Css => break,
            _ => break,
        }
        start = i;
    }
    // A leading '.' or '>' etc. is fine; strip a leading closing paren/'+'/'>' that cannot start one.
    // `<div>p.x`: what follows the `>` of an existing tag is the abbreviation, not `div>p.x`.
    if s[..start].ends_with('<') {
        if let Some(gt) = s[start..].find('>') {
            start += gt + 1;
        }
    }
    let t = &s[start..];
    let trimmed = t.trim_start_matches(['>', '+', '^', '*', ')', '-', ':']);
    start + (t.len() - trimmed.len())
}

// ---------------------------------------------------------------- HTML

#[derive(Debug, Clone)]
enum Node {
    El { name: String, id: Option<String>, classes: Vec<String>, attrs: Vec<(String, String)>, text: Option<String>, count: usize, children: Vec<Node> },
    Group { children: Vec<Node>, count: usize },
}

struct P<'a> {
    c: Vec<char>,
    i: usize,
    _s: &'a str,
}

impl<'a> P<'a> {
    fn peek(&self) -> Option<char> {
        self.c.get(self.i).copied()
    }

    fn take_while(&mut self, f: impl Fn(char) -> bool) -> String {
        let st = self.i;
        while self.peek().is_some_and(&f) {
            self.i += 1;
        }
        self.c[st..self.i].iter().collect()
    }

    fn delimited(&mut self, open: char, close: char) -> Option<String> {
        if self.peek() != Some(open) {
            return None;
        }
        self.i += 1;
        let st = self.i;
        while let Some(c) = self.peek() {
            if c == close {
                let s: String = self.c[st..self.i].iter().collect();
                self.i += 1;
                return Some(s);
            }
            self.i += 1;
        }
        None
    }

    /// Siblings until end / `)` / `^`. Returns (nodes, climbs still to apply by the caller) or None on error.
    fn siblings(&mut self, parent: &str) -> Option<(Vec<Node>, usize)> {
        let mut out = Vec::new();
        loop {
            let mut node = self.item(parent)?;
            loop {
                match self.peek() {
                    Some('>') => {
                        self.i += 1;
                        let pname = match &node {
                            Node::El { name, .. } => name.clone(),
                            Node::Group { .. } => parent.to_string(),
                        };
                        let (kids, climbs) = self.siblings(&pname)?;
                        match &mut node {
                            Node::El { children, .. } | Node::Group { children, .. } => children.extend(kids),
                        }
                        if climbs > 1 {
                            out.push(node);
                            return Some((out, climbs - 1));
                        }
                        if climbs == 1 {
                            out.push(node);
                            if self.peek() == Some('+') {
                                self.i += 1;
                            }
                            if self.peek().is_none() {
                                return Some((out, 0));
                            }
                            node = self.item(parent)?;
                            continue;
                        }
                        break;
                    }
                    _ => break,
                }
            }
            out.push(node);
            match self.peek() {
                Some('+') => {
                    self.i += 1;
                }
                Some('^') => {
                    let n = self.take_while(|c| c == '^').len();
                    return Some((out, n));
                }
                Some(')') | None => return Some((out, 0)),
                _ => return None,
            }
        }
    }

    fn item(&mut self, parent: &str) -> Option<Node> {
        if self.peek() == Some('(') {
            self.i += 1;
            let (children, climbs) = self.siblings(parent)?;
            if climbs != 0 || self.peek() != Some(')') {
                return None;
            }
            self.i += 1;
            let count = self.multiplier()?;
            return Some(Node::Group { children, count });
        }
        let name = self.take_while(|c| c.is_alphanumeric() || c == '-' || c == ':');
        let (mut id, mut classes, mut attrs, mut text) = (None, Vec::new(), Vec::new(), None);
        loop {
            match self.peek() {
                Some('#') => {
                    self.i += 1;
                    let v = self.take_while(|c| c.is_alphanumeric() || "-_$".contains(c));
                    if v.is_empty() {
                        return None;
                    }
                    id = Some(v);
                }
                Some('.') => {
                    self.i += 1;
                    let v = self.take_while(|c| c.is_alphanumeric() || "-_$".contains(c));
                    if v.is_empty() {
                        return None;
                    }
                    classes.push(v);
                }
                Some('[') => {
                    let body = self.delimited('[', ']')?;
                    attrs.extend(parse_attrs(&body)?);
                }
                Some('{') => {
                    text = Some(self.delimited('{', '}')?);
                }
                _ => break,
            }
        }
        let count = self.multiplier()?;
        if name.is_empty() && id.is_none() && classes.is_empty() && attrs.is_empty() && text.is_none() {
            return None;
        }
        let name = if name.is_empty() { implied_name(parent).to_string() } else { name };
        Some(Node::El { name, id, classes, attrs, text, count, children: Vec::new() })
    }

    fn multiplier(&mut self) -> Option<usize> {
        if self.peek() != Some('*') {
            return Some(1);
        }
        self.i += 1;
        let n = self.take_while(|c| c.is_ascii_digit());
        let n: usize = n.parse().ok()?;
        (1..=500).contains(&n).then_some(n)
    }
}

fn parse_attrs(body: &str) -> Option<Vec<(String, String)>> {
    let mut out = Vec::new();
    let c: Vec<char> = body.chars().collect();
    let mut i = 0;
    while i < c.len() {
        while i < c.len() && c[i].is_whitespace() {
            i += 1;
        }
        if i >= c.len() {
            break;
        }
        let st = i;
        while i < c.len() && !c[i].is_whitespace() && c[i] != '=' {
            i += 1;
        }
        let name: String = c[st..i].iter().collect();
        if name.is_empty() {
            return None;
        }
        let mut val = String::new();
        if i < c.len() && c[i] == '=' {
            i += 1;
            if i < c.len() && (c[i] == '"' || c[i] == '\'') {
                let q = c[i];
                i += 1;
                let vs = i;
                while i < c.len() && c[i] != q {
                    i += 1;
                }
                val = c[vs..i].iter().collect();
                i += 1;
            } else {
                let vs = i;
                while i < c.len() && !c[i].is_whitespace() {
                    i += 1;
                }
                val = c[vs..i].iter().collect();
            }
        }
        out.push((name, val));
    }
    Some(out)
}

fn implied_name(parent: &str) -> &'static str {
    match parent {
        "ul" | "ol" => "li",
        "table" | "tbody" | "thead" | "tfoot" => "tr",
        "tr" => "td",
        "select" | "optgroup" => "option",
        "p" | "span" | "a" | "em" | "strong" | "b" | "i" | "label" | "button" => "span",
        _ => "div",
    }
}

const VOID: &[&str] = &["br", "hr", "img", "input", "meta", "link", "area", "base", "col", "embed", "source", "track", "wbr"];

fn number(s: &str, i: usize, total: usize) -> String {
    // `$` runs are item numbers; the run length is the zero padding.
    let _ = total;
    let mut out = String::new();
    let c: Vec<char> = s.chars().collect();
    let mut k = 0;
    while k < c.len() {
        if c[k] == '$' {
            let st = k;
            while k < c.len() && c[k] == '$' {
                k += 1;
            }
            out.push_str(&format!("{:0width$}", i + 1, width = k - st));
        } else {
            out.push(c[k]);
            k += 1;
        }
    }
    out
}

fn render(nodes: &[Node], depth: usize, indent: &str, ctx: (usize, usize), out: &mut Vec<String>) {
    for n in nodes {
        match n {
            Node::Group { children, count } => {
                for i in 0..*count {
                    let c = if *count > 1 { (i, *count) } else { ctx };
                    render(children, depth, indent, c, out);
                }
            }
            Node::El { name, id, classes, attrs, text, count, children } => {
                for i in 0..*count {
                    let c = if *count > 1 { (i, *count) } else { ctx };
                    render_el(name, id, classes, attrs, text, children, depth, indent, c, out);
                }
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn render_el(name: &str, id: &Option<String>, classes: &[String], attrs: &[(String, String)], text: &Option<String>, children: &[Node], depth: usize, indent: &str, ctx: (usize, usize), out: &mut Vec<String>) {
    let num = |s: &str| number(s, ctx.0, ctx.1);
    let name = num(name);
    let pad = indent.repeat(depth);
    let mut attrs_out: Vec<(String, String)> = Vec::new();
    // built-in default attributes (user ones override by name)
    let defaults: &[(&str, &str)] = match name.as_str() {
        "a" => &[("href", "")],
        "img" => &[("src", ""), ("alt", "")],
        "link" => &[("rel", "stylesheet"), ("href", "")],
        "script:src" => &[("src", "")],
        _ => &[],
    };
    let tag = if name == "script:src" { "script".to_string() } else { name.clone() };
    let tag = if tag == "btn" { "button".to_string() } else { tag };
    // Snippet attributes come first (`a.go` -> `<a href="" class="go">`), unless the abbreviation
    // gives the same attribute itself.
    for (k, v) in defaults {
        if !attrs.iter().any(|(n, _)| n == k) {
            attrs_out.push((k.to_string(), v.to_string()));
        }
    }
    if let Some(i) = id {
        attrs_out.push(("id".into(), num(i)));
    }
    if !classes.is_empty() {
        attrs_out.push(("class".into(), classes.iter().map(|c| num(c)).collect::<Vec<_>>().join(" ")));
    }
    for (k, v) in attrs {
        attrs_out.push((num(k), num(v)));
    }
    if tag == "input" && !attrs_out.iter().any(|(n, _)| n == "type") {
        attrs_out.insert(0, ("type".into(), "text".into()));
    }
    let mut open = format!("<{tag}");
    let mut first_empty_done = false;
    for (k, v) in &attrs_out {
        if v.is_empty() && !first_empty_done {
            open.push_str(&format!(" {k}=\"{MARK}\""));
            first_empty_done = true;
        } else {
            open.push_str(&format!(" {k}=\"{v}\""));
        }
    }
    open.push('>');
    if VOID.contains(&tag.as_str()) {
        out.push(format!("{pad}{open}"));
        return;
    }
    let close = format!("</{tag}>");
    let txt = text.as_deref().map(num);
    if children.is_empty() {
        // The caret lands inside the first empty element unless an attribute already claimed it.
        let inner = match txt {
            Some(t) => t,
            None if first_empty_done => String::new(),
            None => MARK.to_string(),
        };
        out.push(format!("{pad}{open}{inner}{close}"));
        return;
    }
    out.push(format!("{pad}{open}{}", txt.unwrap_or_default()));
    render(children, depth + 1, indent, ctx, out);
    out.push(format!("{pad}{close}"));
}

fn expand_html(abbr: &str, indent: &str) -> Option<Expansion> {
    if abbr == "!" || abbr == "html:5" {
        let s = format!(
            "<!DOCTYPE html>\n<html lang=\"en\">\n<head>\n{i}<meta charset=\"UTF-8\">\n{i}<meta name=\"viewport\" content=\"width=device-width, initial-scale=1.0\">\n{i}<title>Document</title>\n</head>\n<body>\n{i}{MARK}\n</body>\n</html>",
            i = indent
        );
        return Some(finish(s));
    }
    let mut p = P { c: abbr.chars().collect(), i: 0, _s: abbr };
    let (nodes, climbs) = p.siblings("")?;
    if climbs != 0 || p.i != p.c.len() {
        return None;
    }
    let mut lines = Vec::new();
    render(&nodes, 0, indent, (0, 1), &mut lines);
    Some(finish(lines.join("\n")))
}

// ---------------------------------------------------------------- CSS

/// (short, property). Order does not matter; the longest short that prefixes the abbreviation wins.
const CSS_PROPS: &[(&str, &str)] = &[
    ("m", "margin"), ("mt", "margin-top"), ("mr", "margin-right"), ("mb", "margin-bottom"), ("ml", "margin-left"),
    ("p", "padding"), ("pt", "padding-top"), ("pr", "padding-right"), ("pb", "padding-bottom"), ("pl", "padding-left"),
    ("w", "width"), ("h", "height"), ("mw", "max-width"), ("mh", "max-height"), ("miw", "min-width"), ("mih", "min-height"),
    ("d", "display"), ("pos", "position"), ("t", "top"), ("r", "right"), ("b", "bottom"), ("l", "left"), ("z", "z-index"),
    ("fl", "float"), ("cl", "clear"), ("ov", "overflow"), ("ovx", "overflow-x"), ("ovy", "overflow-y"), ("op", "opacity"),
    ("fz", "font-size"), ("fw", "font-weight"), ("ff", "font-family"), ("fs", "font-style"), ("lh", "line-height"),
    ("c", "color"), ("bg", "background"), ("bgc", "background-color"), ("bgi", "background-image"),
    ("bd", "border"), ("bdt", "border-top"), ("bdr", "border-right"), ("bdb", "border-bottom"), ("bdl", "border-left"),
    ("bdrs", "border-radius"), ("ta", "text-align"), ("td", "text-decoration"), ("tt", "text-transform"), ("va", "vertical-align"),
    ("cur", "cursor"), ("whs", "white-space"), ("bxsh", "box-shadow"), ("bxz", "box-sizing"), ("jc", "justify-content"),
    ("ai", "align-items"), ("fxd", "flex-direction"), ("fx", "flex"), ("gap", "gap"), ("ls", "letter-spacing"),
];

const UNITLESS: &[&str] = &["font-weight", "z-index", "opacity", "line-height", "flex"];

fn css_keyword(prop: &str, k: &str) -> Option<&'static str> {
    Some(match (prop, k) {
        ("display", "n") => "none",
        ("display", "b") => "block",
        ("display", "i") => "inline",
        ("display", "ib") => "inline-block",
        ("display", "f") => "flex",
        ("display", "g") => "grid",
        ("position", "a") => "absolute",
        ("position", "r") => "relative",
        ("position", "f") => "fixed",
        ("position", "s") => "sticky",
        ("text-align", "c") => "center",
        ("text-align", "l") => "left",
        ("text-align", "r") => "right",
        ("text-align", "j") => "justify",
        ("float", "l") => "left",
        ("float", "r") => "right",
        ("float", "n") => "none",
        ("overflow" | "overflow-x" | "overflow-y", "h") => "hidden",
        ("overflow" | "overflow-x" | "overflow-y", "a") => "auto",
        ("overflow" | "overflow-x" | "overflow-y", "s") => "scroll",
        ("font-weight", "b") => "bold",
        ("font-weight", "n") => "normal",
        ("font-style", "i") => "italic",
        ("text-decoration", "n") => "none",
        ("text-decoration", "u") => "underline",
        ("cursor", "p") => "pointer",
        ("justify-content", "c") => "center",
        ("justify-content", "sb") => "space-between",
        ("align-items", "c") => "center",
        ("box-sizing", "bb") => "border-box",
        ("width" | "height", "a") => "auto",
        ("margin" | "margin-left" | "margin-right", "a") => "auto",
        _ => return None,
    })
}

fn css_value(prop: &str, raw: &str) -> Option<String> {
    if raw.is_empty() {
        return None;
    }
    if let Some(k) = css_keyword(prop, raw) {
        return Some(k.to_string());
    }
    // `#abc` colours and other literals pass through
    if raw.starts_with('#') || raw.chars().all(|c| c.is_alphabetic() || c == '-') && raw.len() > 2 {
        return Some(raw.to_string());
    }
    let mut parts = Vec::new();
    let mut rest = raw;
    let mut neg = false;
    if let Some(r) = rest.strip_prefix('-') {
        neg = true;
        rest = r;
    }
    for (n, tok) in rest.split('-').enumerate() {
        if tok.is_empty() {
            return None;
        }
        let digits_end = tok.find(|c: char| !(c.is_ascii_digit() || c == '.')).unwrap_or(tok.len());
        let (num, unit) = tok.split_at(digits_end);
        if num.is_empty() || num.parse::<f64>().is_err() {
            return None;
        }
        let unit = match unit {
            "" if UNITLESS.contains(&prop) => "",
            "" => if num == "0" { "" } else { "px" },
            "p" => "%",
            "e" => "em",
            "r" => "rem",
            "x" => "px",
            "vw" | "vh" | "pt" | "ex" | "ch" => unit,
            _ => return None,
        };
        parts.push(format!("{}{num}{unit}", if n == 0 && neg { "-" } else { "" }));
    }
    Some(parts.join(" "))
}

fn expand_css(abbr: &str) -> Option<Expansion> {
    let (abbr, important) = match abbr.strip_suffix('!') {
        Some(a) => (a, true),
        None => (abbr, false),
    };
    // property key = longest known short that prefixes the abbreviation and is followed by
    // ':' / a digit / '-' / end.
    let (prop, rest) = CSS_PROPS
        .iter()
        .filter(|(s, _)| abbr.starts_with(s) && abbr[s.len()..].chars().next().is_none_or(|c| c == ':' || c == '-' || c.is_ascii_digit() || c == '.'))
        .max_by_key(|(s, _)| s.len())
        .map(|(s, p)| (*p, &abbr[s.len()..]))?;
    let rest = rest.strip_prefix(':').unwrap_or(rest);
    let imp = if important { " !important" } else { "" };
    if rest.is_empty() {
        return Some(finish(format!("{prop}: {MARK};")));
    }
    let v = css_value(prop, rest)?;
    Some(finish(format!("{prop}: {v}{imp};")))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn h(a: &str) -> String {
        expand(a, Lang::Html, "\t").unwrap().text
    }
    fn c(a: &str) -> String {
        expand(a, Lang::Css, "\t").unwrap().text
    }

    #[test]
    fn plain_tags_ids_classes_attrs_text() {
        assert_eq!(h("div"), "<div></div>");
        assert_eq!(h("p.intro"), "<p class=\"intro\"></p>");
        assert_eq!(h("div#main.a.b"), "<div id=\"main\" class=\"a b\"></div>");
        assert_eq!(h(".box"), "<div class=\"box\"></div>");
        assert_eq!(h("a[href=x.html title=\"Go there\"]{Click}"), "<a href=\"x.html\" title=\"Go there\">Click</a>");
        assert_eq!(h("input[disabled]"), "<input type=\"text\" disabled=\"\">");
        // snippet attributes first, then #id .class [attrs]
        assert_eq!(h("a.go{Hi}"), "<a href=\"\" class=\"go\">Hi</a>");
        assert_eq!(h("img#p.x[alt=pic]"), "<img src=\"\" id=\"p\" class=\"x\" alt=\"pic\">");
    }

    #[test]
    fn nesting_siblings_and_climb() {
        assert_eq!(h("ul>li"), "<ul>\n\t<li></li>\n</ul>");
        assert_eq!(h("div>p+span"), "<div>\n\t<p></p>\n\t<span></span>\n</div>");
        assert_eq!(h("div>p>b^i"), "<div>\n\t<p>\n\t\t<b></b>\n\t</p>\n\t<i></i>\n</div>");
        assert_eq!(h("div>div>b^^i"), "<div>\n\t<div>\n\t\t<b></b>\n\t</div>\n</div>\n<i></i>".replace("</div>\n<i>", "</div>\n<i>"));
    }

    #[test]
    fn repeat_numbering_groups_and_implied_names() {
        assert_eq!(h("ul>li*3"), "<ul>\n\t<li></li>\n\t<li></li>\n\t<li></li>\n</ul>");
        assert_eq!(h("li.item$*2"), "<li class=\"item1\"></li>\n<li class=\"item2\"></li>");
        assert_eq!(h("li.item$$*2"), "<li class=\"item01\"></li>\n<li class=\"item02\"></li>");
        assert_eq!(h("ul>.x"), "<ul>\n\t<li class=\"x\"></li>\n</ul>");
        assert_eq!(h("table>.r>.c"), "<table>\n\t<tr class=\"r\">\n\t\t<td class=\"c\"></td>\n\t</tr>\n</table>");
        assert_eq!(h("ul>li*2>a{Item $}"), "<ul>\n\t<li>\n\t\t<a href=\"\">Item 1</a>\n\t</li>\n\t<li>\n\t\t<a href=\"\">Item 2</a>\n\t</li>\n</ul>");
        assert_eq!(h("(p+p)*2"), "<p></p>\n<p></p>\n<p></p>\n<p></p>");
        assert_eq!(h("div>(h1+p)*2"), "<div>\n\t<h1></h1>\n\t<p></p>\n\t<h1></h1>\n\t<p></p>\n</div>");
        assert_eq!(h("p>.x"), "<p>\n\t<span class=\"x\"></span>\n</p>");
    }

    #[test]
    fn builtins_and_void_tags() {
        assert_eq!(h("img"), "<img src=\"\" alt=\"\">");
        assert_eq!(h("br"), "<br>");
        assert_eq!(h("link"), "<link rel=\"stylesheet\" href=\"\">");
        assert_eq!(h("script:src"), "<script src=\"\"></script>");
        assert_eq!(h("btn.go{OK}"), "<button class=\"go\">OK</button>");
        let skeleton = h("!");
        assert!(skeleton.starts_with("<!DOCTYPE html>\n<html lang=\"en\">") && skeleton.contains("<title>Document</title>") && skeleton.ends_with("</html>"));
        assert_eq!(h("html:5"), skeleton);
    }

    #[test]
    fn cursor_lands_in_first_empty_slot() {
        let e = expand("a", Lang::Html, "\t").unwrap();
        assert_eq!(&e.text[..e.cursor], "<a href=\"");
        let e = expand("div>p", Lang::Html, "\t").unwrap();
        assert_eq!(&e.text[..e.cursor], "<div>\n\t<p>");
        let e = expand("p{hi}", Lang::Html, "\t").unwrap();
        assert_eq!(e.cursor, e.text.len());
        let e = expand("!", Lang::Html, "\t").unwrap();
        assert!(e.text[..e.cursor].ends_with("<body>\n\t"));
    }

    #[test]
    fn invalid_abbreviations_expand_to_nothing() {
        for a in ["", ">", "div>", "div+", "(div", "div)", "div*0", "div*9999", "#", ".", "div[", "div{x"] {
            assert!(expand(a, Lang::Html, "\t").is_none(), "{a:?}");
        }
        assert!(expand("zzz", Lang::Css, "\t").is_none());
        assert!(expand("m1q", Lang::Css, "\t").is_none());
    }

    #[test]
    fn css_properties_values_units() {
        assert_eq!(c("m10"), "margin: 10px;");
        assert_eq!(c("m0"), "margin: 0;");
        assert_eq!(c("p10-20"), "padding: 10px 20px;");
        assert_eq!(c("mt-5"), "margin-top: -5px;");
        assert_eq!(c("w100p"), "width: 100%;");
        assert_eq!(c("h2e"), "height: 2em;");
        assert_eq!(c("fz1.5r"), "font-size: 1.5rem;");
        assert_eq!(c("fw700"), "font-weight: 700;");
        assert_eq!(c("z10"), "z-index: 10;");
        assert_eq!(c("op.5"), "opacity: .5;");
        assert_eq!(c("d:n"), "display: none;");
    }

    #[test]
    fn css_keywords_important_and_cursor() {
        assert_eq!(c("pos:a"), "position: absolute;");
        assert_eq!(c("ta:c"), "text-align: center;");
        assert_eq!(c("d:f"), "display: flex;");
        assert_eq!(c("m10!"), "margin: 10px !important;");
        assert_eq!(c("bg:#fff"), "background: #fff;");
        let e = expand("c", Lang::Css, "\t").unwrap();
        assert_eq!((e.text.as_str(), e.cursor), ("color: ;", 7));
        // longest short wins: `bdrs` is border-radius, not `bd` + `rs`
        assert_eq!(c("bdrs4"), "border-radius: 4px;");
    }

    #[test]
    fn abbreviation_is_found_before_the_caret() {
        let (s, e) = expand_before_caret("    ul>li*2", Lang::Html, "\t").unwrap();
        assert_eq!(s, 4);
        assert!(e.text.starts_with("<ul>"));
        let (s, _) = expand_before_caret("<div>p.x", Lang::Html, "\t").unwrap();
        assert_eq!(s, 5);
        let (s, e) = expand_before_caret("a { m10", Lang::Css, "\t").unwrap();
        assert_eq!((s, e.text.as_str()), (4, "margin: 10px;"));
        let (s, _) = expand_before_caret("x a[href=\"u v\"]{t t}", Lang::Html, "\t").unwrap();
        assert_eq!(s, 2);
        assert!(expand_before_caret("hello world", Lang::Css, "\t").is_none());
        assert!(expand_before_caret("   ", Lang::Html, "\t").is_none());
    }

    #[test]
    fn extension_mapping() {
        assert_eq!(Lang::from_ext("HTML"), Some(Lang::Html));
        assert_eq!(Lang::from_ext("scss"), Some(Lang::Css));
        assert_eq!(Lang::from_ext("rs"), None);
    }
}
