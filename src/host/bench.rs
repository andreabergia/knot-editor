//! Release diagnostics for the production extension pool.

use std::{
    num::NonZeroUsize,
    process,
    sync::Arc,
    time::{Duration, Instant},
};

use anyhow::{Context, Result, bail, ensure};

use super::{
    lifecycle::{ExtensionKey, ExtensionState},
    pool::{ExtensionConfig, ExtensionEvent, ExtensionEventInbox, ExtensionPool},
    protocol::{
        BufferChange, BufferHandle, BufferSubscriptionId, ByteRange, ExtensionId,
        ExtensionLifecycleId, HostOperation, HostRequest, HostResponse, HostResponseValue,
        SnapshotText, TextEdit, TextSnapshot,
    },
    scheduler::PoolConfig,
};

const EVENT_TIMEOUT: Duration = Duration::from_secs(10);
const TRANSFER_BYTES: usize = 10 * 1024 * 1024;

pub fn run(mut args: impl Iterator<Item = String>) -> Result<()> {
    let mut samples = 3;
    let mut workers = 2;
    let mut stress = false;
    while let Some(argument) = args.next() {
        match argument.as_str() {
            "--samples" => samples = positive(&args.next().context("--samples needs a value")?)?,
            "--workers" => workers = positive(&args.next().context("--workers needs a value")?)?,
            "--stress" => stress = true,
            _ => bail!(
                "unknown argument: {argument}; usage: v8-bench [--samples N] [--workers N] [--stress]"
            ),
        }
    }
    let initialization = Instant::now();
    super::engine::initialize();
    let initialization = initialization.elapsed();
    println!("V8 process initialization: {initialization:?}");
    if stress {
        return stress_pool(workers);
    }
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .build()?;
    let mut startup = Vec::new();
    let mut host_call = Vec::new();
    let mut edit = Vec::new();
    let mut transfer = Vec::new();
    let mut fanout = Vec::new();
    let mut idle_mib = Vec::new();
    let mut max_depth = 0;
    let mut max_lag = Duration::ZERO;
    let mut max_queue_lag = Duration::ZERO;
    let mut movements = 0;
    let mut turns = 0;
    let mut max_threads = 0;
    println!(
        "v8-bench: samples={samples}, configured workers={workers}, transfer={} MiB",
        TRANSFER_BYTES / 1024 / 1024
    );
    for sample in 0..samples {
        let baseline_rss = resident_bytes();
        let (pool, mut inbox) = ExtensionPool::new(config(workers));
        let keys = [key(1, sample as u64 + 1), key(2, sample as u64 + 1)];
        let started = Instant::now();
        for key in keys {
            pool.load(key, ExtensionConfig::default())?;
        }
        startup.push(started.elapsed() / keys.len() as u32);
        if let (Some(before), Some(after)) = (baseline_rss, resident_bytes()) {
            idle_mib.push((after.saturating_sub(before)) as f64 / keys.len() as f64 / 1048576.0);
        }
        max_threads = max_threads.max(process_threads().unwrap_or(0));
        ensure!(
            pool.diagnostics().worker_count == workers,
            "pool worker count changed"
        );

        let first = keys[0];
        let started = Instant::now();
        let execution = pool.execute_fixture_module(
            first,
            module(sample, "active"),
            "import { editor } from 'knot:editor'; await editor.activeBuffer();",
        )?;
        let request = receive(&runtime, &mut inbox)?;
        ensure!(
            request.operation == HostOperation::ActiveBuffer,
            "unexpected host call"
        );
        respond(
            &pool,
            request,
            HostResponseValue::ActiveBuffer(Some(BufferHandle::new(1))),
        )?;
        execution.wait()?;
        host_call.push(started.elapsed());

        let started = Instant::now();
        let execution = pool.execute_fixture_module(first, module(sample, "edit"),
            "import { editor } from 'knot:editor'; const b = await editor.activeBuffer(); await b.applyEdits(Array.from({length:10}, (_,i) => ({range:{startByteOffset:i,endByteOffset:i},text:'x'})), {ifRevision:1});")?;
        let request = receive(&runtime, &mut inbox)?;
        respond(
            &pool,
            request,
            HostResponseValue::ActiveBuffer(Some(BufferHandle::new(1))),
        )?;
        let request = receive(&runtime, &mut inbox)?;
        ensure!(
            matches!(&request.operation, HostOperation::ApplyEdits { edits, .. } if edits.len() == 10),
            "unexpected edit request"
        );
        respond(
            &pool,
            request,
            HostResponseValue::AppliedEdits { revision: 2 },
        )?;
        execution.wait()?;
        edit.push(started.elapsed());

        let text: Arc<[u16]> = vec![b'a' as u16; TRANSFER_BYTES].into();
        let started = Instant::now();
        let execution = pool.execute_fixture_module(first, module(sample, "transfer"),
            "import { editor } from 'knot:editor'; const b = await editor.activeBuffer(); const s = await b.snapshot(); if (s.text.length !== 10485760) throw Error('transfer length');")?;
        let request = receive(&runtime, &mut inbox)?;
        respond(
            &pool,
            request,
            HostResponseValue::ActiveBuffer(Some(BufferHandle::new(1))),
        )?;
        let request = receive(&runtime, &mut inbox)?;
        ensure!(
            matches!(request.operation, HostOperation::Snapshot { .. }),
            "unexpected snapshot request"
        );
        respond(
            &pool,
            request,
            HostResponseValue::Snapshot(TextSnapshot {
                text: SnapshotText::Utf16(text),
                range: ByteRange {
                    start_byte_offset: 0,
                    end_byte_offset: TRANSFER_BYTES,
                },
                revision: 2,
            }),
        )?;
        execution.wait()?;
        transfer.push(started.elapsed());

        for key in keys {
            let execution = pool.execute_fixture_module(key, module(sample, &format!("listener-{}", key.extension.value())),
                "import { editor } from 'knot:editor'; const b = await editor.activeBuffer(); globalThis.events = 0; await b.onDidChange(() => { events++; for(let i=0;i<10000;i++) {} });")?;
            let request = receive(&runtime, &mut inbox)?;
            respond(
                &pool,
                request,
                HostResponseValue::ActiveBuffer(Some(BufferHandle::new(1))),
            )?;
            let request = receive(&runtime, &mut inbox)?;
            ensure!(
                matches!(
                    request.operation,
                    HostOperation::SubscribeBufferChanges { .. }
                ),
                "unexpected subscription request"
            );
            respond(
                &pool,
                request,
                HostResponseValue::BufferChangesSubscribed {
                    subscription: BufferSubscriptionId::new(key.extension.value()),
                },
            )?;
            execution.wait()?;
        }
        let started = Instant::now();
        for revision in 0..8 {
            for key in keys {
                pool.dispatch_buffer_change(
                    key,
                    BufferSubscriptionId::new(key.extension.value()),
                    BufferChange {
                        buffer: BufferHandle::new(1),
                        before_revision: revision,
                        revision: revision + 1,
                        edits: vec![TextEdit {
                            range: ByteRange {
                                start_byte_offset: 0,
                                end_byte_offset: 0,
                            },
                            text: "x".into(),
                        }],
                    },
                )?;
            }
        }
        for key in keys {
            pool.execute_fixture_module(
                key,
                module(sample, &format!("barrier-{}", key.extension.value())),
                "if (events !== 8) throw Error(`received ${events} events`);",
            )?
            .wait()?;
            let metrics = pool.buffer_change_queue_metrics(key)?;
            max_depth = max_depth.max(metrics.max_depth);
            max_lag = max_lag.max(metrics.max_enqueue_to_start_lag);
        }
        fanout.push(started.elapsed());
        let diag = pool.diagnostics();
        max_queue_lag = max_queue_lag.max(diag.max_enqueue_to_start_lag);
        movements += diag.worker_movements;
        turns += diag.turn_count;
        for key in keys {
            pool.unload(key)?;
        }
        Arc::try_unwrap(pool)
            .unwrap_or_else(|_| panic!("benchmark pool leaked"))
            .shutdown();
    }
    print_duration("incremental isolate startup", &mut startup);
    print_duration("active-buffer host call", &mut host_call);
    print_duration("batched edit, 10 edits", &mut edit);
    print_duration("external UTF-16 snapshot, 10 MiB", &mut transfer);
    print_duration("event fan-out, 2 isolates x 8 events", &mut fanout);
    if !idle_mib.is_empty() {
        idle_mib.sort_by(f64::total_cmp);
        println!(
            "idle RSS / isolate: {:.2} MiB (process delta)",
            idle_mib[idle_mib.len() / 2]
        );
    }
    println!("slow-consumer burst: max depth={max_depth}, max start lag={max_lag:?}");
    println!(
        "scheduler: turns={turns}, movements={movements}, max queue wait={max_queue_lag:?}, max process threads={max_threads}"
    );
    Ok(())
}

