# First Product Slice: Dogfoodable Text Editing

Status: in progress — checkpoint 2 implemented; review gate pending.

This slice makes Knot usable for editing its own source on macOS. It evolves
the validated prototype in place and introduces the real workbench ownership
model before polishing editor behavior around a temporary single-view shell.

The slice is successful when Knot can be used safely for an ordinary editing
session involving existing and new UTF-8 files, multiple windows, tabs, and
splits, including Find within the current file. Syntax highlighting is not
required.

## Product constraints

- ⬜ Use native macOS windows, menus, file dialogs, and save/discard alerts.
- ⬜ Allow documents to exist independently of a workspace. Open, Save As, and
  future drag-and-drop flows may target any local file the user selects.
- ⬜ Keep workspace containment as a workspace-tree rule, not a document
  persistence rule.
- ⬜ Make windows, tabbed panes, and splits part of the initial product UI.
- ⬜ Let each tab own an independent editor view. Multiple tabs, panes, or
  windows may present the same buffer with independent cursor, selection, and
  scroll state.
- ⬜ Implement user-facing semantic operations as registered native commands.
  Menus, keybindings, the palette, and JavaScript must enter the same
  target-preserving dispatcher rather than call separate shell or view methods.
  Raw text input, IME, pointer gestures, scrolling, and focus changes remain
  input protocols rather than commands.
- ⬜ Retain validated prototype code when it fits these behaviors; change
  boundaries only where this slice supplies a concrete counterexample.
- ⬜ Treat every durable path introduced or hardened by this slice as product
  code. Add automated regression coverage at the model, command, integration,
  or UI boundary appropriate to the behavior.

## User journey

1. Launching `knot` opens a native window with one empty untitled buffer.
2. `knot <file>` opens an existing file. A missing file creates a new buffer
   associated with that destination and does not touch the filesystem until
   Save.
3. `knot <folder>` opens that folder as a workspace. Files selected in its tree
   open as ordinary documents.
4. New and Open use standard menu items and shortcuts. These entry points
   dispatch the same commands exposed through the palette and JavaScript. Open
   uses the native file dialog and accepts either a file or a folder.
5. A document can be shown in tabs, splits, and multiple windows. Each view
   retains its own presentation state while edits remain shared.
6. Save writes the selected document. An untitled document uses the native
   Save dialog; Save As always does so and may select a path outside the
   workspace.
7. Closing a dirty document or quitting uses native Save / Don't Save / Cancel
   confirmation. Closing the final tab leaves a new empty untitled buffer.

## Ownership to validate

Application state strongly owns open documents and deduplicates normalized
resource URIs. A document owns its `BufferModel`, persistence identity and
state, and undo history. Documents do not belong to a workspace or window.

Each native window owns a workbench. Its layout is a split tree whose leaves
are tabbed panes. A tab refers to one document and owns one surface view; an
editor tab therefore owns one `EditorView`. Closing a tab disposes that view.
Closing the last view of a document requests document closure and performs any
required dirty-state confirmation.

A window may own one optional workspace for its tree and related commands.
Opening or saving a document does not require membership in that workspace.
`BufferRegistry` remains a weak transport-handle registry rather than a
document lifetime owner.

This ownership is deliberately limited to the behavior exercised here. The
slice does not introduce a general docking or surface framework.

## Delivery checkpoints

Implement this slice as a sequence of review-sized goals rather than one
uninterrupted run. Each checkpoint must leave the repository in a coherent
state, update the applicable checkboxes below, add focused automated coverage,
and end with a review summary. Make logical commits within a checkpoint, then
stop for code review and any listed manual testing before starting the next
checkpoint.

The checkpoints follow implementation dependencies and therefore cut across
the concern-oriented execution sections below. Existing prototype ownership is
not a compatibility boundary: replace `OpenBufferCollection` directly with the
application-owned document model rather than introducing transitional APIs.

### Checkpoint 1: Application-owned documents ✅

- ✅ Introduce the application-owned document collection and explicit untitled,
  destination-associated, persisted, and generated document states.
