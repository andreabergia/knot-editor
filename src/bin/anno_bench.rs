//! `anno-bench` binary: step 5 annotation-layer benchmark.
//!
//! Usage:
//!   anno-bench --fixture <path.kfx> [--tile <n>] [--duration <secs>]
//!             [--seed <n>] [--annotations <n>] [--sources <n>]
//!             [--workload <name>]
//!
//! `--workload` selects:
//!   - `all`            (default) run every workload in sequence
//!   - `stabilize`      per-edit token stabilization cost (p50/p99)
//!   - `baseline`       per-edit D5 offset-remap cost (the comparator)
//!   - `resolve-all`    resolve every annotation (10k/frame) cost
//!   - `query`          random `query_range` hits cost
//!
//! Output is one TSV summary line per workload, matching `core-bench`'s
//! shape: `<fixture>\t<workload>\tkey=val\t...`.
//!
//! ## Default vs full configuration
//!
//! The plan's validation gate is the full 1M-line / 10k-annotation run:
//!
//! ```text
//! anno-bench --fixture bench/fixtures/rust_sample.kfx --tile 635 \
//!            --annotations 10000 --sources 3 --duration 5
//! ```
//!
//! That is heavy (builds a 29 MiB buffer, seeds 10k annotations, runs N
//! interleaved producers). The defaults here are intentionally *lighter*
//! (`--annotations 1000 --sources 3`, untiled fixture) so the harness runs
//! fast during development; pass the flags above to reproduce the plan's
//! full stress test. Findings are written to
//! `docs/step5-annotation-benchmark.md` when the full run is executed.

use std::env;
use std::path::PathBuf;
use std::process::ExitCode;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};

use knot::core::annotation::{
    AnnotationData, AnnotationKind, AnnotationStore, OffsetStore,
};
use knot::core::buffer::TextBuffer;
use knot::view::fixture::Fixture;

// ---- Config ---------------------------------------------------------------

struct Cfg {
    fixture: PathBuf,
    tile: Option<usize>,
    duration: Duration,
    seed: u64,
    annotations: usize,
    sources: usize,
    workload: String,
}

impl Cfg {
    fn display_fixture(&self) -> String {
        self.fixture
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("<no-name>")
            .to_owned()
    }
}

fn parse_args(args: impl Iterator<Item = String>) -> Result<Cfg> {
    let mut fixture = None;
    let mut tile = None;
    let mut duration = Duration::from_secs(5);
    let mut seed: u64 = 0xC0FF_EE_BABE;
    let mut annotations: usize = 1000;
    let mut sources: usize = 3;
    let mut workload = String::from("all");

    let mut it = args.peekable();
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--fixture" | "-f" => {
                fixture = Some(PathBuf::from(it.next().context("--fixture requires a path")?));
            }
            "--tile" => {
                let n: usize = it.next().context("--tile requires a count")?.parse()?;
                tile = Some(n);
            }
            "--duration" | "-d" => {
                let secs: u64 = it
                    .next()
                    .context("--duration requires a number of seconds")?
                    .parse()?;
                duration = Duration::from_secs(secs);
            }
            "--seed" => {
                seed = it.next().context("--seed requires a number")?.parse()?;
            }
            "--annotations" | "-a" => {
                annotations = it.next().context("--annotations requires a count")?.parse()?;
            }
            "--sources" | "-s" => {
                sources = it.next().context("--sources requires a count")?.parse()?;
            }
            "--workload" | "-w" => {
                workload = it.next().context("--workload requires a name")?.to_owned();
            }
            other => bail!("unknown argument `{other}`"),
        }
    }

    let fixture = fixture.context("missing --fixture")?;
    if !matches!(
        workload.as_str(),
        "all" | "stabilize" | "baseline" | "resolve-all" | "query"
    ) {
        bail!("unknown workload `{workload}`");
    }

    Ok(Cfg {
        fixture,
        tile,
        duration,
        seed,
        annotations,
        sources,
        workload,
    })
}

// ---- Output ---------------------------------------------------------------

fn emit(fixture: &str, workload: &str, fields: &[(&str, String)]) {
    let mut line = format!("{fixture}\t{workload}");
    for (k, v) in fields {
        line.push_str(&format!("\t{k}={v}"));
    }
    println!("{line}");
}

// ---- Stats ----------------------------------------------------------------

