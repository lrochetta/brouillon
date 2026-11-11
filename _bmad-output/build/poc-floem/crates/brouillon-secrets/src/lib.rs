//! Local, offline secret scanner and line-preserving redactor.
//!
//! * No network, no model, no regex crate: small hand-written matchers over bytes, linear time.
//! * [`Scanner::scan`] returns [`Finding`]s (byte spans of the secret VALUE, never the text).
//! * [`mask`] replaces each span by `[REDACTED:<rule>:<len>]` and keeps every line break, so the
//!   masked text has exactly the same number of lines. [`redact_text`] is the variant for logs.
//! * Severity tiers: `High` (private key blocks, known provider token shapes, `://user:pass@`,
//!   JWT, bearer tokens), `Medium` (`password=`-style assignments with a non-placeholder value),
//!   `Info` (high entropy token in an assignment context; informational, not masked by default).
//! * False positives are cut by placeholder detection ([`is_placeholder`]) and by a
//!   "not a secret" fingerprint [`Allowlist`] (salted hash of rule + matched text, never the text).
//! * Size cap: input beyond [`Scanner::max_bytes`] is scanned up to the cap only and
//!   [`Scan::truncated`] is set; callers that must fail closed check it.
//!
//! Honest limits: best effort. Masking is defense in depth, not the only control. Known false
//! negatives: unquoted lowercase-letters-only passwords (indistinguishable from identifiers),
//! secrets split across lines, encoded/encrypted secrets, secrets with no key context.
#![forbid(unsafe_code)]

use sha2::{Digest, Sha256};
use std::collections::HashSet;

