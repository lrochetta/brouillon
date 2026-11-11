//! Image pipeline: assets/{illustrations,infographics}/*.svg, assets/screenshots/*.png and assets/social/*
//! -> assets/optimized/** (safe SVG minification, recompressed PNG, lossless WebP, responsive widths)
//! + assets/manifest.json.
use crate::util::*;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

#[derive(Serialize, Deserialize, Default, Clone, Debug)]
pub struct Alt {
    #[serde(default)]
    pub en: String,
    #[serde(default)]
    pub fr: String,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct FileEntry {
    pub path: String,
    pub format: String,
    pub width: u32,
    pub height: u32,
    pub bytes: u64,
    pub hash: String,
    pub role: String, // source | optimized | variant
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Asset {
    pub id: String,
    pub kind: String,
    pub status: String,
    pub alt: Alt,
    pub text: Vec<String>,
    pub files: Vec<FileEntry>,
    pub srcset: BTreeMap<String, String>,
    /// Per-file budget from the design manifest seed (max_kb), when it is stricter than the default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub budget_bytes: Option<u64>,
}

#[derive(Serialize, Deserialize, Debug)]
pub struct Manifest {
    pub schema: u32,
    pub generated_by: String,
    pub assets: Vec<Asset>,
}

/// (directory under assets/, kind)
pub const SOURCES: [(&str, &str); 4] =
    [("illustrations", "illustration"), ("infographics", "infographic"), ("screenshots", "screenshot"), ("social", "social")];
/// Widths offered for raster images (only those smaller than the original are generated).
const WIDTHS: [u32; 5] = [640, 960, 1280, 1600, 1920];

fn files_in(dir: &Path) -> Vec<PathBuf> {
    let mut v: Vec<PathBuf> = std::fs::read_dir(dir).map(|r| r.filter_map(|e| e.ok()).map(|e| e.path()).filter(|p| p.is_file()).collect()).unwrap_or_default();
    v.sort();
    v
}

fn read_meta(dir: &Path, stems: &[&str]) -> Value {
    for s in stems {
        if let Ok(t) = std::fs::read_to_string(dir.join(format!("{s}.meta.json"))) {
            return serde_json::from_str(&t).unwrap_or_else(|e| panic!("{s}.meta.json: {e}"));
        }
    }
    if let Ok(t) = std::fs::read_to_string(dir.join("_meta.json")) {
        let all: Value = serde_json::from_str(&t).unwrap_or_else(|e| panic!("_meta.json: {e}"));
        for s in stems {
            if let Some(v) = all.get(*s) {
                return v.clone();
            }
        }
    }
    Value::Null
}

fn svg_title(src: &str) -> String {
    roxmltree::Document::parse(src)
        .ok()
        .and_then(|d| d.descendants().find(|n| n.has_tag_name("title")).and_then(|n| n.text().map(|t| t.trim().to_string())))
        .unwrap_or_default()
}

pub fn svg_dims(src: &str) -> Option<(u32, u32)> {
    let t = resvg::usvg::Tree::from_str(src, &resvg::usvg::Options::default()).ok()?;
    Some((t.size().width().ceil() as u32, t.size().height().ceil() as u32))
}

/// Safe SVG minification: drops prolog, doctype, comments, <metadata>, editor namespaces/attributes and
/// layout-only whitespace. Keeps ids, <title>, <desc>, <style> rules, var() and text whitespace.
pub fn minify_svg(src: &str) -> String {
    let src = src.trim_start_matches('\u{feff}');
    let b = src.as_bytes();
    let (mut i, mut out) = (0usize, String::with_capacity(src.len()));
    let mut in_text = 0u32;
    let mut raw_until: Option<&'static str> = None; // inside <style>/<script>: copy up to the closing tag
    let drop_el = |n: &str| n == "metadata" || n.starts_with("sodipodi:") || n.starts_with("inkscape:") || n.starts_with("rdf:") || n.starts_with("cc:") || n.starts_with("dc:");
    let drop_attr = |n: &str| n.starts_with("inkscape:") || n.starts_with("sodipodi:") || n.starts_with("xmlns:inkscape") || n.starts_with("xmlns:sodipodi") || n.starts_with("xmlns:rdf") || n.starts_with("xmlns:cc") || n.starts_with("xmlns:dc") || n.starts_with("data-name");
    while i < b.len() {
        if let Some(close) = raw_until {
            let end = src[i..].find(close).map(|p| i + p).unwrap_or(b.len());
            let css = src[i..end].trim();
            // strip CSS comments, collapse whitespace
            let mut c = String::new();
            let mut rest = css;
            while let Some(p) = rest.find("/*") {
                c.push_str(&rest[..p]);
                rest = rest[p..].find("*/").map(|q| &rest[p + q + 2..]).unwrap_or("");
            }
            c.push_str(rest);
            out.push_str(&c.split_whitespace().collect::<Vec<_>>().join(" "));
            i = end;
            raw_until = None;
            continue;
        }
        if b[i] != b'<' {
            let end = src[i..].find('<').map(|p| i + p).unwrap_or(b.len());
            let t = &src[i..end];
            if in_text > 0 || !t.trim().is_empty() && !t.contains('\n') {
                out.push_str(t);
            } else if !t.trim().is_empty() {
                out.push_str(t.trim());
            }
            i = end;
            continue;
        }
        if src[i..].starts_with("<!--") {
            i = src[i..].find("-->").map(|p| i + p + 3).unwrap_or(b.len());
            continue;
        }
        if src[i..].starts_with("<?") {
            i = src[i..].find("?>").map(|p| i + p + 2).unwrap_or(b.len());
            continue;
        }
        if src[i..].starts_with("<!DOCTYPE") {
            i = src[i..].find('>').map(|p| i + p + 1).unwrap_or(b.len());
            continue;
        }
        if src[i..].starts_with("<![CDATA[") {
            let e = src[i..].find("]]>").map(|p| i + p + 3).unwrap_or(b.len());
            out.push_str(&src[i..e]);
            i = e;
            continue;
        }
        // a tag: find its end, respecting quotes
        let mut j = i + 1;
        let mut q = 0u8;
        while j < b.len() {
            let c = b[j];
            if q != 0 {
                if c == q {
                    q = 0;
                }
            } else if c == b'"' || c == b'\'' {
                q = c;
            } else if c == b'>' {
                break;
            }
            j += 1;
        }
        let tag = &src[i + 1..j.min(b.len())];
        i = (j + 1).min(b.len());
        if let Some(name) = tag.strip_prefix('/') {
            let name = name.trim();
            if name == "text" && in_text > 0 {
                in_text -= 1;
            }
            out.push_str(&format!("</{name}>"));
            continue;
        }
        let selfclose = tag.ends_with('/');
        let body = tag.trim_end_matches('/');
        let name_end = body.find(|c: char| c.is_whitespace()).unwrap_or(body.len());
        let name = &body[..name_end];
        if drop_el(name) {
            if !selfclose {
                let close = format!("</{name}>");
                i = src[i..].find(&close).map(|p| i + p + close.len()).unwrap_or(b.len());
            }
            continue;
        }
        // attributes
        let mut s = format!("<{name}");
        let mut r = &body[name_end..];
        loop {
            r = r.trim_start();
            if r.is_empty() {
                break;
            }
            let eq = match r.find('=') {
                Some(e) => e,
                None => break,
            };
            let an = r[..eq].trim();
            let rest = r[eq + 1..].trim_start();
            let qc = rest.chars().next().unwrap_or('"');
            let end = rest[1..].find(qc).map(|p| p + 1).unwrap_or(rest.len() - 1);
            let val = &rest[1..end];
            r = &rest[(end + 1).min(rest.len())..];
            if drop_attr(an) {
                continue;
            }
            let val = val.split_whitespace().collect::<Vec<_>>().join(" ");
            s.push_str(&format!(" {an}={qc}{val}{qc}"));
        }
        if selfclose {
            s.push_str("/>");
        } else {
            s.push('>');
            if name == "text" {
                in_text += 1;
            }
            if name == "style" {
                raw_until = Some("</style>");
            }
        }
        out.push_str(&s);
    }
    out
}

fn entry(rel: &str, format: &str, w: u32, h: u32, bytes: &[u8], role: &str) -> FileEntry {
    FileEntry { path: rel.replace('\\', "/"), format: format.into(), width: w, height: h, bytes: bytes.len() as u64, hash: sha8(bytes), role: role.into() }
}

fn status_of(meta: &Value) -> String {
    meta["status"].as_str().unwrap_or("delivered").to_string()
}

fn alt_of(meta: &Value, fallback_en: &str) -> Alt {
    let g = |k: &str| meta["alt"][k].as_str().unwrap_or("").to_string();
    let mut a = Alt { en: g("en"), fr: g("fr") };
    if a.en.is_empty() {
        a.en = fallback_en.to_string();
    }
    a
}

fn text_of(meta: &Value) -> Vec<String> {
    meta["text"].as_array().map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect()).unwrap_or_default()
}

/// Reads the optional design manifest seed (131-asset-manifest-seed.json): alt EN/FR, preview flag, max_kb.
pub fn load_seed(path: &Path) -> Vec<Value> {
    std::fs::read_to_string(path).ok().and_then(|t| serde_json::from_str::<Value>(&t).ok()).and_then(|v| v["assets"].as_array().cloned()).unwrap_or_default()
}

fn seed_for<'a>(seed: &'a [Value], id: &str) -> Option<&'a Value> {
    let base = id.trim_end_matches(".fr");
    seed.iter().find(|s| {
        let f = s["file"].as_str().unwrap_or("");
        let st = Path::new(f).file_stem().map(|x| x.to_string_lossy().to_string()).unwrap_or_default();
        let st = st.trim_end_matches(".fr");
        st == base || st.ends_with(&format!("-{base}"))
    })
}

