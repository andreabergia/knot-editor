# Commands and keymaps

Part of the [architecture](../architecture.md). [Decisions](../decisions.md)
records rationale; [D003a](../plans/d003a-command-routing-plan.md) tracks the
remaining routing work.

## Dispatch

The application owns one foreground catalog for command definitions, name
ownership, and invocation admission. Keybindings, palette choices, native menus,
and scripts carry the same Knot-owned command and structured outcome. The
palette retains the origin captured before it takes focus.

Each native product invocation captures its window, focus, workbench, tab, and surface.
The window routes the command to the captured focused view, then the workbench,
then the application. A participant declines, finishes with an outcome, or
starts pending work. If all decline, the command is unavailable. Focus changes
cannot redirect an admitted command; closed or replaced targets are invalid.
Pending work revalidates its captured target before applying results.

The workbench exposes the tab's typed command view to this routing path.
Participants decide which names and arguments they handle. Raw text input,
IME, pointer events, scrolling, and terminal bytes remain view input protocols.

## Extension execution

The foreground extension command bridge and pooled V8 runtime preserve serial
root trees, captured buffers, structured outcomes, cancellation, and nested
composition. Same-lifecycle children run within their parent's scheduler root;
native and cross-lifecycle children await foreground outcomes. Extension
registrations and pending work are tied to their lifecycle.

Command outcomes report execution status, not semantic values. Direct JavaScript
functions remain available when command lookup and focus routing are unnecessary.
Keymap loading, argument schemas, aliases, and general handler priorities remain
deferred.
