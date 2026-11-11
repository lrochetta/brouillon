//! `check`: fails (exit 1) when an image breaks a rule of the Brouillon image policy.
use crate::pipeline::{svg_dims, Manifest, SOURCES};
use crate::util::png_dims;
use std::path::{Path, PathBuf};

// Forbidden third-party names. Assembled from fragments so this file passes its own scan
// (same list as site/scripts/forbidden.mjs, plus the word "clone").
fn forbidden() -> Vec<(String, String)> {
    let v = [
        ("Edit", "Plus"),
        ("Notepad", "++"),
        ("Sub", "lime"),
        ("Edit", "Pad"),
        ("Agent", "Pad"),
        ("Scin", "tilla"),
        ("Lex", "illa"),
        ("clo", "ne"),
    ];
    v.iter().map(|(a, b)| (format!("{a}{b}"), format!("{a}{b}").to_lowercase())).collect()
}

fn find_forbidden(text: &str) -> Vec<String> {
    let t = text.to_lowercase();
    let squeezed: String = t.chars().filter(|c| !c.is_whitespace() && *c != '\u{a0}').collect();
    let mut hits = vec![];
    for (name, low) in forbidden() {
        let hit = if low == "clone" {
            // whole word only ("clone", "clones")
            t.split(|c: char| !c.is_alphanumeric()).any(|w| w == "clone" || w == "clones")
        } else {
            squeezed.contains(&low)
        };
        if hit {
            hits.push(name);
        }
    }
    if squeezed.contains(&format!("notepad{}{}", "plus", "plus")) {
        hits.push("Notepad plus plus".into());
    }
    hits
}

/// Budgets in bytes, by kind and format (the 150 kB page budget is checked at site level).
pub fn budget(kind: &str, ext: &str, name: &str) -> u64 {
    match (kind, ext) {
        ("illustration", "svg") => 20_000,
        ("infographic", "svg") => 30_000,
        ("social", "svg") => 60_000,
        (_, "webp") => 90_000,
        ("screenshot", "png") => 500_000,
        ("social", "png") => {
            if name.starts_with("github-social") {
                900_000 // GitHub limit is 1 MB
            } else if name.starts_with("og-") {
                250_000
            } else if name.starts_with("readme-banner") {
                200_000
            } else {
                300_000
            }
        }
        _ => 100_000,
    }
}

fn social_dims(name: &str) -> Option<(u32, u32)> {
    if name.starts_with("og-") {
        Some((1200, 630))
    } else if name.starts_with("github-social-preview") {
        Some((1280, 640))
    } else if name.starts_with("readme-banner") {
        Some((1280, 320))
    } else if name.starts_with("release-banner") {
        Some((1200, 600))
    } else {
        None
    }
}

fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
    if let Ok(rd) = std::fs::read_dir(dir) {
        let mut v: Vec<PathBuf> = rd.filter_map(|e| e.ok()).map(|e| e.path()).collect();
        v.sort();
        for p in v {
            if p.is_dir() {
                walk(&p, out);
            } else {
                out.push(p);
            }
        }
    }
}