struct Stats {
    samples: Vec<Duration>,
    peak_rss_bytes: u64,
    rss_sample_counter: usize,
}

impl Stats {
    fn new() -> Self {
        Self {
            samples: Vec::new(),
            peak_rss_bytes: current_rss(),
            rss_sample_counter: 0,
        }
    }
    fn record(&mut self, d: Duration) {
        self.samples.push(d);
        self.rss_sample_counter = self.rss_sample_counter.wrapping_add(1);
        if self.rss_sample_counter.is_multiple_of(256) {
            let rss = current_rss();
            if rss > self.peak_rss_bytes {
                self.peak_rss_bytes = rss;
            }
        }
    }
    fn count(&self) -> usize {
        self.samples.len()
    }
    fn p(&self, pct: u8) -> Duration {
        if self.samples.is_empty() {
            return Duration::ZERO;
        }
        let mut s = self.samples.clone();
        s.sort();
        let idx = (pct as usize).min(100) * (s.len() - 1) / 100;
        s[idx]
    }
}

fn current_rss() -> u64 {
    use sysinfo::{Pid, ProcessesToUpdate, ProcessRefreshKind, System};
    let mut sys = System::new();
    let pid = Pid::from_u32(std::process::id());
    sys.refresh_processes_specifics(
        ProcessesToUpdate::Some(&[pid]),
        true,
        ProcessRefreshKind::new().with_memory(),
    );
    sys.process(pid).map(|p| p.memory()).unwrap_or(0)
}

fn rss_mib(bytes: u64) -> f64 {
    bytes as f64 / ((1u64 << 20) as f64)
}

// ---- RNG ------------------------------------------------------------------

struct Rng(u64);
impl Rng {
    fn new(seed: u64) -> Self {
        Rng(seed.max(1))
    }
    fn next_u64(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x
    }
    fn below(&mut self, n: usize) -> usize {
        if n == 0 {
            return 0;
        }
        (self.next_u64() % n as u64) as usize
    }
}

// ---- Buffer construction --------------------------------------------------

fn build_buffer(fixture: &Fixture) -> (TextBuffer, usize) {
    let line_count = fixture.lines.len();
    let total_bytes: usize = fixture.lines.iter().map(|l| l.len() + 1).sum();
    let mut text = String::with_capacity(total_bytes);
    for (i, line) in fixture.lines.iter().enumerate() {
        text.push_str(line);
        if i + 1 < line_count {
            text.push('\n');
        }
    }
    let len = text.len();
    (TextBuffer::from_text(text), len)
}

/// Insert menu for the interleaved producer workloads (ASCII, short).
const INSERT_MENU: &[&str] = &["x", "let y = 2;\n", "// c\n", "foo\n", "    "];

/// Perform one random edit from `source` on `buffer`. Returns nothing; the
/// buffer's edit log records it for the stores to consume.
fn do_edit(buffer: &mut TextBuffer, rng: &mut Rng, source: usize) {
    let total_lines = buffer.line_count();
    let at = buffer.line_start(rng.below(total_lines));
    let len = buffer.len();
    let pick = rng.next_u64() % 2;
    if pick == 0 || len == 0 {
        let ins = INSERT_MENU[rng.below(INSERT_MENU.len())];
        if at <= len {
            buffer.insert(at, ins);
        }
    } else {
        let max_span = 4.min(len.saturating_sub(at));
        if max_span > 0 {
            let span = 1 + rng.below(max_span);
            buffer.delete(at..at + span);
        }
    }
    let _ = source;
}

// ---- Workloads ------------------------------------------------------------

/// Seed `annotations` total across `sources` independent sources at random
/// byte ranges, mirroring them into the D5 offset baseline. Returns the token
/// store, the offset baseline, the buffer, and the seeded ids.
fn seed(
    cfg: &Cfg,
    buffer: &mut TextBuffer,
) -> (AnnotationStore, OffsetStore, Vec<u64>) {
    let mut token_store = AnnotationStore::new();
    let mut offset_store = OffsetStore::new();
    let mut ids = Vec::with_capacity(cfg.annotations);

    let mut rng = Rng::new(cfg.seed ^ 0x5A17);
    let total = buffer.len();
    let per_source = cfg.annotations / cfg.sources.max(1);
    for src in 0..cfg.sources {
        let kind = match src % 5 {
            0 => AnnotationKind::Diagnostic,
            1 => AnnotationKind::Search,
            2 => AnnotationKind::Git,
            3 => AnnotationKind::Breakpoint,
            _ => AnnotationKind::Other(src as u8),
        };
        for _ in 0..per_source {
            let s = rng.below(total + 1);
            let e = rng.below(total + 1);
            let (s, e) = if s <= e { (s, e) } else { (e, s) };
            let id = token_store.add(buffer, s, e, kind, AnnotationData::default());
            offset_store.add(s, e);
            ids.push(id);
        }
    }
    (token_store, offset_store, ids)
}

