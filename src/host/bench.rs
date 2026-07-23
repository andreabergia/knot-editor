//! Small, reproducible measurements for the Step 7 V8 runtime experiment.
//!
//! This is intentionally a CLI harness rather than a statistical framework.
//! It drives the real extension thread, V8 facade, and typed host protocol;
//! the gpui foreground owner is deliberately not constructed here.

use std::{
    env,
    process::Command,
    time::{Duration, Instant},
};

use anyhow::{Context, Result, bail};

use super::{
    BufferChangeQueueMetrics, ExtensionRuntimeExecutionError, ExtensionRuntimeHandle, V8Host,
    protocol::{
        BufferChange, BufferHandle, BufferSubscriptionId, ByteRange, ExtensionId, HostOperation,
        HostRequest, HostResponse, HostResponseValue, TextSnapshot,
    },
};

const BUFFER: BufferHandle = BufferHandle::new(1);

pub fn run(args: impl Iterator<Item = String>) -> Result<()> {
    let args = args.collect::<Vec<_>>();
    if args.as_slice() == ["--probe-init"] {
        let started = Instant::now();
        let _host = V8Host::new();
        println!("init_ns={}", started.elapsed().as_nanos());
        return Ok(());
    }

    let samples = parse_samples(&args)?;
    let process_init = process_v8_initialization()?;
    let host = V8Host::new();

    println!("workload\tp50_us\tp95_us\tbytes\tnotes");
    print_times("process-v8-init", &[process_init], 0, "fresh child process");
    print_times(
        "isolate-startup",
        &sample(samples, |index| isolate_startup(&host, index))?,
        0,
        "incremental, includes extension thread",
    );
    print_times(
        "builtin-module-init",
        &sample(samples, |index| builtin_module_init(&host, index))?,
        0,
        "knot:editor facade",
    );

    let idle_rss = idle_rss_per_isolate(&host)?;
    println!("idle-rss-per-isolate\t-\t-\t{}\tapproximate KiB", idle_rss);

    let host_calls = sample(samples, |index| active_buffer_call(&host, index))?;
    print_times(
        "host-call-active-buffer",
        &host_calls,
        0,
        "V8 facade + channel + typed op",
    );

    let edits = sample(samples, |index| batched_edit_call(&host, index))?;
    print_times("batched-edit-10", &edits, 10, "one applyEdits crossing");

    for (position, (label, bytes, text)) in transfer_cases().into_iter().enumerate() {
        let snapshot = sample(samples, |index| snapshot_call(&host, index, &text))?;
        print_times(label, &snapshot, bytes, "UTF-8 Rust string to V8 string");
        if position < 2 {
            let adapter = sample(samples, |index| utf16_adapter_call(&host, index, &text))?;
            print_times(
                &format!("utf16-adapter-{bytes}"),
                &adapter,
                bytes,
                "snapshot transfer plus lazy boundary table",
            );
        }
    }

    let (cold, warm) = jit_warmup(&host)?;
    print_times(
        "jit-warmup-cold-32-calls",
        &[cold],
        0,
        "first loop in isolate",
    );
    print_times(
        "jit-warmup-steady-32-calls",
        &[warm],
        0,
        "second loop in same isolate",
    );

    let (fanout, metrics) = event_fanout(&host, samples)?;
    print_times(
        "event-fanout-2x8",
        &fanout,
        0,
        "two isolates, eight ordered events",
    );
    println!(
        "event-queue\t-\t-\t-\tmax_depth={} max_lag_us={}",
        metrics.max_depth,
        metrics.max_enqueue_to_start_lag.as_micros()
    );
    Ok(())
}

fn parse_samples(args: &[String]) -> Result<usize> {
    match args {
        [] => Ok(12),
        [flag, value] if flag == "--samples" => value
            .parse::<usize>()
            .context("--samples must be a positive integer")
            .and_then(|samples| {
                if samples == 0 {
                    bail!("--samples must be a positive integer")
                } else {
                    Ok(samples)
                }
            }),
        _ => bail!("usage: v8-bench [--samples <n>]"),
    }
}

