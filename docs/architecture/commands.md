# Commands and keymaps

Part of the [architecture](../architecture.md). Design rationale is recorded in
[decisions](../decisions.md); deferred work lives in [roadmap.md](../roadmap.md).

## Dispatch and captured targets

`CommandCatalog` owns discovery metadata and name ownership for native and
extension commands. Extension definitions bind their names to one lifecycle.
Native participants receive Knot-owned `Command` values containing a stable
name and explicit JSON-like arguments.

Keybindings and the command palette enter the product dispatcher. The product
application owns one native catalog and dispatcher across all product windows.
Admission allocates the invocation identity, captures the origin, and creates
the completion before routing begins:

```text
keybinding / palette
                   |
                   v
        admitted Command + captured origin
                   |
                   v
       captured product window
                   |
                   v
      focused view -> workbench -> application
                   |
             claim or decline
```

Dispatch captures the originating window, weak shell and focus identities, and
optional surface-associated buffer. These native target identities remain
inside `app`.

Product dispatch additionally captures the exact workbench, pane, tab, and
surface kind and identity selected at admission. Native menu actions, fixed
keybindings, the product palette, and toolbar actions all submit the same `Command` value and
captured product target. Dispatch is deferred and every
source receives the same asynchronous structured outcome. Before execution,
the dispatcher revalidates the captured window, shell, focus, workbench, pane,
tab, and surface identities. A focus change therefore cannot retarget an
operation, and a closed or replaced target completes as invalid. Document-only
commands on a live terminal tab complete as unavailable. New Terminal opens a
session in the captured pane; split commands create an independent session
when that pane's captured tab is a terminal. Close Tab, Close Window, and Quit
use protected mixed-surface closure. Move Terminal to New Window requires a
captured terminal tab and revalidates its session and view after opening the
destination. It keeps the sole session presentation and returns unavailable on
a document tab or invalid target after a stale capture. Commands whose behavior belongs to a later
product checkpoint remain discoverable and complete as unavailable.

The product `copy` definition reaches the captured focused view first. An editor
copies its selection, and a terminal copies its selected cells. Either declines
without a selection. The workbench may claim commands with its own operation;
the application claims the extension startup report command. A command that no
participant claims is unavailable. `copy` is the public name in discovery,
bindings, menus, and scripts.

Ordinary product editing uses this same captured-view path: character, word,
line, page, and document movement and selection, newline and tab insertion,
deletion, undo and redo, and native clipboard operations are registered editor
commands. Undo and redo resolve through the captured editor to its shared
document model; switching focus cannot retarget them. Explicit commands end an
active typing group, while repeated directional deletion commands retain their
group until another interaction boundary.
Text supplied by the platform and IME composition enter the view's input
handler; pointer selection and scrolling remain view input protocols.

The foreground extension command bridge and pooled V8 command runtime preserve
serial root trees, captured buffers, structured outcomes, cancellation, and
nested composition. Same-lifecycle children execute inline in their parent's
scheduler root; native and cross-lifecycle children await an explicit
foreground outcome. Product ownership of the pool and transport loop remains
for D017's final integration checkpoint. The product-facing request adapter
already awaits the ordinary dispatcher outcome, reports a missing captured
target as invalid, and rejects nested product requests as unavailable.

The command palette keeps the weak focus target captured before the palette
takes visible focus. Palette controls target the palette, while confirmation
dispatches the selected command at the preserved origin without refocusing it.
A missing origin is rejected rather than replaced with current focus.

Focus-owning editor and palette views publish semantic gpui key contexts.

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
