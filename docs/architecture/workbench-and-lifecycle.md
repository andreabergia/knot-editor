# Workbench and view lifecycle

Part of the [architecture](../architecture.md). [Decisions](../decisions.md)
records rationale; deferred work lives in the [roadmap](../roadmap.md).

## Product application launch

The macOS product is packaged as `Knot.app` for native activation, file-open,
and reopen callbacks. CLI and platform requests join a launch queue while
[personal config and extensions](extension-host.md#package-admission-and-startup)
start. Successful startup opens queued product windows, or one empty window,
then activates the app. Fatal config failure discards the queue and opens a
focused diagnostic window with Copy and Quit; product windows remain unavailable
for that launch.

## Editor views

Each `EditorView` owns cursor, selection, scroll, focus, IME, rendering policy,
and its completion controller/surface. Multiple views share a model while
retaining independent presentation state. Completion ownership is detailed in
[semantic UI](semantic-ui.md#completion-ownership-and-aggregation).

Movement and deletion respect grapheme boundaries; word navigation uses Unicode
word boundaries and vertical movement retains a shaped x coordinate. Text,
selection, caret, hit testing, and IME bounds share one shaping projection and
horizontal offset. Editing/navigation reveal the caret; wheel scrolling may move
away from it. Clipboard operations use source text; expanded tabs are only a
rendering projection. UTF-16 platform input is converted to UTF-8 model edits,
with a synchronous projection refresh before resolving the resulting caret.
Other views update through their persistent positions.

## Workbench and protected closure

`Workbench` owns a window-local binary split tree, stable pane/tab identities,
ordered tabs, and focused pane. A document tab strongly owns one `EditorView`
of an application-owned document. A terminal tab owns the sole attached view of
an application-owned session, with no document model. A tool tab attaches a
workbench-owned native frame whose definition and presentation survive hiding.
Left/right sidebars and bottom docks are identified groups in the same split
tree, with window-local sizes and selectors. Tools can also share ordinary
document groups. Splitting a document
creates another view; splitting a terminal creates another session. Empty panes
collapse; final-tab closure returns an empty-workbench transition to the shell.

One application-level coordinator protects tab, window, and quit closure. It
captures exact window, workbench, tab, surface, and presentation identities,
counts live document views across windows, and deduplicates dirty documents
losing their final view. Native Save / Don't Save / Cancel prompts run serially;
Save reuses the ordinary persistence path.

Presentation mutation waits until all decisions succeed, then revalidates the
capture, view counts, and approved content revisions. Cancellation, persistence
failure, racing edits, stale targets, and concurrent closure leave presentations
intact. An already accepted save remains committed if closure later cancels.
Terminal sessions stop only after the scope transition succeeds. Native close
requests are vetoed until authorized; Quit uses the same flow.

Workbench transitions remain independent of dialogs and document removal. A weak
shell-wide registry counts views in live windows without retaining windows or
workbenches. Final application document removal explicitly closes its model,
invalidating transport access even if a stale view retains the entity. Nonfinal
view closure leaves the shared document open. An empty surviving product
workbench receives a new untitled document while retaining hidden tool
definitions. A workbench with visible tools can remain tool-only after its final
document closes; closed documents are removed normally. Closing a tool hides
its attachment without entering document confirmation. Hiding the last visible
tool can leave an empty window; native window closure needs no document prompt.

Native commands and extension APIs adapt the coordinator's semantic result.
Extension closure additionally revalidates caller lifecycle/cancellation before
save and close admission. Completion follows the foreground transition without
awaiting observation listeners.

## Tab relocation

`Workbench` owns typed reorder, insertion, and edge-split transitions. The product
shell coordinates transfers across workbenches on the foreground. Captures
include pane, tab, surface, and exact presentation; destinations retain a live
insertion/split anchor. Both endpoints and document/session ownership are checked
before mutation. Capturing or abandoning a move changes neither ownership nor
focus.

Same-pane reorder preserves tab identity. Moving between panes preserves the
payload, collapses empty source panes, and allocates destination-local tab/pane
identities. Editors retain their view entity and scroll state; pending caret
reveals still apply. Terminal cross-window moves hand off the sole attachment to
a destination view of the same session; same-window moves retain their view.
Replacement or pane/window transfer invalidates old exact command targets.

Native tools stay in their owning product window.

Transfers do not enter protected closure or remove documents/sessions. An emptied
source receives ordinary untitled replacement. New-window transfer creates and
validates an empty destination before removing the source; failure preserves the
source and removes an unused new destination. The destination activates and
focuses the moved tab. On macOS a new terminal window is laid out before showing.

A transient application-wide drag coordinator holds weak endpoints and painted
window-local geometry. macOS delivers pressed drags to the originating window,
so events map to screen coordinates for hit testing against the foremost Knot
window and for preview positioning. Transfers run after input dispatch so both
windows are available for revalidation. Source closure or Escape cancels drag
state; destination closure removes its geometry. No coordinator owns window
lifetime.

Tab bars accept insertion and pane bodies accept append or edge splits. Invalid
space inside Knot frames rejects a drop; release outside all Knot frames creates
a window near the pointer. Preview rendering, hit-test thresholds, and coordinate
fallbacks live in the [relocation code](../../src/app/product/tab_drag.rs).

## Native resource tree

`WorkspaceTree` owns cached native filesystem presentation state. It emits
normalized resource requests and workspace generations to the shell, independently
of the extension `TreeView` provider protocol. The explorer is native tool
content in the workbench; the shell owns its resource event subscription.
Changing workspace roots refreshes content without changing placement or
visibility. Hiding preserves selection, expansion, and the live native view.
Reopening returns to the former live group and tab order, or the remembered
dock/normal group if that group disappeared; historical document groups are
never recreated. Files opened from a dock go to the normal document group,
creating it when the window contains only tools. Root and directory request
validation belongs to [documents and workspaces](documents-and-persistence.md).

Native Open retains its captured target through the picker. Platforms unable to
select files and folders together first ask which kind to open; cancellation
leaves the workbench intact.

## Terminal sessions and views

The application registry owns stable `TerminalSession`s. A session owns its PTY,
Alacritty grid, process status, authoritative size, session preferences, and event
delivery. Alacritty reads, parses, and writes on its background event loop.
Repaint wakeups coalesce; protocol requests, replies, and child exit retain order
in a process-generation-scoped queue. Restart replaces the process/grid and
rejects old events. Shutdown and child reaping run off the foreground.

`TerminalView` owns focus, layout, scroll interaction, drawing, and input. An
attachment lease gives one view input/resize authority. Releasing it does not stop
the session; natural exit retains the final grid, and startup failure remains
visible and restartable. Protected closure removes affected registry entries and
stops their sessions after document decisions succeed.

Measured view geometry supplies drawing, baseline, cell hit testing, viewport
size, and PTY pixel dimensions at the destination window's scale. Fallback glyphs
stay within emulator cell allocations. Selection uses scrollback grid coordinates;
application mouse reports use viewport cells. Geometry is remeasured after resize
or attachment handoff. Typography, bootstrap metrics, and encoding rules live in
[terminal layout](../../src/app/terminal_layout.rs),
[session](../../src/app/terminal_session.rs), and
[keyboard input](../../src/app/terminal_input.rs).

Keyboard input follows command/keymap precedence and current emulator modes.
Focus reports require both exact view focus and native window activation. Detach
reports focus loss before handoff; stopped or detached views cannot write input.
Keyboard/paste reveals the live grid, while focus reports and emulator replies
preserve scrollback. Paste uses the PTY channel, normalizing plain input and
sanitizing bracketed paste. OSC 52, title, and bell events do not access product
features or the clipboard; additional protocols remain deferred in the roadmap.

A mouse press chooses application reporting or local selection through release,
even if modifiers change. Shift forces local interaction. Wheels choose their
current route independently. The session-local reporting toggle survives restart
and handoff without changing application-requested modes. Disabling it balances
active reports before local selection takes over. Focus, geometry, detach, restart,
and mode changes invalidate gestures without sending old-process releases to a
replacement. Wire formats and coordinate limits live in
[terminal_mouse.rs](../../src/app/terminal_mouse.rs); the
[mouse fixture](../../scripts/terminal-mouse-fixture.py) supports native acceptance.

Link lookup uses displayed emulator cells, soft wraps, and OSC 8 identity.
Explicit destinations take precedence; unsupported explicit schemes suppress text
detection. Only complete HTTP(S) destinations are actionable. Modifier-click owns
the gesture ahead of mouse reporting; release revalidates grid, scrollback,
process, destination, and geometry. Output or lifecycle changes invalidate armed
links. Link recognition lives in [terminal_links.rs](../../src/app/terminal_links.rs).

## Observing product state

Snapshots derive associations and focus from live shells/workbenches rather than
replicating ownership. Committed layout, relocation, focus, activation, and closure
transitions signal the [application observer](extension-host.md#application-observation).
Diagnostic windows are excluded; terminals and tools remain explicit
non-document surfaces with no buffer/document association. Tool focus participates
in captured command targeting; native explorer Copy copies the selected URI.
