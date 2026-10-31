# Commands and keymaps

Part of the [architecture](../architecture.md). [Decisions](../decisions.md)
records rationale. Public API contracts live in [knot.d.ts](../../src/host/js/knot.d.ts).

## Dispatch

The application owns one foreground catalog for definitions, name ownership,
and admission. Keys, palette, menus, and scripts invoke the same semantic commands
and structured outcomes. The palette captures its origin before taking focus.

Each definition carries a title, an optional description, and whether the
palette lists it; native input commands such as cursor movement opt out, and
extensions pass the same metadata to `commands.register`. The palette shows
each command's title, description, and the binding shown for the origin's view
kind: the argument-less binding with the fewest keystrokes, then the shortest
text, then the latest write. It matches the command name without showing it.
Commands confirmed in the palette form a most-recently-used list of 50 names in
the application state database; recent commands list first with an empty query
and break equal scores otherwise.

Annotated native handlers declare metadata and typed async signatures; linked
adapters assemble catalog entries and generated name constants. Distinct routing
participants may share a name and metadata. Assembly rejects duplicate
participant entries or conflicting metadata. Dynamic extension ownership remains in the
application catalog. Declaration contracts live in
[commands/declarations.rs](../../src/app/commands/declarations.rs).

An invocation captures window, focus, workbench, tab, surface, and exact presentation.
Routing offers it to the captured focused view, then workbench, then application.
Participants synchronously claim/decline before starting typed async work; all
declining produces unavailable. Raw text, IME, pointer, scroll, and terminal bytes
remain view input protocols. Find commands currently report unavailable.

Focus changes cannot redirect an admitted command. Pending work revalidates the
captured window and presentation before foreground access. Same-pane reorder
preserves authority; replacement or transfer invalidates it even under unchanged
document identity. No foreground borrow crosses an await.

Owned command contexts run short foreground closures in the current dispatch
turn, after the active window update releases its borrow, preserving order with
subsequent key events and raw text input. Only suspended futures move to gpui's
executor. This also keeps extension participant selection synchronous, including
inline children. Dialog and protected-close handlers await semantic completion.

## Keymaps

One application-owned registry retains a preset slot and lifecycle-owned slots,
then derives a merged effective map. The selected preset's bindings are defaults
beneath every extension and personal-config binding. Among those, view-specific
bindings outrank global bindings and the latest write breaks remaining ties.
Removing a lifecycle's ownership reveals earlier writes. An unavailable command
does not select another binding. Null unbinds leave raw input available.

The Knot preset is native data for each platform: `secondary-` entries become
cmd on macOS and ctrl on Windows and Linux, and per-platform tables add
movement keys and, on macOS, the Cocoa Emacs editing keys in editors. On
Windows and Linux every global binding whose first keystroke is ctrl with a
printable character other than backtick is unbound in terminals, so the shell
receives readline and job-control keys, and terminals copy and paste with
ctrl-shift-c and ctrl-shift-v. Named keys such as tab and pageup, and ctrl-`,
stay global because shells do not read them, so tab navigation and New
Terminal work from a terminal.
Extensions register further presets by name, lifecycle-owned like themes. The
`keymap.preset` setting selects one when startup completes and whenever the
setting changes; an unregistered name selects Knot, and installing a preset
replaces the whole slot. A preset binding for an undeclared view kind is
skipped.

GPUI recognizes keys and scoped sequences, then one Knot action consults the map
and enters command dispatch. Scoped prefixes remain local to their views; the
palette retains its own input context. Menus and palette commands bypass keymaps.
Fixed config-error bindings remain available independently of product windows.

Menus show each command's shown global single-keystroke binding through a
binding to the menu action in a context no element declares, so it labels the
menu without dispatching. The menus are set again whenever the keymap changes.
macOS performs a menu's key equivalent only when the window leaves the key
unhandled, so a key that some view unbinds shows no menu key. Show Keybindings
lists every slot with its owner and the owner that overrides it, using the same
winner rule as resolution.

Synchronous JavaScript keybinding calls form an ordered atomic batch per V8 turn.
Changed effective maps rebuild bindings across windows; host-response ordering is
specified in [transport](extension-host.md#handler-declarations-and-transport).

## Extension execution

The foreground bridge owns one serial invocation tree, admitted presentation
authority, buffer capture, cancellation, and completion channels. The pooled
runtime schedules JavaScript. Nested invocation first selects a participant in
the parent's captured window; window-handled outcomes return to the bridge
without dispatching twice. Same-lifecycle children run inline under the parent's
scheduler root; cross-lifecycle children await foreground completion. Each parent
has at most one unfinished child, and ancestry cycles are unavailable.

View-scoped extension handlers bind to live instances of a contributed view
registration. The focused instance claims before global extension handlers reach
the application participant. Disposal, lifecycle teardown, and window closure
remove bindings; late results validate instance and lifecycle.

Contextual buffer/selection access validates the live invocation, caller lifecycle,
target authority, and exact presentation. Roots and children inherit the same
capture. Activation and unrelated callbacks cannot borrow command focus. A valid
nontext target has a null buffer; current selection support is limited to
admitted contributed-view handlers. Explicit buffer access remains independent
of presentation lifetime; clipboard access is application-scoped.

Outcomes report execution status rather than semantic values. Resource APIs return
committed data directly; ordinary functions suit composition that needs return
values without command routing. Cancellation and exactly-once response settlement
belong to [host lifecycle](extension-host.md#disposal-cancellation-and-teardown).
