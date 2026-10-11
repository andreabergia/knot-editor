# Workbench and view lifecycle

Part of the [architecture](../architecture.md). Design rationale is recorded in
[decisions](../decisions.md); deferred work lives in [roadmap.md](../roadmap.md).

## Product application launch

The local macOS build packages the product binary and icon in `Knot.app`.
At startup the application consumes an already-delivered file-open request for
the initial window and queues it while personal config and installed extensions
start. Later file-open events and dock reopens join the same launch queue.
Successful startup opens the queued product windows, or one empty product
window when no requests arrived, then activates the app. A fatal personal
config error discards the queue and opens a dedicated diagnostic window with
Copy and Quit controls. The editor does not open in that launch; the diagnostic
window takes keyboard focus and accepts the Quit command, including Cmd-Q. It
stays open until Quit.

## Editor views

Each `EditorView` owns cursor, selection, scroll, focus, IME, rendering choices,
its persistent selection range, and at most one completion controller and
surface. Multiple views may observe one model while retaining independent
presentation state. Completion request state and merged semantic candidates
belong to the controller; the replaceable list and compact surfaces own only
selection, layout, and rendering.

Editor commands mutate the captured tab's view. Cursor and selection use
grapheme boundaries, vertical navigation retains a shaped horizontal position,
and page movement uses the pane's viewport. Each view owns both scroll axes;
editing and navigation reveal its caret, while wheel scrolling can move away
from it. Text, selections, caret, mouse hit-testing, and IME candidate bounds
share the same text shaping and horizontal offset.

Native clipboard operations use the selected source text. Tab expansion is a
rendering projection and never changes buffer bytes. Platform text input and
IME ranges are UTF-16; the view converts them to UTF-8 model edits and refreshes
its projection synchronously before resolving the resulting caret. Model
notifications then update the other views through their persistent positions.

## Workbench and protected closure

`Workbench` is the window-local layout model. It owns a binary split tree,
stable pane and tab identities, the focused pane, and ordered tabs within each
pane. Split-tree leaves refer to panes. A document tab refers to an
application-owned document and strongly owns one `EditorView`; a terminal tab
refers to an application-owned session and owns its sole `TerminalView`. It has
no document or buffer model. Splitting a
document pane creates a new view of its active document. The terminal split
policy allocates a distinct terminal identity for the new pane. Empty panes
are removed and their split branch collapses; closing the last tab produces
an explicit empty-workbench transition for the product shell to handle.

Workbench closure stays independent of native dialogs. The application-wide
view count determines whether a tab is the last view of its document. A dirty
last view returns a pending close tied to the exact pane, tab, and document;
confirmation revalidates those identities and the current application-wide
view count before applying the transition. A closed final view requests
document closure but does not remove the application-owned document itself.
This keeps window-local presentation mutation separate from application-level
document and confirmation coordination.

Protected closure is application-coordinated across tab, window, and quit
scopes. It snapshots exact tab and surface identities, determines which dirty
documents would lose their final application-wide view, deduplicates them, and
presents native Save / Don't Save / Cancel alerts serially. Save reuses the ordinary
captured persistence path. View and window mutation is deferred until every
decision succeeds, then the snapshot and approved document revisions are
revalidated before one scope transition is applied. Cancellation, persistence
failure, racing edits, stale views, and concurrent closure attempts leave the
presentation intact. Terminal tabs participate in the scope and its stale
identity checks, but not document view counts or dirty prompts. Native
window-close requests are vetoed until this flow authorizes programmatic
removal; Quit enters the same flow before asking gpui to terminate.

Each native product window renders one workbench recursively as tabbed panes
and horizontal or vertical splits. A shell-wide weak workbench registry counts
document views across windows without owning window lifetime. Clean final-tab
closure removes the final-view document and immediately installs a new
untitled document and workbench; dirty final views remain pending for the
protected-closure flow.

## Tab relocation

The product shell owns tab drag state and painted pane/tab-bar geometry. Tab
labels carry exact relocation captures without activating an inactive source.
Bar midpoints select insertion slots; the pane body accepts append in its
center and a split along the nearest normalized edge in its outer band. A
floating title and a painted insertion line or destination region show the
pending drop without changing the workbench. Left-button release hit-tests
its delivered position and commits through the relocation coordinator. Escape
is captured above editor and terminal input in either product window.

A transient application-wide coordinator retains weak source and destination
identities and each shell's painted geometry. It maps delivered window-local
events through the current native content origin into screen coordinates and
selects the foremost Knot window frame using platform stacking order. It
hit-tests that window's pane geometry for insertion or docking. Title bars,
toolbars, sidebars, non-product windows, and product overlays reject drops.
A release outside all Knot window frames tears the tab into a new window near
the pointer, constrained to the containing display. Each release recomputes
its destination from the delivered position; the transfer runs after input
dispatch so both endpoint windows are available for exact revalidation.