/// Default cap on scanned bytes (scanning is linear; see `examples/bench.rs`).
pub const DEFAULT_MAX_BYTES: usize = 4 * 1024 * 1024;
/// Lines longer than this are not scanned by the entropy rule (minified code, blobs).
const LONG_LINE: usize = 2000;
/// A private key block without an END marker is masked at most this far.
const PEM_WINDOW: usize = 64 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Severity {
    Info,
    Medium,
    High,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Finding {
    pub rule: &'static str,
    pub severity: Severity,
    /// Byte span of the secret value in the scanned text.
    pub start: usize,
    pub end: usize,
    /// 1-based line of `start`.
    pub line: usize,
}

#[derive(Clone, Debug, Default)]
pub struct Scan {
    pub findings: Vec<Finding>,
    /// The input was longer than the cap: only the first `scanned_bytes` were looked at.
    pub truncated: bool,
    pub scanned_bytes: usize,
}

impl Scan {
    pub fn count_at_least(&self, min: Severity) -> usize {
        self.findings.iter().filter(|f| f.severity >= min).count()
    }
}

/// "Not a secret" fingerprints: salted SHA-256 of `rule` + matched text. The text is never stored.
#[derive(Clone, Debug, Default)]
pub struct Allowlist {
    salt: String,
    fps: HashSet<String>,
}

impl Allowlist {
    pub fn new(salt: &str) -> Self {
        Self { salt: salt.into(), fps: HashSet::new() }
    }
    pub fn fingerprint(&self, rule: &str, matched: &str) -> String {
        let mut h = Sha256::new();
        h.update(self.salt.as_bytes());
        h.update([0]);
        h.update(rule.as_bytes());
        h.update([0]);
        h.update(matched.as_bytes());
        h.finalize()[..16].iter().map(|b| format!("{b:02x}")).collect()
    }
    /// Remember "this is not a secret"; returns the fingerprint to persist.
    pub fn allow(&mut self, rule: &str, matched: &str) -> String {
        let fp = self.fingerprint(rule, matched);
        self.fps.insert(fp.clone());
        fp
    }
    pub fn allow_fingerprint(&mut self, fp: String) {
        self.fps.insert(fp);
    }
    pub fn contains(&self, rule: &str, matched: &str) -> bool {
        !self.fps.is_empty() && self.fps.contains(&self.fingerprint(rule, matched))
    }
    pub fn fingerprints(&self) -> impl Iterator<Item = &String> {
        self.fps.iter()
    }
}

#[derive(Clone, Debug)]
pub struct Scanner {
    pub max_bytes: usize,
    allow: Allowlist,
    entropy: bool,
}

impl Default for Scanner {
    fn default() -> Self {
        Self::new()
    }
}

/// Scan with the defaults.
pub fn scan(text: &str) -> Scan {
    Scanner::new().scan(text)
}

impl Scanner {
    pub fn new() -> Self {
        Self { max_bytes: DEFAULT_MAX_BYTES, allow: Allowlist::default(), entropy: true }
    }
    pub fn with_max_bytes(mut self, n: usize) -> Self {
        self.max_bytes = n;
        self
    }
    pub fn with_allowlist(mut self, a: Allowlist) -> Self {
        self.allow = a;
        self
    }
    pub fn with_entropy(mut self, on: bool) -> Self {
        self.entropy = on;
        self
    }

    /// Scan a text of any size: beyond `max_bytes` it is cut into line-aligned chunks and every
    /// chunk is scanned (a private key block crossing a chunk border is the only thing that can
    /// be missed). Over 16 x `max_bytes` it is cut off and `truncated` is set: fail closed.
    pub fn scan_all(&self, text: &str) -> Scan {
        let total_cap = self.max_bytes.saturating_mul(16);
        if text.len() <= self.max_bytes || text.len() > total_cap {
            return self.scan(text);
        }
        let (mut findings, mut off, mut line_base) = (Vec::new(), 0usize, 0usize);
        while off < text.len() {
            let mut end = (off + self.max_bytes).min(text.len());
            if end < text.len() {
                match text[off..end].rfind('\n') {
                    Some(i) => end = off + i + 1,
                    None => {
                        while !text.is_char_boundary(end) {
                            end -= 1;
                        }
                    }
                }
            }
            let chunk = &text[off..end];
            for mut f in self.scan(chunk).findings {
                f.start += off;
                f.end += off;
                f.line += line_base;
                findings.push(f);
            }
            line_base += chunk.bytes().filter(|&c| c == b'\n').count();
            off = end;
        }
        Scan { findings, truncated: false, scanned_bytes: text.len() }
    }

    pub fn scan(&self, text: &str) -> Scan {
        let mut limit = text.len();
        let mut truncated = false;
        if limit > self.max_bytes {
            limit = self.max_bytes;
            while !text.is_char_boundary(limit) {
                limit -= 1;
            }
            truncated = true;
        }
        let text = &text[..limit];
        let b = text.as_bytes();
        let mut raw: Vec<Finding> = Vec::new();
        let (mut pos, mut ln) = (0usize, 0usize);
        while pos < b.len() {
            let end = b[pos..].iter().position(|&c| c == b'\n').map_or(b.len(), |i| pos + i);
            ln += 1;
            let line = &text[pos..end];
            if let Some(c) = pem_begin(line) {
                let start = pos + c;
                let span_end = pem_end(text, start);
                raw.push(Finding { rule: "private-key", severity: Severity::High, start, end: span_end, line: ln });
                // Continue after the line holding the end of the block.
                let consumed = &b[pos..span_end];
                ln += consumed.iter().filter(|&&c| c == b'\n').count();
                pos = b[span_end..].iter().position(|&c| c == b'\n').map_or(b.len(), |i| span_end + i + 1);
                continue;
            }
            self.scan_line(line, pos, ln, &mut raw);
            pos = end + 1;
        }
        // "Not a secret" fingerprints are applied to the raw matches, before merging.
        raw.retain(|f| !self.allow.contains(f.rule, &text[f.start..f.end]));
        Scan { findings: merge(raw), truncated, scanned_bytes: limit }
    }

    fn scan_line(&self, line: &str, base: usize, ln: usize, raw: &mut Vec<Finding>) {
        let b = line.as_bytes();
        let n = b.len();
        if n < 6 {
            return;
        }
        let long = n > LONG_LINE;
        let mut add = |rule: &'static str, severity: Severity, s: usize, e: usize| {
            if e > s {
                raw.push(Finding { rule, severity, start: base + s, end: base + e, line: ln });
            }
        };

        // 1. Runs of token characters: provider prefixes, JWT, entropy, `--password value`.
        let mut i = 0;
        while i < n {
            if !is_run(b[i]) {
                i += 1;
                continue;
            }
            let mut j = i;
            while j < n && is_run(b[j]) {
                j += 1;
            }
            let mut e = j;
            while e > i && b[e - 1] == b'.' {
                e -= 1;
            }
            let run = &line[i..e];
            if run.starts_with("--") && has_keyword(run) && j < n && (b[j] == b' ' || b[j] == b'\t') {
                if let Some((vs, ve)) = next_word(line, j) {
                    let v = &line[vs..ve];
                    if !v.starts_with('-') && v.len() >= 4 && !is_placeholder(v) {
                        add("cli-secret-flag", Severity::Medium, vs, ve);
                    }
                }
            }
            if run.len() >= 8 {
                if run.starts_with("eyJ") && run.len() >= 30 && run.split('.').count() == 3 && run.split('.').all(|p| !p.is_empty()) {
                    add("jwt", Severity::High, i, e);
                } else if run.starts_with("SG.") && run.len() >= 60 {
                    add("sendgrid-key", Severity::High, i, e);
                } else {
                    let mut so = i;
                    for seg in run.split('.') {
                        let s0 = so;
                        so += seg.len() + 1;
                        if seg.len() < 8 {
                            continue;
                        }
                        // A provider prefix at the start of the segment or right after a dash
                        // (`evil-ghp_...`, `token-sk-ant-...`).
                        let sb = seg.as_bytes();
                        let hit = std::iter::once(0).chain((1..sb.len()).filter(|&k| sb[k - 1] == b'-')).find_map(|k| prefix_rule(&seg[k..]).map(|r| (k, r)));
                        if let Some((k, rule)) = hit {
                            add(rule, Severity::High, s0 + k, s0 + seg.len());
                        } else if self.entropy && !long && seg.len() >= 20 && entropy_context(b, s0) && looks_random(seg) {
                            add("high-entropy", Severity::Info, s0, s0 + seg.len());
                        }
                    }
                }
            }
            i = j;
        }

        // 2. `scheme://user:password@host`.
        for (p, _) in line.match_indices("://") {
            if p == 0 || !b[p - 1].is_ascii_alphabetic() {
                continue;
            }
            let a0 = p + 3;
            let mut a1 = a0;
            while a1 < n && !matches!(b[a1], b'/' | b'?' | b'#' | b' ' | b'\t' | b'"' | b'\'' | b'<' | b'>' | b'`' | b'\r') {
                a1 += 1;
            }
            let auth = &line[a0..a1];
            if let Some(at) = auth.rfind('@') {
                if let Some(colon) = auth[..at].find(':') {
                    let pw = &auth[colon + 1..at];
                    if !pw.is_empty() && !is_placeholder(pw) && !matches!(pw.as_bytes()[0], b'$' | b'{' | b'%' | b'<' | b'[') {
                        add("url-credentials", Severity::High, a0 + colon + 1, a0 + at);
                    }
                }
            }
        }

        // 3. `Authorization: Bearer <token>` / `Basic <b64>` and `sshpass -p <pw>`.
        if !long {
            if let Some(p) = find_ci(line, "bearer ", 0) {
                let (s, e) = token_span(b, p + 7);
                if e - s >= 20 && !is_placeholder(&line[s..e]) {
                    add("bearer-token", Severity::High, s, e);
                }
            }
            if find_ci(line, "authorization", 0).is_some() {
                if let Some(p) = find_ci(line, "basic ", 0) {
                    let (s, e) = token_span(b, p + 6);
                    if e - s >= 8 && !is_placeholder(&line[s..e]) {
                        add("basic-auth", Severity::High, s, e);
                    }
                }
            }
            if let Some(p) = find_ci(line, "sshpass", 0) {
                let mut from = p + 7;
                while let Some((ws, we)) = next_word(line, from) {
                    if &line[ws..we] == "-p" {
                        if let Some((vs, ve)) = next_word(line, we) {
                            if !is_placeholder(&line[vs..ve]) {
                                add("sshpass", Severity::High, vs, ve);
                            }
                        }
                        break;
                    }
                    from = we;
                }
            }
        }

        // 4. Assignments: `password = "..."`, `"api_key": "..."`, `DB_PASSWORD=...`, `'token' => '...'`.
        let mut k = 0;
        while k < n {
            let c = b[k];
            if c != b'=' && c != b':' && c != b',' {
                k += 1;
                continue;
            }
            let mut after = k + 1;
            if c == b',' {
                // `define('DB_PASSWORD', 'x')`, `set("token", "x")`: only after a quoted key.
                let mut q = k;
                while q > 0 && (b[q - 1] == b' ' || b[q - 1] == b'\t') {
                    q -= 1;
                }
                if q == 0 || !matches!(b[q - 1], b'"' | b'\'' | b'`') {
                    k += 1;
                    continue;
                }
            } else if c == b'=' {
                if after < n && b[after] == b'=' {
                    k += 2;
                    continue;
                }
                if after < n && b[after] == b'>' {
                    after += 1;
                }
            } else if after < n && (b[after] == b':' || b[after] == b'/') {
                k += 2;
                continue;
            } else if after < n && b[after] == b'=' {
                after += 1;
            }
            // Key, walking back over spaces and one closing quote.
            let mut ke = k;
            while ke > 0 && (b[ke - 1] == b' ' || b[ke - 1] == b'\t') {
                ke -= 1;
            }
            if ke > 0 && matches!(b[ke - 1], b'"' | b'\'' | b'`') {
                ke -= 1;
            }
            let mut ks = ke;
            while ks > 0 && ke - ks < 64 && is_run(b[ks - 1]) {
                ks -= 1;
            }
            let key = &line[ks..ke];
            if key.len() >= 3 && has_keyword(key) && !benign_key(key) {
                if let Some((vs, ve)) = assignment_value(line, after) {
                    add("assignment", Severity::Medium, vs, ve);
                }
            }
            k = after.max(k + 1);
        }
    }
}

