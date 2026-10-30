# D071 — Command palette and shared picker

Status: 🚧 in progress. Item of the
[daily-driver plan](daily-driver.md); depends on D064 (themes, complete).

## Goal

One native picker serves the command palette, every native picker, and
`workbench.showPicker`. It fuzzy-matches with highlighted characters, and later
pickers (Go to Line, Go to File, recent files, the project selector) inherit it
unchanged. The palette lists commands by readable title with a description and
their effective binding, hides low-level input commands, and ranks recently
used commands first across restarts. Extensions can label and describe their
commands, give picker items more structure, and preview the highlighted item;
Select Theme previews themes live.

## Design

- **One picker widget.** A native `Picker` owns the query field, matching,
  scoring, highlighting, selection, and row rendering. The palette, the native
  pickers (Set Language Mode, Restart Extension, Recover Unsaved Documents…),
  and extension pickers all build `Picker` items. The palette keeps its own
  slot, independent of the per-window extension interaction slot
  ([decisions](../decisions.md#extension-transient-interactions)).
- **Fuzzy matching with `nucleo-matcher`.** It provides fzf-compatible scoring
  with word-boundary, camelCase, separator, and consecutive bonuses, smart case,
  Unicode normalization, a path mode for D082, and match indices for
  highlighting. It is MPL-2.0, used unmodified as a dependency. The label is
  the primary haystack and the description the secondary one, and both
  highlight. An item may also carry extra match text that is never shown, and
  the palette uses it for the command name, so `projects.sw` still finds Switch
  Project. An empty query keeps the caller's order. A non-empty query sorts by
  score, and ties keep the caller's order.
- **Picker items.** Each item has `label`, optional `description` (inline and
  muted), optional `detail` (a muted second line), and optional `hint`
  (right-aligned, used by the palette for bindings).
- **Highlight preview.** `PickerOptions.onHighlight?: (id: string | null) =>
  void | Promise<void>` fires for the first highlighted item and whenever the
  highlighted item changes. Package roots are serial, so a notification root
  could not run while the caller's command is still awaiting the picker.
  Instead, `bootstrap.js` implements `showPicker` as a loop inside the caller's
  root: it opens the picker, then repeatedly awaits a host request that settles
  with either the next highlight or the final result, and awaits `onHighlight`
  before asking again. Native code keeps only the latest unread highlight, so a
  slow callback naturally coalesces. Without `onHighlight`, the loop asks only
  for the result. Undoing a preview is the caller's job.
- **Command metadata.** `commands.register(name, handler, options?)` takes
  `{ title?, description?, palette? }`. The title falls back to the name and
  `palette` defaults to true. Native `#[command]` gains optional `description`
  and `palette = false`. `CommandDefinition` carries `description` and
  `palette`. The native cursor movement, selection extension, delete, insert
  newline, and insert tab commands set `palette = false`. There are no
  context (`when`) rules: a command that does not apply to its target fails as
  it does today.
- **Palette rows.** Each row shows the title (highlighted), the description,
  and the effective binding as the hint. The binding comes from the keymap
  registry for the focused view kind captured when the palette opens, and only
  argument-less bindings count. With several keys, the one with the fewest
  keystrokes is shown, then the shortest text, and remaining ties go to the
  latest write. Chords render in platform style: `⌘K ⌘S` on
  macOS, `Ctrl+K Ctrl+S` elsewhere. The raw command name is not displayed.
- **Recently used commands.** Every command confirmed in the palette is
  recorded in a most-recently-used list of up to 50 names, persisted in the
  application state database. With an empty query, recent commands come first
  in recency order and the rest follow by title. With a query, recency breaks
  score ties only. Names that are no longer registered are skipped when shown
  and pruned on the next write.
- **One application state database.** Tool layout and window session merge into
  one `state.sqlite3`, and the palette history joins it as a third table. A
  shared `StateDatabase` owns a single connection and serializes writes off the
  foreground. Each record keeps its own cache, version, and debounce, and quit
  flushes every pending record. Damaged or versioned-away records still read as
  absent. `recovery.sqlite3` stays separate because its failure policy (preserve
  and report, never replace), FULL-synchronized captures, and isolation from
  UI-state failures differ. Extension storage stays per package.

## Exclusions

- Palette prefixes and modes (`>`, `:`, `@`). Go to Line (D076) and Go to File
  (D082) are their own commands.
- Context (`when`) rules for hiding inapplicable commands.
- Query-driven or streaming pickers (D118).
- Multi-select pickers, and picker actions beyond selection (D084 decides what
  the project selector needs).
- Recording commands invoked by keys or scripts in the recent list.

## Deferred decisions

- Whether D084's picker actions extend `PickerItem` or use companion commands.
  That is decided in D084.
- Whether the matcher's path mode is enough for Go to File ranking, decided in
  D082.

## Checkpoints

### 1. ✅ Application state database

Deliverables:

- `StateDatabase` owns `state.sqlite3`, one connection, and a serialized
  write queue on the background executor. `RecordStore` becomes a table handle
  over it, keeping its cache, version, and debounce.
- Tool layout and window session move into it. Their per-file `user_path()`
  functions are replaced by one path.
- The quit path flushes every record through the database.

Validation: the existing `tool_layout` and `window_sessions` tests, plus the
`product/tests/window_restore.rs` tests, pass against the shared file. New tests
cover two records writing concurrently without busy errors, a damaged table not
affecting the other, and a flush draining both.

Implementation context: `src/app/record_store.rs` is the type to generalize.
Its users are `src/app/tool_layout.rs` (`ToolLayoutStore::open`, `user_path`)
and `src/app/window_sessions.rs` (`install`, `user_path`, the flushes at lines
96, 114, and 180). Startup wiring is where those `user_path()` results are
passed in `src/app/mod.rs` or the startup entry. Quit flushing runs from
`extensions::host::quit_after_storage` (`src/app/extensions/host.rs`).
`src/app/recovery.rs` stays untouched. Test helpers use
`record_store::test_schema`.

Review gate: one database file in the data directory, with restore and tool
layout behaving as before.

### 2. ✅ Shared picker with fuzzy matching

Deliverables:

- Add the `nucleo-matcher` dependency.
- A new `src/app/picker.rs` holds `Picker`: items with label, description,
  detail, hint, and hidden match text. It scores, sorts, highlights matched
  characters in the label and description, and handles keyboard selection with
  scroll-into-view.
- `PickerView` (`src/app/transient.rs`) and `CommandPalette`
  (`src/app/command_palette.rs`) become thin wrappers over it, or are removed in
  its favor. Palette rows keep their current content for now.
- A `list.match.foreground` color key in the native theme table colors
  highlighted characters, and every bundled theme sets it.

Validation: a table of ranking tests (query → expected first items) over
command-like titles and paths. Further tests cover stable order for an empty
query and for ties, highlight ranges for multi-byte text, and the existing
palette and picker tests.

Implementation context: the overlay code to merge is duplicated in
`command_palette.rs` and `transient.rs`, and both use
`text_input::TextInput`. Native picker users are `src/app/product/language_mode.rs`,
`extension_restart.rs`, and `recovery_listing.rs`, which build
`host::protocol::PickerItem`. Add new protocol fields as optional so these
stay unchanged until checkpoint 5. Colors live in `src/app/theme.rs` (the
`color::` table) with defaults there. Product overlay tests are in
`src/app/product/tests/transient.rs`.

Review gate: try the palette and Set Language Mode by hand for ranking feel
before command metadata builds on it.

### 3. ✅ Command metadata and palette rows

Deliverables:

- `#[command]` accepts `description = "…"` and `palette = false`.
  `CommandDefinition` gains `description` and `palette`.
- Input commands opt out of the palette, and the main workbench and file
  commands gain descriptions.
- `commands.register(name, handler, options?)` carries title, description, and
  palette through `registerCommand`.
- The palette shows the title, description, and binding hint, matches the
  command name as hidden text, and omits `palette: false` commands.
- A binding lookup by command for a view kind, plus platform chord formatting.

Validation: unit tests for keystroke and chord formatting on both platform
styles, and for binding lookup (view-specific beats global, shortest wins,
argument-carrying bindings ignored). Palette tests cover hidden commands and
name matching. A host or product test covers an extension title and
description reaching the palette. A `contract.test.ts` update covers the new
option shape.

Implementation context: the macro is `crates/knot-handler-macros/src/lib.rs`
(the attribute parser around line 59 and its expansion around line 175, plus
its tests at line 386). The registry is `src/app/commands/declarations.rs`
(`definitions()` returns name and title). The catalog is
`src/app/commands/catalog.rs` (`CommandDefinition`, `register_native`,
`register_extension`). The extension registration path is
`registerCommand` in `src/host/js/bootstrap.js` (which already sends a title
equal to the name), `register_command` in `src/app/extensions/host/handlers.rs`,
`commands.register` in `src/host/js/knot.d.ts`, and the fake host in
`scripts/extensions/testing/host.ts`. Binding resolution is
`BindingRegistry` in `src/app/commands/keymaps.rs`, which needs a reverse
lookup over `active`. The palette opens at
`ProductShell::open_command_palette_from` (`src/app/product.rs`), where the
captured `ProductCommandTarget` supplies the view kind. Native command
declarations live in `src/app/product/commands.rs`, `src/app/editor/`, and
`src/app/terminal/view.rs`.

Review gate: palette content and descriptions reviewed in the running app.

### 4. Recently used commands

Deliverables:

- A `command_history` record in the state database (version 1, a list of
  command names). It is updated when a palette selection is confirmed, and the
  ranking rules from the design apply.
- Unregistered names are skipped and pruned.

Validation: unit tests for ordering with and without a query and for pruning
and the 50-entry bound. A store test covers persistence across reopen. A
product test covers confirming a command moving it to the top.

Implementation context: the record follows the pattern of `tool_layout.rs`
over checkpoint 1's `StateDatabase`, exposed as an application `Global`
installed at startup alongside the others in `src/app/mod.rs`, with its
flush joined to the quit path. Recording happens in the palette's `Confirmed`
subscription in `open_command_palette_from` (`src/app/product.rs`).

Review gate: confirm the ranking feels right after a few days of use, or
adjust the empty-query rule.

### 5. Picker items and highlight preview for extensions

Deliverables:

- `PickerItem` gains `detail` and `hint` in the protocol, decoder,
  `knot.d.ts`, `bootstrap.js` validation, and the fake host.
- `PickerOptions.onHighlight` uses the pull loop. A new host operation (for
  example `nextPickerEvent`) settles with the latest unread highlight or the
  final result. Cancellation and teardown settle it like the picker itself,
  and a picker without `onHighlight` makes no extra requests.
- `@knot/themes` Select Theme previews the highlighted theme by setting the
  theme settings, and restores the stored selection on dismissal.
- The extension guide documents the new item fields and preview.

Validation: product tests cover the highlight sequence (initial item, movement,
query changes, and coalescing while the extension has not asked again),
dismissal and cancellation during an outstanding highlight request, and window
closure. A themes package test against the fake host covers preview then
dismiss restoring the selection, and preview then select persisting it.
`contract.test.ts` is updated.

Implementation context: the host operation is `show_picker` in
`src/app/extensions/host/handlers.rs` (around line 1030), which currently
awaits a single `TransientOutcome`. The shell side is
`ProductShell::begin_picker` and the transient slot in `src/app/product.rs`,
with `PickerEvent` in the picker. Protocol types live in
`src/host/protocol.rs` (`PickerItem`, `PickerResult`). The JS entry is
`showPicker` in `src/host/js/bootstrap.js` (line 379). Fake host prompts are
in `scripts/extensions/testing/host.ts` (line 862). The consumer is
`extensions/@knot/themes/src/selection.ts` with
`test/selection.test.ts`. The guide is `docs/extension-api.md`.

Review gate: Select Theme previews smoothly when arrowing through themes.

### 6. Documentation and completion

- Update [semantic UI](../architecture/semantic-ui.md) (the shared picker,
  matching, and highlight pull),
  [commands](../architecture/commands.md) (metadata, palette visibility,
  history), and
  [workbench and lifecycle](../architecture/workbench-and-lifecycle.md) (the
  state database).
- Record rationale in [decisions](../decisions.md): `nucleo-matcher` over a
  hand-written scorer, one state database with recovery kept separate, and
  pull-based highlight because roots are serial.
- Review gate: remove this plan and mark D071 ✅ in the daily-driver plan.
