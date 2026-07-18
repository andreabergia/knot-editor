//! wgpu + cosmic-text backend.
//!
//! Low-level GPU rendering via wgpu, shaping and layout via cosmic-text
//! (which uses harfrust, the HarfBuzz port, internally). Glyphs are
//! rasterized on the CPU through cosmic-text's `SwashCache` and uploaded
//! into a fixed RGBA8 texture atlas; the frame is a single textured-quad
//! draw call over the visible layout runs.
//!
//! This is the "build it ourselves" path: it exposes where the hard
//! problems live (atlas management, mask vs. color glyphs, subpixel
//! binning) without hiding them behind a retained-mode layout machine.
//!
//! Because `create_backend` runs before the harness has a window, GPU
//! state is constructed lazily inside `init`. Until then `gpu` is `None`
//! and the backend is inert.

use std::collections::HashMap;
use std::mem;

use anyhow::{Context, Result};
use bytemuck::{Pod, Zeroable};
use cosmic_text::{
    Attrs, Buffer, CacheKey, Color, Family, FontSystem, Metrics, Shaping, Style, SwashCache,
    SwashContent, Weight,
};
use wgpu::{
    BlendState, BufferUsages, ColorTargetState, ColorWrites, Device, FragmentState, LoadOp,
    MultisampleState, PipelineLayoutDescriptor, PrimitiveState, Queue, RenderPassDescriptor,
    RenderPipeline, SamplerBindingType, ShaderStages, StoreOp, Surface, SurfaceConfiguration,
    TextureFormat, TextureUsages, TextureViewDimension, VertexState,
};
use winit::window::Window;

use crate::view::{Renderer, Segment};

/// Glyph atlas edge length. Plenty for the fixture set; if exhausted we
/// simply drop new glyphs (acceptable for a benchmark — visible
/// degeneration is obvious and the metric still reflects shaping cost).
const ATLAS_SIZE: u32 = 2048;
/// Padding between atlas entries to avoid texture bleeding at quad edges.
const ATLAS_PAD: u32 = 1;

/// Font size in logical pixels; matches the harness's `LINE_HEIGHT_PX`
/// assumption closely enough that the visible-line count stays sane.
const FONT_SIZE: f32 = 16.0;
const LINE_HEIGHT: f32 = 20.0;
const INITIAL_VERTEX_CAPACITY: u64 = 1024 * 16;
const VERTEX_SIZE: u64 = mem::size_of::<Vertex>() as u64;

pub struct WgpuCosmic {
    gpu: Option<Gpu>,
    /// Reusable CPU-side scratch for the cosmic-text buffer, kept across
    /// frames so the per-frame allocation is just `clear()`.
    font_system: Option<FontSystem>,
    swash: Option<SwashCache>,
    buffer: Option<Buffer>,
    frames: u64,
}

struct Gpu {
    device: Device,
    queue: Queue,
    surface: Surface<'static>,
    config: SurfaceConfiguration,
    pipeline: RenderPipeline,
    atlas: Atlas,
    atlas_bind_group: wgpu::BindGroup,
    vertex_buf: wgpu::Buffer,
    vertex_capacity: u64,
}

impl WgpuCosmic {
    pub fn new() -> Self {
        Self {
            gpu: None,
            font_system: None,
            swash: None,
            buffer: None,
            frames: 0,
        }
    }
}

// Atlas entry: where a cached glyph lives in the atlas texture.
#[derive(Clone, Copy)]
struct AtlasEntry {
    x: u32,
    y: u32,
    w: u32,
    h: u32,
    /// True for `SwashContent::Color` (e.g. emoji); the shader emits the
    /// texture's RGB directly instead of tinting by the vertex color.
    color: bool,
}

struct Atlas {
    texture: wgpu::Texture,
    view: wgpu::TextureView,
    entries: HashMap<CacheKey, AtlasEntry>,
    /// Shelf-pack cursor: next free x, current row top, current row height.
    cursor_x: u32,
    cursor_y: u32,
    row_height: u32,
}

