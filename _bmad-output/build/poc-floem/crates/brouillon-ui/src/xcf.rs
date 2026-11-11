//! GIMP XCF v000 Layered Image Exporter.
//!
//! Rasterizes vector drawing strokes and exports them into standard GIMP XCF format,
//! letting users open their Brouillon drawings directly inside GIMP as layered graphics.

use std::fs::File;
use std::io::{Write, Seek, SeekFrom};
use std::path::Path;
use brouillon_editor_engine::sketch::{SketchDocument, StrokePoint, SketchTool};

/// Bounding dimensions of the Brouillon page canvas.
const CANVAS_WIDTH: usize = 800;
const CANVAS_HEIGHT: usize = 1130;
const TILE_SIZE: usize = 64;

/// Rasterizes drawing vector paths and exports them into a native GIMP .xcf file.
pub fn export_to_xcf(path: &Path, doc: &SketchDocument) -> Result<(), String> {
    let mut file = File::create(path).map_err(|e| format!("Failed to create XCF: {}", e))?;
    let landscape = doc.active_page().landscape;
    let canvas_width = if landscape { CANVAS_HEIGHT } else { CANVAS_WIDTH };
    let canvas_height = if landscape { CANVAS_WIDTH } else { CANVAS_HEIGHT };

    // 1. Allocate main canvas RGBA pixel buffer
    let mut pixels = vec![0u8; canvas_width * canvas_height * 4];

    // 2. Rasterize vector strokes into the buffer
    for stroke in &doc.active_page().strokes {
        if stroke.points.is_empty() { continue; }
        
        let color = if stroke.is_eraser {
            (0, 0, 0, 0)
        } else {
            let (r, g, b) = stroke.color.rgb();
            let alpha = stroke.color.alpha(stroke.tool);
            (r, g, b, (alpha * 255.0) as u8)
        };

        // Helper to calculate brush width
        let get_width = |pt: &StrokePoint| -> f64 {
            match stroke.tool {
                SketchTool::Fineliner | SketchTool::MechanicalPencil => stroke.width as f64,
                SketchTool::Ballpoint => (stroke.width * (0.4 + 0.6 * pt.pressure)) as f64,
                SketchTool::Marker | SketchTool::Paintbrush => (stroke.width * (0.3 + 0.7 * pt.pressure)) as f64,
                SketchTool::Pencil => {
                    let tilt_factor = (1.0 - (pt.tilt_x.abs() + pt.tilt_y.abs()).min(0.8) as f32).max(0.4);
                    (stroke.width * (0.3 + 0.7 * pt.pressure) * tilt_factor) as f64
                }
                SketchTool::Calligraphy => {
                    let tilt_influence = (pt.tilt_x - pt.tilt_y).abs() as f32;
                    (stroke.width * (0.5 + 0.5 * pt.pressure) * (1.0 + tilt_influence)) as f64
                }
                SketchTool::Highlighter => stroke.width as f64,
                SketchTool::Shader => (stroke.width * 2.0) as f64,
                SketchTool::Eraser | SketchTool::LassoSelect => stroke.width as f64,
            }
        };

        // Draw circles at all points (joint caps)
        for pt in &stroke.points {
            let w = get_width(pt);
            let radius = w / 2.0;
            let cx = pt.x as f64;
            let cy = pt.y as f64;
            
            let min_x = (cx - radius).max(0.0).floor() as usize;
            let max_x = (cx + radius).min((canvas_width - 1) as f64).ceil() as usize;
            let min_y = (cy - radius).max(0.0).floor() as usize;
            let max_y = (cy + radius).min((canvas_height - 1) as f64).ceil() as usize;

            for py in min_y..=max_y {
                for px in min_x..=max_x {
                    let dx = px as f64 + 0.5 - cx;
                    let dy = py as f64 + 0.5 - cy;
                    if dx * dx + dy * dy <= radius * radius {
                        let idx = (py * canvas_width + px) * 4;
                        if idx + 3 < pixels.len() {
                            pixels[idx] = color.0;
                            pixels[idx + 1] = color.1;
                            pixels[idx + 2] = color.2;
                            pixels[idx + 3] = color.3;
                        }
                    }
                }
            }
        }

        // Draw connecting line segments
        if stroke.points.len() > 1 {
            for i in 0..stroke.points.len() - 1 {
                let pt0 = &stroke.points[i];
                let pt1 = &stroke.points[i+1];
                let w0 = get_width(pt0);
                let w1 = get_width(pt1);
                let w = (w0 + w1) / 2.0;
                
                draw_line_segment(&mut pixels, canvas_width, canvas_height, pt0.x as f64, pt0.y as f64, pt1.x as f64, pt1.y as f64, w, color);
            }
        }
    }

    // 3. Write XCF Header
    file.write_all(b"gimp xcf v000\0").unwrap();
    file.write_all(&(canvas_width as u32).to_be_bytes()).unwrap();
    file.write_all(&(canvas_height as u32).to_be_bytes()).unwrap();
    file.write_all(&0u32.to_be_bytes()).unwrap(); // Base Type: RGB

    // 4. Image Properties (PROP_END)
    file.write_all(&0u32.to_be_bytes()).unwrap(); // Type: PROP_END
    file.write_all(&0u32.to_be_bytes()).unwrap(); // Length: 0

    // 5. Layer Offsets (1 layer, 1 pointer, terminated by 0)
    let layer_pointer_offset = file.seek(SeekFrom::Current(0)).unwrap();
    file.write_all(&0u32.to_be_bytes()).unwrap(); // Placeholder offset_layer_0
    file.write_all(&0u32.to_be_bytes()).unwrap(); // List terminator

    // 6. Channel Offsets (terminated by 0)
    file.write_all(&0u32.to_be_bytes()).unwrap();

    // 7. Write Layer Definition
    let offset_layer_0 = file.seek(SeekFrom::Current(0)).unwrap() as u32;
    file.write_all(&(canvas_width as u32).to_be_bytes()).unwrap();
    file.write_all(&(canvas_height as u32).to_be_bytes()).unwrap();
    file.write_all(&0u32.to_be_bytes()).unwrap(); // Layer Type: RGB with Alpha

    let name = "Dessin Crayon\0";
    file.write_all(&(name.len() as u32).to_be_bytes()).unwrap();
    file.write_all(name.as_bytes()).unwrap();

    // Layer Properties (PROP_END)
    file.write_all(&0u32.to_be_bytes()).unwrap();
    file.write_all(&0u32.to_be_bytes()).unwrap();

    let hierarchy_pointer_offset = file.seek(SeekFrom::Current(0)).unwrap();
    file.write_all(&0u32.to_be_bytes()).unwrap(); // Placeholder hierarchy_pointer
    file.write_all(&0u32.to_be_bytes()).unwrap(); // Mask pointer: 0

    // 8. Write Hierarchy
    let hierarchy_pointer = file.seek(SeekFrom::Current(0)).unwrap() as u32;
    file.write_all(&(canvas_width as u32).to_be_bytes()).unwrap();
    file.write_all(&(canvas_height as u32).to_be_bytes()).unwrap();
    file.write_all(&4u32.to_be_bytes()).unwrap(); // BPP: 4 (RGBA)

    let level_pointer_offset = file.seek(SeekFrom::Current(0)).unwrap();
    file.write_all(&0u32.to_be_bytes()).unwrap(); // Placeholder level_pointer

    // 9. Write Level 0
    let level_pointer = file.seek(SeekFrom::Current(0)).unwrap() as u32;
    file.write_all(&(canvas_width as u32).to_be_bytes()).unwrap();
    file.write_all(&(canvas_height as u32).to_be_bytes()).unwrap();

    // Compute tile count & offsets placeholders
    let cols = (canvas_width + TILE_SIZE - 1) / TILE_SIZE;
    let rows = (canvas_height + TILE_SIZE - 1) / TILE_SIZE;
    let tile_count = cols * rows;

    let tile_pointers_offset = file.seek(SeekFrom::Current(0)).unwrap();
    for _ in 0..tile_count {
        file.write_all(&0u32.to_be_bytes()).unwrap(); // Placeholder tile pointers
    }
    file.write_all(&0u32.to_be_bytes()).unwrap(); // List terminator

    // 10. Write Tiles Data & collect actual offsets
    let mut tile_offsets = Vec::with_capacity(tile_count);
    
    for ty in 0..rows {
        for tx in 0..cols {
            let start_x = tx * TILE_SIZE;
            let start_y = ty * TILE_SIZE;
            let end_x = (start_x + TILE_SIZE).min(canvas_width);
            let end_y = (start_y + TILE_SIZE).min(canvas_height);
            let tw = end_x - start_x;
            let th = end_y - start_y;

            let offset = file.seek(SeekFrom::Current(0)).unwrap() as u32;
            tile_offsets.push(offset);

            // Write raw RGBA tile bytes
            let mut tile_bytes = Vec::with_capacity(tw * th * 4);
            for y in start_y..end_y {
                for x in start_x..end_x {
                    let idx = (y * canvas_width + x) * 4;
                    tile_bytes.extend_from_slice(&pixels[idx..idx + 4]);
                }
            }
            file.write_all(&tile_bytes).unwrap();
        }
    }

    // 11. Patch Offset Placeholders
    
    // Layer Pointer in image header
    file.seek(SeekFrom::Start(layer_pointer_offset)).unwrap();
    file.write_all(&offset_layer_0.to_be_bytes()).unwrap();

    // Hierarchy Pointer in layer definition
    file.seek(SeekFrom::Start(hierarchy_pointer_offset)).unwrap();
    file.write_all(&hierarchy_pointer.to_be_bytes()).unwrap();

    // Level Pointer in hierarchy definition
    file.seek(SeekFrom::Start(level_pointer_offset)).unwrap();
    file.write_all(&level_pointer.to_be_bytes()).unwrap();

    // Tile Pointers in level definition
    file.seek(SeekFrom::Start(tile_pointers_offset)).unwrap();
    for offset in tile_offsets {
        file.write_all(&offset.to_be_bytes()).unwrap();
    }

    Ok(())
}

