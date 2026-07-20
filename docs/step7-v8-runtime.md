# Step 7 — V8 scripting runtime

Status: plan agreed; implementation not started.

## Goal and decisions

Validate that JavaScript on V8 can provide Knot's first real extension boundary
without blocking gpui or leaking runtime internals into the editor architecture.
The experiment covers runtime lifecycle, commands, buffer access, events,
cancellation, failure isolation, and the cost of crossing the Rust/JavaScript
boundary. Views and native editor contributions remain step 8.

- `deno_core` is prototype machinery behind a Knot-owned wrapper. Public
  modules expose Knot concepts only; extensions cannot consume Deno ops,
  resource IDs, `ext:` modules, `OpState`, or Rust data structures.
- The target architecture remains one isolate per extension on a bounded V8
  worker pool, implemented directly with `rusty_v8`. Because
  `deno_core::JsRuntime` is `!Send`, this prototype gives each extension one OS
  thread for its lifetime. Thread affinity is a prototype trade-off, not an
  architectural decision.
- A shared Tokio runtime performs asynchronous native work, timers,
  cancellation, and message routing. JavaScript executes only on extension
  threads; the gpui foreground thread remains the sole owner of editor state.
- Knot is UTF-8-native. Public text ranges are half-open UTF-8 byte ranges;
  field names say `byteOffset` rather than the ambiguous `character`. UTF-16
  conversion exists only at boundaries that require it, such as DAP, an LSP
  server that does not negotiate UTF-8, or JavaScript string indices.
- Forced interruption is fatal to the affected extension. Its isolate,
  commands, subscriptions, pending operations, and queued callbacks are all
  disposed; it is not automatically restarted.
- Slow-consumer/backpressure policy is deliberately deferred. The prototype
  verifies ordering with finite bursts and records queue depth and lag without
  claiming a production policy.
- Extension loading is limited to static fixture modules and Knot's built-in
  API module. Filesystem resolution, TypeScript, npm, manifests, dependency
  resolution, hot reload, generated bindings, and packaging are out of scope.

## Public JavaScript surface

Load extensions as ES modules which import from `knot:editor`. Bootstrap code
captures private native bindings, builds the public facade, then removes Deno's
binding globals before any extension module executes.

The prototype API is intentionally small and extension-oriented:

```ts
type ByteRange = { startByteOffset: number; endByteOffset: number };
type TextEdit = { range: ByteRange; text: string };

interface TextSnapshot {
  readonly text: string;
  readonly range: ByteRange;
  readonly revision: number;

  // Explicit adapter helpers. Both reject offsets that split an encoded value.
  byteOffsetAtUtf16(utf16CodeUnitOffset: number): number;
  utf16OffsetAtByte(byteOffset: number): number;
}

interface TextBuffer {
  snapshot(range?: ByteRange): Promise<TextSnapshot>;
  applyEdits(
    edits: readonly TextEdit[],
    options: { ifRevision: number },
  ): Promise<{ revision: number }>;
  onDidChange(listener: (event: BufferChangeEvent) => void | Promise<void>): Disposable;
}

interface BufferChangeEvent {
  readonly buffer: TextBuffer;
  readonly beforeRevision: number;
  readonly revision: number;
  readonly edits: readonly TextEdit[];
}

interface CommandContext {
  readonly buffer: TextBuffer | null;
  readonly signal: AbortSignal;
}

interface Disposable { dispose(): void; }

export const editor: {
  activeBuffer(): Promise<TextBuffer | null>;
};

export const commands: {
  register(
    name: string,
    handler: (context: CommandContext, ...args: unknown[]) => unknown | Promise<unknown>,
  ): Disposable;
};
```

`TextBuffer` is a JavaScript proxy over an opaque private handle. Handle values,
transport messages, Rust references, and locking primitives are never public.
A closed buffer rejects future operations with a stable `BufferClosedError`.

All edit ranges refer to the snapshot revision supplied in `ifRevision`.
`applyEdits` validates UTF-8 boundaries, requires ranges to be sorted and
non-overlapping in that revision, applies them in reverse order as one
editor-visible commit, and rejects stale revisions with `RevisionConflictError`.
No-op batches return the current revision and emit no event. Events contain the
committed batch in pre-commit coordinates and are delivered serially, in order,
within each extension; different extension threads may process them in parallel.

String conversion is not hidden by pretending it is free. `snapshot(range)`
transfers only requested text, `applyEdits` crosses once per batch, and change
events carry inserted text rather than a fresh buffer snapshot. The benchmark
must measure UTF-8↔V8-string conversion separately from channel and op overhead
at 1 KiB, 100 KiB, and 10 MiB. Do not add a public `Uint8Array`/zero-copy API in
this step unless the evidence shows the string API is an actual bottleneck;
record that finding for the API design instead.

