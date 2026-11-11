//! BK-036 text transforms (base64, MD5, SHA-256 included; SHA-1 is not provided: TODO).
//! Acts on the selection, or on the whole buffer when nothing is selected.
//! Invalid input yields a message and no edit; nothing here panics.

use super::*;

pub fn commands() -> Vec<CommandSpec> {
    [
        ("rot13", "Transform: ROT13"),
        ("url_encode", "Transform: URL Encode"),
        ("url_decode", "Transform: URL Decode"),
        ("html_encode", "Transform: HTML Entities Encode"),
        ("html_decode", "Transform: HTML Entities Decode"),
        ("base64_encode", "Transform: Base64 Encode"),
        ("base64_decode", "Transform: Base64 Decode"),
        ("hash_md5", "Transform: MD5 Hash (hex)"),
        ("hash_sha256", "Transform: SHA-256 Hash (hex)"),
        ("json_beautify", "Transform: JSON Beautify"),
        ("json_minify", "Transform: JSON Minify"),
        ("strip_tags", "Transform: Strip Tags"),
        ("count_text", "Transform: Count Characters, Words, Lines"),
        ("sum_numbers", "Transform: Sum Numbers"),
        ("insert_datetime", "Transform: Insert Date/Time"),
    ]
    .iter()
    .map(|(id, cap)| spec(id, cap, "Transform"))
    .collect()
}

pub fn run(id: &str, ctx: &CmdCtx) -> Option<CmdOutcome> {
    let t = ctx.text;
    if id == "insert_datetime" {
        let secs = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs() as i64);
        let s = format_datetime(ctx.args.first().map_or("%Y-%m-%d %H:%M:%S", String::as_str), secs);
        let sel: &[Region] = if ctx.selection.is_empty() { &[Region { anchor: 0, head: 0 }] } else { ctx.selection };
        return Some(outcome(t, sel.iter().map(|r| TextEdit::new(r.start(), r.end(), s.as_str())).collect()));
    }
    let f: fn(&str) -> Result<String, String> = match id {
        "rot13" => |s| Ok(rot13(s)),
        "url_encode" => |s| Ok(url_encode(s)),
        "url_decode" => url_decode,
        "html_encode" => |s| Ok(html_encode(s)),
        "html_decode" => |s| Ok(html_decode(s)),
        "base64_encode" => |s| Ok(base64_encode(s)),
        "base64_decode" => base64_decode,
        "hash_md5" => |s| Ok(hash_md5(s)),
        "hash_sha256" => |s| Ok(hash_sha256(s)),
        "json_beautify" => json_beautify,
        "json_minify" => json_minify,
        "strip_tags" => |s| Ok(strip_tags(s)),
        "count_text" | "sum_numbers" => return Some(report(id, t, ctx.selection)),
        _ => return None,
    };
    let ranges: Vec<(usize, usize)> = if ctx.selection.iter().all(|r| r.is_caret()) { vec![(0, t.len())] } else { ctx.selection.iter().filter(|r| !r.is_caret()).map(|r| (r.start(), r.end())).collect() };
    let mut edits = Vec::new();
    for (s, e) in ranges {
        match f(&t[s..e]) {
            Ok(n) => edits.push(TextEdit::new(s, e, n)),
            Err(m) => return Some(message(m)),
        }
    }
    Some(outcome(t, edits))
}

fn report(id: &str, t: &str, sel: &[Region]) -> CmdOutcome {
    let text: String = if sel.iter().all(|r| r.is_caret()) { t.to_string() } else { sel.iter().map(|r| &t[r.start()..r.end()]).collect::<Vec<_>>().join("\n") };
    if id == "count_text" {
        let lines = if text.is_empty() { 0 } else { text.lines().count() };
        message(format!("{} characters, {} words, {} lines", text.chars().count(), text.split_whitespace().count(), lines))
    } else {
        let nums: Vec<f64> = text.split_whitespace().filter_map(|w| w.trim_matches(|c: char| c == ',' || c == ';').parse::<f64>().ok()).collect();
        message(format!("Sum of {} numbers: {}", nums.len(), nums.iter().sum::<f64>()))
    }
}

pub fn rot13(s: &str) -> String {
    s.chars()
        .map(|c| match c {
            'a'..='z' => (((c as u8 - b'a') + 13) % 26 + b'a') as char,
            'A'..='Z' => (((c as u8 - b'A') + 13) % 26 + b'A') as char,
            _ => c,
        })
        .collect()
}

pub fn base64_encode(s: &str) -> String {
    use base64::Engine;
    base64::engine::general_purpose::STANDARD.encode(s.as_bytes())
}

