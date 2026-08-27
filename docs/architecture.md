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
An application-global `DocumentCollection` assigns document identities,
strongly owns every open document and its model, and deduplicates normalized
resource URIs across the application. Shell selection is presentation state
and remains outside the collection; `BufferRegistry` remains only a
transport-handle registry.

A document explicitly records whether it is untitled,
destination-associated-but-uncreated, persisted, or generated. Untitled dirty
state compares the model revision with its initial clean revision; a
destination-associated document is dirty until persistence succeeds; a
persisted document compares against its last successfully persisted revision;
and generated documents are never persistable. URI and persistence state
never enter `BufferModel` or `core`.

Generated search documents retain their application-owned
`SearchResultsController` alongside the model. The controller keeps semantic
source targets and emitted output ranges separate from the generated text; the
generic document collection and model have no search-result semantics.
`BufferSubscriptionRegistry` routes committed changes to interested extension
lifecycles.

Each `EditorView` owns cursor, selection, scroll, focus, IME, rendering choices,
its persistent selection range, and at most one completion controller and
surface. Multiple views may observe one model while retaining independent
presentation state. Completion request state and merged semantic candidates
belong to the controller; the replaceable list and compact surfaces own only
selection, layout, and rendering.

`Workbench` is the window-local editor layout model. It owns a binary split
tree, stable pane and tab identities, the focused pane, and ordered tabs within
each pane. Split-tree leaves refer to panes; each tab refers to an
application-owned document and strongly owns one `EditorView`. Splitting a
pane creates a new view of its active document, and closing a tab drops that
view. Empty panes are removed and their split branch collapses; closing the
last tab produces an explicit empty-workbench transition for the product shell
to handle.

Workbench closure stays independent of native dialogs. The application-wide
view count determines whether a tab is the last view of its document. A dirty
last view returns a pending close tied to the exact pane, tab, and document;
confirmation revalidates those identities and the current application-wide
view count before applying the transition. A closed final view requests
document closure but does not remove the application-owned document itself.
This keeps window-local presentation mutation separate from application-level
document and confirmation coordination.

The shell-owned `CompletionProviderRegistry` assigns monotonic registration
identities and order to lifecycle-owned, shell-wide providers. An editor
completion invocation snapshots registry membership, buffer identity and
revision, cursor and prefix range, and a view-local generation before the shell
fans requests out to independent extension runtimes.

```text
CompletionProviderRegistry snapshot
                 |
                 v
EditorView -> CompletionController -> independent extension runtimes
   |                    ^                         |
   |                    +--- validated results ---+
   |                              |
   +---- list or compact surface <-+
```

Responses reach a controller only while the registration and lifecycle remain
live and weak editor identity, active generation, buffer handle, and public
revision still match. The controller merges partial results deterministically
and assigns stable semantic item identities. Surface replacement reattaches to
the current immutable snapshot without restarting provider work. Acceptance
resolves the selected identity through the controller and performs one
revision-checked prefix replacement.

`TreeView` owns cached semantic items, expansion, selection, focus, scroll, and
per-parent loading/error generations. It renders and handles input from cached
foreground state only.

`WorkspaceTree` is a separate native surface for application-owned filesystem
resources. It owns the same kinds of foreground presentation state, but emits
requests containing normalized resource identities and workspace generations
to the shell. It has no extension provider identity and does not share the
extension-owned `TreeView` protocol.

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

### URI workspaces and persistence

`ResourceUri` is the application boundary for persisted resources. It parses
absolute hierarchical URIs and provides stable equality, scheme access, and
segment-aware containment. Scheme-specific providers own normalization. Only
the local-folder provider converts between `file://` URIs and platform paths;
generic workspace, tree, buffer, and shell state contains no `Path`.

The shell owns a registry that routes each operation by URI scheme. Providers
implement one asynchronous byte-oriented contract: normalize, enumerate
immediate children, read, replace, and stat. The memory and local-folder
providers implement the same Knot-owned results and structured errors. The
local provider dispatches native I/O through a Tokio runtime; its lexical root
is an application boundary rather than a sandbox and filesystem operations
follow symlinks.

```text
Shell-owned provider registry
        scheme -> provider
                |
       WorkspaceState
       root + generation
          /          \
         v            v
 WorkspaceTree   resource open/save
 cached UI       captured revision
                         |
                         v
                    Document
                         |
                         v
                    BufferModel
```

`WorkspaceState` owns one normalized root and a monotonic generation. The
shell normalizes and checks every target against a captured workspace before
dispatch and revalidates the capture before applying foreground results.
`WorkspaceTree` additionally versions each directory request. Enumeration is
sorted in application code with directories before files and deterministic
name and URI ordering. Painting and input use only its cached entries,
loading, selection, expansion, and error state.

