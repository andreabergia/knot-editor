# D003a: Focused command routing

Status: proposed. Design agreed; implementation has not started.

Source: D003, promoted from the [deferred-work register](../roadmap.md).
This plan establishes the command behavior needed by the
[D003b keymap plan](d003b-keymaps-plan.md). See the current
[command architecture](../architecture/commands.md),
[extension host](../architecture/extension-host.md), and
[architecture decisions](../decisions.md).

## Outcome and design

A shortcut, palette choice, menu action, or script invokes one semantic command
at its captured target. For example, a single `copy` command may be handled by
an editor, terminal, or extension-backed view. Dispatch offers the command to
the focused view, then its enclosing workbench, then the application. A handler
may claim it or decline it so routing continues. A claimed command completes
through the existing structured outcome path; a command with no applicable
handler is unavailable. Delayed work continues to validate the captured target
and lifecycle rather than following later focus changes.

The application owns global command definitions, invocation admission, focus
routing, and handler registration. A command name identifies one semantic
operation, while multiple context-specific handlers may implement it. Native
and extension-owned handlers follow the same routing contract; extension
registrations and in-flight work are bound to their lifecycle. A view is a
command-routing participant because it is focused, not because its content is
a tree. The existing native tree presentation can participate through a view
identity; this plan does not require an arbitrary extension UI framework.

Existing product command hardcoding is revised wherever it prevents this
contract. Command definitions may retain stable names or gain public aliases
where needed; `copy` is the end-to-end example. The exact compatibility
mapping for existing names is reviewed before exposing the public API.

## Exclusions and deferred decisions

- Key binding registration, precedence, remapping, and removal belong to
  [D003b](d003b-keymaps-plan.md).
- This plan does not create a general declarative extension UI system or make
  raw text input, IME, pointer events, or terminal byte input into commands.
- Argument schemas, command aliases beyond those needed for the public command
  names, and general handler priorities are deferred.

## Checkpoints

### 1. Shared routing contract ⬜

- Define one command definition with multiple focused-view, workbench, and
  application handlers. Specify claim, decline, outcome, and captured-target
  semantics without relying on command-name prefixes or product-shell command
  switches for applicability.
- Route native `copy` through at least two distinct focused surfaces, with a
  workbench or application fallback. Keep palette, menu, and script invocation
  on the same path.
- Test handler order, decline and fallback, unavailable commands, focus changes,
  stale targets, and equivalent outcomes across invocation sources.
- **Review gate:** approve public command names and the routing behavior before
  extending the handler protocol.

### 2. Extension-owned view handlers ⬜

- Allow an extension-backed focused view to handle an existing command through
  the same routing path, using Knot-owned view and lifecycle identities. Keep
  JavaScript execution asynchronous and painting independent of it.
- Remove handlers on disposal, failed startup, or unload. Reject late results
  against a replaced view or ended lifecycle.
- Test a focused extension-backed view handling `copy`, declining to a fallback,
  failure isolation, and exact cleanup after unload.
- **Review gate:** confirm extension views are ordinary routing participants and
  that no tree-specific command rule leaked into the public contract.

### 3. Product integration and architecture record ⬜

- Move product editing and workbench commands onto the agreed routing contract;
  remove the native hardcoding that decides applicability by command name or
  surface kind. Preserve protected closure and captured-target validation.
- Cover representative editor, terminal, workbench, and extension commands in
  product-level tests, including palette and menu entry points.
- Update the command and extension-host architecture references and record only
  validated decisions in `docs/decisions.md`.
- **Review gate:** review the product dispatch path and regression evidence
  before D003b changes key bindings.
