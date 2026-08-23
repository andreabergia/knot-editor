# Step 12: Capability Aggregation Plan

This experiment decides whether independent asynchronous completion providers
can feed one view-owned completion session while its native presentation is
replaced, without moving provider or presentation semantics into buffers.

## Validated constraints

- ✅ Use editor completion as the concrete capability and two separate
  JavaScript extension lifecycles as providers.
- ✅ Provider registrations are lifecycle-owned and shell-wide. Language
  matching, trigger characters, and configurable provider priorities are
  deferred.
- ✅ Each provider returns one asynchronous result set per request. Providers
  may progress concurrently because they run in independent extension
  runtimes.
- ✅ A request carries both the captured buffer revision and a monotonically
  increasing completion generation. Both must match before a response may
  update the session.
- ✅ The shell owns the provider registry. Each `EditorView` owns at most one
  active completion controller and session; completion state does not belong
  to `BufferModel`.
- ✅ The controller owns request state and the merged semantic snapshot. A
  surface owns only selection, layout, and rendering.
- ✅ Replace presentation by switching between two visibly different native
  surface implementations during an in-flight request. Provider registrations
  and the active request survive the switch.
- ✅ Surfaces do not take focus. The editor retains its input path and exposes
  completion-specific key context while a session is active.
- ✅ Completion is explicitly invoked for the prototype. Automatic triggers,
  re-query after typing, streaming results, and fuzzy matching are deferred.
- ✅ Extension-facing completion-surface replacement remains deferred. The
  experiment validates the internal ownership seam without defining an
  extension widget or presentation protocol.

## Prototype boundary

```text
extension A registration ----\
                              +--> shell completion-provider registry
extension B registration ----/                  |
                                                 v
EditorView cursor + BufferModel snapshot --> CompletionController
        |                              request generation + revision
        |                                        |
        |                         fan out to independent runtimes
        |                                        |
        |                         one-shot result / provider error
        |                                        |
        |                          validate, merge, publish snapshot
        |                                        |
        +<-- revision-checked acceptance         v
                                      active CompletionSurface
                                         list <--> compact
```

Provider requests contain an opaque buffer handle, buffer revision, cursor byte
offset, identifier prefix, and completion generation. Responses echo the
registration, revision, and generation. Native routing also retains the weak
editor/session identity needed to discard results after the originating view
or session disappears; that identity does not cross the extension boundary.

The controller publishes an immutable semantic snapshot containing merged
items, stable controller-assigned item identities, pending-provider count, and
per-provider failures. A surface receives the current snapshot when attached
and every later snapshot while it remains active. Accepting a stable item
identity asks the controller to apply its insertion semantics; surfaces never
mutate the buffer directly.

## Completion-specific merge policy

The controller extracts the contiguous ASCII identifier prefix immediately
before the cursor. It filters and ranks provider items by:

1. case-sensitive label prefix match;
2. case-insensitive label prefix match;
3. provider registration order; and
4. provider-local result order.

Non-prefix candidates are excluded. Identical insertion text is deduplicated,
keeping the higher-ranked item. Response arrival order never affects the final
ordering. Provider priority is registration order only; the prototype adds no
configurable ranking or general capability-composition framework.

Results publish incrementally. A surface preserves selection by stable item ID
when a late response changes ordering, falling back to the first item only if
the selected item disappeared. Switching surface implementations deliberately
resets selection to the first item because presentation state is not shared
across implementations.

## Execution

### 1. Add the completion provider protocol and registry

- ✅ Add Knot-owned protocol identities and serializable request, result-item,
  response, and recoverable provider-error types.
- ✅ Add a minimal JavaScript API for registering and disposing one-shot
  completion providers, with lifecycle-owned cleanup.
- ✅ Store active registrations in a shell-owned registry with monotonic
  registration identity and registration order.
- ✅ Dispatch requests through the existing reverse runtime path without
  letting JavaScript participate synchronously in editor input or rendering.
- ⬜ Revalidate registration and extension lifecycle on completion; reject
  duplicate completion and results from disposed providers.

### 2. Introduce a view-owned completion controller

- ⬜ Let each `EditorView` own at most one active completion session and keep
  the provider registry outside the view and buffer.
- ⬜ On `editor.show-completions`, capture the editable buffer, public
  revision, cursor byte offset, replacement prefix range, and a fresh
  completion generation.
- ⬜ Fan the request out to a snapshot of active provider registrations so a
  later registration change cannot alter the membership of an in-flight
  generation.
- ⬜ Track each provider independently as pending, successful, or failed and
  publish partial snapshots as responses arrive.
- ⬜ Apply the completion-specific filter, ranking, and deduplication policy
  without storing completion state in `BufferModel` or `core`.
- ⬜ Treat a provider failure as recoverable and local to one provider and
  generation. Keep other results visible and invoke the failed provider again
  on the next request.

### 3. Reject stale work and apply accepted items safely

- ⬜ Accept a provider response only while registration, lifecycle, editor,
  active session generation, buffer identity, and public revision all match.
- ⬜ Do not actively cancel a superseded request merely because a newer
  request starts; let fixture responses arrive and prove that stale results are
  discarded.
