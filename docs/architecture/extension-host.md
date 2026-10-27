# Extension host

Part of the [architecture](../architecture.md). [Decisions](../decisions.md)
records rationale; public API contracts live in [knot.d.ts](../../src/host/js/knot.d.ts).

## Boundaries

| Owner | Responsibility |
| --- | --- |
| `host::protocol` | Knot-owned identities, semantic payloads, and failures; no gpui or concrete core models |
| `host::declarations` | Typed async handler contracts, linked discovery, and response transport |
| `host::scheduler` | V8-independent lifecycle state and bounded worker scheduling |
| `host::engine` | V8 initialization, persistent isolates, contexts, bindings, and exact-once runtime teardown |
| `host::lifecycle` | Extension identity, scheduler-visible state, and failure data |
| `host::pool` | V8-free application control, awaitable event inbox, diagnostics, and shutdown |
| `host::module_graph` | Validation of immutable captured sources without V8 or filesystem access |
| `app::extension_package`, `personal_config`, `extension_load` | Background source discovery, admission, startup ordering, and outcomes |
| `app::extension_host` | Foreground routing, resource ownership, observation, and lifecycle cleanup |

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

## Package admission and startup

Installed packages provide a manifest and immutable package-local source graph.
The application validates the supported API version before capturing/evaluating
an entry, retaining recoverable package identity even on validation failure so
duplicates and rejected dependencies remain diagnosable. Discovery runs on gpui's
background executor; the foreground admits valid packages in dependency order
and awaits entry completion before loading dependents.

Bundled packages are written in TypeScript, compiled by `build.rs` into the
binary, and merged into the same discovery result, without reading sources from
disk. They use canonical `@knot/...`
identities, the same manifest, API-version, dependency, and rollback rules, and a
synthetic module root instead of a captured directory. The reserved scope cannot
be claimed by an installed package, making bundled identity structural rather
than a precedence rule; a rejected installed directory that shares a bundled
identity is diagnosed without blocking the bundled package. Installed packages
may require bundled ones. A bundled package failure is reported and fails its
dependents like any other package, without stopping product startup.

The application retains an ordered startup report, publishing outcomes as they
become known so one pending entry does not hide earlier failures. Failed or
cancelled entry rolls back its runtime and foreground registrations before the
next package starts; independent packages remain eligible.

Static imports resolve only the public `knot` module or relative captured sources
within the package root. Dynamic imports reject. Generated source URLs preserve
failure locations; V8 compilation and values stay inside the isolate. Private
bootstrap and dispatch handles cannot be invoked or replaced through author
globals to fabricate callback context or buffer proxies.

Personal config uses one manifest-free lifecycle with optional pre-init and
post-init entries sharing globals, compiled modules, and registrations. The
application selects one existing XDG config directory in preference to the
platform directory; exact discovery rules live in
[personal_config.rs](../../src/app/personal_config.rs). Capture resolves a
symlinked root and validates source containment. Pre-init cannot import post-init,
even indirectly. Post-init capture/validation errors are retained until that
phase rather than preventing valid pre-init work.

