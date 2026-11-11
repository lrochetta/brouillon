# tools/images

Share images, image pipeline and image checks for Brouillon. One small Rust program (resvg, oxipng, image-webp; no global install, no Node dependency).

```powershell
$env:CARGO_TARGET_DIR = "D:\travail\_target-images"   # keep build output outside the repo and off C:
cargo run --release --manifest-path tools/images/Cargo.toml -- all
```

| Command | What it does |
|---|---|
| `social` | Draws `assets/social/` (8 OG images 1200x630, `github-social-preview.png` 1280x640, `readme-banner*.svg/png` 1280x320 dark + light, `release-banner.png` 1200x600). Texts are converted to paths with the Inter / JetBrains Mono files in `fonts/` (OFL), so SVG and PNG look the same everywhere. Release banner version comes from `site/public/downloads/build-info.json`, or `--release-version X`. |
| `process` | Reads `assets/illustrations/*.svg`, `assets/infographics/*.svg`, `assets/screenshots/*.png`. SVG: safe minification (prolog, comments, metadata, editor attributes, layout whitespace; ids, `<title>`, `<desc>`, `<style>`, `var()` kept). PNG: oxipng, area-average downscale to 640/960/1280/1600/1920 widths (`name@2x.png` gives a 1x and a 2x), lossless WebP kept only when smaller. Writes `assets/optimized/**` and `assets/manifest.json`. |
| `check` | Exit code 1 when: file over budget; missing `<title>` (SVG) or alt EN/FR; forbidden third-party name; external URL; script / `foreignObject` / event handler in an SVG; text containing MCP or SSH without the word "preview" / "aperçu"; PNG metadata chunk; wrong size for a share image; source not yet processed. |
| `all` | `social`, `process`, `check`. |

Per-image metadata, in this order: `assets/<dir>/<name>.meta.json`, `assets/<dir>/_meta.json` (keyed by name), then an optional design seed `assets/seed.json` (alt EN/FR, `preview`, `max_kb`; override with `--seed`):

```json
{ "alt": { "en": "...", "fr": "..." }, "status": "delivered", "text": ["visible text of a PNG"] }
```

`status` is `delivered` or `preview`. `text` lists the words visible inside a PNG (the check cannot read pixels).

Budgets (bytes, in `src/check.rs`): illustration SVG 20 000, infographic SVG 30 000, WebP 90 000, screenshot PNG 500 000, OG 250 000, GitHub preview 900 000, README banner 200 000. A seed `max_kb` can only tighten an SVG budget.

Fonts in `fonts/` were derived once from `site/public/assets/fonts` (Inter variable instanced at wght 400 / 650 / 720, opsz 32; JetBrains Mono 400) with fontTools. Not needed to regenerate.

## README banner (GitHub light/dark)

```html
<picture>
  <source media="(prefers-color-scheme: dark)" srcset="assets/social/readme-banner-dark.svg">
  <source media="(prefers-color-scheme: light)" srcset="assets/social/readme-banner-light.svg">
  <img alt="Brouillon, a fast, local-first text editor. Free, open source (MIT), written in Rust." src="assets/social/readme-banner.svg" width="1280" height="320">
</picture>
```

GitHub social preview: upload `assets/social/github-social-preview.png` in Settings, Social preview (the API cannot set it).
