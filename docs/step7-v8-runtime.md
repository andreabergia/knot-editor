# Step 7 — V8 scripting runtime

Status: language and engine decided; implementation not started.

## Decisions

- Knot uses JavaScript on V8 as its embedded application language.
  Configuration, built-in behavior, and third-party extensions are ordinary
  JavaScript using the same public APIs. There is no separate configuration
  format or privileged built-in scripting surface.
- The native Rust core owns performance-sensitive substrates, including text
  storage and editing primitives. Most higher-level editor behavior lives in
  JavaScript so that it can be inspected, replaced, and extended.
- The Rust/JavaScript boundary must be coarse enough to avoid an FFI crossing
  for every small operation. Native APIs should expose semantic operations and
  batch data transfer where appropriate.
- Each extension runs in its own V8 isolate. Isolates separate JavaScript state
  and failures, but are not a complete security boundary.
- Isolates are scheduled over a bounded worker pool away from the UI thread.
  Different extension isolates may execute callbacks concurrently. A single
  isolate executes at most one callback at a time and should be able to move
  between workers between callbacks if V8 and `deno_core` permit it cleanly.
- Tokio handles asynchronous host work, timers, cancellation, and message
  routing. CPU-bound JavaScript execution does not run on Tokio's general
  worker threads.
- The prototype uses `deno_core` for V8 lifecycle, modules, promises, async Rust
  operations, and related runtime plumbing.
- `deno_core` is private implementation machinery behind a strict Knot-owned
  V8 wrapper. Knot's core model and public JavaScript API must not expose Deno
  operations, resource tables, extension modules, state containers, or other
  Deno-specific concepts.
- The wrapper is deliberately specific to Knot's V8 runtime. It is not a
  generic `ScriptingEngine`, does not support multiple implementations, and
  does not abstract JavaScript or V8 semantics away.
- Public JavaScript modules and objects are Knot-owned. Private native bindings
  may use `deno_core` facilities, but package and built-in code consume only the
  Knot API layered above them.
- The prototype must exercise forced interruption, CPU and memory limits, and
  noisy-neighbor behavior enough to validate the shared-pool architecture.
  Production quota policy and process isolation are deferred.

## Questions for the prototype

### Runtime foundation

- Can separate isolates genuinely execute in parallel over a bounded pool?
- Can an isolate safely move between pool workers between callbacks, without
  ever being entered concurrently?
- What are the startup-time and memory costs per extension isolate?
- Can interruption and basic CPU/memory limits stop one extension from
  monopolizing a worker or exhausting the host?
- How are an extension's pending work and registrations disposed together?

### Native boundary

- Which editor values cross as snapshots, stable handles, or owned data?
- What is the lifetime model for buffers, views, workspaces, annotations, and
  commands referenced by JavaScript?
- Which calls need batching or zero-copy buffers to keep boundary overhead
  negligible?
- How are buffer revisions carried through asynchronous work so stale results
  can be detected before they mutate editor state?

### `deno_core` containment

- Can module loading, event-loop driving, promise rejection reporting,
  inspector support, and native bindings remain entirely inside the V8 wrapper?
- Can the public API be expressed as Knot-owned JavaScript modules without
  leaking `Deno.core`, `ext:` modules, op identifiers, `OpState`, or resource
  table semantics?
- Do `op2` and CppGC provide the required host-object ergonomics and performance
  without forcing Deno-specific types into the core model?
- Which `deno_core` facilities are essential, and which should Knot avoid so
  the wrapper remains small and understandable?

### Commands

- Can an extension register and invoke a command, read and edit a buffer, await
  host work, and be cancelled without a late mutation?
- Are thrown and rejected errors contained and reported with useful source
  information?

### Events

- Can committed, revisioned buffer changes fan out to extension isolates in
  parallel while remaining ordered within each extension?
- What backpressure rule prevents a slow extension from accumulating an
  unbounded event queue?
- Does disposing a subscription reliably remove it and its queued delivery?

### Errors and tooling

- How are JavaScript exceptions, asynchronous rejection stacks, and source
  locations presented inside Knot?
- What minimum Inspector Protocol integration is needed during the prototype?
- How are failures in one event subscriber reported without suppressing other
  subscribers?

### Performance evidence

- Measure cold startup, idle memory, and Knot built-in module initialization
  cost per isolate, separating any process-wide shared cost where possible.
- Measure synchronous and asynchronous Rust/JavaScript calls with realistic
  arguments rather than arithmetic-only engine benchmarks.
- Measure parallel event fan-out, promise scheduling latency, JIT warm-up,
  sustained workloads, and large data transfers.
- Confirm that scripted higher-level behavior remains responsive while native
  text editing and rendering continue on their owning threads.

## Deliberately separate explorations

- Views and native editor contribution points are a UI problem, not part of
  this runtime/command/event experiment.
- Generic providers are deferred until a concrete capability needs aggregation.
- Parallel callbacks within one extension require multiple isolates and
  duplicated module state. They are not implicit; an explicit worker API may
  be explored later.
