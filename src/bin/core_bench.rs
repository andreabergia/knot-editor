//! `core-bench` binary: drives the piece-table workloads over a fixture.
//!
//! Usage:
//!   core-bench --fixture <path.kfx> [--tile <n>] [--duration <secs>]
//!              [--seed <n>] [--workload <name>]
//!
//! `--workload` selects one of:
//!   - `all`         (default) run every workload in sequence
//!   - `construct`   measure TextBuffer::from_text + lazy line-index build
//!   - `edits`       random mixed insert/delete stream (line index live)
//!   - `edits-noidx` same stream with the line index left lazy
//!   - `interleaved` two-producer alternating edits with Position remap
//!   - `lookup`      line-index + Position resolve queries
//!
//! Output is one TSV-ish summary line per workload, the same shape as
//! the renderer `bench` binary so the two can share a results table:
//!   <fixture>\t<workload>\tkey=val\t...
//!
//! Reuses the step-2 fixture corpus (see `bench/README.md`). The 1M-line
//! workload is the existing `rust_sample.kfx` path with `--tile 635`.
//! The fixture format (`src/view/fixture.rs`) gives raw line text usable
//! as buffer input; the segment-table portion is renderer-only and
//! ignored here per `docs/step4-buffer-plan.md`.

use std::env;
use std::path::PathBuf;
use std::process::ExitCode;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};

use knot::core::buffer::{BufferEdit, Position, Split, TextBuffer};
use knot::view::fixture::Fixture;

// ---- Config ---------------------------------------------------------------

struct Cfg {
    fixture: PathBuf,
    tile: Option<usize>,
    duration: Duration,
    seed: u64,
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
    let mut seed: u64 = 0xC0FF_EEBABE;
    let mut workload = String::from("all");

    let mut it = args.peekable();
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--fixture" | "-f" => {
                fixture = Some(PathBuf::from(
                    it.next().context("--fixture requires a path")?,
                ));
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
            "--workload" | "-w" => {
                workload = it.next().context("--workload requires a name")?.to_owned();
            }
            other => bail!("unknown argument `{other}`"),
        }
    }

    let fixture = fixture.context("missing --fixture")?;
    if !matches!(
        workload.as_str(),
        "all" | "construct" | "edits" | "edits-noidx" | "interleaved" | "lookup"
    ) {
        bail!("unknown workload `{workload}`");
    }

    Ok(Cfg {
        fixture,
        tile,
        duration,
        seed,
        workload,
    })
}

// ---- Output ---------------------------------------------------------------

/// One TSV summary line: `fixture\tworkload\tkey=val\t...`.
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
    pub peak_rss_bytes: u64,
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

    /// Record one latency sample. RSS is sampled every 256th call so the
    /// per-sample sysinfo refresh doesn't pollute the ops/sec tail — the
    /// steady-state `current_rss()` overhead (~µs) would otherwise swamp
    /// sub-µs measurements like `resolve`.
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
    use sysinfo::{Pid, ProcessRefreshKind, ProcessesToUpdate, System};
    let mut sys = System::new();
    let pid = Pid::from_u32(std::process::id());
    sys.refresh_processes_specifics(
        ProcessesToUpdate::Some(&[pid]),
        true,
        ProcessRefreshKind::new().with_memory(),
    );
    sys.process(pid).map(|p| p.memory()).unwrap_or(0)
}

fn rss_mib() -> f64 {
    current_rss() as f64 / ((1u64 << 20) as f64)
}

// ---- RNG ------------------------------------------------------------------

/// xorshift64 — deterministic, cheap, no deps.
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

    /// Uniform usize in `[0, n)`.
    fn below(&mut self, n: usize) -> usize {
        if n == 0 {
            return 0;
        }
        (self.next_u64() % n as u64) as usize
    }
}

// ---- Buffer construction from fixture ------------------------------------

/// Build the full text and a fresh TextBuffer from the loaded fixture.
/// Returns `(buffer, len_bytes, input_line_count)`. Lines are joined with
/// `'\n'` (no trailing newline added) to form a single contiguous UTF-8
/// byte buffer; the initial piece table contains exactly one piece covering
/// the whole span (per `docs/step4-buffer-plan.md` § "Piece-table
/// construction from fixture"). No `'\r'` normalization is performed.
fn build_buffer(fixture: &Fixture) -> (TextBuffer, usize, usize) {
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
    (TextBuffer::from_text(text), len, line_count)
}