Startup awaits pre-init, installed extensions, then post-init. A config failure
unloads every startup lifecycle; installed-package failures do not stop post-init.
Window admission and fatal diagnostics belong to
[product launch](workbench-and-lifecycle.md#product-application-launch).

### Restarting a package

The native Restart Extension command lists every bundled and installed package in
the startup report, including failed ones, in the window's transient picker. A
restart performs a terminating unload of the package's current lifecycle, so a
stuck turn stops and its queued commands cancel, and runs the shared foreground
cleanup. It then rediscovers the package (bundled sources, or its installed
directory re-read from disk), revalidates its manifest, API version, and that its
dependencies are loaded, and activates a fresh lifecycle under the same storage
and recovery owner. The outcome replaces that package's report entry and is shown
in a message. Dependents keep running: packages share only the global command
namespace, so a dependent's own registrations never depended on the restarted
lifecycle. Personal config is not restartable; restarting Knot is its recovery
path. Per-turn timeouts remain [D019](../roadmap.md).

## Runtime and scheduler flow

One application-owned `ProductExtensionHost` owns the pool, foreground bridges,
and awaitable event-inbox task. Each lifecycle has a persistent runtime capsule,
one FIFO root queue, and at most one active logical command tree. The pool has
fixed worker capacity; defaults and state transitions live in
[scheduler.rs](../../src/host/scheduler.rs).

V8 initialization must precede worker creation: every thread that may lock a
shared isolate must be created after initialization. A worker locks a capsule
for one turn, enters its persistent context, runs JavaScript, drains explicit
microtasks, records failures, and leaves V8 scopes before releasing the locker.
Only thread-safe embedder state travels with the capsule. Ordinary callback
exceptions fail that request/turn; forced interruption and heap failure are fatal
to the lifecycle.

Host requests retain persistent promise resolvers and yield the worker while
awaiting native work. A validated response queues a continuation under the same
root, settles its resolver in a later locked turn, then drains microtasks.
Multiple requests can settle out of order. Unrelated roots remain queued until
the active root and its detached native requests settle. Responses racing with
a running turn are retained for the next continuation; request and turn identities
reject stale or duplicate delivery. Forced termination also wakes an awaiting
root, so it does not depend on a later host response.

Buffer listeners, application listeners, and semantic providers enter as unrelated
FIFO roots. They may await host work without retaining a worker. Callback failure
is isolated so later roots still run. Dropping a native provider receiver cancels
queued work or wakes active work to release retained promises. Diagnostics measure
queue depth, lag, and worker movement; quotas, dropping, coalescing, deadlines,
and backpressure remain [D019](../roadmap.md).

The foreground command bridge owns admitted invocation trees and completion
channels; the host schedules their JavaScript. Routing, inherited presentation
authority, inline children, ancestry limits, and contextual access belong to
[commands](commands.md#extension-execution).

## Handler declarations and transport

JS-to-native capabilities are annotated async application handlers. Linked
descriptors pair typed arguments, results, and errors; assembly rejects duplicate
operations and invalid context types before startup. Foreground resource owners
expose semantic methods independently of request envelopes. Generated adapters
own decoding and response transport rather than per-operation routing tables.
Native commands share this async contract with command-specific adapters.
Implementation contracts live in
[declarations](../../src/host/declarations.rs) and
[application handlers](../../src/app/extension_host/handlers.rs).

Owned contexts retain weak host/application access and caller authority. Short
foreground closures revalidate lifecycle, cancellation, and resource identity,
then return owned results; no foreground borrow crosses an await. Mutations
revalidate immediately before commit. Shared product operations return committed
semantic data, which handlers adapt without invoking commands or querying focus
afterward. Open/save/root behavior belongs to [documents](documents-and-persistence.md);
closure belongs to [workbench lifecycle](workbench-and-lifecycle.md).

The V8 boundary decodes recognized typed fields without conversion hooks and
rejects numeric precision loss. Typed native failures supply stable public codes;
argument and numeric errors stay author-side categories. Descriptor metadata
checks the TypeScript error union. Immutable external UTF-16 snapshot storage
retains a shared reference until V8 garbage collection or isolate disposal,
without changing the UTF-8 core.

Synchronous JavaScript keybinding calls collect an ordered batch per V8 turn.
The engine holds earlier host responses until atomic foreground batch application
settles, so an await continuation cannot overtake preceding keymap mutations.
A batch failure fails the current turn. Binding ownership and precedence belong
to [commands](commands.md#keymaps).

## Disposal, cancellation, and teardown

Disposal immediately prevents new local callback admission and retains one
native-cleanup promise, including failure. Earlier accepted mutations and
invalidations settle before acknowledgement; executing callbacks are outside that
barrier so self-disposal cannot deadlock. Queued callbacks cannot enter removed
author registrations. Native provider removal cancels pending receivers; its
surface effects belong to [semantic UI](semantic-ui.md).

Command cancellation wakes suspended execution, aborts its signal, and rejects
pending JS requests. Admission/publication races retain cancellation until the
execution is recorded. An already queued native response keeps its pending entry
until its admitted turn consumes it without settling the rejected resolver again.
This preserves root ownership while cancellation callbacks run. Accepted commits
are not rolled back.

Unload, startup rollback, fatal termination, heap failure, caught worker panic,
and pool shutdown converge on one runtime finalizer. Stop prevents queued work
and terminates running JavaScript. Once its locker leaves, the finalizer releases
persistent handles while locked, settles accepted native work exactly once, and
drops the isolate. A terminal lifecycle event triggers foreground cleanup.

Foreground cleanup is shared by lifecycle events, startup rollback, explicit
unload, and host release. It removes command/keymap registrations, subscriptions,
contributions, and semantic providers even when buffer models or views are still
retained. No final author callback or disposer retry is required. Cancellation
listener failure cannot prevent other listeners from receiving cancellation.

## Application metadata queries

Queries derive documents from the application collection and windows, panes,
tabs, roots, and focus from live product shells. Diagnostic windows are excluded;
terminals are non-document surfaces. No synchronized window catalog is maintained.

Discoverable targets are distinct from command captures. They qualify identities
with the workbench and exact presentation so replacement or transfer cannot
retarget them. Queries require a live lifecycle, honor command cancellation, and
work without command context; returned targets reserve no future validity.

## Application observation

The foreground host owns a comparison baseline, session sequence, and
lifecycle-scoped subscriptions. Document commits, shell transitions, tab focus,
native activation, and window closure signal comparison; repaint-only changes do
not publish metadata events. All documents are tracked independently of author
buffer lookup. One buffer-commit drain routes text edits to buffer subscribers;
application observation compares metadata without consuming those edits again.

Two events are not derived from metadata alone. `viewStateChanged` follows a
window's settled [layout change](workbench-and-lifecycle.md#window-layouts); the
host commits pending metadata first, so it never precedes the window's own
structural events. `launched` is sent once when startup has committed its
restored and requested windows; the snapshot's `launched` field reports it to
packages that start later, such as a restarted one.

Observation refreshes the committed baseline and captures registration, snapshot,
and watermark in one foreground request. The engine installs the author listener
before that root finishes; subsequent notifications run through the ordinary FIFO
callback path. Commits never await listeners, and a listener may await host work
or its own disposal. Queries can see newer state than an event. No separate replay,
recovery, or backpressure mechanism is introduced.

## Extension record storage

`app::extension_storage` owns separate SQLite databases selected by canonical
package identity or personal config, independently of lifecycle IDs and installation
paths. Built-in `@knot/...` identities are accepted only for bundled packages;
installed packages cannot use the reserved scope. Admission binds the owner
before startup requests can run; source-only diagnostic lifecycles have no
implicit durable owner. Teardown removes lifecycle bindings while retaining
application-owned connections and records.

The same admission binding stamps the owner of retained recovery captures,
which live in the application recovery store rather than an extension database
(see [retained captures](documents-and-persistence.md#retained-captures)).

Binding performs no I/O. Async handlers await background operations serialized
through a reused connection per database. Native transactions never span a JS
await. Caller lifetime is checked before database work starts; once started,
work finishes normally even if its caller disappears. Failures preserve damaged
or incompatible databases. Public value, ordering, and error contracts live in
[knot.d.ts](../../src/host/js/knot.d.ts); implementation and physical schema live in
[extension_storage.rs](../../src/app/extension_storage.rs).

Product and config-diagnostic quit stop storage admission synchronously, then
await admitted work and connection release before gpui quit. Host release also
detaches a drain. Task completion guards keep connections alive through admitted
work; the drain runs independently of the caller awaiting shutdown. No foreground
disk wait or final JavaScript callback is needed.