impl Atlas {
    fn new(device: &Device, queue: &Queue) -> Self {
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("knot atlas"),
            size: wgpu::Extent3d {
                width: ATLAS_SIZE,
                height: ATLAS_SIZE,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: TextureFormat::Rgba8Unorm,
            usage: TextureUsages::COPY_DST | TextureUsages::TEXTURE_BINDING,
            view_formats: &[],
        });
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        let zero = vec![0u8; (ATLAS_SIZE * ATLAS_SIZE * 4) as usize];
        queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            &zero,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(ATLAS_SIZE * 4),
                rows_per_image: Some(ATLAS_SIZE),
            },
            wgpu::Extent3d {
                width: ATLAS_SIZE,
                height: ATLAS_SIZE,
                depth_or_array_layers: 1,
            },
        );
        Self {
            texture,
            view,
            entries: HashMap::new(),
            cursor_x: 0,
            cursor_y: 0,
            row_height: 0,
        }
    }

    /// Allocate a rectangle of `w`×`h` atlas pixels. Returns `None` if the
    /// atlas is exhausted (we don't grow it — see module docs).
    fn alloc(&mut self, w: u32, h: u32) -> Option<(u32, u32)> {
        let w = w + ATLAS_PAD;
        let h = h + ATLAS_PAD;
        if w > ATLAS_SIZE {
            return None;
        }
        if self.cursor_x + w > ATLAS_SIZE {
            self.cursor_y += self.row_height;
            self.cursor_x = 0;
            self.row_height = 0;
        }
        if self.cursor_y + h > ATLAS_SIZE {
            return None;
        }
        let (x, y) = (self.cursor_x, self.cursor_y);
        self.cursor_x += w;
        if h > self.row_height {
            self.row_height = h;
        }
        Some((x, y))
    }
}

// Vertex layout. Two triangles per glyph, drawn as a list.
#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct Vertex {
    // Normalized device coordinates (already converted from pixels on CPU).
    pos: [f32; 2],
    uv: [f32; 2],
    color: [f32; 4],
    /// 1.0 for color glyphs, 0.0 for mask glyphs.
    color_glyph: f32,
}

