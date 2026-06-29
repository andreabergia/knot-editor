# Step 2 Task 7 — skia Backend Implementation Plan

Execution plan for step 7 of `docs/step2-renderer-benchmark.md`: implement the
skia (`skia-safe`) backend so it can be benchmarked alongside `wgpu_cosmic`.

## Goal

Add a third `Renderer` backend that represents the "batteries-included extreme"
called for by the step-2 plan (`docs/step2-renderer-benchmark.md:185-190`):
skia with its own HarfBuzz+ICU shaping, BiDi, font fallback, and layout. The
contrast against the manual wgpu + cosmic-text path is the whole point of the
comparison. This task delivers the backend and a visible smoke test; the full
benchmark run + `step2-bench-results.md` update is step 8.

## Decisions

- **GPU backend: Metal via `raw-window-handle`.** macOS-native, no GL/glutin.
  Get the `NSView` from winit's `HasWindowHandle`, attach a `CAMetalLayer`,
  render skia into the layer's drawables. Keeps us off deprecated OpenGL on
  macOS.
- **Text API: `textlayout::Paragraph`.** High-level FontCollection +
  ParagraphBuilder + TextStyle: HarfBuzz+ICU shaping, BiDi, fallback, and
  word-wrap all built in. The "what does production give us for free"
  baseline the plan asks for.
- **Scope: implement + smoke-test render.** Backend compiles, wires into
  `create_backend`/`available_backends`, and renders one fixture visibly.
  Defer the full benchmark + results doc to step 8.
- **Parity with `wgpu_cosmic`** so the comparison is apples-to-apples:
  - Monospace font family, 16px font size, 20px line height — matches the
    harness's `LINE_HEIGHT_PX` assumption and keeps visible-line count
    identical across backends.
  - Disable vsync via `CAMetalLayer.displaySyncEnabled = NO` (parity with
    `wgpu_cosmic`'s `PresentMode::AutoNoVsync`) so frame times reflect
    shaping + raster cost, not the display refresh cap.
  - First-3-frames `eprintln!` debug output (run/glyph/vertex counts) like
    `wgpu_cosmic`.
  - Flatten visible lines with `\n` separators, push one `TextStyle` per
    segment — same flattening strategy as `wgpu_cosmic`'s `set_rich_text`.
  - `offset` ignored: the harness pre-computes the visible range and passes
    it in, exactly as `wgpu_cosmic` does.

## Architecture

```
src/view/backends/
  mod.rs            add `pub mod skia;`
  skia.rs           NEW — skia-safe (Metal + textlayout::Paragraph)
```

`src/view/mod.rs` — extend `create_backend` with a `"skia"` match arm, add
`"skia"` to `available_backends`, and update the unknown-backend error
message string.

### `skia.rs` shape (mirrors `wgpu_cosmic`'s lazy-init pattern)

```rust
pub struct Skia {
    gpu: Option<Gpu>,
    font_collection: Option<FontCollection>,
    frames: u64,
}

struct Gpu {
    context: DirectContext,
    device: MTLDevice,
    queue: MTLCommandQueue,
    metal_layer: CAMetalLayer,  // retained; attached to the NSView
    width: u32,
    height: u32,
}
```

- `new()` — all fields `None`/zero. `create_backend` runs before the harness
  has a window, so GPU state is constructed lazily inside `init` (same
  pattern as `wgpu_cosmic`).
- `init(window)`:
  1. System-default `MTLDevice` + a `MTLCommandQueue` off it.
  2. Create a `CAMetalLayer`, set `device`, set
     `displaySyncEnabled = NO` (vsync off, parity with `AutoNoVsync`).
  3. Get the `NSView` from `window` via `HasWindowHandle` and attach the
     layer: `setWantsLayer:YES` + `setLayer:`. Retain the layer.
  4. `DirectContext::new_metal(BackendContext { device, queue })`.
  5. Build a `FontCollection` with the platform `FontMgr` — this is the
     load-bearing "for free" piece for CJK/Arabic/emoji fallback.
  6. Stash everything in `self.gpu` / `self.font_collection`.
- `resize(w, h)`:
  - Set `metal_layer.set_drawable_size(w * scale, h * scale)` (scale = 1 for
    now; the harness window is fixed at 1200x800 physical).
- `render_frame(_offset, visible)`:
  1. `metal_layer.nextDrawable()` → its texture →
     `BackendRenderTarget::new_metal(...)`.
  2. `Surface::from_backend_render_target(...)`.
  3. Build a `Paragraph`:
     - `ParagraphStyle` with monospace default.
     - For each visible line, for each segment: push a `TextStyle`
       (color = `Segment.color`, bold/italic via `FontStyle`), append the
       segment text. Insert `\n` between lines (skia treats `\n` as a hard
       break, same as cosmic-text's paragraph split).
     - `layout(surface_width)`.
     - `paint(canvas, 0, 0)`.
  4. `context.flush_and_submit()`; `drawable.present()`.
- `teardown()`: `eprintln!` frame count (parity with `wgpu_cosmic`).

## Dependencies (`Cargo.toml`)

- `skia-safe` with features `gpu`, `metal`, `textlayout` (plus whatever the
  platform `FontMgr` needs).
- `metal` crate (MTLDevice / MTLCommandQueue / MTLTexture).
- objc interop for `NSView` / `CAMetalLayer`. Exact crate set
  (`objc2` + `objc2-app-kit` + `objc2-quartz-core` vs older `objc` +
  `cocoa`) to be pinned to whatever skia-safe's current Metal example uses,
  to avoid FFI drift.
- `raw-window-handle` (already a transitive dep via winit; verify version).

## Smoke test

1. ✅ `cargo build` — clean, no warnings. `skia-bindings` used its
   default `binary-cache` feature (pre-built binaries), so the "long
   native compile" risk did **not** materialize: ~27s, no CMake/clang.
2. ✅ `bench --fixture bench/fixtures/rust_sample.kfx --backend skia
   --duration 2` — renders visibly; first-3-frames debug sane:
   `size=1200x800 lines=44 height=880.0` (880/44 = 20px/line — exact
   line-height parity with wgpu_cosmic), no panic, no notdef storm.
   Also verified cjk/emoji/arabic fixtures render without crashing
   (font fallback works).

## Doc updates

- Mark step 7 ✅ in `docs/step2-renderer-benchmark.md:210` (per
  `AGENTS.md`: keep plans updated with ✅ markers).
- Do **not** update `docs/step2-bench-results.md` yet — that's step 8.

## Commit

One commit: `Step 2 task 7: skia backend (Metal + textlayout::Paragraph)`.
If the smoke test reveals a meaningfully separate fixup, split it into a
second commit so the history stays readable.

## Out of scope (deferred to step 8)

- Running all fixtures against skia.
- Recording skia numbers in `docs/step2-bench-results.md`.
- The candidate-1-vs-3 comparison writeup and the primary-renderer
  decision.
