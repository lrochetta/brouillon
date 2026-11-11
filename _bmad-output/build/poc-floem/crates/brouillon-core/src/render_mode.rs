//! Which renderer is really drawing the window, as a pure value.
//!
//! `apply_render_backend` (in the binary) turns the `render.backend` setting into
//! two environment variables before the window exists; floem then reads them:
//! `FLOEM_FORCE_TINY_SKIA=1` selects the CPU rasteriser, otherwise it tries the
//! wgpu renderer, restricted by `WGPU_BACKEND`. This module reads that applied
//! state back, so the badge shows what runs, not what was asked for.
//!
//! Limit: floem 0.2.0 does not tell the application which renderer it ended up
//! with, nor the adapter name. When the GPU renderer fails to start it falls back
//! to the CPU rasteriser silently, so "GPU" here means "GPU was requested and
//! nothing forced the CPU", and the engine is only known when `WGPU_BACKEND`
//! names exactly one.

/// Graphics API wgpu was told to use.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GpuEngine {
    Direct3D12,
    Vulkan,
    OpenGl,
    Metal,
    /// `WGPU_BACKEND` unset or a list: wgpu picks.
    Unspecified,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RenderMode {
    Cpu,
    Gpu(GpuEngine),
}

impl GpuEngine {
    fn parse(wgpu_backend: Option<&str>) -> Self {
        match wgpu_backend.map(|s| s.trim().to_ascii_lowercase()).as_deref() {
            Some("dx12" | "d3d12") => Self::Direct3D12,
            Some("vulkan" | "vk") => Self::Vulkan,
            Some("gl" | "opengl" | "gles") => Self::OpenGl,
            Some("metal" | "mtl") => Self::Metal,
            _ => Self::Unspecified,
        }
    }

    /// Human name, `None` when unknown.
    pub fn name(self) -> Option<&'static str> {
        match self {
            Self::Direct3D12 => Some("Direct3D 12"),
            Self::Vulkan => Some("Vulkan"),
            Self::OpenGl => Some("OpenGL"),
            Self::Metal => Some("Metal"),
            Self::Unspecified => None,
        }
    }
}

impl RenderMode {
    /// Pure decision from the two variables (floem only honours the exact value "1").
    pub fn from_env(force_tiny_skia: Option<&str>, wgpu_backend: Option<&str>) -> Self {
        if force_tiny_skia == Some("1") {
            Self::Cpu
        } else {
            Self::Gpu(GpuEngine::parse(wgpu_backend))
        }
    }

    /// Short label shown next to the icon.
    pub fn short(self) -> &'static str {
        match self {
            Self::Cpu => "CPU",
            Self::Gpu(_) => "GPU",
        }
    }

    /// Label for the window title and accessibility: "CPU", "GPU", "GPU (Direct3D 12)".
    pub fn long(self) -> String {
        match self {
            Self::Gpu(e) => match e.name() {
                Some(n) => format!("GPU ({n})"),
                None => "GPU".to_string(),
            },
            Self::Cpu => "CPU".to_string(),
        }
    }

    /// Hover explanation. Figures come from one measured machine (re/43), hence "about".
    pub fn explanation(self, settings_file: &str) -> String {
        let tail = format!("Settings file: {settings_file}");
        match self {
            Self::Cpu => format!(
                "Rendering: CPU\n\
                 Software rasteriser: no graphics driver, lowest memory (about 60 MB idle), works everywhere including virtual machines. \
                 Smooth enough for normal windows; very large windows can be slower.\n\
                 To use your graphics card, set render.backend = \"gpu\" in settings.toml and restart.\n{tail}"
            ),
            Self::Gpu(_) => format!(
                "Rendering: {}\n\
                 Uses your graphics card: smoother on very large windows, more memory (about 150 MB idle). \
                 If the graphics adapter cannot start, the editor may draw on the CPU instead without saying so.\n\
                 To go back to the lighter renderer, set render.backend = \"cpu\" in settings.toml and restart.\n{tail}",
                self.long()
            ),
        }
    }
}

/// The renderer in effect, read from the environment applied at startup.
pub fn effective_render_mode() -> RenderMode {
    RenderMode::from_env(
        std::env::var("FLOEM_FORCE_TINY_SKIA").ok().as_deref(),
        std::env::var("WGPU_BACKEND").ok().as_deref(),
    )
}

