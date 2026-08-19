# Step 11: Command and Keymap Dispatch Plan

This experiment decides whether one Knot-owned command abstraction can route
through gpui's focus hierarchy across text and non-text surfaces, while keeping
native view identities private and preserving the target selected at dispatch
time.

## Validated constraints

- ✅ A command is a Knot value containing a globally unique name and explicit
  arguments. gpui actions are an application adapter, not the public command
  model.
- ✅ Command definitions and command handlers are separate concepts. One
  definition may have several context-specific native handlers.
- ✅ Native and extension commands share one namespace. Native definitions
  reserve their names; an extension command has one lifecycle-owned handler;
  duplicate definitions fail.
- ✅ Arguments use a Knot-owned JSON-like transport value. Handlers validate
  their own arguments; schemas and palette-generated argument forms are
  deferred.
- ✅ Native handlers route from the captured focus target through view, window
  or workspace, and global scopes. An extension-owned handler is a global
  destination after its lifecycle is validated.
- ✅ Focus-stealing UI explicitly preserves or replaces a command target. It
  does not manipulate a global focus stack.
- ✅ A palette preserves the target from which it opened. Palette-control
  input targets the palette, while the selected semantic command targets the
  preserved origin.
- ✅ Keymap layering is exercised with gpui key contexts and pre-registered
  bindings. The prototype does not build a keymap loader or dynamically
  compile effective keymaps.
- ✅ Prefix-style invocation means gpui multi-keystroke bindings. Emacs-style
  universal or numeric prefix arguments are out of scope.
- ✅ JavaScript is the command-composition language. Step 11 adds no composite
  definitions, pipelines, repetition, decorators, rollback, or command DSL.
- ✅ Step 11 supports programmatic invocation from a top-level script, but not
  command-to-command invocation from inside a running extension handler. The
  latter belongs to the separate step 11b exploration.
- ✅ IME composition, text insertion, pointer motion, scrolling, focus changes,
  and other raw input protocols do not become registered commands.

## Prototype boundary

```text
key binding / palette / top-level script
                  |
                  v
       Command { name, arguments }
                  |
      captured window/workspace/focus target
                  |
                  v
      gpui action at captured FocusHandle
                  |
      invocation identity + optional buffer
                  |
        focused view -> window/workspace -> global
                  |
          native or extension handler
```

The application-owned command catalog contains discovery metadata and
ownership. It does not contain native focus routing. Native handlers remain on
the gpui dispatch path so an editor, tree, or terminal can implement the same
semantic command differently without adopting a common surface model.

Dispatch first captures the window, workspace, and focus target. During the
same synchronous gpui routing turn, the applicable command scope completes the
invocation context with its optional associated buffer and allocates the
invocation identity. This avoids resolving context later from mutable global
focus or active-buffer state.

The completed invocation context contains:

- a monotonic invocation identity;
- the originating window;
- the workspace/shell that owned that window at dispatch;
- a weak gpui focus target;
- the target's optional buffer handle; and
- the command value and invocation source.

The focus target is internal. Extensions receive only semantic opaque handles,
including the optional buffer handle; they never receive `FocusHandle`, gpui
objects, or concrete native-view identities. A terminal or tree invocation has
no buffer merely to satisfy the protocol.

gpui 0.2.2 publicly supports dispatching an action at a specific
`FocusHandle`. The palette retains a `WeakFocusHandle` captured when it opens,
upgrades it when the user selects an entry, and dispatches without temporarily
restoring visible focus. Failure to upgrade is an invalid target, not a reason
to retarget the command.

## Execution

### 1. Separate command values, definitions, and ownership

- ✅ Add a serializable `Command` value with a name and JSON-like arguments at
  the Knot-owned protocol boundary.
- ✅ Add minimal definition metadata needed for discovery: stable name,
  display title, and native or extension ownership.
- ✅ Refactor the existing extension-only `CommandRegistry` into the
  authoritative command catalog without moving native handlers into it.
- ✅ Reserve native command names and preserve lifecycle cleanup for extension
  definitions.
- ✅ Reject duplicate native/extension names and invocations of missing or
  disposed definitions with explicit errors.
- ✅ Keep argument validation in handlers; do not add schemas, coercion, or
  generic argument UI.

### 2. Introduce captured invocation context

- ✅ Allocate monotonic identities for every accepted invocation and retain the
  captured context alongside any in-flight extension command.
- ✅ Keep the current restriction of at most one in-flight extension command;
  concurrent scheduling belongs to step 11b.
- ✅ Capture the target window, workspace/shell, weak focus handle, and optional
  associated buffer when dispatch begins.
- ✅ Ensure a later focus or active-buffer change does not retarget an existing
  invocation.
- ✅ Revalidate weak target, window/workspace ownership, buffer existence,
  extension lifecycle, cancellation, and invocation identity immediately
  before applying a delayed mutation.
- ✅ Keep native target data inside `app`; extend the extension protocol only
  with semantic command arguments and opaque context handles required by the
  exercise.
- ✅ Return structured unavailable, invalid-target, invalid-argument,
  cancelled, and handler-failure outcomes instead of silently dropping an
  invocation.

### 3. Adapt commands to gpui focus routing

- ✅ Introduce the smallest gpui action adapter that carries a Knot `Command`
  and invocation context without making gpui actions the registry identity.
- ✅ Dispatch the action at the captured focus handle and let gpui perform its
  normal capture/bubble traversal.
- ✅ Attach native handlers at surface and enclosing shell scopes. A handler
  claims a recognized command; otherwise routing continues outward.
- ✅ Add a diagnostic native command used only by the prototype to record the
  resolved surface kind and whether the context contains a buffer.
