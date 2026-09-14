# D017: Production V8 Isolate Pool and Scheduling

Status: in progress; Tasks 1-5 implemented, Task 5 awaiting review.

This slice replaces the prototype `deno_core` runtime with a direct
`rusty_v8` host and runs persistent extension isolates on a bounded worker
pool. There is no production extension client yet, so the implementation may
reshape the host aggressively and temporarily remove fixture behavior between
goals. The repository must still compile and its applicable tests must pass at
every review gate.

The slice is successful when extension count no longer determines OS-thread
count, an isolate can resume on different pool workers, asynchronous host work
does not occupy a worker, and the validated extension behaviors again operate
through Knot-owned protocols without a `deno_core` dependency.

## Selected design

- Target macOS for implementation and acceptance. Keep portable Rust where it
  follows naturally, but do not add cross-platform validation to this slice.
- Depend directly on a `rusty_v8` release providing `SharedIsolate` and
  `Locker`; begin with V8 152. Do not introduce a Knot-owned unsafe mobility
  wrapper.
- Use one persistent V8 isolate and context per loaded extension lifetime.
- Use a central scheduler with a bounded set of worker threads. The production
  default is `min(available_parallelism, 4)`, with explicit configuration for
  deterministic tests and benchmarks.
- Move an isolate only between turns. A worker holds its locker only while it
  enters V8, runs JavaScript or microtasks, and inspects the resulting state.
- Schedule ready extensions in FIFO order. Run at most one turn before
  returning an extension to the scheduler, while keeping JavaScript execution
  within a turn non-preemptive.
- Keep callbacks serial within an extension. While a root callback is pending,
  queue unrelated callbacks; host responses and same-extension nested command
  frames are continuations of its active logical command tree and may resume
  it.
- Represent asynchronous native operations with persistent V8 promise
  resolvers. Emitting a host request yields the worker; its response makes the
  isolate runnable and is applied during a later turn.
- Drain explicitly scheduled microtasks at turn boundaries. A turn finishes as
  completed, awaiting host work, ready for another continuation, or fatal.
- Retain explicit cancellation, forced termination, heap-limit failure, and
  lifecycle cleanup. Do not add automatic timeouts, general quotas,
  backpressure, event coalescing, or slow-consumer policy; those remain D019.
- Keep fixture-only in-memory modules. Packaging, dependency resolution, and
  production load order remain D018.
- Treat benchmarks as recorded evidence, not fixed performance gates.

## Scheduler invariants

- The number of runtime worker threads is fixed for the pool lifetime and does
  not grow with extension count. The scheduler/coordinator is one additional
  fixed thread if the implementation uses a dedicated coordinator.
- At most one worker owns a turn for an extension, and at most one locker has
  entered its isolate.
- Each extension has at most one active root callback or command tree.
  Unrelated roots preserve enqueue order.
- An extension appears at most once in the ready queue. New work changes its
  state or appends to its extension-local queue rather than duplicating it.
- Awaiting a host response retains persistent JavaScript state but owns no
  worker and no locker.
- Every queued or pending completion settles exactly once on success,
  cancellation, unload, startup failure, termination, or pool shutdown.
- Fatal failure tears down only the affected extension lifetime. Stale work
  and responses are rejected by extension and lifecycle identity.
- Isolates are entered only after V8 process initialization and are disposed
  under the locking/entry discipline required by `rusty_v8`.

## Delivery protocol

Execute each task as a separate `/goal`. Within a task, make logical commits,
update its checkboxes with ✅ as work lands, run the listed automated checks,
then stop for code review and the manual review gate. Do not begin the next
task in the same goal.

The test commands below are minimum focused checks. Each task must also run any
nearby tests affected by its actual changes. Run `cargo fmt` once at the end of
each goal that changes Rust, immediately before the final tests and commits.

## Protocol restoration ledger

The removed prototype validated behavior that later D017 tasks must restore on
Knot-owned boundaries:

- Task 3: persistent globals and explicit microtasks across turns; scoped
  exception and rejection reports without poisoning an isolate; fatal forced
  termination, heap failure, unload-before-disposal, neighbor isolation, and
  retained external UTF-16 ownership.
- Task 4: static fixture modules, invalid and private specifier rejection, the
  public `knot:editor` facade without Deno globals, and stable JavaScript error
  names for host failures.
- Task 5: lifecycle-scoped unique request identities; extension, lifecycle,
  and request validation; exact-once pending settlement; and asynchronous typed
  responses.
