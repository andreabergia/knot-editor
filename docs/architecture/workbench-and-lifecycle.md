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
window stays open until Quit.

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

## Native resource tree

`WorkspaceTree` is a separate native surface for application-owned filesystem
resources. It owns the same kinds of foreground presentation state, but emits
requests containing normalized resource identities and workspace generations
to the shell. It has no extension provider identity and does not share the
extension-owned `TreeView` protocol.

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
New Terminal creates a registered
session and one tab presentation in the captured pane. Tab switching retains
both; splitting a terminal allocates an independent session. A successful
protected tab, window, or application close removes the affected registry
entries and shuts their sessions down after dirty-document decisions complete.
Move Terminal to New Window opens an empty destination shell, then revalidates
the captured source tab, view, and registered session. In one foreground
transition it detaches the source view, attaches a destination view to the same
session and grid, removes the source tab, and focuses the new view. Failure to
create or validate the destination leaves the source attached. The source
workbench applies its ordinary empty-workbench replacement policy. Subsequent
window closure sees only the tabs still in that window; destination resize and
input belong to the new view. On macOS the destination stays hidden until its
terminal content has been laid out, then becomes the active window.
The diagnostic window continues to exercise view rebuilding; see the
[D024 plan](../plans/d024-terminal-session-plan.md).
