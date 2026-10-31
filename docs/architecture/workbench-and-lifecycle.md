# Workbench and view lifecycle

Part of the [architecture](../architecture.md). [Decisions](../decisions.md)
records rationale; deferred work lives in the [roadmap](../roadmap.md).

## Product application launch

The macOS product is packaged as `Knot.app` for native activation, file-open,
and reopen callbacks. CLI and platform requests join a launch queue while
[personal config and extensions](extension-host.md#package-admission-and-startup)
start. Successful startup first restores
[unsaved documents](documents-and-persistence.md#durable-unsaved-document-recovery),
then the [recorded product windows](#native-window-hot-exit), then opens each
queued request in its own window. Recovered documents that no restored window
presents go to the first opened window, so text is never hidden. With nothing
restored or requested one empty window opens. The last request window is
focused, else the previously active restored window, and the app activates.
When a package failed to start, each launch window opens the extension startup
report once; windows opened later do not.
`ApplicationSnapshot.launched` and a one-time `launched` event then tell
extensions that startup windows exist; extensions activate before any window
does. A reopen after launch opens an empty window and restores nothing. Fatal
config failure discards the queue and opens a focused diagnostic window with
Copy and Quit; product windows remain unavailable for that launch, and nothing
is recorded.

A product window has no status line. A native operation whose failure leaves no
other visible effect, such as Save, Open, Add Folder, recovery, or creating a
transfer window, reports it through a native warning prompt that needs no
answer. Successful and in-progress operations show only their effect.
Extension calls receive the failure as their result instead of a prompt.

## Editor views

Each `EditorView` owns cursor, selection, scroll, focus, IME, rendering policy,
and its completion controller/surface. Multiple views share a model while
retaining independent presentation state. Completion ownership is detailed in
[semantic UI](semantic-ui.md#completion-ownership-and-aggregation).

A view keeps no copy of the text. It reads the lines it paints, measures, or
edits from the model, and converts positions, including the platform's
document-wide UTF-16 offsets, through the buffer's line index, so editing cost
does not grow with document size. A test guards that editing, navigation, IME,
scrolling, and painting never read the whole document.

Movement and deletion respect grapheme boundaries; word navigation uses Unicode
word boundaries and vertical movement retains a shaped x coordinate. Text,
selection, caret, hit testing, and IME bounds share one shaping projection and
horizontal offset. The caret never sits inside a `\r\n` terminator, and
inserted newlines from typing, paste, and IME take the buffer's line ending.
Editing/navigation reveal the caret; wheel scrolling may move away from it, up
to the widest line shaped since the text last changed. Clipboard operations use
source text; expanded tabs are only a rendering projection. Tab stops follow
`editor.tabSize`, which the view resolves while laying out for its scope: its
document's effective language and the `editor` view kind, or a contributed
text frame's view ID. Views observe the settings store and repaint on any
change. A view's own edit
refreshes its derived state synchronously before resolving the resulting caret.
Other views update through their persistent positions.

Restorable view state is the anchor and caret as byte offsets, the start of the
top visible line as a byte offset, and horizontal scroll in pixels. Applying it
clamps each offset to the current text and snaps back to a character boundary,
so a changed or shorter file never yields an invalid position, and keeps the
restored scroll instead of revealing the caret. Folding and zoom do not exist
yet and are not recorded.

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
collapse, keeping their siblings' ratios; final-tab closure returns an
empty-workbench transition to the shell.

Each ordinary split gives a ratio of its extent to its first child, addressed
by its boundary pane, the first pane of its second child; a split beside a dock
gives the dock its absolute size instead. Grow and shrink move the focused
pane's nearest ordinary split by a step, or a focused dock's size, and equalize
gives every pane of a same-direction chain an even share. Each pane keeps its
tabs in recently activated order; a recent-tab walk shows tabs without
reordering them and commits one activation when ctrl is released. Pinned tabs
form a prefix of their pane, and every insertion, move, and restore keeps a tab
in its pinned or unpinned section. A document opened as a preview replaces the
pane's current preview in place and becomes regular when edited, pinned,
double-clicked, moved, kept open by command, or opened again without
`preview`; the
`workbench.previewTabs` setting turns previews into regular tabs. Directional
focus and moves find the neighbor from pane rectangles laid out from ratios
and dock sizes over the painted workbench, so they work while a pane is
maximized, and a move with no neighbor splits the pane on that side. Maximize
is shell render state that lasts while its pane stays focused.

One application-level coordinator protects tab, pane, window, and quit
closure; Close Other Tabs covers a pane's unpinned tabs but the origin, and
Close Pane every tab of the pane, hiding tools in a dock. It
captures exact window, workbench, tab, surface, and presentation identities,
counts live document views across windows, and deduplicates dirty documents
losing their final view. Native Save / Don't Save / Cancel prompts run serially;
Save reuses the ordinary persistence path. Quit is a hot exit: it first commits
every dirty document to [durable
recovery](documents-and-persistence.md#durable-unsaved-document-recovery) and
skips the prompts, keeping them only when that commit fails.

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

Each window keeps its last 20 closed document and terminal tabs as layout tabs
with their pane and position. Reopen Closed Tab recreates the latest through
the [layout](#window-layouts) per-kind step, in its pane when that still exists
and otherwise in the focused one; untitled documents and tools are not kept.

Native commands and extension APIs adapt the coordinator's semantic result.
Extension closure additionally revalidates caller lifecycle/cancellation before
save and close admission. Completion follows the foreground transition without
awaiting observation listeners.

## Window layouts

One versioned `WindowLayout` value serves both restorers: native window hot exit
and extension sessions through `workbench.captureLayout`. It records logical
window bounds, windowed, maximized, or fullscreen state, and the display UUID;
the split tree with dock groups and each split's ratio; ordered tabs, which are
pinned, and the selected tab per pane; the focused pane; dock sizes; and each
editor tab's view state. Preview, maximize, recent order, and the reopen stack
are not recorded, so restored tabs are regular. Every tab kind is
restored by position and records only what recreates it: a document tab its
document identity, resource URI, and explicit language, a tool tab its stable
tool ID, and a terminal tab its shell's working directory, so a restored
terminal starts a fresh shell there. The value never contains pane, tab, view,
or window handles.

Restoring plans a pruned arrangement in one place for both restorers. Document
tabs resolve to the window's entry with the same document identity, else the
same normalized URI. Unmatched documents, unregistered or repeated tools, and
repeated documents within a pane are dropped, then empty panes and one-child
splits collapse; a second pane claiming a taken dock becomes ordinary, and an
out-of-range selection or focus falls back to the first tab or the normal group.
Each surviving tab is then recreated by its kind's step (a fresh editor view of
the shared model, the registered tool frame, a new terminal session in its
recorded directory, or the window's default when that no longer exists), so a
split of one document restores as independent views of one model. A recorded
language becomes the document's explicit language unless the live document
already has one. Tools the
layout does not name are hidden and keep their remembered placement; entries
it does not place open in the normal group. Restoration never publishes the
[tool-only default](#automatic-tool-layout-memory). An unsupported version or
invalid shape is ignored and the entries are arranged as without a layout.

Restored windows are always created, so geometry is applied at creation and
moving an existing window stays deferred. Saved bounds are kept when enough of
their title bar lies on a connected display; otherwise the window is sized to
fit and centered on its saved display if connected, else on the primary one.

Cursor, selection, and scroll changes, focus, dock resizing, window bounds, and
shell changes restart a per-window trailing-edge debounce of about a second.
When it settles the window captures its layout: a layout that differs from the
last one it reported sends a `viewStateChanged` observation event, and a
changed layout or root set re-records the native window session.

## Session replacement

`workbench.replaceWindows` closes a set of windows and opens their replacements
in one foreground commit. It knows windows, documents, roots, layouts, and
retained captures, never projects; the
[project registry](../extension-api.md#project-sessions) builds its switch on
it, and native startup reuses it with no released windows. Admission claims the
protected-closure slot, so it never interleaves with closure prompts, quit, or
another replacement, and captures every released window. Staging validates
destination roots, reads files, and loads retained captures off the foreground
without creating documents, so observers see nothing until the commit.

The commit first revalidates: the caller is live, every released window is
live, the documents presented in the released windows equal the caller's
suspended set exactly, and each document whose final view is removed, when dirty
or a non-empty untitled document, names a capture retained at its current
revision. A mismatch rejects with `SessionConflict` and nothing changes. The
commit then creates every destination window with its geometry, so a failed
creation leaves the sources intact, and closes every released window as any
window closes, stopping its terminals. Each destination installs its roots, then
its [layout](#window-layouts) or, without one, presents its entries in the
normal document group. Documents left with no view are removed through the
ordinary path, which retires their automatic recovery. A window left without a
normal document group receives the ordinary untitled document, so its docks
never fill it. Each window's result reports whether its layout was applied or
ignored.

Each entry presents a live document matching its identity or resource before
considering staged content, so a session never duplicates an open document;
otherwise its capture is restored or its file opened. A missing clean file is
reported and skipped, and a failed entry is skipped with its error code while the
rest commits. Restored dirty documents are drained to durable automatic recovery
before the operation resolves, so the caller can release the captures they came
from.

## Native window hot exit

The application records every product window's roots and layout in one
record of the application state database, beside the tool-only snapshot, with
the same debounce and flush. Recording starts once startup has opened its
windows, so diagnostic windows and a launch whose config failed never write.
Settled window changes and window closure re-record every product window in
creation order; closing a window removes it, so quitting with no windows
restores none. Quit records the windows before closing them and stops
recording, and the quit path flushes the record before exit.

Startup marks the record consumed durably before restoring it through the
session commit, so a crash during restoration loses only the arrangement and
the next launch opens the default window. Documents resolve live by identity
first, so restored recovery documents are presented in place, then by opening
their file; untitled documents nobody recovered are pruned, and roots that no
longer exist are dropped. Unsaved text keeps its own recovery path.

## Tab relocation

`Workbench` owns typed reorder, insertion, and edge-split transitions. The product
shell coordinates transfers across workbenches on the foreground. Captures
include pane, tab, surface, and exact presentation; destinations retain a live
insertion/split anchor. Both endpoints and document/session ownership are checked
before mutation. Capturing or abandoning a move changes neither ownership nor
focus.

Same-pane reorder preserves tab identity. A moved tab stays in its pinned or
unpinned section and stops being a preview. Moving between panes preserves the
payload, collapses empty source panes, and allocates destination-local tab/pane
identities. Editors retain their view entity and scroll state; pending caret
reveals still apply. Terminal cross-window moves hand off the sole attachment to
a destination view of the same session; same-window moves retain their view.
Replacement or pane/window transfer invalidates old exact command targets.

Native tools stay in their owning product window. Dragging a tool tab
reorders, joins a tab group, or splits pane edges like any tab; dropping near
a workbench left/right/bottom edge moves it to that dock around the whole
layout instead of splitting one pane. Releasing a tool outside all Knot frames
cancels without creating a window. Dock-adjacent dividers drag to resize the
window-local dock size within usable limits; other dividers drag the split's
ratio, leaving each side a minimum size.

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
a window near the pointer. Preview rendering, hit-test thresholds, workbench-edge dock targets, divider
drag handling, and coordinate fallbacks live in the [relocation
code](../../src/app/product/tab_drag.rs).

## Automatic tool-layout memory

One application-owned `ToolLayoutStore` holds a single versioned tool-only
snapshot in the application state database: one SQLite file with a table per
UI-state record and one connection that serializes their writes, which also
backs [native window hot exit](#native-window-hot-exit). Quit flushes every
pending record. Unsaved-text recovery keeps its own database, because a
damaged recovery file is preserved and reported rather than treated as empty. It records stable namespaced tool
IDs, dock location, group order, dock sizes, visibility, the selected tool per
dock, and hidden tools' remembered locations. Documents, terminals, document
splits, and live handles never enter the record; a tool in a document group
flattens to the normal document group of a fresh window.

A fresh window applies the stored arrangement when its explorer registers;
after that registration the stored per-dock selection is reapplied, so a
contributed tool keeps the selection the user gave it instead of losing the dock
to the explorer. Already-open windows keep their live arrangements. Only explicit
user arrangements publish a new default: moves, dock resizes, and show/hide
changes, including a command-invoked extension show, hide, or focus. Divider
dragging coalesces through a trailing-edge debounce so one settled arrangement
reaches storage, and identical publishes skip storage. Restore, window
activation, listener callbacks, automatic presentation, and document structure
changes never publish. The cache updates synchronously so the next window
inherits immediately; writes run off the foreground and the quit path flushes a
pending write before exit. Reset Layout returns to product defaults. A missing,
corrupt, or version-mismatched record falls back to defaults, and unknown stored
IDs are skipped without creating blank frames.

Tool frames stay workbench-owned behind stable namespaced IDs and explicit
native content variants. Extension contributions attach through this contract
instead of receiving layout storage or persisting live handles. A contributed
frame currently provides either a semantic tree or read-only text; text content
owns a private buffer rendered by the editor and never enters the document
collection or persistence.

## Native resource tree

`WorkspaceTree` owns cached native filesystem presentation state. It emits
normalized resource requests and workspace generations to the shell, independently
of the extension `TreeView` provider protocol. The explorer is native tool
content in the workbench; the shell owns its resource event subscription.
Every root is a top-level row in root order, labelled by its last path segment
plus the parent segment when names collide. Directory listings are cached by URI
and shared between overlapping roots, while expansion and selection are keyed by
root and URI so a nested root browses independently. Changing workspace roots
keeps retained roots' expansion, selection, and listings, re-requests directories
that were loading under the stale snapshot, and never changes placement or
visibility. Hiding preserves selection, expansion, and the live native view.
Reopening returns to the former live group and tab order, or the remembered
dock/normal group if that group disappeared; historical document groups are
never recreated. A single click opens a file as a preview tab, and a double click or
Enter as a regular one. Files opened from a dock go to the normal document group,
creating it when the window contains only tools. Root and directory request
validation belongs to [documents and workspaces](documents-and-persistence.md).

Native Open retains its captured target through the picker. Platforms unable to
select files and folders together first ask which kind to open; cancellation
leaves the workbench intact. Opening a folder replaces every root. Add Folder to
Workspace appends one through the window's transient folder prompt, and Remove
Folder and Move Folder Up/Down act on the explorer's selected root (the root row
or the root above the selection). Each edit replaces the whole set computed from
live foreground state.

## Terminal sessions and views

The application registry owns stable `TerminalSession`s. A session owns its PTY,
Alacritty grid, process status, authoritative size, session preferences, and event
delivery. Alacritty reads, parses, and writes on its background event loop.
Repaint wakeups coalesce; protocol requests, replies, and child exit retain order
in a process-generation-scoped queue. Restart replaces the process/grid and
rejects old events. Shutdown and child reaping run off the foreground.

A new session's working directory is chosen when it is created and kept across
restart. New Terminal, splitting a terminal pane, and `terminals.open` without
a directory start in the focused (or split) terminal's current directory.
Otherwise the window's shell picks the innermost root containing the window's
most recently focused document, otherwise the first root, otherwise the home
directory, skipping roots that are not existing local directories. A terminal
tab restored in place starts in its recorded directory, or by this rule when
that no longer exists; restoration installs a window's roots before its layout,
so restored shells see them. `terminals.open` also accepts an explicit
directory. The rule lives in
[terminal/directory.rs](../../src/app/terminal/directory.rs).

A session reports its shell's current working directory by querying the shell
process, on macOS and Linux, and otherwise the directory it started in; the
query lives in [terminal/process.rs](../../src/app/terminal/process.rs). A
captured layout records it, so quit and project switches see the latest `cd`,
while a crash restores the directory of the last debounced capture.

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
[terminal layout](../../src/app/terminal/layout.rs),
[session](../../src/app/terminal/session.rs), and
[keyboard input](../../src/app/terminal/input.rs).

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
[terminal/mouse.rs](../../src/app/terminal/mouse.rs); the
[mouse fixture](../../scripts/terminal-mouse-fixture.py) supports native acceptance.

Link lookup uses displayed emulator cells, soft wraps, and OSC 8 identity.
Explicit destinations take precedence; unsupported explicit schemes suppress text
detection. Only complete HTTP(S) destinations are actionable. Modifier-click owns
the gesture ahead of mouse reporting; release revalidates grid, scrollback,
process, destination, and geometry. Output or lifecycle changes invalidate armed
links. Link recognition lives in [terminal/links.rs](../../src/app/terminal/links.rs).

## Observing product state

Snapshots derive associations and focus from live shells/workbenches rather than
replicating ownership. Committed layout, relocation, focus, activation, and closure
transitions signal the [application observer](extension-host.md#application-observation).
Diagnostic windows are excluded; terminals and tools remain explicit
non-document surfaces with no buffer/document association. Tool focus participates
in captured command targeting; native explorer Copy copies the selected URI.