/// 16 px badge icon on a 24 grid (live area 20, stroke 1.75, round caps/joins).
/// CPU = hollow chip with pins; GPU = hollow card with a filled fan (hollow vs
/// filled carries the difference without colour). `color` is `#rrggbb`.
pub fn icon_svg(mode: RenderMode, color: &str) -> String {
    let head = format!(
        "<svg xmlns=\"http://www.w3.org/2000/svg\" viewBox=\"0 0 24 24\" width=\"24\" height=\"24\" fill=\"none\" \
         stroke=\"{color}\" stroke-width=\"1.75\" stroke-linecap=\"round\" stroke-linejoin=\"round\">"
    );
    let body = match mode {
        RenderMode::Cpu => format!(
            "<rect x=\"6\" y=\"6\" width=\"12\" height=\"12\" rx=\"2\"/><rect x=\"10\" y=\"10\" width=\"4\" height=\"4\" rx=\"0.5\"/>\
             <path d=\"M9 2v4M15 2v4M9 18v4M15 18v4M2 9h4M2 15h4M18 9h4M18 15h4\"/>"
        ),
        RenderMode::Gpu(_) => format!(
            "<rect x=\"2\" y=\"5\" width=\"20\" height=\"13\" rx=\"2\"/><path d=\"M6 18v3\"/>\
             <circle cx=\"12\" cy=\"11.5\" r=\"3.5\" fill=\"{color}\"/>"
        ),
    };
    format!("{head}{body}</svg>")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn env_table() {
        use GpuEngine::*;
        let cases: &[(Option<&str>, Option<&str>, RenderMode)] = &[
            (Some("1"), Some("dx12"), RenderMode::Cpu), // default Windows CPU setup
            (Some("1"), None, RenderMode::Cpu),
            (Some("0"), Some("dx12"), RenderMode::Gpu(Direct3D12)), // floem only honours "1"
            (Some("true"), None, RenderMode::Gpu(Unspecified)),
            (None, Some("dx12"), RenderMode::Gpu(Direct3D12)),
            (None, Some("D3D12"), RenderMode::Gpu(Direct3D12)),
            (None, Some(" vulkan "), RenderMode::Gpu(Vulkan)),
            (None, Some("gl"), RenderMode::Gpu(OpenGl)),
            (None, Some("metal"), RenderMode::Gpu(Metal)),
            (None, Some("dx12,vulkan"), RenderMode::Gpu(Unspecified)),
            (None, None, RenderMode::Gpu(Unspecified)),
        ];
        for (f, w, want) in cases {
            assert_eq!(RenderMode::from_env(*f, *w), *want, "{f:?} {w:?}");
        }
    }

    #[test]
    fn labels() {
        assert_eq!(RenderMode::Cpu.short(), "CPU");
        assert_eq!(RenderMode::Cpu.long(), "CPU");
        assert_eq!(RenderMode::Gpu(GpuEngine::Vulkan).short(), "GPU");
        assert_eq!(RenderMode::Gpu(GpuEngine::Direct3D12).long(), "GPU (Direct3D 12)");
        assert_eq!(RenderMode::Gpu(GpuEngine::Unspecified).long(), "GPU");
    }

    #[test]
    fn explanations_name_the_setting_and_path() {
        let cpu = RenderMode::Cpu.explanation("C:\\x\\settings.toml");
        assert!(cpu.starts_with("Rendering: CPU"));
        assert!(cpu.contains("render.backend = \"gpu\"") && cpu.contains("C:\\x\\settings.toml"));
        let gpu = RenderMode::Gpu(GpuEngine::Direct3D12).explanation("p");
        assert!(gpu.starts_with("Rendering: GPU (Direct3D 12)"));
        assert!(gpu.contains("render.backend = \"cpu\""));
        assert!(!cpu.contains("faster") && !gpu.contains("faster"));
    }

    #[test]
    fn icons_are_valid_and_differ() {
        let a = icon_svg(RenderMode::Cpu, "#ffffff");
        let b = icon_svg(RenderMode::Gpu(GpuEngine::Unspecified), "#ffffff");
        assert_ne!(a, b);
        for s in [&a, &b] {
            assert!(s.contains("#ffffff") && s.ends_with("</svg>"));
        }
    }
}