pub fn base64_decode(s: &str) -> Result<String, String> {
    use base64::Engine;
    let compact: String = s.chars().filter(|c| !c.is_whitespace()).collect();
    let bytes = base64::engine::general_purpose::STANDARD.decode(compact.as_bytes()).map_err(|e| format!("Invalid Base64: {e}"))?;
    String::from_utf8(bytes).map_err(|_| "Decoded bytes are not valid UTF-8".to_string())
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

pub fn hash_md5(s: &str) -> String {
    use md5::{Digest, Md5};
    hex(&Md5::digest(s.as_bytes()))
}

pub fn hash_sha256(s: &str) -> String {
    use sha2::{Digest, Sha256};
    hex(&Sha256::digest(s.as_bytes()))
}

pub fn url_encode(s: &str) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~') {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

pub fn url_decode(s: &str) -> Result<String, String> {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' {
            let hex = s.get(i + 1..i + 3).and_then(|h| u8::from_str_radix(h, 16).ok()).ok_or_else(|| format!("Invalid percent escape at byte {i}"))?;
            out.push(hex);
            i += 3;
        } else {
            out.push(b[i]);
            i += 1;
        }
    }
    String::from_utf8(out).map_err(|_| "Decoded bytes are not valid UTF-8".to_string())
}

const ENTITIES: &[(&str, &str)] = &[("&amp;", "&"), ("&lt;", "<"), ("&gt;", ">"), ("&quot;", "\""), ("&#39;", "'"), ("&apos;", "'"), ("&nbsp;", "\u{a0}")];

pub fn html_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            _ => out.push(c),
        }
    }
    out
}

pub fn html_decode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(p) = rest.find('&') {
        out.push_str(&rest[..p]);
        rest = &rest[p..];
        if let Some((ent, rep)) = ENTITIES.iter().find(|(e, _)| rest.starts_with(e)) {
            out.push_str(rep);
            rest = &rest[ent.len()..];
        } else if let Some(n) = rest.strip_prefix("&#").and_then(|r| r.find(';').map(|e| (&r[..e], e))).and_then(|(d, e)| {
            let v = d.strip_prefix(['x', 'X']).map_or_else(|| d.parse::<u32>().ok(), |h| u32::from_str_radix(h, 16).ok())?;
            Some((char::from_u32(v)?, e + 3))
        }) {
            out.push(n.0);
            rest = &rest[n.1..];
        } else {
            out.push('&');
            rest = &rest[1..];
        }
    }
    out.push_str(rest);
    out
}

pub fn strip_tags(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut depth = false;
    for c in s.chars() {
        match c {
            '<' => depth = true,
            '>' if depth => depth = false,
            _ if !depth => out.push(c),
            _ => {}
        }
    }
    out
}

/// Key order is preserved (token-level formatter; validity checked with serde_json).
pub fn json_beautify(s: &str) -> Result<String, String> {
    serde_json::from_str::<serde_json::Value>(s).map_err(|e| format!("Invalid JSON: {e}"))?;
    let mut out = String::new();
    let (mut depth, mut in_str, mut esc) = (0usize, false, false);
    let chars: Vec<char> = s.chars().collect();
    let nl = |out: &mut String, d: usize| {
        out.push('\n');
        out.extend(std::iter::repeat_n(' ', d * 2));
    };
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if in_str {
            out.push(c);
            if esc { esc = false } else if c == '\\' { esc = true } else if c == '"' { in_str = false }
        } else if c == '"' {
            in_str = true;
            out.push(c);
        } else if c == '{' || c == '[' {
            out.push(c);
            let close = if c == '{' { '}' } else { ']' };
            let next = chars[i + 1..].iter().position(|x| !x.is_whitespace()).map(|p| i + 1 + p);
            if next.is_some_and(|n| chars[n] == close) {
                out.push(close);
                i = next.unwrap();
            } else {
                depth += 1;
                nl(&mut out, depth);
            }
        } else if c == '}' || c == ']' {
            depth = depth.saturating_sub(1);
            nl(&mut out, depth);
            out.push(c);
        } else if c == ',' {
            out.push(c);
            nl(&mut out, depth);
        } else if c == ':' {
            out.push_str(": ");
        } else if !c.is_whitespace() {
            out.push(c);
        }
        i += 1;
    }
    Ok(out)
}

pub fn json_minify(s: &str) -> Result<String, String> {
    serde_json::from_str::<serde_json::Value>(s).map_err(|e| format!("Invalid JSON: {e}"))?;
    let (mut out, mut in_str, mut esc) = (String::new(), false, false);
    for c in s.chars() {
        if in_str {
            out.push(c);
            if esc { esc = false } else if c == '\\' { esc = true } else if c == '"' { in_str = false }
        } else if c == '"' {
            in_str = true;
            out.push(c);
        } else if !c.is_whitespace() {
            out.push(c);
        }
    }
    Ok(out)
}