- Task 6: revisioned buffer snapshots and edits; serial ordered change events
  that survive listener failure; lossless finite slow-subscriber bursts with
  depth and lag measurements; contribution replacement/disposal; and shared
  external UTF-16 lifetime.
- Task 7: explicit command arguments and outcomes; same-extension nested
  command ordering; cancellation of suspended handlers; and registration
  disposal. The product bridge additionally awaited the native dispatcher's
  ordinary outcome, rejected nested product invocations as unavailable, and
  reported missing targets or unsupported operations explicitly.
- Task 8: tree registration, invalidation, generation, and recoverable callback
  failure; completion registration, revision, generation, and failure; and
  cross-extension progress.

The protocol retained during the rebuild includes opaque extension, lifecycle,
request, registration, invocation, subscription, and buffer identities; typed
operations and responses; UTF-8 byte ranges and revisioned edits; structured
command routing and outcomes; tree and completion generations; semantic
contribution tokens; and stable host-error wire names.

## Task 1: Replace the prototype runtime boundary

- ✅ Record the current protocol behaviors that later tasks must restore, then
  remove the thread-per-extension runtime implementation and implementation-
  coupled tests without weakening `host::protocol` coverage.
- ✅ Split the monolithic host into boundaries for protocol, scheduler, V8
  engine/bindings, lifecycle, and benchmarks. These may initially be skeletal;
  avoid a compatibility trait or parallel old/new runtime implementation.
- ✅ Remove the product command dispatcher's fixture runtime and wake loop until
  the pooled integration is ready. Product-native command behavior must remain
  intact.
- ✅ Replace `deno_core` and `deno_error` with a direct V8 152 dependency,
  initialize the V8 platform before creating any threads that can enter an
  isolate, and remove all Deno types and macros from the source tree.
- ✅ Keep the package, binaries, and remaining tests compiling without
  project-owned warnings.

Automated checks:

- `cargo test host::protocol`
- `cargo test app::product_commands`
- `cargo check --all-targets`
- Confirm `cargo tree -i deno_core` reports that the package is absent.

Manual review gate:

- Inspect dependency removal and the new module ownership. Confirm no runtime
  abstraction exists solely to preserve the prototype implementation and that
  the ordinary product editor still launches.

## Task 2: Implement the scheduler state machine

- ✅ Define extension lifecycle and scheduling states independently of V8
  mechanics: loading, idle, queued, running, awaiting host work, stopping, and
  terminal failure.
- ✅ Implement admission, per-extension root queues, continuation wakeups,
  ready-queue deduplication, FIFO selection, turn completion, and shutdown.
- ✅ Distinguish unrelated root work from continuations belonging to the active
  logical command tree.
- ✅ Add configurable pool sizing with the selected production default and a
  deterministic single-worker test configuration.
- ✅ Cover state transitions, ordering, exact-once completion, stale lifecycle
  rejection, and shutdown using scheduler-level tests without V8 timing.

Automated checks:

- Run the focused scheduler test module.
- `cargo check --all-targets`

Manual review gate:

- Review the state-transition table and queue invariants. Exercise the focused
  tests with one and two configured workers and inspect failure output for an
  intentionally violated test invariant.

## Task 3: Establish movable persistent V8 isolates

- ✅ Add an extension runtime capsule containing `SharedIsolate`, persistent
  context handles, extension identity, and V8-local state required across
  turns.
- ✅ Create, lock, enter, execute, leave, move, re-enter, and dispose an isolate
  through supported `rusty_v8` APIs.
- ✅ Add explicit microtask policy and scoped exception reporting with stable
  Knot-owned runtime errors.
- ✅ Attach the existing thread-safe termination path and configurable heap
  limit without exposing V8 types outside `host`.
- ✅ Prove persistent globals survive turns executed by different workers and
  that independent isolates can execute concurrently.

Automated checks:

- Run focused isolate creation, persistence, movement, exception, heap-limit,
  termination, and disposal tests.
- Run a repeated movement stress test suitable for detecting entry/locking or
  destruction-order faults.

Manual review gate:

- Inspect every locker and persistent-handle lifetime. Run the movement stress
  test repeatedly on macOS and confirm diagnostic worker identities show one
  isolate executing on more than one worker.

## Task 4: Rebuild fixture modules and the JavaScript facade

- ✅ Implement the minimum direct-V8 module pipeline: compile, instantiate,
  resolve from the static fixture module set, evaluate, and report synchronous
  or rejected-promise failures.
