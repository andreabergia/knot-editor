# D069 — Remove demo and test-only commands

Status: 🚧 approved, not started. Part of the
[daily-driver plan](daily-driver.md), phase 1.

## Outcome

A product window shows only product surfaces. The prototype toolbar is gone.
Commands that existed to demonstrate a mechanism are removed. Developer
tooling is grouped under one `Developer:` title prefix. Failures that were
only visible in the toolbar's status text now reach the user through a
prompt.

## Findings

- `knot.fixture.*` sources are `#[cfg(test)]` in `src/app/extensions/host.rs`,
  and `fixture.*` names appear only in test modules. `@example` packages
  compile to `OUT_DIR` for product tests and reach a product run only through
  a manual install (`just install-example-apps-mac`). The compiler already
  enforces this boundary, so no change or guard test is needed.
- Tab drag already moves a terminal to a new window through
  `relocate_tab_to_new_window_with`. Removing Move Terminal to New Window
  loses no capability. Its product tests are the only coverage of that
  relocation path, though.
- The window toolbar (`src/app/product/render.rs`) is a prototype surface.
  Every button duplicates a command, menu item, or key, except the
  extensions-status button, which is the only passive sign that a package
  failed. The free-form `ProductShell::status` string the toolbar renders is
  the only place some failures appear: save, open, recovery, discard, and add
  folder.

## Design

- Remove `terminal.move-to-new-window` and the four `workbench.explorer-*`
  placement commands. D072's generic Move Tool to Dock… replaces the
  explorer commands.
- Remove the toolbar and `ProductShell::status`.
  - Success notices ("saved …", "split horizontally", "new terminal", "tab
    moved here") and progress notices ("choosing what to open", "saving …")
    disappear, because their effect is already visible.
  - Failures that only reached the status string use the native message
    prompt at warning level, which extensions already use for messages.
- When startup completes with a failed package, each product window opens the
  extension startup report once. D077's status bar later replaces this with
  an indicator.
- Developer tooling takes a `Developer:` prefix:
  `Developer: Show Extension Startup Report` and
  `Developer: Restart Extension…`. Restart leaves the File menu.
  `Toggle Terminal Mouse Reporting` is a user-facing terminal feature
  ([decisions](../decisions.md#terminal)), so it keeps its title.

## Exclusions and deferred decisions

- `projects.list`, a registry dump to the clipboard that only tests use, stays
  until D084 rewrites the projects UI.
- Command title casing ("Move left" against "Move Folder Up") and palette
  categories belong to D071.
- A generic Move Tab to New Window command is not added. Tab drag covers
  the workflow, and D072 owns new workbench commands.

## Checkpoints

Commit each checkpoint separately. The validation for every checkpoint is
`cargo test` plus a palette, menu, and keymap check in a running product.

### 1. Remove Move Terminal to New Window

Deliverable: the command, its keymap entries on every platform, its File menu
item, and its toolbar button are removed. The terminal relocation tests drive
the drag path directly.

Implementation context:
- `src/app/product/commands.rs`: the `#[command("terminal.move-to-new-window")]`
  handler.
- `src/app/product.rs`: `move_terminal_to_new_window_with` and the import.
- `src/app/commands/keymaps.rs`: the macOS, Windows, and Linux binding tables.
- `src/app/product/launch.rs`: the File menu.
- `src/app/product/render.rs`: the `move terminal` toolbar button.
- `src/app/product/tests/terminal.rs` covers session survival, a failed or
  vanished destination, and a stale source. Rewrite these tests to call
  `capture_tab_relocation` (`src/app/product/relocate.rs`), then
  `relocate_tab_to_new_window_with`, which mirrors `tab_drag.rs`. Keep the
  stale case: close the tab after the capture and expect the relocation to
  be rejected. Drop the test that only exercises a command-dispatch
  `Unavailable` result for a non-terminal target.

Review gate: terminal relocation coverage is no weaker than before.

### 2. Remove the explorer placement commands

Deliverable: the four `workbench.explorer-*` commands are removed, and a
`ProductShell::place_tool(id, placement, window, cx)` method places a tool,
publishes the tool layout, and refocuses.

Implementation context:
- `src/app/product/commands.rs`: `explorer_left`/`right`/`bottom`/`in_editor`
  and `place_explorer`. The new method goes beside `explorer_frame` in
  `src/app/product/tools.rs` and wraps `Workbench::place_tool`
  (`src/app/workbench/tools.rs`).
- `src/app/product/tests/tool_layout.rs` and `tools.rs` dispatch these
  commands by name. They switch to `place_tool` on the explorer frame, which
  still exercises the user-arrangement publish path.

Review gate: `place_tool` is the seam D072 will wrap. Confirm that it fits
before building on it.

### 3. Group developer commands

Deliverable: `Developer: Show Extension Startup Report` and
`Developer: Restart Extension…` are renamed, and Restart is no longer in the
File menu.

Implementation context: the `title =` attributes in
`src/app/product/commands.rs`, the File menu in `src/app/product/launch.rs`,
and `examples/extensions/README.md`, which names the command and the toolbar
button.

Review gate: the palette shows the prefix consistently.

### 4. Remove the toolbar and status string

Deliverable: the toolbar row and `ProductShell::status` are removed.
Status-only failures show a warning prompt, and a failed startup opens the
extension report in each product window once.

Implementation context:
- `src/app/product/render.rs`: the toolbar row, including the extension
  button label logic, which moves to the auto-open condition.
- About 57 `status =` writes across `src/app/product.rs` and
  `src/app/product/*.rs`. Keep and convert the failures: `save failed`,
  `open failed`, `recovery failed`, `discard failed`, `cannot add folder`,
  `could not create destination window`, and `extensions are unavailable`.
  Drop the rest.
- Use the prompt path already used for extension messages. Find it beside
  `workbench` message handling in `src/app/extensions/host/handlers.rs`.
- Startup completion is in `src/app/extensions/host.rs`, where the state is
  set to `StartupState::Complete`. Each shell observes the host and calls
  `toggle_extension_report` once when the report has `LoadResult::Failed`
  entries.
- Tests: `src/app/product/tests/recovery.rs` asserts status text. Change it
  to assert restored documents, or the prompt on failure paths. Add a test in
  `src/app/product/tests/startup.rs` that a failing installed package opens
  the report.

Review gate: every removed failure message has a visible replacement.

### 5. Documentation

Deliverable: update the architecture references that describe removed
surfaces, mark D069 ✅ in the daily-driver plan, and delete this file.

Review gate: completion.