fn is_run(c: u8) -> bool {
    c.is_ascii_alphanumeric() || c == b'_' || c == b'-' || c == b'.'
}

/// Offset of `-----BEGIN ... PRIVATE KEY` in `line`.
fn pem_begin(line: &str) -> Option<usize> {
    let c = line.find("-----BEGIN ")?;
    let rest = &line[c + 11..];
    let close = rest.find("-----")?;
    if rest[..close].contains("PRIVATE KEY") {
        Some(c)
    } else {
        None
    }
}

/// End (exclusive) of the private key block starting at `start`.
fn pem_end(text: &str, start: usize) -> usize {
    let mut limit = (start + PEM_WINDOW).min(text.len());
    while !text.is_char_boundary(limit) {
        limit -= 1;
    }
    let win = &text[start + 11..limit];
    if let Some(i) = win.find("-----END ") {
        let after = start + 11 + i + 9;
        if let Some(j) = text[after..limit].find("-----") {
            return after + j + 5;
        }
        return limit.min(after + 40);
    }
    // No END marker (truncated paste): mask to the first blank line or the window.
    let body = &text[start..limit];
    let nl = body.find('\n').map_or(body.len(), |x| x + 1);
    let mut p = nl;
    while p < body.len() {
        let e = body[p..].find('\n').map_or(body.len(), |x| p + x);
        if body[p..e].trim().is_empty() {
            return start + p.saturating_sub(1).max(nl.saturating_sub(1));
        }
        p = e + 1;
    }
    limit
}

