# Bundled fonts

This directory will hold the curated font set bundled into the release
binary (closes the ~50 MB fontdb tax — measured in the early proof of concept).

## Status

Empty placeholder. Real fonts are pending:
1. License review (we want Apache-2 or OFL only).
2. Repo size decision (likely <8 MB total ; if larger, evaluate git LFS).
3. Cross-OS rendering test on each weight.

## Candidate set

- **JetBrains Mono** regular + bold + italic (Apache 2) — ~600 KB total
- **Inter** regular + medium (OFL) — ~400 KB total

Total ~1 MB ; binary grows from 9.77 MB to ~10.8 MB ; well under <30 MB budget.

## Integration notes

See `src/fonts.rs`. The view layer (M1 round 3) will call `fonts::bundled_fonts()`
at startup and feed each entry into the cosmic-text `FontSystem`.

Whether Floem 0.2 lets us inject a pre-built `FontSystem` is documented at
the top of `src/fonts.rs`. **Outcome (C) — partial bypass**: we can add
fonts to the existing `floem::text::FONT_SYSTEM` via `.lock().db_mut()`,
but we cannot prevent fontdb's system-font enumeration without an upstream
Floem patch (target: `renderer/src/text/layout.rs:11`).
