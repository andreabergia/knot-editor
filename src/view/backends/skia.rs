//! skia-safe backend (Metal + `textlayout::Paragraph`).
//!
//! The "batteries-included extreme": skia with its own HarfBuzz+ICU
//! shaping, BiDi, font fallback, and layout. The contrast against the
//! manual wgpu + cosmic-text path is the whole point of the step-2
//! comparison.
//!
//! GPU backend is Metal via `raw-window-handle`: we get the `NSView`
//! from winit's `HasWindowHandle`, attach a `CAMetalLayer`, and render
//! skia into the layer's drawables. Keeps us off deprecated OpenGL on
//! macOS.
//!
//! This is a macOS-only backend. On other targets the module is not
//! compiled and `create_backend("skia")` bails with the unknown-backend
//! error.
//!
//! Skeleton only for now — GPU state and `Renderer` impl land in the
//! next chunk. `create_backend` runs before the harness has a window, so
//! GPU state will be constructed lazily inside `init` (same pattern as
//! `wgpu_cosmic`).

#![cfg(target_os = "macos")]

use crate::view::Renderer;

pub struct Skia {
    frames: u64,
}

impl Skia {
    pub fn new() -> Self {
        Self { frames: 0 }
    }
}

impl Renderer for Skia {
    fn init(&mut self, _window: &winit::window::Window) -> anyhow::Result<()> {
        Ok(())
    }

    fn resize(&mut self, _width: u32, _height: u32) {}

    fn render_frame(&mut self, _offset: usize, _visible: &[Vec<crate::view::Segment>]) -> anyhow::Result<()> {
        self.frames += 1;
        Ok(())
    }

    fn teardown(&mut self) {
        eprintln!("[skia] rendered {} frames", self.frames);
    }
}