fn prefix_rule(seg: &str) -> Option<&'static str> {
    let l = seg.len();
    let rest_ok = |pfx: &str, min: usize, exact: bool, pred: fn(u8) -> bool| -> bool {
        seg.starts_with(pfx) && {
            let r = &seg.as_bytes()[pfx.len()..];
            (if exact { r.len() == min } else { r.len() >= min }) && r.iter().all(|&c| pred(c))
        }
    };
    let alnum = |c: u8| c.is_ascii_alphanumeric();
    let urlsafe = |c: u8| c.is_ascii_alphanumeric() || c == b'_' || c == b'-';
    let upper_digit = |c: u8| c.is_ascii_uppercase() || c.is_ascii_digit();
    let rule = if l == 20 && ["AKIA", "ASIA", "AGPA", "AIDA", "AROA", "ANPA"].iter().any(|p| rest_ok(p, 16, true, upper_digit)) {
        "aws-access-key-id"
    } else if ["ghp_", "gho_", "ghu_", "ghs_", "ghr_"].iter().any(|p| rest_ok(p, 36, false, alnum)) {
        "github-token"
    } else if rest_ok("github_pat_", 22, false, urlsafe) {
        "github-pat"
    } else if seg.starts_with("xox") && l > 15 && matches!(seg.as_bytes()[3], b'a' | b'b' | b'p' | b'r' | b's') && seg.as_bytes()[4] == b'-' {
        "slack-token"
    } else if rest_ok("sk_live_", 16, false, alnum) || rest_ok("rk_live_", 16, false, alnum) {
        "stripe-key"
    } else if l == 39 && rest_ok("AIza", 35, true, urlsafe) {
        "google-api-key"
    } else if rest_ok("sk-ant-", 20, false, urlsafe) {
        "anthropic-key"
    } else if rest_ok("sk-", 32, false, urlsafe) {
        "openai-style-key"
    } else if rest_ok("npm_", 36, false, alnum) {
        "npm-token"
    } else if rest_ok("glpat-", 20, false, urlsafe) {
        "gitlab-token"
    } else if rest_ok("hf_", 34, false, |c| c.is_ascii_alphabetic()) {
        "huggingface-token"
    } else if rest_ok("pypi-", 50, false, urlsafe) {
        "pypi-token"
    } else if rest_ok("AGE-SECRET-KEY-1", 40, false, upper_digit) {
        "age-secret-key"
    } else {
        return None;
    };
    // Documentation placeholders such as AKIAIOSFODNN7EXAMPLE.
    let low = seg.to_ascii_lowercase();
    if low.contains("example") || low.contains("xxxx") {
        return None;
    }
    Some(rule)
}

const KEYWORDS: [&str; 15] = [
    "password", "passwd", "passphrase", "pwd", "secret", "token", "apikey", "api_key", "api-key", "private_key", "privatekey", "access_key", "accesskey",
    "auth_key", "credential",
];

const BENIGN_SUFFIX: [&str; 22] = [
    "_type", "_url", "_uri", "_file", "_path", "_name", "_len", "_length", "_count", "_expires", "_ttl", "_header", "_field", "_id", "_endpoint", "_dir", "_hint",
    "_label", "_prompt", "_policy", "_required", "_regex",
];

fn has_keyword(key: &str) -> bool {
    KEYWORDS.iter().any(|k| find_ci(key, k, 0).is_some())
}

