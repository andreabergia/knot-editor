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
//! `create_backend` runs before the harness has a window, so GPU state
//! is constructed lazily inside `init` (same pattern as `wgpu_cosmic`).

#![cfg(target_os = "macos")]

use anyhow::{Context, Result};
use objc2::rc::Retained;
use objc2::runtime::ProtocolObject;
use objc2_app_kit::NSView;
use objc2_core_foundation::CGSize;
use objc2_metal::{
    MTLCommandBuffer, MTLCommandQueue, MTLCreateSystemDefaultDevice, MTLDevice, MTLDrawable,
    MTLPixelFormat,
};
use objc2_quartz_core::{CAMetalDrawable, CAMetalLayer};
use skia_safe::{
    Color, Color4f, ColorType, FontMgr, FontStyle, Point,
    gpu::{self, DirectContext, SurfaceOrigin, backend_render_targets, mtl},
    textlayout::{FontCollection, ParagraphBuilder, ParagraphStyle, TextStyle},
};
use winit::raw_window_handle::{HasWindowHandle, RawWindowHandle};
use winit::window::Window;

use crate::view::{Renderer, Segment};

/// Font size in logical pixels — parity with `wgpu_cosmic` and the
/// harness's `LINE_HEIGHT_PX` assumption.
const FONT_SIZE: f32 = 16.0;
const LINE_HEIGHT: f32 = 20.0;

pub struct Skia {
    gpu: Option<Gpu>,
    font_collection: Option<FontCollection>,
    frames: u64,
}

struct Gpu {
    context: DirectContext,
    // Held to keep the MTLDevice alive for skia's Metal backend context
    // (which stores only a raw handle). The CAMetalLayer retains it too,
    // but we keep our own reference to be explicit about lifetime.
    #[allow(dead_code)]
    device: Retained<ProtocolObject<dyn MTLDevice>>,
    command_queue: Retained<ProtocolObject<dyn MTLCommandQueue>>,
    /// Retained; attached to the NSView in `init`. Kept here so the
    /// layer outlives individual drawables and we can resize it.
    metal_layer: Retained<CAMetalLayer>,
    width: u32,
    height: u32,
}

impl Skia {
    pub fn new() -> Self {
        Self {
            gpu: None,
            font_collection: None,
            frames: 0,
        }
    }
}

impl Renderer for Skia {
    fn init(&mut self, window: &Window) -> Result<()> {
        let device = MTLCreateSystemDefaultDevice().context("no Metal device found")?;
        let command_queue = device
            .newCommandQueue()
            .context("MTLDevice::newCommandQueue")?;

        let (w, h) = {
            let s = window.inner_size();
            (s.width.max(1), s.height.max(1))
        };

        // Create + configure the CAMetalLayer and attach it to the
        // window's NSView. Mirrors skia-safe's metal-window example.
        let metal_layer = {
            let layer = CAMetalLayer::new();
            layer.setDevice(Some(&device));
            layer.setPixelFormat(MTLPixelFormat::BGRA8Unorm);
            layer.setPresentsWithTransaction(false);
            // Skia's blend mode needs a non-framebuffer-only surface.
            layer.setFramebufferOnly(false);
            // vsync off — parity with wgpu_cosmic's PresentMode::AutoNoVsync
            // so frame times reflect shaping + raster cost, not the refresh cap.
            layer.setDisplaySyncEnabled(false);
            layer.setDrawableSize(CGSize::new(w as f64, h as f64));

            let view_ptr = match window
                .window_handle()
                .context("window handle")?
                .as_raw()
            {
                RawWindowHandle::AppKit(appkit) => appkit.ns_view.as_ptr() as *mut NSView,
                other => anyhow::bail!(
                    "skia backend expects an AppKit window handle, got {other:?}"
                ),
            };
            let view = unsafe { view_ptr.as_ref() }.context("NSView pointer was null")?;
            view.setWantsLayer(true);
            view.setLayer(Some(&layer.clone().into_super()));
            layer
        };

        let backend = unsafe {
            mtl::BackendContext::new(
                Retained::as_ptr(&device) as mtl::Handle,
                Retained::as_ptr(&command_queue) as mtl::Handle,
            )
        };
        let context = gpu::direct_contexts::make_metal(&backend, None)
            .context("make_metal DirectContext")?;

        // Platform FontMgr + monospace default: this is the load-bearing
        // "for free" piece for CJK/Arabic/emoji fallback that the step-2
        // plan asks skia to bring to the comparison.
        let mut font_collection = FontCollection::new();
        font_collection.set_default_font_manager(FontMgr::new(), Some("monospace"));
        eprintln!(
            "[skia] Metal device ready; font collection initialized \
             (system FontMgr, default family \"monospace\")"
        );

        self.font_collection = Some(font_collection);
        self.gpu = Some(Gpu {
            context,
            device,
            command_queue,
            metal_layer,
            width: w,
            height: h,
        });
        Ok(())
    }

