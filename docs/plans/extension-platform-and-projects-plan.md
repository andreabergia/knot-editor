# Production extension APIs and JavaScript project management

Status: ✅ D046 and D047 checkpoint 2 complete and approved on 2026-10-15;
checkpoints 3–10 not started.

Source: the project-management design discussion of 2026-10-13 and the
[prioritized roadmap](../roadmap.md). Work identifiers: D046–D052 and promoted
D005. This is an architectural enhancement initiative, validated through a
usable JavaScript extension rather than a Rust project-manager implementation.

## Outcome

A bundled JavaScript extension manages named projects, their ordered roots,
open documents, and presentation state. Users can create/select/rename/remove
projects, switch between them, and resume them after restarting Knot, including
untitled documents and unsaved changes. Initial validation uses one root; the
completed initiative supports multiple roots and window/pane/tab/view restoration.
No terminal state or terminal tabs are restored.

The platform is equally important to the outcome: an ordinary installed JS
extension can observe application state, open resources, contribute native UI,
and persist its own records through the same supported contracts. The project
manager gets no private bypass into editor models or rendering.

## Existing constraints and implementation gaps

Read [architecture](../architecture.md), [decisions](../decisions.md), and the
[extension host](../architecture/extension-host.md),
[documents](../architecture/documents-and-persistence.md),
[workbench](../architecture/workbench-and-lifecycle.md),
[commands](../architecture/commands.md), and
[semantic UI](../architecture/semantic-ui.md) references for current behavior.

- The application owns documents and foreground mutation. `core` owns text and
  edit primitives without filesystem, gpui, or scripting dependencies.
- Persistent V8 isolates, bounded scheduling, awaitable host requests, lifecycle
  cleanup, command composition, and semantic providers already exist. Extend
  those boundaries; do not replace the runtime to build this feature.
- The public `knot` facade currently exposes buffers, commands, keybindings,
  trees, completion, selected text, and clipboard writing. It does not expose
  the application document/workspace catalog, general lifecycle events,
  programmatic resource opening, or durable extension storage.
- Contextual APIs use admitted command captures and reject missing/stale context.
  Explicit live window/view targets and application queries remain D047.
- Application document IDs and buffer/view transport handles are process-local.
  They cannot serve as persisted document references across restarts.
- A shell has one optional workspace root. Workspace generations and directory
  request generations already reject stale tree results.
- Extension trees use predeclared kinds; the shell hardcodes an `outline` tree.
  A production contribution registry must let extensions create their own views.
- Product resource opening already normalizes URIs, deduplicates documents, and
  revalidates captured targets after asynchronous I/O. Protected closure already
  coordinates dirty documents across windows. Reuse these flows.
- Startup evaluates personal pre-init, installed extensions, and post-init before
  opening product windows. Session restoration must integrate with that ordering
  rather than pretending a window exists during extension activation.

## Agreed design and boundaries

### Public API and application context — D046, D047

Publish a documented, typed JS API organized around application documents,
workspaces/windows, workbench surfaces, commands, and storage. Exact method
names are finalized at the first review gate; this plan specifies semantics.
Ship TypeScript declarations for JavaScript authors without requiring a TS
runtime. Audit the existing facade, not only the newly added operations. D046
retains `editor`, `commands`, `workbench`, and the keybinding exports. D047 must
introduce the document/window/workspace namespaces with its queries and operations;
D048 introduces storage, and D049 finalizes native contribution APIs with real
instance semantics. Do not publish empty namespaces or speculative operations.

There is no backward-compatibility requirement. Change contracts directly and
update all repository consumers together, without aliases, shims, or deprecation
periods. Package API-version validation diagnoses unsupported contracts before
evaluation; it does not promise support for older API versions.

Distinguish runtime lifecycle identity, stable package identity, persistent
document identity, live buffer handles, and live window/pane/tab/view identities.
A reopened document resolves to a fresh live handle while retaining its durable
identity where appropriate. A URI is a resource locator, not the identity of an
untitled document or of a document that undergoes Save As.

