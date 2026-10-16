# Production extension APIs and JavaScript project management

Status: ⬜ In progress. Checkpoints 1–4 and D053 are complete and approved;
checkpoints 5–10 remain unstarted. Next: D049, executing
[docking and automatic tool-layout memory](d049-dockable-workbench-plan.md), then
[native extension UI](d049-native-extension-ui-plan.md).

Work identifiers: D046–D052, promoted D005, and completed maintenance interlude
D053. [Roadmap](../roadmap.md) owns initiative priority; this plan owns execution
order and review gates.

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

## Current foundation and remaining design

Current ownership and runtime behavior live in [architecture](../architecture.md)
and its task-specific references. Public contracts live in
[knot.d.ts](../../src/host/js/knot.d.ts), with examples in the
[extension guide](../extension-api.md). Extend existing native product flows,
typed host handlers, lifecycle cleanup, and scheduling rather than creating
project-specific bypasses. There is no backward-compatibility requirement;
change contracts and repository consumers together.

### Public API and application context — D046, D047

✅ Implemented. Contextual APIs use admitted command authority; discoverable
live targets support application operations independently of commands. The
[command reference](../architecture/commands.md) owns routing and authority rules.
Future D049 providers/actions must receive explicit window/instance context;
activation may register features before product windows exist.

### Observation — D047

✅ Implemented. Atomic snapshot-and-subscribe and asynchronous metadata events
are described in [application observation](../architecture/extension-host.md#application-observation).
Future restoration must publish committed transitions without letting listeners
save intermediate restore state as a new session.

### Workspaces and programmatic operations — D047, D052

✅ D047 implemented shared resource opening, document creation/presentation,
root replacement, and protected closure; see
[documents](../architecture/documents-and-persistence.md) and
[workbench lifecycle](../architecture/workbench-and-lifecycle.md).

D052 extends the existing ordered root collection beyond its current one-root
limit. Finalize duplicate/overlap, ordering, and replacement rules at checkpoint 9.
Files remain independent of workspace containment. JS project identity is
independent of roots and stable across rename/root changes; the native application
owns workspaces, not project policy.

### Extension UI — D049

The focused [docking plan](d049-dockable-workbench-plan.md) owns native frames,
resizing, relocation, and automatic tool-only layout memory. The following
[native extension UI plan](d049-native-extension-ui-plan.md) owns contributed
instances/content, explicit window context, picker/input/confirmation APIs, and
Git-status-style acceptance. Execute both before marking checkpoint 5 complete.
Their review gates own concrete schemas, placement, and interaction choices.
Exact mixed document/session restoration remains checkpoint 10.

### Durable extension storage — D048

✅ Implemented. [Host storage architecture](../architecture/extension-host.md#extension-record-storage)
owns package/config database binding and shutdown; the public API reference owns
record semantics. Application-owned recovery and retained document captures remain
D050 work, separate from extension records.

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

Approved 2026-10-15 after boundary tests, authoring checks, Clippy, and product
smoke review. Current types, error/lifetime contracts, and examples live in the
[API reference](../../src/host/js/knot.d.ts) and [guide](../extension-api.md).

### 2. Observable application state — D047 ✅

Approved 2026-10-15 after automated validation and observer, shared-identity,
Save As, and disposal smoke checks. Current
[queries and observation](../architecture/extension-host.md#application-metadata-queries)
include live targets, document UUIDs, ordered roots, and final-model invalidation.
Cross-restart document identity remains D050.

### 3. Programmatic resources and documents — D047 ✅

Approved 2026-10-15 after native/JS parity, stale-target, cancellation, and
shared-document validation plus product review. Current flows live in
[documents](../architecture/documents-and-persistence.md) and
[protected closure](../architecture/workbench-and-lifecycle.md#workbench-and-protected-closure).

### Maintenance interlude — Handler declarations refactoring, D053 ✅

Approved 2026-10-15. Typed async host/native-command declarations and automatic
registration are documented in [decisions](../decisions.md#handler-declarations).
Local checks passed; remote Linux/Windows build/linking was not verified for
this interlude.

### 4. SQLite-backed extension storage — D048 ✅

Approved 2026-10-16 after owner-isolation, reopen, interruption, shutdown, and
failure-preservation validation. Current storage ownership and durability rationale
live in [architecture](../architecture/extension-host.md#extension-record-storage)
and [decisions](../decisions.md#extension-record-storage).

### 5. Production native UI contributions — D049 ⬜

Execute [docking and tool-layout memory](d049-dockable-workbench-plan.md), then
[native extension UI](d049-native-extension-ui-plan.md), one focused checkpoint
and review gate at a time. Those plans own deliverables, validation, and gates.
D049 completes only after acceptance of the full public-API fixture, independent
windows, lifecycle cleanup, and tool-layout memory across restart. Docking alone
does not complete this checkpoint.

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

D049's focused plans own docking, contribution, and tool-layout contracts;
concrete APIs, storage schema, and sizing details are finalized at their review
gates. Recovery cadence/retention, multi-window project activation, and
detailed startup precedence remain at their stated gates. No unresolved choice may be
silently baked into earlier public or persisted schemas. Add a focused subsidiary
design if a checkpoint exposes a materially larger problem; retain this initiative's
ordering and links instead of implementing several unreviewed checkpoints at once.

The current architecture/decision references continue to describe implemented
behavior. Update them as checkpoints land; do not record this proposed platform
as already validated implementation. D019/D021/D034/D043 remain outside the
initiative except where explicitly referenced above.