/// Helper function to rasterize a thick line segment into a 4bpp RGBA pixel buffer.
fn draw_line_segment(
    pixels: &mut [u8],
    canvas_width: usize,
    canvas_height: usize,
    x0: f64,
    y0: f64,
    x1: f64,
    y1: f64,
    brush_width: f64,
    color: (u8, u8, u8, u8),
) {
    let radius = brush_width / 2.0;
    
    // Calculate bounding box of the thick segment
    let min_x = (x0.min(x1) - radius).max(0.0).floor() as usize;
    let max_x = (x0.max(x1) + radius).min((canvas_width - 1) as f64).ceil() as usize;
    let min_y = (y0.min(y1) - radius).max(0.0).floor() as usize;
    let max_y = (y0.max(y1) + radius).min((canvas_height - 1) as f64).ceil() as usize;
    
    let dx = x1 - x0;
    let dy = y1 - y0;
    let len_sq = dx * dx + dy * dy;
    
    for py in min_y..=max_y {
        for px in min_x..=max_x {
            let cx = px as f64 + 0.5;
            let cy = py as f64 + 0.5;
            
            let t = if len_sq > 0.0 {
                (((cx - x0) * dx + (cy - y0) * dy) / len_sq).clamp(0.0, 1.0)
            } else {
                0.0
            };
            
            let proj_x = x0 + t * dx;
            let proj_y = y0 + t * dy;
            let dist_sq = (cx - proj_x).powi(2) + (cy - proj_y).powi(2);
            
            if dist_sq <= radius * radius {
                let idx = (py * canvas_width + px) * 4;
                if idx + 3 < pixels.len() {
                    pixels[idx] = color.0;
                    pixels[idx + 1] = color.1;
                    pixels[idx + 2] = color.2;
                    pixels[idx + 3] = color.3;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use brouillon_editor_engine::sketch::{Stroke, SketchColor, StrokePoint, SketchTool};

    fn test_stroke() -> Stroke {
        Stroke {
            points: vec![
                StrokePoint { x: 100.0, y: 100.0, pressure: 0.5, tilt_x: 0.0, tilt_y: 0.0 },
                StrokePoint { x: 200.0, y: 150.0, pressure: 0.8, tilt_x: 0.0, tilt_y: 0.0 },
            ],
            tool: SketchTool::Ballpoint,
            color: SketchColor::Blue,
            width: 3.5,
            layer_idx: 0,
            is_eraser: false,
        }
    }

    #[test]
    fn test_xcf_export_generates_valid_gimp_headers() {
        let temp_dir = std::env::temp_dir();
        let path = temp_dir.join("test_drawing.xcf");
        
        let mut doc = SketchDocument::new();
        doc.active_page_mut().strokes.push(test_stroke());

        let res = export_to_xcf(&path, &doc);
        assert!(res.is_ok(), "XCF export should succeed without errors");
        assert!(path.exists(), "Exported XCF file must exist on disk");

        let bytes = std::fs::read(&path).expect("Read exported XCF bytes");
        assert!(bytes.len() > 14);
        assert_eq!(&bytes[0..14], b"gimp xcf v000\0", "Magic bytes must conform to GIMP XCF format specification");

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn test_canvas_constants() {
        assert_eq!(CANVAS_WIDTH, 800);
        assert_eq!(CANVAS_HEIGHT, 1130);
        assert_eq!(TILE_SIZE, 64);
    }

    #[test]
    fn test_xcf_empty_doc() {
        let temp_dir = std::env::temp_dir();
        let path = temp_dir.join("test_xcf_empty.xcf");
        let doc = SketchDocument::new();
        let res = export_to_xcf(&path, &doc);
        assert!(res.is_ok());
        let bytes = std::fs::read(&path).unwrap();
        assert_eq!(&bytes[0..14], b"gimp xcf v000\0");
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn test_xcf_landscape_export() {
        let temp_dir = std::env::temp_dir();
        let path = temp_dir.join("test_xcf_landscape.xcf");
        let mut doc = SketchDocument::new();
        doc.active_page_mut().landscape = true;
        doc.active_page_mut().strokes.push(test_stroke());
        let res = export_to_xcf(&path, &doc);
        assert!(res.is_ok());
        
        let bytes = std::fs::read(&path).unwrap();
        // Width should be CANVAS_HEIGHT=1130 in landscape, height=800
        let w = u32::from_be_bytes([bytes[14], bytes[15], bytes[16], bytes[17]]);
        let h = u32::from_be_bytes([bytes[18], bytes[19], bytes[20], bytes[21]]);
        assert_eq!(w, 1130);
        assert_eq!(h, 800);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn test_xcf_portrait_dimensions() {
        let temp_dir = std::env::temp_dir();
        let path = temp_dir.join("test_xcf_portrait.xcf");
        let doc = SketchDocument::new();
        let res = export_to_xcf(&path, &doc);
        assert!(res.is_ok());
        
        let bytes = std::fs::read(&path).unwrap();
        let w = u32::from_be_bytes([bytes[14], bytes[15], bytes[16], bytes[17]]);
        let h = u32::from_be_bytes([bytes[18], bytes[19], bytes[20], bytes[21]]);
        assert_eq!(w, 800);
        assert_eq!(h, 1130);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn test_xcf_multi_stroke() {
        let temp_dir = std::env::temp_dir();
        let path = temp_dir.join("test_xcf_multi.xcf");
        let mut doc = SketchDocument::new();
        for _ in 0..5 {
            doc.active_page_mut().strokes.push(test_stroke());
        }
        let res = export_to_xcf(&path, &doc);
        assert!(res.is_ok());
        let bytes = std::fs::read(&path).unwrap();
        assert!(bytes.len() > 100, "Multi-stroke XCF should have substantial file size");
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn test_xcf_eraser_stroke() {
        let temp_dir = std::env::temp_dir();
        let path = temp_dir.join("test_xcf_eraser.xcf");
        let mut doc = SketchDocument::new();
        doc.active_page_mut().strokes.push(Stroke {
            points: vec![
                StrokePoint { x: 50.0, y: 50.0, pressure: 1.0, tilt_x: 0.0, tilt_y: 0.0 },
            ],
            tool: SketchTool::Eraser,
            color: SketchColor::Black,
            width: 10.0,
            layer_idx: 0,
            is_eraser: true,
        });
        let res = export_to_xcf(&path, &doc);
        assert!(res.is_ok());
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn test_xcf_single_point_stroke() {
        let temp_dir = std::env::temp_dir();
        let path = temp_dir.join("test_xcf_single.xcf");
        let mut doc = SketchDocument::new();
        doc.active_page_mut().strokes.push(Stroke {
            points: vec![
                StrokePoint { x: 400.0, y: 565.0, pressure: 0.5, tilt_x: 0.0, tilt_y: 0.0 },
            ],
            tool: SketchTool::Fineliner,
            color: SketchColor::Red,
            width: 1.0,
            layer_idx: 0,
            is_eraser: false,
        });
        let res = export_to_xcf(&path, &doc);
        assert!(res.is_ok());
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn test_draw_line_segment_basic() {
        let mut pixels = vec![0u8; 100 * 100 * 4];
        draw_line_segment(&mut pixels, 100, 100, 10.0, 10.0, 20.0, 10.0, 2.0, (255, 0, 0, 255));
        // Check that some pixels in the line area were written
        let y = 10;
        let x = 15;
        let idx = (y * 100 + x) * 4;
        assert_eq!(pixels[idx], 255, "Red channel should be set");
        assert_eq!(pixels[idx + 3], 255, "Alpha should be fully opaque");
    }

    #[test]
    fn test_tile_count_computation() {
        let cols = (CANVAS_WIDTH + TILE_SIZE - 1) / TILE_SIZE;
        let rows = (CANVAS_HEIGHT + TILE_SIZE - 1) / TILE_SIZE;
        // 800/64 = 12.5 -> 13 cols, 1130/64 = 17.65 -> 18 rows
        assert_eq!(cols, 13);
        assert_eq!(rows, 18);
        assert_eq!(cols * rows, 234);
    }
}