/// UTC formatting with `%Y %m %d %H %M %S %%`.
pub fn format_datetime(pattern: &str, unix_secs: i64) -> String {
    let (days, rem) = (unix_secs.div_euclid(86_400), unix_secs.rem_euclid(86_400));
    // civil-from-days (proleptic Gregorian)
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    let mut out = String::new();
    let mut it = pattern.chars();
    while let Some(c) = it.next() {
        if c != '%' {
            out.push(c);
            continue;
        }
        match it.next() {
            Some('Y') => out.push_str(&format!("{y:04}")),
            Some('m') => out.push_str(&format!("{m:02}")),
            Some('d') => out.push_str(&format!("{d:02}")),
            Some('H') => out.push_str(&format!("{:02}", rem / 3600)),
            Some('M') => out.push_str(&format!("{:02}", rem % 3600 / 60)),
            Some('S') => out.push_str(&format!("{:02}", rem % 60)),
            Some('%') => out.push('%'),
            Some(k @ ('A' | 'a')) => {
                // 1970-01-01 was a Thursday.
                let name = ["Thursday", "Friday", "Saturday", "Sunday", "Monday", "Tuesday", "Wednesday"][days.rem_euclid(7) as usize];
                out.push_str(if k == 'A' { name } else { &name[..3] });
            }
            Some(k @ ('B' | 'b')) => {
                let name = [
                    "January", "February", "March", "April", "May", "June", "July", "August", "September", "October",
                    "November", "December",
                ][(m - 1) as usize];
                out.push_str(if k == 'B' { name } else { &name[..3] });
            }
            Some(o) => {
                out.push('%');
                out.push(o);
            }
            None => out.push('%'),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::textops::{apply, sel};

    #[test]
    fn round_trips() {
        assert_eq!(rot13(&rot13("Hello, World!")), "Hello, World!");
        assert_eq!(rot13("abc"), "nop");
        let s = "a b&c/é~";
        assert_eq!(url_decode(&url_encode(s)).unwrap(), s);
        assert_eq!(url_encode("a b"), "a%20b");
        let h = "<a href=\"x\">'&'</a>";
        assert_eq!(html_decode(&html_encode(h)), h);
        assert_eq!(html_decode("&#65;&#x42;&bogus;&amp;"), "AB&bogus;&");
        let j = r#"{"b":[1,2,{"x":"a,b"}],"a":{},"c":[]}"#;
        let pretty = json_beautify(j).unwrap();
        assert!(pretty.starts_with("{\n  \"b\": [\n    1,"));
        assert!(pretty.contains("\"a\": {}") && pretty.contains("\"c\": []"));
        assert_eq!(json_minify(&pretty).unwrap(), j);
    }

    #[test]
    fn base64_and_hashes() {
        assert_eq!(base64_encode("héllo"), "aMOpbGxv");
        assert_eq!(base64_decode("aMOp
bGxv").unwrap(), "héllo");
        assert!(base64_decode("***").is_err());
        assert_eq!(hash_md5("abc"), "900150983cd24fb0d6963f7d28e17f72");
        assert_eq!(hash_sha256("abc"), "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad");
        assert_eq!(apply("base64_encode", "xx abc", &sel(3, 6), &[]), "xx YWJj");
    }

    #[test]
    fn invalid_input_gives_message_not_panic() {
        assert!(url_decode("%zz").is_err());
        assert!(url_decode("%f0%28").is_err());
        assert!(url_decode("%").is_err());
        assert!(json_beautify("{oops").is_err());
        let ctx = CmdCtx { text: "{oops", selection: &[], path: None, args: &[] };
        let o = run("json_beautify", &ctx).unwrap();
        assert!(o.edits.is_empty() && o.message.is_some());
    }

    #[test]
    fn commands_on_selection_and_reports() {
        assert_eq!(apply("rot13", "abc abc", &sel(4, 7), &[]), "abc nop");
        assert_eq!(apply("strip_tags", "<b>x</b> y", &sel(0, 0), &[]), "x y");
        let ctx = CmdCtx { text: "1 2.5, x 3", selection: &[], path: None, args: &[] };
        assert_eq!(run("sum_numbers", &ctx).unwrap().message.unwrap(), "Sum of 3 numbers: 6.5");
        let ctx = CmdCtx { text: "a b\nc", selection: &[], path: None, args: &[] };
        assert_eq!(run("count_text", &ctx).unwrap().message.unwrap(), "5 characters, 3 words, 2 lines");
    }

    #[test]
    fn datetime_format() {
        assert_eq!(format_datetime("%Y-%m-%d %H:%M:%S", 0), "1970-01-01 00:00:00");
        assert_eq!(format_datetime("%Y-%m-%d %H:%M:%S %%", 1_709_210_096), "2024-02-29 12:34:56 %");
    }
}
