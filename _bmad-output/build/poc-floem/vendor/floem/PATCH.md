# floem 0.2.0, patched copy

Upstream floem 0.2.0 (MIT) with the changes below, wired through `[patch.crates-io]` in the workspace `Cargo.toml`:

- `src/views/text_input.rs`: the cursor-blink repaint. Upstream queued a new 500 ms timer on every paint and never cancelled
  the previous ones, so every repaint from another cause (status bar, typing, heartbeat) added a repaint chain that never
  ended. Paints per second climbed until a core was pinned (measured 97-100 %), and the chains outlived the field.
  The patch keeps one pending timer per field and only while it has the focus.
- `src/window_handle.rs` (`paint`): trims cosmic-text's shape-run cache after every paint (`SHAPE_RUN_CACHE_KEEP_PAINTS`).
  The `shape-run-cache` feature is on, the cache is keyed by the full text of each run and nothing in floem ever evicted it:
  every distinct text ever laid out (a line growing by one character per keystroke, each search result, each scrolled page)
  stayed in memory for the life of the process. Measured on a 2 MB file, typing 3 characters at 14 keys/s on one growing
  line: +130..220 MB/min before, +5 MB/min after. This is the main leak.
- `src/id.rs` (`ViewId::remove`, new `is_alive`) and `src/window_handle.rs` (`process_central_messages`): `remove` only dropped
  the id from `view_ids`; the view, its `ViewState`, its child list and its parent link stayed in the secondary maps, and
  `ViewId::view()` kept returning the removed view. A message still queued for it (the first update of a `dyn_stack` that its
  `dyn_container` replaced in the same batch) ran on that zombie, which built a new child tree under a dead id that nothing
  ever removed (one popup label tree per keystroke). `remove` now takes the entries out (dropped after the storage borrow is
  released), and queued messages for a removed view are discarded.
- `src/window_handle.rs`, `src/context.rs` (`PaintState`), `src/renderer.rs` (`Renderer::new`, `should_request_gpu`): with the CPU
  rasteriser selected (`FLOEM_FORCE_TINY_SKIA=1`) upstream still called `GpuResources::request` for every window, creating a
  wgpu instance, surface, adapter and D3D12 device that tiny-skia never uses (and `unwrap`-panicking when no adapter exists).
  Now no GPU resources are requested in CPU mode (`GpuResourcesUpdate` is sent directly to start the renderer), `Renderer::new`
  takes `Option<GpuResources>`, and in GPU mode a failed adapter/device request falls back to tiny-skia instead of panicking.
  The GPU path is otherwise unchanged. `floem_renderer` (crates.io) needs no patch: it only defines the `GpuResources` type.
- `src/app_handle.rs`, `src/window.rs`, `src/lib.rs` (`set_close_guard`): upstream closes the window unconditionally on
  `CloseRequested`. A host-installed guard (GUI thread) can now veto it, used to ask before quitting with unsaved hex edits.
- `src/lib.rs`: `#![allow(warnings)]`, because a path dependency, unlike a registry one, shows the crate's own warnings.

Remove this directory and the patch line when upstream fixes it.