fn benign_key(key: &str) -> bool {
    let kb = key.as_bytes();
    BENIGN_SUFFIX.iter().any(|s| kb.len() >= s.len() && kb[kb.len() - s.len()..].eq_ignore_ascii_case(s.as_bytes()))
}

/// Case-insensitive ASCII substring search.
fn find_ci(hay: &str, needle: &str, from: usize) -> Option<usize> {
    let (h, nd) = (hay.as_bytes(), needle.as_bytes());
    if nd.is_empty() || h.len() < nd.len() || from > h.len() - nd.len() {
        return None;
    }
    let first = nd[0].to_ascii_lowercase();
    (from..=h.len() - nd.len()).find(|&i| h[i].to_ascii_lowercase() == first && h[i..i + nd.len()].eq_ignore_ascii_case(nd))
}

/// Next whitespace-delimited word at or after `from`; a quoted word loses its quotes.
fn next_word(line: &str, from: usize) -> Option<(usize, usize)> {
    let b = line.as_bytes();
    let mut s = from;
    while s < b.len() && (b[s] == b' ' || b[s] == b'\t') {
        s += 1;
    }
    if s >= b.len() || b[s] == b'\r' {
        return None;
    }
    if matches!(b[s], b'"' | b'\'') {
        let q = b[s];
        let e = b[s + 1..].iter().position(|&c| c == q).map_or(b.len(), |i| s + 1 + i);
        return (e > s + 1).then_some((s + 1, e));
    }
    let mut e = s;
    while e < b.len() && !matches!(b[e], b' ' | b'\t' | b'\r') {
        e += 1;
    }
    Some((s, e))
}

fn token_span(b: &[u8], from: usize) -> (usize, usize) {
    let mut e = from;
    while e < b.len() && (b[e].is_ascii_alphanumeric() || matches!(b[e], b'.' | b'_' | b'~' | b'+' | b'/' | b'=' | b'-')) {
        e += 1;
    }
    (from.min(e), e)
}

/// Value after an assignment separator, or `None` when it is not secret-shaped.
fn assignment_value(line: &str, after: usize) -> Option<(usize, usize)> {
    let b = line.as_bytes();
    let n = b.len();
    let mut s = after;
    while s < n && (b[s] == b' ' || b[s] == b'\t') {
        s += 1;
    }
    if s >= n {
        return None;
    }
    let (vs, ve, quoted) = if matches!(b[s], b'"' | b'\'' | b'`') {
        let q = b[s];
        let mut e = s + 1;
        while e < n && b[e] != q {
            if b[e] == b'\\' {
                e += 1;
            }
            e += 1;
        }
        let e = e.min(n);
        (s + 1, e, true)
    } else {
        let mut e = s;
        while e < n && !matches!(b[e], b' ' | b'\t' | b',' | b';' | b'}' | b')' | b']' | b'(' | b'\r' | b'"' | b'\'') {
            e += 1;
        }
        if e < n && b[e] == b'(' {
            return None; // function call
        }
        (s, e, false)
    };
    if ve <= vs {
        return None;
    }
    let v = &line[vs..ve];
    if is_placeholder(v) {
        return None;
    }
    if quoted {
        if v.len() < 6 || v.contains(char::is_whitespace) {
            return None;
        }
    } else {
        if v.len() < 8 || matches!(b[vs], b'$' | b'%' | b'{' | b'<' | b'@' | b'/' | b'~' | b'.' | b'[' | b'&' | b'*') {
            return None;
        }
        // A bare identifier or dotted path (`self.token`, `SOME_CONSTANT`) is a reference, not a value.
        if v.bytes().all(|c| c.is_ascii_alphabetic() || c == b'_' || c == b'.' || c == b'-') {
            return None;
        }
    }
    Some((vs, ve))
}

fn entropy_context(b: &[u8], s0: usize) -> bool {
    let mut k = s0;
    while k > 0 && matches!(b[k - 1], b'"' | b'\'' | b' ' | b'\t') {
        k -= 1;
    }
    k > 0 && matches!(b[k - 1], b'=' | b':')
}

fn looks_random(seg: &str) -> bool {
    let s = seg.as_bytes();
    let hex = s.iter().all(|c| c.is_ascii_hexdigit());
    let digit = s.iter().any(|c| c.is_ascii_digit());
    let alpha = s.iter().any(|c| c.is_ascii_alphabetic());
    if hex || !digit || !alpha {
        return false;
    }
    let mut counts = [0u32; 128];
    for &c in s {
        counts[(c & 0x7f) as usize] += 1;
    }
    let n = s.len() as f64;
    let h: f64 = counts.iter().filter(|&&k| k > 0).map(|&k| {
        let p = k as f64 / n;
        -p * p.log2()
    }).sum();
    h >= 4.0
}

