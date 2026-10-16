# D049 — Dockable workbench

Status: ⬜ Planned; implementation has not started.

Source: checkpoint 5 of the [extension platform and projects plan](extension-platform-and-projects-plan.md).
Design discussion: 2026-10-16. Followed by the
[native extension UI plan](d049-native-extension-ui-plan.md).
Current boundaries: [architecture](../architecture.md),
[decisions](../decisions.md), [workbench](../architecture/workbench-and-lifecycle.md),
and [commands](../architecture/commands.md).

## Outcome

Users can arrange native tool frames alongside documents and terminals: drag
individual frames into tab groups or edge splits, resize them, hide them, and
reopen them. Sidebars are useful initial placements, not the only locations where
tools can live. Tools and documents may share a tab group.

This is the layout prerequisite for production extension contributions, not a
separate docking framework. Both focused plans must complete before D049 closes.

## Design

- Extend the existing workbench split tree, tab ownership, drag feedback, and
  relocation transitions. Use one pane/tab model for documents, terminals, and
  tools, rather than independent sidebar and editor layout engines.
- A tool frame supplies native content, title, focus/command participation, and
  a live identity. Use explicit supported content variants; do not introduce a
  general widget/plugin abstraction for arbitrary Rust or JavaScript views.
- Left/right sidebar and top/bottom placements are initial edge splits around
  the main content. Center placement uses a normal tab group. User moves take
  precedence over initial placement; refresh must not reposition a frame.
- Add draggable split dividers and window-local proportions. Keep size limits
  sufficient for usable panes; no constraint-solving layout system.
- Move the native resource explorer into the same frame infrastructure. Its
  filesystem model and protocol remain separate from extension providers.
- Hiding a tool removes its presentation from the layout without disposing its
  definition. Reopening uses its initial placement unless it remains in a live
  group. Exact hidden-slot restoration and durable layout records are deferred.
- Closing a tool never enters document Save/Don't Save/Cancel. Dirty document
  protection and terminal ownership retain their existing behavior. Removing
  an empty pane collapses its branch; tools must not accidentally keep an
  unpresented document alive or trigger whole-workbench replacement.
- Keep frame identity separate from pane/tab identity. Same-window movement
  preserves the frame and its presentation state; old exact presentation targets
  are revalidated using the established command rules.
- New tool frames remain within their product window in D049. Existing document
  and terminal cross-window transfer/tear-off behavior remains supported.

No compatibility shims: replace hardcoded shell placement and document/terminal
assumptions in touched paths directly. Keep ownership explicit and remove obsolete
special cases instead of retaining two ways to host the same frame.

## Checkpoints

Execute one checkpoint per implementation request and stop at its review gate.
Keep this plan updated with ✅ deliverables, evidence, and reviewed choices.
Split coherent changes into logical commits without conventional titles.

### 1. Native tools in the shared workbench ⬜

Add tool presentations to the existing workbench and move the native resource
explorer into that layout. Provide initial edge/center placement, tool activation,
hide/reopen, and command/focus participation through the existing native command
path. Audit document-only assumptions in snapshots, last-document closure,
workbench replacement, and application observation.

Use the resource explorer as a real end-to-end tool; avoid adding a temporary
parallel production registry for the old outline slot. The extension registry
lands in the follow-up plan.

**Validation:** model tests for mixed tabs, tool hide/reopen and empty-pane
collapse; gpui integration tests for resource opening from a docked explorer,
focus/Copy routing, application snapshots with non-document tools, and protected
document/terminal closure with tools present. A tool-only window must behave
deliberately and retain no closed document.

**Review gate:** inspect a real sidebar and a mixed document/tool tab group.
Approve the native ownership and hide/reopen behavior before extending dragging.

### 2. Frame relocation and resizing ⬜

Extend individual-tab dragging to tool frames: reorder, join a tab group, and
create four-edge splits. Add whole-workbench edge drop targets where needed to
place a sidebar or bottom tool group without splitting only the focused editor
pane. Reuse the existing drag coordinator and previews. Add resizable dividers
to the shared split model.

Tool drops outside their product window cancel without creating a floating
window. Do not alter existing document/terminal tear-off semantics. Moving a
frame retains its native state and updates focus and captured-target validity.

**Validation:** model tests for mixed relocation and divider state; programmatic
gpui event tests for group insertion, pane/workbench edge drops, resizing,
Escape/invalid drops, closure during dragging, and focus after moving. Retain
existing document/terminal relocation and cross-window regression coverage.

**Review gate:** manually move the explorer from a sidebar to a bottom split,
into an editor tab group, and back; resize nested splits. Inspect whether the
shared model remains simple before adding extension ownership.

### 3. Layout cleanup and handoff ⬜

Remove superseded fixed sidebar rendering and redundant shell focus/geometry
paths. Document the shared layout ownership and runtime constraints. Record
validated rationale in decisions and link the implementation evidence here.

**Validation:** run the relevant workbench, product, command, observation, and
relocation tests, then the library suite and repository-required checks. Run
`cargo fmt` once at the end of Rust work before committing. Manual review uses
the product directly; no UI automation tools are required.

**Review gate:** approve the native docking foundation and the concrete tool
identity/attachment contract consumed by the next plan. Do not mark all of D049
complete at this gate.

## Exclusions and choices deferred to review

Floating tool windows and cross-window tool transfer, whole-group dragging or
arbitrary branch reshaping (D037), auto-hide flyouts, persisted docking layouts
and startup restoration (D005), and general widget UI/WebViews are excluded.
This does not restrict existing document/terminal window operations.

Choose exact placement ordering, divider limits, reopen commands, and Rust types
at checkpoint 1/2 review. Do not publish a new layout serialization API. Native
resource-tree improvements beyond making its frame movable remain D034.