Capture command context before UI changes focus. Host operations invoked by
events/providers use explicit targets; top-level activation cannot accidentally
borrow whichever window becomes active later. Operations with an optional
active-window convenience capture it at admission and reject when unavailable.
Opaque handles never grant access to gpui entities or V8 values. D046 exposes
captured command context and removes global-focus fallback from existing
context-dependent operations. D047 must supply explicit live window/view targets
and admission-time active-window convenience for its application operations;
D049 must supply window/instance context to native contribution providers and
actions. Context-free activation may register features and use genuinely
application-scoped operations, but may not implicitly select a product window.

Document errors, cancellation, completion, and disposal. New asynchronous APIs
report structured errors and settle exactly once. Commands retain execution
outcomes; semantic operations return data directly. Existing synchronous
keybinding batching remains an intentional contract. No generic command task
graph or command-schema framework is required.

### Observation — D047

Expose snapshots of windows, workspaces, documents, and their presentations,
alongside post-commit events for document creation/opening, closure, resource
association changes, dirty/content changes, workspace roots, presentation changes,
and active target changes. Define the minimal event set at checkpoint 2.

Distinguish opening a document for the first time, presenting an existing document
in another tab/window, and activating an existing tab. Folder opening is a
workspace-root transition. Save As changes resource association. Failed/cancelled
operations publish no successful-open event; all native/CLI/JS origins converge
on the same foreground commit boundary.

Provide atomic observation initialization: register observation and capture a
snapshot with a sequence watermark in one foreground operation. Later events
follow that watermark, so activation and asynchronous startup cannot lose changes
between a query and a subscription. Sequences are session-local, not durable
recovery revisions. Payloads identify targets and relevant revisions/generations.

Listeners remain asynchronous scheduler roots, never foreground callbacks.
Operations finish after their foreground commit, without awaiting listeners.
An event handler can await another host operation without the operation waiting
for its own queued callback. Listener failures are isolated and reported;
unsubscription/unload removes ownership and rejects stale deliveries. A caller
can dispose and observe again for a fresh baseline. Reuse scheduler queue-depth
and lag diagnostics; do not add event replay, public batches, observer recovery,
or a separate backpressure mechanism. Do not silently drop correctness events.
Broader enforcement remains D019 unless concrete evidence at a checkpoint
requires bringing a specific policy into scope.

### Workspaces and programmatic operations — D047, D052

Represent workspace roots as an ordered collection of records with stable
root identity and normalized URI from the first relevant model change. Initially
enforce at most one root without baking `root: URI` into persistent schemas or
public types. A root-set generation invalidates captured operations; directory
requests retain per-request generations. Define duplicates, overlap, ordering,
and replacement at the multi-root gate.

Files remain independently openable outside workspace roots. Workspace membership
does not establish a filesystem sandbox or ownership of every open document.
JS project identity is independent of roots and remains stable on rename/root
changes. The Rust application has workspaces, not a project policy model.

Expose explicit resource opening, root-set mutation, untitled-document creation,
document presentation, and protected closure. Existing provider normalization,
UTF-8 validation, dirty semantics, URI deduplication, and target revalidation apply
to JS too. Return typed outcomes for cancellation, missing resources, decoding
failure, and stale targets. The ordinary Open behavior for missing destinations
must be documented rather than implicitly changed by a file/folder API split.

### Extension UI — D049

Keep the validated surface-specific semantic UI architecture. Extend native
trees/lists and add a searchable picker, text input, and confirmation/message
surfaces needed to create, select, rename, and remove projects. Text-oriented
content should use document surfaces where suitable. Do not introduce a general
declarative widget tree, JS rendering callbacks, or a WebView framework.

A contribution declares a namespaced surface ID, title, supported native kind,
placement, and provider/actions. Application registries own definitions and
window-local instances; views own focus, selection, expansion, scrolling, and
rendering. Declare before windows exist and instantiate when windows attach.
Provider calls receive the window/instance context, preventing one shared runtime
from confusing projects in different windows.

Define duplicate IDs, placement/order, visibility, show/focus behavior,
per-window applicability, command context, loading/empty/error states, provider
invalidation, cancellation, disposal, and instance recreation. Unload and failed
activation remove contributions, keybindings, and pending callbacks. Replace the
hardcoded outline slot and adapt existing examples/tests through supported APIs.
Native resource trees remain distinct from extension-owned provider trees.