- ✅ Recreate the private bootstrap and public `knot:editor` facade without
  `Deno.core`, generated ops, or Deno globals.
- ✅ Install private native callbacks explicitly and expose only the semantic
  public API to extension code.
- ✅ Keep imports restricted to the embedded fixture graph and reject access to
  private bootstrap modules.
- ✅ Restore script and fixture-module probes on top of scheduler turns.

Automated checks:

- Cover successful scripts/modules, imports, persistent module state, invalid
  specifiers, missing modules, private imports, syntax errors, thrown errors,
  and rejected evaluation.
- `cargo check --all-targets`

Manual review gate:

- Review the global/API exposure from JavaScript and run representative script
  and module probes, including a deliberately rejected private import.

## Task 5: Implement yielding host requests and promise resumption

- ✅ Bind native JavaScript calls that allocate a Knot request identity, create
  and retain a V8 promise resolver, and emit a typed `HostRequest`.
- ✅ Store pending resolvers by request identity without retaining scoped V8
  handles or Rust references across turns.
- ✅ Route `HostResponse` back through the scheduler, validate extension and
  lifecycle identity, settle its resolver inside a later isolate turn, and
  drain resulting microtasks.
- ✅ Track the root promise independently from host-request promises so a
  callback completes only when its logical work settles.
- ✅ Yield workers whenever no JavaScript continuation is runnable, including
  multiple concurrent host requests from one callback.
- ✅ Settle or reject every pending promise and native completion during unload,
  failure, termination, and pool shutdown.

Automated checks:

- Cover request/response success and errors, multiple in-flight requests,
  out-of-order responses, stale and duplicate responses, unload while waiting,
  and rejection propagation.
- Prove with a single-worker pool that one extension progresses while another
  awaits an intentionally delayed host response.

Manual review gate:

- Trace one request from JavaScript through the typed inbox and back to its
  promise resolver. Run the delayed-response probe and verify the worker is
  released rather than blocked.

## Task 6: Restore buffers, edits, and contributions

- ⬜ Reimplement conversion between JavaScript values and the typed buffer,
  range, edit, contribution, response, and error structures.
- ⬜ Restore active-buffer lookup, revisioned snapshots, batched edits,
  subscriptions, unsubscription, and contribution replacement/disposal.
- ⬜ Restore immutable external UTF-16 strings with ownership retained until V8
  releases them; keep the UTF-8 core and scoped byte/UTF-16 adapters unchanged.
- ⬜ Dispatch ordered buffer-change callbacks as extension root work and retain
  the existing queue measurements without adding D019 policy.
- ⬜ Revalidate cancellation immediately before foreground mutation and reject
  late or stale edits.

Automated checks:

- Restore and adapt the previous buffer API, revision-conflict, range, Unicode,
  external-string lifetime, subscription ordering, slow-consumer burst, and
  teardown tests.
- Run the applicable model/bridge integration tests.

Manual review gate:

- Review conversion and external-string ownership carefully. Run a fixture
  that reads and edits Unicode text, observes ordered changes, and attempts a
  stale revisioned edit.

## Task 7: Restore commands and nested composition

- ⬜ Restore command registration, unregistration, invocation, structured
  outcomes, argument validation, and handler error classification.
- ⬜ Model an invoked extension handler as a root command frame and preserve its
  captured active-buffer target and invocation ancestry across yields.
- ⬜ Admit awaited same-extension command invocation as a nested continuation
  of the active tree rather than an unrelated queued root.
- ⬜ Keep one unfinished child per parent, reject ancestry cycles, and ensure
  unrelated callbacks wait until the active root tree settles.
- ⬜ Restore explicit command cancellation and fatal forced interruption with
  exact lifecycle cleanup.

Automated checks:

- Restore and adapt command registration, routing, arguments, captured target,
  awaited composition, self-queue deadlock, ancestry, cancellation, thrown
  handler, runaway CPU, and cross-extension isolation tests.
- Run the product command tests affected by the host boundary.

Manual review gate:

- Run fixtures for native-to-extension invocation, nested same-extension
  composition, cancellation while awaiting a host response, and forced
  interruption. Inspect the scheduler trace for correct root/continuation
  ordering.

## Task 8: Restore semantic provider callbacks

- ⬜ Restore tree-provider registration, invalidation, unregistration, child
  requests, generation propagation, and recoverable provider errors.
- ⬜ Restore completion-provider registration, unregistration, requests,
  revision/generation propagation, and recoverable provider errors.