## Implementation plan

### 1. Runtime foundation

- [ ] Add `deno_core` and Tokio, initialize the V8 platform on the process's
  parent thread, and implement `host::V8Host` plus one thread-affine
  `ExtensionRuntime` per loaded extension.
  - [x] Add `deno_core` and Tokio; `host::V8Host` initializes V8 on its
    construction thread and owns the shared Tokio runtime. ✅
  - [x] Define Knot-owned typed request/response messages for the initial
    buffer API boundary; their opaque identities and payloads contain no V8,
    Deno, gpui, or editor-model references. ✅
  - [x] Add the thread-affine `ExtensionRuntime` once the typed host
    request/response protocol exists. It owns request-ID allocation and typed
    response validation on a dedicated extension OS thread. ✅
  - [x] Construct and drive one `JsRuntime` exclusively on each extension
    thread. The fixture-script proof retains isolate state across executions;
    async host ops and module loading remain later slices. ✅
  - [x] Give each extension thread a current-thread Tokio driver for V8 and
    `!Send` Deno futures. `V8Host` retains the shared multi-thread Tokio
    runtime for future `Send` native work. ✅
  - [x] Cover the initial runtime shell with focused tests for V8/Tokio
    initialization, isolate persistence, JavaScript failure isolation, and
    typed request/response validation. ✅
- [ ] Drive each `JsRuntime` from its owning thread while entering the shared
  Tokio runtime for async ops. Use typed Knot-owned request/response messages;
  keep all `deno_core` types inside `host`.
  - [x] Prove an internal async op can await `Send` native work on `V8Host`'s
    shared Tokio runtime while V8 remains driven by its extension thread. ✅
  - [x] Bridge the fixture `ActiveBuffer` op through typed host messages and
    resolve its JavaScript promise from the matching typed response. ✅
- [x] Implement the static module loader, private bootstrap bindings,
  `knot:editor` facade, source-aware exception/rejection reporting, and a test
  proving extension code cannot access `Deno.core` or import private modules. ✅
  - [x] Add a static in-memory loader and evaluate fixture ES modules on their
    owning extension thread. It accepts absolute URL specifiers only and has
    no filesystem or package resolution. ✅
  - [x] Evaluate a private `knot:bootstrap` module before extension code. It
    captures its native op binding in module-local scope and rejects direct
    extension imports; the future `knot:editor` facade is its sole importer. ✅
  - [x] Add the initial `knot:editor` facade for the fixture active-buffer
    operation, then remove Deno's private binding global before extension code
    runs. ✅
  - [x] Return JavaScript exception and rejection reports with their source
    location while keeping the extension runtime usable after failure. ✅
- [x] Give each extension a lifecycle token owning its commands,
  subscriptions, queued callbacks, pending promises, and cancellation state.
  Normal unload and initialization failure run the same idempotent teardown. ✅
  - [x] Introduce the lifecycle teardown foundation: it owns pending JavaScript
    promises and cancellation state today, rejects late responses after unload,
    and is idempotently invoked after normal shutdown or failed thread startup.
    Command, subscription, and callback registries will join this token when
    those APIs land. ✅
- [ ] Expose V8's thread-safe isolate handle to a watchdog. A synchronous CPU
  runaway is terminated and then tears down the extension. Configure a small
  test heap limit/near-limit callback and run heap exhaustion in a sacrificial
  test process first; record a limitation rather than risking editor-process
  abort if V8 cannot recover safely.
  - [x] Attach each extension isolate's private thread-safe handle to its
    lifecycle-owned watchdog. The handle remains inside `host` and is cleared
    during lifecycle teardown; termination policy and heap probing remain
    separate slices. ✅
  - [x] Let the host watchdog terminate synchronous JavaScript execution. A
    termination is fatal to that extension: fixture execution reports
    `Terminated`, its command loop exits, and lifecycle teardown disposes the
    isolate's pending state. ✅
  - [x] Make unload terminate the isolate before queuing shutdown and joining
    its thread, keeping the watchdog attached until thread-owned teardown so a
    CPU runaway cannot block unload. Termination requested during bootstrap is
    deferred until initialization finishes. ✅
  - [x] Configure a 32 MiB heap limit and near-limit termination callback in a
    sacrificial test process. The probe verifies the process survives and the
    isolated runtime can execute a follow-up script. A 5 MiB limit aborts V8
    during `JsRuntime` initialization before its callback can run, so production
    isolates retain V8's default heap policy pending broader evidence. ✅