- ✅ Move strong model ownership, persistence identity and state, dirty-state
  semantics, and application-wide normalized-URI deduplication into documents.
- ✅ Preserve the current visible shell while migrating it to the new ownership
  model; remove superseded prototype ownership in the same checkpoint.
- ✅ Cover document lifetime, state transitions, dirty state, and URI
  deduplication with focused tests.
- ✅ Review gate: ownership inspection, code review, and launch smoke are
  complete. No additional manual UI testing is required.

### Checkpoint 2: Workbench state and view lifetime

- ✅ Add the window workbench model, split tree, focused pane, ordered pane tabs,
  and tab-owned editor views.
- ✅ Define tab, pane, last-view, and last-tab transitions independently of native
  dialogs, using explicit pending-close outcomes where confirmation will later
  be required.
- ✅ Prove that multiple tabs and panes can present one document while retaining
  independent cursor, selection, and scroll state.
- ✅ Cover workbench transformations and document/view lifetime at the model
  boundary.
- ✅ Review gate: inspect workbench invariants and lifecycle behavior. No polished
  native UI is expected yet.

### Checkpoint 3: Product shell and layout

- Replace default fixture startup with the product shell and keep fixtures
  available through explicit exploration entry points.
- Render workbench tabs and horizontal or vertical splits, support multiple
  native windows, and create a replacement untitled document when required.
- Accept the initial CLI path and macOS open events through one application
  entry flow; opening and persistence behavior may remain limited until later
  checkpoints.
- Add lower-level coverage for shell transitions and UI coverage where layout
  behavior cannot be established below gpui.
- Review gate: manually exercise launch, new windows, tabs, both split
  directions, independent views of one document, and final-tab replacement.

### Checkpoint 4: Target-preserving product commands

- Register the slice's document, window, editing, and Find command names in the
  native catalog, implementing the commands whose underlying behavior exists.
- Route native menus, fixed keybindings, the palette, and JavaScript through
  the shared dispatcher with captured product targets and asynchronous
  outcomes.
- Reject destroyed targets without falling back to current focus.
- Cover routing, discovery, target capture, destruction, and asynchronous
  completion at command and integration boundaries.
- Review gate: invoke the implemented commands through every applicable entry
  path and compare their targets and outcomes.

### Checkpoint 5: New and Open

- Complete zero-or-one-path launch behavior, missing-file destination
  association, workspace-folder opening, New, and interactive Open.
- Use native dialogs and normalize platform paths only at the local-provider
  and macOS boundaries.
- Open files independently of workspace containment and deduplicate them
  application-wide.
- Cover success, cancellation, decode and open failures, stale completions,
  and resource deduplication.
- Review gate: manually exercise launch with no path, an existing file, a
  missing file, and a folder, plus New and Open through all applicable command
  entry points.

### Checkpoint 6: Safe Save and Save As

- Add new-file creation and atomic local replacement to the provider contract.
- Implement Save and native Save As without prematurely binding or retargeting
  a document.
- Detect external-change conflicts and preserve dirty state across racing edits
  and stale asynchronous completions.
- Cover atomic create and replace, overwrite selection, conflicts, races,
  failures, and document identity changes.
- Review gate: manually exercise first save, Save As inside and outside the
  workspace, overwrite confirmation, a racing edit, and an external conflict.

### Checkpoint 7: Protected closure

- Coordinate close-tab, close-window, and quit against application-owned dirty
  documents using native Save / Don't Save / Cancel alerts.
- Prompt at most once for a document visible through multiple views or windows
  and preserve state on cancellation or failure.
- Cover tab, window, document, and application lifecycle combinations.
- Review gate: manually exercise all three closure scopes with clean, dirty,
  multiply viewed, cancelled, saved, and discarded documents.

### Checkpoint 8: Ordinary editor interaction

- Complete keyboard movement and selection variants, mouse placement and
  selection, scrolling, caret visibility, and native clipboard integration.
- Verify insertion, multiline deletion, newline, grapheme, Unicode, and IME
  behavior through the product shell.
