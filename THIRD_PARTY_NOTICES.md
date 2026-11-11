# Third-party notices

Brouillon is MIT-licensed (see LICENSE). It includes or depends on the following third-party material.

## Vendored source

- `_bmad-output/build/poc-floem/vendor/floem`: floem 0.2.0, Copyright (c) 2023 Floem, MIT licence (its LICENSE file is kept in that folder). Local changes are listed in `vendor/floem/PATCH.md`.

## Fonts (OFL 1.1)

- Inter and JetBrains Mono, used by the website and the image tooling (`site/public/assets/fonts`, `tools/images/fonts`). Their licence texts are next to the font files (`OFL-Inter.txt`, `OFL-JetBrainsMono.txt`).

## Rust dependencies

Resolved versions are pinned in the `Cargo.lock` files (`_bmad-output/build/poc-floem`, `remote-helper`, `docs-helper`, `tools/*`). Each crate keeps its own licence. Release builds publish CycloneDX SBOMs (see `docs/SIGNING.md`). The `crates/stubs/*` crates are local placeholders written for this project (not copies of the upstream crates).
