# Semantic UI and generated text

Part of the [architecture](../architecture.md). Design rationale is recorded in
[decisions](../decisions.md); deferred work lives in [deferred.md](../deferred.md).

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

Responses apply only while registration, parent, generation, and extension
lifecycle still match. JavaScript never participates synchronously in painting
or input.

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

- Completion composition is deliberately feature-specific. ASCII prefix
  filtering, case-sensitive then case-insensitive ranking, registration order,
  provider-local order, and insertion-text deduplication produce results that
  do not depend on arrival timing.
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

Semantic tree state retains accessibility information, but gpui 0.2.2 lacks
the required public platform bridge.

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
