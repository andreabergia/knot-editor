# Semantic UI and generated text

Part of the [architecture](../architecture.md). Design rationale is recorded in
[decisions](../decisions.md); deferred work lives in [roadmap](../roadmap.md).

## Boundary

Extension APIs carry native, surface-specific semantic data. gpui objects,
drawing callbacks, layout protocols, V8 values, and Rust objects do not cross
the public boundary. Native surfaces own layout and interaction; painting and
input never call JavaScript synchronously.

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
Built-ins and extensions use the same application-owned contribution registry.
Editor contributions include anchored decorations, gutter markers, and
command-backed actions.

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

Completion providers use the same asynchronous reverse-runtime direction but
remain completion-specific: the shell snapshots ordered registrations, the
view-owned controller tracks one-shot provider states and merge policy, and a
native surface consumes semantic snapshots. Provider objects and request
machinery never enter surfaces, buffers, or `core`.

## Completion ownership and aggregation

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

- Completion composition is feature-specific and independent of arrival timing.
  Filtering, ranking, and deduplication rules live in the
  [completion controller](../../src/app/completion.rs).
- Recoverable failure is local to one provider and request. Other candidates
  remain visible, and the provider participates again on the next invocation
  without re-registration.

Surface replacement deliberately resets presentation selection. Applicability,
automatic triggers, streaming, active cancellation of superseded requests,
timeouts, backpressure, richer edits, and popup polish remain deferred.

## Extension trees

`TreeView` owns cached semantic items, expansion, selection, focus, scroll, and
per-parent loading/error generations. It renders and handles input from cached
foreground state only.
Each product window owns an instance of a declared tree kind. The foreground
semantic bridge retains weak references to live instances, accepts provider
registration before windows open, and routes invalidations and responses to
each instance. Its generation rejects responses from a replaced request.
Clicking an item focuses its tree before emitting its command. The product
shell routes that event after the tree update through the captured window
command path.

Items are ordinary content or `group` headers. A group is neither activatable
nor disclosable: its children are fetched when it first appears and remain
visible, so a grouped list is one provider tree rather than a second model.
Items expose optional right-aligned status text and a theme severity badge
(`neutral`, `info`, `success`, `warning`, `error`). Actions are labeled text
commands; icon glyphs are not part of the boundary yet. A general,
extension-replaceable icon/status source (icon packs) is deferred (roadmap
D055).

Frame, group, and row actions are semantic command references with JSON
arguments, never drawing callbacks. The native view renders them as controls
and emits the same captured-window command event as activation, with the action
arguments instead of null. Action clicks stop propagation so they cannot also
activate the row. The same actions are reachable from the keyboard by entering
an action-focus mode on the current row; keyboard navigation indexes the
rendered rows directly so scrolling stays aligned when a transient loading or
error row is present. A root invalidation keeps stale rows visible while the
refetch is pending and reloads group children when the refreshed result arrives,
so selection and navigation stay usable during a refresh.

Contributed text frames are a separate content kind. Each instance owns a
private read-only `BufferModel` rendered by the ordinary `EditorView`; it is not
registered with the document collection and has no resource or persistence.
The extension replaces the whole text through a foreground request, and the
bridge keeps the last text so a window created later presents current content.
Wrapping the editor surface is provisional; the [roadmap](../roadmap.md) tracks a
dedicated read-only presentation (D054).

Semantic tree state retains accessibility information, but gpui 0.2.2 lacks
the required public platform bridge.

Provider disposal follows the shared
[host lifecycle](extension-host.md#disposal-cancellation-and-teardown). Native
unregistration cancels retained requests across live surfaces. Completion removal
also removes that provider's already-completed aggregate results; other providers
retain their state. Tree invalidation acknowledges native acceptance separately
from later fetch/paint. Invalid provider results fail only their request.

## Generated text

Generated search documents retain their application-owned
`SearchResultsController` alongside the model. The controller keeps semantic
source targets and emitted output ranges separate from the generated text; the
generic document collection and model have no search-result semantics.

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

- The validated search surface duplicates only result ordering and emitted
  range geometry. It does not duplicate previews, grouping, or source identity
  in a second presentation model.
- Generated search buffers are immutable snapshots. Navigation is rejected
  after the source revision changes; refresh, incremental updates, and restored
  view state remain deferred liabilities.
