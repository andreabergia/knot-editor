# D003b: JavaScript-configurable keymaps

Status: proposed. Design agreed; implementation has not started.

Source: D003, promoted from the [deferred-work register](../roadmap.md).
Depends on the [D003a focused command-routing plan](d003a-command-routing-plan.md).
See the current [command architecture](../architecture/commands.md),
[personal config plan](../archive/plans/d036-personal-config-plan.md), and
[architecture decisions](../decisions.md).

## Outcome and design

Personal config and installed extensions can add, change, and remove bindings
while Knot runs. The common JavaScript form is
`knot.keybinding('Cmd-C', 'copy')`: an application-wide key invokes a semantic
command, whose focused handler is selected by D003a. A binding may optionally
target one named focused view when the key itself needs different meaning
there. Tree has no special keymap category. Multi-keystroke sequences remain
supported through gpui's key input path. An optional command value carries
explicit arguments through the existing dispatcher.

One application-owned registry holds native defaults, extension bindings, and
personal bindings as owned entries. Native defaults use the same registration
and removal machinery as other sources; extension and config entries also
carry lifecycle ownership. Effective binding
resolution gives personal config priority over extensions and extensions over
native defaults, including bindings added after post-init. Within a source,
view-specific bindings win over application-wide bindings; later registrations
resolve otherwise equal conflicts. A personal application-wide binding can
therefore replace a more specific earlier-source binding without guessing its
view selector. Knot compiles the effective map so this source priority is not
reversed by gpui's view-depth matching. A binding resolves to one command; an
unavailable command does not retry a lower-priority binding. Command fallback
is D003a's handler routing.

A null command creates an unbind rule that suppresses lower-priority bindings
for the key across views, optionally limited to one view. Disposing the rule reveals those
bindings again. Registration returns a disposable handle; only its owner can
dispose it. Unload and failed startup remove every entry from that lifecycle.
The application rebuilds gpui's effective keymap from its authoritative
registry after changes, so removal, replacement, and multiple product windows
stay in sync. Personal config can register in pre-init or post-init; post-init
is where users can override all installed extensions after startup.

## Exclusions and deferred decisions

- No standalone keymap file, settings format, keymap editor UI, general
  context-expression language, modal layer system, or live config-file reload.
- No key bindings for raw text insertion, IME composition, pointer events, or
  terminal byte protocols.
- The exact JavaScript import/export shape, spelling of the optional view
  selector, and command-argument syntax are settled at the API review gate.
  The simple two-string call above is the required common case.

## Checkpoints

### 1. Uniform binding registry and effective map ⬜

- Model registrations, owner/lifecycle, view selector, unbind rules, precedence,
  and disposal without special native-binding behavior. Migrate fixed defaults
  into the registry, including editor and product shortcuts.
- Rebuild the gpui map after mutations; validate key sequences and selectors
  without panics. Preserve focus-specific matching and multi-keystroke input.
- Test user replacement and removal of native and extension bindings, later
  extension registration, same-source conflicts, view-specific bindings,
  disposal revealing lower entries, and multi-window updates.
- **Review gate:** inspect effective-map rules and the default-binding inventory.

### 2. JavaScript registration and lifecycle ⬜

- Expose the simple `knot.keybinding(key, command)` call, optional view and
  command arguments, null-command unbinding, and a disposable registration.
  Route requests through Knot-owned transport; keep V8 values inside `host`.
- Make changes effective at runtime and ensure config startup awaits or reports
  registration errors before product readiness. Preserve fatal personal-config
  errors and independent extension startup failures.
- Test pre-init, extension, and post-init ordering; runtime replacement;
  invalid input diagnostics; disposal; failed startup; and lifecycle unload.
- **Review gate:** approve the JavaScript API and startup/error behavior using
  a small real configuration example.

### 3. Product validation and architecture record ⬜

- Verify remapping and unbinding native and extension commands from config in
  the product, including an application-wide `copy` binding routed to different
  focused views. Cover menu and palette invocation independently of keymaps.
- Update the command and extension-host architecture references and record
  validated precedence and ownership choices in `docs/decisions.md`.
- **Review gate:** inspect end-to-end behavior and regression coverage before
  marking D003 complete.