fn process_v8_initialization() -> Result<Duration> {
    let executable = env::current_exe().context("locating v8-bench executable")?;
    let output = Command::new(executable)
        .arg("--probe-init")
        .output()
        .context("starting V8 initialization probe")?;
    if !output.status.success() {
        bail!("V8 initialization probe failed")
    }
    let line = std::str::from_utf8(&output.stdout)?.trim();
    let nanos = line
        .strip_prefix("init_ns=")
        .context("invalid V8 initialization probe output")?
        .parse()?;
    Ok(Duration::from_nanos(nanos))
}

fn sample(
    mut count: usize,
    mut measure: impl FnMut(u64) -> Result<Duration>,
) -> Result<Vec<Duration>> {
    let mut values = Vec::with_capacity(count);
    while count > 0 {
        values.push(measure(count as u64)?);
        count -= 1;
    }
    Ok(values)
}

fn print_times(name: &str, values: &[Duration], bytes: usize, notes: &str) {
    let (p50, p95) = percentiles(values);
    println!(
        "{name}\t{}\t{}\t{bytes}\t{notes}",
        p50.as_micros(),
        p95.as_micros(),
    );
}

fn percentiles(values: &[Duration]) -> (Duration, Duration) {
    let mut values = values.to_vec();
    values.sort_unstable();
    let p50 = values[(values.len() - 1) / 2];
    let p95 = values[(values.len() - 1) * 95 / 100];
    (p50, p95)
}

fn isolate_startup(host: &V8Host, index: u64) -> Result<Duration> {
    let started = Instant::now();
    let runtime = host.spawn_extension(ExtensionId::new(10_000 + index));
    runtime.shutdown();
    Ok(started.elapsed())
}

fn builtin_module_init(host: &V8Host, index: u64) -> Result<Duration> {
    let runtime = host.spawn_extension(ExtensionId::new(20_000 + index));
    let started = Instant::now();
    let execution = runtime.execute_fixture_module(
        format!("file:///bench/builtin-{index}.js"),
        "import { editor, commands } from 'knot:editor'; if (!editor || !commands) throw new Error('facade missing');",
    );
    completed(pollster::block_on(execution))?;
    let elapsed = started.elapsed();
    runtime.shutdown();
    Ok(elapsed)
}

fn active_buffer_call(host: &V8Host, index: u64) -> Result<Duration> {
    let mut runtime = host.spawn_extension(ExtensionId::new(30_000 + index));
    let execution = runtime.execute_fixture_module(
        format!("file:///bench/active-{index}.js"),
        "import { editor } from 'knot:editor'; globalThis.bench = async () => { if (await editor.activeBuffer() === null) throw new Error('missing buffer'); };",
    );
    completed(pollster::block_on(execution))?;
    let started = Instant::now();
    let execution = runtime.execute_fixture_script("active-call", "globalThis.bench()");
    let request = pollster::block_on(runtime.receive_request()).context("active request")?;
    respond(
        &runtime,
        request,
        HostResponseValue::ActiveBuffer(Some(BUFFER)),
    )?;
    completed(pollster::block_on(execution))?;
    let elapsed = started.elapsed();
    runtime.shutdown();
    Ok(elapsed)
}

fn batched_edit_call(host: &V8Host, index: u64) -> Result<Duration> {
    let mut runtime = host.spawn_extension(ExtensionId::new(40_000 + index));
    let execution = runtime.execute_fixture_module(
        format!("file:///bench/edit-{index}.js"),
        "import { editor } from 'knot:editor'; globalThis.bench = async () => { const b = await editor.activeBuffer(); await b.applyEdits(Array.from({length: 10}, () => ({range: {startByteOffset: 0, endByteOffset: 0}, text: 'x'})), {ifRevision: 0}); };",
    );
    completed(pollster::block_on(execution))?;
    let started = Instant::now();
    let execution = runtime.execute_fixture_script("batched-edit", "globalThis.bench()");
    let request = pollster::block_on(runtime.receive_request()).context("active request")?;
    respond(
        &runtime,
        request,
        HostResponseValue::ActiveBuffer(Some(BUFFER)),
    )?;
    let request = pollster::block_on(runtime.receive_request()).context("edit request")?;
    let HostOperation::ApplyEdits { edits, .. } = &request.operation else {
        bail!("expected edit request")
    };
    if edits.len() != 10 {
        bail!("expected ten edits")
    }
    respond(
        &runtime,
        request,
        HostResponseValue::AppliedEdits { revision: 1 },
    )?;
    completed(pollster::block_on(execution))?;
    let elapsed = started.elapsed();
    runtime.shutdown();
    Ok(elapsed)
}

