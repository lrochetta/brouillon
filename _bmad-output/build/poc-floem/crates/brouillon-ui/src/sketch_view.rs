use std::time::Instant;
use std::sync::Arc;
use std::path::PathBuf;
use floem::context::{PaintCx, StyleCx};
use floem::event::{Event, EventListener};
use floem::kurbo::{Circle, Point, Rect, Stroke as KurboStroke};
use floem::peniko::Color;
use floem::reactive::{create_effect, RwSignal, SignalGet, SignalUpdate};
use floem::views::{scroll, stack, label, empty, Decorators};
use floem::{IntoView, View, ViewId};
use floem_renderer::Renderer;
use floem::style::CursorStyle;
use floem::ext_event::{create_ext_action, register_ext_trigger};
use floem::reactive::Trigger;

use brouillon_editor_engine::sketch::{SketchColor, SketchDocument, SketchTool, Stroke, StrokePoint};
use brouillon_core::theme::current_theme;
use brouillon_editor_engine::tabs::TabManager;
use crate::ai_extract;
use crate::export;

/// Custom reactive view implementing the GPU Vello drawing canvas.
pub struct SketchCanvas {
    id: ViewId,
    document: RwSignal<SketchDocument>,
    current_stroke: RwSignal<Option<Stroke>>,
    lasso_points: RwSignal<Vec<Point>>,
    selected_strokes: RwSignal<Vec<usize>>,
    tool: RwSignal<SketchTool>,
    color: RwSignal<SketchColor>,
    width: RwSignal<f32>,
    infinite_layout: RwSignal<bool>,
    theme_idx: RwSignal<usize>,
    last_pos: RwSignal<Option<(Point, Instant)>>,
    zoom: RwSignal<f64>,
    pan: RwSignal<Point>,
    landscape_layout: RwSignal<bool>,
    tool_eraser_active: RwSignal<bool>,
    pointer_pos: RwSignal<Option<Point>>,
}

impl View for SketchCanvas {
    fn id(&self) -> ViewId {
        self.id
    }

