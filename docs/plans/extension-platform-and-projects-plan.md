# Production extension APIs and JavaScript project management

Status: ⬜ In progress. Checkpoints 1–9 and D053 are complete and approved, and
the [codebase health maintenance
interlude](#maintenance-interlude--codebase-health-cleanup-) is approved.
Checkpoint 10 is planned and next.

Work identifiers: D046–D052, promoted D005, and completed maintenance interlude
D053. [Roadmap](../roadmap.md) owns initiative priority; this plan owns execution
order and review gates.

## Outcome

A bundled JavaScript extension manages named projects, their ordered roots,
open documents, and presentation state. Users can create/select/rename/remove
projects, switch between them, and resume them after restarting Knot, including
untitled documents and unsaved changes. Initial validation uses one root; the
completed initiative supports multiple roots and window/pane/tab/view restoration.
Terminal tabs are restored by position as fresh shells; no terminal process
or content is restored.

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

✅ D052 carries multiple ordered roots through the model, explorer, public API,
and projects; see [checkpoint 9](#9-multi-root-workspace-product-behavior--d052-).
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

Startup restoration is native hot exit extended to windows, settled at
[checkpoint 10](#10-layout-view-state-and-startup-restoration--d005-d051-): the
application restores every product window before launch requests open, and the
project extension rebinds its active sessions to them by root set. Switching
remains the extension's restoration path. Native recovery and window restoration
remain usable when extension startup fails. Fatal personal-config behavior stays
as currently defined; recovery data is preserved for a subsequent successful
launch.

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

### 9. Multi-root workspace product behavior — D052 ✅

Approved 2026-10-27. A window holds an ordered list of roots: proposed sets are
deduplicated after normalization, retained roots keep their IDs, nested and
overlapping roots are allowed, and `TooManyWorkspaceRoots` is gone. The explorer
shows every root as a top-level row with disambiguated labels and keys view
state by root and URI. Open replaces every root; Add Folder to Workspace…,
Remove Folder from Workspace, and Move Folder Up/Down edit the selected root,
and removing a root never closes documents. `@knot/projects` matches windows by
order-insensitive root sets through an in-memory binding: root edits in a bound
window move the project and its other windows, while clearing the roots or
matching another project unbinds the window. Switching restores every root.
File › New Blank Window opens a window outside any project, and the Projects
tool's New Window action opens one on the current project through the new
`windows.open({ roots })`. Health review P2-6 is resolved.

Current behavior lives in [workspaces and resource
trees](../architecture/documents-and-persistence.md#workspaces-and-resource-trees),
[workbench lifecycle](../architecture/workbench-and-lifecycle.md), and the
[extension guide](../extension-api.md#project-sessions); rationale in
[decisions](../decisions.md#multi-root-workspaces) (multi-root workspaces and the
bundled project registry). Validation is in `src/app/workspace.rs`,
`src/app/workspace_tree.rs`, `src/app/product/tests/workspaces.rs`,
`tests/sessions.rs`, `tests/project_sessions.rs`, and the `workspaces` and
`windows.open` contract in `scripts/extension-api/contract.test.ts`.

### 10. Layout, view state, and startup restoration — D005, D051 ⬜

One versioned window layout serves two restorers. Native hot exit restores every
product window at startup; `@knot/projects` stores the same layout in its session
records and passes it back when switching. Execute in three sub-checkpoints, 10a
layout capture and restore, 10b native window hot exit, and 10c projects, with
one review gate at the end.

#### Restoration semantics (approved for planning 2026-10-27)

- **One layout value.** `workbench.captureLayout(window)` returns a documented,
  versioned JSON `WindowLayout`: window geometry, the split tree with dock
  groups, ordered tabs and the selected tab per pane, the focused pane, dock
  sizes, and per-tab view state. Authors store the value verbatim; it never
  contains pane, tab, view, or window handles.
- **Every tab kind is restored by position.** Each tab records its kind and
  only what that kind needs to recreate a presentation in the same pane and
  order: a document tab its document ID and resource URI, a tool tab its stable
  tool ID, and a terminal tab nothing. A restored terminal starts a fresh shell
  as New Terminal does; process, scrollback, and contents are not restored
  (D043). Future kinds, such as interactive generated views (D027), join the
  same rule. A tab is pruned only when its kind cannot be recreated.
- **Switching closes and opens windows.** `replaceWindows` no longer reuses
  windows: every destination window is created with its layout and geometry,
  and every source window is released and closed. Closing stops a released
  window's terminals, as closing any window does, so checkpoint 8's
  clear-instead-of-close exception and the reused-window rules go away. Source
  text is still retained before the commit.
- **Restoring a layout.** `SessionWindow.layout` is optional. When present,
  each document tab resolves to the window's entry with the same document ID,
  else the same URI. When absent, entries present in the normal group of a
  fresh window as today. Pruning drops unmatched or unresolved document tabs,
  unregistered tools, and duplicates of one document within a pane. It then
  collapses empty panes and one-child splits. A window left without a normal group receives the ordinary untitled
  document. An out-of-range selection or focus falls back to the first
  tab or the normal group. A layout with an unsupported version or invalid shape
  is ignored and the window falls back to the entries-only arrangement; each
  window's result reports `layout: "applied" | "ignored"`.
- **Tools.** A restored layout places the tools it names exactly, including
  tools sharing document groups. Tools it does not name are hidden in that window
  and keep their remembered placement. Restoration never publishes the
  tool-only default; New Blank Window, `windows.open`, and launch-request windows
  still inherit it.
- **Shared documents and view state.** Each document tab gets its own
  `EditorView`, so a split of one document restores as two independent views of
  one shared model. View state is the anchor and caret as byte offsets, the top
  visible position as a byte offset, and horizontal scroll in pixels. Offsets
  clamp to the current text length and snap back to a character boundary, so
  changed or missing content never yields an invalid position. Folding and zoom
  do not exist yet and are not recorded.
- **Geometry.** A layout records logical bounds, windowed, maximized, or
  fullscreen state, and the display UUID. Restoration always creates its
  windows, so geometry is applied at creation and moving an existing window
  stays [D062](../roadmap.md#platforms-and-accessibility). Saved bounds are used
  when they overlap a connected display
  enough to reach the title bar. Otherwise the window is sized to fit and centered
  on the saved display if it is still connected, or else on the primary display.
- **View-state events.** A new `viewStateChanged { window }` observation event
  reports cursor, selection, scroll, focused pane, dock size, and window bounds
  changes through a trailing-edge debounce of about a second. Structural
  changes keep `presentationsChanged`. The same native signal drives window
  hot-exit writes.
- **Native window hot exit.** The application persists every product window's
  roots and layout in an application-owned record beside the tool-only snapshot,
  updated through the debounce while windows change and flushed by the quit path
  before exit. Closing a window removes it, so quitting with no windows restores
  none. Diagnostic windows are never recorded, and a launch whose config failed
  never writes. The record is marked consumed durably before restoration
  commits, so a crash during restore loses only the arrangement and the next
  launch opens the default window. Unsaved text keeps its own recovery path.
- **Startup order and precedence.** After config and extension startup, the
  application restores automatic recovery snapshots, then restores the recorded
  windows: documents resolve live by document UUID first, then by opening their
  file. It then opens each queued launch request in its own window, as today, and
  focuses the last request window, or else the previously active restored window.
  Restored recovery documents that no window presents go to the first opened
  window, so text is never hidden. With no recorded windows and no requests,
  one empty window opens as today. A dock-icon reopen after launch opens an
  empty window and restores nothing.
- **Launch signal.** `ApplicationSnapshot.launched` and a one-time `launched`
  event tell extensions that startup windows and requests are committed.
  Extensions activate before any window exists. An extension restarted later
  sees `launched: true` in its snapshot.
- **Projects rebind.** `@knot/projects` defers startup suspension, capture
  reconciliation, and live tracking until `launched`. Active sessions whose
  root set matches a restored window stay active and bound; the rest are
  suspended as today. Records gain `layout` per window (session schema 2,
  upgraded on read: schema 1 windows have no layout). Live tracking re-captures
  on `viewStateChanged` and writes only changed records. Switching captures
  the source layouts and passes the destination's layouts. A session record with an
  unknown schema is preserved unchanged, shown with an error status, and
  never switched to. While any such record exists, capture reconciliation is
  skipped, so its captures are never released as orphans.

#### 10a. Layout capture and restore ⬜

- ⬜ Editor view state: read and apply anchor, caret, top position, and
  horizontal scroll on `EditorView`, with clamping and character-boundary
  snapping. This replaces the test-only presentation-state helpers where
  practical.
- ⬜ A `window_layout` module: capture a `Workbench` (with its editor views and
  native window bounds) into the versioned layout, and build a pruned,
  invariant-checked arrangement from a layout plus resolved documents and
  registered tools. Each tab kind recreates its presentation through one
  per-kind step (open document view, attach tool, start terminal), so a new
  kind adds a step rather than a special case. Pruning lives in one place
  shared by both restorers.
- ⬜ `workbench.captureLayout` handler and `WindowLayout` declarations in
  `knot.d.ts`. `replaceWindows` drops window reuse (`SessionWindow.target`):
  it creates every destination window with its layout and geometry in the same
  commit and closes every released window, stopping its terminals, and reports
  the per-window `layout` outcome. Update `@knot/projects` switching in the
  same step so the bundled package keeps working.
- ⬜ The `viewStateChanged` event: per-window debounce fed by editor view state,
  focus, dock-resize, and `observe_window_bounds` changes.
- **Validation:** unit tests for layout round-trip, pruning (unknown tools,
  unmatched documents, nested one-child splits, bad
  selection and focus), unsupported versions, offset clamping on shorter and
  multi-byte text, and display fitting as a pure function over display bounds.
  Product tests in `product/tests/sessions.rs` for `replaceWindows` with layouts:
  splits with two views of one document keeping independent view state, tools
  in a document group and in docks, hidden unnamed tools, terminal panes
  restored in place as fresh shells, released windows closing and stopping
  their terminals, an ignored layout falling back, and no tool-layout
  publish. Observation tests for event coalescing. `captureLayout` and
  `replaceWindows` layout cases in `scripts/extension-api/contract.test.ts`.

#### 10b. Native window hot exit and startup ⬜

- ⬜ Window session record: generalize the tool-layout store's single versioned
  row, debounce, and flush to hold a second record, or add a sibling store with
  the same mechanics. Flush it in `quit_after_storage` beside the tool layout,
  and capture windows before quit closes them.
- ⬜ Startup restoration in `finish_product_startup`, after recovery restore and
  before draining the launch gate. It reuses session-replacement staging and
  commit with no sources, presents leftover recovered documents, opens requests,
  and emits `launched`.
- ⬜ `launched` in the application snapshot and events; `knot.d.ts` and the
  [extension guide](../extension-api.md).
- **Validation:** product tests for the startup paths: restore of several
  multi-root windows with splits, shared views, docks, and dirty documents;
  restore plus queued file and folder requests, with the request window focused
  and no duplicated documents; a request racing restoration; a changed file
  clamping view state; a missing clean file skipped; a lost or unsupported record
  opening the default window; a consumed record after a simulated interrupted
  restore; config failure writing nothing; a contributed tool from a failed
  package pruned; terminal panes restored in place with fresh shells; a reopen
  after launch restoring
  nothing; and quit, restart, and quit again producing the same windows.

#### 10c. Projects restoration ⬜

- ⬜ `@knot/projects` session schema 2 with per-window layouts, a read-time
  upgrade from schema 1, and preserved unreadable records.
- ⬜ Defer startup suspension, reconciliation, and tracking to `launched`, and
  rebind restored windows. Track `viewStateChanged`. Switching captures and
  passes layouts.
- **Validation:** `tests/project_sessions.rs` and `tests/projects.rs` for a
  multi-root project with splits, shared views, docks, and dirty documents
  surviving switch away and back and restart; rebinding after restart without
  new windows or documents; an active project with no restored window suspended;
  a schema-1 record switching without layout; an unknown-schema record
  preserved with its captures; repeated switching and restart producing no
  duplicate windows or documents; and the extension failing at startup while
  native windows and recovery still restore.

#### Documentation and review gate

- ⬜ Update [workbench lifecycle](../architecture/workbench-and-lifecycle.md)
  (launch, session replacement with layouts, window hot exit, editor view state),
  [documents and persistence](../architecture/documents-and-persistence.md)
  where startup ordering changes, [extension host](../architecture/extension-host.md)
  observation events, `architecture.md` for application-owned window restoration,
  and the [extension guide](../extension-api.md#project-sessions). Add
  decisions for native window hot exit with projects rebinding, the shared
  versioned layout value, restore-and-open precedence, and restoring every tab
  kind by position. Revise the [session replacement
  contract](../decisions.md#session-replacement-contract) and [project
  sessions](../decisions.md#project-sessions) decisions where switching now
  closes and opens windows and stops released terminals. In
  [design](../design.md), replace the planned layout and startup restoration
  sentence with current behavior.
- **Review gate:** review the complete product workflow and API documentation:
  quit and relaunch with several project and blank windows, splits, docks,
  dirty documents, and a launch request; switching projects with layouts; and a
  changed display. Then mark the initiative complete. Preserve durable details
  in current docs and remove this plan in a separate commit; Git retains its
  execution history.

## Follow-up from the codebase health review

The [codebase health review](codebase-health-review.md) findings are scheduled
as follows. Remove the review once every item below has landed or moved into the
roadmap register.

| Finding | Where |
| --- | --- |
| Prototype removal, `Generated` removal, test-only fencing, symlink Save, Find stub, `ProductShell` split, hygiene | ✅ [Maintenance interlude](#maintenance-interlude--codebase-health-cleanup-) |
| Extension restart/disable escape hatch (P1-3) | ✅ Checkpoint 8: native Restart Extension |
| Single-root error removal (P2-6) | ✅ Checkpoint 9 |
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

Excluded: terminal process/content persistence, persistent undo/history,
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
