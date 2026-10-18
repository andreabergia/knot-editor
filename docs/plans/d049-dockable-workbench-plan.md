# D049 — Docking and automatic tool-layout memory

Status: 🚧 Checkpoint 3 implemented and automated checks passed; native review pending.

Source: checkpoint 5 of the [extension platform and projects plan](extension-platform-and-projects-plan.md).
Design discussion: 2026-10-16. Followed by the
[native extension UI plan](d049-native-extension-ui-plan.md).
Current boundaries: [architecture](../architecture.md),
[decisions](../decisions.md), [workbench](../architecture/workbench-and-lifecycle.md),
and [commands](../architecture/commands.md).

## Outcome

Users can arrange native tool frames alongside documents and terminals: drag
individual frames into tab groups or edge splits, resize them, hide them, and
reopen them. Familiar sidebar and bottom docks provide tool selectors, toggle
commands, and draggable dividers. Tools and documents may also share a tab group.
Tool arrangements are remembered automatically: fresh windows inherit the last
arrangement the user customized, while already-open windows remain independent.

This is the docking and layout-memory foundation for production extension
contributions. Both focused plans must complete before D049 closes.

## Design

- Design for recognizable editor behavior, not preservation of the prototype.
  Replace the existing layout model where necessary. Reuse split, tab, drag, or
  relocation code only where it fits; no compatibility layer or parallel legacy
  path is required. Prefer a small coherent model over a generic docking framework.
- A tool frame supplies native content, title, focus/command participation, and
  a live identity. Use explicit supported content variants; do not introduce a
  general widget/plugin abstraction for arbitrary Rust or JavaScript views.
- Support recognizable left/right sidebars and bottom docks, with tool selection
  and toggles. Frames can also join document tab groups or create edge splits.
  User placement takes precedence over contributed defaults; refreshing content
  must not reposition a frame. Docks must behave as tools users can hide/reopen,
  not merely narrow editor panes with different content.
- Add draggable dividers and window-local sizes. Keep size limits
  sufficient for usable panes; no constraint-solving layout system.
- Move the native resource explorer into the same frame infrastructure. Its
  filesystem model and protocol remain separate from extension providers.
- Hiding a tool removes it from visible content without disposing its definition.
  Remember its location and size so reopening returns it there. If its former
  live tab group has disappeared, use the remembered dock or the normal document
  group as appropriate; do not recreate empty historical document groups.
- Closing a tool never enters document Save/Don't Save/Cancel. Dirty document
  protection and terminal ownership retain their existing behavior. Removing
  an empty pane collapses its branch; tools must not accidentally keep an
  unpresented document alive or trigger whole-workbench replacement.
- Keep frame identity separate from pane/tab identity. Same-window movement
  preserves the frame and its presentation state; old exact presentation targets
  are revalidated using the established command rules.
- New tool frames remain within their product window in D049. Existing document
  and terminal cross-window transfer/tear-off behavior remains supported.

### Automatic tool-layout memory

Persist tools only: stable namespaced tool IDs, dock location, tool grouping and
order, sizes, visibility, selected tool, and hidden tools' remembered locations.
Do not serialize document tabs, document split structure, terminal contents, or
empty document placeholders into tool-layout records. A tool placed in a document
group is remembered as belonging in the normal document tab group of a fresh
window; the source window's document splits are not reproduced.

- A fresh window uses the last tool arrangement the user customized. Successful
  user moves, resizes, and show/hide changes update this default automatically.
  Coalesce divider dragging; do not write on every pointer movement.
- Already-open windows retain their own live arrangements. Merely activating a
  window, restoring a layout, or applying defaults does not publish a new default.
  An extension automatically showing a debugger affects its target window only.
- Reopening a hidden tool restores its remembered placement and size. Provide
  Reset Layout to return to product/contribution defaults; no explicit Save
  Layout step, profiles, or layered preference system is needed.
- Full session restoration later restores that session's exact mixed layout
  instead of the fresh-window default. Document/session restoration remains D005;
  D049 must neither invent a second session identity system nor require it to
  remember tools across application restarts.