/// Per-edit stabilization cost of the token store, and of the D5 offset
/// baseline (the comparator). `baseline_only` restricts output to the
/// baseline workload.
fn run_stabilize(cfg: &Cfg, fixture: &Fixture, baseline_only: bool) {
    let (mut buffer, _len) = build_buffer(fixture);
    let _ = buffer.line_count();
    let (mut token_store, mut offset_store, _ids) = seed(cfg, &mut buffer);

    // Reset the edit cursor baseline to the seeded state.
    let _ = (&mut token_store, &mut offset_store);

    let mut rng = Rng::new(cfg.seed);
    let mut token_stats = Stats::new();
    let mut offset_stats = Stats::new();

    let deadline = Instant::now() + cfg.duration;
    let mut source_turn = 0usize;
    while Instant::now() < deadline {
        let source = source_turn % cfg.sources;
        source_turn += 1;

        // Single owner of the log: the store drains it via stabilize; the
        // baseline mirrors the same edits.
        do_edit(&mut buffer, &mut rng, source);

        let t = Instant::now();
        token_store.stabilize(&buffer);
        if !baseline_only {
            token_stats.record(t.elapsed());
        }

        let t = Instant::now();
        offset_store.stabilize(&buffer);
        offset_stats.record(t.elapsed());
    }

    if baseline_only {
        emit(
            &cfg.display_fixture(),
            "baseline",
            &[
                ("edits", format!("{}", offset_stats.count())),
                ("p50_us", format!("{:.2}", offset_stats.p(50).as_nanos() as f64 / 1000.0)),
                ("p99_us", format!("{:.2}", offset_stats.p(99).as_nanos() as f64 / 1000.0)),
                (
                    "throughput_kops",
                    format!("{:.2}", offset_stats.count() as f64 / cfg.duration.as_secs_f64() / 1000.0),
                ),
                ("annotations", format!("{}", cfg.annotations)),
                (
                    "peak_rss_MiB",
                    format!("{:.1}", rss_mib(offset_stats.peak_rss_bytes)),
                ),
            ],
        );
    } else {
        emit(
            &cfg.display_fixture(),
            "stabilize",
            &[
                ("edits", format!("{}", token_stats.count())),
                ("p50_ns", format!("{:.0}", token_stats.p(50).as_nanos() as f64)),
                ("p99_ns", format!("{:.0}", token_stats.p(99).as_nanos() as f64)),
                (
                    "throughput_kops",
                    format!("{:.2}", token_stats.count() as f64 / cfg.duration.as_secs_f64() / 1000.0),
                ),
                ("annotations", format!("{}", cfg.annotations)),
                (
                    "peak_rss_MiB",
                    format!("{:.1}", rss_mib(token_stats.peak_rss_bytes)),
                ),
                // Comparator: how much worse the offset approach is per edit.
                ("baseline_p50_ns", format!("{:.0}", offset_stats.p(50).as_nanos() as f64)),
                ("baseline_p99_ns", format!("{:.0}", offset_stats.p(99).as_nanos() as f64)),
                (
                    "baseline_over_token_p50_x",
                    format!(
                        "{:.1}",
                        offset_stats.p(50).as_nanos() as f64
                            / token_stats.p(50).as_nanos().max(1) as f64
                    ),
                ),
            ],
        );
    }
}

