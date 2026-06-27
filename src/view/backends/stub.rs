//! No-op backend for harness validation.
//!
//! Does no rendering and no shaping: it exists so the harness can be
//! validated end-to-end (loop, metrics, fixture loading) before any real
//! backend lands. Any numbers it reports reflect harness overhead only.

use winit::window::Window;

use crate::view::{Renderer, Segment};

pub struct Stub {
    frames: u64,
}

impl Stub {
    pub fn new() -> Self {
        Self { frames: 0 }
    }
}

impl Renderer for Stub {
    fn init(&mut self, _window: &Window) -> anyhow::Result<()> {
        Ok(())
    }

    fn resize(&mut self, _width: u32, _height: u32) {}

    fn render_frame(&mut self, _offset: usize, visible: &[Vec<Segment>]) -> anyhow::Result<()> {
        // Touch the data so the optimizer can't elide the work entirely;
        // we want the harness to reflect the cost of walking the visible
        // lines, which is the floor any real backend will pay.
        let mut total = 0usize;
        for line in visible {
            for seg in line {
                total += seg.text.len();
            }
        }
        // Sink the result so the loop above is not dead.
        std::hint::black_box(total);
        self.frames += 1;
        Ok(())
    }

    fn teardown(&mut self) {
        eprintln!("[stub] rendered {} frames", self.frames);
    }
}
