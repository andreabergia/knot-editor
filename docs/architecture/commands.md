# Commands and keymaps

Part of the [architecture](../architecture.md). [Decisions](../decisions.md)
records rationale; [D003a](../archive/plans/d003a-command-routing-plan.md)
records the completed routing work.

## Dispatch

The application owns one foreground catalog for command definitions, name
ownership, and invocation admission. Keybindings, palette choices, native menus,
and scripts carry the same Knot-owned command and structured outcome. The
palette retains the origin captured before it takes focus.

Each native product invocation captures its window, focus, workbench, tab, and surface.
An extension-backed focused view also contributes its Knot-owned view identity.
The window routes the command to the captured focused view, then the workbench,
then the application. A participant declines, finishes with an outcome, or
starts pending work. If all decline, the command is unavailable. Focus changes
cannot redirect an admitted command; closed or replaced targets are invalid.
Pending work revalidates its captured target before applying results.
GPUI enters the captured window after the current update releases it, during
that update's effect cycle and before the invocation turn returns.

The catalog retains one public definition per command name. An extension can
register a handler for an existing name and a declared view kind. The foreground
binds that handler to each live instance of the kind, while the focused view
claims or declines the command during window dispatch. Disposal, lifecycle
teardown, and window closure remove the corresponding bindings. Extension
requests and late results validate the captured instance and lifecycle.
Globally registered extension commands reach the application participant after
the focused view and workbench have declined. Top-level script invocation
captures the active product window and enters that same dispatch path.
Nested script invocation reuses its parent's captured window. The window selects
the participant before the extension bridge schedules a child or returns an
inline same-lifecycle handler. A window-handled child reports its result to the bridge
without routing the command a second time.

The workbench exposes the tab's typed command view to this routing path.
Participants decide which names and arguments they handle. Raw text input,
IME, pointer events, scrolling, and terminal bytes remain view input protocols.

## Keymaps

The application owns one registry for native, installed-extension, and personal
binding slots. A slot contains an owner, key sequence, optional focused view
kind, and a command or unbind rule. Personal slots outrank extension slots,
which outrank native defaults. Within one source, a view-specific slot outranks
an application-wide slot; later registrations break remaining ties. Unloading
a lifecycle removes its slots. The registry resolves one binding before command
routing; an unavailable command does not select another binding.

GPUI recognizes keys in product windows and invokes one Knot keymap action per
effective key and scope. The action uses the focused view kind to resolve the
registry and enters the captured command dispatcher. GPUI contexts limit scoped
bindings to their views, so unrelated input and sequence prefixes reach their
original view. The command palette retains its own input context. Every changed
effective map rebuilds GPUI bindings from the registry, plus fixed bindings for
the config-error window, across all windows.

## Extension execution

The foreground extension command bridge and pooled V8 runtime preserve serial
root trees, captured buffers, structured outcomes, cancellation, and nested
composition. Same-lifecycle children run within their parent's scheduler root;
window-handled and cross-lifecycle children await foreground outcomes. Extension
registrations and pending work are tied to their lifecycle.

Command outcomes report execution status, not semantic values. Direct JavaScript
functions remain available when command lookup and focus routing are unnecessary.
JavaScript keymap mutation, argument schemas, aliases, and general handler
priorities remain deferred.