/// Resolve every annotation once per frame; measure per-frame cost at the
/// seeded annotation count.
fn run_resolve_all(cfg: &Cfg, fixture: &Fixture) {
    let (mut buffer, _len) = build_buffer(fixture);
    let _ = buffer.line_count();
    let (mut token_store, _offset_store, ids) = seed(cfg, &mut buffer);

    // Warm up with a few edits so resolutions exercise a realistic state.
    let mut rng = Rng::new(cfg.seed ^ 0x7777);
    for _ in 0..256 {
        let source = rng.below(cfg.sources);
        do_edit(&mut buffer, &mut rng, source);
        token_store.stabilize(&buffer);
    }

    let mut stats = Stats::new();
    let deadline = Instant::now() + cfg.duration;
    while Instant::now() < deadline {
        let t = Instant::now();
        for id in &ids {
            let _ = token_store.resolve(&buffer, *id);
        }
        stats.record(t.elapsed());
    }

    emit(
        &cfg.display_fixture(),
        "resolve-all",
        &[
            ("frames", format!("{}", stats.count())),
            ("annotations", format!("{}", ids.len())),
            (
                "p50_us_per_frame",
                format!("{:.3}", stats.p(50).as_nanos() as f64 / 1000.0),
            ),
            (
                "p99_us_per_frame",
                format!("{:.3}", stats.p(99).as_nanos() as f64 / 1000.0),
            ),
            (
                "peak_rss_MiB",
                format!("{:.1}", rss_mib(stats.peak_rss_bytes)),
            ),
        ],
    );
}

/// Random `query_range` hits; measure per-query latency.
fn run_query(cfg: &Cfg, fixture: &Fixture) {
    let (mut buffer, _len) = build_buffer(fixture);
    let _ = buffer.line_count();
    let (mut token_store, _offset_store, _ids) = seed(cfg, &mut buffer);

    // Warm up.
    let mut rng = Rng::new(cfg.seed ^ 0x3333);
    for _ in 0..256 {
        let source = rng.below(cfg.sources);
        do_edit(&mut buffer, &mut rng, source);
        token_store.stabilize(&buffer);
    }

    let mut stats = Stats::new();
    let deadline = Instant::now() + cfg.duration;
    let total = buffer.len().max(1);
    while Instant::now() < deadline {
        let a = rng.below(total);
        let b = a + 1 + rng.below(64.min(total - a));
        let t = Instant::now();
        let _ = token_store.query_range(a, b);
        stats.record(t.elapsed());
    }

    emit(
        &cfg.display_fixture(),
        "query",
        &[
            ("queries", format!("{}", stats.count())),
            ("p50_ns", format!("{:.0}", stats.p(50).as_nanos() as f64)),
            ("p99_ns", format!("{:.0}", stats.p(99).as_nanos() as f64)),
            (
                "peak_rss_MiB",
                format!("{:.1}", rss_mib(stats.peak_rss_bytes)),
            ),
        ],
    );
}

// ---- Driver ---------------------------------------------------------------

fn main() -> ExitCode {
    let args: Vec<String> = env::args().skip(1).collect();
    let cfg = match parse_args(args.into_iter()) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("error: {e:#}");
            eprintln!(
                "usage: anno-bench --fixture <path.kfx> [--tile <n>] \
                 [--duration <secs>] [--seed <n>] [--annotations <n>] \
                 [--sources <n>] [--workload <name>]"
            );
            return ExitCode::from(2);
        }
    };
    match run(&cfg) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("anno-bench failed: {e:#}");
            ExitCode::FAILURE
        }
    }
}

fn run(cfg: &Cfg) -> Result<()> {
    let mut fixture = Fixture::load(&cfg.fixture)
        .with_context(|| format!("loading fixture {}", cfg.fixture.display()))?;
    if let Some(tile) = cfg.tile {
        fixture = fixture.tiled(tile);
    }
    eprintln!(
        "[anno-bench] fixture={} lines={} annotations={} sources={} workload={}",
        cfg.display_fixture(),
        fixture.line_count(),
        cfg.annotations,
        cfg.sources,
        cfg.workload,
    );

    match cfg.workload.as_str() {
        "all" => {
            run_stabilize(cfg, &fixture, false);
            run_stabilize(cfg, &fixture, true);
            run_resolve_all(cfg, &fixture);
            run_query(cfg, &fixture);
        }
        "stabilize" => run_stabilize(cfg, &fixture, false),
        "baseline" => run_stabilize(cfg, &fixture, true),
        "resolve-all" => run_resolve_all(cfg, &fixture),
        "query" => run_query(cfg, &fixture),
        other => bail!("unknown workload `{other}`"),
    }
    Ok(())
}
