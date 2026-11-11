//! Share images (OG, GitHub social preview, README and release banners), drawn from the brand motif:
//! solid blue cursor + hollow violet ghost cursor on the dark editor background.
//! Every text is converted to paths, so the SVGs render identically anywhere.
use crate::text::{Fonts, Weight};
use crate::util::{encode_png, esc, svg_to_rgba, write};
use serde_json::json;
use std::path::Path;

pub struct Theme {
    pub bg: &'static str,
    pub text: &'static str,
    pub muted: &'static str,
    pub faint: &'static str,
    pub accent: &'static str,
    pub agent: &'static str,
}
pub const DARK: Theme = Theme { bg: "#14151c", text: "#c0caf5", muted: "#9aa3c7", faint: "#8a93b8", accent: "#7aa2f7", agent: "#bb9af7" };
pub const LIGHT: Theme = Theme { bg: "#fbfaf7", text: "#1a1b26", muted: "#4a4f6a", faint: "#5c6283", accent: "#2f4fd0", agent: "#7a3fd0" };

#[derive(Clone, Copy, PartialEq)]
enum Deco {
    Right,
    Left,
}

struct Item {
    text: String,
    w: Weight,
    size: f32,
    color: &'static str,
    gap: f32, // extra space above
}

struct Card<'a> {
    w: f32,
    h: f32,
    th: &'a Theme,
    margin: f32,
    deco: Deco,
    deco_scale: f32,
    items: Vec<Item>,
    brand_row: bool, // small logo + wordmark at top-left
    foot: Option<String>,
    title: String,
    desc: String,
}

fn mark(th: &Theme, x: f32, y: f32, s: f32) -> String {
    // brand logo geometry (32-unit viewBox): solid cursor 9..13 x 5..27, ghost 17.5..21 x 6.5..25.5 (stroke 2)
    format!(
        r#"<g transform="translate({} {}) scale({})"><rect x="9" y="5" width="4" height="22" rx="2" fill="{}"/><rect x="17.5" y="6.5" width="3.5" height="19" rx="1.75" fill="none" stroke="{}" stroke-width="2"/></g>"#,
        x - 9.0 * s,
        y - 5.0 * s,
        s,
        th.accent,
        th.agent
    )
}

fn tracking(size: f32) -> f32 {
    if size >= 60.0 {
        -0.02
    } else if size >= 36.0 {
        -0.01
    } else {
        0.0
    }
}

