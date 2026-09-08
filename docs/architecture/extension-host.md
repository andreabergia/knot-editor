# Extension host

Part of the [architecture](../architecture.md). Design rationale is recorded in
[decisions](../decisions.md); deferred work lives in [deferred.md](../deferred.md).

## Extension host

`host` is the isolation boundary around V8 and `deno_core`. Each prototype
extension owns a persistent JavaScript runtime on one OS thread. A shared Tokio
runtime performs asynchronous native work.

`host::protocol` contains Knot-owned transport identities, requests, responses,
contribution data, and errors. It depends on neither gpui nor concrete core
models. A foreground-local bridge resolves opaque handles immediately before
dispatching synchronous work against application registries and entities.

```text
extension JavaScript
        |
extension runtime thread
        |
typed request / response
        |
gpui foreground bridge
        |
application registries and models
```

Runtime lifecycle state owns pending work, cancellation, command
registrations, subscriptions, resource limits, and teardown. Callbacks are
serial per extension. A same-runtime composed command is a nested handler frame
inside the active callback rather than a queued callback. Independent
extension threads may progress in parallel.
Fatal interruption or disposal removes work belonging to that lifecycle.

Commands are invoked on their owning extension with an invocation identity,
explicit JSON-like arguments, and an optional captured-buffer handle.
Cancellation is checked again before applying foreground mutations, preventing
late completion from editing a document. Every invocation produces a
structured completed, unavailable, invalid-target, invalid-argument,
cancelled, or handler-failure outcome. Extension handlers explicitly classify
argument validation failures; other JavaScript exceptions are handler failures.

Buffer snapshots are immutable. An extension-local response store transfers a
shared UTF-16 allocation to an external V8 string without exposing mutable
buffer storage. Each isolate holds an independent reference.

## Buffer transport

- Buffer edits are revision-checked batches. Change events preserve order per
  extension and carry committed edits rather than fresh snapshots.
- Knot remains UTF-8-native. A buffer retains at most one immutable UTF-16
  snapshot allocation keyed by revision and range for sharing with V8 external
  strings. Current evidence does not justify a UTF-16 core or public byte-array
  text API.

`BufferSubscriptionRegistry` routes committed changes to interested extension
lifecycles.

## Validated production direction

The current runtime remains one persistent isolate and OS thread per extension.
The production target is a bounded pool of movable isolates built on `rusty_v8`.
Scheduling, quotas, slow-consumer policy, packaging, and module resolution remain
open; see [deferred extension work](../deferred.md#extensions-and-automation).
