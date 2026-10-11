# D004: Tab dragging and docking

Status: ✅ Complete. All four checkpoints and their review gates passed.
Final macOS acceptance: 2026-10-11 ("ok works really well").

Source: D004, promoted from the [deferred-work register](../../roadmap.md).
Current boundaries: [architecture](../../architecture.md),
[workbench lifecycle](../../architecture/workbench-and-lifecycle.md), and
[workbench decisions](../../decisions.md#workbench-layout-and-view-lifetime).

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

- [D037](../../roadmap.md): dragging whole panes or split branches and arbitrary
  split-tree reshaping.
- [D005](../../roadmap.md): saving and restoring layouts and tab placement.
- [D006](../../roadmap.md): dragging files into Knot or tabs out as OS files.
- No copy gesture or duplicate-tab drop. Existing explicit split commands keep
  their current new-view or new-session semantics.
- Broader platform product validation remains under its existing roadmap items;
  the initial interaction is reviewed on macOS.

## Checkpoints

### ✅ 1. Validate drag delivery across window boundaries

- ✅ Prove the gpui 0.2.2 event path for reorder, inter-pane, inter-window, and
  outside-window release on macOS, including cancellation. Its in-window
  `on_drag`/`on_drop` API is present, but cross-window and desktop release
  delivery must be established before choosing the interaction adapter.
- ✅ If gpui cannot deliver a required gesture, define the smallest native or
  framework adapter needed while keeping drop semantics in the product shell.
  Record the actual event behavior and selected path in this plan.
- ✅ Validate with a focused diagnostic and a manual drag matrix. Do not narrow
  the agreed window or tear-out outcome at this gate without review.
- ✅ **Review gate:** approve the event path and visible drag/drop affordance.

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
- ✅ Native adapter retest: the user confirmed cross-window dragging is
  possible. The log contains source-window releases and coordinated drops
  from window 1 to window 2 (top and left), and from window 2 to window 1
  (center). The user identified a stuck drag label in the destination:
  gpui positions its shared preview using each window's own mouse position,
  while only the source receives continued native movement.
- ✅ The preview now uses the coordinator's latest screen pointer, compensates
  for each window's stale local mouse position, and renders its label only in
  the content under that pointer. Every pointer change requests repaint,
  including movement within one unchanged target. A rendered-bounds regression
  test proves the destination label retains the grab offset and follows two
  source-delivered positions within the same target. Drop and cancellation
  clear the shared pointer and feedback.
- ✅ The user approved the corrected diagnostic on 2026-10-09 ("perfect now")
  following the requested preview-tracking, Escape, and desktop-release retest.
  The coordinator is selected for product integration. The retained log confirms
  center drops in both directions; desktop/cancellation acceptance also relies
  on the user's manual review. Checkpoints 2–4 remain open.

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
| Tab to each insertion slot in its window | One matching `DROP`; correct highlight | ✅ User review |
| Tab to center and each edge in its window | One matching `DROP`; correct highlight | ✅ User review |
| Window 1 tab to Window 2 center/insert/edge | Source `MOVE`/`UP`, destination `COORDINATED_DROP`, visible feedback | ✅ Drops logged; corrected preview approved |
| Window 2 tab to Window 1 | Same evidence in the reverse direction | ✅ Drop logged; corrected preview approved |
| Tab to desktop; release | Delivered `UP`, `TEAR_OUT_READY`, no `DROP` | ✅ User review |
| Tab to empty non-target space; release | `UP` and `NO_TARGET`, no `DROP` | ✅ User review |
| Escape over an in-window target, then release | `CANCEL stopped=true`; no `DROP` | ✅ User review |
| Escape after crossing to the other window or desktop | Cancellation receiver, `stopped=true`; no `DROP` | ✅ User review |
| Drag after each cancel/invalid release | New `START` and exactly one valid `DROP`; no stuck preview | ✅ User review |

The selected adapter candidate is the foreground product-shell coordinator,
using gpui's delivered source events without adding a native event monitor.
Native source release over another window is confirmed in both directions;
desktop release is accepted through the user's manual review. No native event
monitor is required for the selected path. The diagnostic currently
targets side-by-side windows on one
display; native geometry across displays requires validation before relying
on gpui window bounds for that case. The event path and corrected preview passed the checkpoint 1 review gate.

### ✅ 2. Add atomic tab relocation to the workbench

- ✅ Add typed transitions for reorder, move into a pane, and move into a new edge
  split. Preserve the tab payload and view state; collapse emptied panes and
  maintain active/focused identities. Support moves between distinct
  workbenches without treating them as protected closure.
- ✅ Coordinate cross-window moves in the application, including document view
  counts, terminal attachment, empty-workbench replacement, stale targets,
  and destination creation failure.
- ✅ Cover model invariants, same-pane index changes, all four split edges,
  cross-window document and terminal identity, dirty documents, cancelled or
  failed moves, and captured-command invalidation with focused automated tests.
- ✅ **Review gate:** ownership and atomicity approved by the user on
  2026-10-10 ("ok looks good to me").

Checkpoint 2 model evidence: seven focused tests pass, including all twenty
same-pane source/insertion combinations, all four edges, pane collapse,
presentation preservation, exact stale captures, and cross-workbench payload
identity. Six application-boundary tests cover dirty last-view transfer and replacement,
shared-document view counts, abandoned inactive-tab captures, failed creation,
stale presentations, closed destinations, source replacement during creation,
terminal attachment handoff into an existing window, and final-terminal source
replacement. The existing terminal command now uses the common coordinator;
its continuity, detached-view resize, process-exit, failure, and protected
closure tests remain green.

✅ Validation: `cargo test --all-targets --quiet` passes (470 library tests plus
5 drag-probe tests); `cargo check --quiet` passes; Rust was formatted with
`cargo fmt`. Architecture and decision references describe the relocation
boundary. Review the workbench relocation model and product coordinator before
checkpoint 3.

### ✅ 3. Add in-window drag targets and feedback

- ✅ Make tab labels draggable; expose ordered tab-bar insertion targets, pane
  center targets, and four edge targets. Show a clear preview and active drop
  target, and apply the checkpoint 2 transition only on a valid drop.
- ✅ Test target selection and drop dispatch at the product boundary.
- ✅ Manual in-window review approved by the user on 2026-10-11
  ("within a window, it works really well").
- ✅ **Review gate:** in-window interaction and feedback approved.

Checkpoint 3 uses shell-owned painted geometry and the selected source-event
coordinator for in-window release dispatch. Tab-bar midpoints choose ordered
insertion slots; the remaining pane body uses a center append target and the
nearest normalized edge within its outer 22% band. The preview paints a line,
full body, or destination half-body alongside a floating tab title. Captured
source and rendered destination identities pass through checkpoint 2's exact
validation; pointer movement never changes model order or activation. Escape
runs on the root capture path so focused editors and terminals cannot consume
it before cancellation.

Four product-boundary gpui tests exercise actual mouse/key dispatch: reorder
with a different release position, all four split previews and transitions,
center insertion with source collapse and editor focus, Escape/outside/toolbar
release and stale-source rejection, and live terminal view/session/attachment
preservation with destination focus. These tests do not establish native macOS
interaction quality.

✅ Validation: `cargo test --all-targets --quiet` passes (474 library tests
plus 5 drag-probe tests); `cargo check --quiet` passes; Rust was formatted
with `cargo fmt`.

Run `cargo run --bin knot` and check:

- reorder before the first tab, between tabs, and after the last tab;
- center and all four edge drops, including an emptied source pane;
- floating label, insertion line, and destination region tracking;
- Escape with editor or terminal focus, and release over the toolbar;
- destination focus and unchanged source state after cancellation.

At this gate the user also reported that dragging between windows did not
yet work; checkpoint 4 connected that interaction to the validated event path.

### ✅ 4. Complete cross-window moves and tear-out

- ✅ Connect drops into other Knot windows and outside-window tear-out to the
  selected event path. Create a destination window before removing the source;
  preserve its document editor or terminal session and focus the destination.
- ✅ Test destination failure, stale or closed windows, last-tab replacement,
  dirty document transfer without prompts, and terminal continuity.
- ✅ Manual macOS review accepted by the user on 2026-10-11
  ("ok works really well").
- ✅ Update the workbench architecture reference and decisions with validated
  behavior and rationale; mark completed checkpoints with ✅.
- ✅ **Review gate:** end-to-end D004 slice and automated coverage accepted.

#### Checkpoint 4 implementation and review evidence

- ✅ Product windows share a transient foreground drag coordinator. It retains
  weak endpoints and painted pane/tab-bar geometry, reads current native bounds
  and platform stacking order, and converts events from either receiver into
  a screen pointer. Release-position hit testing selects the final target;
  exact endpoint checks precede a deferred foreground transfer.
- ✅ The destination paints insertion or docking feedback. The floating title
  follows the shared pointer with the diagnostic's nested preview correction;
  updates within one unchanged target still repaint. Escape in either window
  clears all state, and source closure cancels the shared drag. A closed
  destination cannot consume a captured transfer.
- ✅ A release outside Knot frames creates a destination near the pointer,
  constrained to its display. Window creation occurs before source mutation;
  existing toolbar/title-bar/sidebar space, non-product windows, and product
  overlays reject drops. Failure or stale captures preserve the source.
- ✅ Destination layout retains deliberate editor scrolling through relocation,
  including a caret outside the scrolled viewport; normal viewport clamping
  and pending edit-driven caret reveals still apply. The dirty last-view test
  checks cursor, selection, scroll, view entity, view counts, command invalidation,
  and source replacement after actual dispatched tear-out.
- ✅ Nine additional product-boundary tests cover source-delivered cross-window
  insertion, center, and all four edges; bidirectional moves; destination-delivered
  release; compensated label bounds and movement; Escape across windows and on
  desktop; failed tear-out creation; stale presentation and closed destinations;
  source closure cleanup, including final-window closure and reopening;
  overlays and title-bar occlusion; negative display origins; dirty last-view
  replacement; and two live terminal attachment handoffs
  through an existing window and desktop tear-out. Checkpoint 2 retains its
  failed/stale creation and terminal lifecycle regression coverage.

✅ Validation: `cargo test --all-targets --quiet` passes (483 library tests
plus 5 drag-probe tests); `cargo check --quiet` passes; Rust was formatted
with `cargo fmt`.

Run `cargo run --bin knot`, open a second window with **File → New Window**, and
position them side by side. Review without UI automation:

| Gesture | Expected result | Result |
| --- | --- | --- |
| Document to other window's bar, center, and each edge | Accurate preview; one moved view; destination focused | ✅ User acceptance |
| Reverse window direction; move label within one target | Label follows pointer; one successful drop | ✅ User acceptance |
| Dirty final document tab to desktop | New focused window; cursor/selection/scroll retained; source replaced with untitled; no save prompt | ✅ User acceptance |
| Live terminal to other window, then desktop | Same process and grid; input/resize works; one attached presentation | ✅ User acceptance |
| Escape over other window or desktop, then drag again | No mutation/new window; no stuck preview; next drag succeeds | ✅ User acceptance |
| Release over toolbar or title bar | No mutation/new window; feedback cleared | ✅ User acceptance |
| Overlapping windows and, if available, different displays | Foremost window receives drop; geometry and label remain correct | ✅ User acceptance |

The automated bounds checks establish coordinate math and gpui dispatch.
The user accepted the final macOS review on 2026-10-11 ("ok works really well")
after the requested cross-window, desktop, cancellation, and terminal checks.
Native acceptance relies on that overall report, without separately logged
gesture results. The display configuration was not reported; synthetic tests
cover negative origins and distinct content offsets without claiming a native
multi-display test. All required implementation, documentation, automated
coverage, and review gates are complete.
