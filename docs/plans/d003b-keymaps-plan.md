# D003b: JavaScript-configurable keymaps

Status: in progress. Checkpoint 1 implementation is ready for review.

Source: D003, promoted from the [deferred-work register](../roadmap.md).
Depends on the [D003a focused command-routing plan](../archive/plans/d003a-command-routing-plan.md).
See the current [command architecture](../architecture/commands.md),
[personal config plan](../archive/plans/d036-personal-config-plan.md), and
[architecture decisions](../decisions.md).

## Outcome and design

Personal config and installed extensions can add, change, and remove bindings
while Knot runs. Scripts import the public root module with
`import * as knot from 'knot'`. The common form is
`knot.keybinding('Cmd-C', 'copy')`: an application-wide key invokes a semantic
command, whose focused handler is selected by D003a. A binding may optionally
target one named focused view kind with `{ view: 'editor' }` when the key itself
needs different meaning there. Native kinds include `editor`, `terminal`, and
`workspace-tree`; extension-backed views use their declared kind. Native kind
names are reserved. Tree has no special keymap category. Application-wide means
product-window command surfaces; the command palette and config-error window
retain their own input behavior. Multi-keystroke sequences remain
supported through gpui's key input path. An optional command value carries
explicit arguments through the existing dispatcher.

One application-owned registry holds native defaults, extension bindings, and
personal bindings as slots keyed by owner, key sequence, and optional view kind.
Native defaults use the same set and remove machinery as other sources;
extension and config owners are their lifecycles. Setting the same slot again
replaces it, and `knot.removeKeybinding(key, { view })` removes that owner's
slot. A null command sets an unbind rule in the slot, suppressing lower-priority
bindings in its scope; removing it reveals them. These calls return nothing.
Unload and failed startup remove every slot owned by that lifecycle. Effective
binding resolution gives personal config priority over extensions and extensions over
native defaults, including bindings added after post-init. Within a source,
view-specific bindings win over application-wide bindings; later registrations
from different owners resolve otherwise equal conflicts. A personal
application-wide binding can
therefore replace a more specific earlier-source binding without guessing its
view selector. gpui recognizes keys and contexts; Knot resolves the winning
slot so gpui's view-depth matching cannot reverse source priority. A binding
resolves to one command; an
unavailable command does not retry a lower-priority binding. Command fallback
is D003a's handler routing.

JavaScript queues ordered set and remove operations during a V8 turn. After the
turn, Knot validates and applies the batch atomically, then rebuilds gpui's
keymap once if the effective bindings changed. An `await` may end a turn, so a
later continuation is a later batch. Batch errors settle before the originating
turn reports completion; config startup cannot become ready with a failed batch.
The application rebuilds gpui's map from its authoritative registry, including
fixed bindings for the config-error window, so removal, replacement, and multiple
product windows stay in sync. Personal config can set bindings in pre-init or
post-init; post-init can override all installed extensions after startup.

## Exclusions and deferred decisions

- No standalone keymap file, settings format, keymap editor UI, general
  context-expression language, modal layer system, or live config-file reload.
- No key bindings for raw text insertion, IME composition, pointer events, or
  terminal byte protocols.
- The exact command-argument syntax is settled at the API review gate. The
  simple two-string call above is the required common case.

## Checkpoints

### 1. Public root module 🟡

- ✅ Replace the `knot:editor` public facade with the sole public `knot` module.
  Preserve the `editor`, `commands`, and `workbench` namespaces under a module
  namespace import; migrate fixtures, tests, and documentation. Remove the old
  specifier without a compatibility alias and keep `knot:bootstrap` private.
- ✅ Test public import resolution and rejection of private and removed specifiers.
- ⏳ **Review gate:** inspect the root module surface and migrated imports.

### 2. Uniform binding registry and gpui adapter ⬜

- Model owner slots, view-kind selectors, unbind rules, precedence, replacement,
  and removal without special native-binding behavior. Migrate fixed editor and
  product shortcuts into the registry.
- Give the command palette a semantic command routed through D003a and replace
  its special gpui action. Keep palette focus and captured-origin behavior.
- Bind keys to a Knot action that resolves the winning slot for the focused view.
  Rebuild gpui's map after effective changes, including the separate fixed
  config-error bindings. Validate sequences and selectors without panics;
  preserve multi-keystroke input and avoid consuming keys outside their scope.
- First verify that a personal global binding beats an editor-specific default
  and an editor-only unbind leaves the terminal binding usable. Then test owner
  replacement and removal, later extension registration, same-source conflicts,
  view kinds, lifecycle cleanup, sequence prefixes, palette invocation, and
  multi-window updates.
- **Review gate:** inspect resolution rules, gpui behavior, and the default inventory.

### 3. JavaScript mutations and lifecycle ⬜

- Expose synchronous, void `knot.keybinding(key, command, options?)` and
  `knot.removeKeybinding(key, options?)`, including null-command unbinding,
  optional view kind, and command arguments. Queue ordered mutations in `host`;
  transport one batch of Knot-owned data after each V8 turn.
- Validate and apply each batch atomically, rebuild at most once per changed
  batch, and settle errors before the turn completes. Make changes effective at
  runtime while preserving fatal personal-config errors and independent extension
  startup failures.
- Test pre-init, extension, and post-init ordering; runtime replacement;
  same-turn coalescing; await boundaries; invalid-input diagnostics and rollback;
  explicit removal; unbinds; failed startup; and lifecycle unload.
- **Review gate:** approve the JavaScript API and turn/error behavior using a
  small real configuration example.

### 4. Product validation and architecture record ⬜

- Verify remapping and unbinding native and extension commands from config in
  the product, including an application-wide `copy` binding routed to different
  focused views. Cover menu and palette invocation independently of keymaps.
- Update the command and extension-host architecture references and record
  validated precedence and ownership choices in `docs/decisions.md`.
- **Review gate:** inspect end-to-end behavior and regression coverage before
  marking D003 complete.