Pickers and inputs capture their originating window, settle on dismissal or
window closure, and restore focus when appropriate. Avoid synchronously calling
JS for painting or raw input. No accessibility claim beyond what gpui exposes;
retain semantic information for D030.

### Durable extension storage — D048

The Rust application owns SQLite access off the foreground and outside V8.
Expose async extension-private record operations: read/write/delete/list and
atomic batches. JSON-compatible values describe extension records; SQLite is
the durability backend. Public callers do not get connections, SQL, or private
application tables.

Namespace by validated canonical package identity, not runtime IDs, directory
paths, or caller-supplied ownership strings. Personal config has a reserved stable
namespace. Runtime unload/reload and package updates preserve data. Uninstallation
does not implicitly destroy recovery or extension records; explicit cleanup policy
is reviewed at checkpoint 4. Initial storage is application-global. JS can key
records by stable project ID; avoid a mutable current-folder storage scope.

Define atomic batch behavior, write ordering, conflicting writes, committed-write
acknowledgement, cancellation before/after commit, and shutdown drain. A serial
service may order writes, but read/modify/write is not automatically atomic:
provide an expected-version conditional write/batch for records that require it.
Do not hold a database transaction open across arbitrary JS awaits.

The application owns physical schema migrations and storage diagnostics; the
extension owns versions/migrations of its values. Test reopen, migration failure,
unwritable storage, corruption handling, and interruption. Preserve problematic
data rather than silently replacing it with an empty database. Finalize connection
layout, journal/durability settings, backup/recovery policy, SQLite crate, and
multi-process contention handling at the storage gate. This initiative does not
add synchronization, secrets storage, or a general database API.

### Unsaved-document recovery — D050

Recovery belongs to the Rust application document/persistence layer. `core`
remains storage-independent. The application preserves unsaved text independently
of extension health; JS stores document references and presentation metadata.

Persistent document identity survives restart and Save As. Live handles do not.
The recovery service records text plus title, optional resource URI, document
state, and saved-baseline/version information sufficient to reconstruct dirty
state and detect external changes. Recovery generations identify committed
snapshots; live buffer revisions remain session-local. Do not reuse old buffer
revisions or infer cleanliness by comparing reset revision counters.

Restore one shared document model for all its presentations. Preserve URI
deduplication when recovery and ordinary opening meet. Missing files, changed
files, and URI ownership collisions produce explicit recovery outcomes; recovered
text is preserved and never silently written over disk. Generated documents are
recreated by their owning feature or skipped with a diagnostic, not backed up as
ordinary editable documents. Persistent undo is excluded (D012).

Resolving an older retained capture must not overwrite newer edits in an already
live shared document. Report the conflict and preserve both content versions;
finalize whether the user reuses the live document or recovers the older version
as a separate untitled document at the recovery gate.

Define two retention roles: automatic protection of active unsaved documents and
durable checkpoints retained by suspended sessions. Saving may retire an automatic
backup but cannot invalidate an older suspended session's retained snapshot.
Expose opaque retained capture references where immutable checkpoint semantics
are needed, in addition to stable document IDs. Rust owns snapshot bytes,
retention bookkeeping, and revision validation; JS owns which capture its session
references. A durable capture barrier must finish before a session is committed
or views are removed. Session updates release old captures only after the new
record is durable. Prefer orphan retention to premature deletion after a crash;
make eventual cleanup inspectable. Finalize the coordinated storage API at
checkpoint 7, including how extensions retain/release captures without selecting
application tables or forging owners.

Separate save, close presentation, suspend, discard unsaved changes, and delete
project metadata. Last-view closure must not automatically erase a retained
snapshot. Ordinary closure keeps Save / Don't Save / Cancel semantics; Don't Save
is an explicit discard, with shared-session references accounted for. Project
deletion needs explicit handling of retained unsaved content rather than cascading
data loss. Retention and discard behavior must be visible in the UI.

Recovery writes can be batched off the foreground. Specify the acknowledged
durability point, background capture cadence, crash-loss window, graceful shutdown
flush, and failure behavior at checkpoint 7. Do not promise preservation of edits
that have not reached a durable checkpoint. The native application can list and
recover retained documents even if the project extension fails or loses metadata.
Do not require a final JS unload callback to preserve user text.

