# Production extension APIs and JavaScript project management

Status: ⬜ In progress. Checkpoints 1–8 and D053 are complete and approved, and
the [codebase health maintenance
interlude](#maintenance-interlude--codebase-health-cleanup-) is approved.
Checkpoint 9 is implemented and awaits its review gate; checkpoint 10 remains
unstarted.

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
Contributed providers and actions receive explicit window/instance context;
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
limit; the duplicate, overlap, ordering, and replacement rules are settled in
[checkpoint 9](#9-multi-root-workspace-product-behavior--d052-).
Files remain independent of workspace containment. JS project identity is
independent of roots and stable across rename/root changes; the native application
owns workspaces, not project policy.

### Extension UI — D049

✅ Implemented. The [workbench architecture](../architecture/workbench-and-lifecycle.md)
owns docking, resizing, relocation, and automatic tool-only layout memory.
[Semantic UI](../architecture/semantic-ui.md) and [decisions](../decisions.md)
own contributed instances/content, explicit window context,
picker/input/confirmation semantics, and the shared native text input. Exact
mixed document/session restoration remains checkpoint 10.

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
text is preserved and never silently written over disk. Persistent undo is
excluded (D012).

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

### 5. Production native UI contributions — D049 ✅

Approved after acceptance of the full public-API fixture, independent windows,
lifecycle cleanup, and tool-layout memory across restart. Current ownership,
runtime flows, and rationale live in [workbench
lifecycle](../architecture/workbench-and-lifecycle.md), [semantic
UI](../architecture/semantic-ui.md), and the relevant [decisions](../decisions.md)
sections.

### 6. Usable JS project registry — D051 ✅

Approved after dogfooding the bundled `@knot/projects` registry. Bundled package
loading, canonical identity, collision policy, and built-in storage ownership live
in [host architecture](../architecture/extension-host.md#package-admission-and-startup)
and [decisions](../decisions.md#bundled-extension-packages); folder selection lives
in [decisions](../decisions.md#folder-selection) and
[documents and persistence](../architecture/documents-and-persistence.md#workspaces-and-resource-trees);
project ownership and active-project derivation are recorded in
[decisions](../decisions.md#bundled-project-registry). The registry itself is
[extensions/@knot/projects](../../extensions/@knot/projects), documented by the
[extension guide](../extension-api.md).

### Maintenance interlude — Codebase health cleanup ✅

Approved 2026-10-24. Prototype probes, generated document state, the Find stub,
and test-only code are gone from production builds. The library exports only
`run()`, `core`, and the hidden declaration surface, so `cargo check --lib`
covers `app` and `host` for dead code. The remaining item-level allowances name
checkpoint 8, D050, or D052. Local saves resolve symlinks and sync the parent
directory; see [documents and
persistence](../architecture/documents-and-persistence.md). Generated-document
rationale lives in [decisions](../decisions.md#generated-text-surfaces).
`ProductShell` concerns live in `src/app/product/{save,open,closure,tools,render,launch}.rs`.
Its inline tests moved to `product/tests.rs` and were later split by theme under
`product/tests/`, leaving shared fixtures in `product/tests.rs`.

### 7. Durable unsaved-document recovery — D050 ✅

Approved 2026-10-25 in three sub-checkpoints: 7a recovery store and automatic
protection, 7b restore, conflicts, discard, the native recovery listing, and
hot-exit quit, and 7c the extension `recovery` namespace with owner-stamped
retained captures, a durable retain barrier, and the orphan policy. Unsaved
content survives restart and crash as dirty documents with an empty undo history,
restore never writes a resource, and native recovery works without any extension.
Current behavior lives in [documents and
persistence](../architecture/documents-and-persistence.md#durable-unsaved-document-recovery)
and rationale in [decisions](../decisions.md#unsaved-document-recovery-store)
(store, restore and hot exit, retained captures). Validation is in
`src/app/recovery.rs`, `src/app/product/tests/recovery.rs`, the recovery handler
tests, and the `recovery` contract in `scripts/extension-api/contract.test.ts`.

### 8. Safe JS project switching and document sessions — D051, D050 ✅

Approved 2026-10-25. `@knot/projects` keeps an active or suspended session record
per project; a window belongs to a project when its root matches and the session
is active, and a project may span several windows. Selecting a project retains
the source windows' unsaved and untitled text, durably suspends the source, and
swaps the windows through the generic `workbench.replaceWindows` commit, which
revalidates retained revisions and rejects racing edits as `SessionConflict`;
the registry retries with fresh captures. Active sessions follow tabs live for
hot exit, losing the last window suspends without captures, startup suspends
unbound sessions and reconciles orphan captures, removal names the unsaved text
it discards, and partial restores commit and report skipped entries. A released
window holding a terminal is cleared instead of closed, and a window left
without a normal document group receives an untitled document (an empty start
panel is D061). The native Restart Extension command stops and reloads one
package, resolving health review P1-3.

Current behavior lives in [session
replacement](../architecture/workbench-and-lifecycle.md#session-replacement),
[retained captures](../architecture/documents-and-persistence.md#retained-captures),
[restarting a package](../architecture/extension-host.md#restarting-a-package),
and the [extension guide](../extension-api.md#project-sessions); rationale in
[decisions](../decisions.md#project-sessions) (project sessions, the replacement
contract, and extension restart). Validation is in
`src/app/product/tests/sessions.rs`, `tests/project_sessions.rs`,
`tests/projects.rs`, `tests/extension_restart.rs`, and the `replaceWindows` and
`windows.focus` contract in `scripts/extension-api/contract.test.ts`.

### 9. Multi-root workspace product behavior — D052 ⬜

Remove single-root enforcement, including the `TooManyWorkspaceRoots` error
type rather than a raised limit, then carry multiple roots through the native
model, explorer, public API, and `@knot/projects`. Execute in two
sub-checkpoints, 9a native and 9b projects, with one review gate at the end.

#### Root semantics (approved for planning 2026-10-25)

- **Ordering and identity.** A window's roots are an ordered list. Order is
  user-visible (explorer order) and deterministic; retained normalized URIs keep
  their `RootId` across any replacement, so add, remove, and reorder are all
  replacements of the whole set.
- **Duplicates.** Normalization can make different inputs equal, so a proposed
  set is deduplicated after normalization, keeping the first occurrence, rather
  than rejected. Adding a root that is already present is a no-op. No new error
  code.
- **Overlap.** Nested and overlapping roots are allowed. Each root is browsed on
  its own, so a file under both appears under both. Expansion and selection are
  keyed by `(RootId, URI)` so the two copies stay independent, while cached
  directory listings can be shared by URI.
- **Labels.** A root row shows the last path segment. Roots with the same
  segment add their parent segment (`src — knot`, `src — other`). There are no
  user-assigned root names; projects own naming.
- **Explorer layout.** Every root is a top-level expandable row, even when there
  is only one, expanded on first install. One layout and keyboard model gives
  root-level actions an obvious target.
- **Folder Open.** Native Open of a folder keeps replacing the window's roots.
  A new **Add Folder to Workspace…** command appends one. **Remove Folder from
  Workspace** and **Move Folder Up/Down** act on the explorer's selected root:
  the root row itself, or the root that contains the selected row. Launch
  requests for a directory keep opening a window with that single root.
- **Root removal.** Removing a root never closes documents: files stay
  independent of workspace containment, and there is no prompt.
- **Invalidation.** Every root-set change bumps the generation, so in-flight
  enumeration and Open/root requests captured earlier are rejected as today.
  Retained roots keep their expansion, selection, and cached children, and
  directories that were loading are re-requested under the new snapshot. A
  removed root drops its state, and a new root requests its children.
- **Projects follow root edits.** A window belongs to an active project when its
  root set equals the project's root set, ignoring order. No two projects may
  share a root set, but one folder may belong to several projects. When a
  bound window's roots change to a non-empty set that no other project uses,
  live tracking updates the project's roots and applies the same set to the
  project's other windows. Clearing a window's roots, or changing them to
  another project's set, unbinds that window. The binding used to detect the
  edit is in memory only; the registry still persists no per-window binding.

#### 9a. Native multi-root model, explorer, and API ✅

- ✅ `WorkspaceState::replace_roots` becomes infallible and deduplicates;
  delete `TooManyWorkspaceRoots`, `WorkspaceSnapshot::root()`, and the
  single-root helpers (`replace_root`, `install_workspace`'s `expect`). Remove
  the error from `WorkspaceError`, `ReplaceError`, the handler error enums,
  and `knot.d.ts`. Drop the D052
  `dead_code` allowances, deleting `is_descendant_of` if nothing uses it.
- ✅ `product::operations::replace_roots` and `workbench.replaceWindows` accept
  any number of roots per window and deduplicate after normalization. They
  still validate every root before one commit.
- ✅ `WorkspaceTree`: top-level root rows with disambiguated labels;
  expansion and selection keyed by `(RootId, URI)`; a per-root visited set so
  overlapping roots both render; Left from a top-level entry selects its root
  row; `set_workspace` preserves retained-root state as described above.
- ✅ Native commands Add Folder to Workspace…, Remove Folder from Workspace,
  and Move Folder Up/Down. They go through the shared root-replacement path,
  appear in the palette, and put Add Folder in the File menu. The explorer
  frame auto-shows on a non-empty root set as today; tool-layout memory is
  unchanged.
- ✅ `knot.d.ts` and the [extension guide](../extension-api.md):
  `replaceRoots` takes an ordered set with the deduplication rule, and there
  are no separate add/remove methods because retained IDs make read-modify-
  replace sufficient. Update the `workspaces` and `replaceWindows` cases in
  `scripts/extension-api/contract.test.ts`.
- ✅ **Validation:** `workspace.rs` unit tests for deduplication after
  normalization, retained IDs across reorder/add/remove, and generation bumps.
  `workspace_tree.rs` tests for root rows and labels, overlapping roots with
  independent expansion and selection, deterministic order, and stale responses
  across add/remove while enumeration is in flight. Product tests in
  `product/tests/workspaces.rs` for the native commands, Open replacing every
  root, Add Folder of an existing root as a no-op, root removal leaving
  documents open, and an extension `replaceRoots` racing a native Add Folder
  (rejects `InvalidTarget`). A `replaceWindows` case with multi-root
  destinations in `product/tests/sessions.rs`. The race is covered by the
  existing `roots` scenario of the pending-operation test. The workspace and
  explorer [architecture references](../architecture/documents-and-persistence.md#workspaces-and-resource-trees)
  describe the new behavior; the decision record waits for the review gate.

#### 9b. Multi-root projects ✅

- ✅ Membership compares order-insensitive root sets instead of first roots.
  `projects.create` rejects a root set another project already uses. The
  Projects tree and picker describe a project by its first root's label plus
  `+N`.
- ✅ Live tracking keeps an in-memory window→project binding, adopts root edits
  from bound windows into the project record, and propagates them to the
  project's other windows with `workspaces.replaceRoots`. Switching passes the
  project's full root list to `replaceWindows`.
- ✅ Records: schema 1 already stores `roots` as an array, so existing
  single-root records load unchanged and keep their IDs. The registry schema
  stays 1 unless 9b needs a field, in which case it gains a read-time upgrade.
  Update the header comment and the
  [bundled project registry decision](../decisions.md#bundled-project-registry) wording
  about first-root matching.
- ✅ **Validation:** `tests/projects.rs` and `tests/project_sessions.rs` for
  set-equality membership regardless of order; adding and removing a root in
  a bound window updating the record and the project's second window; clearing
  roots unbinding (and suspending on the last window); a set colliding with
  another project unbinding instead of merging; a stored single-root schema-1
  record loading with its ID; and a multi-root project switched away, restored,
  and restored again after restart (startup suspension and capture
  reconciliation included). All live in `product/tests/project_sessions.rs`;
  the existing registry reload test covers a stored schema-1 record keeping
  its ID, and the schema stayed 1.

#### Documentation and review gate

- ✅ Update the workspaces section of
  [documents and persistence](../architecture/documents-and-persistence.md#workspaces-and-resource-trees),
  the explorer notes in [workbench lifecycle](../architecture/workbench-and-lifecycle.md),
  and `architecture.md` only if ownership changes (it should not). Add a
  *Multi-root workspaces* decision recording deduplication over rejection,
  allowed overlap with `(RootId, URI)` view state, replace-on-Open, and
  projects following root edits. Remove P2-6 from the health-review table.
- ⏸️ **Review gate:** approve root semantics and native navigation in the product:
  add, remove, and reorder roots; overlapping roots; Open replacing roots;
  projects following edits across two windows; and a multi-root project
  surviving switch and restart.

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
  unavailable contributed surfaces, and layouts containing excluded
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

## Follow-up from the codebase health review

The [codebase health review](codebase-health-review.md) findings are scheduled
as follows. Remove the review once every item below has landed or moved into the
roadmap register.

| Finding | Where |
| --- | --- |
| Prototype removal, `Generated` removal, test-only fencing, symlink Save, Find stub, `ProductShell` split, hygiene | ✅ [Maintenance interlude](#maintenance-interlude--codebase-health-cleanup-) |
| Extension restart/disable escape hatch (P1-3) | ✅ Checkpoint 8: native Restart Extension |
| Single-root error removal (P2-6) | ✅ Checkpoint 9a |
| Editor view incremental line model, piece-table length cache, insert coalescing and edit-log bounding, CRLF policy (P0-1, P1-1, P2-7) | After checkpoint 10, as a focused plan before D002 and D031 |
| `engine.rs` split and marshaling unification, command module regrouping, `app/terminal` and `app/extensions` directories, inline test extraction, tokio runtime removal (P1-6, P2-1–P2-4) | After checkpoint 10, as pure-move maintenance between slices |

Pull the editor view work forward only if dogfooding on large files becomes
disruptive. Checkpoint 10 restores byte-offset view state that does not depend
on the view's line representation.

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

The completed D049 contracts for docking, contributed frames, and tool-layout
memory now live in the current architecture and decisions references; storage
schema and remaining sizing details are finalized at their review gates.
Recovery cadence/retention, multi-window project activation, and detailed
startup precedence remain at their stated gates.

**Explorer migration sequencing (D053).** The native shell-owned explorer stays
native through checkpoint 8. Migrate it to a bundled JavaScript extension only
after checkpoint 6 establishes bundled loading and checkpoint 9 settles
multi-root workspace and root-replacement semantics; the migration is then a
focused D053 slice that reuses D049's tree and action APIs. Do not pull it ahead
of checkpoint 6, and do not re-scope checkpoint 9's native multi-root resource
tree to the packaged extension, without recording that decision here first.

No unresolved choice may be silently baked into earlier public or persisted
schemas. Add a focused subsidiary
design if a checkpoint exposes a materially larger problem; retain this initiative's
ordering and links instead of implementing several unreviewed checkpoints at once.

The current architecture/decision references continue to describe implemented
behavior. Update them as checkpoints land; do not record this proposed platform
as already validated implementation. D019/D021/D034/D043 remain outside the
initiative except where explicitly referenced above.
