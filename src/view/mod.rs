//! Rendering and view trait.
//!
//! Roadmap step 2: minimal `Renderer` seed trait plus a benchmark harness
//! that drives pluggable backends through the same workload. This is a
//! comparative evaluation, not the editor's renderer.

pub mod backends;
pub mod bench;
pub mod fixture;

use winit::window::Window;

/// A styled text run. Bold/italic force every backend to exercise its
/// multi-attribute shaping path, not just color runs.
pub struct Segment<'a> {
    pub text: &'a str,
    /// 0xRRGGBB
    pub color: u32,
    pub bold: bool,
    pub italic: bool,
}

/// Minimal renderer seed. Grows during step 7 into the full `View` surface.
///
/// `visible` is the list of lines currently on screen, each as a list of
/// styled segments. Backends are intentionally ignorant of measurement;
/// the harness collects metrics around `render_frame`.
pub trait Renderer {
    fn init(&mut self, window: &Window) -> anyhow::Result<()>;
    fn resize(&mut self, width: u32, height: u32);
    fn render_frame(&mut self, offset: usize, visible: &[Vec<Segment>]) -> anyhow::Result<()>;
    fn teardown(&mut self);
}

/// Backend selector, driven by the `--backend` bench flag.
pub fn create_backend(name: &str) -> anyhow::Result<Box<dyn Renderer>> {
    match name {
        "stub" => Ok(Box::new(backends::stub::Stub::new()) as Box<dyn Renderer>),
        "wgpu_cosmic" => Ok(Box::new(backends::wgpu_cosmic::WgpuCosmic::new()) as Box<dyn Renderer>),
        other => anyhow::bail!(
            "unknown backend `{other}`; expected one of: stub, wgpu_cosmic \
             (wgpu_direct, skia arrive in tasks 6-7)"
        ),
    }
}

pub fn available_backends() -> &'static [&'static str] {
    &["stub", "wgpu_cosmic"]
}