- ⬜ Queue provider calls as unrelated roots and preserve serial callback
  semantics when commands or other providers are active.
- ⬜ Ensure dropped native requests, provider removal, extension unload, and
  stale results settle cleanly without retaining isolate state.

Automated checks:

- Restore and adapt tree and completion provider tests, including asynchronous
  results, failure, stale generation, removal, queued ordering, and teardown.
- Run the applicable native tree and completion integration tests.

Manual review gate:

- Exercise tree and completion fixtures while another callback is awaiting
  host work. Verify callbacks queue in extension order and another extension
  continues independently.

## Task 9: Complete lifecycle and pool integration

- ⬜ Expose final non-V8 host controls, request inboxes, completions, watchdogs,
  and shutdown ownership shaped for pooled runtimes rather than OS threads.
- ⬜ Integrate one application-owned pool into the product host without periodic
  wake loops or one thread owner per extension.
- ⬜ Make startup failure, ordinary unload, cancellation, isolate termination,
  heap-limit failure, worker panic, and whole-pool shutdown converge on one
  exact-once teardown path.
- ⬜ Remove all obsolete prototype controls, thread handles, comments, tests,
  and naming.
- ⬜ Add observability sufficient to report worker count, extension states,
  queue depth, turn counts, movements, and enqueue-to-start lag in tests and
  diagnostics without defining D019 policy.

Automated checks:

- Cover product host creation/destruction, multiple extension lifetimes,
  reload identity, pool shutdown, worker failure containment, stale responses,
  and resource cleanup.
- Assert that worker count remains constant as extension count grows.
- Run the full host, app command, tree, and completion test groups.

Manual review gate:

- Launch and close the product repeatedly while running multiple fixture
  extensions. Confirm process thread count is bounded and no runtime wake loop,
  extension-named thread, hang, or teardown warning remains.

## Task 10: Validate scheduling and resource behavior

- ⬜ Rework `v8-bench` around the production pool and preserve comparable
  startup, host-call, edit, transfer, fan-out, and slow-consumer measurements
  where they still describe the new architecture.
- ⬜ Add evidence for bounded threads, queue wait, parallel turns, isolate
  movement, async yielding, fairness between ready extensions, shutdown, and
  per-isolate idle memory.
- ⬜ Stress more extensions than workers with persistent state, delayed host
  responses, failures, and repeated load/unload cycles.
- ⬜ Audit that no scheduler behavior silently implements quotas, timeouts,
  dropping, coalescing, or backpressure belonging to D019.
- ⬜ Run the complete regression suite and resolve flaky timing assumptions by
  adding deterministic synchronization rather than widening sleeps.

Automated checks:

- Run all tests.
- Run the release benchmark with a small review sample and the scheduler stress
  scenario with deterministic assertions.

Manual review gate:

- Inspect the release evidence on the target macOS machine. Confirm the pool
  never exceeds its configured worker count, extensions progress in parallel
  up to that bound, a waiting extension releases capacity, and one fatal
  extension does not stop the others.

## Task 11: Close the slice

- ⬜ Audit the implementation against every success criterion and scheduler
  invariant in this plan; fill any remaining model, lifecycle, command,
  provider, transport, or integration coverage gaps.
- ⬜ Update `docs/architecture.md` and
  `docs/architecture/extension-host.md` with the implemented ownership,
  dependency direction, scheduler states, runtime flows, and behavioral
  constraints.
- ⬜ Update `docs/decisions.md` with the validated direct-V8, pool-sizing,
  mobility, scheduling, and async-yield rationale.
- ⬜ Keep D018 and D019 deferred, refining their descriptions only if this work
  exposes a concrete new boundary.
- ⬜ Record benchmark evidence and the completed result in this plan, mark it
  completed, and remove or archive superseded fixture evidence only when it is
  no longer useful historical context.
- ⬜ Run `cargo fmt`, the full test suite, and the final release diagnostics.

Manual review gate:

- Review the complete diff and documentation against the selected design.
  Perform the multi-extension macOS stress run and approve the slice before
  marking this plan completed.

## Explicit exclusions

- Extension packaging, installation, dependency resolution, production module
  discovery, and load order (D018).
- Configurable quotas, automatic deadlines, backpressure, event dropping or
  coalescing, and slow-consumer enforcement (D019).
- Command schemas, aliases, macros, repetition, and concurrent root task graphs
  (D020).
- A production extension manager or user-facing extension UI. Fixtures and
  diagnostics are sufficient clients for this infrastructure slice.
- Linux and Windows acceptance work.