/// Returns (errors, svg_text) for one SVG source.
fn check_svg(rel: &str, src: &str, errs: &mut Vec<String>) -> String {
    let doc = match roxmltree::Document::parse(src) {
        Ok(d) => d,
        Err(e) => {
            errs.push(format!("{rel}: not valid XML ({e})"));
            return String::new();
        }
    };
    let root = doc.root_element();
    let has_title = root.children().any(|n| n.has_tag_name("title") && n.text().map(|t| !t.trim().is_empty()).unwrap_or(false));
    if !has_title {
        errs.push(format!("{rel}: missing <title> as a direct child of <svg>"));
    }
    let mut corpus = String::new();
    for n in doc.descendants() {
        if n.is_element() {
            let name = n.tag_name().name().to_lowercase();
            if matches!(name.as_str(), "script" | "foreignobject" | "iframe" | "object" | "embed") {
                errs.push(format!("{rel}: forbidden element <{name}>"));
            }
            for a in n.attributes() {
                let an = a.name().to_lowercase();
                let v = a.value().trim();
                let vl = v.to_lowercase();
                if an.starts_with("on") && an.len() > 2 && an.chars().all(|c| c.is_ascii_alphabetic()) {
                    errs.push(format!("{rel}: event handler attribute {an}"));
                }
                if (an == "href" || an == "src") && !(v.starts_with('#') || vl.starts_with("data:image/")) {
                    errs.push(format!("{rel}: external or script reference {an}=\"{}\"", v.chars().take(60).collect::<String>()));
                }
                if vl.contains("javascript:") {
                    errs.push(format!("{rel}: javascript: URL"));
                }
                if let Some(p) = vl.find("url(") {
                    let arg = vl[p + 4..].trim_start().trim_start_matches(['\'', '"']);
                    if !arg.starts_with('#') && !arg.starts_with("data:image/") {
                        errs.push(format!("{rel}: external url() in attribute {an}"));
                    }
                }
            }
            if name == "style" {
                let css = n.text().unwrap_or("").to_lowercase();
                if css.contains("@import") || css.contains("http://") || css.contains("https://") || css.contains("expression(") {
                    errs.push(format!("{rel}: <style> loads or imports external content"));
                }
                let mut rest = css.as_str();
                while let Some(p) = rest.find("url(") {
                    let arg = rest[p + 4..].trim_start().trim_start_matches(['\'', '"']);
                    if !arg.starts_with('#') && !arg.starts_with("data:image/") {
                        errs.push(format!("{rel}: <style> url() is not local"));
                    }
                    rest = &rest[p + 4..];
                }
            }
        } else if n.is_text() {
            corpus.push_str(n.text().unwrap_or(""));
            corpus.push(' ');
        }
    }
    // any scheme URL left in text content or attributes (namespace declarations are not attributes in roxmltree)
    if corpus.contains("http://") || corpus.contains("https://") {
        errs.push(format!("{rel}: external URL in text content"));
    }
    corpus
}

fn png_has_metadata(b: &[u8]) -> Option<String> {
    let mut i = 8;
    while i + 8 <= b.len() {
        let len = u32::from_be_bytes(b[i..i + 4].try_into().ok()?) as usize;
        let ty = &b[i + 4..i + 8];
        if matches!(ty, b"tEXt" | b"iTXt" | b"zTXt" | b"eXIf" | b"tIME") {
            return Some(String::from_utf8_lossy(ty).to_string());
        }
        i += 12 + len;
    }
    None
}

