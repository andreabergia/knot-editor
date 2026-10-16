# D049 — Native extension UI

Status: ⬜ Planned; implementation has not started. Depends on the completed
[dockable workbench plan](d049-dockable-workbench-plan.md).

Source: checkpoint 5 of the [extension platform and projects plan](extension-platform-and-projects-plan.md).
Design discussion: 2026-10-16. Current boundaries:
[architecture](../architecture.md), [decisions](../decisions.md),
[semantic UI](../architecture/semantic-ui.md),
[extension host](../architecture/extension-host.md), and
[commands](../architecture/commands.md).

## Outcome

An ordinary JavaScript extension can define native tool frames before windows
exist, populate them independently in each window, and expose native picker,
input, and confirmation/message interactions. Users can dock these frames using
the shared workbench, including sidebars and mixed document/tool tab groups.

The concrete acceptance case is a Git-status-style frame: grouped file lists,
file status labels, stage/unstage-style row actions, group actions, and file
activation. Use fixture data and commands; implementing Git itself is excluded.

## Design

### Definitions and instances

- One lifecycle-owned registration declares a namespaced contribution ID, title,
  initial placement, supported native content, provider, and actions together.
  Replace predeclared tree kinds and `registerTreeDataProvider("outline", ...)`;
  do not retain aliases or a compatibility layer.
- The application owns definitions and attaches them when product windows open.
  Each contribution has at most one instance per product window. Native views
  own selection, expansion, scrolling, focus, and rendering.
- Contributions are available in every product window initially. Explicit
  window-targeted show/hide/focus controls presentation. Define initial
  visibility and deterministic placement order at the first review gate.
- Duplicate live IDs fail without replacing another registration. Disposing a
  registration removes its instances and ownership; hiding is separate.
- Providers and actions receive explicit live window and instance context.
  Context-free activation can register definitions but cannot borrow active
  focus. Same-window docking preserves instance identity; hide/reopen or window
  recreation invalidates released instance handles and pending results.
- Native mutation is foreground-owned and transported through the existing typed
  async handlers. Reuse lifecycle cleanup, reverse runtime requests, generation
  checks, and command dispatch rather than adding another scheduler or event bus.

### Native content and actions

Extend the existing tree surface to cover flat and grouped lists. Supply labels,
descriptions, status text/badges, expansion, selection, activation, and named
actions. Frame actions, group actions, and row actions invoke existing registered
commands with the originating context and relevant item identity/arguments.
Keep action data semantic; JavaScript does not render controls or handle painting.

Prove file activation and stage/unstage-style actions in a Git-status fixture.
Actions must operate on their captured item/window after focus changes; clicking
an action must not also trigger the row's activation. Keyboard activation and
actions must be usable without a pointer. Begin with single selection.

Text-oriented frames reuse native buffer/editor infrastructure. Provide the
smallest read-only text content contract needed for a log-like fixture, keeping
provider ownership separate from ordinary editable document persistence. Do not
build a logging service, streaming protocol, or new text renderer. Finalize the
concrete text update contract when implementing content, against the actual code.

Preserve native loading/empty/error states and retry/invalidation. Allow
invalidation for a specified window instance or all instances, optionally at a
parent. Responses apply only to the live registration, instance, parent, and
generation. Async provider failure affects that request; unrelated frames and
windows continue. Never invoke JS synchronously during painting or raw input.

### Transient interactions

Add searchable single-selection pickers, text input, and confirmation/message
operations under `workbench`. Pickers initially receive a complete item list and
filter locally. Inputs support a title/prompt and initial text, not a form schema
or async validation callback. Use existing native prompts for confirmations where
they meet the contract.

Capture the originating window before focus changes, using an explicit target or
admitted command capture. Settle once on accept/dismissal, window closure, caller
cancellation, or unload. Dismissal is a normal typed result; operational failures
use host errors. Restore the previous live focus when appropriate. Use one active
extension transient interaction per window; reject overlap rather than inventing
a queue or nesting framework. Integrate this rule with the existing palette.

