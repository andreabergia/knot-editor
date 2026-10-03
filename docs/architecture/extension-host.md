# Extension host

Part of the [architecture](../architecture.md). Design rationale is recorded in
[decisions](../decisions.md); validation evidence lives in
[D017](../archive/plans/d017-v8-isolate-pool-plan.md).

## Boundaries

`host` owns six boundaries:

- `protocol` contains Knot-owned transport identities, requests, responses,
  contribution data, and errors. It depends on neither gpui nor concrete core
  models.
- `scheduler` owns bounded worker scheduling independently of V8 mechanics.
- `engine` owns direct `v8` initialization, isolates, contexts, bindings, and
  all V8 types.
- `lifecycle` defines extension identity, scheduler-visible state, and failure
  data. The engine owns exact-once teardown.
- `pool` exposes the V8-free application control, event-inbox, completion,
  watchdog, diagnostics, and shutdown surface.
- `module_graph` validates generated file URLs, one or more entry modules, and
  captured directory-local sources without V8 or filesystem access.

A package load binds its immutable source graph to one isolate before scheduling
entry as a root turn. Static imports may resolve only `knot:editor` or relative
URLs that remain within the package root and name a captured source. Dynamic
imports reject. Compilation and runtime failures carry generated source URL,
line, and column as Knot-owned data. Package entry failure remains a failed
startup attempt for the application to roll back through lifecycle unload.

The scheduler owns a V8-independent state machine and a fixed worker pool of
`min(available_parallelism, 4)` threads by default. Tests and diagnostics may
set the worker count explicitly.
Each loaded lifecycle has one authoritative state, one FIFO root queue, and at
most one active logical command tree. A cloneable handle admits lifecycles,
queues roots, wakes continuations, and requests stop; the pool owner shuts down
and joins its workers. The engine composes this scheduler with one persistent
runtime capsule per loaded lifecycle. One application-global
`ProductExtensionHost` owns the pool, foreground protocol bridges, and an
awaitable event-inbox task. The `--fixture` path loads two static diagnostic
lifecycles through the same package graph API and product composition.
Filesystem discovery runs on gpui's background executor. The application
plans dependencies, admits each lifecycle on the foreground, and awaits entry
completion before loading its dependents. A failed or cancelled entry unloads
its isolate and clears foreground command and provider registrations before
another package starts. The product publishes each startup outcome to the
ordered report and stderr as it becomes known, including invalid packages and
runtime failures. Known outcomes remain visible while a later entry awaits
completion.

V8 process initialization is idempotent and owned by `engine`. It must happen
before creating scheduler workers, because every thread that may lock a shared
isolate must be created after initialization. The host composition point must
initialize the engine before constructing a pool whose executor enters V8.

Each runtime capsule owns a `SharedIsolate`, persistent context, lifecycle
identity, explicit microtask state, heap-limit state, and only thread-safe
embedder data. A worker holds the capsule's locker only for one JavaScript turn:
it enters the persistent context, compiles and executes the script, drains the
explicit microtask checkpoint, converts any exception or unhandled rejection
to a Knot-owned report, and leaves all scopes before releasing the locker.
Synchronous exceptions and unhandled rejections fail only that turn. Forced
termination and heap-limit termination are fatal to the lifecycle.

Package entry modules run as scheduler root turns. Each capsule owns an
immutable in-memory source graph and persistent compiled-module cache. Static
imports use URL resolution and can reach only sources already embedded in that
graph; bare, missing, and private-bootstrap imports fail resolution. The engine
eagerly evaluates a private `knot:bootstrap` module and the public
`knot:editor` facade when it creates the capsule. Bootstrap captures the native
request callback and removes its temporary global before extension work can
run, leaving only the semantic `editor`, `commands`, and `workbench` exports.
The facade's buffer API crosses a pool-owned typed request inbox. Its native
callback allocates a lifecycle-scoped request identity, retains only a
persistent V8 promise resolver, and returns the promise without waiting on the
worker. Cached JavaScript buffer proxies expose revisioned snapshots, batched
edits, change subscriptions, and one replaceable contribution set. The engine
converts these values directly at the V8 boundary; immutable UTF-16 snapshot
storage is externalized with one `Arc` reference retained until V8 releases it.

Personal config uses one capsule with optional pre-init and post-init graph
entries. Loading the graph evaluates neither entry; the application schedules
each entry as a separate awaited root turn. Compiled modules, JavaScript state,
and registrations remain in the capsule between turns. Pre-init cannot import
post-init, including through another local module, so the later entry cannot
run during the earlier phase. Either entry may be absent. Unload uses the same
exact-once lifecycle finalizer as installed extensions.
The application reserves a distinct config lifecycle identity. It captures
sources on the background executor before loading that lifecycle, awaits
pre-init, then the installed-package dependency plan, then post-init. A config
root may be a symlink to a directory; source containment uses the resolved root.
An invalid or dangling root symlink is a discovery error. A capture or
validation error associated with post-init is retained until that
phase; pre-init errors stop startup before extension loading. A config
failure unloads the config and every installed lifecycle admitted during that
launch and removes their foreground registrations. Installed-package failures
retain their independent report outcomes and do not interrupt post-init.

Command handlers remain persistent JavaScript functions keyed by opaque
foreground-issued registrations. Native-to-extension invocation enters as
scheduler root work with explicit JSON-compatible arguments and its captured
buffer. Handler outcomes cross back as Knot-owned structured results rather
than V8 values.

