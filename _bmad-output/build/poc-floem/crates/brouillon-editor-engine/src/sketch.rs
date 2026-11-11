use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// Position coordinate with pressure and tilt stylus information.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq)]
pub struct StrokePoint {
    pub x: f32,
    pub y: f32,
    pub pressure: f32,
    pub tilt_x: f32,
    pub tilt_y: f32,
}

/// The set of high-fidelity drawing brushes matching reMarkable tools.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub enum SketchTool {
    Ballpoint,
    Fineliner,
    Marker,
    Pencil,
    MechanicalPencil,
    Calligraphy,
    Paintbrush,
    Highlighter,
    Shader,
    Eraser,
    LassoSelect,
}

impl SketchTool {
    /// Friendly label for UI menus and palette options.
    pub fn label(self) -> &'static str {
        match self {
            SketchTool::Ballpoint => "Stylo à bille",
            SketchTool::Fineliner => "Feutre fin",
            SketchTool::Marker => "Feutre",
            SketchTool::Pencil => "Crayon de bois",
            SketchTool::MechanicalPencil => "Portemine",
            SketchTool::Calligraphy => "Plume calligraphie",
            SketchTool::Paintbrush => "Pinceau",
            SketchTool::Highlighter => "Surligneur",
            SketchTool::Shader => "Estompe",
            SketchTool::Eraser => "Gomme",
            SketchTool::LassoSelect => "Lasso de sélection",
        }
    }
}

/// The color palette supported by the drawing canvas.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub enum SketchColor {
    Black,
    Gray,
    White,
    Red,
    Blue,
    Green,
    Yellow,
    Cyan,
    Magenta,
}

impl SketchColor {
    /// Return the raw hex color representation.
    pub fn rgb(self) -> (u8, u8, u8) {
        match self {
            SketchColor::Black => (26, 27, 38), // Tokyo Night dark base
            SketchColor::Gray => (120, 120, 128),
            SketchColor::White => (255, 255, 255),
            SketchColor::Red => (247, 118, 142), // red
            SketchColor::Blue => (122, 162, 247), // blue
            SketchColor::Green => (158, 206, 106), // green
            SketchColor::Yellow => (224, 175, 104), // orange-yellow
            SketchColor::Cyan => (125, 207, 255),
            SketchColor::Magenta => (187, 154, 247), // purple-magenta
        }
    }

    /// Return the transparent alpha color value for brushes.
    pub fn alpha(self, tool: SketchTool) -> f32 {
        match tool {
            SketchTool::Highlighter => 0.35, // very transparent overlay
            SketchTool::Shader => 0.15,      // soft shading layers
            SketchTool::Pencil => 0.18,      // beautiful faint pencil graphite
            _ => 1.0,
        }
    }
}

/// A single continuous vector path drawn on the sketchbook.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Stroke {
    pub points: Vec<StrokePoint>,
    pub tool: SketchTool,
    pub color: SketchColor,
    pub width: f32,
    pub layer_idx: usize,
    #[serde(default)]
    pub is_eraser: bool,
}

/// A drawing page with vector strokes, layers, and templates.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SketchPage {
    pub strokes: Vec<Stroke>,
    pub layers: Vec<String>,
    pub template: String,        // "Blank", "Dots", "Grid", "Lines"
    pub infinite_canvas: bool,   // User preference layout toggle
    #[serde(default)]
    pub landscape: bool,
    #[serde(skip)]
    pub redo_strokes: Vec<Stroke>,
}

impl Default for SketchPage {
    fn default() -> Self {
        Self {
            strokes: Vec::new(),
            layers: vec!["Dessin".to_string(), "Annotations".to_string()],
            template: "Blank".to_string(),
            infinite_canvas: false, // defaults to Fixed A4 layout
            landscape: false,
            redo_strokes: Vec::new(),
        }
    }
}