    fn style_pass(&mut self, _cx: &mut StyleCx<'_>) {}

    fn paint(&mut self, cx: &mut PaintCx) {
        let doc_val = self.document.get();
        let active_page = doc_val.active_page();
        let t = current_theme(self.theme_idx.get());
        let inf = self.infinite_layout.get();
        let is_landscape = self.landscape_layout.get();

        let page_width = if is_landscape { 1130.0 } else { 800.0 };
        let page_height = if is_landscape { 800.0 } else { 1130.0 };
        let width = if inf { 2000.0 } else { page_width };
        let height = if inf { 2000.0 } else { page_height };

        let zoom_val = self.zoom.get();
        let pan_val = self.pan.get();
        let project = |p: Point| Point::new(p.x * zoom_val + pan_val.x, p.y * zoom_val + pan_val.y);

        let page_rect = Rect::new(pan_val.x, pan_val.y, pan_val.x + width * zoom_val, pan_val.y + height * zoom_val);

        // Fill background paper base.
        cx.fill(&page_rect, Color::rgb8(250, 250, 248), 0.0);

        // Draw shadow boundary border for A4 Page Ratio bounds.
        if !inf {
            cx.stroke(&page_rect, t.border, &KurboStroke::new(1.0));
        }

        // Draw Grid / Lines / Dots templates if requested.
        match active_page.template.as_str() {
            "Grid" => {
                let step = 30.0;
                let stroke = KurboStroke::new(0.5);
                let col = Color::rgb8(220, 220, 225).multiply_alpha(0.5);
                for x in (step as usize..width as usize).step_by(step as usize) {
                    let p1 = project(Point::new(x as f64, 0.0));
                    let p2 = project(Point::new(x as f64, height));
                    let path = floem::kurbo::Line::new(p1, p2);
                    cx.stroke(&path, col, &stroke);
                }
                for y in (step as usize..height as usize).step_by(step as usize) {
                    let p1 = project(Point::new(0.0, y as f64));
                    let p2 = project(Point::new(width, y as f64));
                    let path = floem::kurbo::Line::new(p1, p2);
                    cx.stroke(&path, col, &stroke);
                }
            }
            "Dots" => {
                let step = 25.0;
                let col = Color::rgb8(180, 180, 190).multiply_alpha(0.6);
                for x in (step as usize..width as usize).step_by(step as usize) {
                    for y in (step as usize..height as usize).step_by(step as usize) {
                        let center = project(Point::new(x as f64, y as f64));
                        let dot = Circle::new(center, 0.8 * zoom_val);
                        cx.fill(&dot, col, 0.0);
                    }
                }
            }
            "Lines" => {
                let step = 32.0;
                let stroke = KurboStroke::new(0.5);
                let col = Color::rgb8(210, 210, 220).multiply_alpha(0.6);
                for y in (step as usize..height as usize).step_by(step as usize) {
                    let p1 = project(Point::new(0.0, y as f64));
                    let p2 = project(Point::new(width, y as f64));
                    let path = floem::kurbo::Line::new(p1, p2);
                    cx.stroke(&path, col, &stroke);
                }
            }
            _ => {} // Blank
        }

        // Paint completed saved drawing strokes.
        for stroke in &active_page.strokes {
            paint_stroke_run(cx, stroke, zoom_val, pan_val);
        }

        // Paint the current drawing stroke in progress.
        if let Some(ref stroke) = self.current_stroke.get() {
            paint_stroke_run(cx, stroke, zoom_val, pan_val);
        }

        // Paint the selection lasso path as a clean solid blue vector boundary line.
        let lasso = self.lasso_points.get();
        if !lasso.is_empty() {
            let mut path = floem::kurbo::BezPath::new();
            path.move_to(project(lasso[0]));
            for &p in &lasso[1..] {
                path.line_to(project(p));
            }
            cx.stroke(&path, Color::rgb8(120, 160, 255), &KurboStroke::new(1.0));
        }

        // Paint bounding boxes for lasso selected strokes
        let selected = self.selected_strokes.get();
        if !selected.is_empty() {
            for &idx in &selected {
                if let Some(stroke) = active_page.strokes.get(idx) {
                    if stroke.points.is_empty() { continue; }
                    let mut min_x = f64::INFINITY;
                    let mut max_x = f64::NEG_INFINITY;
                    let mut min_y = f64::INFINITY;
                    let mut max_y = f64::NEG_INFINITY;
                    for pt in &stroke.points {
                        let x = pt.x as f64;
                        let y = pt.y as f64;
                        if x < min_x { min_x = x; }
                        if x > max_x { max_x = x; }
                        if y < min_y { min_y = y; }
                        if y > max_y { max_y = y; }
                    }
                    let pad = 6.0;
                    let p_min = project(Point::new(min_x - pad, min_y - pad));
                    let p_max = project(Point::new(max_x + pad, max_y + pad));
                    let rect = Rect::new(p_min.x, p_min.y, p_max.x, p_max.y);
                    // Draw bounding rect boundary in blue
                    let stroke_style = KurboStroke::new(1.0);
                    cx.stroke(&rect, Color::rgb8(50, 130, 255), &stroke_style);
                    // Fill with ultra-translucent light blue tint
                    cx.fill(&rect, Color::rgba8(50, 130, 255, 15), 0.0);
                }
            }
        }

        // Draw the eraser/brush shape cursor preview when in eraser mode
        let is_eraser = self.tool.get() == SketchTool::Eraser || self.tool_eraser_active.get();
        if is_eraser {
            if let Some(p) = self.pointer_pos.get() {
                let size = self.width.get() as f64;
                let radius = (size / 2.0) * zoom_val;
                let center = p;
                
                let circle = Circle::new(center, radius);
                let (r, g, b) = self.color.get().rgb();
                let alpha = 0.26;
                let brush_color = Color::rgba8(r, g, b, (alpha * 255.0) as u8);
                
                cx.fill(&circle, brush_color, 0.0);
                cx.stroke(&circle, Color::rgba8(r, g, b, 120), &KurboStroke::new(0.5));
            }
        }

        // Draw a small tracking dot at the last drawing position (active stroke feedback)
        if let Some((last_pt, _time)) = self.last_pos.get() {
            if self.current_stroke.get().is_some() {
                let dot = Circle::new(last_pt, 3.0 * zoom_val);
                cx.fill(&dot, Color::rgba8(120, 200, 255, 80), 0.0);
            }
        }
    }
}

/// Paint a high-fidelity vector stroke using overlapping brush circles.
fn paint_stroke_run(cx: &mut PaintCx, stroke: &Stroke, zoom_val: f64, pan_val: Point) {
    if stroke.points.is_empty() { return; }
    
    let color = if stroke.is_eraser {
        Color::rgb8(250, 250, 248)
    } else {
        let (r, g, b) = stroke.color.rgb();
        let alpha = stroke.color.alpha(stroke.tool);
        Color::rgba8(r, g, b, (alpha * 255.0) as u8)
    };
    
    let (r, g, b) = if stroke.is_eraser {
        (250, 250, 248)
    } else {
        stroke.color.rgb()
    };
    
    // Helper to calculate dynamic width for a point
    let get_width = |pt: &brouillon_editor_engine::sketch::StrokePoint| -> f32 {
        match stroke.tool {
            SketchTool::Fineliner | SketchTool::MechanicalPencil => stroke.width,
            SketchTool::Ballpoint => stroke.width * (0.4 + 0.6 * pt.pressure),
            SketchTool::Marker | SketchTool::Paintbrush => stroke.width * (0.3 + 0.7 * pt.pressure),
            SketchTool::Pencil => {
                let tilt_factor = (1.0 - (pt.tilt_x.abs() + pt.tilt_y.abs()).min(0.8) as f32).max(0.4);
                stroke.width * (0.3 + 0.7 * pt.pressure) * tilt_factor
            }
            SketchTool::Calligraphy => {
                let tilt_influence = (pt.tilt_x - pt.tilt_y).abs() as f32;
                stroke.width * (0.5 + 0.5 * pt.pressure) * (1.0 + tilt_influence)
            }
            SketchTool::Highlighter => stroke.width,
            SketchTool::Shader => stroke.width * 2.0,
            SketchTool::Eraser | SketchTool::LassoSelect => stroke.width,
        }
    };

    let project = |x: f64, y: f64| Point::new(x * zoom_val + pan_val.x, y * zoom_val + pan_val.y);

    // 1. Draw continuous connecting line segments
    if stroke.points.len() > 1 {
        for i in 0..stroke.points.len() - 1 {
            let pt1 = &stroke.points[i];
            let pt2 = &stroke.points[i+1];
            let p1 = project(pt1.x as f64, pt1.y as f64);
            let p2 = project(pt2.x as f64, pt2.y as f64);
            
            let w1 = get_width(pt1) as f64 * zoom_val;
            let w2 = get_width(pt2) as f64 * zoom_val;
            let w = ((w1 + w2) / 2.0) as f64;
            
            let line = floem::kurbo::Line::new(p1, p2);
            cx.stroke(&line, color, &KurboStroke::new(w));
        }
    }

    // 2. Draw textured points/caps (identical to original brush physics)
    for pt in &stroke.points {
        let dynamic_width = get_width(pt) as f64 * zoom_val;
        let radius = dynamic_width / 2.0;
        let center = project(pt.x as f64, pt.y as f64);
        
        if stroke.tool == SketchTool::Pencil {
            let circle = Circle::new(center, radius);
            cx.fill(&circle, Color::rgba8(r, g, b, 20), 0.0);
            
            let offset_x = (pt.pressure as f64 * 2.0 - 1.0) * dynamic_width * 0.2;
            let offset_y = (pt.pressure as f64 * 2.0 - 1.0) * dynamic_width * 0.2;
            let grain = Circle::new(Point::new(center.x + offset_x, center.y + offset_y), radius * 0.4);
            cx.fill(&grain, Color::rgba8(r, g, b, 40), 0.0);
        } else if stroke.tool == SketchTool::Highlighter {
            let rect = Rect::new(center.x - radius, center.y - radius * 2.0, center.x + radius, center.y + radius * 2.0);
            cx.fill(&rect, color, 0.0);
        } else if stroke.tool == SketchTool::Shader {
            let circle = Circle::new(center, radius * 3.0);
            cx.fill(&circle, Color::rgba8(r, g, b, 5), 1.0);
        } else if stroke.tool == SketchTool::Eraser {
            let circle = Circle::new(center, radius);
            cx.fill(&circle, Color::rgba8(240, 80, 80, 40), 0.0);
        } else {
            let circle = Circle::new(center, radius);
            cx.fill(&circle, color, 0.0);
        }
    }
}

/// Point-in-polygon Ray-casting algorithm to detect selected lasso strokes.
fn point_in_polygon(p: Point, poly: &[Point]) -> bool {
    if poly.len() < 3 { return false; }
    let mut inside = false;
    let mut j = poly.len() - 1;
    for i in 0..poly.len() {
        if (poly[i].y > p.y) != (poly[j].y > p.y)
            && p.x < (poly[j].x - poly[i].x) * (p.y - poly[i].y) / (poly[j].y - poly[i].y) + poly[i].x
        {
            inside = !inside;
        }
        j = i;
    }
    inside
}

/// Fits raw drawing points list to perfect vector shape primitives (Line, Circle, Box).
fn snap_to_perfect_shape(stroke: &Stroke) -> Option<Stroke> {
    if stroke.points.len() < 12 { return None; }
    
    let start = stroke.points.first()?;
    let last = stroke.points.last()?;
    
    let min_x = stroke.points.iter().map(|p| p.x).fold(f32::INFINITY, f32::min);
    let max_x = stroke.points.iter().map(|p| p.x).fold(f32::NEG_INFINITY, f32::max);
    let min_y = stroke.points.iter().map(|p| p.y).fold(f32::INFINITY, f32::min);
    let max_y = stroke.points.iter().map(|p| p.y).fold(f32::NEG_INFINITY, f32::max);
    
    let span_x = max_x - min_x;
    let span_y = max_y - min_y;
    let dist = ((last.x - start.x).powi(2) + (last.y - start.y).powi(2)).sqrt();
    
    let mut line_deviation = 0.0;
    let len = stroke.points.len() as f32;
    for pt in &stroke.points {
        let num = ((last.y - start.y) * pt.x - (last.x - start.x) * pt.y + last.x * start.y - last.y * start.x).abs();
        let den = ((last.y - start.y).powi(2) + (last.x - start.x).powi(2)).sqrt();
        line_deviation += if den > 0.0 { num / den } else { 0.0 };
    }
    let avg_deviation = line_deviation / len;
    
    if avg_deviation < 4.0 && dist > 40.0 {
        let mut snapped = stroke.clone();
        snapped.points = vec![
            *start,
            StrokePoint {
                x: last.x,
                y: last.y,
                pressure: 1.0,
                tilt_x: 0.0,
                tilt_y: 0.0,
            }
        ];
        return Some(snapped);
    }
    
    let start_end_dist = ((last.x - start.x).powi(2) + (last.y - start.y).powi(2)).sqrt();
    if start_end_dist < 45.0 && span_x > 35.0 && span_y > 35.0 {
        let mut snapped = stroke.clone();
        snapped.points = vec![
            StrokePoint { x: min_x, y: min_y, pressure: 1.0, tilt_x: 0.0, tilt_y: 0.0 },
            StrokePoint { x: max_x, y: min_y, pressure: 1.0, tilt_x: 0.0, tilt_y: 0.0 },
            StrokePoint { x: max_x, y: max_y, pressure: 1.0, tilt_x: 0.0, tilt_y: 0.0 },
            StrokePoint { x: min_x, y: max_y, pressure: 1.0, tilt_x: 0.0, tilt_y: 0.0 },
            StrokePoint { x: min_x, y: min_y, pressure: 1.0, tilt_x: 0.0, tilt_y: 0.0 },
        ];
        return Some(snapped);
    }
    
    None
}

/// Create a scroll view containing the interactive vector sketchpad.
pub fn sketch_view(
    crayon_mode: RwSignal<bool>,
    document_sig: RwSignal<SketchDocument>,
    active_tool: RwSignal<SketchTool>,
    active_color: RwSignal<SketchColor>,
    active_width: RwSignal<f32>,
    infinite_layout: RwSignal<bool>,
    theme_idx: RwSignal<usize>,
    tabs_sig: RwSignal<TabManager>,
    sync_active_to_mirrors: Arc<dyn Fn() + 'static>,
) -> impl IntoView {
    let id = ViewId::new();
    let current_stroke: RwSignal<Option<Stroke>> = floem::reactive::create_rw_signal(None);
    let lasso_points: RwSignal<Vec<Point>> = floem::reactive::create_rw_signal(Vec::new());
    let selected_strokes: RwSignal<Vec<usize>> = floem::reactive::create_rw_signal(Vec::new());
    let last_pos: RwSignal<Option<(Point, Instant)>> = floem::reactive::create_rw_signal(None);
    let pointer_pos: RwSignal<Option<Point>> = floem::reactive::create_rw_signal(None);

    let shared_width = Arc::new(std::sync::atomic::AtomicU32::new(active_width.get_untracked().to_bits()));
    let shared_width_sync = shared_width.clone();
    create_effect(move |_| {
        let val = active_width.get();
        shared_width_sync.store(val.to_bits(), std::sync::atomic::Ordering::SeqCst);
    });
    
    let trigger = Trigger::new();
    let shared_width_gui = shared_width.clone();
    let active_width_gui = active_width;
    create_effect(move |_| {
        trigger.track();
        let bits = shared_width_gui.load(std::sync::atomic::Ordering::SeqCst);
        let val = f32::from_bits(bits);
        if active_width_gui.get_untracked() != val {
            active_width_gui.set(val);
        }
    });

    create_effect(move |_| {
        if crayon_mode.get() {
            id.request_focus();
        }
    });

    let zoom: RwSignal<f64> = floem::reactive::create_rw_signal(1.0);
    let pan: RwSignal<Point> = floem::reactive::create_rw_signal(Point::ZERO);
    let initial_landscape = document_sig.get_untracked().active_page().landscape;
    let landscape_layout: RwSignal<bool> = floem::reactive::create_rw_signal(initial_landscape);
    let tool_eraser_active: RwSignal<bool> = floem::reactive::create_rw_signal(false);

    let left_button_down: RwSignal<bool> = floem::reactive::create_rw_signal(false);
    let right_button_down: RwSignal<bool> = floem::reactive::create_rw_signal(false);

    // Sync active page landscape back and forth with landscape_layout signal
    create_effect(move |_| {
        let doc = document_sig.get();
        let page_land = doc.active_page().landscape;
        floem::reactive::batch(move || {
            if landscape_layout.get_untracked() != page_land {
                landscape_layout.set(page_land);
            }
        });
    });

    create_effect(move |_| {
        let land = landscape_layout.get();
        document_sig.update(|doc| {
            if doc.active_page().landscape != land {
                doc.active_page_mut().landscape = land;
            }
        });
    });

    let id_clone = id;
    
    // Wire reactive trigger to repaint when signals change.
    create_effect(move |_| {
        let _ = document_sig.get();
        let _ = current_stroke.get();
        let _ = lasso_points.get();
        let _ = selected_strokes.get();
        let _ = infinite_layout.get();
        let _ = theme_idx.get();
        let _ = crayon_mode.get();
        let _ = zoom.get();
        let _ = pan.get();
        let _ = landscape_layout.get();
        let _ = pointer_pos.get();
        id_clone.request_paint();
    });

    let canvas_widget = SketchCanvas {
        id,
        document: document_sig,
        current_stroke,
        lasso_points,
        selected_strokes,
        tool: active_tool,
        color: active_color,
        width: active_width,
        infinite_layout,
        theme_idx,
        last_pos,
        zoom,
        pan,
        landscape_layout,
        tool_eraser_active,
        pointer_pos,
    };

    let decorated_canvas = canvas_widget
        .style(move |s| {
            let is_landscape = landscape_layout.get();
            let page_width = if is_landscape { 1130.0 } else { 800.0 };
            let page_height = if is_landscape { 800.0 } else { 1130.0 };
            let width = if infinite_layout.get() { 2000.0 } else { page_width };
            let height = if infinite_layout.get() { 2000.0 } else { page_height };
            s.width(width as f32)
                .height(height as f32)
        })
        .keyboard_navigable()
        .on_event_stop(EventListener::PointerWheel, move |event| {
            if let Event::PointerWheel(pe) = event {
                let delta_y = pe.delta.y;
                if delta_y != 0.0 {
                    let factor = if delta_y > 0.0 { 1.1 } else { 0.9 };
                    let old_z = zoom.get_untracked();
                    let new_z = (old_z * factor).clamp(0.15, 8.0);
                    
                    let cursor = pe.pos;
                    let old_pan = pan.get_untracked();
                    let new_pan = cursor - (cursor - old_pan) * (new_z / old_z);
                    
                    zoom.set(new_z);
                    pan.set(new_pan);
                }
            }
        })
        .on_event_stop(EventListener::PointerDown, move |event| {
            if let Event::PointerDown(pe) = event {
                id.request_focus();
                let p = pe.pos;
                let current_tool = active_tool.get();
                let current_color = active_color.get();
                let current_width = active_width.get();
                
                if pe.button.is_primary() {
                    left_button_down.set(true);
                } else if pe.button.is_secondary() {
                    right_button_down.set(true);
                }

                last_pos.set(Some((p, Instant::now())));

                let is_dual_mouse = left_button_down.get() && right_button_down.get();
                if is_dual_mouse {
                    current_stroke.set(None);
                    lasso_points.set(Vec::new());
                    return;
                }

                if pe.button.is_primary() {
                    let unproject = |pt: Point| {
                        Point::new((pt.x - pan.get().x) / zoom.get(), (pt.y - pan.get().y) / zoom.get())
                    };
                    let doc_p = unproject(p);

                    // Clear redo stack on fresh stroke drawing
                    document_sig.update(|d| {
                        d.active_page_mut().redo_strokes.clear();
                    });

                    if current_tool == SketchTool::LassoSelect {
                        lasso_points.set(vec![doc_p]);
                        selected_strokes.set(Vec::new());
                    } else {
                        let start_point = StrokePoint {
                            x: doc_p.x as f32,
                            y: doc_p.y as f32,
                            pressure: 0.5,
                            tilt_x: 0.0,
                            tilt_y: 0.0,
                        };
                        
                        let is_eraser_stroke = current_tool == SketchTool::Eraser || tool_eraser_active.get();
                        let new_stroke = Stroke {
                            points: vec![start_point],
                            tool: current_tool,
                            color: current_color,
                            width: current_width,
                            layer_idx: 0,
                            is_eraser: is_eraser_stroke,
                        };
                        current_stroke.set(Some(new_stroke));
                    }
                }
            }
        })
        .on_event_stop(EventListener::PointerMove, move |event| {
            if let Event::PointerMove(pe) = event {
                let p = pe.pos;
                pointer_pos.set(Some(p));
                let current_tool = active_tool.get();

                let is_dual_mouse = left_button_down.get() && right_button_down.get();

                if is_dual_mouse {
                    if let Some((last, _)) = last_pos.get_untracked() {
                        let delta = p - last;
                        pan.update(|pt| {
                            pt.x += delta.x;
                            pt.y += delta.y;
                        });
                    }
                    last_pos.set(Some((p, Instant::now())));
                    return;
                }

                let unproject = |pt: Point| {
                    Point::new((pt.x - pan.get().x) / zoom.get(), (pt.y - pan.get().y) / zoom.get())
                };
                let doc_p = unproject(p);

                if current_tool == SketchTool::LassoSelect {
                    lasso_points.update(|pts| {
                        pts.push(doc_p);
                    });
                } else if let Some(mut stroke) = current_stroke.get_untracked() {
                    let pressure = if let Some((last, time)) = last_pos.get_untracked() {
                        let dx = p.x - last.x;
                        let dy = p.y - last.y;
                        let dist = (dx * dx + dy * dy).sqrt();
                        let dt = time.elapsed().as_secs_f32().max(0.001);
                        let velocity = dist as f32 / dt;
                        
                        let max_vel = 1200.0;
                        (1.0 - (velocity / max_vel)).clamp(0.15, 1.0)
                    } else {
                        0.5
                    };

                    last_pos.set(Some((p, Instant::now())));

                    let next_point = StrokePoint {
                        x: doc_p.x as f32,
                        y: doc_p.y as f32,
                        pressure,
                        tilt_x: 0.0,
                        tilt_y: 0.0,
                    };
                    
                    stroke.points.push(next_point);
                    current_stroke.set(Some(stroke));
                }
            }
        })
        .on_event_stop(EventListener::PointerUp, move |event| {
            if let Event::PointerUp(pe) = event {
                if pe.button.is_primary() {
                    left_button_down.set(false);
                } else if pe.button.is_secondary() {
                    right_button_down.set(false);
                }

                let current_tool = active_tool.get();
                let is_dual_mouse = left_button_down.get() && right_button_down.get();

                if is_dual_mouse {
                    return;
                }

                if current_tool == SketchTool::LassoSelect {
                    let pts = lasso_points.get_untracked();
                    if pts.len() > 3 {
                        let doc_val = document_sig.get_untracked();
                        let active_page = doc_val.active_page();
                        let mut selected = Vec::new();
                        
                        for (idx, stroke) in active_page.strokes.iter().enumerate() {
                            let mut hits = 0;
                            for pt in &stroke.points {
                                if point_in_polygon(Point::new(pt.x as f64, pt.y as f64), &pts) {
                                    hits += 1;
                                }
                            }
                            if stroke.points.len() > 0 && hits * 2 > stroke.points.len() {
                                selected.push(idx);
                            }
                        }
                        
                        selected_strokes.set(selected);
                        eprintln!("[lasso] selected {} stroke(s)", selected_strokes.get_untracked().len());
                    }
                    lasso_points.set(Vec::new());
                } else if let Some(mut stroke) = current_stroke.get_untracked() {
                    if stroke.tool == SketchTool::Eraser {
                        document_sig.update(|d| {
                            let page = d.active_page_mut();
                            page.strokes.retain(|s| {
                                for pt in &s.points {
                                    for e_pt in &stroke.points {
                                        let dx = pt.x - e_pt.x;
                                        let dy = pt.y - e_pt.y;
                                        let dist = (dx * dx + dy * dy).sqrt();
                                        if dist < 12.0 {
                                            return false;
                                        }
                                    }
                                }
                                true
                            });
                        });
                    } else {
                        let fit_shape = snap_to_perfect_shape(&stroke);
                        if let Some(snapped_stroke) = fit_shape {
                            stroke = snapped_stroke;
                            eprintln!("[shape-snap] snapped stroke to perfect vector shape");
                        }

                        document_sig.update(|d| {
                            d.active_page_mut().strokes.push(stroke);
                        });
                    }
                    current_stroke.set(None);
                    last_pos.set(None);
                }
            }
        })
        .on_event_stop(EventListener::PointerLeave, move |_| {
            pointer_pos.set(None);
        });

    // Helper functions to build beautiful Tokyo-Night styling toolbar cells
    let tool_btn = move |tool_type: SketchTool, label_str: &'static str, sig: RwSignal<SketchTool>, t_sig: RwSignal<usize>| {
        label(move || label_str.to_string())
            .on_click_stop(move |_| {
                sig.set(tool_type);
                if tool_type == SketchTool::Eraser {
                    tool_eraser_active.set(false);
                }
                eprintln!("[Crayon] outil sélectionné : {:?}", tool_type);
            })
            .style(move |s| {
                let theme = current_theme(t_sig.get());
                let is_active = sig.get() == tool_type;
                let mut s = s.padding_horiz(8.0)
                    .padding_vert(4.0)
                    .font_size(12.0)
                    .cursor(CursorStyle::Pointer)
                    .border_radius(4.0);
                if is_active {
                    s = s.background(theme.accent.multiply_alpha(0.25)).color(theme.accent);
                } else {
                    s = s.color(theme.fg).hover(|h| h.background(theme.fg.multiply_alpha(0.06)));
                }
                s
            })
    };