fn render(f: &Fonts, c: &Card) -> String {
    let (dw, dh) = (13.0 * c.deco_scale, 22.0 * c.deco_scale);
    let (deco_x, text_x, max_w) = match c.deco {
        Deco::Right => (c.w - c.margin - dw, c.margin, c.w - c.margin - dw - 64.0 - c.margin),
        Deco::Left => (c.margin, c.margin + dw + 56.0, c.w - c.margin * 2.0 - dw - 56.0),
    };
    let deco_y = (c.h - dh) / 2.0;
    let mut s = format!(
        r#"<svg xmlns="http://www.w3.org/2000/svg" width="{w}" height="{h}" viewBox="0 0 {w} {h}" role="img"><title>{t}</title><desc>{d}</desc><rect width="{w}" height="{h}" fill="{bg}"/>"#,
        w = c.w,
        h = c.h,
        t = esc(&c.title),
        d = esc(&c.desc),
        bg = c.th.bg
    );
    s.push_str(&mark(c.th, deco_x, deco_y, c.deco_scale));
    let mut top = c.margin;
    if c.brand_row {
        let ms = 1.5;
        s.push_str(&mark(c.th, text_x, top, ms));
        let size = 34.0;
        let base = top + 11.0 * ms + size * 0.36;
        let wm = f.path(Weight::SemiBold, "Brouillon", size, text_x + 13.0 * ms + 14.0, base, 0.0);
        s.push_str(&format!(r#"<path d="{wm}" fill="{}"/>"#, c.th.text));
        top += 22.0 * ms;
    }
    // vertical centring of the text block between header and footer
    let foot_h = if c.foot.is_some() { 60.0 } else { 0.0 };
    let mut hh = 0.0;
    for (i, it) in c.items.iter().enumerate() {
        hh += if i == 0 { it.size * 0.8 } else { it.size * 1.2 + it.gap };
    }
    let area_top = top + 36.0;
    let area_bot = c.h - c.margin - foot_h;
    let mut y = area_top + ((area_bot - area_top - hh) / 2.0).max(0.0);
    for (i, it) in c.items.iter().enumerate() {
        y += if i == 0 { it.size * 0.8 } else { it.size * 1.2 + it.gap };
        let wd = f.width(it.w, &it.text, it.size, tracking(it.size));
        assert!(wd <= max_w, "text overflows ({wd:.0} > {max_w:.0}): {}", it.text);
        let d = f.path(it.w, &it.text, it.size, text_x, y, tracking(it.size));
        s.push_str(&format!(r#"<path d="{d}" fill="{}"/>"#, it.color));
    }
    if let Some(ft) = &c.foot {
        let d = f.path(Weight::Mono, ft, 24.0, text_x, c.h - c.margin + 2.0, 0.0);
        s.push_str(&format!(r#"<path d="{d}" fill="{}"/>"#, c.th.faint));
    }
    s.push_str("</svg>\n");
    s
}

fn it(th: &Theme, kind: &str, text: &str, size: f32, gap: f32) -> Item {
    let (w, color) = match kind {
        "eyebrow" => (Weight::SemiBold, th.accent),
        "title" => (Weight::Bold, th.text),
        _ => (Weight::Regular, th.muted),
    };
    Item { text: text.into(), w, size, color, gap }
}

struct Page {
    key: &'static str,
    lang: &'static str,
    eyebrow: &'static str,
    title: Vec<&'static str>,
    sub: Vec<&'static str>,
    alt: &'static str,
}

fn pages() -> Vec<Page> {
    vec![
        Page { key: "home", lang: "en", eyebrow: "Free, open source, Rust", title: vec!["A fast, local-first", "text editor."], sub: vec!["MIT licence. Windows alpha test build."],
            alt: "Brouillon: a fast, local-first text editor. Free, open source (MIT), written in Rust. Windows alpha." },
        Page { key: "home", lang: "fr", eyebrow: "Gratuit, open source, Rust", title: vec!["Un éditeur de texte", "rapide, local-first."], sub: vec!["Licence MIT. Build de test alpha pour Windows."],
            alt: "Brouillon : un éditeur de texte rapide et local-first. Gratuit, open source (MIT), écrit en Rust. Alpha pour Windows." },
        Page { key: "download", lang: "en", eyebrow: "Alpha, Windows x64", title: vec!["Download", "the alpha."], sub: vec!["Unsigned test build.", "SHA-256 for every file."],
            alt: "Download the Brouillon alpha for Windows x64: unsigned test build, SHA-256 checksum for every file." },
        Page { key: "download", lang: "fr", eyebrow: "Alpha, Windows x64", title: vec!["Télécharger", "l’alpha."], sub: vec!["Build de test non signé.", "SHA-256 de chaque fichier."],
            alt: "Télécharger l’alpha de Brouillon pour Windows x64 : build de test non signé, SHA-256 de chaque fichier." },
        Page { key: "privacy", lang: "en", eyebrow: "Privacy", title: vec!["It does not", "phone home."], sub: vec!["No account, no telemetry in the editor.", "No cookies on this site."],
            alt: "Brouillon privacy: no account, no telemetry in the editor, no cookies on this website." },
        Page { key: "privacy", lang: "fr", eyebrow: "Confidentialité", title: vec!["Il n’appelle", "personne."], sub: vec!["Sans compte, sans télémétrie dans l’éditeur.", "Sans cookie sur ce site."],
            alt: "Confidentialité de Brouillon : sans compte, sans télémétrie dans l’éditeur, sans cookie sur ce site." },
        Page { key: "changelog", lang: "en", eyebrow: "Changelog", title: vec!["Release notes,", "version by version."], sub: vec!["With build number and UTC date."],
            alt: "Brouillon changelog: release notes for every version, with build number and UTC date." },
        Page { key: "changelog", lang: "fr", eyebrow: "Journal des modifications", title: vec!["Les notes de version,", "version par version."], sub: vec!["Avec numéro de build et date UTC."],
            alt: "Journal des modifications de Brouillon : notes de chaque version, avec numéro de build et date UTC." },
    ]
}

fn texts(items: &[Item], extra: &[&str]) -> Vec<String> {
    let mut v: Vec<String> = items.iter().map(|i| i.text.clone()).collect();
    v.extend(extra.iter().map(|s| s.to_string()));
    v
}

pub fn run(assets: &Path, fonts_dir: &Path, release_version: &str) {
    let f = Fonts::load(fonts_dir);
    let out = assets.join("social");
    let mut meta = serde_json::Map::new();
    let th = &DARK;
    let save_png = |name: &str, svg: &str, w: u32, h: u32| {
        let png = encode_png(&svg_to_rgba(svg, w, h), w, h);
        write(&out.join(name), &png);
        println!("  social/{name}  {w}x{h}  {} bytes", png.len());
    };

    // 1. Open Graph, 1200x630, per page and language
    for p in pages() {
        let mut items = vec![it(th, "eyebrow", p.eyebrow, 28.0, 0.0)];
        for (i, t) in p.title.iter().enumerate() {
            items.push(it(th, "title", t, 80.0, if i == 0 { 22.0 } else { 0.0 }));
        }
        for (i, t) in p.sub.iter().enumerate() {
            items.push(it(th, "sub", t, 32.0, if i == 0 { 26.0 } else { 0.0 }));
        }
        let t = texts(&items, &["Brouillon", "brouillon.rochetta.fr"]);
        let card = Card {
            w: 1200.0, h: 630.0, th, margin: 80.0, deco: Deco::Right, deco_scale: 13.0, items, brand_row: true,
            foot: Some("brouillon.rochetta.fr".into()), title: "Brouillon".into(), desc: p.alt.to_string(),
        };
        let svg = render(&f, &card);
        let name = format!("og-{}-{}.png", p.key, p.lang);
        save_png(&name, &svg, 1200, 630);
        let alt = json!({ p.lang: p.alt });
        meta.insert(name.trim_end_matches(".png").into(), json!({ "alt": alt, "status": "delivered", "text": t }));
    }

    // 2. GitHub social preview, 1280x640 (40 px safe zone: content kept 100 px from the edges)
    {
        let items = vec![
            it(th, "title", "Brouillon", 150.0, 0.0),
            it(th, "sub", "A fast, local-first text editor.", 54.0, 18.0),
            it(th, "sub", "Free, open source (MIT), written in Rust.", 36.0, 20.0),
        ];
        let t = texts(&items, &["github.com/lrochetta/brouillon"]);
        let card = Card {
            w: 1280.0, h: 640.0, th, margin: 100.0, deco: Deco::Right, deco_scale: 12.0, items, brand_row: false,
            foot: Some("github.com/lrochetta/brouillon".into()), title: "Brouillon".into(),
            desc: "Brouillon, a fast, local-first text editor. Free, open source (MIT), written in Rust.".into(),
        };
        save_png("github-social-preview.png", &render(&f, &card), 1280, 640);
        meta.insert("github-social-preview".into(), json!({ "alt": {"en": "Brouillon, a fast, local-first text editor. Free, open source (MIT), written in Rust.", "fr": "Brouillon, un éditeur de texte rapide et local-first. Gratuit, open source (MIT), écrit en Rust."}, "status": "delivered", "text": t }));
    }

    // 3. README banner, 1280x320, dark and light
    for (suffix, th) in [("dark", &DARK), ("light", &LIGHT)] {
        let items = vec![
            it(th, "title", "Brouillon", 96.0, 0.0),
            it(th, "sub", "A fast, local-first text editor.", 38.0, 12.0),
            it(th, "sub", "Free, open source (MIT). Rust. Windows alpha.", 26.0, 14.0),
        ];
        let t = texts(&items, &[]);
        let card = Card {
            w: 1280.0, h: 320.0, th, margin: 64.0, deco: Deco::Left, deco_scale: 8.0, items, brand_row: false, foot: None,
            title: "Brouillon".into(), desc: "Brouillon, a fast, local-first text editor. Free, open source (MIT), written in Rust.".into(),
        };
        let svg = render(&f, &card);
        write(&out.join(format!("readme-banner-{suffix}.svg")), svg.as_bytes());
        save_png(&format!("readme-banner-{suffix}.png"), &svg, 1280, 320);
        if suffix == "dark" {
            write(&out.join("readme-banner.svg"), svg.as_bytes());
            save_png("readme-banner.png", &svg, 1280, 320);
        }
        let alt = json!({"en": "Brouillon, a fast, local-first text editor. Free, open source (MIT), written in Rust.", "fr": "Brouillon, un éditeur de texte rapide et local-first. Gratuit, open source (MIT), écrit en Rust."});
        let names: Vec<&str> = if suffix == "dark" { vec!["readme-banner-dark", "readme-banner"] } else { vec!["readme-banner-light"] };
        for n in names {
            meta.insert(n.into(), json!({ "alt": alt, "status": "delivered", "text": t }));
        }
    }

    // 4. Release banner, 1200x600 (English: release notes are published in English)
    {
        let title = format!("Brouillon {release_version}");
        let items = vec![
            it(th, "eyebrow", "Release notes", 28.0, 0.0),
            it(th, "title", &title, 76.0, 22.0),
            it(th, "sub", "Unsigned alpha test build for Windows x64.", 32.0, 24.0),
            it(th, "sub", "Back up your files.", 32.0, 0.0),
        ];
        let t = texts(&items, &["Brouillon", "brouillon.rochetta.fr"]);
        let desc = format!("Brouillon {release_version} release notes: unsigned alpha test build for Windows x64. Back up your files.");
        let card = Card {
            w: 1200.0, h: 600.0, th, margin: 80.0, deco: Deco::Right, deco_scale: 12.0, items, brand_row: true,
            foot: Some("brouillon.rochetta.fr".into()), title: title.clone(), desc: desc.clone(),
        };
        save_png("release-banner.png", &render(&f, &card), 1200, 600);
        let fr = format!("Notes de version de Brouillon {release_version} : build de test alpha non signé pour Windows x64. Sauvegardez vos fichiers.");
        meta.insert("release-banner".into(), json!({ "alt": {"en": desc, "fr": fr}, "status": "delivered", "text": t }));
    }

    // fill the missing language of each OG alt from the sibling page
    let keys: Vec<String> = meta.keys().cloned().collect();
    for k in keys.iter().filter(|k| k.starts_with("og-")) {
        let sib = if k.ends_with("-en") { k.replace("-en", "-fr") } else { k.replace("-fr", "-en") };
        let mut m = meta[k]["alt"].as_object().unwrap().clone();
        for (kk, vv) in meta[&sib]["alt"].as_object().unwrap() {
            m.insert(kk.clone(), vv.clone());
        }
        meta.get_mut(k).unwrap()["alt"] = serde_json::Value::Object(m);
    }
    write(&out.join("_meta.json"), serde_json::to_string_pretty(&serde_json::Value::Object(meta)).unwrap().as_bytes());
}