Tree and completion providers remain persistent JavaScript objects keyed by
opaque foreground-issued registrations. Native child and completion requests
enter as unrelated scheduler roots and return only typed semantic data with
their registration, generation, and revision identities preserved. Callback
throws and invalid results become recoverable per-request provider failures;
they do not fail the extension lifecycle.

The engine retains the root script or module promise independently from its
host-request promises. A pending root with no runnable JavaScript yields its
worker in `AwaitingHostWork`. A validated response is queued against that
root, settles the persistent resolver inside a later locked isolate turn, and
drains microtasks before the root is inspected again. Multiple requests may be
pending and may settle out of order; unrelated roots remain queued until the
active root and its detached native requests have all settled.

Unload first marks the scheduler lifecycle stopping and terminates any running
JavaScript through a thread-safe control handle. Startup rollback, unload,
fatal termination, heap failure, caught worker panic, and pool shutdown all
remove and dispose a capsule through one exact-once lifecycle finalizer. The
finalizer emits a terminal lifecycle event so the foreground removes command,
buffer, tree, completion, subscription, and contribution ownership once.
Disposal waits for the running locker to leave, then releases persistent
handles and callback state while locked before dropping the shared isolate.
Immutable external UTF-16 strings transfer one `Arc` reference to V8 and
release it on garbage collection or isolate disposal, including when disposal
happens on a pool worker.

```text
extension JavaScript
        |
host engine + bounded scheduler
        |
Knot-owned typed protocol
        |
application-owned gpui foreground bridge
        |
application registries and models
```

## Boundary contracts

Extension requests cross the application boundary only as Knot-owned semantic
data and opaque extension, lifecycle, request, registration, invocation,
subscription, and buffer identities. The foreground owner revalidates captured
identity, revision, generation, and lifecycle before mutation.

Buffer ranges and edits remain UTF-8 byte based. Snapshot storage supplies an
immutable external UTF-16 representation to V8 without changing the UTF-8
core.

## Scheduler flow

A newly admitted lifecycle starts in `Loading`. Successful loading moves it to
`Idle`, or to `Queued` when roots arrived during loading. `Queued` is also the
ready-queue deduplication bit. A worker takes one FIFO turn and changes the
lifecycle to `Running`; its result completes the active root, tail-queues one
continuation turn, yields in `AwaitingHostWork`, or fails that lifecycle.
Stop changes the lifecycle to `Stopping`; normal cleanup removes it, while a
fatal outcome records `Failed`. The pool admits another lifecycle
independently of a failed one.

Scheduler diagnostics are read-only evidence: they report configured worker
count, lifecycle states, current and maximum queue depths, turn counts, worker
movements, and maximum enqueue-to-start lag. They do not trigger quotas,
timeouts, dropping, coalescing, or backpressure.

Unrelated roots remain behind the active command tree. A continuation must
name that tree and may wake an awaiting lifecycle; a response that races with
the running turn is retained and queued when the turn yields. Stop and shutdown
cancel queued work immediately and settle a running root when its outstanding
turn returns. Root completion senders are consumed on settlement so accepted
root work completes exactly once. Extension and lifecycle identity plus
per-turn identity reject stale work and late or duplicate turn results.
Response admission additionally validates extension, lifecycle, and request
identity and reserves the request before scheduling, so duplicate responses
cannot enqueue twice. Unload, fatal failure, forced termination, and pool
shutdown reject retained resolvers under the isolate locker and settle the
native root completion exactly once before disposal.
The pool-issued termination handle also queues a continuation for an awaiting
root, so forced termination does not depend on a later host response to wake a
parked lifecycle.

Buffer-change notifications enter the scheduler as unrelated FIFO roots. Each
callback may yield for host work without holding a worker, and a rejected
listener completes only that callback so later notifications still run. The
runtime records maximum queued callback depth and enqueue-to-start lag as
evidence; it does not cap, drop, coalesce, or delay notifications as policy.

Semantic provider callbacks use the same unrelated-root queue. A provider call
therefore waits behind an active command, buffer listener, or provider call,
while an asynchronous provider that awaits host work releases its worker.
Dropping the native receiver cancels a queued call before entry or wakes an
active call to release its persistent promise and pending host resolvers.
Unregistration removes the JavaScript provider synchronously, and unload or
pool shutdown settles outstanding native callback completions during the
ordinary lifecycle teardown.

The foreground command bridge owns the lifecycle-scoped catalog and one serial
invocation tree. It captures the active buffer on the root, inherits it through
children, returns same-lifecycle children to JavaScript as inline
continuations, and defers cross-lifecycle or native outcomes until their work
settles. The product catalog mirrors lifecycle registrations for palette and
keybinding discovery, while the application bridge retains the captured native
target for the whole root tree. Each parent has at most one unfinished child,
and ancestry cycles are rejected as unavailable. Cancellation marks the tree,
wakes a suspended isolate turn, aborts its JavaScript signal, rejects its
pending host requests, and rejects late responses. Forced V8 interruption
remains fatal only to the affected lifecycle.

The foreground buffer bridge owns active-buffer lookup, lifecycle-scoped
subscriptions, revision validation, model mutation, contribution ownership,
and change fan-out. It revalidates cancellation inside the foreground update
immediately before mutation. Model observers route committed changes back to
the pool without a periodic wake loop.

The foreground semantic bridge owns lifecycle admission, tree registration
routing, and the shell-wide completion registry. Native tree invalidations and
view-owned completion sessions dispatch awaitable provider calls through the
application pool. Those native owners remain authoritative for parent,
registration, generation, buffer-revision, and lifecycle validation, so late
or stale provider results are ignored without involving V8.