    let tool_eraser_btn = label(move || "🧽 Gomme Outil".to_string())
        .on_click_stop(move |_| {
            if active_tool.get_untracked() != SketchTool::Eraser {
                tool_eraser_active.update(|v| *v = !*v);
                eprintln!("[Crayon] Gomme Outil activée : {}", tool_eraser_active.get_untracked());
            }
        })
        .style(move |s| {
            let theme = current_theme(theme_idx.get());
            let is_disabled = active_tool.get() == SketchTool::Eraser;
            let is_active = tool_eraser_active.get() && !is_disabled;
            let mut s = s.padding_horiz(8.0)
                .padding_vert(4.0)
                .font_size(12.0)
                .border_radius(4.0);
            if is_disabled {
                s = s.color(theme.fg_dim).cursor(CursorStyle::Default);
            } else {
                s = s.cursor(CursorStyle::Pointer);
                if is_active {
                    s = s.background(theme.accent.multiply_alpha(0.25)).color(theme.accent);
                } else {
                    s = s.color(theme.fg).hover(|h| h.background(theme.fg.multiply_alpha(0.06)));
                }
            }
            s
        });

    let color_btn = move |color_type: SketchColor, label_str: &'static str, sig: RwSignal<SketchColor>, t_sig: RwSignal<usize>| {
        label(move || label_str.to_string())
            .on_click_stop(move |_| {
                sig.set(color_type);
                eprintln!("[Crayon] couleur sélectionnée : {:?}", color_type);
            })
            .style(move |s| {
                let theme = current_theme(t_sig.get());
                let is_active = sig.get() == color_type;
                let mut s = s.padding_horiz(8.0)
                    .padding_vert(4.0)
                    .font_size(12.0)
                    .cursor(CursorStyle::Pointer)
                    .border_radius(4.0);
                if is_active {
                    s = s.background(theme.accent.multiply_alpha(0.25)).color(theme.accent);
                } else {
                    s = s.color(theme.fg).hover(|h| h.background(theme.fg.multiply_alpha(0.06)));
                }
                s
            })
    };

    let tools_row = stack((
        tool_btn(SketchTool::Ballpoint, "🖊 Stylo", active_tool, theme_idx),
        tool_btn(SketchTool::Fineliner, "✒ Feutre Fin", active_tool, theme_idx),
        tool_btn(SketchTool::Marker, "🖍 Feutre", active_tool, theme_idx),
        tool_btn(SketchTool::Pencil, "✏ Crayon", active_tool, theme_idx),
        tool_btn(SketchTool::MechanicalPencil, "✏ Portemine", active_tool, theme_idx),
        tool_btn(SketchTool::Calligraphy, "✒ Calligraphie", active_tool, theme_idx),
        tool_btn(SketchTool::Paintbrush, "🖌 Pinceau", active_tool, theme_idx),
        tool_btn(SketchTool::Highlighter, "🖌 Surligneur", active_tool, theme_idx),
        tool_btn(SketchTool::Shader, "🌫 Estompe", active_tool, theme_idx),
        tool_btn(SketchTool::Eraser, "🧽 Gomme", active_tool, theme_idx),
        tool_eraser_btn,
        tool_btn(SketchTool::LassoSelect, "🕸 Lasso", active_tool, theme_idx),
    ))
    .style(|s| s.flex_row().gap(4.0).padding(4.0));

    let colors_row = stack((
        color_btn(SketchColor::Black, "⚫ Noir", active_color, theme_idx),
        color_btn(SketchColor::Red, "🔴 Rouge", active_color, theme_idx),
        color_btn(SketchColor::Blue, "🔵 Bleu", active_color, theme_idx),
        color_btn(SketchColor::Green, "🟢 Vert", active_color, theme_idx),
        color_btn(SketchColor::Yellow, "🟡 Jaune", active_color, theme_idx),
        
        empty().style(move |s| {
            let t = current_theme(theme_idx.get());
            s.width(1.0).height(16.0).background(t.border).margin_horiz(8.0)
        }),
        
        label(|| "Taille : ".to_string()).style(move |s| s.color(current_theme(theme_idx.get()).fg).font_size(12.0).font_bold()),
        
        {
            let shared_width_dec = shared_width.clone();
            let trigger_dec = trigger;
            let is_dec_down = Arc::new(std::sync::atomic::AtomicBool::new(false));
            let is_dec_down_clone1 = is_dec_down.clone();
            let is_dec_down_clone2 = is_dec_down.clone();
            let is_dec_down_clone3 = is_dec_down.clone();
            
            label(|| " - ".to_string())
                .on_event_stop(EventListener::PointerDown, move |_| {
                    if !is_dec_down_clone1.swap(true, std::sync::atomic::Ordering::SeqCst) {
                        let mut current = f32::from_bits(shared_width_dec.load(std::sync::atomic::Ordering::SeqCst));
                        current = (current - 0.5).max(0.5);
                        shared_width_dec.store(current.to_bits(), std::sync::atomic::Ordering::SeqCst);
                        register_ext_trigger(trigger_dec);
                        
                        let is_dec = is_dec_down_clone1.clone();
                        let shared = shared_width_dec.clone();
                        std::thread::spawn(move || {
                            std::thread::sleep(std::time::Duration::from_millis(400));
                            while is_dec.load(std::sync::atomic::Ordering::SeqCst) {
                                let mut current = f32::from_bits(shared.load(std::sync::atomic::Ordering::SeqCst));
                                current = (current - 0.5).max(0.5);
                                shared.store(current.to_bits(), std::sync::atomic::Ordering::SeqCst);
                                register_ext_trigger(trigger_dec);
                                std::thread::sleep(std::time::Duration::from_millis(120));
                            }
                        });
                    }
                })
                .on_event_stop(EventListener::PointerUp, move |_| {
                    is_dec_down_clone2.store(false, std::sync::atomic::Ordering::SeqCst);
                })
                .on_event_stop(EventListener::PointerLeave, move |_| {
                    is_dec_down_clone3.store(false, std::sync::atomic::Ordering::SeqCst);
                })
                .style(move |s| {
                    let t = current_theme(theme_idx.get());
                    s.padding_horiz(8.0)
                        .font_size(16.0)
                        .font_bold()
                        .color(t.fg)
                        .cursor(CursorStyle::Pointer)
                        .border_radius(2.0)
                        .hover(|h| h.background(t.border))
                })
        },
        
        label(move || format!("{:.1}px", active_width.get())).style(move |s| s.color(current_theme(theme_idx.get()).fg).font_size(12.0)),
        
        {
            let shared_width_inc = shared_width.clone();
            let trigger_inc = trigger;
            let is_inc_down = Arc::new(std::sync::atomic::AtomicBool::new(false));
            let is_inc_down_clone1 = is_inc_down.clone();
            let is_inc_down_clone2 = is_inc_down.clone();
            let is_inc_down_clone3 = is_inc_down.clone();
            
            label(|| " + ".to_string())
                .on_event_stop(EventListener::PointerDown, move |_| {
                    if !is_inc_down_clone1.swap(true, std::sync::atomic::Ordering::SeqCst) {
                        let mut current = f32::from_bits(shared_width_inc.load(std::sync::atomic::Ordering::SeqCst));
                        current = (current + 0.5).min(20.0);
                        shared_width_inc.store(current.to_bits(), std::sync::atomic::Ordering::SeqCst);
                        register_ext_trigger(trigger_inc);
                        
                        let is_inc = is_inc_down_clone1.clone();
                        let shared = shared_width_inc.clone();
                        std::thread::spawn(move || {
                            std::thread::sleep(std::time::Duration::from_millis(400));
                            while is_inc.load(std::sync::atomic::Ordering::SeqCst) {
                                let mut current = f32::from_bits(shared.load(std::sync::atomic::Ordering::SeqCst));
                                current = (current + 0.5).min(20.0);
                                shared.store(current.to_bits(), std::sync::atomic::Ordering::SeqCst);
                                register_ext_trigger(trigger_inc);
                                std::thread::sleep(std::time::Duration::from_millis(120));
                            }
                        });
                    }
                })
                .on_event_stop(EventListener::PointerUp, move |_| {
                    is_inc_down_clone2.store(false, std::sync::atomic::Ordering::SeqCst);
                })
                .on_event_stop(EventListener::PointerLeave, move |_| {
                    is_inc_down_clone3.store(false, std::sync::atomic::Ordering::SeqCst);
                })
                .style(move |s| {
                    let t = current_theme(theme_idx.get());
                    s.padding_horiz(8.0)
                        .font_size(16.0)
                        .font_bold()
                        .color(t.fg)
                        .cursor(CursorStyle::Pointer)
                        .border_radius(2.0)
                        .hover(|h| h.background(t.border))
                })
        },
        
        empty().style(move |s| {
            let size = active_width.get() as f64;
            let is_eraser = active_tool.get() == SketchTool::Eraser || tool_eraser_active.get();
            let color = active_color.get().rgb();
            let display_size = size.max(2.0).min(24.0);
            
            let mut s = s.width(display_size)
                .height(display_size)
                .margin_left(8.0)
                .border_radius(display_size / 2.0);
                
            if is_eraser {
                s = s.border(1.0)
                    .border_color(Color::rgb8(150, 150, 150))
                    .background(Color::rgb8(240, 240, 240).multiply_alpha(0.5));
            } else {
                let floem_color = Color::rgb8(color.0, color.1, color.2);
                s = s.background(floem_color);
            }
            s
        }),
    ))
    .style(|s| s.flex_row().items_center().gap(4.0).padding(4.0));

    let doc_sig_clone = document_sig;
    let sync_active_to_mirrors_clone = sync_active_to_mirrors.clone();
    
    let ops_row = stack((
        label(|| "🤖 Convertir la Page (IA)".to_string())
            .on_click_stop(move |_| {
                let doc_val = doc_sig_clone.get_untracked();
                let sync = sync_active_to_mirrors_clone.clone();
                
                let success_cb = create_ext_action(floem::reactive::Scope::current(), move |loaded: (PathBuf, PathBuf)| {
                    let (md_path, json_path) = loaded;
                    tabs_sig.update(|tm| {
                        let _ = tm.open_file(&md_path);
                        let _ = tm.open_file(&json_path);
                    });
                    sync();
                    crayon_mode.set(false);
                });
                
                let error_cb = create_ext_action(floem::reactive::Scope::current(), move |msg: String| {
                    eprintln!("[Crayon Mode] {}", msg);
                });
                
                ai_extract::transcribe_canvas(
                    doc_val,
                    move |md, json_str| {
                        let md_path = PathBuf::from("extracted_notes.md");
                        let json_path = PathBuf::from("extracted_notes.json");
                        let _ = std::fs::write(&md_path, &md);
                        let _ = std::fs::write(&json_path, &json_str);
                        success_cb((md_path, json_path));
                    },
                    move |err| {
                        error_cb(err);
                    }
                );
            })
            .style(move |s| {
                let t = current_theme(theme_idx.get());
                s.padding_horiz(10.0)
                    .padding_vert(4.0)
                    .background(t.accent)
                    .color(Color::rgb8(255, 255, 255))
                    .border_radius(4.0)
                    .cursor(CursorStyle::Pointer)
                    .font_size(12.0)
                    .font_bold()
                    .hover(|h| h.background(t.accent.multiply_alpha(0.8)))
            }),
            
        label(|| "📄 PDF".to_string())
            .on_click_stop(move |_| {
                let doc_val = doc_sig_clone.get_untracked();
                let path = PathBuf::from("exported_drawing.pdf");
                match export::export_to_pdf(&path, &doc_val, "Croquis Brouillon") {
                    Ok(_) => eprintln!("[Export] PDF enregistré avec succès dans exported_drawing.pdf"),
                    Err(e) => eprintln!("[Export] Erreur PDF: {}", e),
                }
            })
            .style(move |s| {
                let t = current_theme(theme_idx.get());
                s.padding_horiz(8.0).padding_vert(4.0).background(t.border).color(t.fg).border_radius(4.0).cursor(CursorStyle::Pointer).font_size(12.0).hover(|h| h.background(t.fg.multiply_alpha(0.1)))
            }),
            
        label(|| "📝 Word".to_string())
            .on_click_stop(move |_| {
                let doc_val = doc_sig_clone.get_untracked();
                let path = PathBuf::from("exported_drawing.docx");
                let (_, json_str) = ai_extract::generate_mock_transcription(&doc_val);
                match export::export_to_docx(&path, &json_str) {
                    Ok(_) => eprintln!("[Export] Word enregistré avec succès dans exported_drawing.docx"),
                    Err(e) => eprintln!("[Export] Erreur Word: {}", e),
                }
            })
            .style(move |s| {
                let t = current_theme(theme_idx.get());
                s.padding_horiz(8.0).padding_vert(4.0).background(t.border).color(t.fg).border_radius(4.0).cursor(CursorStyle::Pointer).font_size(12.0).hover(|h| h.background(t.fg.multiply_alpha(0.1)))
            }),
            
        label(|| "🖼 PNG".to_string())
            .on_click_stop(move |_| {
                let doc_val = doc_sig_clone.get_untracked();
                let path = PathBuf::from("exported_drawing.png");
                match export::export_to_png(&path, &doc_val, 2.0) {
                    Ok(_) => eprintln!("[Export] PNG enregistré avec succès dans exported_drawing.png"),
                    Err(e) => eprintln!("[Export] Erreur PNG: {}", e),
                }
            })
            .style(move |s| {
                let t = current_theme(theme_idx.get());
                s.padding_horiz(8.0).padding_vert(4.0).background(t.border).color(t.fg).border_radius(4.0).cursor(CursorStyle::Pointer).font_size(12.0).hover(|h| h.background(t.fg.multiply_alpha(0.1)))
            }),
            
        label(|| "🎨 XCF GIMP".to_string())
            .on_click_stop(move |_| {
                let doc_val = doc_sig_clone.get_untracked();
                let path = PathBuf::from("exported_drawing.xcf");
                match crate::xcf::export_to_xcf(&path, &doc_val) {
                    Ok(_) => eprintln!("[Export] XCF GIMP enregistré avec succès dans exported_drawing.xcf"),
                    Err(e) => eprintln!("[Export] Erreur XCF GIMP: {}", e),
                }
            })
            .style(move |s| {
                let t = current_theme(theme_idx.get());
                s.padding_horiz(8.0).padding_vert(4.0).background(t.border).color(t.fg).border_radius(4.0).cursor(CursorStyle::Pointer).font_size(12.0).hover(|h| h.background(t.fg.multiply_alpha(0.1)))
            }),

        label(move || if infinite_layout.get() { "🌐 Infini".to_string() } else { "📄 A4 Page".to_string() })
            .on_click_stop(move |_| {
                infinite_layout.update(|inf| *inf = !*inf);
            })
            .style(move |s| {
                let t = current_theme(theme_idx.get());
                s.padding_horiz(8.0).padding_vert(4.0).background(t.border).color(t.fg).border_radius(4.0).cursor(CursorStyle::Pointer).font_size(12.0).hover(|h| h.background(t.fg.multiply_alpha(0.1)))
            }),

        floem::views::dyn_container(
            move || infinite_layout.get(),
            {
                let landscape_layout = landscape_layout;
                let theme_idx = theme_idx;
                move |is_infinite| {
                    if is_infinite {
                        empty().into_any()
                    } else {
                        label(move || if landscape_layout.get() { "🔄 Paysage".to_string() } else { "🔄 Portrait".to_string() })
                            .on_click_stop(move |_| {
                                landscape_layout.update(|land| *land = !*land);
                                eprintln!("[Crayon] Orientation modifiée : Paysage={}", landscape_layout.get_untracked());
                            })
                            .style(move |s| {
                                let t = current_theme(theme_idx.get());
                                s.padding_horiz(8.0).padding_vert(4.0).background(t.border).color(t.fg).border_radius(4.0).cursor(CursorStyle::Pointer).font_size(12.0).hover(|h| h.background(t.fg.multiply_alpha(0.1)))
                            })
                            .into_any()
                    }
                }
            }
        ),
    ))
    .style(|s| s.flex_row().items_center().gap(8.0).padding(4.0));

    let crayon_toolbar = stack((
        tools_row,
        colors_row,
        ops_row,
    ))
    .style(move |s| {
        let t = current_theme(theme_idx.get());
        s.flex_col()
            .width_full()
            .background(t.bg_chrome)
            .border_bottom(1.0)
            .border_color(t.border)
            .padding(6.0)
            .gap(4.0)
    });

    stack((
        crayon_toolbar,
        scroll(decorated_canvas).style(|s| s.size_full()),
    ))
    .style(|s| s.size_full().flex_col())
}

