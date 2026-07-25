# Step 7 — V8 scripting runtime

Status: complete. The prototype validates the V8 extension boundary and
failure model well enough to proceed to the extension view experiment.

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
  onDidChange(
    listener: (event: BufferChangeEvent) => void | Promise<void>,
  ): Promise<Disposable>;
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
  ): Promise<Disposable>;
};
```

`CommandContext.buffer` is the prototype's initial text-editor surface, not a
claim that every view has a buffer. Step 11 generalizes invocation to capture
the focused view target and derive an optional associated `TextBuffer`; native
view identities remain opaque to JavaScript.

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

- [x] Add `deno_core` and Tokio, initialize the V8 platform on the process's
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
    thread. The fixture-script proof retains isolate state across executions. ✅
  - [x] Give each extension thread a current-thread Tokio driver for V8 and
    `!Send` Deno futures. `V8Host` retains the shared multi-thread Tokio
    runtime for `Send` native work. ✅
  - [x] Cover the initial runtime shell with focused tests for V8/Tokio
    initialization, isolate persistence, JavaScript failure isolation, and
    typed request/response validation. ✅
- [x] Drive each `JsRuntime` from its owning thread while dispatching `Send`
  native work onto the shared Tokio runtime. Use typed Knot-owned
  request/response messages; keep all `deno_core` types inside `host`.
  - [x] Prove an internal async op can await `Send` native work on `V8Host`'s
    shared Tokio runtime while V8 remains driven by its extension thread. ✅
  - [x] Bridge the fixture `ActiveBuffer` op through typed host messages and
    resolve its JavaScript promise from the matching typed response. ✅
  - [x] Make fixture execution completion and host-request receipt awaitable,
    allowing the foreground host to pump and respond to requests while module
    evaluation is pending without exposing Tokio channel types. ✅
- [x] Implement the static module loader, private bootstrap bindings,
  `knot:editor` facade, source-aware exception/rejection reporting, and a test
  proving extension code cannot access `Deno.core` or import private modules. ✅
  - [x] Add a static in-memory loader and evaluate fixture ES modules on their
    owning extension thread. It accepts absolute URL specifiers only and has
    no filesystem or package resolution. ✅
  - [x] Evaluate a private `knot:bootstrap` module before extension code. It
    captures its native op binding in module-local scope and rejects direct
    extension imports; the `knot:editor` facade is its sole importer. ✅
  - [x] Add the initial `knot:editor` facade for the fixture active-buffer
    operation, then remove Deno's private binding global before extension code
    runs. ✅
  - [x] Return JavaScript exception and rejection reports with their source
    location while keeping the extension runtime usable after failure. ✅
- [x] Give each extension a lifecycle token owning pending promises,
  termination state, resource limits, and idempotent teardown after normal
  shutdown or failed thread startup. It rejects late responses after unload;
  command, subscription, and callback registries will join it when those APIs
  land. ✅
- [x] Expose V8's thread-safe isolate handle to a watchdog. A synchronous CPU
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
  - [x] Apply a 32 MiB initial heap limit and near-limit termination callback to
    every extension isolate. A sacrificial child-process test drives the real
    extension runtime to exhaustion, verifies fatal teardown and a stable
    `MemoryLimitExceeded` result, then proves a neighboring isolate remains
    usable. `JsRuntime` construction enters the extension's Tokio context so
    V8 can schedule delayed GC work without aborting the process. ✅

### 2. Editor model and public boundary

- [x] Replace the blank winit application with a minimal gpui application. ✅
  A gpui entity owns the real `TextBuffer`, its public revision, and buffer
  lifecycle. The window displays buffer text, runtime status, errors, and an
  independently ticking heartbeat.
  - [x] Promote the useful Step 3 gpui shell and editor implementation onto
    the default `knot` application path instead of replacing it with a second
    toy view. Preserve its text shaping, clipping, scrolling, cursor,
    selection, keyboard, mouse, diagnostic-overlay, and IME code. ✅
  - [x] Separate document and view ownership. Add a gpui `BufferModel` entity
    which owns `core::TextBuffer`, a public `u64` revision starting at zero,
    and its open/closed lifecycle. `EditorView` retains only presentation
    state and a handle to the model. ✅
  - [x] Add an editor-owned buffer registry with monotonically allocated,
    never-reused `BufferHandle`s, weak references to `BufferModel` entities,
    and one optional active handle. A missing registry entry or a dead/closed
    entity means `BufferClosed`; closing the active buffer also clears the
    active handle. ✅
  - [x] Treat `TextBuffer::edit_seq()` as the private primitive-edit-log cursor
    used by annotations. Do not expose it as the public revision. Increment
    the public revision once per non-empty editor-visible atomic commit,
    regardless of how many primitive `TextBuffer` edits the commit produces. ✅
  - [x] Make the editor's line/segment storage a derived rendering projection,
    never an independently mutable document. Local edits mutate `BufferModel`,
    notify observers, and refresh the projection. Preserve fixture styling at
    load and retain the spike's current behavior of falling back to default
    styling after edited text is rebuilt. ✅
  - [x] Load `bench/fixtures/rust_sample.kfx` at startup, construct the real
    `TextBuffer` from its text, and open it as the active editor buffer. Keep
    the existing Step 3 fixture-selection argument only if doing so does not
    obscure this default proof. ✅
  - [x] Count each current local replacement, including an IME preedit
    replacement, as one prototype commit. Deferring preedit outside the
    authoritative buffer is a later editor-semantics refinement, not part of
    the V8 boundary proof. ✅
  - [x] Render the existing editor plus a compact status area containing the
    active buffer revision, runtime state, and latest runtime error. Run the
    heartbeat from a gpui foreground timer, store its counter separately from
    runtime activity, and repaint it periodically so host responsiveness is
    directly visible. ✅
  - [x] Cover only the model invariants here: handle allocation/invalidation,
    one revision increment per local commit, and projection refresh from the
    authoritative `TextBuffer`. Update `architecture.md` as this commit makes
    gpui and `BufferModel` the real application/model ownership path. ✅
- [x] Separate extension runtime endpoint ownership so request reception,
  clonable response/control access, and thread joining can move independently.
  The gpui bridge owns the unique request inbox; it must not borrow an entity
  across an `.await`. ✅
- [x] Route host requests onto gpui's foreground executor and return results by
  one-shot response. Never share `TextBuffer` through `Arc<Mutex<_>>` and never
  block the gpui thread waiting for JavaScript. ✅
  - [x] Keep V8 initialization on the process parent thread, then transfer only
    Knot-owned runtime controls and messages into application state. No gpui
    entity, `TextBuffer`, or V8/Deno value crosses a thread boundary. ✅
  - [x] Spawn a long-lived local task on gpui's foreground executor. It awaits
    the next typed `HostRequest`, updates the editor registry/model
    synchronously, and completes that request through its existing one-shot
    response path before awaiting the next request. ✅
  - [x] Dispatch `ActiveBuffer` first: return the active registry handle or
    `None`, and prove that the handle resolves to the same real `BufferModel`
    displayed by `EditorView`. ✅
  - [x] Add a focused integration test using gpui's test context: issue an
    `ActiveBuffer` request from a real extension runtime, let the foreground
    executor dispatch it, and verify the opaque handle resolves to the
    displayed model. ✅
- [x] Make runtime teardown and bridge failure handling non-blocking and
  visible in the gpui shell. ✅
  - [x] Keep runtime shutdown and OS-thread joining off the gpui foreground
    executor. Dropping a view or closing the window may request shutdown on
    the foreground thread, but waiting for extension teardown happens on
    background work. ✅
  - [x] Surface bridge/runtime state and failures in the status area rather
    than logging them only to stderr. A closed bridge rejects or disposes
    pending work without freezing the application. ✅
  - [x] Verify that the heartbeat progresses while the bridge is active and
    after it closes. Update `architecture.md` with the real request flow. ✅
- [x] Implement snapshot/range reads, UTF-8 boundary validation, atomic edit
  batches, revision conflicts, opaque-handle invalidation, and the two explicit
  UTF-16 adapter helpers. Keep byte/UTF-16 index construction lazy so ordinary
  UTF-8 operations do no re-encoding work. ✅
  - [x] Put buffer semantics in `BufferModel`/editor-host code, leaving
    `host::protocol` as transport data and V8 ops as marshalling only. Add
    checked helpers around `TextBuffer` because its current UTF-8 preconditions
    are assertions rather than recoverable host errors. ✅
  - [x] Resolve a handle immediately before every operation. Reject missing,
    closed, or dead entities with `BufferClosed`; never let a stale handle
    resolve to a subsequently opened buffer. ✅
  - [x] Implement `snapshot(None)` as the complete buffer and
    `snapshot(Some(range))` as the requested half-open UTF-8 byte range.
    Validate ordering, bounds, and both scalar boundaries before calling
    `TextBuffer::read_range`; return the exact requested range and current
    public revision. ✅
  - [x] Validate an edit batch completely before mutation: matching revision,
    ordered and non-overlapping pre-commit ranges, in-bounds offsets, UTF-8
    scalar boundaries, and representable sizes. Stale revisions return
    `RevisionConflict`; structural or boundary failures return stable request
    errors without partial mutation. ✅
  - [x] Detect a semantically empty batch before mutation, including an empty
    list and replacements whose text already equals their range. Return the
    current revision without touching `TextBuffer`, incrementing revision, or
    notifying observers. ✅
  - [x] Apply a valid batch in reverse range order so every range remains in
    pre-commit coordinates. Publish it as one commit: increment revision once,
    refresh/notify views once, and retain the original ordered edits for the
    later change event. ✅
  - [x] Extend the private bootstrap and `knot:editor` facade with cached
    `TextBuffer` proxies keyed by opaque private handles. JavaScript receives
    no numeric handle, Rust type, gpui entity, op name, or Deno object. ✅
  - [x] Materialize `TextSnapshot` as an immutable public object. Define both
    UTF-16 adapters relative to `snapshot.text` (byte offset zero is the start
    of the snapshot); callers use `snapshot.range.startByteOffset` when they
    need a buffer-absolute position. ✅
  - [x] Build a snapshot's byte/UTF-16 boundary table only on its first adapter
    call and cache it privately. Reject a split surrogate and a non-UTF-8
    boundary; ordinary snapshot reads and edits must not construct this table. ✅
  - [x] Add narrow Unicode and transaction tests covering ASCII, CJK, Arabic,
    combining marks, emoji/ZWJ, partial snapshots, stale revisions, invalid
    boundaries, overlap/order failures, semantic no-ops, and one-revision
    multi-edit commits. Add a fixture module that reads and edits the active
    Rust buffer so the window visibly proves the end-to-end path. ✅
- [x] Implement extension-owned command registration and invocation. Pass an
  `AbortSignal`; cancellation rejects awaited host work and invalidates the
  invocation token so a response arriving later cannot mutate editor state.
  - [x] Add Knot-owned command, registration, and invocation identities to the
    typed protocol. Keep the editor registry authoritative for command names;
    each registration records its owning extension and lifecycle token. ✅
  - [x] Add `commands.register` to the facade with an extension-local handler
    registry and idempotent disposable. Extension unload disposes all of its
    registrations; an explicit dispose prevents later invocation. ✅
  - [x] Invoke handlers only on their owning extension thread and serialize
    callbacks within that extension. Construct `CommandContext` with the
    active buffer proxy and a fresh `AbortSignal`, then propagate synchronous
    returns, promises, throws, and rejections to the host as typed outcomes. ✅
  - [x] Give every invocation a cancellation token checked both before editor
    dispatch and immediately before any mutation. Aborting rejects awaited
    host operations and makes late native or JavaScript completions unable to
    edit even if their underlying work finishes. ✅
  - [x] Wire one visible gpui action/button to invoke the fixture command that
    awaits, snapshots, and edits the Rust buffer. Report its running,
    completed, cancelled, or failed state in the window. Add focused tests for
    disposal, unload, throw/rejection, and the late-response mutation guard. ✅
- [x] Implement buffer-change subscriptions and disposables. Fan one committed
  change out to several extension threads while preserving per-extension
  ordering and isolating thrown/rejected subscriber failures.
  - [x] Add subscription identities and lifecycle-owned registries without
    exposing transport concepts publicly. `onDidChange` returns an idempotent
    disposable; buffer closure and extension unload remove all affected
    subscriptions.
  - [x] After each non-empty commit, construct exactly one `BufferChangeEvent`
    containing the cached buffer proxy, before/after revisions, and the
    original ordered edits in pre-commit coordinates. Local and extension
    commits use the same publication path.
  - [x] Fan the immutable Knot-owned event payload to every subscribed
    extension. Queue callbacks on each extension's runtime command stream so
    callbacks for one extension execute serially and in commit order while
    different extension threads remain independent.
  - [x] Await each listener result before advancing that extension's callback
    queue. Record thrown/rejected listener failures against that extension and
    continue later callbacks and other extensions; do not fail or roll back the
    already committed edit.
  - [x] Track queue depth and enqueue-to-start lag for later Phase 3 evidence,
    but add no dropping, coalescing, producer blocking, or eviction policy. ✅
  - [x] Test one event for a multi-edit commit, ordering across finite bursts,
    several extensions receiving the same commit, disposal and closure, and a
    failing subscriber not suppressing subsequent delivery. Update
    `architecture.md` with the commit/event fan-out flow. ✅

### 3. Integrated proof and evidence

- [x] Load fixture extensions that register a command, read the active buffer,
  edit it after an await, and observe the resulting revisioned event. Invoke
  the command from the gpui window and show the changed text without pausing
  the heartbeat. ✅
- [x] Cancel an awaiting command and prove that its late completion cannot edit
  the buffer. Exercise thrown handlers, rejected promises, explicit disposal,
  buffer closure, extension initialization failure, and forced CPU termination;
  another extension and gpui must remain responsive. ✅
  - [x] Dispatch a cancelled invocation's late edit request through the gpui
    host boundary and verify that it returns `Cancelled` without mutating the
    displayed buffer. ✅
  - [x] Terminate one extension during synchronous CPU execution, then verify a
    neighboring extension and the gpui heartbeat remain responsive. ✅
  - [x] Exercise a failing startup module, thrown and rejected handlers,
    explicit command disposal, cancellation, buffer closure, and forced CPU
    termination together through one gpui harness. The surviving extension and
    foreground heartbeat continue afterward. ✅
- [x] Demonstrate real parallel execution with finite CPU callbacks in two
  extension isolates. Verify callbacks within one extension never overlap. ✅
  - [x] Dispatch finite CPU-bound buffer-change callbacks to two isolates and
    use elapsed time to prove cross-isolate overlap; a second callback in one
    isolate proves its queue remains serial. ✅
- [x] Benchmark cold isolate startup, built-in module initialization, idle RSS
  per isolate, host-call latency, batched edit latency, event fan-out/lag,
  UTF-8↔V8 string marshalling, UTF-16 adapter conversion, JIT warm-up, and
  large transfers. Separate process-wide V8 cost from incremental isolate cost.
  ✅
- [x] Use a finite slow-subscriber burst to record maximum queue depth and lag;
  do not implement dropping, coalescing, producer blocking, or subscriber
  eviction in this prototype. ✅
  - [x] An eight-event burst with 40 ms CPU-bound listeners records the
    control's maximum depth and enqueue-to-start lag while asserting ordered,
    lossless delivery. ✅

### Benchmark evidence

`cargo run --release --bin v8-bench -- --samples 12` runs the real extension
thread, public JavaScript facade, and typed request/response channel. It uses
a fixture responder rather than constructing gpui, so the numbers exclude
foreground model work. Values are microseconds (p50/p95) unless noted.

Measured on 2026-07-23: Apple M1 Pro, macOS 26.5.2 (arm64), Rust 1.97.1,
`deno_core` 0.408.0 / V8 crate 149.4.0.

| Workload | p50 | p95 | Notes |
| --- | ---: | ---: | --- |
| Process-wide V8 initialization | 8,915 | 8,915 | fresh child process, excludes process launch |
| Incremental isolate startup | 4,175 | 4,626 | includes extension thread |
| Built-in module initialization | 4,004 | 4,268 | `knot:editor` facade |
| Idle RSS per isolate | 2,324 KiB | — | approximate macOS process sample |
| Active-buffer host call | 194 | 294 | facade, channel, typed op |
| Batched edit (10 edits) | 332 | 433 | one `applyEdits` crossing |
| String marshal, ASCII 1 KiB | 361 | 400 | Rust UTF-8 to V8 string |
| UTF-16 cache build, ASCII 1 KiB | 1 | 2 | retains 2 KiB |
| External UTF-16, cold ASCII 1 KiB | 294 | 384 | build plus external string |
| External UTF-16, cached ASCII 1 KiB | 279 | 355 | shared backing |
| String marshal, Unicode 100 KiB | 800 | 886 | Rust UTF-8 to V8 string |
| UTF-16 cache build, Unicode 100 KiB | 70 | 88 | retains 100 KiB for `é` fixture |
| External UTF-16, cold Unicode 100 KiB | 375 | 527 | build plus external string |
| External UTF-16, cached Unicode 100 KiB | 296 | 344 | shared backing |
| String marshal, ASCII 10 MiB | 2,559 | 2,955 | V8 can use one-byte storage |
| UTF-16 cache build, ASCII 10 MiB | 9,963 | 10,389 | retains 20 MiB |
| External UTF-16, cold ASCII 10 MiB | 10,747 | 11,339 | build plus external string |
| External UTF-16, cached ASCII 10 MiB | 391 | 484 | shared backing |
| String marshal, Unicode 10 MiB | 36,045 | 36,932 | Rust UTF-8 to V8 string |
| UTF-16 cache build, Unicode 10 MiB | 10,258 | 10,860 | retains 10 MiB for emoji fixture |
| External UTF-16, cold Unicode 10 MiB | 10,589 | 11,351 | build plus external string |
| External UTF-16, cached Unicode 10 MiB | 321 | 384 | shared backing |
| UTF-16 adapter, 1 KiB | 561 | 676 | includes lazy boundary table |
| UTF-16 adapter, 100 KiB | 4,647 | 5,401 | includes lazy boundary table |
| JIT warm-up, 32 host calls | 1,246 | 342 | first loop / second loop in one isolate |
| Event fan-out, 2 isolates × 8 events | 229 | 289 | serial delivery within each isolate |
| Event queue | depth 8 | lag 192 | max depth / max lag in microseconds |

The 10 MiB UTF-16 adapter case intentionally is not run: building its JavaScript
boundary table exceeds this prototype's 32 MiB per-isolate heap cap. The raw
10 MiB string transfer succeeds. This is evidence to keep snapshots scoped and
to avoid triggering the adapter on whole-document transfers, not evidence for a
byte-oriented public API yet.

The boundary-cache spike keeps the core UTF-8-native. `BufferModel` retains at
most one immutable UTF-16 allocation keyed by `(revision, range)` and invalidates
it on edit. Extension isolates create external V8 strings over independent
references to that shared allocation, so repeated snapshots do not transcode
or copy the text per isolate.

The cache is decisive for repeated large snapshots and for the tested Unicode
payload even when cold. It is not evidence for an always-UTF-16 core: cold
10 MiB ASCII is 10.7 ms rather than 2.6 ms, and its retained representation is
twice as large. Keep the UTF-8 core and bounded boundary cache for the
prototype. If retained beyond the prototype, use external one-byte backing for
ASCII snapshots instead of caching them as UTF-16.

### 4. Decision checkpoint and documentation

- [x] Record measurements and answer whether thread-per-extension
  `deno_core` validates the public boundary and failure model well enough to
  proceed, including any evidence that changes the eventual `rusty_v8` pool. ✅
  The prototype validates the boundary well enough to proceed. Typed messages,
  foreground ownership, revisioned batched edits, cancellation, serial
  per-extension callbacks, cross-extension parallelism, and fatal per-isolate
  interruption compose without exposing Deno or V8 details. Keep `deno_core`
  behind `host` while iterating on the public API.

  Thread-per-extension is not the production topology. Incremental startup is
  about 4.2 ms and an idle isolate adds about 2.3 MiB RSS before accounting for
  one permanently reserved OS thread. This does not change the target bounded
  movable-isolate pool on `rusty_v8`; it strengthens the reason to move before
  loading extensions at scale. The Knot-owned protocol and lifecycle semantics
  should survive that change. Forced interruption remains fatal to one
  extension, with no automatic restart, and slow-consumer policy remains open.
- [x] Record whether string marshalling is acceptable, which operations require
  batching, and whether a future byte-oriented transfer API is justified. ✅
  Small scoped strings are acceptable and edits remain batched. Large repeated
  snapshots use the bounded external-string cache; current evidence does not
  justify a public byte-oriented API.
- [x] Update this checklist with ✅/⚠️ findings as work lands. Update
  `roadmap.md` with the conclusion and `architecture.md` whenever runtime,
  application ownership, or message flow changes. ✅ No architectural flow
  changed in the final proof, so `architecture.md` requires no further update.

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