fn config(workers: usize) -> PoolConfig {
    PoolConfig::new(NonZeroUsize::new(workers).expect("validated worker count"))
}

fn positive(value: &str) -> Result<usize> {
    let number: usize = value.parse().context("expected a positive integer")?;
    ensure!(number > 0, "expected a positive integer");
    Ok(number)
}

fn key(extension: u64, lifecycle: u64) -> ExtensionKey {
    ExtensionKey::new(
        ExtensionId::new(extension),
        ExtensionLifecycleId::new(lifecycle),
    )
}

fn module(sample: usize, label: &str) -> String {
    format!("file:///fixtures/bench-{sample}-{label}.js")
}

fn receive(
    runtime: &tokio::runtime::Runtime,
    inbox: &mut ExtensionEventInbox,
) -> Result<HostRequest> {
    loop {
        let event = runtime
            .block_on(async { tokio::time::timeout(EVENT_TIMEOUT, inbox.receive()).await })
            .context("host event timed out")?
            .context("host event stream closed")?;
        if let ExtensionEvent::Request(request) = event {
            return Ok(request);
        }
        bail!("extension ended during benchmark: {event:?}");
    }
}

fn respond(pool: &ExtensionPool, request: HostRequest, value: HostResponseValue) -> Result<()> {
    pool.respond(HostResponse {
        extension: request.extension,
        lifecycle: request.lifecycle,
        id: request.id,
        result: Ok(value),
    })
    .map_err(|error| anyhow::anyhow!("host response rejected: {error:?}"))?;
    Ok(())
}

