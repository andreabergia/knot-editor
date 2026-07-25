# V8 Runtime Evidence

The runtime experiment validated the Knot-owned JavaScript boundary: lifecycle,
commands, revisioned buffer access, events, cancellation, failure isolation,
and transfer cost.

## Validated behavior

- Each prototype extension runs a persistent V8 isolate on its own OS thread.
- Callbacks are serial within one extension and execute in parallel across
  extensions.
- gpui remains the sole owner of editor state. Typed messages carry requests
  between extension threads and the foreground.
- Cancellation rejects late edits. Startup failure, thrown handlers, rejected
  promises, buffer closure, disposal, and forced CPU interruption leave another
  extension and the foreground responsive.
- Change events are ordered and lossless for the tested finite slow-consumer
  burst.
- V8 and Deno types remain inside `host`; the public API exposes Knot concepts
  and opaque handles.

## Measurements

`cargo run --release --bin v8-bench -- --samples 12`, measured 2026-07-23 on
Apple M1 Pro, macOS 26.5.2, Rust 1.97.1, `deno_core` 0.408.0 / V8 149.4.0.
Values are p50 unless noted.

| Workload | Result |
| --- | ---: |
| Process-wide V8 initialization | 8.9 ms |
| Incremental isolate startup | 4.2 ms |
| Built-in module initialization | 4.0 ms |
| Idle RSS per isolate | 2.3 MiB |
| Active-buffer host call | 194 µs |
| Batched edit, 10 edits | 332 µs |
| ASCII string marshal, 10 MiB | 2.6 ms |
| Unicode string marshal, 10 MiB | 36.0 ms |
| External UTF-16, cold Unicode, 10 MiB | 10.6 ms |
| External UTF-16, cached Unicode, 10 MiB | 321 µs |
| External UTF-16, cold ASCII, 10 MiB | 10.7 ms |
| External UTF-16, cached ASCII, 10 MiB | 391 µs |
| Event fan-out, 2 isolates × 8 events | 229 µs |
| Slow-consumer burst | depth 8; max start lag 192 µs |

The 10 MiB JavaScript UTF-16 boundary-table adapter exceeded the prototype's
32 MiB isolate heap cap. Raw transfer succeeded. Large adapters should remain
scoped rather than being triggered for whole-document snapshots.

## Decisions

- JavaScript on V8 is the extension language/runtime.
- Keep `deno_core` as replaceable prototype machinery behind the Knot-owned
  host protocol. Thread-per-extension is not the production topology; the
  target remains a bounded movable-isolate pool on `rusty_v8`.
- Keep the editable core UTF-8-native. Retain one immutable UTF-16 snapshot
  allocation keyed by revision and range, shared across isolates through V8
  external strings and invalidated on edit.
- Small scoped strings are acceptable and edits remain batched. Current
  evidence does not justify a public byte-array text API.
- Forced interruption terminates one extension. Production quotas,
  slow-consumer policy, packaging, and module resolution remain open.