### Project switching and restoration policy — D051, D005

The JS extension owns named projects, ordered roots, recent selection, document
membership, window bindings, and versioned session/layout records. Start with
one active project per window; different windows can use different projects.
Shared documents across windows remain application-owned. Finalize whether one
project may be active in several windows at checkpoint 8; do not assume closing
one window closes its documents elsewhere.

Switching automatically suspends unsaved documents durably. JS captures source
session state and prepares the destination through generic application operations.
The application validates targets/revisions, commits recovery, and coordinates
presentation replacement. Avoid a sequence of unprotected close/open commands
that can leave half a switch applied. The native transition knows nothing about
project names; it accepts typed document/presentation captures.

Stage native reads and restore candidates without destroying the source. Persist
the suspended source session before removal; revalidate after every asynchronous
phase. On cancellation, storage failure, racing edits, stale window, or failed
preparation, preserve the source presentation and content. Destination resource
failures can be surfaced as a reviewed partial-restore result without discarding
source recovery. Publish state events from committed transitions, with operation
identity/reason so JS can avoid saving intermediate restore state as a new session.

Startup attaches contributed UI after config/extension startup, then allows the
project extension to restore into available windows. Launch file requests and
manual actions must take precedence over automatic restoration according to a
defined policy. Native recovery remains usable when extension startup fails.
Fatal personal-config behavior stays as currently defined; recovery data is
preserved for a subsequent successful launch.

## Checkpoints and review gates

Execute one checkpoint per implementation request. Mark deliverables ✅ only
after validation and the applicable review gate. Update current architecture
references as behavior lands; record validated rationale in decisions. Split
coherent implementation work into logical commits without conventional titles.
Run `cargo fmt` once at the end of Rust work, before committing.

### 1. Production public API foundation — D046 ✅ complete

Detailed execution: [D046 public API foundation plan](../archive/plans/d046-public-api-foundation-plan.md).

- Audit all existing JS operations and identify prototype coupling, implicit
  context, swallowed asynchronous errors, and missing lifecycle contracts.
- Publish the API contract/types and an identity/context matrix. Implement the
  facade/bridge cleanup needed for current operations, with an explicit absence
  of backward compatibility and pre-evaluation package API-version diagnostics.
- Keep host/protocol V8-free and semantic. Remove historical/prototype comments
  only in touched code; do not begin an unrelated module/crate restructuring.
- **Validation:** JS boundary tests for arguments, error conversion, cancellation,
  disposal/unload, and existing command/buffer/keymap/provider behavior; verify
  declaration examples against the shipped facade.
- **Review gate:** ✅ approved on 2026-10-15, including namespaces, context,
  identities, error/disposal rules, compatibility strategy and scoped cleanup.
  Validation passed: 559 library tests, 5 example tests, Clippy with warnings
  denied, the pinned authoring checker and manual product smoke checks.

### 2. Observable application state — D047 ✅

Detailed execution: [D047 application state and operations plan](d047-application-state-and-operations-plan.md).
This also covers checkpoint 3, retaining a separate review gate for each.
Checkpoint 2 is complete and explicitly approved on 2026-10-15 after automated
validation and manual observer, shared-identity, Save As, and disposal smoke checks.
Checkpoint 3 has not started.
Keep observation small: one foreground snapshot-and-subscribe operation, ordered
notifications, window-owned workspaces, and existing product lifetime rules.

- Finalize and publish document/window/workspace namespaces and live-target
  types with implemented operations; extend the D046 declarations and contract
  tests rather than publishing speculative stubs.
- Implement document/window/workspace/presentation queries and atomic initial
  observation with ordered post-commit events.
- Introduce persistent document identity and ordered root records in the model
  and protocol, with one-root enforcement. Durable metadata follows checkpoint 4;
  no cross-restart promise until it is persisted.
- Route native Open, CLI/platform requests, Save As, activation, and close commits
  through common event boundaries. Complete actual document/buffer lifetime
  invalidation rather than retaining transport entries after product closure.