// ---- Position remap helper (D7 right-half relocation) ---------------------

/// Walk the edit log forward from `*cursor`, applying each `Split` whose
/// `old_piece` matches the token's current anchor and whose `split_offset`
/// is below the token's current offset (i.e. the token sat in the right
/// half of that split). Relocates the token to `(new_piece, off -
/// split_offset)` and advances `*cursor` past the consumed edits.
///
/// The log's `Split` records describe interior splits only; whole-piece
/// deletions are *not* detectable from the log alone, so a remapped token
/// may still resolve to `None` later (the caller checks `resolve`). This
/// function never sets `*token` to `None`: it leaves the token where the
/// log says the content moved, and resolution failure is reported by
/// `TextBuffer::resolve` at use time.
fn apply_log(
    buf: &TextBuffer,
    cursor: &mut usize,
    token: &mut Option<Position>,
    remap_count: &mut usize,
    remap_stats: &mut Stats,
) {
    let edits = buf.edits_since(*cursor);
    if edits.is_empty() {
        return;
    }
    let t = Instant::now();
    let mut local_remaps = 0usize;
    if let Some(tok) = *token {
        let mut cur_piece = tok.piece();
        let mut cur_off = tok.offset() as usize;
        for ev in edits {
            let splits: &[Split] = match ev {
                BufferEdit::Insert { splits, .. } => splits,
                BufferEdit::Delete { splits, .. } => splits,
            };
            for sp in splits.iter() {
                if sp.old_piece == cur_piece && cur_off > sp.split_offset {
                    cur_off -= sp.split_offset;
                    cur_piece = sp.new_piece;
                    local_remaps += 1;
                }
            }
        }
        *token = Some(Position::new(cur_piece, cur_off as u32));
    }
    *cursor += edits.len();
    *remap_count += local_remaps;
    remap_stats.record(t.elapsed());
}

// ---- Workloads ------------------------------------------------------------

/// Construction cost: `TextBuffer::from_text` on the joined fixture, then
/// the lazy line-index build on first `line_count()`.
fn run_construct(cfg: &Cfg, fixture: &Fixture) {
    let t0 = Instant::now();
    let (mut buf, len, lines) = build_buffer(fixture);
    let build_us = t0.elapsed().as_micros() as f64;

    let pieces = buf.piece_count();

    let t1 = Instant::now();
    let lc = buf.line_count();
    let line_index_us = t1.elapsed().as_micros() as f64;

    emit(
        &cfg.display_fixture(),
        "construct",
        &[
            ("bytes", format!("{len}")),
            ("input_lines", format!("{lines}")),
            ("build_ms", format!("{:.3}", build_us / 1000.0)),
            ("line_index_ms", format!("{:.3}", line_index_us / 1000.0)),
            ("line_count", format!("{lc}")),
            ("pieces", format!("{pieces}")),
            ("peak_rss_MiB", format!("{:.1}", rss_mib())),
        ],
    );
}