The floating title uses the shared screen pointer and compensates for each
window's stale local mouse position. Every pointer update repaints feedback,
including movement within one unchanged target. Completion, invalid release,
or Escape clears the pointer and all previews. Closing a source window cancels
the shared drag; closing a destination removes its geometry. The coordinator
owns neither shell nor workbench lifetime. When platform stacking order is
unavailable, the receiving window is checked first.

A relocated editor keeps deliberate scroll offsets through its first destination
layout, subject to viewport clamping. Pending caret reveals from editing still
apply; later ordinary resizes resume caret reveal behavior.

`Workbench` owns typed reorder, insertion, and four-edge split transitions.
Relocation captures exact pane, tab, surface, and presentation entity identities;
the destination captures an existing tab as an insertion or split anchor.
Both captures are checked before mutation. Moving into the same pane preserves
its tab identity and adjusts insertion indices after removal. Moving to another
pane preserves the payload and collapses an emptied source pane, including a
single-tab edge move whose original pane disappears.

The product shell coordinates transfers between distinct workbenches in one
foreground operation. It validates the source shell, both workbench entities,
the destination window and root, captured tabs/presentations, and the registered
document or attached terminal session. Documents retain their `EditorView`
entity and buffer ownership; each destination workbench allocates its own tab
identity and any new pane identity. Cross-window moves and moves to another
pane invalidate old captured command targets; reordering within the same pane
retains an exact target to the same view. Capturing and abandoning a move
changes neither ownership nor focus.

Moves do not enter protected closure or remove documents or sessions. The
application-wide document view count is unchanged after a transfer. A source
that becomes empty receives the ordinary untitled-workbench replacement. A
new-window transfer opens an empty destination shell before removing the source;
creation failure or a stale endpoint leaves the source intact and removes any
newly created unused destination. Existing windows are retained on failure.
The destination activates and focuses the moved tab. The shell's tab relocation
coordinator supplies the transition boundary used by Move Terminal to New Window.

## Native resource tree

`WorkspaceTree` is a separate native surface for application-owned filesystem
resources. It owns the same kinds of foreground presentation state, but emits
requests containing normalized resource identities and workspace generations
to the shell. It has no extension provider identity and does not share the
extension-owned `TreeView` protocol.

Open keeps its captured product target while the user chooses a path. Where the
native picker cannot select files and folders together, the shell first asks
which kind to open and then uses the corresponding picker. Cancelling either
choice leaves the workbench intact.

## Terminal sessions and views

`TerminalSession` is a stable gpui foreground model that owns one local PTY,
Alacritty emulator grid, process status, authoritative grid size, and one
coalesced wakeup path. Alacritty's event loop reads, parses, and writes on a
background thread. Its child-exit and repaint events reach the session; the
view observes session changes for repaint. A session survives release or
reconstruction of that view. Shutdown sends the event-loop signal and joins its thread on a
background thread; restart replaces the process and grid. Generation checks
prevent old process events from changing the replacement.

`TerminalView` owns focus, scroll interaction, layout, and gpui drawing. An
attachment lease permits one view per session and can be released explicitly
before a handoff; releasing the view also releases its lease. The view sends
input and size changes to its session, and the session retains the final grid
and exit status after natural process exit. Startup failure remains visible as
a status and can be retried by restarting. Restart is a view-local action that
replaces the session's process and grid while keeping its registry identity.
Physical Space input is sent as a space byte even when the platform key event
has no text character.
New Terminal creates a registered
session and one tab presentation in the captured pane. Tab switching retains
both; splitting a terminal allocates an independent session. A successful
protected tab, window, or application close removes the affected registry
entries and shuts their sessions down after dirty-document decisions complete.
Move Terminal to New Window opens an empty destination shell, then revalidates
the captured source tab, view, and registered session through the common tab
relocation coordinator. In one foreground transition it moves the payload,
releases the source attachment, attaches a destination view to the same session
and grid, and focuses the new view. Same-window terminal relocation retains its
view and attachment. Failure to
create or validate the destination leaves the source attached. The source
workbench applies its ordinary empty-workbench replacement policy. Subsequent
window closure sees only the tabs still in that window; destination resize and
input belong to the new view. On macOS the destination stays hidden until its
terminal content has been laid out, then becomes the active window.
The diagnostic window continues to exercise view rebuilding; see the
[D024 plan](../plans/d024-terminal-session-plan.md).

The terminal claims the shared Paste command while its session is running and
sends clipboard text through the existing PTY input channel. Plain paste
normalizes newlines to carriage returns; bracketed paste preserves newlines,
removes embedded escape characters, and adds the terminal's bracketed-paste
markers. A detached or stopped terminal cannot accept paste.