Implement proper native text entry for picker/input and reuse that path in the
command palette, replacing its key-event string assembly. Support ordinary
selection, clipboard editing, Unicode and IME through gpui input handling. Share
only the input/list behavior actually needed, not a generic widget framework.

## Checkpoints

Execute one checkpoint per implementation request and stop at its review gate.
Keep this plan updated with ✅ evidence and split coherent work into logical
commits. Update declarations, authoring examples, and docs alongside each API.

### 1. Lifecycle-owned contributed frames ⬜

Connect namespaced registration and per-window instances to the dockable
workbench. Adapt the outline fixture to that supported path and remove hardcoded
outline creation, declared-kind tables, and singleton shell routing. Implement
window-targeted show/hide/focus, provider context, invalidation, disposal, and
failed-startup cleanup. Extend application snapshots/command captures to represent
contributed tools without pretending they are document presentations.

**Validation:** host/JS boundary tests and gpui integration tests for registration
before/after windows, duplicate IDs, multiple contributions/windows, targeted
invalidation, moving and reopening instances, stale results, window closure,
disposal from callbacks, unload, startup rollback, and captured command routing.

**Review gate:** inspect outline frames in two windows, including dragging into
a document tab group. Approve exact public names, IDs, context, visibility,
placement, hide/reopen, and invalidation semantics before expanding content.

### 2. Useful content and actions ⬜

Implement the semantic tree/list additions, frame/group/row actions, and the
minimal read-only text frame. Ship a normal installed-package fixture with a
Git-status-style frame and log-like output through public APIs only. Update all
affected repository fixtures and remove superseded item-command special cases.

**Validation:** grouped/empty/loading/error data, keyboard and pointer selection,
activation versus action clicks, item arguments, status presentation, refresh
after actions, stale item/window captures, independent two-window content, and
text replacement/disposal. Verify shared docking and focus routing with editor
and terminal tabs; no fixture-only production bypass.

**Review gate:** manually inspect the Git-status-style frame in sidebar, bottom,
and mixed tab-group placements. Approve that its information and actions are
sufficient for a real Git extension, and inspect the text frame's ownership/API.

### 3. Picker, input, and confirmation/message ⬜

Implement the transient operations with native text input and local picker
filtering. Move the command palette onto the shared input path. Extend the fixture
with create/select/rename/remove-style flows without implementing project storage
or project switching policy.

**Validation:** native input behavior and JS results; accept/dismiss, no matches,
Unicode/IME and paste/editing, overlap with a palette/another request, focus
restoration, originating-window isolation, cancellation, unload, and closure.
Include an awaited picker inside an extension command to catch scheduler deadlocks.

**Review gate:** inspect ordinary typing, picker selection, rename input, and
confirmation in a product build. Approve result/error and overlap semantics.

### 4. Cleanup and D049 acceptance ⬜

Finish the touched proof-of-concept cleanup: obsolete outline/provider APIs,
singleton surface assumptions, redundant selection/focus paths, examples, and
historical comments. Keep cleanup tied to the new production path; no unrelated
host restructuring. Update architecture references for actual ownership/runtime
changes and decisions for validated choices.

**Validation:** focused model/lifecycle/command/UI tests, full library tests,
extension authoring/API checks, and Clippy with warnings denied. Run `cargo fmt`
once at the end of Rust work before committing. Record actual results and any
platform limitations. Manual product review complements automated coverage;
do not use native-app/browser automation.

**Review gate:** accept the complete extension fixture with two independent
windows and movable frames; verify unload and failed activation remove ownership
and settle pending operations. Only then mark parent checkpoint 5 complete.

## Exclusions and deferred choices

No actual Git/debugger/project-manager implementation, general widget tree,
WebViews, multi-selection, dynamic/streaming picker providers, async input
validation, floating tool windows, cross-window tool movement, applicability
predicates, or persisted visibility/layout. Project registry and persistence
remain D051/D005. Existing document/terminal behavior remains covered.

Choose concrete API names and content/action schemas at their stated review
gates; do not publish speculative types ahead of working behavior. No cross-restart
instance identity or general layout API is introduced here.