impl SketchPage {
    /// Undo the last drawn stroke. Returns true if successful.
    pub fn undo_stroke(&mut self) -> bool {
        if let Some(stroke) = self.strokes.pop() {
            self.redo_strokes.push(stroke);
            true
        } else {
            false
        }
    }

    /// Redo the last undone stroke. Returns true if successful.
    pub fn redo_stroke(&mut self) -> bool {
        if let Some(stroke) = self.redo_strokes.pop() {
            self.strokes.push(stroke);
            true
        } else {
            false
        }
    }
}


/// A collection of pages comprising a digital sketchbook document.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct SketchDocument {
    pub pages: Vec<SketchPage>,
    pub active_page_idx: usize,
}

impl SketchDocument {
    /// Create a fresh empty sketchbook.
    pub fn new() -> Self {
        Self {
            pages: vec![SketchPage::default()],
            active_page_idx: 0,
        }
    }

    /// Save the sketchbook vector document into a JSON formatted string.
    pub fn to_json(&self) -> Result<String, serde_json::Error> {
        serde_json::to_string_pretty(self)
    }

    /// Load the sketchbook vector document from a JSON formatted string.
    pub fn from_json(json: &str) -> Result<Self, serde_json::Error> {
        serde_json::from_str(json)
    }

    /// Number of strokes over all pages (what "has drawing" and "unsaved
    /// drawing" are measured with).
    pub fn stroke_count(&self) -> usize {
        self.pages.iter().map(|p| p.strokes.len()).sum()
    }

    /// Where the drawing of the text file `text_path` is stored: the same name
    /// with `.sketch.json` appended (`notes.txt` -> `notes.txt.sketch.json`).
    pub fn sidecar_path(text_path: &Path) -> PathBuf {
        let mut name = text_path.file_name().map(|n| n.to_os_string()).unwrap_or_default();
        name.push(".sketch.json");
        text_path.with_file_name(name)
    }

    /// Write the drawing next to `text_path` (durable write).
    pub fn save_sidecar(&self, text_path: &Path) -> std::io::Result<()> {
        let json = self.to_json().map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
        crate::durable::durable_write(&Self::sidecar_path(text_path), json.as_bytes())
    }

    /// Read the drawing stored next to `text_path`, if any and valid.
    pub fn load_sidecar(text_path: &Path) -> Option<Self> {
        let json = std::fs::read_to_string(Self::sidecar_path(text_path)).ok()?;
        Self::from_json(&json).ok()
    }

    /// Add a new empty page.
    pub fn add_page(&mut self) {
        self.pages.push(SketchPage::default());
        self.active_page_idx = self.pages.len() - 1;
    }

    /// Return the active drawing page.
    pub fn active_page(&self) -> &SketchPage {
        &self.pages[self.active_page_idx]
    }

