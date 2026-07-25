# Knot Code Architecture

This document is an orientation map for the code that exists today. It covers
component boundaries, ownership, and the important runtime flows.

Knot is still a prototype. Some directories contain completed experiments
rather than code on the current application path.

## System map

Knot is one Rust package with a library and several binaries. Its intended
dependency direction is:

```text
gpui application and views        extension runtimes
             |                           |
             +------ editor host --------+
                         |
                  core editor model
```

The boundaries are being validated as modules before any crate split.

The library exports four top-level modules:

- `core` owns editor data models and is independent of UI, rendering, and
  scripting.
- `app` owns the gpui application shell, editor view, foreground buffer
  models, and buffer registry.
- `host` owns the native boundary to extension runtimes.
- `view` is a renderer benchmark harness, not the future editor view layer.

## Current application paths

The default `knot` binary calls `app::run` and opens the gpui shell and editor.
It loads `rust_sample.kfx` by default and creates the active core-backed
`BufferModel`. It also starts the prototype extension runtime and connects its
typed request stream to the foreground buffer registry.

The standalone `step3-gpui` binary delegates to `app::run` and uses the same
application path as the default binary.

The remaining binaries benchmark the renderer, text buffer, and annotation
models. Their supporting code and fixtures are experiments, not application
architecture.

## Core editor model

`src/core` contains three cooperating models:

- `buffer::TextBuffer` owns text, stable positions, line lookup, and the edit
  log. It is currently implemented as a stable-ID piece table.
- `anchored_range::AnchoredRangeStore` owns stable range geometry without
  source, feature, or presentation metadata.
- `transaction::EditTransaction` records one reversible group of primitive
  buffer edits. It is not an undo history manager.

Buffer offsets and ranges are UTF-8 byte offsets. Grapheme-aware movement and
other presentation concerns belong above `core`.

Anchored ranges follow the buffer's edit stream:

```text
TextBuffer mutation
        |
        v
BufferEdit log
        |
        v
AnchoredRangeStore::stabilize
        |
        v
anchored-range resolution and queries
```

The buffer and anchored-range Rustdoc define stable-position behavior, endpoint
semantics, and lifecycle details.

## Application model and view

gpui's foreground thread exclusively owns editor state. Each `BufferModel`
entity owns one `core::TextBuffer`, its shared
`core::anchored_range::AnchoredRangeStore`, its source-owned editor
contribution registry, its open/closed lifecycle, and a public revision that
advances once per editor-visible atomic commit. Visual contribution metadata
remains in `app` and refers to core anchored-range IDs; `core` does not know
about decoration tokens or contribution ownership. The core buffer's
`edit_seq` remains a private primitive-edit-log cursor.

`BufferRegistry` gives models monotonic, never-reused transport handles and
stores only weak entity references. It also tracks the optional active buffer.
That active-buffer state is the prototype's current extension-facing command
context, not a requirement that every view have a buffer. Step 11 will
generalize command invocation to capture a focused view target with an optional
associated text buffer.
`CommandRegistry` is foreground-owned and keeps command names authoritative;
each registration is bound to its extension and unique extension lifetime.
Each buffer has at most one atomic contribution set per `ContributionSource`.
A source is either built-in code or an extension identity plus its unique
lifecycle. Lifecycle cleanup removes only the set published by that runtime
incarnation.
Each `EditorView` owns cursor, selection, scroll, focus, IME, rendering
choices, and derived line, segment, decoration, gutter-marker, and
contribution-action projections. Multiple editor-view entities may observe the
same `BufferModel`; model notifications update their text and contribution
projections independently without coupling their presentation state. Each view
also owns the identity and direction of a persistent range in the model's
shared anchored-range store, so its cursor and selection follow buffer edits
while remaining view-local state.
Contribution actions emit semantic command requests; the shell rechecks that
the current contribution and command registration have the same live extension
lifecycle before dispatch. Local edits commit through the model and model
notifications refresh view projections. A foreground-local bridge task
owns each extension request inbox and dispatches requests synchronously against
the registry and its entities between awaits.

The native `TreeView` entity owns the outline's cached semantic items,
expansion, selection, focus, scroll, and per-parent loading/error generations.
It emits asynchronous child requests and command names to the shell; rendering
and input use only cached foreground state. Responses are applied only when
their registration, parent, and generation still match. Provider disposal or
extension-lifecycle cleanup clears the provider and invalidates pending
responses. Tree roles, labels, ordering, expansion, and selection remain
renderer-independent semantics, but gpui 0.2.2 has no public platform
accessibility bridge through which Knot can expose them.

```text
gpui Shell / BufferRegistry
             |
             v
      BufferModel entity
       /       |       \
      v        v        v
TextBuffer  AnchoredRange  ContributionSource
               Store       → ContributionSet
       \       |       /
        model notifications
          /           \
         v             v
 EditorView A       EditorView B
 projection and     projection and
 view state         view state
```

## Extension host

`src/host` is the Knot-owned isolation boundary around V8 and `deno_core`.
`V8Host` owns process-wide V8 initialization and shared native asynchronous
work. Each extension runs a persistent JavaScript runtime on its own OS thread
and communicates with the host through typed request and response messages.

`host::protocol` contains the transport-level identities, buffer, command, and
semantic editor-contribution data and errors without depending on V8, Deno,
gpui, or the concrete core buffer. This keeps runtime mechanics behind the host
boundary. The application dispatches active-buffer, snapshot, batched-edit,
and contribution-set requests by resolving opaque handles against
foreground-owned models immediately before each operation. `BufferModel`
validates UTF-8 byte ranges and revisions before using core's assertion-based
buffer and anchored-range APIs. It retains at most one immutable UTF-16 snapshot,
keyed by revision and requested byte range, and invalidates that cache on the
next edit. The editable core remains UTF-8-native.