impl Renderer for WgpuCosmic {
    fn init(&mut self, window: &Window) -> Result<()> {
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle());
        // The harness owns the `Window` and keeps it alive for the entire
        // run (it lives in `State::Running` alongside the renderer, and
        // the renderer is torn down first). We can therefore safely hand
        // wgpu raw handles and claim a `'static` surface — no need to
        // take ownership of the window, which the `Renderer` trait
        // doesn't give us anyway.
        let target = unsafe {
            wgpu::SurfaceTargetUnsafe::from_display_and_window(window, window)
                .map_err(|e| anyhow::anyhow!("window handle error: {e}"))?
        };
        let surface =
            unsafe { instance.create_surface_unsafe(target) }.context("creating wgpu surface")?;

        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::HighPerformance,
            force_fallback_adapter: false,
            compatible_surface: Some(&surface),
        }))
        .context("requesting wgpu adapter")?;

        let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
            label: Some("knot device"),
            required_features: wgpu::Features::empty(),
            required_limits: wgpu::Limits::downlevel_defaults(),
            experimental_features: wgpu::ExperimentalFeatures::disabled(),
            memory_hints: wgpu::MemoryHints::default(),
            trace: wgpu::Trace::Off,
        }))
        .context("requesting wgpu device")?;

        let caps = surface.get_capabilities(&adapter);
        let format = caps
            .formats
            .iter()
            .copied()
            .find(|&f| f.is_srgb())
            .unwrap_or(caps.formats[0]);
        let size = window.inner_size();
        let config = SurfaceConfiguration {
            usage: TextureUsages::RENDER_ATTACHMENT,
            format,
            width: size.width.max(1),
            height: size.height.max(1),
            // The benchmark measures raw shaping + rasterization cost;
            // vsync would cap us at the display refresh rate and hide the
            // work we are trying to time. AutoNoVsync lets the platform
            // pick the fastest non-vsynced mode available.
            present_mode: wgpu::PresentMode::AutoNoVsync,
            desired_maximum_frame_latency: 2,
            alpha_mode: wgpu::CompositeAlphaMode::Auto,
            view_formats: vec![],
        };
        surface.configure(&device, &config);

        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("knot shader"),
            source: wgpu::ShaderSource::Wgsl(SHADER.into()),
        });

        let atlas = Atlas::new(&device, &queue);
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("knot sampler"),
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            address_mode_w: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });
        let bind_group_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("knot bind layout"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(SamplerBindingType::Filtering),
                    count: None,
                },
            ],
        });
        let atlas_bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("knot bind group"),
            layout: &bind_group_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&atlas.view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(&sampler),
                },
            ],
        });
        let pipeline_layout = device.create_pipeline_layout(&PipelineLayoutDescriptor {
            label: Some("knot pipeline layout"),
            bind_group_layouts: &[Some(&bind_group_layout)],
            immediate_size: 0,
        });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("knot pipeline"),
            layout: Some(&pipeline_layout),
            vertex: VertexState {
                module: &shader,
                entry_point: Some("vs_main"),
                compilation_options: Default::default(),
                buffers: &[wgpu::VertexBufferLayout {
                    array_stride: VERTEX_SIZE,
                    step_mode: wgpu::VertexStepMode::Vertex,
                    attributes: &vertex_attrs(),
                }],
            },
            fragment: Some(FragmentState {
                module: &shader,
                entry_point: Some("fs_main"),
                compilation_options: Default::default(),
                targets: &[Some(ColorTargetState {
                    format,
                    blend: Some(BlendState::ALPHA_BLENDING),
                    write_mask: ColorWrites::ALL,
                })],
            }),
            primitive: PrimitiveState::default(),
            depth_stencil: None,
            multisample: MultisampleState::default(),
            multiview_mask: None,
            cache: None,
        });

        let vertex_buf = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("knot vertices"),
            size: INITIAL_VERTEX_CAPACITY,
            usage: BufferUsages::VERTEX | BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        let font_system = FontSystem::new();
        let db_count = font_system.db().faces().count();
        let cjk_fonts = font_system
            .db()
            .faces()
            .filter(|f| {
                f.post_script_name.contains("PingFang")
                    || f.post_script_name.contains("Hei")
                    || f.post_script_name.contains("Song")
            })
            .count();
        eprintln!(
            "[wgpu_cosmic] fontdb loaded {} faces, {} CJK candidates",
            db_count, cjk_fonts
        );
        let swash = SwashCache::new();
        let mut buffer = Buffer::new_empty(Metrics::new(FONT_SIZE, LINE_HEIGHT));
        buffer.set_size(Some(config.width as f32), Some(config.height as f32));

        self.font_system = Some(font_system);
        self.swash = Some(swash);
        self.buffer = Some(buffer);
        self.gpu = Some(Gpu {
            device,
            queue,
            surface,
            config,
            pipeline,
            atlas,
            atlas_bind_group,
            vertex_buf,
            vertex_capacity: INITIAL_VERTEX_CAPACITY,
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
        gpu.config.width = width;
        gpu.config.height = height;
        gpu.surface.configure(&gpu.device, &gpu.config);
        if let Some(buf) = self.buffer.as_mut() {
            buf.set_size(Some(width as f32), Some(height as f32));
        }
    }

    fn render_frame(&mut self, _offset: usize, visible: &[Vec<Segment>]) -> Result<()> {
        let Some(gpu) = self.gpu.as_mut() else {
            return Ok(());
        };
        let Some(font_system) = self.font_system.as_mut() else {
            return Ok(());
        };
        let Some(swash) = self.swash.as_mut() else {
            return Ok(());
        };
        let Some(buffer) = self.buffer.as_mut() else {
            return Ok(());
        };

        // 1. Rebuild the buffer's text from the visible segments. We
        //    flatten into a single string joined by `\n` (cosmic-text
        //    treats each `\n` as a paragraph break) and supply styled
        //    spans for `set_rich_text`. Each segment carries its own
        //    color + bold/italic, so the multi-attribute shaping path is
        //    exercised per segment, not just per line.
        //
        //    `set_rich_text` reconstructs its own internal string by
        //    concatenating the span texts it is handed — it does NOT use
        //    a caller-provided string. Therefore the `\n` separators
        //    MUST be inside some span (here: their own default-attrs
        //    span); if they are emitted outside of any span the
        //    concatenated string ends up with no newlines and the whole
        //    visible range collapses to one paragraph / one buffer line.
        let default_attrs = Attrs::new().family(Family::Monospace);
        let mut text = String::new();
        // (start, end, attrs) — indices into `text`. Attrs are `'static`
        // (Family::Monospace + Color/Weight/Style are all owned).
        let mut ranges: Vec<(usize, usize, Attrs<'static>)> = Vec::new();
        for (i, line) in visible.iter().enumerate() {
            if i > 0 {
                let start = text.len();
                text.push('\n');
                ranges.push((start, text.len(), default_attrs.clone()));
            }
            if line.is_empty() {
                let start = text.len();
                text.push(' ');
                ranges.push((start, text.len(), default_attrs.clone()));
                continue;
            }
            for seg in line {
                let start = text.len();
                text.push_str(seg.text);
                let attrs = default_attrs
                    .clone()
                    .color(color_to_cosmic(seg.color))
                    .weight(if seg.bold {
                        Weight::BOLD
                    } else {
                        Weight::NORMAL
                    })
                    .style(if seg.italic {
                        Style::Italic
                    } else {
                        Style::Normal
                    });
                ranges.push((start, text.len(), attrs));
            }
        }
        let spans: Vec<(&str, Attrs)> = ranges
            .iter()
            .map(|(s, e, a)| (&text[*s..*e], a.clone()))
            .collect();
        buffer.set_rich_text(spans.into_iter(), &default_attrs, Shaping::Advanced, None);

        // 2. Shape + lay out. We avoid `borrow_with` here because it
        //    holds `&mut FontSystem` for as long as we walk layout runs,
        //    and we need `&mut FontSystem` again inside `ensure_glyph`
        //    (via swash). `shape_until_scroll` resolves dirty state up
        //    front; `layout_runs` borrows only `&buffer`.
        buffer.set_size(
            Some(gpu.config.width as f32),
            Some(gpu.config.height as f32),
        );
        buffer.shape_until_scroll(font_system, true);

        // 3. Walk layout runs, rasterize new glyphs into the atlas, and
        //    emit vertices. Positions come out of cosmic-text in physical
        //    pixels relative to the buffer's top-left; we convert to NDC
        //    on the CPU so the shader stays uniform-free.
        let (surf_w, surf_h) = (gpu.config.width as f32, gpu.config.height as f32);
        let scale = 1.0;
        let mut vertices: Vec<Vertex> = Vec::new();
        let mut dbg_min_y = f32::INFINITY;
        let mut dbg_max_y = f32::NEG_INFINITY;
        let mut dbg_runs = 0usize;
        let mut dbg_glyphs = 0usize;
        let mut dbg_notdef = 0usize;
        let mut dbg_no_entry = 0usize;
        for run in buffer.layout_runs() {
            dbg_runs += 1;
            let baseline = run.line_y;
            dbg_min_y = dbg_min_y.min(run.line_top);
            dbg_max_y = dbg_max_y.max(run.line_top + run.line_height);
            for glyph in run.glyphs.iter() {
                dbg_glyphs += 1;
                if glyph.glyph_id == 0 {
                    dbg_notdef += 1;
                }
                let physical = glyph.physical((glyph.x, baseline), scale);
                let color = glyph.color_opt.unwrap_or(Color::rgb(0xC0, 0xC0, 0xC0));
                let entry = match ensure_glyph(swash, font_system, gpu, physical.cache_key) {
                    Some(e) => e,
                    None => {
                        dbg_no_entry += 1;
                        continue;
                    }
                };
                if entry.w == 0 || entry.h == 0 {
                    continue;
                }
                let px = physical.x as f32;
                let py = physical.y as f32;
                let pw = entry.w as f32;
                let ph = entry.h as f32;
                let (u0, v0) = uv(entry.x, entry.y);
                let (u1, v1) = uv(entry.x + entry.w, entry.y + entry.h);
                let col = color_to_array(color);
                let cg = if entry.color { 1.0 } else { 0.0 };
                push_quad(
                    &mut vertices,
                    [px, py],
                    [pw, ph],
                    [u0, v0, u1, v1],
                    col,
                    cg,
                    surf_w,
                    surf_h,
                );
            }
        }
        if self.frames < 3 {
            eprintln!(
                "[wgpu_cosmic] size={}x{} runs={} glyphs={} notdef={} no_entry={} verts={} y_range=[{}..{}]",
                gpu.config.width,
                gpu.config.height,
                dbg_runs,
                dbg_glyphs,
                dbg_notdef,
                dbg_no_entry,
                vertices.len(),
                dbg_min_y,
                dbg_max_y
            );
        }

        // 4. Acquire the surface texture.
        let frame = match gpu.surface.get_current_texture() {
            wgpu::CurrentSurfaceTexture::Success(t) => t,
            wgpu::CurrentSurfaceTexture::Suboptimal(t) => t,
            wgpu::CurrentSurfaceTexture::Outdated | wgpu::CurrentSurfaceTexture::Lost => {
                gpu.surface.configure(&gpu.device, &gpu.config);
                return Ok(());
            }
            wgpu::CurrentSurfaceTexture::Timeout | wgpu::CurrentSurfaceTexture::Occluded => {
                return Ok(());
            }
            wgpu::CurrentSurfaceTexture::Validation => {
                anyhow::bail!("surface validation error");
            }
        };
        let view = frame
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());

        let mut encoder = gpu
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor::default());

        if vertices.is_empty() {
            // Clear-only frame.
            let _pass = encoder.begin_render_pass(&RenderPassDescriptor {
                label: Some("knot clear"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: LoadOp::Clear(wgpu::Color::BLACK),
                        store: StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
        } else {
            let bytes = bytemuck::cast_slice(&vertices);
            let needed = (vertices.len() as u64) * VERTEX_SIZE;
            if needed > gpu.vertex_capacity {
                let mut new_cap = gpu.vertex_capacity.max(INITIAL_VERTEX_CAPACITY);
                while needed > new_cap {
                    new_cap *= 2;
                }
                // Allocate `new_cap` bytes (not `needed`) so subsequent
                // frames that fit within `new_cap` can `write_buffer`
                // without re-growing.
                gpu.vertex_buf = gpu.device.create_buffer(&wgpu::BufferDescriptor {
                    label: Some("knot vertices"),
                    size: new_cap,
                    usage: BufferUsages::VERTEX | BufferUsages::COPY_DST,
                    mapped_at_creation: false,
                });
                gpu.vertex_capacity = new_cap;
            }
            gpu.queue.write_buffer(&gpu.vertex_buf, 0, bytes);

            let mut pass = encoder.begin_render_pass(&RenderPassDescriptor {
                label: Some("knot render"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: LoadOp::Clear(wgpu::Color::BLACK),
                        store: StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            pass.set_pipeline(&gpu.pipeline);
            pass.set_bind_group(0, &gpu.atlas_bind_group, &[]);
            pass.set_vertex_buffer(0, gpu.vertex_buf.slice(..));
            pass.draw(0..vertices.len() as u32, 0..1);
        }

        gpu.queue.submit(std::iter::once(encoder.finish()));
        frame.present();
        self.frames += 1;
        Ok(())
    }

    fn teardown(&mut self) {
        let cached = self
            .gpu
            .as_ref()
            .map(|g| g.atlas.entries.len())
            .unwrap_or(0);
        eprintln!(
            "[wgpu_cosmic] rendered {} frames, {} glyphs cached",
            self.frames, cached
        );
    }
}

/// Rasterize a glyph into the atlas if not already present.
fn ensure_glyph(
    swash: &mut SwashCache,
    font_system: &mut FontSystem,
    gpu: &mut Gpu,
    cache_key: CacheKey,
) -> Option<AtlasEntry> {
    if let Some(&e) = gpu.atlas.entries.get(&cache_key) {
        return Some(e);
    }
    let image = swash.get_image(font_system, cache_key).as_ref()?.clone();
    let placement = image.placement;
    if placement.width == 0 || placement.height == 0 {
        let entry = AtlasEntry {
            x: 0,
            y: 0,
            w: 0,
            h: 0,
            color: false,
        };
        gpu.atlas.entries.insert(cache_key, entry);
        return Some(entry);
    }
    let (ax, ay) = gpu.atlas.alloc(placement.width, placement.height)?;
    let mut buf = Vec::with_capacity((placement.width * placement.height * 4) as usize);
    match image.content {
        SwashContent::Mask | SwashContent::SubpixelMask => {
            for &a in &image.data {
                buf.push(255);
                buf.push(255);
                buf.push(255);
                buf.push(a);
            }
        }
        SwashContent::Color => {
            buf.extend_from_slice(&image.data);
        }
    }
    gpu.queue.write_texture(
        wgpu::TexelCopyTextureInfo {
            texture: &gpu.atlas.texture,
            mip_level: 0,
            origin: wgpu::Origin3d { x: ax, y: ay, z: 0 },
            aspect: wgpu::TextureAspect::All,
        },
        &buf,
        wgpu::TexelCopyBufferLayout {
            offset: 0,
            bytes_per_row: Some(placement.width * 4),
            rows_per_image: Some(placement.height),
        },
        wgpu::Extent3d {
            width: placement.width,
            height: placement.height,
            depth_or_array_layers: 1,
        },
    );
    let entry = AtlasEntry {
        x: ax,
        y: ay,
        w: placement.width,
        h: placement.height,
        color: matches!(image.content, SwashContent::Color),
    };
    gpu.atlas.entries.insert(cache_key, entry);
    Some(entry)
}

const SHADER: &str = r#"
struct VsIn {
    @location(0) pos: vec2<f32>,
    @location(1) uv: vec2<f32>,
    @location(2) color: vec4<f32>,
    @location(3) color_glyph: f32,
};

struct VsOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) uv: vec2<f32>,
    @location(1) color: vec4<f32>,
    @location(2) color_glyph: f32,
};

@group(0) @binding(0) var t_atlas: texture_2d<f32>;
@group(0) @binding(1) var s_atlas: sampler;

@vertex
fn vs_main(in: VsIn) -> VsOut {
    var out: VsOut;
    out.clip = vec4<f32>(in.pos, 0.0, 1.0);
    out.uv = in.uv;
    out.color = in.color;
    out.color_glyph = in.color_glyph;
    return out;
}

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
    let tex = textureSample(t_atlas, s_atlas, in.uv);
    let mask = vec4<f32>(in.color.rgb, tex.a * in.color.a);
    let color = vec4<f32>(tex.rgb, tex.a);
    return mix(mask, color, in.color_glyph);
}
"#;

fn vertex_attrs() -> [wgpu::VertexAttribute; 4] {
    wgpu::vertex_attr_array![0 => Float32x2, 1 => Float32x2, 2 => Float32x4, 3 => Float32]
}

fn uv(x: u32, y: u32) -> (f32, f32) {
    (x as f32 / ATLAS_SIZE as f32, y as f32 / ATLAS_SIZE as f32)
}

#[allow(clippy::too_many_arguments)]
fn push_quad(
    out: &mut Vec<Vertex>,
    pos: [f32; 2],
    size: [f32; 2],
    uv_rect: [f32; 4],
    color: [f32; 4],
    color_glyph: f32,
    surf_w: f32,
    surf_h: f32,
) {
    // Pixel coords → NDC. Y is flipped: screen-space y grows downward,
    // NDC y grows upward.
    let to_ndc =
        |px: f32, py: f32| -> [f32; 2] { [px / surf_w * 2.0 - 1.0, 1.0 - py / surf_h * 2.0] };
    let [u0, v0, u1, v1] = uv_rect;
    let p00 = to_ndc(pos[0], pos[1]);
    let p10 = to_ndc(pos[0] + size[0], pos[1]);
    let p11 = to_ndc(pos[0] + size[0], pos[1] + size[1]);
    let p01 = to_ndc(pos[0], pos[1] + size[1]);
    let corners = [
        (p00, [u0, v0]),
        (p10, [u1, v0]),
        (p11, [u1, v1]),
        (p00, [u0, v0]),
        (p11, [u1, v1]),
        (p01, [u0, v1]),
    ];
    for (cp, cuv) in corners {
        out.push(Vertex {
            pos: cp,
            uv: cuv,
            color,
            color_glyph,
        });
    }
}

fn color_to_cosmic(c: u32) -> Color {
    Color::rgb(
        ((c >> 16) & 0xFF) as u8,
        ((c >> 8) & 0xFF) as u8,
        (c & 0xFF) as u8,
    )
}

fn color_to_array(c: Color) -> [f32; 4] {
    let [r, g, b, a] = c.as_rgba();
    [
        r as f32 / 255.0,
        g as f32 / 255.0,
        b as f32 / 255.0,
        a as f32 / 255.0,
    ]
}