The native application owns these records, separately from extension storage.
Start with one versioned tool-layout snapshot in application-owned SQLite, written
off the foreground through a small native boundary. Choose concrete schema and
write coalescing at checkpoint 3. No event log, custom storage service, or general
public layout serialization API is needed. Stable contribution IDs reconnect
saved preferences to fresh live instances; live handles are never persisted.
Unavailable tools do not create blank frames, and temporary unload/failure does
not erase their remembered placement. Startup registration and restore must not
overwrite saved preferences with incomplete defaults.

## Checkpoints

Execute one checkpoint per implementation request and stop at its review gate.
Keep this plan updated with ✅ deliverables, evidence, and reviewed choices.
Split coherent changes into logical commits without conventional titles.

### 1. Native tools in the shared workbench 🚧

Establish the native docking model and move the resource explorer into it.
Provide recognizable sidebar/bottom docks, tool selectors and toggle commands,
mixed document/tool tab groups, initial placement, activation, hide/reopen with
remembered position/size, and command/focus participation. Audit document-only
assumptions in snapshots, last-document closure, workbench replacement, and
application observation. Change the current implementation as needed.

Use the resource explorer as a real end-to-end tool; avoid adding a temporary
parallel production registry for the old outline slot. The extension registry
lands in the follow-up plan.

**Validation:** model tests for mixed tabs, tool hide/reopen and empty-pane
collapse; gpui integration tests for resource opening from a docked explorer,
focus/Copy routing, application snapshots with non-document tools, and protected
document/terminal closure with tools present. A tool-only window must behave
deliberately and retain no closed document.

Implementation evidence:

- ✅ Workbench-owned native tool definitions and live identities, typed tool tabs,
  dock groups and sizes, mixed groups, and hide/reopen without disposal. The
  explorer uses this infrastructure; its filesystem protocol remains separate.
- ✅ Left/right/bottom dock selection and toggle controls; palette commands move
  the explorer to each dock or the normal editor tabs. Explorer defaults to the
  left sidebar on first workspace use, or when explicitly opened without a root.
  Refreshing/clearing roots retains its frame and user placement/visibility.
- ✅ Tool focus/Copy routing, exact presentation capture, tool snapshots with
  null document/buffer, and protected closure/replacement paths handle tools.
  Closing the last document with a visible tool leaves a tool-only window;
  closing the last document with hidden tools preserves their definitions while
  creating the ordinary untitled replacement. Tools cannot transfer windows.
- ✅ `cargo test --all-targets --locked` passed (648 library tests plus the
  binary, facade integration, and drag-delivery example targets), as did
  `cargo clippy --all-targets --locked -- -D warnings`,
  `scripts/check-extension-api.sh`, and `git diff --check`. Ran `cargo fmt`.
  Tests live in `src/app/workbench/tools.rs` and
  `src/app/product/tests/tools.rs` alongside existing closure, observation,
  resource, and relocation regression tests. Final-document closure explicitly
  verifies focus transfers to the surviving tool.
- ✅ Built `target/debug/Knot.app` for native review. No native interaction
  has been claimed as verified; no UI automation was used.
- ✅ Fixed native review regression: docks inherited a zero flex basis and
  collapsed despite their explicit size. Flexible editor branches and fixed
  dock branches now choose sizing independently. A rendered gpui regression
  reproduced the zero-width sidebar before the fix, then verifies left/right
  widths, bottom height, hide/reopen geometry, and opening a file by clicking
  a visible explorer row. Native review remains pending on the rebuilt app.
- ⏳ Native review pending. Use **Toggle Explorer**, **Move Explorer to Editor
  Tabs**, **Move Explorer to Left/Right Sidebar**, **Move Explorer to Bottom
  Dock**, and dock toggle commands from the palette. Inspect hide/reopen and
  resource opening. Divider dragging and frame dragging remain checkpoint 2.

