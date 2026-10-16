# Production extension APIs and JavaScript project management

Status: ✅ D046, D047 and the D053 maintenance interlude complete, approved
on 2026-10-15. D048 is complete and approved on 2026-10-16.
Numbered product checkpoints 5–10 remain unstarted; D049 is next.
D049 execution is split into [docking and automatic tool-layout memory](d049-dockable-workbench-plan.md)
and [native extension UI](d049-native-extension-ui-plan.md) plans, in that order.

Source: the project-management design discussion of 2026-10-13 and the
[prioritized roadmap](../roadmap.md). Work identifiers: D046–D052 and promoted
D005. A separate D053 maintenance interlude follows D047 and precedes D048.
This is an architectural enhancement initiative, validated through a
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
- The public `knot` facade exposes buffers, commands, keybindings, trees,
  completion, selected text and clipboard writing. D047 additionally implements
  application queries/observation, programmatic resources/documents, root replacement
  and protected closure; it is approved and complete. Durable storage is D048.
- Contextual APIs use admitted command captures and reject missing/stale context.
  D047 implements separate live targets and application queries.
- D047 document UUIDs are separate from session-local collection IDs and buffer/view
  handles. Identity is preserved across Save As; persistence across restarts is
  still D050 work.
- Each shell owns zero/one ordered root records with retained IDs and a monotonic
  generation. Workspace/directory generations reject stale tree results.
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

Contributions live in draggable native tool frames. Sidebars are supported
placements, alongside edge splits and tab groups shared with documents and
terminals. Provide recognizable docks, tool selectors/toggles, dragging, and
resizing. Design the layout model for this behavior; replace the prototype where
needed rather than treating reuse as a constraint. The
native resource explorer uses that same frame infrastructure while retaining its
own filesystem protocol. Floating tool windows and cross-window tool movement
are deferred; existing document/terminal window operations remain supported.
Validate useful content through a Git-status-style fixture with grouped files,
status labels, row/group actions, and activation, plus a minimal read-only text
frame. No actual Git implementation or general widget framework is required.

Automatically persist tool-only layout preferences in application-owned storage:
tool dock locations, grouping/order, sizes, visibility/selection, and hidden
tools' remembered locations. Fresh windows inherit the last arrangement the user
customized; already-open windows retain independent live layouts. Focusing an
older window, restoring preferences, or an extension automatically showing a tool
does not update that default. Reset Layout returns to product/contribution
defaults; no explicit Save Layout workflow is required.

Tool-layout records contain no document tabs, document splits, terminals, or
empty document placeholders. A tool placed among document tabs reopens in the
normal document tab group of a fresh window. Exact mixed layouts belong to
session restoration at checkpoint 10, which takes precedence over fresh-window
defaults. Basic tool-layout memory is part of D049, not deferred to D005.

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

Each extension has its own SQLite database selected by validated canonical
package identity; personal config has a separate stable database. Initial storage
is application-global. Reload, updates, and uninstall preserve data.

Use ordinary synchronous SQLite calls on the existing background executor, off
the foreground and outside V8. Reuse a connection per database and serialize its
access; no dedicated storage service or custom queue. Expose async JSON record
read/write/delete/list through the existing host bridge. Each write atomically
replaces one JSON record; related state lives in one record. Writes acknowledge
after commit. Concurrent writes use last-committed-write semantics; no public versions or conditional updates. Transactions never span JS
awaits. Graceful shutdown drains admitted native storage work.

Initialize the record schema and report storage errors without replacing damaged
or incompatible databases. Extensions own the structure and schema versions of
their JSON values. D048 adds no automatic backups, cleanup UI, or multi-process
coordination. Application-owned data belongs in separate storage; application
metadata and recovery coordination remain D050 work. Public callers do not get
connections, SQL, or another owner's data. Concrete crate/settings and API names
are reviewed in the focused plan's implementation gates.

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

Checkpoint 2 is complete and explicitly approved on 2026-10-15 after automated
validation and manual observer, shared-identity, Save As, and disposal smoke checks.
Checkpoint 3 is complete and explicitly approved on 2026-10-15 following automated
validation (608 library tests and 5 example tests, Clippy, API and documentation
checks) and user acceptance of its product review gate.
Keep observation small: one foreground snapshot-and-subscribe operation, ordered
notifications, window-owned workspaces, and existing product lifetime rules.

- Finalize and publish document/window/workspace namespaces and live-target
  types with implemented operations; extend the D046 declarations and contract
  tests rather than publishing speculative stubs.
- Implement document/window/workspace/presentation queries and atomic initial
  observation with ordered post-commit events.
- Introduce persistent document identity and ordered root records in the model
  and protocol, with one-root enforcement. Durable document metadata follows checkpoint 7 (D050);
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

### 3. Programmatic resources and documents — D047 ✅

- Expose explicit file/folder opening, root-set replacement, untitled creation,
  document presentation, and protected closure through existing product flows.