fn snapshot_call(host: &V8Host, index: u64, text: &str) -> Result<Duration> {
    snapshot_script(host, index, text, false)
}

fn utf16_adapter_call(host: &V8Host, index: u64, text: &str) -> Result<Duration> {
    snapshot_script(host, index, text, true)
}

fn snapshot_script(host: &V8Host, index: u64, text: &str, adapter: bool) -> Result<Duration> {
    let mut runtime = host.spawn_extension(ExtensionId::new(50_000 + index));
    let source = if adapter {
        "import { editor } from 'knot:editor'; globalThis.bench = async () => { const b = await editor.activeBuffer(); const s = await b.snapshot(); const byte = s.byteOffsetAtUtf16(s.text.length); if (s.utf16OffsetAtByte(byte) !== s.text.length) throw new Error('adapter'); };"
    } else {
        "import { editor } from 'knot:editor'; globalThis.bench = async () => { const b = await editor.activeBuffer(); const s = await b.snapshot(); if (s.text.length === 0) throw new Error('snapshot'); };"
    };
    let execution = runtime.execute_fixture_module(
        format!("file:///bench/snapshot-{adapter}-{index}.js"),
        source,
    );
    completed(pollster::block_on(execution))?;
    let started = Instant::now();
    let execution = runtime.execute_fixture_script("snapshot-call", "globalThis.bench()");
    let request = pollster::block_on(runtime.receive_request()).context("active request")?;
    respond(
        &runtime,
        request,
        HostResponseValue::ActiveBuffer(Some(BUFFER)),
    )?;
    let request = pollster::block_on(runtime.receive_request()).context("snapshot request")?;
    respond(&runtime, request, snapshot(text))?;
    completed(pollster::block_on(execution))?;
    let elapsed = started.elapsed();
    runtime.shutdown();
    Ok(elapsed)
}

fn jit_warmup(host: &V8Host) -> Result<(Duration, Duration)> {
    let mut runtime = host.spawn_extension(ExtensionId::new(60_000));
    let setup = runtime.execute_fixture_module(
        "file:///bench/jit-setup.js",
        "import { editor } from 'knot:editor'; globalThis.bench = async () => { for (let i = 0; i < 32; i++) await editor.activeBuffer(); };",
    );
    completed(pollster::block_on(setup))?;
    let run = |runtime: &mut ExtensionRuntimeHandle, suffix: &str| -> Result<Duration> {
        let started = Instant::now();
        let execution =
            runtime.execute_fixture_script(format!("jit-{suffix}"), "globalThis.bench()");
        for _ in 0..32 {
            let request = pollster::block_on(runtime.receive_request()).context("JIT host call")?;
            respond(
                runtime,
                request,
                HostResponseValue::ActiveBuffer(Some(BUFFER)),
            )?;
        }
        completed(pollster::block_on(execution))?;
        Ok(started.elapsed())
    };
    let cold = run(&mut runtime, "cold")?;
    let warm = run(&mut runtime, "warm")?;
    runtime.shutdown();
    Ok((cold, warm))
}

