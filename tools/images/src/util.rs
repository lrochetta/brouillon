use sha2::{Digest, Sha256};
use std::path::Path;

pub fn esc(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}

pub fn sha8(b: &[u8]) -> String {
    let h = Sha256::digest(b);
    h.iter().take(4).map(|x| format!("{x:02x}")).collect()
}

/// Rasterises an SVG string (text must already be paths) to straight-alpha RGBA8.
pub fn svg_to_rgba(svg: &str, w: u32, h: u32) -> Vec<u8> {
    let tree = resvg::usvg::Tree::from_str(svg, &resvg::usvg::Options::default()).expect("svg parse");
    let mut pm = resvg::tiny_skia::Pixmap::new(w, h).unwrap();
    let s = w as f32 / tree.size().width();
    resvg::render(&tree, resvg::tiny_skia::Transform::from_scale(s, s), &mut pm.as_mut());
    pm.pixels()
        .iter()
        .flat_map(|p| {
            let c = p.demultiply();
            [c.red(), c.green(), c.blue(), c.alpha()]
        })
        .collect()
}

/// Opaque images are stored as RGB (smaller); anything with alpha stays RGBA.
pub fn encode_png(rgba: &[u8], w: u32, h: u32) -> Vec<u8> {
    let opaque = rgba.chunks_exact(4).all(|p| p[3] == 255);
    let mut out = Vec::new();
    {
        let mut enc = png::Encoder::new(&mut out, w, h);
        enc.set_depth(png::BitDepth::Eight);
        let data: Vec<u8>;
        if opaque {
            enc.set_color(png::ColorType::Rgb);
            data = rgba.chunks_exact(4).flat_map(|p| [p[0], p[1], p[2]]).collect();
        } else {
            enc.set_color(png::ColorType::Rgba);
            data = rgba.to_vec();
        }
        let mut wr = enc.write_header().unwrap();
        wr.write_image_data(&data).unwrap();
    }
    let mut opts = oxipng::Options::from_preset(4);
    opts.strip = oxipng::StripChunks::Safe;
    oxipng::optimize_from_memory(&out, &opts).unwrap_or(out)
}

pub fn write(path: &Path, bytes: &[u8]) {
    if let Some(p) = path.parent() {
        std::fs::create_dir_all(p).unwrap();
    }
    std::fs::write(path, bytes).unwrap_or_else(|e| panic!("write {}: {e}", path.display()));
}

/// Decodes a PNG to straight-alpha RGBA8.
pub fn decode_png(bytes: &[u8]) -> (Vec<u8>, u32, u32) {
    let mut dec = png::Decoder::new(std::io::Cursor::new(bytes));
    dec.set_transformations(png::Transformations::EXPAND | png::Transformations::STRIP_16);
    let mut r = dec.read_info().expect("png header");
    let mut buf = vec![0; r.output_buffer_size()];
    let info = r.next_frame(&mut buf).expect("png decode");
    let (w, h) = (info.width, info.height);
    let px = &buf[..info.buffer_size()];
    let rgba = match info.color_type {
        png::ColorType::Rgba => px.to_vec(),
        png::ColorType::Rgb => px.chunks_exact(3).flat_map(|p| [p[0], p[1], p[2], 255]).collect(),
        png::ColorType::Grayscale => px.iter().flat_map(|&g| [g, g, g, 255]).collect(),
        png::ColorType::GrayscaleAlpha => px.chunks_exact(2).flat_map(|p| [p[0], p[0], p[0], p[1]]).collect(),
        png::ColorType::Indexed => unreachable!("expanded"),
    };
    (rgba, w, h)
}

pub fn png_dims(bytes: &[u8]) -> Option<(u32, u32)> {
    if bytes.len() < 24 || &bytes[1..4] != b"PNG" {
        return None;
    }
    Some((u32::from_be_bytes(bytes[16..20].try_into().ok()?), u32::from_be_bytes(bytes[20..24].try_into().ok()?)))
}

/// Area-average downscale (premultiplied) to width `nw`, keeping aspect ratio.
pub fn downscale(rgba: &[u8], w: u32, h: u32, nw: u32) -> (Vec<u8>, u32, u32) {
    let nh = ((h as f64) * (nw as f64) / (w as f64)).round().max(1.0) as u32;
    let (sx, sy) = (w as f64 / nw as f64, h as f64 / nh as f64);
    let mut out = vec![0u8; (nw * nh * 4) as usize];
    for oy in 0..nh {
        let (y0, y1) = (oy as f64 * sy, (oy + 1) as f64 * sy);
        for ox in 0..nw {
            let (x0, x1) = (ox as f64 * sx, (ox + 1) as f64 * sx);
            let mut acc = [0f64; 4];
            let mut wsum = 0f64;
            let mut yy = y0.floor() as u32;
            while (yy as f64) < y1 && yy < h {
                let wy = ((yy + 1) as f64).min(y1) - (yy as f64).max(y0);
                let mut xx = x0.floor() as u32;
                while (xx as f64) < x1 && xx < w {
                    let wx = ((xx + 1) as f64).min(x1) - (xx as f64).max(x0);
                    let i = ((yy * w + xx) * 4) as usize;
                    let a = rgba[i + 3] as f64 / 255.0;
                    let ww = wx * wy;
                    acc[0] += rgba[i] as f64 * a * ww;
                    acc[1] += rgba[i + 1] as f64 * a * ww;
                    acc[2] += rgba[i + 2] as f64 * a * ww;
                    acc[3] += a * ww;
                    wsum += ww;
                    xx += 1;
                }
                yy += 1;
            }
            let o = ((oy * nw + ox) * 4) as usize;
            if acc[3] > 0.0 {
                out[o] = (acc[0] / acc[3]).round().min(255.0) as u8;
                out[o + 1] = (acc[1] / acc[3]).round().min(255.0) as u8;
                out[o + 2] = (acc[2] / acc[3]).round().min(255.0) as u8;
            }
            out[o + 3] = (acc[3] / wsum * 255.0).round().min(255.0) as u8;
        }
    }
    (out, nw, nh)
}

pub fn webp_lossless(rgba: &[u8], w: u32, h: u32) -> Option<Vec<u8>> {
    let mut out = Vec::new();
    image_webp::WebPEncoder::new(&mut out).encode(rgba, w, h, image_webp::ColorType::Rgba8).ok()?;
    Some(out)
}
