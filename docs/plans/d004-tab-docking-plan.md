# D004: Tab dragging and docking

Status: checkpoint 1 diagnostic includes source-window coordination; its five
automated tests pass. In-window interaction reviewed; native cross-window and
desktop-release validation of the adapter remain open.

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

#### Checkpoint 1 evidence and manual gate

- ✅ Added `examples/tab_drag_delivery.rs`, a standalone two-window probe using
  the pinned gpui 0.2.2 `on_drag`, `on_drop`, and typed hover feedback. It exposes
  insertion, center, and all four edge targets. It reports delivery only and
  does not mutate product tabs or create tear-out windows.
- ✅ Inspected the resolved gpui source: `app.rs` owns the application-wide
  `active_drag` and exposes `stop_active_drag`; `elements/div.rs` commits typed
  drops only when a mouse-up reaches a hovered target in that window;
  `window.rs` clears an unconsumed drag after mouse-up dispatch. The macOS
  `platform/mac/window.rs::handle_view_event` forwards native events through
  the receiving window and synthesizes repeated movement for pressed drags.
  These are source observations, **not proof of native cross-window delivery**.
- ✅ `cargo test --example tab_drag_delivery` passes (five tests). Probe validation
  checks target payloads, explicit Escape
  cancellation, an injected outside release, and an injected destination-window
  release. Injected events establish gpui dispatch behavior only; they cannot
  establish which window macOS delivers real events to.
- ✅ Initial macOS review on 2026-10-09: the user reported that the in-window
  interaction worked well, but cross-window dragging did not work. The native
  log confirms insertion 0/1, top/bottom drops, Escape cancellation, and a
  successful subsequent drag. During sequence 1, out-of-content movement
  (`x > 640`) continued to arrive at receiver 1; no destination-window drop
  occurred. This confirms that typed `on_drop` alone is insufficient.
- ✅ Extended the diagnostic with a foreground coordinator that maps source
  content positions into window coordinates, hit-tests registered destination
  targets, paints destination feedback, and handles cross-window releases in
  the source listener. In-window drops retain their typed gpui handler. Escape
  clears both states. An outside-all-window-frames release reports
  `TEAR_OUT_READY`; it does not create a window in this delivery probe.
  Additional tests cover actual source-listener dispatch into another target,
  cancellation, release-position revalidation, content-origin offsets, invalid
  space, and a missing destination.
- ⏳ Re-run native cross-window dragging in both directions, desktop release,
  and Escape after crossing, using the adapted diagnostic. Approve the
  coordinator path after those results. Checkpoints 2–4 remain open.

Run `cargo run --example tab_drag_delivery 2> /tmp/knot-tab-drag-delivery.log`
on macOS. Place the two windows side by side (or reposition them if needed).
Each gesture gets a `START` sequence; `MOVE` records its receiver, local
position, whether that position is within the receiver's content, pressed
button, and native content bounds. `UP` is logged in capture before a target
can consume it; `DROP` identifies an in-window receiver/target;
`PREVIEW` identifies the coordinated target and `COORDINATED_DROP` identifies
a cross-window drop committed from the receiving source listener.
`TEAR_OUT_READY` identifies a delivered release outside all window frames.
`NO_TARGET` records an
unconsumed release after dispatch. `CANCEL` reports whether Escape actually
cleared gpui's active drag. Hovered targets turn blue with a light border.
Retain the log and record actual receiver, feedback, and outcome for each row.

| Manual gesture | Evidence required | Result |
| --- | --- | --- |
| Tab to each insertion slot in its window | One matching `DROP`; correct highlight | ⏳ |
| Tab to center and each edge in its window | One matching `DROP`; correct highlight | ⏳ |
| Window 1 tab to Window 2 center/insert/edge | Source `MOVE`/`UP`, destination `COORDINATED_DROP`, visible feedback | ⏳ Adapter retest |
| Window 2 tab to Window 1 | Same evidence in the reverse direction | ⏳ |
| Tab to desktop; release | Delivered `UP`, `TEAR_OUT_READY`, no `DROP` | ⏳ Adapter retest |
| Tab to empty non-target space; release | `UP` and `NO_TARGET`, no `DROP` | ⏳ |
| Escape over an in-window target, then release | `CANCEL stopped=true`; no `DROP` | ⏳ |
| Escape after crossing to the other window or desktop | Cancellation receiver, `stopped=true`; no `DROP` | ⏳ |
| Drag after each cancel/invalid release | New `START` and exactly one valid `DROP`; no stuck preview | ⏳ |

The selected adapter candidate is the foreground product-shell coordinator,
using gpui's delivered source events without adding a native event monitor.
Native source release over another window and the desktop still need evidence;
if release is missing, a narrow macOS event-delivery adapter will also be
necessary. The diagnostic currently targets side-by-side windows on one
display; native geometry across displays requires validation before relying
on gpui window bounds for that case. Review must confirm the observed event
path and preview before product relocation work begins.

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
