# tools/icons

Regenerates the raster brand assets from the SVG sources in `assets/brand/`.

```powershell
cargo run --release --manifest-path tools/icons/Cargo.toml
# optional review sheet (not committed): $env:ICON_PREVIEW_DIR = "D:\somewhere"
```

Keep `CARGO_TARGET_DIR` outside the repo (the crate pulls in resvg, about 60 crates; its build output is not committed).

Outputs: `assets/brand/brouillon.ico` (16, 20, 24, 32, 40, 48, 64 as 32-bit DIB, 256 as PNG), `brouillon-{16,32,48,64,128,256,512,1024}.png`,
`brouillon.iconset/` (macOS `iconutil` layout) and
`_bmad-output/build/poc-floem/crates/brouillon/assets/window-icon-32.rgba` (raw RGBA for the window icon).

Sizes up to 32 px are rendered from `brouillon-icon-small.svg`, larger ones from `brouillon-icon.svg`.
The site favicon is built from `brouillon-icon.svg` by `site/scripts/build.mjs`.
