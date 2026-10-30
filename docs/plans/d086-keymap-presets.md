# D086 — Keymap presets

Status: 🚧 approved, not started. Item of the
[daily-driver plan](daily-driver.md); depends on D070 (settings) and D071
(palette); promotes D060 (audited platform defaults). D088 fills in the other
presets.

## Goal

The default bindings are a named, selectable preset rather than a fixed native
table. Knot ships its own preset, redesigned from scratch and correct on macOS,
Windows, and Linux. Packages and personal config can contribute further presets,
and selecting one rebinds every window. A Show Keybindings listing shows the
effective keymap with each binding's owner and what it shadows, and the
application menus show the bindings that are currently in effect.

## Design

Presets follow the [themes](../decisions.md#themes) precedent.

- **One preset slot.** The native `BindingOwner::Native` slot becomes the
  preset slot. It holds the selected preset's bindings, which sit beneath every
  extension and personal-config binding, as native bindings do today
  ([commands](../architecture/commands.md#keymaps)). Switching presets replaces
  the whole slot, so the ordering rule does not change.
- **Knot is native data.** The Knot preset stays a Rust table, so typing, Save,
  Quit, and the palette work before packages start and when they fail, as the
  native color defaults do. It is always registered under `"knot"` and cannot
  be replaced.
- **Other presets are contributions.** `keymaps.registerPreset({ name, title,
  bindings })` registers a lifecycle-owned preset. The latest registration of a
  name wins, and teardown reveals the previous one. There is no `extends`: a
  derived preset spreads `keymaps.getPreset("knot")` and changes a few entries.
  `keymaps.listPresets()` lists names and titles. A binding is `{ key, command,
  view? }`, as in `keybinding()`.
- **Selection is a setting.** `keymap.preset` is a natively defined string
  setting with the default `"knot"`. An unregistered name falls back to Knot.
  As with themes, the preset resolves when startup completes and when the
  setting changes, not on every registration, so package load order never
  decides what the first keystroke does. A Select Keymap Preset command is
  D088's.
- **The keybinding API moves under `keymaps`.** `keybinding` and
  `removeKeybinding` become `keymaps.bind` and `keymaps.unbind` beside the
  preset operations. No compatibility path is kept.
- **Platform detection.** `knot.platform` is a constant, `"macos" | "windows"
  | "linux"`, available to packages and personal config. Preset code branches
  on it instead of the registration format carrying per-OS fields.
- **Platform-correct native tables.** The Knot preset uses gpui's
  `secondary-` modifier (cmd on macOS, ctrl elsewhere) for shared bindings, and
  small per-OS tables hold the rest: word movement, document start and end,
  Redo, and the macOS cmd-arrow keys. This also fixes Linux, which today
  receives the macOS `cmd-*` table.
- **Knot preset conventions:**
  - `secondary-k` remains the chord leader.
  - The terminal follows VS Code and Zed. On Windows and Linux, copy and paste
    are terminal-scoped `ctrl-shift-c` / `ctrl-shift-v`. Global `ctrl-*`
    bindings that a shell or TUI needs are unbound in the terminal scope so
    the key reaches the PTY, using exactly the keys those editors pass through.
    On macOS, `cmd-*` never reaches the shell, so nothing changes.
  - On macOS, the editor gets the Cocoa text-system Emacs keys: `ctrl-a/e`
    (line start and end), `ctrl-f/b/n/p` (character and line movement),
    `ctrl-d` (delete forward), `ctrl-h` (delete backward), and `ctrl-k`
    (delete to line end). Commands missing for these, such as delete to line
    end, are added here.
  - Commands added by later items bind into the Knot preset when they land.
    D086 only rebinds the existing command set.
- **Show Keybindings.** A native command opens the shared picker with one row
  per effective binding: the key as the label, the command title, and the owner
  (preset name, package, or personal config) and view scope as detail. Slots
  that a higher binding shadows are listed with the owner that overrides them.
  Accepting a row runs its command, as the palette does. Conflicts are not
  warnings, because latest-write-wins is the rule rather than an error.
- **Menu shortcuts.** Menu items dispatch `ProductCommandSource` actions, while
  keys dispatch `KeymapAction`, so menus show no key equivalents today. For each
  menu command, `rebuild()` also registers its shown binding against the menu
  action in a context that never matches dispatch, and then sets the menus
  again, so gpui's macOS menu displays the effective key without that
  binding ever firing.

## Exclusions

- The VS Code, Zed, IntelliJ, and Sublime presets, the Select Keymap Preset
  picker, and any first-launch choice (D088).
- A pending-chord indicator (it needs the status bar, D077).
- Key contexts and ordered key delivery for modal editing (D110).
- Bindings for commands that later items add.
- Physical application-keypad keys (D040).

## Deferred decisions

- Whether `@knot/keymaps` exists before D088. This plan tests contributed
  presets with an example or test package, and D088 creates the bundled
  package.
- Whether Show Keybindings grows into a dedicated editable surface. That waits
  for a settings UI (D091).

## Checkpoints

### 1. Platform-correct Knot preset

- Replace `COMMON_BINDINGS` / `PLATFORM_BINDINGS` with the redesigned Knot
  preset: `secondary-` shared bindings plus macOS, Windows, and Linux tables
  selected by `cfg(target_os)`. Add terminal-scoped copy, paste, and
  pass-through unbinds for Windows and Linux, and the macOS Emacs editing keys
  with any missing editor commands.
- Rename `BindingOwner::Native` to `BindingOwner::Preset`, still built from the
  native table.
- Validation: per-platform table tests (each table loads, has no duplicate key
  and scope, and every binding names a registered command); editor tests for
  the new commands; product keybinding tests for terminal copy and paste and a
  shell pass-through key.
- Review gate: walk through the Knot preset tables for each platform.
- **Implementation context:** `src/app/commands/keymaps.rs` (tables,
  `native_registry`, `install`, config-error quit binding),
  `src/app/commands/keymaps/tests.rs` (extend
  `windows_native_defaults_use_control_shortcuts`). Command constants live in
  `src/app/commands/dispatch.rs`, and editor commands are annotated with
  `#[command]` beside the editor view (mirror an existing movement command).
  The terminal commands are `TERMINAL_COPY_COMMAND` /
  `TERMINAL_PASTE_COMMAND` in `src/app/terminal/view.rs`, and a null
  terminal-scoped binding leaves raw input to `src/app/terminal/input.rs`.
  Product key tests are in `src/app/product/tests/keybindings.rs`.

### 2. Contributed presets, selection, and `knot.platform`

- Add a preset registry (lifecycle-owned registrations, latest name wins,
  `"knot"` reserved and native) and the `keymap.preset` setting. Resolution
  installs the selected preset into the preset slot at startup completion and
  on setting changes, then calls `rebuild`. Teardown removes the lifecycle's
  registrations.
- Public API: `keymaps.registerPreset`, `getPreset`, `listPresets`, `bind`, and
  `unbind` (replacing `keybinding` / `removeKeybinding`), plus `knot.platform`.
  Update the declarations, the JSON schema if it applies, the fake host, the
  examples, and every bundled caller.
- Validation: registry unit tests (shadowing, teardown reveal, fallback to
  Knot, the reserved name); product tests that selecting a contributed preset
  rebinds an open window, that extension and personal bindings still win over
  it, and that switching back restores Knot; fake-host contract test; a
  `knot.platform` test.
- Review gate: API shape and resolution timing.
- **Implementation context:** mirror `src/app/themes.rs` (registry with owner
  and lifecycle, `apply_at_startup` observing `ApplicationSettings`,
  `remove_lifecycle`) and its handlers in
  `src/app/extensions/host/handlers.rs` (`register_theme`, `get_theme`,
  `list_themes`). Call startup resolution beside the theme calls in
  `src/app/product/launch.rs` and `src/app/extensions/host.rs`. Define the
  setting with the others in `src/app/settings.rs`. On the JS side, update
  `src/host/js/knot.js`, `src/host/js/bootstrap.js` (mirror `themes`), and
  `src/host/js/knot.d.ts`; update the fake host in `scripts/extensions/testing/`
  and the contract test in `scripts/extensions/contract.test.ts`. Existing
  `keybinding` batch handling stays in `BindingRegistry::apply_batch`. Teardown
  goes through the shared foreground cleanup that already calls
  `themes::remove_lifecycle`.

### 3. Show Keybindings

- A native command lists the effective and shadowed bindings in the shared
  picker, with owner and scope detail, and runs the accepted command.
- `BindingRegistry` gains a listing query that returns every slot with its
  owner, view, and whether it is effective.
- Validation: registry tests for the listing (effective versus shadowed,
  owner labels); a product test that the picker opens with the expected rows.
- Review gate: try the listing with a contributed preset and a personal
  override.
- **Implementation context:** `BindingRegistry` in
  `src/app/commands/keymaps.rs` (`shown_bindings` shows the ranking style).
  Picker usage is in the native pickers under `src/app/product/`
  (`language_mode.rs` and `recovery_listing.rs` are small examples). Owner
  labels come from the extension identity behind
  `BindingOwner::Extension`. Register the command beside other native product
  commands in `src/app/product/commands.rs`.

### 4. Menu shortcuts

- Spike first: confirm on macOS that a menu-action binding in a never-matching
  context shows as the key equivalent and that pressing the key dispatches once,
  through `KeymapAction`.
- `rebuild()` registers display bindings for menu commands from
  `shown_bindings(None)` and sets the menus again.
- Validation: a unit test that the display bindings follow the effective map
  (including after a preset switch); manual macOS check.
- Review gate: menus show the right keys after switching preset and after a
  personal override.
- **Implementation context:** menus are built in `src/app/product/launch.rs`
  (`MenuItem::action` with `ProductCommandSource`). gpui picks the key
  equivalent in `gpui-0.2.2/src/platform/mac/platform.rs` (it prefers bindings
  whose predicate matches a default context, else the first one). Factor the
  menu construction so `keymaps::rebuild` can call `cx.set_menus` again.

### 5. Documentation and completion

- Update [commands](../architecture/commands.md#keymaps) (preset slot,
  selection, platform tables, menus, Show Keybindings), add the decision to
  [decisions](../decisions.md#commands-and-keymaps) (presets as contributions
  over a native Knot preset, platform constant, terminal pass-through
  following VS Code and Zed), and update the
  [extension guide](../extension-api.md) for `keymaps` and `knot.platform`.
- Review gate: remove this plan and mark D086 ✅ in the daily-driver plan.