/// Random mixed insert/delete stream at line-aligned offsets (always UTF-8
/// char boundaries regardless of fixture content). Measures per-edit
/// latency over the duration and reports table size after the run.
///
/// `prefill_index` controls whether the line index is built before the
/// edit loop (so each edit exercises the incremental `update_line_starts`
/// path) or left lazy (so edits skip it entirely). The delta between the
/// two isolates the line-index update cost — the bottleneck the plan's
/// D4 implementation note predicted ("if the suffix shift shows up as a
/// bottleneck, swap to Fenwick tree or BTreeMap for O(log n) per edit").
fn run_edits_mode(cfg: &Cfg, fixture: &Fixture, prefill_index: bool) {
    let (mut buf, _len, _lines) = build_buffer(fixture);
    if prefill_index {
        let _ = buf.line_count();
    }

    let mut rng = Rng::new(cfg.seed);
    let mut stats = Stats::new();

    // Candidate inserted spans — short, ASCII, occasionally multiline.
    const INSERT_MENU: &[&str] = &[
        "x",
        "foo",
        "let x = 1;\n",
        "// note\n",
        "    ",
        "abc\n",
        "fn f() {}\n",
    ];
    const MAX_DEL_LEN: usize = 8;

    // Initial offset anchors chosen at random byte positions (line_of
    // not available when the index is lazy); derive once from the raw
    // fixture's total byte length so picks stay in range. We update the
    // bound periodically to follow buffer growth.
    let mut upper_bound = buf.len() + 1;

    let deadline = Instant::now() + cfg.duration;
    let mut iter = 0usize;
    while Instant::now() < deadline {
        // Refresh the offset bound every 256 iterations so deletes can't
        // overshoot and inserts don't bias to the low end as the buffer
        // grows.
        if iter.is_multiple_of(256) {
            upper_bound = buf.len() + 1;
        }
        let at = if prefill_index {
            // Use the line index when it's available — guarantees char
            // alignment and realistic line-edit locality.
            let line = rng.below(buf.line_count().max(1));
            buf.line_start(line)
        } else {
            rng.below(upper_bound)
        };
        let len = buf.len();
        if at > len {
            continue;
        }

        let pick = rng.next_u64() % 2;
        let t = Instant::now();
        if pick == 0 {
            let ins = INSERT_MENU[rng.below(INSERT_MENU.len())];
            buf.insert(at, ins);
        } else if len > 0 {
            let max_span = MAX_DEL_LEN.min(len - at);
            if max_span > 0 {
                let span = 1 + rng.below(max_span);
                let end = at + span;
                buf.delete(at..end);
            }
        }
        stats.record(t.elapsed());
        iter += 1;
    }

    let final_lines = buf.line_count();
    let final_len = buf.len();
    let name = if prefill_index {
        "edits"
    } else {
        "edits-noidx"
    };
    emit(
        &cfg.display_fixture(),
        name,
        &[
            ("edits", format!("{}", stats.count())),
            (
                "p50_us",
                format!("{:.2}", stats.p(50).as_nanos() as f64 / 1000.0),
            ),
            (
                "p99_us",
                format!("{:.2}", stats.p(99).as_nanos() as f64 / 1000.0),
            ),
            (
                "p999_us",
                format!("{:.2}", stats.p(100).as_nanos() as f64 / 1000.0),
            ),
            (
                "throughput_kops",
                format!(
                    "{:.2}",
                    stats.count() as f64 / cfg.duration.as_secs_f64() / 1000.0
                ),
            ),
            ("pieces", format!("{}", buf.piece_count())),
            ("bytes_after", format!("{}", final_len)),
            ("lines_after", format!("{}", final_lines)),
            (
                "peak_rss_MiB",
                format!("{:.1}", stats.peak_rss_bytes as f64 / ((1u64 << 20) as f64)),
            ),
        ],
    );
}

/// Random edit stream with the lazy line index built up-front. This is the
/// realistic steady-state after a buffer is loaded and the UI has already
/// asked for a line count (which it always does once at first paint).
fn run_edits(cfg: &Cfg, fixture: &Fixture) {
    run_edits_mode(cfg, fixture, true);
}

/// Random edit stream with the line index left lazy. The delta vs `edits`
/// isolates the incremental `update_line_starts` cost.
fn run_edits_noidx(cfg: &Cfg, fixture: &Fixture) {
    run_edits_mode(cfg, fixture, false);
}

