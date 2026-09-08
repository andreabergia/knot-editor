# Commands and keymaps

Part of the [architecture](../architecture.md). Design rationale is recorded in
[decisions](../decisions.md); deferred work lives in [deferred.md](../deferred.md).

## Dispatch and captured targets

`CommandCatalog` owns discovery metadata and name ownership for native and
extension commands. Extension definitions bind their names to one lifecycle;
native handlers remain attached to gpui views and shells rather than moving
into the catalog. Both use Knot-owned `Command` values containing a stable name
and explicit JSON-like arguments.

Keybindings, the command palette, and top-level scripts enter the dispatcher
owned by their application mode. The product application owns one native
catalog and dispatcher across all product windows; the explicit fixture shell
retains its extension-capable dispatcher. Admission allocates the invocation
identity, captures the origin, and creates the completion before routing
begins:

```text
keybinding / palette / top-level script
                   |
                   v
        admitted Command + captured origin
                   |
                   v
       serialized root FIFO queue
                   |
                   v
      gpui action at captured focus
                   |
       focused view -> enclosing shell
                   |
          native handler or extension runtime
```

Dispatch captures the originating window, weak shell and focus identities, and
optional surface-associated buffer. These native target identities remain
inside `app`; extensions receive the invocation identity, arguments, and an
optional opaque buffer handle. gpui routes the private action from the captured
focus target, so editor, tree, and terminal handlers can claim the same command
without a shared surface type. Unclaimed extension commands reach the shell as
the global destination.

Product dispatch additionally captures the exact workbench, pane, tab, and
document selected at admission. Native menu actions, fixed keybindings, the
product palette, toolbar actions, and the product V8 bridge all submit the same
`Command` value and captured product target. Dispatch is deferred and every
source receives the same asynchronous structured outcome. Before execution,
the dispatcher revalidates the captured window, shell, focus, workbench, pane,
tab, and document identities. A focus change therefore cannot retarget an
operation, and a closed or replaced target completes as invalid. Commands
whose behavior belongs to a later product checkpoint remain discoverable and
complete as unavailable.

One shell runs one root invocation tree at a time. Additional roots remain in
FIFO order until the active root and its attached descendants settle. A
handler-originated invocation is attached as one child of its active parent
and inherits the parent's captured window, workspace, focus, and optional
buffer. Native children use the same gpui focus route. Cross-extension
children run on their owning runtime while the caller remains suspended;
unrelated roots cannot enter that gap. A second unfinished child from the same
parent is rejected, so this remains command composition rather than a task
graph.

The command palette keeps the weak focus target captured before the palette
takes visible focus. Palette controls target the palette, while confirmation
dispatches the selected command at the preserved origin without refocusing it.
A missing origin is rejected rather than replaced with current focus.

Extension execution retains the inherited captured context for the invocation
lifetime. Immediately before execution and every delayed foreground mutation,
the shell revalidates the invocation, lifecycle, window and shell ownership,
focus target, and optional buffer. Every admitted invocation completes once
with a structured outcome.

The catalog resolves global ownership before choosing how a child runs. A
same-extension child executes as a nested JavaScript handler frame in the
current serial callback. Cross-extension ancestry is checked before dispatch;
targeting a runtime already occupied by an ancestor is unavailable rather than
deadlocking. Repeating a same-extension command registration in its active
ancestry is rejected. Root cancellation propagates downward, actively aborts
extension handler signals, rejects late foreground mutations, and waits for
the invocation tree to settle before the next root starts. Cancelling a child
does not cancel its parent. Runtime or shell teardown settles affected work and
cannot overwrite an existing terminal outcome.

Focus-owning editor, tree, terminal, and palette views publish semantic gpui
key contexts. Fixed bindings exercise base, focused-surface, persistent active,
one-shot transient, and multi-keystroke routing. Central dispatch consumes the
transient context; cancellation clears it without invoking a command.

## Composition semantics and limits

- Command handlers still do not return semantic values. A handler's successful
  return completes its invocation; unsuccessful children affect a parent only
  when its JavaScript branches or throws. Direct JavaScript functions remain
  preferable when global command lookup, focus routing, or cross-extension
  reuse is unnecessary.
- Configurable keymap loading, extension-defined bindings, argument schemas,
  aliases, macros, repetition, detached children, concurrent roots, priorities,
  and general task-graph scheduling remain deferred.
- IME, text insertion, pointer motion, scrolling, focus changes, and other raw
  input protocols remain outside the registered command model.

Native definitions reserve their names. Handlers validate arguments; argument
schemas and generated argument UI remain deferred.
