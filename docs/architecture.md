# Knot Code Architecture

Current component boundaries, ownership, dependency direction, and major
runtime flows. Implementation contracts live in code; rationale lives in
`decisions.md`.

## System map

Knot is one Rust package whose module boundaries may become crate boundaries:

```text
gpui application and views        extension runtimes
             |                           |
             +------ editor host --------+
                         |
                  core editor model
```

- `core` owns UI- and scripting-independent editor data.
- `app` owns the gpui shell, foreground models, and native views.
- `host` contains V8 and the typed extension transport.
- `view` is the standalone renderer benchmark harness, not the application
  view layer.

The default `knot` binary runs `app::run`. Other binaries exercise prototype
benchmarks.

## Core editor model

`src/core` contains:

- `TextBuffer`: UTF-8 text, stable positions, line lookup, and the primitive
  edit log, implemented as a stable-ID piece table.
- `AnchoredRangeStore`: stable range geometry without feature, source, or
  presentation metadata.
- `EditTransaction`: one reversible group of primitive edits, not an undo
  history manager.

Anchored ranges consume the buffer edit stream:

```text
TextBuffer mutation
        |
        v
BufferEdit log
        |
        v
AnchoredRangeStore::stabilize
```

## Foreground application

gpui's foreground thread exclusively owns editor state.

Each `BufferModel` owns:

- one `TextBuffer`;
- an application-level editable or read-only access policy;
- one shared `AnchoredRangeStore`;
- source-owned semantic editor contributions;
- open/closed lifecycle and public revision;
- at most one immutable UTF-16 snapshot cache entry.

Presentation metadata stays in `app` and refers to core range IDs. A model
commit advances the public revision once, invalidates the snapshot cache, and
notifies every observing view. All text mutation paths enforce the model's
access policy; read-only models still permit snapshots, view anchors,
contributions, and closure. `core::TextBuffer` remains unconditionally mutable.

`BufferRegistry` assigns monotonic transport handles and holds weak model
references. Its active-buffer entry is the prototype command context.
`OpenBufferCollection` independently assigns local presentation identities and
strongly owns the titled models visible in the shell, including its selected
entry. Generated search entries also retain their application-owned
`SearchResultsController`, which keeps source targets and emitted output ranges
separate from the generated text. The collection does not own transport
handles or file and persistence state.
`CommandRegistry` binds command names to one extension lifecycle.
`BufferSubscriptionRegistry` routes committed changes to interested extension
lifecycles.

Each `EditorView` owns cursor, selection, scroll, focus, IME, rendering choices,
and its persistent selection range. Multiple views may observe one model while
retaining independent presentation state.

`TreeView` owns cached semantic items, expansion, selection, focus, scroll, and
per-parent loading/error generations. It renders and handles input from cached
foreground state only.

`TerminalView` is a native gpui surface that owns one authoritative local PTY
session, Alacritty emulator grid, and its focus state. Alacritty's event loop
performs PTY reads, parsing, and writes on a background thread and sends
coalesced wakeups to the gpui foreground. The foreground routes input and
resize messages to that event loop and renders the grid through gpui. The view
does not use or expose an editor model. Layout bounds determine the grid and
PTY dimensions. Child exit is reported back to the view; closing or replacing
a session shuts it down and reaps the child off the foreground thread.

The production boundary separates a stable `TerminalSession`, which owns the
PTY, emulator, and process lifecycle, from a disposable `TerminalView`, which
owns presentation, focus, and layout. A session survives view reconstruction
or relocation across tabs and windows. Explicit terminal closure terminates
the session, and reopening creates a new one; detached persistence and
simultaneous presentations remain out of scope.

```text
gpui Shell / registries
           |
           v
      BufferModel
       /       \
TextBuffer   AnchoredRangeStore
       \       /
 semantic contributions
       /       \
EditorView A  EditorView B
```

Native fixture search captures an immutable source snapshot and revision. Its
controller derives semantic matches, formats them into a read-only
`BufferModel`, and attaches built-in actions to the emitted result ranges. The
normal open-buffer and editor paths render the generated model; search identity
does not enter `TextBuffer` or `BufferModel`.

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
serial per extension. Independent extension threads may progress in parallel.
Fatal interruption or disposal removes work belonging to that lifecycle.

Commands are invoked on their owning extension with an invocation identity and
optional active-buffer handle. Cancellation is checked again before applying
foreground mutations, preventing late completion from editing a document.

Buffer snapshots are immutable. An extension-local response store transfers a
shared UTF-16 allocation to an external V8 string without exposing mutable
buffer storage. Each isolate holds an independent reference.

## Semantic extension UI

Editor contributions follow one foreground-owned path:

```text
extension replacement set
        |
typed host request
        |
revision, range, and lifecycle validation
        |
BufferModel source replacement
        |
stable anchors and model notification
```

The request envelope supplies source identity. Replacement, explicit disposal,
buffer closure, and extension teardown remove the complete source set.

Tree providers use the reverse path:

```text
extension registration/invalidation
        |
native TreeView generation
        |
reverse runtime command
        |
extension getChildren callback
        |
generation and lifecycle validation
        |
native cached presentation
```

Responses apply only while registration, parent, generation, and extension
lifecycle still match. JavaScript never participates synchronously in painting
or input.

## Invariants

- `core` has no platform, rendering, or scripting dependencies.
- Text and edit history are buffer state. Cursor, selection, folding state,
  scroll, and zoom are view state.
- A view need not own a text buffer.
- Stable positions and buffer edit events are one contract.
- `AnchoredRangeStore` owns geometry; visual policy belongs above `core`.
- V8 and Deno objects remain inside `host`.
- Extension requests cross boundaries as Knot-owned semantic data and opaque
  identities.

## Major gaps

- Production undo history, edit grouping, and view-state restoration.
- Focus-target command routing and complete keymaps.
- Filesystem and capability-provider aggregation.
- Production terminal interaction and rendering.
- Production extension scheduling, quotas, and slow-consumer policy.
- Public platform accessibility integration.
