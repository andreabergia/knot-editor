# Workbench and view lifecycle

Part of the [architecture](../architecture.md). Design rationale is recorded in
[decisions](../decisions.md); deferred work lives in [deferred.md](../deferred.md).

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

`Workbench` is the window-local editor layout model. It owns a binary split
tree, stable pane and tab identities, the focused pane, and ordered tabs within
each pane. Split-tree leaves refer to panes; each tab refers to an
application-owned document and strongly owns one `EditorView`. Splitting a
pane creates a new view of its active document, and closing a tab drops that
view. Empty panes are removed and their split branch collapses; closing the
last tab produces an explicit empty-workbench transition for the product shell
to handle.

Workbench closure stays independent of native dialogs. The application-wide
view count determines whether a tab is the last view of its document. A dirty
last view returns a pending close tied to the exact pane, tab, and document;
confirmation revalidates those identities and the current application-wide
view count before applying the transition. A closed final view requests
document closure but does not remove the application-owned document itself.
This keeps window-local presentation mutation separate from application-level
document and confirmation coordination.

Protected closure is application-coordinated across tab, window, and quit
scopes. It snapshots exact view identities, determines which dirty documents
would lose their final application-wide view, deduplicates them, and presents
native Save / Don't Save / Cancel alerts serially. Save reuses the ordinary
captured persistence path. View and window mutation is deferred until every
decision succeeds, then the snapshot and approved document revisions are
revalidated before one scope transition is applied. Cancellation, persistence
failure, racing edits, stale views, and concurrent closure attempts leave the
presentation intact. Native window-close requests are vetoed until this flow
authorizes programmatic removal; Quit enters the same flow before asking gpui
to terminate.

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

## Terminal: implemented fixture

`TerminalView` is a native gpui surface that owns one authoritative local PTY
session, Alacritty emulator grid, and its focus state. Alacritty's event loop
performs PTY reads, parsing, and writes on a background thread and sends
coalesced wakeups to the gpui foreground. The foreground routes input and
resize messages to that event loop and renders the grid through gpui. The view
does not use or expose an editor model. Layout bounds determine the grid and
PTY dimensions. Child exit is reported back to the view; closing or replacing
a session shuts it down and reaps the child off the foreground thread.

## Terminal: validated production direction

The following separation is a validated design, not the current implementation
(see [D024](../deferred.md#terminals-and-generated-surfaces)).

The production boundary separates a stable `TerminalSession`, which owns the
PTY, emulator, and process lifecycle, from a disposable `TerminalView`, which
owns presentation, focus, and layout. A session survives view reconstruction
or relocation across tabs and windows. Explicit terminal closure terminates
the session, and reopening creates a new one; detached persistence and
simultaneous presentations remain out of scope.
