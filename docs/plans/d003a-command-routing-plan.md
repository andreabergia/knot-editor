# D003a: Focused command routing

Status: checkpoint 1 approved; checkpoint 2 in progress.

Source: D003, promoted from the [deferred-work register](../roadmap.md).
This plan establishes the command behavior needed by the
[D003b keymap plan](d003b-keymaps-plan.md). See the current
[command architecture](../architecture/commands.md),
[extension host](../architecture/extension-host.md), and
[architecture decisions](../decisions.md).

## Outcome and design

A shortcut, palette choice, menu action, or script sends one semantic command to
its originating window. The window starts dispatch on the focus path captured
at invocation: the focused view receives the command first, then a decline
bubbles it to the enclosing workbench and application. The dispatcher does not
choose a handler or decide applicability from the command name, focused surface
kind, or a global handler table before sending the command to the window. A
handler claims with the existing structured outcome or declines; if every
participant declines, the command is unavailable.

Once admitted for dispatch, the window receives the command in that turn.
Asynchronous handlers may finish later, but their continuation validates the
captured view and lifecycle before applying results. A later focus change never
redirects an admitted command.
The command palette retains the focus path from before it took focus. Native
menu actions and scripts enter the same window dispatch path.

The application owns global command definitions and invocation admission. The
window owns focus-path dispatch, while each view, workbench, and application
participant decides whether it handles the command when it receives it. A
command name identifies one semantic operation; native and extension-backed
views follow the same claim-or-decline contract. Extension handlers and their
in-flight work are bound to the owning view and lifecycle. A view participates
because it is focused, not because its content is a tree. This does not require
an arbitrary extension UI framework.

Completion requires every implemented command to use this window-first path,
including native view, workbench, application, and extension commands. No
implemented command may retain a parallel dispatch path or product-shell
applicability switch. Discoverable commands whose behavior is not implemented
may remain unavailable.

Use `copy` as the one public command name for the end-to-end example. There are
no users to preserve compatibility for: remove `editor.copy` rather than keeping
an alias, and do not retain compatibility shims for other renamed commands.
Copy acts on meaningful selected content. A view with nothing to copy
declines; the workbench or application handles it only if it has its own
meaningful copy operation. Otherwise the outcome is unavailable. Do not make
terminal copy mean the whole visible screen or editor fallback mean the current
line merely to force a claim.

## Exclusions and deferred decisions

- Key binding registration, precedence, remapping, and removal belong to
  [D003b](d003b-keymaps-plan.md).
- This plan does not create a general declarative extension UI system or make
  raw text input, IME, pointer events, or terminal byte input into commands.
- Argument schemas, command aliases, and general handler priorities are
  deferred.

## Checkpoints

### 1. Window-first routing contract 🔄

- ✅ Use one participant result for decline, finished outcome, or pending work.
  The dispatcher admits and settles invocations; the focused view, workbench,
  and application decide which commands they claim. A pending participant
  starts work against the captured target after the window dispatch turn.
- ✅ Replace the provisional preselected native handler tables with dispatch into
  the captured window focus path. Let participants claim or decline after
  receiving the command; preserve structured outcomes and captured-target
  validation for asynchronous continuations.
- ✅ Remove the `editor.copy` alias and the provisional terminal-screen and
  editor-line copy behavior. Route `copy` through an editor selection and a
  second focused native surface with meaningful selected content; a focused
  terminal requires terminal selection support before it can claim `copy`.
- ✅ Exercise bubbling through a real workbench or application handler without
  inventing copy behavior solely for a fallback. Keep key, palette, menu, and
  script invocation on the same window dispatch path.
- ✅ Test claim order, decline and bubbling, unavailable commands, focus changes,
  stale targets, asynchronous target validation, and equivalent outcomes across
  invocation sources.
- **Review gate:** confirm the window-first path and public `copy` behavior
  before extending the handler protocol.

### 2. Extension-owned view handlers ⬜

- ✅ Share one foreground command catalog between product discovery and the
  extension invocation bridge; keep native command names in the product command
  definitions used by bindings and handlers.
- Allow an extension-backed focused view to receive and handle an existing
  command through the same window focus path, using Knot-owned view and
  lifecycle identities. Keep JavaScript execution asynchronous and painting
  independent of it.
- Remove handlers on disposal, failed startup, or unload. Reject late results
  against a replaced view or ended lifecycle.
- Test a focused extension-backed view handling `copy`, declining through the
  remaining participants, failure isolation, and exact cleanup after unload.
- **Review gate:** confirm extension views are ordinary routing participants and
  that no tree-specific command rule leaked into the public contract.

### 3. Product integration and architecture record ⬜

- Inventory every implemented command and move all remaining native view,
  workbench, application, and extension command paths onto window-first
  bubbling. Remove parallel dispatch and product-shell applicability switches.
  Preserve protected closure and captured-target validation.
- Cover the complete implemented-command inventory at the appropriate model
  and integration boundaries, with product-level tests for representative
  editor, terminal, workbench, and extension commands and palette and menu
  entry points.
- Update the command and extension-host architecture references and record only
  validated decisions in `docs/decisions.md`.
- **Review gate:** verify that every implemented command enters the window-first
  path, then review the regression evidence before D003b changes key bindings.

### 4. Remove superseded command experiments and legacy paths ⬜

- Inventory production command entry points, catalogs, bridges, handlers,
  aliases, and diagnostic fixtures against the final window-first path. Remove
  the preselected native handler tables, native-versus-extension dispatch split,
  duplicate catalog or queue machinery made obsolete by the new routing model,
  `editor.copy` compatibility code, and provisional copy behavior. Do not leave
  an alternate production path behind a feature flag or diagnostic entry point.
- Remove command-routing experiment scaffolding, including runtime fixture
  paths used only to exercise the old design. If a fixture also covers another
  supported feature, move that coverage to focused tests before deleting the
  runtime path. Replace tests that assert legacy behavior with tests of the
  final contract; retain test fixtures that exercise supported behavior.
- Search production code, tests, bindings, menus, and documentation for removed
  names and paths. Verify that every remaining command entry point reaches the
  captured window focus path and that no old routing implementation remains
  reachable or compiled.
- **Review gate:** approve the removal inventory and reference audit, then run
  the full command, product, and extension regression suites before closing
  D003a and starting D003b.