- Add focused model and view tests plus UI coverage for behavior that depends
  on native input protocols.
- Review gate: edit representative Rust and Markdown files using keyboard,
  mouse, clipboard, Unicode, and IME input.

### Checkpoint 9: Undo and redo

- Add per-document history using ordinary `EditTransaction` mutation paths and
  practical typing and deletion grouping.
- Break groups at the defined interaction boundaries, clear redo after
  divergent edits, and keep history independent across documents and views.
- Route Undo and Redo through the shared command path and cover history,
  grouping, anchors, notifications, and routing.
- Review gate: manually exercise grouped typing and deletion, paste, divergent
  edits, and document or view switching.

### Checkpoint 10: Current-file Find

- Add the focused query field, live match state and highlighting, and
  next/previous navigation that follows buffer edits.
- Register and route Find operations with the required macOS bindings.
- Cover matching, navigation, edits, focus, target preservation, and command
  entry paths.
- Review gate: manually search and edit a file with zero, one, and multiple
  matches using the query field and keyboard navigation.

### Checkpoint 11: Regression and dogfood closure

- Fill remaining coverage gaps across model, lifecycle, command, persistence,
  integration, and native UI paths.
- Complete the two-window dogfood session and all cross-entry-point command
  checks described in section 8.
- Update architecture and decisions, record the slice result, move newly
  deferred work, format once, and run focused checks followed by the full test
  suite.
- Review gate: review the complete slice against its success criteria and
  product constraints before marking it completed.

## Execution

### 1. Establish the product entry path

- ⬜ Replace the fixture-oriented default launch with the minimal product
  shell while keeping exploration fixtures available outside normal startup.
- ⬜ Accept zero or one file-or-folder CLI path and route macOS open events
  through the same application flow.
- ⬜ Convert platform paths to normalized `file://` resources at the macOS and
  local-provider boundary; keep paths out of generic document state.
- ⬜ Create one empty untitled document when launch or final-tab closure leaves
  the application without a visible document.

### 2. Introduce windows, tabbed panes, and splits

- ✅ Move strong document ownership above individual windows and keep weak
  extension handles independent of it.
- ⬜ Give each window a workbench containing a split tree, focused pane, and
  ordered tabs per pane.
- ✅ Open the same document in multiple independent editor views and keep their
  cursor, selection, and scroll state separate.
- ⬜ Support creating and closing windows, tabs, and horizontal or vertical
  splits with standard native commands and shortcuts.
- ⬜ Define last-view and last-tab behavior, including dirty confirmation and
  creation of the replacement untitled buffer.
- ✅ Keep tab dragging, arbitrary docking, and layout persistence out of this
  slice.

### 3. Make product operations first-class commands

- ⬜ Register New, Open, Save, Save As, Close Tab, Close Window, New Window,
  Quit, Split Horizontal, Split Vertical, Undo, Redo, Cut, Copy, Paste, and
  Select All as native `CommandCatalog` entries. Register Find and its
  next/previous navigation operations through the same path.
- ⬜ Route native menus and keybindings through the command dispatcher. Do not
  retain direct menu-to-shell or keybinding-to-shell implementations of the
  same operations.
- ⬜ Let the command palette discover and invoke each operation from the
  catalog without operation-specific palette code.
- ⬜ Allow JavaScript to invoke and await the same commands through
  `commands.invoke(...)`. Interactive Open and Save As invocations use the
  native dialogs; explicit destination arguments remain deferred until an
  automation use case defines their safety and semantics.
- ⬜ Preserve captured targets: editing and Save address the focused document,
  Split the focused pane, Close the focused tab or window, and Open the
  captured window. Quit coordinates application-owned documents across
  windows. Reject destroyed targets rather than falling back to current focus.
- ⬜ Keep asynchronous command invocations pending until dialogs, confirmation,
  or persistence work settles and return the existing structured command
  outcome to every caller.

### 4. Make documents and resources user-facing

- ✅ Represent untitled, destination-associated-but-uncreated, persisted, and
  generated documents explicitly. Dirty state must account for never-saved
  documents rather than relying only on revision comparison.