    /// Return a mutable reference to the active drawing page.
    pub fn active_page_mut(&mut self) -> &mut SketchPage {
        &mut self.pages[self.active_page_idx]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_stroke() -> Stroke {
        Stroke {
            points: vec![
                StrokePoint { x: 10.0, y: 20.0, pressure: 0.5, tilt_x: 0.0, tilt_y: 0.0 },
                StrokePoint { x: 30.0, y: 40.0, pressure: 0.8, tilt_x: 0.1, tilt_y: 0.0 },
            ],
            tool: SketchTool::Ballpoint,
            color: SketchColor::Black,
            width: 2.5,
            layer_idx: 0,
            is_eraser: false,
        }
    }

    #[test]
    fn stroke_count_and_sidecar_round_trip() {
        let mut doc = SketchDocument::new();
        assert_eq!(doc.stroke_count(), 0);
        doc.active_page_mut().strokes.push(sample_stroke());
        doc.add_page();
        doc.active_page_mut().strokes.push(sample_stroke());
        assert_eq!(doc.stroke_count(), 2);

        let dir = std::env::temp_dir().join(format!("brouillon-sketch-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let text = dir.join("notes.txt");
        assert_eq!(SketchDocument::sidecar_path(&text), dir.join("notes.txt.sketch.json"));
        assert!(SketchDocument::load_sidecar(&text).is_none());
        doc.save_sidecar(&text).unwrap();
        let back = SketchDocument::load_sidecar(&text).expect("sidecar reloads");
        assert_eq!(back.stroke_count(), 2);
        assert_eq!(back.pages.len(), 2);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn default_page_has_sane_defaults() {
        let page = SketchPage::default();
        assert!(page.strokes.is_empty());
        assert_eq!(page.layers.len(), 2);
        assert_eq!(page.template, "Blank");
        assert!(!page.infinite_canvas);
        assert!(!page.landscape);
        assert!(page.redo_strokes.is_empty());
    }

    #[test]
    fn undo_stroke_roundtrip() {
        let mut page = SketchPage::default();
        page.strokes.push(sample_stroke());
        assert_eq!(page.strokes.len(), 1);

        assert!(page.undo_stroke());
        assert!(page.strokes.is_empty());
        assert_eq!(page.redo_strokes.len(), 1);

        assert!(page.redo_stroke());
        assert_eq!(page.strokes.len(), 1);
        assert!(page.redo_strokes.is_empty());
    }

    #[test]
    fn undo_empty_page_returns_false() {
        let mut page = SketchPage::default();
        assert!(!page.undo_stroke());
    }

    #[test]
    fn redo_empty_returns_false() {
        let mut page = SketchPage::default();
        assert!(!page.redo_stroke());
    }

    #[test]
    fn multiple_undo_redo() {
        let mut page = SketchPage::default();
        for _ in 0..5 {
            page.strokes.push(sample_stroke());
        }
        assert_eq!(page.strokes.len(), 5);

        for i in (0..5).rev() {
            assert!(page.undo_stroke());
            assert_eq!(page.strokes.len(), i);
        }
        assert_eq!(page.redo_strokes.len(), 5);

        for i in 1..=5 {
            assert!(page.redo_stroke());
            assert_eq!(page.strokes.len(), i);
        }
    }

    #[test]
    fn document_new_has_one_page() {
        let doc = SketchDocument::new();
        assert_eq!(doc.pages.len(), 1);
        assert_eq!(doc.active_page_idx, 0);
    }

    #[test]
    fn document_add_page_switches_active() {
        let mut doc = SketchDocument::new();
        doc.add_page();
        assert_eq!(doc.pages.len(), 2);
        assert_eq!(doc.active_page_idx, 1);
    }

    #[test]
    fn document_serde_roundtrip() {
        let mut doc = SketchDocument::new();
        doc.active_page_mut().strokes.push(sample_stroke());
        doc.active_page_mut().template = "Grid".to_string();

        let json = doc.to_json().expect("serialize");
        let doc2 = SketchDocument::from_json(&json).expect("deserialize");
        assert_eq!(doc2.pages.len(), 1);
        assert_eq!(doc2.active_page().strokes.len(), 1);
        assert_eq!(doc2.active_page().template, "Grid");
    }

    #[test]
    fn sketch_color_rgb_values() {
        let (r, g, b) = SketchColor::Black.rgb();
        assert_eq!((r, g, b), (26, 27, 38));
        let (r, g, b) = SketchColor::Red.rgb();
        assert_eq!((r, g, b), (247, 118, 142));
    }

    #[test]
    fn sketch_tool_labels_are_non_empty() {
        let tools = [
            SketchTool::Ballpoint, SketchTool::Fineliner, SketchTool::Marker,
            SketchTool::Pencil, SketchTool::MechanicalPencil, SketchTool::Calligraphy,
            SketchTool::Paintbrush, SketchTool::Highlighter, SketchTool::Shader,
            SketchTool::Eraser, SketchTool::LassoSelect,
        ];
        for tool in tools {
            assert!(!tool.label().is_empty(), "Tool {:?} has empty label", tool);
        }
    }
}
