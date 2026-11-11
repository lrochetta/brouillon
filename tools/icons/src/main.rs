//! Regenerates every raster brand asset from the two SVG sources.
//!
//!   cargo run --release --manifest-path tools/icons/Cargo.toml
//!
//! Reads  assets/brand/brouillon-icon.svg        (sizes >= 40 px)
//!        assets/brand/brouillon-icon-small.svg  (sizes <= 32 px)
//! Writes assets/brand/{brouillon.ico, brouillon-<n>.png, brouillon.iconset/}
//!        and the raw 32x32 RGBA used for the window icon.
//! With ICON_PREVIEW_DIR set, also writes an 8x nearest-neighbour review sheet there.
use resvg::{tiny_skia, usvg};
use std::{fs, path::{Path, PathBuf}};

/// Sizes at or below this use the small (thicker) variant.
const SMALL_MAX: u32 = 32;
const ICO_SIZES: [u32; 8] = [16, 20, 24, 32, 40, 48, 64, 256];
const PNG_SIZES: [u32; 8] = [16, 32, 48, 64, 128, 256, 512, 1024];
/// Raw RGBA window icon (embedded by crates/brouillon with include_bytes!).
const WINDOW_ICON_SIZE: u32 = 32;

fn render(tree: &usvg::Tree, px: u32) -> tiny_skia::Pixmap {
    let mut pm = tiny_skia::Pixmap::new(px, px).unwrap();
    let s = px as f32 / tree.size().width();
    resvg::render(tree, tiny_skia::Transform::from_scale(s, s), &mut pm.as_mut());
    pm
}

fn rgba(pm: &tiny_skia::Pixmap) -> Vec<u8> {
    // tiny-skia stores premultiplied RGBA; PNG/ICO want straight alpha.
    pm.pixels().iter().flat_map(|p| { let c = p.demultiply(); [c.red(), c.green(), c.blue(), c.alpha()] }).collect()
}

/// 32-bit BMP (DIB) ICO entry payload: BITMAPINFOHEADER + BGRA bottom-up + AND mask.
fn dib(pm: &tiny_skia::Pixmap) -> Vec<u8> {
    let (w, h) = (pm.width() as usize, pm.height() as usize);
    let px = rgba(pm);
    let mut v = Vec::new();
    for x in [40u32, w as u32, (h * 2) as u32] { v.extend(x.to_le_bytes()); }
    v.extend(1u16.to_le_bytes());
    v.extend(32u16.to_le_bytes());
    for x in [0u32, (w * h * 4) as u32, 0, 0, 0, 0] { v.extend(x.to_le_bytes()); }
    for y in (0..h).rev() {
        for x in 0..w { let i = (y * w + x) * 4; v.extend([px[i + 2], px[i + 1], px[i], px[i + 3]]); }
    }
    let row = w.div_ceil(32) * 4;
    v.extend(std::iter::repeat_n(0u8, row * h)); // AND mask all 0: the alpha channel rules
    v
}

fn ico(entries: &[(u32, Vec<u8>)]) -> Vec<u8> {
    let mut out = vec![0, 0, 1, 0];
    out.extend((entries.len() as u16).to_le_bytes());
    let mut off = 6 + 16 * entries.len() as u32;
    for (px, data) in entries {
        let b = if *px >= 256 { 0 } else { *px as u8 };
        out.extend([b, b, 0, 0]);
        out.extend(1u16.to_le_bytes());
        out.extend(32u16.to_le_bytes());
        out.extend((data.len() as u32).to_le_bytes());
        out.extend(off.to_le_bytes());
        off += data.len() as u32;
    }
    for (_, d) in entries { out.extend(d); }
    out
}

fn main() {
    let root: PathBuf = std::env::args().nth(1).map(PathBuf::from)
        .unwrap_or_else(|| Path::new(env!("CARGO_MANIFEST_DIR")).join("../..").canonicalize().unwrap());
    let brand = root.join("assets/brand");
    let load = |n: &str| usvg::Tree::from_str(&fs::read_to_string(brand.join(n)).unwrap(), &usvg::Options::default()).unwrap();
    let (big, small) = (load("brouillon-icon.svg"), load("brouillon-icon-small.svg"));
    let pick = |px: u32| render(if px <= SMALL_MAX { &small } else { &big }, px);

    let entries: Vec<(u32, Vec<u8>)> = ICO_SIZES.iter()
        .map(|&px| { let pm = pick(px); (px, if px >= 256 { pm.encode_png().unwrap() } else { dib(&pm) }) })
        .collect();
    fs::write(brand.join("brouillon.ico"), ico(&entries)).unwrap();

    for px in PNG_SIZES { pick(px).save_png(brand.join(format!("brouillon-{px}.png"))).unwrap(); }

    let set = brand.join("brouillon.iconset");
    fs::create_dir_all(&set).unwrap();
    for base in [16u32, 32, 128, 256, 512] {
        pick(base).save_png(set.join(format!("icon_{base}x{base}.png"))).unwrap();
        pick(base * 2).save_png(set.join(format!("icon_{base}x{base}@2x.png"))).unwrap();
    }
    let win = root.join("_bmad-output/build/poc-floem/crates/brouillon/assets/window-icon-32.rgba");
    fs::write(win, rgba(&pick(WINDOW_ICON_SIZE))).unwrap();

    if let Some(dir) = std::env::var_os("ICON_PREVIEW_DIR") {
        preview(&PathBuf::from(dir), &big, &small);
    }
    println!("ok: {}", brand.display());
}

/// Rows: big then small variant; columns: 16, 20, 24, 32 px, each enlarged 8x without smoothing.
fn preview(dir: &Path, big: &usvg::Tree, small: &usvg::Tree) {
    fs::create_dir_all(dir).unwrap();
    let sizes = [16u32, 20, 24, 32];
    let (k, pad) = (8u32, 12u32);
    let w = sizes.iter().map(|s| s * k + pad).sum::<u32>() + pad;
    let h = 2 * (32 * k + pad) + pad;
    let mut sheet = tiny_skia::Pixmap::new(w, h).unwrap();
    sheet.fill(tiny_skia::Color::from_rgba8(235, 235, 235, 255));
    let stride = w as usize;
    let mut y0 = pad;
    for tree in [big, small] {
        let mut x0 = pad;
        for px in sizes {
            let pm = render(tree, px);
            let dst = sheet.pixels_mut();
            for yy in 0..px * k { for xx in 0..px * k {
                dst[(y0 + yy) as usize * stride + (x0 + xx) as usize] = pm.pixel(xx / k, yy / k).unwrap();
            } }
            x0 += px * k + pad;
        }
        y0 += 32 * k + pad;
    }
    sheet.save_png(dir.join("sheet-big-over-small-x8.png")).unwrap();
    for px in sizes {
        render(small, px).save_png(dir.join(format!("small-{px}.png"))).unwrap();
    }
}