/// Obvious examples and references: `YOUR_API_KEY`, `xxxx`, `<token>`, `${VAR}`, `changeme`...
pub fn is_placeholder(v: &str) -> bool {
    let t = v.trim();
    if t.is_empty() {
        return true;
    }
    let l = t.to_ascii_lowercase();
    const SUB: [&str; 28] = [
        "your", "example", "changeme", "change_me", "placeholder", "dummy", "xxxx", "****", "....", "<", ">", "${", "{{", "%s", "$(", "todo", "redacted",
        "insert", "replace", "sample", "fake", "foobar", "_here", "enter", "mypassword", "my_password", "notasecret", "lorem",
    ];
    if SUB.iter().any(|s| l.contains(s)) {
        return true;
    }
    const EXACT: [&str; 26] = [
        "password", "passwd", "secret", "token", "test", "null", "none", "nil", "true", "false", "undefined", "string", "bearer", "basic", "admin", "root", "user",
        "username", "default", "pass", "pwd", "apikey", "api_key", "key", "value", "empty",
    ];
    if EXACT.contains(&l.as_str()) {
        return true;
    }
    let bytes = l.as_bytes();
    let mut distinct = [false; 256];
    for &c in bytes {
        distinct[c as usize] = true;
    }
    let d = distinct.iter().filter(|&&x| x).count();
    // all the same character, or a trivially repeating pattern
    d == 1 || (bytes.len() >= 6 && d <= 2)
}

/// Sort and merge overlapping findings (union span, highest severity, first rule).
fn merge(mut v: Vec<Finding>) -> Vec<Finding> {
    v.sort_by(|a, b| a.start.cmp(&b.start).then(b.end.cmp(&a.end)));
    let mut out: Vec<Finding> = Vec::with_capacity(v.len());
    for f in v {
        match out.last_mut() {
            Some(last) if f.start < last.end => {
                last.end = last.end.max(f.end);
                if f.severity > last.severity {
                    last.severity = f.severity;
                    last.rule = f.rule;
                }
            }
            _ => out.push(f),
        }
    }
    out
}

/// Result of [`mask`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Masked {
    pub text: String,
    pub redactions: usize,
}

/// Replace every finding of at least `min` severity by `[REDACTED:<rule>:<len>]`, keeping every
/// line break inside the replaced span, so the line count and the line numbers do not change.
/// `findings` must come from `scan(text)` (sorted, non overlapping).
pub fn mask(text: &str, findings: &[Finding], min: Severity) -> Masked {
    mask_with(text, findings, min, true)
}

fn mask_with(text: &str, findings: &[Finding], min: Severity, with_len: bool) -> Masked {
    let mut out = String::with_capacity(text.len());
    let (mut at, mut redactions) = (0usize, 0usize);
    for f in findings.iter().filter(|f| f.severity >= min) {
        if f.start < at || f.end > text.len() || !text.is_char_boundary(f.start) || !text.is_char_boundary(f.end) {
            continue;
        }
        out.push_str(&text[at..f.start]);
        let span = &text[f.start..f.end];
        if with_len {
            out.push_str(&format!("[REDACTED:{}:{}]", f.rule, span.len()));
        } else {
            out.push_str(&format!("[REDACTED:{}]", f.rule));
        }
        out.extend(span.chars().filter(|&c| c == '\n' || c == '\r'));
        at = f.end;
        redactions += 1;
    }
    out.push_str(&text[at..]);
    Masked { text: out, redactions }
}

/// Scan then mask High and Medium findings. Text beyond the size cap is NOT returned
/// (`complete = false`): a caller that would otherwise leak the unscanned tail must fail closed.
pub fn mask_text(scanner: &Scanner, text: &str) -> (Masked, bool) {
    let s = scanner.scan_all(text);
    let mut m = mask(&text[..s.scanned_bytes], &s.findings, Severity::Medium);
    if s.truncated {
        m.text.push_str("\n[REDACTED:unscanned-tail]");
    }
    (m, !s.truncated)
}

