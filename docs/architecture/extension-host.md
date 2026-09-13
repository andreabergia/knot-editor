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
and joins its workers. The engine integration, extension loading, and the
`--fixture` runtime experience remain unavailable until later D017 tasks.

V8 process initialization is idempotent and owned by `engine`. It must happen
before creating scheduler workers, because every thread that may lock a shared
isolate must be created after initialization. The host composition point must
initialize the engine before constructing a pool whose executor enters V8.

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