#[cfg(test)]
mod tests {
    use super::*;
    use floem::reactive::{with_scope, Scope, create_rw_signal};
    use brouillon_editor_engine::sketch::{SketchDocument, SketchTool, SketchColor};

    fn run<F: FnOnce() + 'static>(f: F) {
        let scope = Scope::new();
        with_scope(scope, f);
        scope.dispose();
    }

    #[test]
    fn test_point_in_polygon_inside() {
        let poly = vec![
            Point::new(0.0, 0.0),
            Point::new(10.0, 0.0),
            Point::new(10.0, 10.0),
            Point::new(0.0, 10.0),
        ];
        assert!(point_in_polygon(Point::new(5.0, 5.0), &poly));
    }

    #[test]
    fn test_point_in_polygon_outside() {
        let poly = vec![
            Point::new(0.0, 0.0),
            Point::new(10.0, 0.0),
            Point::new(10.0, 10.0),
            Point::new(0.0, 10.0),
        ];
        assert!(!point_in_polygon(Point::new(15.0, 5.0), &poly));
    }

    #[test]
    fn test_point_in_polygon_small() {
        let poly = vec![Point::new(0.0, 0.0), Point::new(10.0, 0.0)];
        assert!(!point_in_polygon(Point::new(5.0, 0.0), &poly));
    }

    #[test]
    fn test_point_in_polygon_empty() {
        assert!(!point_in_polygon(Point::new(0.0, 0.0), &[]));
    }

    #[test]
    fn test_snap_to_perfect_shape_few_points() {
        let s = Stroke {
            points: vec![StrokePoint { x: 0.0, y: 0.0, pressure: 1.0, tilt_x: 0.0, tilt_y: 0.0 }],
            tool: SketchTool::Pencil,
            color: SketchColor::Black,
            width: 2.0,
            layer_idx: 0,
            is_eraser: false,
        };
        assert!(snap_to_perfect_shape(&s).is_none());
    }

    #[test]
    fn test_snap_to_perfect_shape_straight_line() {
        let mut pts = Vec::new();
        for i in 0..15 {
            pts.push(StrokePoint {
                x: i as f32 * 10.0,
                y: i as f32 * 10.0,
                pressure: 1.0,
                tilt_x: 0.0,
                tilt_y: 0.0,
            });
        }
        let s = Stroke {
            points: pts,
            tool: SketchTool::Pencil,
            color: SketchColor::Black,
            width: 2.0,
            layer_idx: 0,
            is_eraser: false,
        };
        let snapped = snap_to_perfect_shape(&s);
        assert!(snapped.is_some());
        let snapped = snapped.unwrap();
        assert_eq!(snapped.points.len(), 2);
        assert_eq!(snapped.points[0].x, 0.0);
        assert_eq!(snapped.points[1].x, 140.0);
    }

    #[test]
    fn test_snap_to_perfect_shape_box() {
        let mut pts = Vec::new();
        for i in 0..5 { pts.push(StrokePoint { x: i as f32 * 20.0, y: 0.0, pressure: 1.0, tilt_x: 0.0, tilt_y: 0.0 }); }
        for i in 1..5 { pts.push(StrokePoint { x: 100.0, y: i as f32 * 20.0, pressure: 1.0, tilt_x: 0.0, tilt_y: 0.0 }); }
        for i in 1..5 { pts.push(StrokePoint { x: 100.0 - i as f32 * 20.0, y: 100.0, pressure: 1.0, tilt_x: 0.0, tilt_y: 0.0 }); }
        for i in 1..5 { pts.push(StrokePoint { x: 0.0, y: 100.0 - i as f32 * 20.0, pressure: 1.0, tilt_x: 0.0, tilt_y: 0.0 }); }
        
        let s = Stroke {
            points: pts,
            tool: SketchTool::Pencil,
            color: SketchColor::Black,
            width: 2.0,
            layer_idx: 0,
            is_eraser: false,
        };
        let snapped = snap_to_perfect_shape(&s);
        assert!(snapped.is_some());
        let snapped = snapped.unwrap();
        assert_eq!(snapped.points.len(), 5);
        assert_eq!(snapped.points[0].x, 0.0);
        assert_eq!(snapped.points[2].y, 100.0);
    }

    #[test]
    fn test_snap_to_perfect_shape_noisy() {
        let mut pts = Vec::new();
        for i in 0..20 {
            pts.push(StrokePoint {
                x: i as f32 * 5.0,
                y: (i % 2) as f32 * 50.0,
                pressure: 1.0,
                tilt_x: 0.0,
                tilt_y: 0.0,
            });
        }
        let s = Stroke {
            points: pts,
            tool: SketchTool::Pencil,
            color: SketchColor::Black,
            width: 2.0,
            layer_idx: 0,
            is_eraser: false,
        };
        assert!(snap_to_perfect_shape(&s).is_none());
    }

    #[test]
    fn sketch_view_builds_smoke_test() {
        run(|| {
            let crayon_mode = create_rw_signal(false);
            let document_sig = create_rw_signal(SketchDocument::new());
            let active_tool = create_rw_signal(SketchTool::Pencil);
            let active_color = create_rw_signal(SketchColor::Black);
            let active_width = create_rw_signal(2.0f32);
            let infinite_layout = create_rw_signal(false);
            let theme_idx = create_rw_signal(0);
            let tabs_sig = create_rw_signal(TabManager::new());
            let sync_mirrors: Arc<dyn Fn() + 'static> = Arc::new(|| {});
            let _view = sketch_view(
                crayon_mode,
                document_sig,
                active_tool,
                active_color,
                active_width,
                infinite_layout,
                theme_idx,
                tabs_sig,
                sync_mirrors,
            );
        });
    }

    #[test]
    fn sketch_view_canvas_properties() {
        run(|| {
            let doc = create_rw_signal(SketchDocument::new());
            let current_stroke = create_rw_signal(None);
            let lasso_points = create_rw_signal(Vec::new());
            let selected_strokes = create_rw_signal(Vec::new());
            let tool = create_rw_signal(SketchTool::Pencil);
            let color = create_rw_signal(SketchColor::Black);
            let width = create_rw_signal(2.0f32);
            let infinite_layout = create_rw_signal(false);
            let theme_idx = create_rw_signal(0);
            let last_pos = create_rw_signal(None);
            let zoom = create_rw_signal(1.0);
            let pan = create_rw_signal(Point::ZERO);
            let landscape_layout = create_rw_signal(false);
            let tool_eraser_active = create_rw_signal(false);
            let pointer_pos = create_rw_signal(None);

            let canvas = SketchCanvas {
                id: ViewId::new(),
                document: doc,
                current_stroke,
                lasso_points,
                selected_strokes,
                tool,
                color,
                width,
                infinite_layout,
                theme_idx,
                last_pos,
                zoom,
                pan,
                landscape_layout,
                tool_eraser_active,
                pointer_pos,
            };
            assert_eq!(canvas.document.get_untracked().pages.len(), 1);
        });
    }

    #[test]
    fn sketch_view_crayon_mode_true() {
        run(|| {
            let crayon_mode = create_rw_signal(true);
            let document_sig = create_rw_signal(SketchDocument::new());
            let active_tool = create_rw_signal(SketchTool::Pencil);
            let active_color = create_rw_signal(SketchColor::Black);
            let active_width = create_rw_signal(2.0f32);
            let infinite_layout = create_rw_signal(false);
            let theme_idx = create_rw_signal(0);
            let tabs_sig = create_rw_signal(TabManager::new());
            let sync_mirrors: Arc<dyn Fn() + 'static> = Arc::new(|| {});
            let _view = sketch_view(
                crayon_mode,
                document_sig,
                active_tool,
                active_color,
                active_width,
                infinite_layout,
                theme_idx,
                tabs_sig,
                sync_mirrors,
            );
        });
    }

    #[test]
    fn sketch_view_landscape_layout() {
        run(|| {
            let crayon_mode = create_rw_signal(false);
            let document_sig = create_rw_signal(SketchDocument::new());
            let active_tool = create_rw_signal(SketchTool::Pencil);
            let active_color = create_rw_signal(SketchColor::Black);
            let active_width = create_rw_signal(2.0f32);
            let infinite_layout = create_rw_signal(false);
            let theme_idx = create_rw_signal(0);
            let tabs_sig = create_rw_signal(TabManager::new());
            let sync_mirrors: Arc<dyn Fn() + 'static> = Arc::new(|| {});
            let _view = sketch_view(
                crayon_mode,
                document_sig,
                active_tool,
                active_color,
                active_width,
                infinite_layout,
                theme_idx,
                tabs_sig,
                sync_mirrors,
            );
        });
    }

    #[test]
    fn sketch_view_infinite_layout() {
        run(|| {
            let crayon_mode = create_rw_signal(false);
            let document_sig = create_rw_signal(SketchDocument::new());
            let active_tool = create_rw_signal(SketchTool::Pencil);
            let active_color = create_rw_signal(SketchColor::Black);
            let active_width = create_rw_signal(2.0f32);
            let infinite_layout = create_rw_signal(true);
            let theme_idx = create_rw_signal(0);
            let tabs_sig = create_rw_signal(TabManager::new());
            let sync_mirrors: Arc<dyn Fn() + 'static> = Arc::new(|| {});
            let _view = sketch_view(
                crayon_mode,
                document_sig,
                active_tool,
                active_color,
                active_width,
                infinite_layout,
                theme_idx,
                tabs_sig,
                sync_mirrors,
            );
        });
    }

    #[test]
    fn test_point_in_polygon_not_enough_points() {
        let poly = vec![Point::new(0.0, 0.0), Point::new(10.0, 0.0)];
        assert!(!point_in_polygon(Point::new(5.0, 5.0), &poly));
    }

    #[test]
    fn test_point_in_polygon_outside_x() {
        let poly = vec![
            Point::new(0.0, 0.0),
            Point::new(10.0, 0.0),
            Point::new(10.0, 10.0),
            Point::new(0.0, 10.0),
        ];
        assert!(!point_in_polygon(Point::new(-5.0, 5.0), &poly));
    }

    #[test]
    fn test_point_in_polygon_outside_y() {
        let poly = vec![
            Point::new(0.0, 0.0),
            Point::new(10.0, 0.0),
            Point::new(10.0, 10.0),
            Point::new(0.0, 10.0),
        ];
        assert!(!point_in_polygon(Point::new(5.0, -5.0), &poly));
    }
}