/// For logs and diagnostics: replace secret-looking spans (Medium and above) by
/// `[REDACTED:<rule>]`. Never returns the unscanned tail of an over-long input.
pub fn redact_text(text: &str) -> String {
    let s = scan(text);
    let mut m = mask_with(&text[..s.scanned_bytes], &s.findings, Severity::Medium, false).text;
    if s.truncated {
        m.push_str("[REDACTED:unscanned-tail]");
    }
    m
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rules(t: &str) -> Vec<&'static str> {
        scan(t).findings.iter().map(|f| f.rule).collect()
    }

    // Built at runtime so this file never contains a literal that real scanners flag.
    fn ghp() -> String {
        format!("ghp_{}", "aB3dE6gH9jK2mN5pQ8sT1vW4yZ7bC0dF3gH6")
    }

    #[test]
    fn private_key_blocks_are_masked_with_every_line_kept() {
        let t = "before\n-----BEGIN RSA PRIVATE KEY-----\nMIIEowIBAAKCAQEAxyz\nabcdef\n-----END RSA PRIVATE KEY-----\nafter\n";
        let s = scan(t);
        assert_eq!(s.findings.len(), 1);
        assert_eq!(s.findings[0].severity, Severity::High);
        assert_eq!(s.findings[0].line, 2);
        let m = mask(t, &s.findings, Severity::Medium);
        assert_eq!(m.text.matches('\n').count(), t.matches('\n').count());
        assert!(m.text.starts_with("before\n[REDACTED:private-key:"));
        assert!(m.text.ends_with("\nafter\n"));
        assert!(!m.text.contains("MIIEow") && !m.text.contains("abcdef"));
    }

    #[test]
    fn truncated_private_key_without_end_is_still_masked() {
        let t = "x\n-----BEGIN OPENSSH PRIVATE KEY-----\nb3BlbnNzaC1rZXktdjEAAAAA\nsecondline\n\nnormal text\n";
        let m = mask(t, &scan(t).findings, Severity::Medium);
        assert!(!m.text.contains("b3BlbnNz") && !m.text.contains("secondline"));
        assert!(m.text.contains("normal text"));
        assert_eq!(m.text.matches('\n').count(), t.matches('\n').count());
    }

    #[test]
    fn provider_token_shapes() {
        assert_eq!(rules(&format!("t = {}", ghp())), vec!["github-token"]);
        assert_eq!(rules("k=AKIAQ3EXTRA9ZZ7PLM2R"), vec!["aws-access-key-id"]);
        assert_eq!(rules(&format!("x xoxb-{}-{}", "123456789012", "AbCdEfGhIjKl")), vec!["slack-token"]);
        assert_eq!(rules(&format!("sk_live_{}", "a1B2c3D4e5F6g7H8i9J0")), vec!["stripe-key"]);
        assert_eq!(rules(&format!("key: AIza{}", "SyA1b2C3d4E5f6G7h8I9j0K1l2M3n4O5p6Q")), vec!["google-api-key"]);
        assert_eq!(rules(&format!("sk-ant-api03-{}", "aB3dE6gH9jK2mN5pQ8sT1v")), vec!["anthropic-key"]);
        assert_eq!(rules(&format!("npm_{}", "aB3dE6gH9jK2mN5pQ8sT1vW4yZ7bC0dF3gH6")), vec!["npm-token"]);
        let jwt = format!("eyJhbGciOiJIUzI1NiJ9.{}.{}", "eyJzdWIiOiIxMjM0NTY3ODkwIn0", "dozjgNryP4J3jVmNHl0w5N_XgL0n3I9PlFUP0THsR8U");
        assert_eq!(rules(&format!("Cookie: {jwt}")), vec!["jwt"]);
    }

    #[test]
    fn url_credentials_mask_only_the_password() {
        let t = "DATABASE_URL=postgres://app:Zq8vLm2xPw@db.internal:5432/app";
        let s = scan(t);
        assert_eq!(s.findings[0].rule, "url-credentials");
        let m = mask(t, &s.findings, Severity::Medium).text;
        assert!(m.starts_with("DATABASE_URL=postgres://app:[REDACTED:url-credentials:10]@db.internal"), "{m}");
        assert!(scan("https://example.com/a?b=c").findings.is_empty());
        assert!(scan("redis://${USER}:${PASS}@host").findings.is_empty());
    }

    #[test]
    fn assignments_and_forms() {
        for t in [
            r#"password = "Tr0ub4dor&3xyz""#,
            r#"{"api_key": "9f8e7d6c5b4a3f2e"}"#,
            "DB_PASSWORD=s3cr3tPassw0rd",
            "'token' => 'abCD1234efGH5678'",
            "client_secret: 7hGf3kLm9QwErTy",
            "mysql --password hunter2xyz db",
            "sshpass -p Zq8vLm2xPw ssh host",
            "Authorization: Bearer abcdefghijklmnopqrstuvwx0123",
            "define('DB_PASSWORD', 'Zq8vLm2xPw4r');",
            "cfg.set(\"api_key\", \"9f8e7d6c5b4a3f2e\")",
            "who = evil-ghp_Zq8vLm2xPw4rT6yU9iO1aS3dF5gH7jK0lN2b",
        ] {
            assert!(scan(t).count_at_least(Severity::Medium) >= 1, "must flag: {t}");
        }
    }

    #[test]
    fn placeholders_and_code_are_not_flagged() {
        for t in [
            "password = \"YOUR_PASSWORD\"",
            "API_KEY=xxxxxxxxxxxxxxxx",
            "token: <token>",
            "secret = ${SECRET_VALUE}",
            "password=changeme123",
            "let password = get_password();",
            "token = self.token_value",
            "password: String,",
            "AWS_KEY=AKIAIOSFODNN7EXAMPLE",
            "token_type = \"bearer\"",
            "password = \"{{ secrets.DB }}\"",
            "if password == other_password {",
            "std::collections::HashMap<String, u32>",
            "pwd = os.getcwd()",
            "let tokens = vec![1, 2, 3];",
        ] {
            assert!(scan(t).count_at_least(Severity::Medium) == 0, "false positive: {t} -> {:?}", scan(t).findings);
        }
        assert!(is_placeholder("<TOKEN>") && is_placeholder("aaaaaaaa") && is_placeholder("YOUR_API_KEY"));
        assert!(!is_placeholder("Zq8vLm2xPw"));
    }

    #[test]
    fn masking_is_idempotent_and_keeps_line_count() {
        let t = format!("a\npassword = \"Tr0ub4dor&3xyz\"\n{}\nz", ghp());
        let m1 = mask(&t, &scan(&t).findings, Severity::Medium);
        assert_eq!(m1.redactions, 2);
        assert_eq!(m1.text.lines().count(), t.lines().count());
        let m2 = mask(&m1.text, &scan(&m1.text).findings, Severity::Medium);
        assert_eq!(m2.redactions, 0);
        assert_eq!(m2.text, m1.text);
    }

    #[test]
    fn allowlist_fingerprint_suppresses_without_storing_text() {
        let t = "password = \"Tr0ub4dor&3xyz\"";
        let mut a = Allowlist::new("salt1");
        let fp = a.allow("assignment", "Tr0ub4dor&3xyz");
        assert!(!fp.contains("Tr0ub"));
        let s = Scanner::new().with_allowlist(a.clone()).scan(t);
        assert!(s.findings.is_empty());
        // another salt does not match
        let s2 = Scanner::new().with_allowlist(Allowlist::new("salt2")).scan(t);
        assert_eq!(s2.findings.len(), 1);
    }

    #[test]
    fn redact_text_for_logs() {
        let r = redact_text("connect postgres://u:Zq8vLm2xPw@h/db with token=abCD1234efGH5678 ok");
        assert!(!r.contains("Zq8vLm2xPw") && !r.contains("abCD1234efGH5678"), "{r}");
        assert!(r.contains("[REDACTED:url-credentials]"));
    }

    #[test]
    fn size_cap_is_reported_and_tail_is_not_returned() {
        let big = format!("{}\npassword = \"Tr0ub4dor&3xyz\"\n", "a line of text\n".repeat(100));
        let sc = Scanner::new().with_max_bytes(50);
        let s = sc.scan(&big);
        assert!(s.truncated && s.scanned_bytes <= 50);
        let (m, complete) = mask_text(&sc, &big);
        assert!(!complete && !m.text.contains("Tr0ub4dor") && m.text.ends_with("[REDACTED:unscanned-tail]"));
    }

    #[test]
    fn scan_all_chunks_big_text_and_fails_closed_beyond_the_total_cap() {
        let line = "padding line of ordinary text\n";
        let t = format!("{}password = \"Tr0ub4dor&3xyz\"\n{}", line.repeat(40), line.repeat(40));
        let sc = Scanner::new().with_max_bytes(300);
        let s = sc.scan_all(&t);
        assert!(!s.truncated);
        assert_eq!(s.findings.len(), 1);
        assert_eq!(s.findings[0].line, 41);
        assert!(&t[s.findings[0].start..s.findings[0].end] == "Tr0ub4dor&3xyz");
        let tiny = Scanner::new().with_max_bytes(10);
        assert!(tiny.scan_all(&t).truncated, "beyond 16 x cap is cut off");
    }

    #[test]
    fn hostile_inputs_do_not_panic() {
        let mut v = vec![String::new(), "=".repeat(5000), ":".repeat(5000), "\"".repeat(777), "password=".repeat(300), "://@:".repeat(500), "-----BEGIN PRIVATE KEY-----".repeat(300)];
        v.push("é=é:é\r\n\r\n".repeat(400));
        v.push(format!("password=\"{}", "\u{202e}ñ".repeat(900)));
        for t in &v {
            let s = scan(t);
            let m = mask(t, &s.findings, Severity::Info);
            assert_eq!(m.text.matches('\n').count(), t.matches('\n').count());
        }
    }

    #[test]
    fn entropy_rule_needs_context_and_is_info() {
        let s = scan("session = \"aB3dE6gH9jK2mN5pQ8sT1vW4yZ7bC0dF3gH6\"");
        assert!(s.findings.iter().any(|f| f.rule == "high-entropy" && f.severity == Severity::Info));
        assert!(scan("see aB3dE6gH9jK2mN5pQ8sT1vW4yZ7bC0dF3gH6 here").findings.is_empty());
        assert!(scan("hash = \"da39a3ee5e6b4b0d3255bfef95601890afd80709\"").findings.is_empty());
    }
}