**Review gate:** inspect a real sidebar and a mixed document/tool tab group.
Approve familiar dock interaction, native ownership, and remembered hide/reopen
behavior before extending dragging.

### 2. Frame relocation and resizing ✅

Implement individual-frame dragging: reorder, join a tab group, and
create four-edge splits. Add whole-workbench edge drop targets where needed to
place a sidebar or bottom tool group without splitting only the focused editor
pane. Provide clear drag previews and resizable dividers; reuse existing code
where useful, without constraining the new model to it.

Tool drops outside their product window cancel without creating a floating
window. Do not alter existing document/terminal tear-off semantics. Moving a
frame retains its native state and updates focus and captured-target validity.

**Validation:** model tests for mixed relocation and divider state; programmatic
gpui event tests for group insertion, pane/workbench edge drops, resizing,
Escape/invalid drops, closure during dragging, and focus after moving. Retain
existing document/terminal relocation and cross-window regression coverage.

Implementation evidence:

- ✅ Tool tabs drag like any tab: reorder, join groups, and pane-edge
  four-way splits reuse the existing relocation transitions, preserving frame
  presentation identity and focusing the moved tool. Workbench left/right/bottom
  edge strips (20px) produce dock previews and move to `TabMoveDestination::Dock`,
  which recreates the dock around the whole layout; the top edge keeps pane-local
  splits. Only tool drags consult edge strips, so document/terminal split and
  tear-off semantics are unchanged.
- ✅ Tear-out and cross-window transfer of tools return `Unavailable` before any
  window is created; the source model and captures stay live.
- ✅ Dock-adjacent dividers (9px hit area, `dock-divider-{index}` selectors) drag
  to resize window-local dock sizes clamped to 80–480px; other splits stay equal.
  Escape reverts an in-progress resize. Resizing applies incrementally and does
  not use GPUI active drags, so tab-drag previews are unaffected.
- ✅ Model tests in `src/app/workbench/tools.rs` cover tool moves across groups,
  pane-edge splits, dock drops with empty-pane collapse, capture invalidation,
  and dock-size clamping across hide/reopen. Integration tests in
  `src/app/product/tab_drag/tests.rs` cover group insertion, pane/workbench edge
  drops with preview assertions, divider drag and Escape revert, mid-drag hide,
  invalid drops, and focus after moving; `src/app/product/relocate/tests.rs`
  proves tool tear-out creates no window. Existing document/terminal relocation
  and cross-window suites still pass.
- ✅ `cargo test --all-targets --locked` passed (654 library tests), as did
  `cargo clippy --all-targets --locked -- -D warnings`,
  `scripts/check-extension-api.sh`, and `git diff --check`. Ran `cargo fmt`.
- ⏳ Native review pending. Drag the Explorer tab into an editor group, to pane
  edges for splits, and to window left/right/bottom edges for docks; drag dock
  dividers. Frame dragging across windows must cancel; divider drags clamp.

**Review gate:** manually move the explorer from a sidebar to a bottom split,
into an editor tab group, and back; resize nested splits. Inspect whether the
shared model remains simple before adding extension ownership.

### 3. Automatic tool-layout persistence 🚧

Implement the tool-only snapshot, native application storage, automatic writes,
fresh-window restoration, and Reset Layout. Preserve remembered hidden tools;
flatten tool placement in document groups to the normal fresh-window document
group, without persisting document structure or creating placeholders. Keep
user changes distinct from restore, activation, and extension-driven presentation.

**Validation:** snapshot round trips and real database reopen across application
runs; fresh windows inherit the last user-customized arrangement; existing windows
do not change; focusing an older window does not overwrite defaults; hidden tools
return to their size/location; automatic tool activation does not alter defaults;
mixed document groups produce no inherited document splits or placeholders;
missing tools, Reset Layout, write failure, and graceful shutdown with a pending
write. Tests must prove document/terminal state is absent from tool records.

Implementation evidence:

