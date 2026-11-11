//! Text to SVG path outlines (no font needed at render time, so the SVGs look the same on GitHub).
use ttf_parser::{Face, OutlineBuilder};

#[derive(Clone, Copy)]
pub enum Weight {
    Regular,
    SemiBold,
    Bold,
    Mono,
}

pub struct Fonts {
    regular: Vec<u8>,
    semibold: Vec<u8>,
    bold: Vec<u8>,
    mono: Vec<u8>,
}

impl Fonts {
    pub fn load(dir: &std::path::Path) -> Fonts {
        let rd = |n: &str| std::fs::read(dir.join(n)).unwrap_or_else(|e| panic!("font {n}: {e}"));
        Fonts {
            regular: rd("Inter-Regular.ttf"),
            semibold: rd("Inter-SemiBold.ttf"),
            bold: rd("Inter-Bold.ttf"),
            mono: rd("JetBrainsMono-Regular.ttf"),
        }
    }
    fn face(&self, w: Weight) -> Face<'_> {
        let d = match w {
            Weight::Regular => &self.regular,
            Weight::SemiBold => &self.semibold,
            Weight::Bold => &self.bold,
            Weight::Mono => &self.mono,
        };
        Face::parse(d, 0).expect("font parse")
    }
    /// Advance width of `text` in px.
    pub fn width(&self, w: Weight, text: &str, size: f32, tracking: f32) -> f32 {
        let f = self.face(w);
        let k = size / f.units_per_em() as f32;
        text.chars().map(|c| self.adv(&f, c) * k + tracking * size).sum()
    }
    fn adv(&self, f: &Face, c: char) -> f32 {
        let g = f.glyph_index(c).unwrap_or_else(|| panic!("glyph missing for {c:?}"));
        f.glyph_hor_advance(g).unwrap_or(0) as f32
    }
    /// SVG path data for `text` with baseline origin (x, y).
    pub fn path(&self, w: Weight, text: &str, size: f32, x: f32, y: f32, tracking: f32) -> String {
        let f = self.face(w);
        let k = size / f.units_per_em() as f32;
        let mut cx = x;
        let mut out = String::new();
        for c in text.chars() {
            let g = f.glyph_index(c).unwrap_or_else(|| panic!("glyph missing for {c:?}"));
            let mut b = B { d: String::new(), k, ox: cx, oy: y };
            f.outline_glyph(g, &mut b);
            out.push_str(&b.d);
            cx += self.adv(&f, c) * k + tracking * size;
        }
        out
    }
}

fn n(v: f32) -> String {
    let s = format!("{v:.1}");
    let s = s.trim_end_matches('0').trim_end_matches('.');
    if s.is_empty() || s == "-0" { "0".into() } else { s.into() }
}

struct B {
    d: String,
    k: f32,
    ox: f32,
    oy: f32,
}
impl B {
    fn p(&self, x: f32, y: f32) -> String {
        format!("{} {}", n(self.ox + x * self.k), n(self.oy - y * self.k))
    }
}
impl OutlineBuilder for B {
    fn move_to(&mut self, x: f32, y: f32) {
        let s = format!("M{}", self.p(x, y));
        self.d.push_str(&s);
    }
    fn line_to(&mut self, x: f32, y: f32) {
        let s = format!("L{}", self.p(x, y));
        self.d.push_str(&s);
    }
    fn quad_to(&mut self, x1: f32, y1: f32, x: f32, y: f32) {
        let s = format!("Q{} {}", self.p(x1, y1), self.p(x, y));
        self.d.push_str(&s);
    }
    fn curve_to(&mut self, x1: f32, y1: f32, x2: f32, y2: f32, x: f32, y: f32) {
        let s = format!("C{} {} {}", self.p(x1, y1), self.p(x2, y2), self.p(x, y));
        self.d.push_str(&s);
    }
    fn close(&mut self) {
        self.d.push('Z');
    }
}
