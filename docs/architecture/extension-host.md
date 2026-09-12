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

Only the protocol and process initialization boundaries are implemented now.
The thread-per-extension Deno prototype and its application bridge have been
removed; extension loading and the `--fixture` runtime experience remain
unavailable until the pooled host is integrated.

V8 process initialization is idempotent and owned by `engine`. It must happen
before creating scheduler workers, because every thread that may lock a shared
isolate must be created after initialization.

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
