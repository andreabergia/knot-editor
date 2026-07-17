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
- Knot has one shared V8 runtime. Installed code is trusted to coexist in that
  shared environment; package isolation is not a security boundary.
- V8 runs away from the UI thread. Extension callbacks are scheduled as
  independent tasks so asynchronous work can overlap and one extension's
  failure does not prevent other subscribers from being notified. JavaScript
  execution within the isolate remains serial.
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
- Forced interruption, watchdog policy, hostile-extension containment, and
  worker or process isolation are deferred beyond the first vertical slice.

## Questions for the prototype

### Runtime ownership and scheduling

- How does the dedicated V8 thread integrate with the gpui thread and native
  worker executors without making ordinary editor operations excessively
  chatty?
- What ordering and backpressure rules apply when several extensions subscribe
  to a high-frequency event such as a buffer change?
- How are extension tasks, subscriptions, pending promises, and cancellation
  grouped so unloading an extension reliably disposes all of its work?

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

### JavaScript API and modules

- What is the initial module namespace and resolution model for built-ins,
  user configuration, and extensions?
- How are commands, event subscriptions, options, and editor services exposed
  idiomatically while preserving useful validation and source diagnostics?
- What state survives module reload, and how are replaced registrations and
  subscriptions disposed?

### Errors and tooling

- How are JavaScript exceptions, asynchronous rejection stacks, and source
  locations presented inside Knot?
- What minimum Inspector Protocol integration is needed during the prototype?
- How are failures in one event subscriber reported without suppressing other
  subscribers?

### Performance evidence

- Measure cold startup, idle memory, and the cost of initializing Knot's
  built-in JavaScript modules.
- Measure synchronous and asynchronous Rust/JavaScript calls with realistic
  arguments rather than arithmetic-only engine benchmarks.
- Measure event fan-out to multiple extensions, promise scheduling latency,
  JIT warm-up, sustained built-in workloads, and large data transfers.
- Confirm that scripted higher-level behavior remains responsive while native
  text editing and rendering continue on their owning threads.

