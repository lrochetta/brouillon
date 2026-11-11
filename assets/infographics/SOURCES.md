# Infographic sources

Sources are internal measurement reports that are not published in this repository. Machine for all measures: Windows 11 Pro, Intel Iris Xe iGPU (shared memory), Avast active, release build. Dates: reports dated 2026-11-05. Status: measured / objective / deduced (computed from measured values or from code, not measured itself).

## memory.svg
| Figure | Source | Method | Status |
|---|---|---|---|
| 294.6 MB empty doc, Vulkan (original) | re/43 sec 1, 3 | working set, median of 5 launches | measured |
| 151.7 MB (151.6-153.4) empty doc, Direct3D 12 | re/43 sec 1 | median of 5 | measured |
| 60.9 MB (60.9-61.1) empty doc, CPU renderer | re/43 sec 1 | median of 5 | measured |
| 351.7 MB (344.6-359.5) 100k lines, Vulkan | re/43 sec 1 | median of 5 | measured |
| 201.0 MB (183.5-203.5) 100k lines, D3D12 | re/43 sec 1 | median, n not stated in report | measured |
| 94.2 MB (92.2-97.9) 100k lines, CPU | re/43 sec 1 | median, n not stated in report | measured |
| 7.3 MB / 100,000-line file | re/43 sec 3 | file size | measured |
| 80 MB | re/43 sec 1 (budget "Idle RAM < 80 MB") | dashed line | objective |
| CPU renderer is the default | git commit b7a8369 (2026-11-05), main.rs `DEFAULT_RENDER_BACKEND` | code | measured (fact) |
Not drawn: wgpu-only floor of 116 MB (risks/104, other prototype), 29 MB spike (risks/104): different programs.

## recovery-flow.svg
| Claim | Source | Status |
|---|---|---|
| Journal written every 30 s | re/42 sec 3 (30 s autosave to the journal) | deduced from code; "up to 30 s can be missing" is deduced |
| Atomic fsynced write | re/45 bug 4 (durable_write; unit tests) | deduced, tests only |
| kill -9, relaunch, recovered text, dot on tab and sidebar, status message | re/44 scenario k; re/45 bug 2 (after-k2-relaunch.png) | seen on screen |
| Normal close keeps unsaved tabs modified | re/45 bug 2 (after-d5-reopened.png) | seen on screen |
| Drawing sidecar `.sketch.json` | re/45 bug 1 (after-o4-reopened-drawn.png) | seen on screen |
| Not restored: scroll/fold; journal wins over changed file; share fallback | re/45 "Not done / still open" | stated limits |

## architecture.svg
Line counts: `find crates -name '*.rs' | xargs cat | wc -l` per crate in `_bmad-output/build/poc-floem/crates` (HEAD e75b00f plus uncommitted work tree), measured 2026-11-05. Blank lines, comments and inline tests included.
brouillon 4,701; brouillon-ui 11,025; brouillon-editor-engine 21,938; brouillon-import 584; brouillon-core 4,414 (measured). Total 42,662 and core+engine+import 26,936 (deduced: sums). Stubs (crates/stubs, same count method): tokio 243, wasmtime 150, loro 114, wasmtime-wasi 30 (measured). Dependency edges: each crate's Cargo.toml (drawn simplified). floem use: one file in core (theme.rs), one in engine (cursor.rs), none in import (grep). 939 tests passed: re/45 header (measured, before later merges). Binary 14.69 MB: re/43 sec 1. Counts move as work continues: regenerate before publishing.

## status-matrix.svg
Each row cites its evidence in the figure: re/44 scenarios a-l (seen on screen), re/45 bugs 1-4 (seen / unit tests), re/42 sec 4 items 1-17 (not wired), crates/stubs and sftp.rs (local-folder model, no SSH library), plugin_host.rs (wasmtime stub), crdt.rs (loro stub), mcp_client.rs (tokio stub), discovery/vault (study only). "9 of 30 languages seen": re/44 scenario l. 100 MB row: re/44 scenario i. All statuses: deduced classification of the reports.

## syntax-languages.svg
29 `.toml` definitions in `crates/brouillon-editor-engine/src/syntax/langs/` (counted by listing, 2026-11-05, measured) + Rust via tree-sitter (re/43 sec 4, grammars.rs). 9 seen coloured: C, PHP (with HTML), HTML, SQL, Python, Rust, JSON, Markdown, Shell (re/44 scenario l, seen on screen). The "30" is deduced (29 + 1).

## performance-100mb.svg
| Figure | Source | Method | Status |
|---|---|---|---|
| 211.4 ms read + decode (207.0-211.5) | re/43 sec 5 | cargo bench, 109.6 MB / 1.5 M lines, median of 3 | measured |
| 50.3 ms rope build | re/43 sec 5 | median of 3 | measured |
| About 350 ms open in the window, 1,050,000 lines | re/44 scenario i | one run, harness | measured |
| 164 ms literal count; 44.1 ms regex count | re/43 sec 5 | median of 3 | measured |
| 113 ms for 1,000,000 matches over 51 MB | re/42 sec 5 | perf_count_50mb, lto thin, one run | measured |
| 312-374 ms per key | re/44 scenario i | build 110, three keys | measured |
| 0.1 us insert, 0.2 us undo, 23 us fetch 80 lines | re/43 sec 5 | medians (n=200, 100, 300) | measured |
| 16.6 ms frame | 1000 / 60 | arithmetic | deduced |
Caveat kept: "cause is repaint, not the edit" is a reading of the two reports side by side (engine insert 0.1 us vs window 312-374 ms), not a profile.
