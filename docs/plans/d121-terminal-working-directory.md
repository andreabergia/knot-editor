# D121 — Terminals start in the workspace root

Status: 🚧 checkpoints 1–2 complete; checkpoint 3 next. Item of the
[daily-driver plan](daily-driver.md); no dependencies. D127 builds on it.

## Goal

A new terminal starts in the folder the user is working in rather than in the
Knot process's working directory, which is `/` when Knot is launched from
Finder. Extensions can open a terminal in a directory of their choosing.

## Design

- **Default directory.** Computed by the window's shell when it creates a
  terminal:
  1. The root that contains the window's most recently focused document,
     choosing the innermost when roots are nested.
  2. Otherwise, the first root.
  3. Otherwise, the home directory.
  Only `file` roots that still exist as directories count. A root that is not a
  local directory, or a deleted one, is skipped in favor of the next step. Home
  is the last fallback, which is why a window without roots never uses the
  active document's directory. Revisit that if daily use misses it.
- **Most recently focused document.** The shell remembers the document of the
  last focused document tab in the window, so opening a terminal while a
  terminal or tool has focus still follows the file being edited. A document
  that has been closed, or one with no resource URI, contributes nothing.
- **Every creation path uses it.** New Terminal, splitting a terminal pane, and
  terminal tabs restored by hot exit or a project switch all go through
  `create_terminal`. Restored windows install their roots before applying the
  layout, so restored terminals see the right roots. D127 later replaces the
  default for restored tabs only.
- **Splitting from a terminal** uses the same default. Inheriting the source
  shell's current directory needs D127's mechanism.
- **Restart** keeps the directory the session was created with, because
  `TerminalSession` keeps its `Options`.
- **Extension API.** `terminals.open(options?)` opens a terminal tab and
  resolves its `PresentationInfo`. The options are:
  - `cwd`: a directory URI. Omitting it uses the default above.
  - `target`: a `WindowTarget`, `PaneTarget`, or `ViewTarget` destination, as
    in `documents.open`.
  A docked target opens the terminal in the normal group, as New Terminal
  does. `cwd` must be an existing local directory and is checked through the
  registered providers. A non-`file` URI rejects with `InvalidUri`, a missing
  path with `ResourceNotFound`, and a file with `ResourceWrongKind`.

## Exclusions

- A `terminal.workingDirectory` setting: terminal settings are D045.
- Following the shell's current directory (OSC 7 or process inspection) and
  restoring it: D127.
- Environment variables or a shell command in `terminals.open`. Add them when
  a consumer such as tasks (D125) needs them.

## Deferred decisions

- Whether a window without roots should use the active document's directory
  instead of home, as most editors do. Home until daily use says otherwise.

## Checkpoints

### 1. Native default directory ✅

- Approved. `ProductShell::create_terminal` resolves the directory through
  `terminal::directory::working_directory` for New Terminal, split, and
  restore. The last focused document is recorded from the shell's `on_focus`
  subscriptions, and also when a subscription is installed on a view that
  already has focus, because gpui sends that view no focus event.
- Covered by the resolver's unit tests and product tests in
  `src/app/product/tests/terminal.rs`. Those tests read the session's options
  rather than asking the spawned shell for its directory.

### 2. `terminals.open` extension API ✅

- Approved. The `openTerminal` host operation in
  `src/app/extensions/host/handlers.rs` validates `cwd` through
  `product::operations::resolve_terminal_directory`, then opens the tab with
  `open_terminal`. The contract is declared in `src/host/js/knot.d.ts` and
  described in the [extension guide](../extension-api.md). The fake host in
  `scripts/extensions/` reports it as not faked.
- Covered by a product test in `src/app/product/tests/operations.rs`. Review
  found that environment variables or a shell command for tasks (D125) can be
  added later as optional fields without breaking the API.

### 3. Documentation

- Deliverables:
  - The terminal sessions section of
    [workbench and lifecycle](../architecture/workbench-and-lifecycle.md#terminal-sessions-and-views)
    describes the default directory and the restored-tab behavior.
  - The terminal decision in [decisions](../decisions.md) records why the
    focused document's root wins and why rootless windows use home.
  - D121 is marked complete in the index plan and this plan is removed.
- Validation: links resolve; no design detail is left only in this plan.