Runtime ownership separates the unique request inbox, clonable non-blocking
control and response access, and the OS-thread join handle. V8 is initialized
before gpui starts. Only the Knot-owned endpoints move into application state.
On shell teardown, the foreground thread requests extension shutdown and
background work waits for the extension thread to exit.

Command registration crosses the same typed bridge. The foreground
`CommandRegistry` allocates registrations and resolves a command name to its
owning extension lifetime. Invocation is queued onto its owning extension's
runtime thread with the active buffer handle; requests issued by the handler
carry the invocation identity. The foreground cancellation set rejects
invocation-scoped host work and rechecks before applying an edit, so a late
completion cannot mutate the document.

This is the initial text-editor command surface. The planned command-dispatch
boundary captures the focused view identity at invocation time and derives an
optional text-buffer handle from it. Native commands route through the focus
hierarchy; extension code receives semantic context rather than gpui entities
or a requirement that non-text views fabricate buffers. Captured targets are
revalidated after asynchronous work instead of re-resolving current focus.

Extension-local lifecycle state owns pending work, termination, resource
limits, command registrations, buffer-change subscriptions, and teardown. A
failed or terminated isolate does not take down other extensions. The shell
retains controls for every loaded extension, so each non-empty commit fans one
immutable Knot-owned payload to every matching subscription. Delivery is
queued on the owning extension's command stream, so callbacks are serial and
ordered per extension while independent extension threads can progress
separately; a listener failure is reported but does not suppress later
callbacks. Per-extension queue depth and enqueue-to-start lag are recorded for
the later evidence phase only; no slow-consumer policy is applied.

```text
extension JavaScript
        |
        v
extension runtime thread
        |
  typed request/response
        |
        v
gpui foreground bridge
        |
        v
BufferRegistry / BufferModel / CommandRegistry / BufferSubscriptionRegistry
```

The private bootstrap module retains native bindings and turns opaque handles
into cached JavaScript `TextBuffer` proxies. Each proxy owns one cached,
idempotently disposable editor-contribution-set facade; native set identity is
derived from the request's buffer handle and extension lifecycle rather than
exposed to JavaScript. Snapshots are immutable values;
their byte/UTF-16 boundary table is built only when an adapter is called. An
asynchronous snapshot response first lands in an extension-local native
response store. A synchronous follow-up on the isolate thread then creates a
V8 external two-byte string over the snapshot's shared immutable allocation.
V8 owns one reference until that string is collected or the isolate is
disposed; it never points into mutable `TextBuffer` storage.

Editor contribution publication follows one foreground-owned mutation path:

```text
extension replace(set, revision)
              |
       typed host request
              |
 revision/lifecycle/range validation
              |
 BufferModel atomic source replacement
              |
 shared anchors + model notification
              |
     each EditorView reprojects
```

The source identity comes from the request envelope rather than extension
payload. Disposal and extension teardown enter the same model path and notify
every observing view after removing that source's complete set.

The public workbench facade registers semantic tree data providers by native
view ID. Registration, invalidation, and disposal travel through the typed host
request stream. A foreground tree generation queues a reverse runtime command;
the owning isolate invokes `getChildren` serially with its other callbacks and
returns renderer-neutral items or a recoverable error. Dropping the runtime
command queue clears pending callback completions during teardown.

```text
extension invalidate/register/dispose
                |
        typed host request
                v
       native TreeView generation
                |
        reverse runtime command
                v
   extension getChildren callback
                |
       semantic items/error
                v
 generation/lifecycle validation
                |
       native TreeView cache
                |
        gpui render/input
```

Invalidation advances only the affected parent generation and may be coalesced.
Rendering and input consult the foreground cache only. A response updates that
cache only when its registration, parent, generation, and extension lifecycle
still match; otherwise it is discarded without mutating view state.

Detailed runtime behavior and experiment results live in
`step7-v8-runtime.md` and the host module's Rustdoc.

## Architectural invariants

- `core` must not depend on platform, rendering, or scripting details.
- Text and edit history are buffer state. Cursor, selection, folding, scroll,
  and zoom are view state.
- Buffers model editable or inspectable text. A view may have no associated
  buffer and may instead own a surface-specific model; common command behavior
  comes from focus-based routing and invocation context, not a polymorphic
  buffer hierarchy.
- Stable positions and buffer edit events form one contract: buffer changes
  must preserve the information anchored-range stabilization needs.
- `AnchoredRangeStore` is the shared range-composition mechanism. Visual
  precedence, hit testing, and presentation belong to the future view layer.
- Performance-sensitive editor services stay native. Built-ins and extensions
  should use the same public editor APIs wherever practical.
- V8 and Deno objects remain contained within `host`; other components cross
  that boundary using Knot-owned types.

## Major gaps

- Undo history, edit grouping, and view-state restoration are not implemented.
- Capability aggregation, filesystem providers, the native terminal view, and
  the complete public extension API remain roadmap work.
- Command invocation currently exposes only an optional active text buffer.
  Focus-target capture, native focus-chain routing, and non-buffer command
  contexts remain step-11 work.

Update this document when component boundaries, ownership, dependency
direction, or a major runtime flow changes. Keep implementation contracts in
the code and decision history in the roadmap or step-specific documents.
