# Commands and keymaps

Part of the [architecture](../architecture.md). [Decisions](../decisions.md)
records rationale. Public API contracts live in [knot.d.ts](../../src/host/js/knot.d.ts).

## Dispatch

The application owns one foreground catalog for definitions, name ownership,
and admission. Keys, palette, menus, and scripts invoke the same semantic commands
and structured outcomes. The palette captures its origin before taking focus.

Annotated native handlers declare metadata and typed async signatures; linked
adapters assemble catalog entries and generated name constants. Distinct routing
participants may share a name/title. Assembly rejects duplicate participant
entries or conflicting titles. Dynamic extension ownership remains in the
application catalog. Declaration contracts live in
[command_declarations.rs](../../src/app/command_declarations.rs).

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

One application-owned registry retains native, installed-extension, and personal
slots, then derives a merged effective map. Personal bindings outrank extensions,
which outrank native defaults. Within a source, view-specific bindings outrank
global bindings; later registration breaks remaining ties. Removing ownership
reveals lower-priority slots. An unavailable command does not select another
binding. Null unbinds leave raw input available.

GPUI recognizes keys and scoped sequences, then one Knot action consults the map
and enters command dispatch. Scoped prefixes remain local to their views; the
palette retains its own input context. Menus and palette commands bypass keymaps.
Native defaults follow platform modifiers; fixed config-error bindings remain
available independently of product windows.

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
