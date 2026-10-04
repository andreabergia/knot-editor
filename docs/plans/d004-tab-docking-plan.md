# D004: Tab dragging and docking

Status: planned. Design agreed; no checkpoint implemented.

Source: D004, promoted from the [deferred-work register](../roadmap.md).
Current boundaries: [architecture](../architecture.md),
[workbench lifecycle](../architecture/workbench-and-lifecycle.md), and
[workbench decisions](../decisions.md#workbench-layout-and-view-lifetime).

## Outcome and design

A user can drag an individual document or terminal tab to reorder it in its
pane, add it to another pane, create a pane by dropping on any of the four
edges, move it to another Knot window, or tear it out into a new window. The
center of a pane accepts the tab at the end of its tab order; its tab bar
accepts an insertion position. Drop previews show the destination and insertion
position. A drag that is cancelled or has no valid target changes nothing.

Each successful drop **moves** one tab, preserving its document editor view
state or its live terminal session and sole attached presentation. Moving does
not close a document, prompt to save it, or stop a terminal process. Source
and destination are revalidated by exact workbench, pane, tab, and surface
identities at commit. The transition updates both workbenches as one foreground
operation, so a stale target or failed window creation leaves the source
intact. After a move, the destination tab is active and focused; the source
selects a neighboring tab, collapses an empty pane, or follows the existing
empty-workbench replacement policy if its final tab moved away. Destination
workbenches allocate their own pane and tab identities, and old captured
command targets cannot retarget the moved tab.

`Workbench` owns tab order and split-tree transitions. The product shell owns
drag interaction, target feedback, cross-window coordination, native window
creation, and focus. Document and terminal registries retain their current
application ownership. A terminal view is handed off when its destination
requires a new attachment; the session remains registered and running.

## Exclusions and deferred decisions

- [D037](../roadmap.md): dragging whole panes or split branches and arbitrary
  split-tree reshaping.
- [D005](../roadmap.md): saving and restoring layouts and tab placement.
- [D006](../roadmap.md): dragging files into Knot or tabs out as OS files.
- No copy gesture or duplicate-tab drop. Existing explicit split commands keep
  their current new-view or new-session semantics.
- Broader platform product validation remains under its existing roadmap items;
  the initial interaction is reviewed on macOS.

## Checkpoints

### 1. Validate drag delivery across window boundaries

- Prove the gpui 0.2.2 event path for reorder, inter-pane, inter-window, and
  outside-window release on macOS, including cancellation. Its in-window
  `on_drag`/`on_drop` API is present, but cross-window and desktop release
  delivery must be established before choosing the interaction adapter.
- If gpui cannot deliver a required gesture, define the smallest native or
  framework adapter needed while keeping drop semantics in the product shell.
  Record the actual event behavior and selected path in this plan.
- Validate with a focused diagnostic and a manual drag matrix. Do not narrow
  the agreed window or tear-out outcome at this gate without review.
- **Review gate:** approve the event path and visible drag/drop affordance.

### 2. Add atomic tab relocation to the workbench

- Add typed transitions for reorder, move into a pane, and move into a new edge
  split. Preserve the tab payload and view state; collapse emptied panes and
  maintain active/focused identities. Support moves between distinct
  workbenches without treating them as protected closure.
- Coordinate cross-window moves in the application, including document view
  counts, terminal attachment, empty-workbench replacement, stale targets,
  and destination creation failure.
- Cover model invariants, same-pane index changes, all four split edges,
  cross-window document and terminal identity, dirty documents, cancelled or
  failed moves, and captured-command invalidation with focused automated tests.
- **Review gate:** inspect ownership and atomicity before adding drag UI.

### 3. Add in-window drag targets and feedback

- Make tab labels draggable; expose ordered tab-bar insertion targets, pane
  center targets, and four edge targets. Show a clear preview and active drop
  target, and apply the checkpoint 2 transition only on a valid drop.
- Test target selection and drop dispatch at the product boundary. Manually
  review reorder, center, edges, cancellation, focus, and pane collapse on
  macOS without UI automation.
- **Review gate:** approve the in-window interaction and feedback.

### 4. Complete cross-window moves and tear-out

- Connect drops into other Knot windows and outside-window tear-out to the
  selected event path. Create a destination window before removing the source;
  preserve its document editor or terminal session and focus the destination.
- Test destination failure, stale or closed windows, last-tab replacement,
  dirty document transfer without prompts, and terminal continuity. Manually
  review both window gestures and a live terminal transfer on macOS.
- Update the workbench architecture reference and decisions with validated
  behavior and rationale; mark completed checkpoints with ✅.
- **Review gate:** approve the end-to-end D004 slice and automated coverage.