- **Validation:** no gaps during subscription startup; open versus present versus
  activate; cross-window shared documents; Save As; cancelled/stale opens; listener
  failure/unload; awaiting host calls in listeners without scheduler deadlock.
- **Review gate:** ✅ approved on 2026-10-15: event taxonomy, snapshots, ordering,
  root/identity model, live targets, and final-document invalidation, exercised
  through the observing JS fixture.

### 3. Programmatic resources and documents — D047 ⬜

- Expose explicit file/folder opening, root-set replacement, untitled creation,
  document presentation, and protected closure through existing product flows.
- Capture and validate window/pane/view targets; implement explicit-target
  admission and active-window convenience captured at admission, including a
  structured failure when activation has no available window. Support operations
  returning document data independently from command execution outcomes.
- **Validation:** native/JS parity, URI deduplication, missing destinations, invalid
  UTF-8/provider errors, focus changes, window closure during I/O, cancellation,
  and dirty documents shared by multiple windows.
- **Review gate:** approve target/outcome contracts using a JS open command in
  the real product.

### 4. SQLite-backed extension storage — D048 ⬜

- Finalize and publish the storage namespace with the implemented record API,
  extending the public declarations, documentation, and contract tests.
- Implement stable owner namespaces, async records, conditional updates, and
  atomic batches; persist application identity metadata behind a separate boundary.
- Implement schema initialization/migration, committed-write acknowledgement,
  storage diagnostics, shutdown drain, and preservation on recovery failures.
- Specify multi-process writer behavior without taking on D015 document handoff.
- **Validation:** temporary real DB reopen across lifecycles/process runs; namespace
  isolation; transaction rollback; conflicting writes; cancellation races; shutdown;
  migration failure; corruption/unwritable database and preserved diagnostics.
- **Review gate:** approve public storage semantics, durability/backup policy,
  cleanup behavior, and application/extension table separation.

### 5. Production native UI contributions — D049 ⬜

- Finalize native UI contribution APIs in `workbench`, with explicit originating
  window/instance context for providers and actions. Extend declarations and test
  isolation across windows, including contributions registered before windows exist.
- Replace hardcoded extension-tree creation with namespaced contribution
  definitions, placement, and per-window instances. Implement the needed
  tree/list, picker, input, and confirmation contracts.
- Include show/focus, selection/actions, window context, loading/errors,
  invalidation, command routing, disposal, and failed-startup cleanup.
- Adapt existing outline fixtures/examples through the supported contract.
- **Validation:** gpui integration tests for registration before/after windows,
  multiple contributions/windows, focus/command captures, stale providers,
  cancellation/window closure, unload/recreation, and picker/input dismissal.
- **Review gate:** inspect the native interaction in a product build and approve
  placement and provider/request semantics. Manual review is not a substitute
  for automated lifecycle and routing coverage.

### 6. Usable JS project registry — D051 ⬜

- Bundle a project-manager extension using only public APIs. Establish bundled
  source loading, canonical identity, version/dependency handling, load diagnostics,
  and collision policy with installed packages through the existing loader.
- Create/list/select/rename/remove projects; persist stable project IDs and
  roots arrays; open the selected root in the originating window. Display useful
  errors through the contributed native UI. Root selection does not yet claim
  full session switching or dirty-content suspension.
- Capture the product/API limitations exposed by using the extension and fix
  relevant prototype coupling before proceeding.
- **Validation:** real packaged JS through product host, storage and native UI;
  registry survives restart; package failures clean up contributions; independent
  windows use captured context; missing roots and storage errors remain actionable.
- **Review gate:** dogfood the simple registry and approve the extension/platform
  boundary. This is intermediate validation, not initiative completion.

### 7. Durable unsaved-document recovery — D050 ⬜

- Implement persistent document metadata, automatic recovery, retained consistent
  captures, resolve/restore operations, dirty-baseline reconstruction, and a native
  recovery listing/command independent of the project manager.
- Implement the durable capture barrier and safe linkage of session records to
  retained content; specify retention/release, orphan handling, discard, crash-loss
  window, and graceful flush. Include untitled, missing-destination, and edited files.
- **Validation:** reopen/crash interruption at persistence boundaries; captured
  revision versus racing edits; no durable dangling references; multiple views;
  Save As; external change/deletion; URI collisions; disk failure; saved versus
  suspended generations; extension failure and missing session metadata.