- ⬜ Cancel outstanding reverse callbacks when the controller closes or a
  provider lifecycle terminates, while continuing to reject any late transport
  completion.
- ⬜ End the active session and detach its surface immediately on a newer
  invocation, cursor movement, buffer edit, buffer closure, or editor
  destruction.
- ⬜ On acceptance, resolve the stable item identity and replace the captured
  prefix range with `insert_text` only if the buffer revision and editor
  session still match. Otherwise dismiss without mutation.

### 4. Add two replaceable native completion surfaces

- ⬜ Define the smallest application-owned completion presentation boundary:
  attach with a semantic snapshot, update with later snapshots, render, move
  selection, expose selected item identity, and detach.
- ⬜ Implement a normal list popup showing merged candidates, provider labels,
  pending-provider count, and non-blocking provider failures.
- ⬜ Implement a visibly different compact popup that presents the selected
  candidate and aggregate status without depending on provider objects or
  request machinery.
- ⬜ Anchor both non-focus-taking surfaces to the originating editor cursor
  and route up/down, accept, dismiss, and surface-swap controls through an
  active editor key context.
- ⬜ Add a fixture command that replaces the active surface during an
  in-flight request. Attach the replacement to the current snapshot and deliver
  later results without restarting or reissuing provider work.

### 5. Add observable provider fixtures

- ⬜ Run two providers in separate extension lifecycles with distinct labels,
  overlapping insertion text, and deterministic response delays: one fast and
  one approximately 1.5 seconds slower.
- ⬜ Add a fixture control that makes the slow provider fail for one request
  and recover on a later request without re-registration.
- ⬜ Make pending, incremental arrival, deterministic reordering,
  deduplication, isolated failure, and recovery visible in the running app.
- ⬜ Exercise explicit invocation, keyboard selection, acceptance, dismissal,
  and both directions of surface replacement while the slow provider is
  pending.
- ⬜ Confirm provider waits and late stale responses never block the gpui
  foreground thread.

### 6. Validate and record the decision

- ⬜ Add focused tests for deterministic merge/deduplication, stale response
  rejection, failure isolation, surface replacement with an in-flight request,
  selection preservation within one surface, and revision-checked acceptance.
- ⬜ Use the running fixture for timing, visual replacement, keyboard
  interaction, and foreground responsiveness rather than building exhaustive
  UI tests.
- ⬜ Update `architecture.md` with provider registry, view-owned completion
  session, response-validation flow, and replaceable presentation ownership if
  the experiment validates them.
- ⬜ Record the validated choices and liabilities in `decisions.md` and mark
  roadmap step 12 complete with its checkpoint result.
- ⬜ Update this plan with ✅ markers, deliberately skipped work, and the final
  result.
- ⬜ Run focused tests, run `cargo fmt` once at the end of Rust work, then run
  the full test suite.

## Commit boundaries

1. Completion protocol, JavaScript registration API, and shell registry.
2. View-owned request controller, merge policy, and stale-result handling.
3. Native list and compact surfaces with editor interaction and replacement.
4. Provider fixtures, failure recovery, acceptance, and focused validation.
5. Architecture, decisions, roadmap, and completed-plan updates.

## Deferred decisions and liabilities

- **Provider applicability:** language selectors, workspace scoping, trigger
  characters, and dynamic capability matching remain deferred. Shell-wide
  fixture registration does not validate how production chooses providers for
  a document.
- **Request evolution:** automatic invocation, re-query after typing, streaming
  results, active cancellation of superseded requests, timeouts, backpressure,
  and slow-consumer policy remain deferred. Discarding late one-shot responses
  validates correctness, not production resource use.
- **Ranking quality:** fuzzy matching, configurable priorities,
  provider-specific ranking hooks, and user ranking history remain deferred.
  The fixed prefix policy validates deterministic composition only.
- **Completion semantics:** snippets, additional text edits, commit characters,
  documentation resolution, and provider-specific resolve calls remain
  deferred. Acceptance validates only one revision-checked prefix replacement.
- **Scale and polish:** pagination, virtualization, large result sets,
  production styling, accessibility, and cross-platform popup behavior remain
  deferred.
- **Extension-owned presentation:** step 12 switches between native fixture
  implementations. It does not define how an extension replaces a completion
  surface without receiving a general widget protocol, so the product-level
  extension surface contract remains open.
- **Generalization:** the controller and merge policy stay completion-specific.
  The experiment does not introduce or validate a reusable aggregation
  framework for diagnostics, hover, formatting, or other capabilities.

## Decision checkpoint

Capability aggregation is validated if:

- two independent extension runtimes contribute incrementally without blocking
  the foreground thread;
- request generation, buffer revision, registration, lifecycle, and view
  identity prevent stale work from changing visible or editor state;
- deterministic completion-specific composition does not depend on response
  timing;
- one provider's failure does not remove another provider's results or require
  provider restart;
- replacing presentation during an in-flight request preserves controller and
  provider work while transferring only the semantic snapshot; and
- acceptance uses retained semantic identity and revision-checked buffer
  mutation rather than parsing rendered content.

The boundary is not validated if surfaces need provider objects or runtime
knowledge, if providers must restart when presentation changes, if completion
state leaks into `BufferModel`, or if late results can overwrite a newer editor
session.