- ⬜ Deduplicate open files application-wide by normalized resource URI and
  focus an existing view or create another view according to the invoking
  command.
- ⬜ Add the registered document and window commands to the native macOS menu.
- ⬜ Use native Open and Save dialogs. A selected folder becomes a window
  workspace; a selected file becomes a document regardless of workspace.
- ⬜ Bind an untitled or destination-associated document to its normalized URI
  only after the relevant create/save operation succeeds.

### 5. Complete ordinary editing behavior

- ⬜ Verify insertion, multiline deletion, newline handling, grapheme-aware
  cursor movement, and selection against ordinary source files.
- ⬜ Add standard character, word, line, page, and document movement and their
  selection-extending variants.
- ⬜ Support mouse placement and selection, vertical and horizontal scrolling,
  and keeping the caret visible.
- ⬜ Integrate native clipboard Cut, Copy, Paste, and Select All commands.
- ⬜ Preserve working IME and Unicode behavior through the product shell.
- ⬜ Add current-file Find with a focused query field, visible match state,
  match highlighting, and next/previous navigation that follows buffer edits.
  Use the VS Code macOS defaults: `cmd-f`, `enter`, and `shift-enter`.
- ⬜ Keep plain text as the only required presentation; Replace, regular
  expressions, and syntax highlighting are deferred.

### 6. Add usable undo and redo

- ⬜ Build per-document undo history from ordinary `EditTransaction` edits so
  undo and redo retain the normal model notification and anchor paths.
- ⬜ Group consecutive typing and deletion into practical undo steps. Break
  groups on cursor or selection movement, paste, explicit commands, focus
  transfer, and elapsed interaction boundaries where needed.
- ⬜ Route Undo and Redo through native menus, standard shortcuts, and the
  focused editor command path.
- ⬜ Clear redo after a divergent edit and preserve independent history across
  document and view switches.

### 7. Make persistence safe enough for daily use

- ⬜ Extend the provider contract to create new files and atomically replace
  existing local files using a temporary sibling and rename.
- ⬜ Capture persisted file metadata sufficient to detect an external change
  before replacement. Refuse a conflicting overwrite and offer Reload or Save
  As; live filesystem watching remains deferred.
- ⬜ Preserve dirty state when an edit races an asynchronous save and reject
  stale completions after document, resource, or window lifecycle changes.
- ⬜ Let native Save As overwrite confirmation govern an explicitly selected
  existing destination while still using the safe replacement path.
- ⬜ Surface open, decode, create, conflict, and save failures without losing
  document text or retargeting the operation.

### 8. Close and dogfood the slice

- ⬜ Protect dirty documents when closing a tab, closing a window, or quitting;
  do not prompt twice for a document visible in multiple places.
- ⬜ Add thorough tests for document/view lifetime, URI deduplication, untitled
  state, undo grouping, save races, atomic create/replace, conflict rejection,
  and command routing. Cover native UI behavior where model and integration
  tests cannot establish the interaction contract.
- ⬜ Use Knot to create a new file and edit several existing files in this
  repository across two windows and a split, including two views of one
  buffer.
- ⬜ Exercise undo/redo, Save As outside the workspace, an external-change
  conflict, tab/window closure, and application quit during that session.
- ⬜ Invoke the slice's registered semantic commands through their applicable
  menu or keybinding, the command palette, and JavaScript, confirming that all
  paths preserve the same target and outcome.
- ⬜ Update `architecture.md` and `decisions.md` for the document, workbench,
  history, and persistence boundaries validated by the finished slice.
- ⬜ Mark this plan as completed with its result and move any newly deferred
work to [`deferred.md`](../deferred.md).
- ⬜ Run `cargo fmt` once after the Rust work, then run focused checks and the
  full test suite.

## Explicit exclusions

The slice does not require Replace, regular-expression search, syntax
highlighting, configurable keymaps, layout restoration, tab dragging, live
filesystem watching, Linux support, or production extension scheduling. These
and other known future concerns are tracked in [deferred.md](../deferred.md).