    fn resize(&mut self, width: u32, height: u32) {
        let Some(gpu) = self.gpu.as_mut() else {
            return;
        };
        if width == 0 || height == 0 {
            return;
        }
        gpu.metal_layer
            .setDrawableSize(CGSize::new(width as f64, height as f64));
        gpu.width = width;
        gpu.height = height;
    }

    fn render_frame(&mut self, _offset: usize, visible: &[Vec<Segment>]) -> Result<()> {
        let Some(gpu) = self.gpu.as_mut() else {
            return Ok(());
        };
        let Some(font_collection) = self.font_collection.as_ref() else {
            return Ok(());
        };

        let drawable = match gpu.metal_layer.nextDrawable() {
            Some(d) => d,
            None => return Ok(()),
        };
        let (dw, dh) = {
            let size = gpu.metal_layer.drawableSize();
            (size.width as i32, size.height as i32)
        };

        let tex = drawable.texture();
        let mut surface = {
            let texture_info =
                unsafe { mtl::TextureInfo::new(Retained::as_ptr(&tex) as mtl::Handle) };
            let backend_render_target =
                backend_render_targets::make_mtl((dw, dh), &texture_info);
            gpu::surfaces::wrap_backend_render_target(
                &mut gpu.context,
                &backend_render_target,
                SurfaceOrigin::TopLeft,
                ColorType::BGRA8888,
                None,
                None,
            )
            .context("wrap_backend_render_target")?
        };

        // Build + paint the paragraph. Scoped so the canvas borrow ends
        // before we flush and drop the surface.
        {
            let canvas = surface.canvas();
            canvas.clear(Color4f::new(0.0, 0.0, 0.0, 1.0));

            // Default text style: monospace 16px, 20px line height via a
            // 1.25x height multiplier with half-leading (centers glyphs
            // in the line, matching cosmic-text's Metrics behavior).
            let mut default_style = TextStyle::new();
            default_style
                .set_font_families(&["monospace"])
                .set_font_size(FONT_SIZE)
                .set_font_style(FontStyle::normal())
                .set_color(Color::from_rgb(0xC0, 0xC0, 0xC0))
                .set_height(LINE_HEIGHT / FONT_SIZE)
                .set_height_override(true)
                .set_half_leading(true);

            let mut para_style = ParagraphStyle::new();
            para_style.set_text_style(&default_style);

            let mut builder = ParagraphBuilder::new(&para_style, (*font_collection).clone());
            for (i, line) in visible.iter().enumerate() {
                if i > 0 {
                    builder.add_text("\n");
                }
                if line.is_empty() {
                    // Non-empty text keeps the paragraph from collapsing a
                    // blank line; mirrors wgpu_cosmic's space-for-empty.
                    builder.add_text(" ");
                    continue;
                }
                for seg in line {
                    let mut style = default_style.clone();
                    style
                        .set_color(color_to_skia(seg.color))
                        .set_font_style(font_style_for(seg.bold, seg.italic));
                    builder.push_style(&style);
                    builder.add_text(seg.text);
                    builder.pop();
                }
            }
            let mut paragraph = builder.build();
            paragraph.layout(dw as f32);
            paragraph.paint(canvas, Point::new(0.0, 0.0));

            if self.frames < 3 {
                eprintln!(
                    "[skia] size={}x{} lines={} height={:.1}",
                    dw,
                    dh,
                    paragraph.get_line_metrics().len(),
                    paragraph.height(),
                );
            }
        }

        gpu.context.flush_and_submit();
        drop(surface);

        // Present the drawable via a Metal command buffer (skia's GPU
        // work is already submitted; this is just the present blit).
        let command_buffer = gpu
            .command_queue
            .commandBuffer()
            .context("commandBuffer")?;
        let drawable_ref: Retained<ProtocolObject<dyn MTLDrawable>> = (&drawable).into();
        command_buffer.presentDrawable(&drawable_ref);
        command_buffer.commit();

        self.frames += 1;
        Ok(())
    }

    fn teardown(&mut self) {
        eprintln!("[skia] rendered {} frames", self.frames);
    }
}

fn color_to_skia(c: u32) -> Color {
    Color::from_rgb(
        ((c >> 16) & 0xFF) as u8,
        ((c >> 8) & 0xFF) as u8,
        (c & 0xFF) as u8,
    )
}

fn font_style_for(bold: bool, italic: bool) -> FontStyle {
    match (bold, italic) {
        (true, true) => FontStyle::bold_italic(),
        (true, false) => FontStyle::bold(),
        (false, true) => FontStyle::italic(),
        (false, false) => FontStyle::normal(),
    }
}