/// Two producer cursors, alternating edits, with remap-by-log validation.
///
/// Models the interleaved-producer scenario (D5): two features editing the
/// same buffer from different positions on the same thread. Each producer
/// holds a `Position` token; before acting, the active producer catches the
/// *other* producer up on the edit log (remapping the other's token if a
/// split landed under it). Reports per-edit latency, the per-remap cost,
/// and the stale-but-detectable remap count for the whole run.
fn run_interleaved(cfg: &Cfg, fixture: &Fixture) {
    let (mut buf, _len, _lines) = build_buffer(fixture);
    let _ = buf.line_count();

    let mut rng = Rng::new(cfg.seed);
    let mut stats = Stats::new();
    let mut remap_stats = Stats::new();
    let mut remap_count: usize = 0;

    // Each producer tracks its own cursor into the edit log.
    let mut cursors: [usize; 2] = [0, 0];
    let total_lines = buf.line_count();
    let ls0 = buf.line_start(rng.below(total_lines));
    let tok0 = buf.position_at(ls0);
    let ls1 = buf.line_start(rng.below(total_lines));
    let tok1 = buf.position_at(ls1);
    let mut tokens: [Option<Position>; 2] = [tok0, tok1];

    const INSERT_MENU: &[&str] = &["x", "let y = 2;\n", "// c\n", "foo\n", "    "];

    let deadline = Instant::now() + cfg.duration;
    let mut turn: usize = 0;
    while Instant::now() < deadline {
        let active = turn % 2;
        let other = 1 - active;
        // Active producer catches the other producer up on the log first.
        apply_log(
            &buf,
            &mut cursors[other],
            &mut tokens[other],
            &mut remap_count,
            &mut remap_stats,
        );

        let len = buf.len();
        // Edit at the active producer's resolved position (or a fresh
        // line-start token if the previous anchor was deleted).
        let at = match tokens[active] {
            Some(p) => match buf.resolve(p) {
                Some(off) => off,
                None => {
                    let ln = rng.below(buf.line_count().max(1));
                    let off = buf.line_start(ln);
                    tokens[active] = buf.position_at(off);
                    off
                }
            },
            None => {
                let ln = rng.below(buf.line_count().max(1));
                let off = buf.line_start(ln);
                tokens[active] = buf.position_at(off);
                off
            }
        };

        let t = Instant::now();
        let pick = rng.next_u64() % 2;
        if pick == 0 || len == 0 {
            let ins = INSERT_MENU[rng.below(INSERT_MENU.len())];
            if at <= len {
                buf.insert(at, ins);
            }
        } else {
            let max_span = 4.min(len.saturating_sub(at));
            if max_span > 0 {
                let span = 1 + rng.below(max_span);
                let end = at + span;
                buf.delete(at..end);
            }
        }
        stats.record(t.elapsed());

        // The active producer's own edit may have split right under it; it
        // catches up next turn (when it becomes the "other").
        turn += 1;
    }

    // Final catch-up so neither producer's outstanding remaps are missed.
    for i in 0..2 {
        apply_log(
            &buf,
            &mut cursors[i],
            &mut tokens[i],
            &mut remap_count,
            &mut remap_stats,
        );
    }

    let live_tokens = tokens.iter().filter(|t| t.is_some()).count();
    emit(
        &cfg.display_fixture(),
        "interleaved",
        &[
            ("edits", format!("{}", stats.count())),
            (
                "p50_us",
                format!("{:.2}", stats.p(50).as_nanos() as f64 / 1000.0),
            ),
            (
                "p99_us",
                format!("{:.2}", stats.p(99).as_nanos() as f64 / 1000.0),
            ),
            (
                "p999_us",
                format!("{:.2}", stats.p(100).as_nanos() as f64 / 1000.0),
            ),
            (
                "throughput_kops",
                format!(
                    "{:.2}",
                    stats.count() as f64 / cfg.duration.as_secs_f64() / 1000.0
                ),
            ),
            ("remaps", format!("{remap_count}")),
            (
                "remap_p50_ns",
                format!("{:.0}", remap_stats.p(50).as_nanos() as f64),
            ),
            (
                "remap_p99_ns",
                format!("{:.0}", remap_stats.p(99).as_nanos() as f64),
            ),
            ("live_tokens", format!("{live_tokens}/2")),
            ("pieces", format!("{}", buf.piece_count())),
            (
                "peak_rss_MiB",
                format!("{:.1}", stats.peak_rss_bytes as f64 / ((1u64 << 20) as f64)),
            ),
        ],
    );
}