- **Review gate:** approve exact durability, recovery conflict, retention, and
  discard semantics. Demonstrate native recovery with the project extension absent.

### 8. Safe JS project switching and document sessions — D051, D050 ⬜

- Persist per-project sessions and window bindings; implement generic foreground
  capture/prepare/replace operations and JS orchestration of durable suspension.
- Restore documents into a simple tab arrangement; preserve shared application
  models and source state on failure. Define multi-window project activation.
- Define removing a project with retained unsaved content and ordinary close/quit
  behavior now that automatic recovery exists. Terminal tabs are never persisted;
  any terminal affected by replacement follows existing close behavior and does
  not terminate during preparation or a failed switch. Review its visible treatment.
- **Validation:** A→B→A with unsaved/untitled documents, concurrent views/windows,
  racing edits, cancellation, missing destination resources, storage failure,
  interruption between durable source capture and native commit, project deletion,
  and event listeners that must not replace a session with intermediate state.
- **Review gate:** dogfood suspension and restoration; approve failure/partial
  restore results and lifecycle semantics before richer layout work.

### 9. Multi-root workspace product behavior — D052 ⬜

- Remove single-root enforcement; implement add/remove/reorder/replace roots and
  a native multi-root resource tree. Upgrade project records without changing IDs.
- Define overlapping roots, duplicate normalized URIs, root labels, tree selection,
  root removal with documents still open, folder-open replacement policy, and
  invalidation across root-set changes. Preserve workspace-independent files.
- **Validation:** roots with overlapping paths, normalization and duplicate rules,
  deterministic enumeration, stale requests during root mutations, root removal,
  record migration, and multi-root sessions across restart/switching.
- **Review gate:** approve root semantics and native navigation in the product.

### 10. Layout, view state, and startup restoration — D005, D051 ⬜

- Expose versioned layout/view capture and restore: windows, split tree, pane/tab
  ordering, selected tabs, focus, cursor/selection, and scroll state. Durable records
  describe structure; do not persist process-local pane/tab/view handles.
- Preserve one shared document with independent restored views. Clamp positions
  safely when disk content changes. Define window geometry on changed displays,
  unavailable contributed/generated surfaces, and layouts containing excluded
  terminals; pruning cannot leave an invalid split tree.
- Integrate JS restoration after product windows exist, with user/CLI/platform
  open requests taking precedence, migration of old session records, interrupted
  restore diagnostics, cancellation, and extension-independent recovery access.
- **Validation:** complete multi-root project restart including dirty documents,
  split layouts and shared views; startup order/open-request races; changed/missing
  resources; extension/config failure; invalid session versions; changed displays;
  missing surfaces and terminal omission; no duplication on repeated restoration.
- **Review gate:** review the complete product workflow and API documentation,
  then mark the initiative complete and archive this plan in a separate commit.

## Completion and scope limits

Completion requires all ten checkpoints and review gates, important model,
lifecycle, command, UI-integration, and persistence paths covered by automated
tests, and end-to-end validation using the shipped JS extension. Use focused
tests at their owning boundary and product integration tests for cross-boundary
behavior. No native-app/browser automation is required for manual review.

Excluded: terminal restoration/process persistence, persistent undo/history,
remote providers, arbitrary UI/WebViews, generic SQL/native filesystem access
from JS, collaboration/sync, settings/secrets systems, archive installation,
and unrelated explorer editing features. These exclusions do not excuse missing
failure, cancellation, and lifecycle contracts in the APIs implemented here.

Physical storage schema/settings, exact facade names, UI placement, recovery
cadence/retention, multi-window project activation, and detailed startup precedence
are deliberately finalized at their stated gates. No unresolved choice may be
silently baked into earlier public or persisted schemas. Add a focused subsidiary
design if a checkpoint exposes a materially larger problem; retain this initiative's
ordering and links instead of implementing several unreviewed checkpoints at once.

The current architecture/decision references continue to describe implemented
behavior. Update them as checkpoints land; do not record this proposed platform
as already validated implementation. D019/D021/D034/D043 remain outside the
initiative except where explicitly referenced above.
