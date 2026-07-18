//! Workload, harness loop, and metrics collection.
//!
//! The harness owns the event loop, drives the selected backend through a
//! fixed advancing scroll offset, and collects frame time / peak RSS /
//! CPU% around the run. Backends stay ignorant of measurement.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use sysinfo::{MemoryRefreshKind, Pid, ProcessRefreshKind, ProcessesToUpdate, RefreshKind, System};
use winit::application::ApplicationHandler;
use winit::event::WindowEvent;
use winit::event_loop::{ActiveEventLoop, EventLoop};
use winit::window::{Window, WindowId};

use crate::view::fixture::Fixture;
use crate::view::{Renderer, Segment, available_backends, create_backend};

/// Run duration per fixture per backend.
const RUN_DURATION: Duration = Duration::from_secs(5);

/// Window size for the run (not resizable during the run).
const WIN_W: u32 = 1200;
const WIN_H: u32 = 800;

/// Approximate line height in pixels for the workload's visible-range
/// computation. The stub backend renders nothing, so the exact value
/// doesn't matter for it; real backends will report their own metrics.
const LINE_HEIGHT_PX: u32 = 20;

/// Overscan above/below the visible window.
const OVERSCAN_LINES: usize = 4;

/// One line per frame scroll delta.
const SCROLL_DELTA_LINES: usize = 1;

pub struct BenchConfig {
    pub fixture: PathBuf,
    pub backend: String,
    pub duration: Option<Duration>,
    /// If set, the loaded fixture is tiled (repeated) this many times in
    /// memory to synthesize a large fixture — used for fixture 2 (1M-line
    /// tiled Rust). The on-disk fixture stays small.
    pub tile: Option<usize>,
}

pub fn run(cfg: BenchConfig) -> Result<()> {
    let mut fixture = Fixture::load(&cfg.fixture)?;
    if let Some(tile) = cfg.tile {
        fixture = fixture.tiled(tile);
    }
    eprintln!(
        "[bench] fixture={} lines={} backend={}",
        cfg.display_fixture(),
        fixture.line_count(),
        cfg.backend,
    );

    // Construct the backend before entering the event loop so a bad
    // backend name fails with a clean error rather than a panic
    // mid-loop.
    let renderer = create_backend(&cfg.backend)?;

    let event_loop = EventLoop::new()?;
    let mut harness = Harness::new(cfg, fixture, renderer);
    event_loop.run_app(&mut harness)?;
    harness.report();
    Ok(())
}

struct Harness {
    pending_renderer: Option<Box<dyn Renderer>>,
    fixture_display: String,
    backend_name: String,
    fixture: Fixture,
    duration: Duration,
    state: State,
}

enum State {
    /// Waiting for the window to come up.
    Pending,
    Running {
        renderer: Box<dyn Renderer>,
        window: Window,
        deadline: Instant,
        offset: usize,
        frame_times: Vec<Duration>,
        peak_rss: u64,
        cpu_samples: Vec<f32>,
        sys: System,
        pid: Pid,
        last_metrics_sample: Instant,
    },
    Done {
        summary: Summary,
    },
}

#[derive(Clone, Copy)]
struct Summary {
    frames: usize,
    p50_us: u64,
    p99_us: u64,
    peak_rss_bytes: u64,
    cpu_avg: f32,
}

impl BenchConfig {
    fn display_fixture(&self) -> &str {
        self.fixture
            .file_name()
            .map(|s| s.to_str().unwrap_or("<non-utf8>"))
            .unwrap_or("<no-name>")
    }
}

impl Harness {
    fn new(cfg: BenchConfig, fixture: Fixture, renderer: Box<dyn Renderer>) -> Self {
        let duration = cfg.duration.unwrap_or(RUN_DURATION);
        Self {
            pending_renderer: Some(renderer),
            fixture_display: cfg.display_fixture().to_owned(),
            backend_name: cfg.backend.clone(),
            fixture,
            duration,
            state: State::Pending,
        }
    }

