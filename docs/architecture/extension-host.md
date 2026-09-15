# Extension host

Part of the [architecture](../architecture.md). Design rationale is recorded in
[decisions](../decisions.md); active implementation work lives in
[D017](../plans/d017-v8-isolate-pool-plan.md).

## Current boundaries

`host` owns five explicit boundaries:

- `protocol` contains Knot-owned transport identities, requests, responses,
  contribution data, and errors. It depends on neither gpui nor concrete core
  models.
- `scheduler` owns bounded worker scheduling independently of V8 mechanics.
- `engine` owns direct `v8` initialization, isolates, contexts, bindings, and
  all V8 types.
- `lifecycle` owns extension lifetime and teardown state.
- `bench` owns host benchmark entry points.

The scheduler owns a V8-independent state machine and a fixed worker pool.
Each loaded lifecycle has one authoritative state, one FIFO root queue, and at
most one active logical command tree. A cloneable handle admits lifecycles,
queues roots, wakes continuations, and requests stop; the pool owner shuts down
and joins its workers. The engine composes this scheduler with one persistent
runtime capsule per loaded lifecycle. The product bridge and `--fixture`
runtime experience remain unavailable until later D017 tasks.

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

Fixture modules run as scheduler root turns. Each capsule owns an immutable
in-memory source graph and persistent compiled-module cache. Static imports use
URL resolution and can reach only sources already embedded in that graph;
bare, missing, and private-bootstrap imports fail resolution. The engine
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

The engine retains the root script or module promise independently from its
host-request promises. A pending root with no runnable JavaScript yields its
worker in `AwaitingHostWork`. A validated response is queued against that
root, settles the persistent resolver inside a later locked isolate turn, and
drains microtasks before the root is inspected again. Multiple requests may be
pending and may settle out of order; unrelated roots remain queued until the
active root and its detached native requests have all settled.

Unload first marks the scheduler lifecycle stopping and terminates any running
JavaScript through a thread-safe control handle. Disposal waits for the running
locker to leave, then releases persistent handles and callback state while
locked before dropping the shared isolate. Immutable external UTF-16 strings
transfer one `Arc` reference to V8 and release it on garbage collection or
isolate disposal, including when disposal happens on a pool worker.

```text
extension JavaScript (under reconstruction)
        |
host engine + bounded scheduler
        |
Knot-owned typed protocol
        |
gpui foreground bridge (under reconstruction)
        |
application registries and models
```

## Preserved boundary contracts

Extension requests cross the application boundary only as Knot-owned semantic
data and opaque extension, lifecycle, request, registration, invocation,
subscription, and buffer identities. The foreground owner will revalidate
captured identity, revision, generation, and lifecycle before mutation.

Buffer ranges and edits remain UTF-8 byte based. Snapshot storage retains the
immutable UTF-16 representation needed by the future direct-V8 binding without
changing the UTF-8 core. Detailed behaviors to restore are recorded in D017's
protocol restoration ledger.

## Scheduler flow

A newly admitted lifecycle starts in `Loading`. Successful loading moves it to
`Idle`, or to `Queued` when roots arrived during loading. `Queued` is also the
ready-queue deduplication bit. A worker takes one FIFO turn and changes the
lifecycle to `Running`; its result completes the active root, tail-queues one
continuation turn, yields in `AwaitingHostWork`, or fails that lifecycle.

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

The foreground buffer bridge owns active-buffer lookup, lifecycle-scoped
subscriptions, revision validation, model mutation, contribution ownership,
and change fan-out. It revalidates cancellation inside the foreground update
immediately before mutation. Product ownership of the pool and transport loop
remains for the later integration checkpoint.