Opening a file awaits normalization, stat, byte read, and UTF-8 decoding before
creating or selecting any model. A later open or workspace replacement rejects
the stale result. Saving captures entry identity, resource identity, model
text, revision, and workspace. A successful write advances only the captured
persisted revision while those identities remain live, so edits racing the
write remain dirty.

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

### Command dispatch

`CommandCatalog` owns discovery metadata and name ownership for native and
extension commands. Extension definitions bind their names to one lifecycle;
native handlers remain attached to gpui views and shells rather than moving
into the catalog. Both use Knot-owned `Command` values containing a stable name
and explicit JSON-like arguments.

Keybindings, the command palette, and top-level scripts enter one dispatcher.
Admission allocates the invocation identity, captures the origin, and creates
the completion before routing begins:

```text
keybinding / palette / top-level script
                   |
                   v
        admitted Command + captured origin
                   |
                   v
       serialized root FIFO queue
                   |
                   v
      gpui action at captured focus
                   |
       focused view -> enclosing shell
                   |
          native handler or extension runtime
```

Dispatch captures the originating window, weak shell and focus identities, and
optional surface-associated buffer. These native target identities remain
inside `app`; extensions receive the invocation identity, arguments, and an
optional opaque buffer handle. gpui routes the private action from the captured
focus target, so editor, tree, and terminal handlers can claim the same command
without a shared surface type. Unclaimed extension commands reach the shell as
the global destination.

One shell runs one root invocation tree at a time. Additional roots remain in
FIFO order until the active root and its attached descendants settle. A
handler-originated invocation is attached as one child of its active parent
and inherits the parent's captured window, workspace, focus, and optional
buffer. Native children use the same gpui focus route. Cross-extension
children run on their owning runtime while the caller remains suspended;
unrelated roots cannot enter that gap. A second unfinished child from the same
parent is rejected, so this remains command composition rather than a task
graph.

The command palette keeps the weak focus target captured before the palette
takes visible focus. Palette controls target the palette, while confirmation
dispatches the selected command at the preserved origin without refocusing it.
A missing origin is rejected rather than replaced with current focus.

Extension execution retains the inherited captured context for the invocation
lifetime. Immediately before execution and every delayed foreground mutation,
the shell revalidates the invocation, lifecycle, window and shell ownership,
focus target, and optional buffer. Every admitted invocation completes once
with a structured outcome.

The catalog resolves global ownership before choosing how a child runs. A
same-extension child executes as a nested JavaScript handler frame in the
current serial callback. Cross-extension ancestry is checked before dispatch;
targeting a runtime already occupied by an ancestor is unavailable rather than
deadlocking. Repeating a same-extension command registration in its active
ancestry is rejected. Root cancellation propagates downward, actively aborts
extension handler signals, rejects late foreground mutations, and waits for
the invocation tree to settle before the next root starts. Cancelling a child
does not cancel its parent. Runtime or shell teardown settles affected work and
cannot overwrite an existing terminal outcome.

Focus-owning editor, tree, terminal, and palette views publish semantic gpui
key contexts. Fixed bindings exercise base, focused-surface, persistent active,
one-shot transient, and multi-keystroke routing. Central dispatch consumes the
transient context; cancellation clears it without invoking a command.

Native fixture search captures an immutable source snapshot and revision. Its
controller derives semantic matches, formats them into a read-only
`BufferModel`, and attaches built-in actions to the emitted result ranges. The
normal open-buffer and editor paths render the generated model; search identity
does not enter `TextBuffer` or `BufferModel`. Activating a result carries its
emitted byte range back to the selected controller, which resolves the recorded
source target only while the source revision still matches. The primary source
editor then selects, reveals, and focuses that range.

```text
source snapshot + revision
            |
            v
 semantic search matches
            |
            v
 SearchResultsController ----> source targets
            |
            +----> generated read-only BufferModel
                              |
                              v
                    DocumentCollection
                              |
                              v
                         EditorView
```

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

Completion providers use the same asynchronous reverse-runtime direction but
remain completion-specific: the shell snapshots ordered registrations, the
view-owned controller tracks one-shot provider states and merge policy, and a
native surface consumes semantic snapshots. Provider objects and request
machinery never enter surfaces, buffers, or `core`.

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
- Generic resource state uses normalized URIs; platform paths and native I/O
  errors remain inside the local provider or explicit fixture/benchmark code.
- `BufferModel` owns editable text, never resource identity or persistence.

## Major gaps

- Production undo history, edit grouping, and view-state restoration.
- Focus-target command routing and complete keymaps.
- Filesystem watching, external-change reload, atomic save, conflict handling,
  URI deduplication, and multi-root workspaces.
- Production capability applicability policy.
- Production terminal interaction and rendering.
- Production extension scheduling, quotas, and slow-consumer policy.
- Public platform accessibility integration.