    fn report(&self) {
        if let State::Done { summary } = &self.state {
            let s = *summary;
            println!(
                "{}\t{}\tframes={}\tp50={:.2}ms\tp99={:.2}ms\tpeak_rss={:.1}MiB\tcpu={:.1}%",
                self.fixture_display,
                self.backend_name,
                s.frames,
                s.p50_us as f64 / 1000.0,
                s.p99_us as f64 / 1000.0,
                s.peak_rss_bytes as f64 / (1024.0 * 1024.0),
                s.cpu_avg,
            );
        }
    }
}

impl ApplicationHandler for Harness {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if !matches!(self.state, State::Pending) {
            return;
        }

        let window = event_loop
            .create_window(
                Window::default_attributes()
                    .with_title(format!("knot bench — {}", self.backend_name))
                    .with_inner_size(winit::dpi::PhysicalSize::new(WIN_W, WIN_H)),
            )
            .expect("window creation failed");

        let mut renderer = self
            .pending_renderer
            .take()
            .expect("resumed called twice without a renderer; this is a harness bug");
        renderer.init(&window).expect("backend init failed");

        let now = Instant::now();
        let deadline = now + self.duration;
        let mut sys = System::new();
        sys.refresh_memory();
        let pid = Pid::from_u32(std::process::id());
        sys.refresh_processes_specifics(
            ProcessesToUpdate::Some(&[pid]),
            true,
            ProcessRefreshKind::new().with_cpu().with_memory(),
        );
        std::thread::sleep(sysinfo::MINIMUM_CPU_UPDATE_INTERVAL);

        self.state = State::Running {
            renderer,
            window,
            deadline,
            offset: 0,
            frame_times: Vec::new(),
            peak_rss: 0,
            cpu_samples: Vec::new(),
            sys,
            pid,
            last_metrics_sample: now,
        };
    }

    fn window_event(
        &mut self,
        event_loop: &ActiveEventLoop,
        _window_id: WindowId,
        event: WindowEvent,
    ) {
        match event {
            WindowEvent::CloseRequested => {
                event_loop.exit();
            }
            WindowEvent::RedrawRequested => {
                self.tick();
                if matches!(self.state, State::Done { .. }) {
                    event_loop.exit();
                }
            }
            WindowEvent::Resized(_) => {
                // Window size is fixed for the run; ignore resizes.
            }
            _ => {}
        }
    }

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        // Request a redraw so we keep ticking at the display refresh rate.
        // The actual work happens in `RedrawRequested`.
        if let State::Running { window, .. } = &self.state {
            window.request_redraw();
        } else if !matches!(self.state, State::Done { .. }) {
            event_loop.exit();
        }
    }
}

impl Harness {
    fn tick(&mut self) {
        // Borrow dance: we need to mutably touch renderer, sys, fixture,
        // and frame_times simultaneously, so pull the running state out.
        let State::Running {
            renderer,
            window,
            deadline,
            offset,
            frame_times,
            peak_rss,
            cpu_samples,
            sys,
            pid,
            last_metrics_sample,
        } = &mut self.state
        else {
            return;
        };

        let now = Instant::now();
        if now >= *deadline {
            let summary = summarize(std::mem::take(frame_times), *peak_rss, cpu_samples);
            renderer.teardown();
            self.state = State::Done { summary };
            return;
        }

        // Periodic metrics sampling. Coarse but cheap; sample every ~50ms.
        if now.duration_since(*last_metrics_sample) >= Duration::from_millis(50) {
            sys.refresh_memory();
            sys.refresh_processes_specifics(
                ProcessesToUpdate::Some(&[*pid]),
                true,
                ProcessRefreshKind::new().with_cpu().with_memory(),
            );
            if let Some(proc) = sys.process(*pid) {
                let rss = proc.memory();
                if rss > *peak_rss {
                    *peak_rss = rss;
                }
                cpu_samples.push(proc.cpu_usage());
            }
            *last_metrics_sample = now;
        }

        // Visible-range computation from offset and window height.
        let visible_lines = visible_line_count();
        let total = self.fixture.line_count();
        let start = (*offset).min(total.saturating_sub(1));
        let end = (start + visible_lines + OVERSCAN_LINES).min(total);

        let mut visible: Vec<Vec<Segment>> = Vec::with_capacity(end - start);
        for line in start..end {
            visible.push(self.fixture.segments_of(line));
        }

        let frame_start = Instant::now();
        if let Err(e) = renderer.render_frame(*offset, &visible) {
            eprintln!("[bench] render_frame error: {e:#}");
        }
        let frame_dur = frame_start.elapsed();
        frame_times.push(frame_dur);

        // Advance the scroll offset, wrapping at the buffer end.
        *offset += SCROLL_DELTA_LINES;
        if *offset >= total.saturating_sub(visible_lines) {
            *offset = 0;
        }

        // Keep the window title informative without allocating much.
        let _ = window;
    }
}