/// Line-index + Position resolve query throughput. Builds the buffer and
/// the line index once, warms up the piece chain with a few hundred random
/// edits so `position_at` / `resolve` exercise a realistic chain (not the
/// trivial 1-piece state at load), then issues random queries against each
/// surface for `duration` and reports p50/p99/p999 per query kind.
fn run_lookup(cfg: &Cfg, fixture: &Fixture) {
    let (mut buf, _len, _lines) = build_buffer(fixture);
    let total_lines = buf.line_count();
    let total_bytes = buf.len();

    // Warm-up: do 256 random boundary inserts to grow the piece chain.
    // 256 ≈ the realistic piece count after a short editing session.
    let mut warmup_rng = Rng::new(cfg.seed ^ 0x1200_0000);
    for _ in 0..256 {
        let line = warmup_rng.below(buf.line_count().max(1));
        let at = buf.line_start(line);
        buf.insert(at, "x");
    }
    let pieces = buf.piece_count();
    let warmup_bytes = buf.len();
    eprintln!("[core-bench] lookup warmup: pieces={pieces} bytes={warmup_bytes}");

    let mut rng = Rng::new(cfg.seed);
    let mut loo_stats = Stats::new();
    let mut ls_stats = Stats::new();
    let mut pa_stats = Stats::new();
    let mut res_stats = Stats::new();

    let deadline = Instant::now() + cfg.duration;
    while Instant::now() < deadline {
        let off = rng.below(total_bytes + 1);
        let t = Instant::now();
        let _ = buf.line_of_offset(off);
        loo_stats.record(t.elapsed());

        let line = rng.below(total_lines);
        let t = Instant::now();
        let _ = buf.line_start(line);
        ls_stats.record(t.elapsed());

        let t = Instant::now();
        let p = buf.position_at(off);
        pa_stats.record(t.elapsed());

        if let Some(pos) = p {
            let t = Instant::now();
            let r = buf.resolve(pos);
            res_stats.record(t.elapsed());
            debug_assert!(r.is_some(), "fresh position should resolve");
        }
    }

    /// Build the 4-entry summary block for one query kind:
    /// `{prefix}_count`, `{prefix}_p50_ns`, `{prefix}_p99_ns`,
    /// `{prefix}_p999_ns`. The leaked `&'static str` labels are intentional:
    /// the lookup workload runs once per bench invocation, and the few dozen
    /// bytes leaked per call are a rounding error for a CLI bench tool.
    fn block(s: &Stats, prefix: &'static str) -> Vec<(&'static str, String)> {
        vec![
            (
                Box::leak(format!("{prefix}_count").into_boxed_str()),
                format!("{}", s.count()),
            ),
            (
                Box::leak(format!("{prefix}_p50_ns").into_boxed_str()),
                format!("{:.0}", s.p(50).as_nanos() as f64),
            ),
            (
                Box::leak(format!("{prefix}_p99_ns").into_boxed_str()),
                format!("{:.0}", s.p(99).as_nanos() as f64),
            ),
            (
                Box::leak(format!("{prefix}_p999_ns").into_boxed_str()),
                format!("{:.0}", s.p(100).as_nanos() as f64),
            ),
        ]
    }
    let mut all: Vec<(&'static str, String)> = Vec::new();
    all.extend(block(&loo_stats, "loo"));
    all.extend(block(&ls_stats, "ls"));
    all.extend(block(&pa_stats, "pa"));
    all.extend(block(&res_stats, "res"));
    all.push((
        "peak_rss_MiB",
        format!(
            "{:.1}",
            loo_stats.peak_rss_bytes as f64 / ((1u64 << 20) as f64)
        ),
    ));

    emit(&cfg.display_fixture(), "lookup", &all);
}

// ---- Driver ---------------------------------------------------------------

fn load_fixture(cfg: &Cfg) -> Result<Fixture> {
    let mut fixture = Fixture::load(&cfg.fixture)
        .with_context(|| format!("loading fixture {}", cfg.fixture.display()))?;
    if let Some(tile) = cfg.tile {
        fixture = fixture.tiled(tile);
    }
    eprintln!(
        "[core-bench] fixture={} lines={} duration={:?} workload={}",
        cfg.display_fixture(),
        fixture.line_count(),
        cfg.duration,
        cfg.workload,
    );
    Ok(fixture)
}

fn run(cfg: &Cfg) -> Result<()> {
    let fixture = load_fixture(cfg)?;
    match cfg.workload.as_str() {
        "all" => {
            run_construct(cfg, &fixture);
            run_edits(cfg, &fixture);
            run_edits_noidx(cfg, &fixture);
            run_interleaved(cfg, &fixture);
            run_lookup(cfg, &fixture);
        }
        "construct" => run_construct(cfg, &fixture),
        "edits" => run_edits(cfg, &fixture),
        "edits-noidx" => run_edits_noidx(cfg, &fixture),
        "interleaved" => run_interleaved(cfg, &fixture),
        "lookup" => run_lookup(cfg, &fixture),
        other => bail!("unknown workload `{other}`"),
    }
    Ok(())
}

fn main() -> ExitCode {
    let args: Vec<String> = env::args().skip(1).collect();
    let cfg = match parse_args(args.into_iter()) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("error: {e:#}");
            eprintln!(
                "usage: core-bench --fixture <path.kfx> [--tile <n>] \
                 [--duration <secs>] [--seed <n>] [--workload <name>]"
            );
            return ExitCode::from(2);
        }
    };
    match run(&cfg) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("core-bench failed: {e:#}");
            ExitCode::FAILURE
        }
    }
}