- Capture and validate window/pane/view targets; implement explicit-target
  admission and active-window convenience captured at admission, including a
  structured failure when activation has no available window. Support operations
  returning document data independently from command execution outcomes.
- **Validation:** native/JS parity, URI deduplication, missing destinations, invalid
  UTF-8/provider errors, focus changes, window closure during I/O, cancellation,
  and dirty documents shared by multiple windows.
- **Review gate:** ✅ user accepted the product review gate and approved
  target/outcome contracts on 2026-10-15.

### Maintenance interlude — Handler declarations refactoring, D053 ✅

✅ Complete and approved on 2026-10-15. All 33 host capabilities and
53 native command definitions come from annotated typed async handlers, without
parallel transport or handwritten registration tables. Native participant routing
and dynamic JS lifecycle ownership are preserved. Local checks passed; remote
Linux/Windows build/linking results were not verified for this interlude.

D048 is complete and approved; resume numbered checkpoint 5 (D049) next.
Checkpoints 5–10 remain unstarted. D053 added no D047 requirement or D020 user
automation. The broader extension/project initiative remains in progress.

### 4. SQLite-backed extension storage — D048 ✅

✅ Complete and approved on 2026-10-16, including asynchronous shutdown,
process interruption/reopen coverage, and the four-operation public API.
All 640 library tests and extension API checks passed.

- Publish the storage namespace, declarations, documentation, and contract tests.
- Implement per-owner databases and async JSON read/write/delete/list using
  synchronous SQLite calls on the existing background executor. Atomically replace
  one record per write; keep related state in one record.
- Implement schema initialization, committed-write acknowledgement, diagnostics,
  shutdown drain, and preservation of damaged or incompatible databases.
- Application metadata/recovery remains D050. No conditional updates, dedicated
  storage service, multi-record batches, multi-process coordination, or automatic cleanup/backups.
- **Validation:** real DB reopen across lifecycles and sequential process runs;
  owner isolation; failed-write preservation; cancellation/unload; shutdown;
  interruption; unsupported schemas; corruption/unwritable database and diagnostics.
- **Review gate:** approve public semantics, concrete SQLite durability settings,
  retained data on uninstall, and application/extension database separation.

### 5. Production native UI contributions — D049 ⬜

Execute the focused [docking and tool-layout memory plan](d049-dockable-workbench-plan.md),
then the [native extension UI plan](d049-native-extension-ui-plan.md). Each has
its own checkpoints and review gates; completing the layout prerequisite alone
does not complete D049. The design discussion on 2026-10-16 established draggable
frames, familiar docks/sidebars, mixed document/tool tab groups, automatic
tool-only layout memory, independent open windows, and deferral of floating tool
windows. Current implementation is not a constraint on the design.

- Finalize native UI contribution APIs in `workbench`, with explicit originating
  window/instance context for providers and actions. Extend declarations and test
  isolation across windows, including contributions registered before windows exist.
- Replace hardcoded extension-tree creation with namespaced contribution
  definitions, placement, and per-window instances. First establish native tool
  docking/resizing and automatic tool-layout memory, moving the resource explorer
  into that model. Implement tree/list actions and text frames, picker, input,
  and confirmation contracts, validated by a Git-status-style fixture.
- Include show/focus, selection/actions, window context, loading/errors,
  invalidation, command routing, disposal, and failed-startup cleanup.
- Adapt existing outline fixtures/examples through the supported contract.
- **Validation:** gpui integration tests for registration before/after windows,
  multiple contributions/windows, focus/command captures, stale providers,
  cancellation/window closure, unload/recreation, and picker/input dismissal.
  Include durable tool-layout reopen, fresh-window inheritance, remembered hidden
  tools, independent open windows, and no document placeholders in tool records.
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

- Extend D049's tool-layout memory with exact session layout restoration. A
  restored session uses its own mixed tool/document arrangement instead of the
  fresh-window default. Tool preferences remain separate from document split/tab
  structure; do not encode document placeholders into the tool-only records.
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
  then mark the initiative complete. Preserve durable details in current docs
  and remove this plan in a separate commit; Git retains its execution history.

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

Concrete storage schema/settings and API names are validated in the focused D048
plan. D049's focused plans establish docking and automatic tool-layout semantics;
their concrete APIs, storage schema, and sizing details are finalized at their
review gates. Recovery cadence/retention, multi-window project activation, and
detailed startup precedence remain at their stated gates. No unresolved choice may be
silently baked into earlier public or persisted schemas. Add a focused subsidiary
design if a checkpoint exposes a materially larger problem; retain this initiative's
ordering and links instead of implementing several unreviewed checkpoints at once.

The current architecture/decision references continue to describe implemented
behavior. Update them as checkpoints land; do not record this proposed platform
as already validated implementation. D019/D021/D034/D043 remain outside the
initiative except where explicitly referenced above.