fn visible_line_count() -> usize {
    ((WIN_H / LINE_HEIGHT_PX) as usize).max(1)
}

fn summarize(mut frame_times: Vec<Duration>, peak_rss: u64, cpu_samples: &[f32]) -> Summary {
    frame_times.sort();
    let frames = frame_times.len();
    let p50 = percentile(&frame_times, 50).as_micros() as u64;
    let p99 = percentile(&frame_times, 99).as_micros() as u64;
    let cpu_avg = if cpu_samples.is_empty() {
        0.0
    } else {
        cpu_samples.iter().copied().sum::<f32>() / cpu_samples.len() as f32
    };
    Summary {
        frames,
        p50_us: p50,
        p99_us: p99,
        peak_rss_bytes: peak_rss,
        cpu_avg,
    }
}

fn percentile(sorted: &[Duration], p: u8) -> Duration {
    if sorted.is_empty() {
        return Duration::ZERO;
    }
    let idx = ((p as usize).min(100)) * (sorted.len() - 1) / 100;
    sorted[idx]
}

/// Print the list of available backends (for `--backend list`).
pub fn print_available_backends() {
    eprintln!("available backends:");
    for b in available_backends() {
        eprintln!("  {b}");
    }
}

/// Validate CLI arguments and return a `BenchConfig`, or print usage and
/// return an error.
pub fn parse_args(args: impl Iterator<Item = String>) -> Result<BenchConfig> {
    let mut fixture = None;
    let mut backend = None;
    let mut duration = None;
    let mut tile = None;
    let mut list = false;

    let mut it = args.peekable();
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--fixture" | "-f" => {
                fixture = Some(PathBuf::from(
                    it.next().context("--fixture requires a path")?,
                ));
            }
            "--backend" | "-b" => {
                backend = Some(it.next().context("--backend requires a name")?);
            }
            "--duration" | "-d" => {
                let secs: u64 = it
                    .next()
                    .context("--duration requires a number of seconds")?
                    .parse()?;
                duration = Some(Duration::from_secs(secs));
            }
            "--tile" => {
                let n: usize = it.next().context("--tile requires a count")?.parse()?;
                tile = Some(n);
            }
            "--list-backends" => {
                list = true;
            }
            other => bail!("unknown argument `{other}`"),
        }
    }

    if list {
        print_available_backends();
        std::process::exit(0);
    }

    let fixture = fixture.context("missing --fixture")?;
    let backend = backend.context("missing --backend")?;

    Ok(BenchConfig {
        fixture,
        backend,
        duration,
        tile,
    })
}

// Silence an unused import on platforms where RefreshKind isn't needed
// for direct construction; keep it referenced for future extension.
#[allow(dead_code)]
fn _refresh_kind_anchor() -> RefreshKind {
    RefreshKind::new().with_memory(MemoryRefreshKind::everything())
}