### 2. Editor model and public boundary

- [ ] Replace the blank winit application with a minimal gpui application.
  A gpui entity owns the real `TextBuffer`, its public revision, and buffer
  lifecycle. The window displays buffer text, runtime status, errors, and an
  independently ticking heartbeat.
- [ ] Route host requests onto gpui's foreground executor and return results by
  one-shot response. Never share `TextBuffer` through `Arc<Mutex<_>>` and never
  block the gpui thread waiting for JavaScript.
- [ ] Implement snapshot/range reads, UTF-8 boundary validation, atomic edit
  batches, revision conflicts, opaque-handle invalidation, and the two explicit
  UTF-16 adapter helpers. Keep byte/UTF-16 index construction lazy so ordinary
  UTF-8 operations do no re-encoding work.
- [ ] Implement extension-owned command registration and invocation. Pass an
  `AbortSignal`; cancellation rejects awaited host work and invalidates the
  invocation token so a response arriving later cannot mutate editor state.
- [ ] Implement buffer-change subscriptions and disposables. Fan one committed
  change out to several extension threads while preserving per-extension
  ordering and isolating thrown/rejected subscriber failures.

### 3. Integrated proof and evidence

- [ ] Load fixture extensions that register a command, read the active buffer,
  edit it after an await, and observe the resulting revisioned event. Invoke
  the command from the gpui window and show the changed text without pausing
  the heartbeat.
- [ ] Cancel an awaiting command and prove that its late completion cannot edit
  the buffer. Exercise thrown handlers, rejected promises, explicit disposal,
  buffer closure, extension initialization failure, and forced CPU termination;
  another extension and gpui must remain responsive.
- [ ] Demonstrate real parallel execution with finite CPU callbacks in two
  extension isolates. Verify callbacks within one extension never overlap.
- [ ] Benchmark cold isolate startup, built-in module initialization, idle RSS
  per isolate, host-call latency, batched edit latency, event fan-out/lag,
  UTF-8↔V8 string marshalling, UTF-16 adapter conversion, JIT warm-up, and
  large transfers. Separate process-wide V8 cost from incremental isolate cost.
- [ ] Use a finite slow-subscriber burst to record maximum queue depth and lag;
  do not implement dropping, coalescing, producer blocking, or subscriber
  eviction in this prototype.

### 4. Decision checkpoint and documentation

- [ ] Record measurements and answer whether thread-per-extension
  `deno_core` validates the public boundary and failure model well enough to
  proceed, including any evidence that changes the eventual `rusty_v8` pool.
- [ ] Record whether string marshalling is acceptable, which operations require
  batching, and whether a future byte-oriented transfer API is justified.
- [ ] Update this checklist with ✅/⚠️ findings as work lands. Update
  `roadmap.md` with the conclusion and `architecture.md` whenever runtime,
  application ownership, or message flow changes.

## Verification and acceptance

Keep automated testing narrow: unit-test boundary validation/conversion and use
one integration harness plus the gpui proof for lifecycle behavior.

- ASCII, CJK, Arabic, combining marks, emoji, and ZWJ text round-trip through
  snapshots and edits without changing UTF-8 byte ranges.
- Byte ranges that split a UTF-8 scalar, malformed edit batches, stale
  revisions, disposed registrations, and closed handles fail predictably.
- UTF-16 adapter helpers round-trip valid boundaries and reject a split
  surrogate or a non-boundary UTF-8 offset.
- Multi-edit commits are atomic to observers and produce one ordered event per
  extension with the expected before/after revisions.
- Await, cancellation, exceptions, rejection, unload, CPU interruption, and
  heap-limit probing never freeze gpui or suppress work in another extension.
- No public Rust, gpui, V8, or Deno type/name appears in the JavaScript API.
- `cargo test`, the runtime benchmark, and the interactive gpui proof complete
  on macOS. Linux/Windows portability and the known Windows editor-widget
  Unicode issue remain their existing roadmap checkpoints.

## Commit sequence

Every checklist item above is a separate, independently reviewable commit.
Never combine adjacent checklist items merely because they belong to the same
numbered section. Split an item further when it contains multiple useful
review/revert boundaries—for example, driving `JsRuntime` and integrating its
async event loop may warrant more than one commit—but do not split out changes
that would leave the tree uncompilable.

Use descriptive, non-conventional commit messages and mark the corresponding
checklist item with ✅ in the same commit that completes it. Documentation and
architecture updates required by that item belong in that item's commit rather
than in a final cleanup commit.