- ✅ Exercise that command from `EditorView`, `TreeView`, and `TerminalView`,
  proving the editor supplies a buffer while tree and terminal do not.
- ✅ Add focused tests for nearest-handler precedence, outward fallback,
  unhandled commands, and destroyed focus targets.

The first implementation slice should verify gpui action propagation and
explicit `FocusHandle` dispatch in tests before adding catalog or palette UI.
If gpui's public routing cannot preserve the required context, record the
specific gap before introducing any Knot-owned routing table.

✅ A focused gpui fixture verifies that dispatching at an explicit rendered
`FocusHandle` does not change visible focus, stops at the nearest handler by
default, and reaches an enclosing handler when the nearest handler explicitly
continues propagation.

### 4. Add a target-preserving command palette

- ✅ Add a minimal native palette that lists discoverable command definitions,
  filters them by name/title, selects an entry, and dismisses cleanly.
- ✅ Capture the originating `WeakFocusHandle` and semantic context when the
  palette opens.
- ✅ Keep navigation, filtering, dismissal, and text input local to the palette;
  these controls do not replace the preserved semantic target.
- ✅ Dispatch the selected command at the preserved target without visibly
  refocusing it first.
- ✅ Reject invocation if the originating target has disappeared while the
  palette is open.
- ✅ Use one fixture palette entry carrying fixed, visible arguments for the
  cross-origin exercise. General argument prompting remains deferred.

### 5. Exercise contextual and transient keymaps

- ✅ Register a small fixed set of gpui bindings whose actions carry Knot
  command values with explicit arguments.
- ⬜ Add surface key contexts for editor, tree, terminal, and palette routing.
- ⬜ Exercise one persistent active-map context and one transient-map context,
  with the transient context cleared after its next command or cancellation.
- ⬜ Exercise one multi-keystroke binding through gpui's existing pending-key
  machinery.
- ⬜ Verify precedence among the base binding, active context, transient
  context, and the more specific focused surface.
- ⬜ Do not add configuration loading, keymap editing, arbitrary runtime layer
  installation, universal arguments, or numeric repetition.

### 6. Unify keybinding, palette, and script invocation

- ⬜ Add a top-level JavaScript `commands.invoke(name, arguments)` operation
  that enters the same dispatcher and completes with the same structured
  outcome as other invocation sources.
- ⬜ Reject `commands.invoke` while the calling extension is already executing
  a command handler; nested and concurrent execution semantics are step 11b.
- ⬜ Choose one argument-bearing fixture operation and invoke the identical
  command value from a keybinding, the palette fixture entry, and a top-level
  script.
- ⬜ Confirm all three sources reach the same handler with equal arguments and
  equivalent captured semantic context.
- ⬜ Keep the cross-surface diagnostic separate so the argument-bearing
  operation does not need contrived editor, tree, and terminal semantics.

The fixture operation may be intentionally narrow, such as inserting a fixed
text value into the captured editable buffer. Its purpose is to validate
origin equivalence and explicit arguments, not to define a production editing
command set.

### 7. Validate suspended-target rejection

- ⬜ Invoke one existing asynchronous extension fixture command against a
  captured buffer, suspend it on an already-supported host wait, then close or
  invalidate the target before it attempts mutation.
- ⬜ Confirm the late mutation is rejected rather than applied to the newly
  focused or active buffer.
- ⬜ Confirm the foreground remains responsive while the extension waits.
- ⬜ Do not add general concurrent invocation, nested invocation, result
  pipelines, or cancellation trees in this step.

### 8. Record the decision

- ⬜ Update `architecture.md` if the experiment validates command catalog
  ownership, captured context, gpui routing, and palette target preservation.
- ⬜ Record validated choices and liabilities in `decisions.md`.
- ⬜ Mark roadmap step 11 complete and summarize the checkpoint.
- ⬜ Update this plan with ✅ markers, deliberately skipped work, and the final
  checkpoint result.
- ⬜ Run focused tests, run `cargo fmt` once at the end of Rust work, then run
  the full test suite.

## Commit boundaries

1. Command value, catalog, ownership, and captured invocation context.
2. gpui focus routing and cross-surface validation.
3. Target-preserving command palette.
4. Contextual, transient, and multi-keystroke keymap fixtures.
5. Top-level script invocation and suspended-target validation.
6. Decision records and completed-plan updates.

## Out of scope

Argument schemas or generated forms, configurable keymap loading, runtime
keymap compilation, extension-defined keymaps, keymap persistence, universal
or numeric prefix arguments, keyboard macros, command aliases, extension
overrides of native handlers, command result pipelines, composite command
definitions, implicit repetition, rollback, undo grouping, nested
command-to-command invocation, concurrent command scheduling, cancellation
trees, and arbitrary native view identities in the extension protocol remain
deferred.

## Decision checkpoint

Focus-targeted command routing is validated if:

- keybindings, the palette, and a top-level script dispatch the same command
  value with equal explicit arguments;
- a focus-stealing palette invokes against its preserved live target without
  changing visible focus or silently retargeting;
- gpui routes native commands to the nearest applicable handler and permits
  fallback through enclosing scopes;
- editor, tree, and terminal targets participate without sharing a surface
  model or fabricating buffers;
- native focus identity remains inside `app`, while extensions receive only
  semantic opaque context; and
- a suspended command cannot mutate a destroyed target or whichever buffer
  became active later.

The approach fails the checkpoint if commands require a central enumeration of
native view types to route correctly, if palette invocation depends on
temporarily restoring focus, if non-text surfaces require dummy buffers, or if
late asynchronous work resolves context again from current global state.