pub fn run(assets: &Path, seed: &[Value]) -> Manifest {
    let opt = assets.join("optimized");
    let _ = std::fs::remove_dir_all(&opt);
    let mut out: Vec<Asset> = Vec::new();
    for (dir, kind) in SOURCES {
        let d = assets.join(dir);
        let files = files_in(&d);
        // group by id (stem, minus @2x)
        let mut groups: BTreeMap<String, Vec<PathBuf>> = BTreeMap::new();
        for f in files {
            let name = f.file_name().unwrap().to_string_lossy().to_string();
            if name.starts_with('_') || name.ends_with(".meta.json") || name.starts_with('.') {
                continue;
            }
            let ext = f.extension().map(|e| e.to_string_lossy().to_lowercase()).unwrap_or_default();
            if ext != "svg" && ext != "png" {
                continue;
            }
            let stem = f.file_stem().unwrap().to_string_lossy().to_string();
            let id = stem.trim_end_matches("@2x").to_string();
            groups.entry(id).or_default().push(f);
        }
        for (id, srcs) in groups {
            let mut meta = read_meta(&d, &[&id, &format!("{id}@2x")]);
            let mut budget_bytes = None;
            if let Some(sd) = seed_for(seed, &id).filter(|_| kind != "social") {
                if meta.is_null() {
                    meta = serde_json::json!({});
                }
                if meta["alt"].is_null() {
                    meta["alt"] = sd["alt"].clone();
                }
                if meta["status"].is_null() {
                    meta["status"] = Value::from(if sd["preview"].as_bool().unwrap_or(false) { "preview" } else { "delivered" });
                }
                budget_bytes = sd["max_kb"].as_f64().filter(|k| *k > 0.0).map(|k| (k * 1024.0).round() as u64);
            }
            let mut asset = Asset {
                id: format!("{dir}/{id}"),
                kind: kind.into(),
                status: status_of(&meta),
                alt: Alt::default(),
                text: text_of(&meta),
                files: vec![],
                srcset: BTreeMap::new(),
                budget_bytes,
            };
            let mut title = String::new();
            for f in &srcs {
                let ext = f.extension().unwrap().to_string_lossy().to_lowercase();
                let bytes = std::fs::read(f).unwrap();
                let rel_src = format!("{dir}/{}", f.file_name().unwrap().to_string_lossy());
                if ext == "svg" {
                    let src = String::from_utf8_lossy(&bytes).to_string();
                    title = svg_title(&src);
                    if kind == "social" {
                        let (w, h) = svg_dims(&src).unwrap_or((0, 0));
                        asset.files.push(entry(&rel_src, "svg", w, h, &bytes, "source"));
                        continue;
                    }
                    let mut min = minify_svg(&src);
                    if svg_dims(&min).is_none() || roxmltree::Document::parse(&min).is_err() {
                        eprintln!("  warn {rel_src}: minified SVG did not parse, keeping the original");
                        min = src.clone();
                    }
                    let (w, h) = svg_dims(&min).unwrap_or((0, 0));
                    let rel = format!("optimized/{dir}/{id}.svg");
                    write(&assets.join(&rel), min.as_bytes());
                    println!("  {rel_src}: {} -> {} bytes", bytes.len(), min.len());
                    asset.files.push(entry(&rel, "svg", w, h, min.as_bytes(), "optimized"));
                } else if kind == "social" {
                    let (w, h) = png_dims(&bytes).unwrap_or((0, 0));
                    asset.files.push(entry(&rel_src, "png", w, h, &bytes, "source"));
                } else {
                    let is2x = f.file_stem().unwrap().to_string_lossy().ends_with("@2x");
                    let (rgba, w, h) = decode_png(&bytes);
                    let base_w = if is2x { w / 2 } else { w };
                    let mut widths: Vec<u32> = WIDTHS.iter().copied().filter(|x| *x < base_w).collect();
                    widths.push(base_w);
                    if is2x {
                        widths.push(w);
                    }
                    widths.dedup();
                    for tw in widths {
                        let (px, pw, ph) = if tw == w { (rgba.clone(), w, h) } else { downscale(&rgba, w, h, tw) };
                        let png = encode_png(&px, pw, ph);
                        let rel = format!("optimized/{dir}/{id}-{pw}w.png");
                        write(&assets.join(&rel), &png);
                        asset.files.push(entry(&rel, "png", pw, ph, &png, "variant"));
                        let mut line = format!("  {rel}: {} bytes", png.len());
                        if let Some(wp) = webp_lossless(&px, pw, ph) {
                            let relw = format!("optimized/{dir}/{id}-{pw}w.webp");
                            // keep WebP only when it actually wins
                            if wp.len() < png.len() {
                                write(&assets.join(&relw), &wp);
                                asset.files.push(entry(&relw, "webp", pw, ph, &wp, "variant"));
                                line += &format!(" / webp {}", wp.len());
                            }
                        }
                        println!("{line}");
                    }
                }
            }
            asset.alt = alt_of(&meta, &title);
            // srcset per format
            for fmt in ["png", "webp"] {
                let mut v: Vec<&FileEntry> = asset.files.iter().filter(|f| f.format == fmt && f.role == "variant").collect();
                v.sort_by_key(|f| f.width);
                if !v.is_empty() {
                    asset.srcset.insert(fmt.into(), v.iter().map(|f| format!("{} {}w", f.path, f.width)).collect::<Vec<_>>().join(", "));
                }
            }
            out.push(asset);
        }
    }
    let m = Manifest { schema: 1, generated_by: "tools/images (cargo run --release -- process)".into(), assets: out };
    write(&assets.join("manifest.json"), (serde_json::to_string_pretty(&m).unwrap() + "\n").as_bytes());
    println!("  assets/manifest.json: {} assets", m.assets.len());
    m
}