pub fn run(assets: &Path) -> bool {
    let mut errs: Vec<String> = vec![];
    let mut warns: Vec<String> = vec![];
    let rel_of = |p: &Path| p.strip_prefix(assets).unwrap_or(p).to_string_lossy().replace('\\', "/");

    // 1. every file on disk
    let mut all = vec![];
    walk(assets, &mut all);
    let mut svg_text: std::collections::HashMap<String, String> = Default::default();
    let mut n_files = 0;
    for p in &all {
        let rel = rel_of(p);
        let ext = p.extension().map(|e| e.to_string_lossy().to_lowercase()).unwrap_or_default();
        if rel == "manifest.json" {
            continue;
        }
        let bytes = std::fs::read(p).unwrap();
        n_files += 1;
        match ext.as_str() {
            "svg" => {
                let src = String::from_utf8_lossy(&bytes).to_string();
                let c = check_svg(&rel, &src, &mut errs);
                svg_text.insert(rel.clone(), c);
            }
            "png" => {
                if png_dims(&bytes).is_none() {
                    errs.push(format!("{rel}: not a PNG"));
                }
                if rel.starts_with("optimized/") || rel.starts_with("social/") {
                    if let Some(c) = png_has_metadata(&bytes) {
                        errs.push(format!("{rel}: PNG carries a {c} metadata chunk"));
                    }
                }
            }
            "webp" | "json" => {}
            _ => warns.push(format!("{rel}: unexpected file type (ignored)")),
        }
        if !find_forbidden(&p.file_name().unwrap().to_string_lossy()).is_empty() {
            errs.push(format!("{rel}: forbidden name in file name"));
        }
        if ext == "json" {
            let t = String::from_utf8_lossy(&bytes);
            let h = find_forbidden(&t);
            if !h.is_empty() {
                errs.push(format!("{rel}: forbidden name(s) {h:?}"));
            }
        }
    }

    // 2. manifest
    let mpath = assets.join("manifest.json");
    let manifest: Option<Manifest> = std::fs::read_to_string(&mpath).ok().and_then(|t| serde_json::from_str(&t).ok());
    match &manifest {
        None => errs.push("manifest.json missing or unreadable: run `process`".into()),
        Some(m) => {
            let mut listed: std::collections::HashSet<String> = Default::default();
            for a in &m.assets {
                if a.status != "delivered" && a.status != "preview" {
                    errs.push(format!("{}: status must be delivered or preview, got {:?}", a.id, a.status));
                }
                if a.alt.en.trim().is_empty() {
                    errs.push(format!("{}: alt text (EN) missing", a.id));
                }
                if a.alt.fr.trim().is_empty() {
                    errs.push(format!("{}: alt text (FR) missing", a.id));
                }
                let mut corpus = format!("{} {} {} {}", a.id, a.alt.en, a.alt.fr, a.text.join(" "));
                for f in &a.files {
                    listed.insert(f.path.clone());
                    let p = assets.join(&f.path);
                    let bytes = match std::fs::read(&p) {
                        Ok(b) => b,
                        Err(_) => {
                            errs.push(format!("{}: listed file {} does not exist", a.id, f.path));
                            continue;
                        }
                    };
                    if bytes.len() as u64 != f.bytes {
                        errs.push(format!("{}: {} changed since `process` ({} vs {} bytes): re-run", a.id, f.path, bytes.len(), f.bytes));
                    }
                    let name = p.file_name().unwrap().to_string_lossy().to_string();
                    let max = budget(&a.kind, &f.format, &name).min(if f.format == "svg" { a.budget_bytes.unwrap_or(u64::MAX) } else { u64::MAX });
                    if bytes.len() as u64 > max {
                        errs.push(format!("{}: {} is {} bytes, budget {} bytes", a.id, f.path, bytes.len(), max));
                    }
                    let dims = match f.format.as_str() {
                        "png" => png_dims(&bytes),
                        "svg" => svg_dims(&String::from_utf8_lossy(&bytes)),
                        _ => Some((f.width, f.height)),
                    };
                    if dims != Some((f.width, f.height)) {
                        errs.push(format!("{}: {} dimensions {:?} differ from the manifest ({}x{})", a.id, f.path, dims, f.width, f.height));
                    }
                    if a.kind == "social" {
                        if let Some(d) = social_dims(&name) {
                            if dims != Some(d) {
                                errs.push(format!("{}: {} must be {}x{}", a.id, f.path, d.0, d.1));
                            }
                        }
                    }
                    if let Some(t) = svg_text.get(&f.path) {
                        corpus.push(' ');
                        corpus.push_str(t);
                    }
                }
                let h = find_forbidden(&corpus);
                if !h.is_empty() {
                    errs.push(format!("{}: forbidden name(s) {h:?} in text or alt", a.id));
                }
                let up = corpus.to_uppercase();
                let words: Vec<&str> = up.split(|c: char| !c.is_alphanumeric()).collect();
                if (words.contains(&"MCP") || words.contains(&"SSH")) && !{ let l = corpus.to_lowercase(); l.contains("preview") || l.contains("aperçu") || l.contains("apercu") } {
                    errs.push(format!("{}: mentions MCP or SSH without the word preview / aperçu", a.id));
                }
            }
            // sources not processed
            for p in &all {
                let rel = rel_of(p);
                let first = rel.split('/').next().unwrap_or("");
                let ext = p.extension().map(|e| e.to_string_lossy().to_lowercase()).unwrap_or_default();
                if SOURCES.iter().any(|(d, _)| *d == first) && (ext == "svg" || ext == "png") && !rel.contains("/_") {
                    let stem = p.file_stem().unwrap().to_string_lossy().to_string();
                    let id = format!("{first}/{}", stem.trim_end_matches("@2x"));
                    if !m.assets.iter().any(|a| a.id == id) {
                        errs.push(format!("{rel}: not in manifest.json, run `process`"));
                    }
                }
            }
            let _ = listed;
            println!("checked {} files, {} assets", n_files, m.assets.len());
        }
    }
    for w in &warns {
        println!("warn  {w}");
    }
    for e in &errs {
        println!("FAIL  {e}");
    }
    if errs.is_empty() {
        println!("OK: all image checks pass");
    }
    errs.is_empty()
}