fn event_fanout(
    host: &V8Host,
    samples: usize,
) -> Result<(Vec<Duration>, BufferChangeQueueMetrics)> {
    let mut all = Vec::with_capacity(samples);
    let mut latest = BufferChangeQueueMetrics::default();
    for sample in 0..samples {
        let mut first = host.spawn_extension(ExtensionId::new(70_000 + sample as u64 * 2));
        let mut second = host.spawn_extension(ExtensionId::new(70_001 + sample as u64 * 2));
        for (runtime, subscription) in [
            (&mut first, BufferSubscriptionId::new(1)),
            (&mut second, BufferSubscriptionId::new(2)),
        ] {
            let execution = runtime.execute_fixture_module(
                format!("file:///bench/listener-{sample}-{}.js", subscription.value()),
                "import { editor } from 'knot:editor'; const b = await editor.activeBuffer(); await b.onDidChange(() => { globalThis.eventCount = (globalThis.eventCount ?? 0) + 1; });",
            );
            let request =
                pollster::block_on(runtime.receive_request()).context("listener buffer")?;
            respond(
                runtime,
                request,
                HostResponseValue::ActiveBuffer(Some(BUFFER)),
            )?;
            let request =
                pollster::block_on(runtime.receive_request()).context("listener subscription")?;
            respond(
                runtime,
                request,
                HostResponseValue::BufferChangesSubscribed { subscription },
            )?;
            completed(pollster::block_on(execution))?;
        }
        let started = Instant::now();
        for revision in 1..=8 {
            let change = BufferChange {
                buffer: BUFFER,
                before_revision: revision - 1,
                revision,
                edits: vec![],
            };
            first
                .dispatch_buffer_change(BufferSubscriptionId::new(1), change.clone())
                .map_err(|_| anyhow::anyhow!("first runtime closed"))?;
            second
                .dispatch_buffer_change(BufferSubscriptionId::new(2), change)
                .map_err(|_| anyhow::anyhow!("second runtime closed"))?;
        }
        completed(pollster::block_on(first.execute_fixture_script(
            "verify-first",
            "if (globalThis.eventCount !== 8) throw new Error('lost event')",
        )))?;
        completed(pollster::block_on(second.execute_fixture_script(
            "verify-second",
            "if (globalThis.eventCount !== 8) throw new Error('lost event')",
        )))?;
        all.push(started.elapsed());
        latest = first.buffer_change_queue_metrics();
        first.shutdown();
        second.shutdown();
    }
    Ok((all, latest))
}

fn idle_rss_per_isolate(host: &V8Host) -> Result<usize> {
    let before = resident_memory();
    let mut runtimes = Vec::with_capacity(4);
    for index in 0..4 {
        let runtime = host.spawn_extension(ExtensionId::new(80_000 + index));
        completed(pollster::block_on(runtime.execute_fixture_script(
            "rss-probe",
            "globalThis.rssProbe = true",
        )))?;
        runtimes.push(runtime);
    }
    let after = resident_memory();
    for runtime in runtimes {
        runtime.shutdown();
    }
    Ok(after.saturating_sub(before) / 4)
}

fn resident_memory() -> usize {
    let pid = sysinfo::get_current_pid().expect("current process id");
    let mut system = sysinfo::System::new();
    system.refresh_processes(sysinfo::ProcessesToUpdate::Some(&[pid]), true);
    system
        .process(pid)
        .map_or(0, |process| process.memory() as usize / 1024)
}

fn snapshot(text: &str) -> HostResponseValue {
    HostResponseValue::Snapshot(TextSnapshot {
        text: text.to_owned(),
        range: ByteRange {
            start_byte_offset: 0,
            end_byte_offset: text.len(),
        },
        revision: 0,
    })
}

fn respond(
    runtime: &ExtensionRuntimeHandle,
    request: HostRequest,
    result: HostResponseValue,
) -> Result<()> {
    runtime
        .respond(HostResponse {
            extension: request.extension,
            lifecycle: request.lifecycle,
            id: request.id,
            result: Ok(result),
        })
        .map_err(|error| anyhow::anyhow!("runtime rejected response: {error:?}"))?;
    Ok(())
}

fn completed(result: std::result::Result<(), ExtensionRuntimeExecutionError>) -> Result<()> {
    result.map_err(|error| anyhow::anyhow!("fixture execution failed: {error:?}"))
}

fn transfer_cases() -> Vec<(&'static str, usize, String)> {
    [
        ("string-marshalling-ascii-1024", 1024, "a".repeat(1024)),
        (
            "string-marshalling-unicode-102400",
            102_400,
            "é".repeat(51_200),
        ),
        (
            "string-marshalling-unicode-10485760",
            10_485_760,
            "😀".repeat(2_621_440),
        ),
    ]
    .into_iter()
    .collect()
}