- ✅ Tool-only snapshot in `src/app/tool_layout.rs`: stable namespaced tool IDs
  (`knot.explorer`), dock/document-group location, order, sizes, visibility,
  per-dock selection, and hidden tools' remembered locations. Visible tools in
  ordinary groups and vanished panes flatten to the document group. A visible
  dock reports its active tab as its selection; hidden docks keep their
  remembered selection. Live `ToolFrameId` handles, `PaneId`s, documents, and
  terminals never enter the record.
- ✅ One versioned row in application-owned SQLite
  (`tool-layout.sqlite3` beside extension storage, `PRAGMA application_id`,
  `STRICT` table with `json_valid` payload, single `state = 0` row). Version or
  payload mismatch, corrupt files, and missing files all fall back to product
  defaults without touching the filesystem on read.
- ✅ Small native boundary: `ToolLayoutStore` caches the published snapshot
  synchronously so fresh windows inherit immediately; writes go off the
  foreground with 300ms trailing-edge debounce, so divider dragging coalesces
  into one write and identical publishes skip storage entirely. `flush()` forces
  a pending write; the quit path awaits it after extension-storage drain, so a
  pending customization survives restart. Write failure keeps the live cache,
  records the error, and stays pending for retry.
- ✅ Fresh-window restore applies sizes, placements, order, visibility, and
  selection onto registered tools at explorer registration; unknown stored
  tools are skipped without blank frames and docks already in stored order are
  left untouched. `workbench.reset-layout` restores product defaults and
  publishes the reset.
- ✅ Publish points are user-only: explorer/dock toggle and move commands,
  the hide button, same-window tool drag relocation, and divider release
  (Escape revert publishes nothing). Workspace-open auto-show, restore,
  window activation, and document splits never publish.
- ✅ `cargo test --all-targets --locked` passed (667 library tests), as did
  `cargo clippy --all-targets --locked -- -D warnings`,
  `scripts/check-extension-api.sh`, and `git diff --check`. Ran `cargo fmt`.
  Tests live in `src/app/tool_layout.rs` (reopen durability, coalescing,
  fallback, write failure), `src/app/workbench/tools.rs` (tool-only content,
  flattening, round trip), and `src/app/product/tests/tool_layout.rs`
  (fresh-window inheritance, existing-window independence, activation and
  auto-show publishing nothing, hidden-tool restore, no inherited splits,
  Reset Layout, unknown-tool restore).
- ✅ Built `target/debug/Knot.app` for native review. No native interaction
  has been claimed as verified; no UI automation was used.
- ⏳ Native review pending. Customize the explorer (move to each dock and
  editor tabs, resize dividers, hide/reopen), open another window, quit, and
  restart the product. Inspect the restored arrangement and Reset Layout from
  the palette.

**Review gate:** customize the explorer, hide/reopen it, open another window, and
restart the product. Inspect the restored experience and stored tool-only data.
Approve automatic memory before connecting extension definitions to it.

### 4. Layout cleanup and handoff ⬜

Remove superseded fixed sidebar rendering and redundant shell focus/geometry
paths. Document the shared layout ownership and runtime constraints. Record
validated rationale in decisions and link the implementation evidence here.

**Validation:** run the relevant workbench, product, command, observation, and
relocation tests, then the library suite and repository-required checks. Run
`cargo fmt` once at the end of Rust work before committing. Manual review uses
the product directly; no UI automation tools are required.

**Review gate:** approve docking, automatic tool-layout memory, and the native
tool identity/attachment contract consumed by the next plan. Do not mark all of
D049 complete at this gate.

## Exclusions and choices deferred to review

Floating tool windows and cross-window tool transfer, whole-group dragging or
arbitrary branch reshaping (D037), auto-hide flyouts, full document/session layout
and startup restoration (D005), and general widget UI/WebViews are excluded.
This does not restrict existing document/terminal window operations.

Choose exact placement ordering, divider limits, reopen commands, and Rust types
at checkpoint 1/2 review, and storage details at checkpoint 3. These choices must
preserve the agreed behavior, not reopen its scope. Native
resource-tree improvements beyond making its frame movable remain D034.