fn print_duration(label: &str, samples: &mut [Duration]) {
    samples.sort();
    println!("{label}: {:?} p50", samples[samples.len() / 2]);
}

fn resident_bytes() -> Option<u64> {
    let output = process::Command::new("ps")
        .args(["-o", "rss=", "-p", &process::id().to_string()])
        .output()
        .ok()?;
    let kib: u64 = String::from_utf8(output.stdout).ok()?.trim().parse().ok()?;
    Some(kib * 1024)
}

fn process_threads() -> Option<usize> {
    let output = process::Command::new("ps")
        .args(["-M", "-p", &process::id().to_string()])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    Some(
        String::from_utf8(output.stdout)
            .ok()?
            .lines()
            .count()
            .saturating_sub(1),
    )
}

fn stress_pool(workers: usize) -> Result<()> {
    let count = workers * 4 + 1;
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .build()?;
    let mut total_turns = 0;
    let mut total_movements = 0;
    for cycle in 0..3 {
        let (pool, mut inbox) = ExtensionPool::new(config(workers));
        let mut keys = Vec::new();
        for id in 1..=count {
            let key = key(id as u64, cycle + 1);
            pool.load(key, ExtensionConfig::default())?;
            keys.push(key);
        }
        ensure!(
            pool.diagnostics().worker_count == workers,
            "workers grew with extension count"
        );
        for round in 0..8 {
            let mut executions = Vec::new();
            for key in &keys {
                executions.push(pool.execute_fixture_module(*key, module(round, &format!("stress-{cycle}-{}", key.extension.value())),
                    format!("globalThis.counter = (globalThis.counter ?? 0) + 1; if (counter !== {}) throw Error('persistent state');", round + 1))?);
            }
            for execution in executions {
                execution.wait()?;
            }
        }
        let waiting = pool.execute_fixture_module(keys[0], module(cycle as usize, "waiting"),
            "import { editor } from 'knot:editor'; await editor.activeBuffer(); globalThis.resumed = true;")?;
        let delayed = receive(&runtime, &mut inbox)?;
        ensure!(
            delayed.operation == HostOperation::ActiveBuffer,
            "unexpected delayed operation"
        );
        pool.execute_fixture_module(
            keys[1],
            module(cycle as usize, "neighbor"),
            "if (counter !== 8) throw Error('waiting extension held worker');",
        )?
        .wait()?;
        respond(&pool, delayed, HostResponseValue::ActiveBuffer(None))?;
        waiting.wait()?;
        pool.execute_fixture_module(
            keys[0],
            module(cycle as usize, "resumed"),
            "if (!resumed || counter !== 8) throw Error('lost continuation state');",
        )?
        .wait()?;
        let failed = pool.execute_fixture_module(
            keys[2],
            module(cycle as usize, "fatal"),
            "import { editor } from 'knot:editor'; await editor.activeBuffer();",
        )?;
        let fatal_request = receive(&runtime, &mut inbox)?;
        ensure!(
            fatal_request.extension == keys[2].extension,
            "wrong fatal probe request"
        );
        let watchdog = pool.watchdog(keys[2])?;
        ensure!(watchdog.terminate(), "watchdog did not terminate isolate");
        drop(watchdog);
        ensure!(failed.wait().is_err(), "fatal probe unexpectedly succeeded");
        pool.execute_fixture_module(
            keys[3],
            module(cycle as usize, "after-failure"),
            "if (counter !== 8) throw Error('neighbor was affected by failure');",
        )?
        .wait()?;
        let diag = pool.diagnostics();
        ensure!(
            diag.worker_count == workers && diag.extensions.len() == count,
            "pool size changed under stress"
        );
        ensure!(
            diag.extensions
                .iter()
                .any(|entry| entry.key == keys[2]
                    && matches!(entry.state, ExtensionState::Failed(_))),
            "fatal lifecycle was not recorded"
        );
        ensure!(
            diag.turn_count >= (count * 8 + 5) as u64,
            "missing stress turns"
        );
        total_turns += diag.turn_count;
        total_movements += diag.worker_movements;
        println!(
            "stress cycle {}: {count} extensions, {workers} workers, {} turns, {} movements, max queue wait {:?}",
            cycle + 1,
            diag.turn_count,
            diag.worker_movements,
            diag.max_enqueue_to_start_lag
        );
        let failed_key = keys[2];
        for key in keys {
            if key != failed_key {
                pool.unload(key)?;
            }
        }
        Arc::try_unwrap(pool)
            .unwrap_or_else(|_| panic!("stress pool leaked"))
            .shutdown();
    }
    println!(
        "stress total: {total_turns} turns, {total_movements} movements, 3 load/unload cycles"
    );
    Ok(())
}
