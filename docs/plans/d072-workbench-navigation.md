# D072 — Workbench navigation and layout commands

Status: 🚧 in progress. Item of the
[daily-driver plan](daily-driver.md); depends on D086 (keymap presets).

## Goal

Every common pane and tab operation works from the keyboard: moving between
tabs in order, by number, and by recent use; moving focus and tabs between
panes by direction; splitting in any direction; resizing, equalizing, and
maximizing panes; pinning tabs; closing other tabs or a whole pane; and
reopening closed tabs. Documents opened from the explorer and search can open
as preview tabs. Move Tool to Dock… places any tool.

Current behavior lives in
[workbench and lifecycle](../architecture/workbench-and-lifecycle.md) and
[commands](../architecture/commands.md#keymaps).

## Design

- **Split ratios.** Each `WorkbenchLayout::Split` carries the fraction of
  its extent given to `first`, and `LayoutNode::Split` records it, so sizes
  survive restarts and project switches. Every divider drags, not only those
  beside docks. Grow and shrink change the ratio of the focused pane's nearest
  enclosing split by a fixed step, clamped so neither side drops below a
  minimum size; equalize resets every ordinary split to an even share. Dock
  sizes stay absolute as today.
- **Ordered tab navigation.** Next and previous tab wrap within the focused
  pane, which may be a dock. Go to tab 1–8 selects by position and 9 selects
  the last tab, as in browsers, VS Code, and Zed.
- **Recently used switching without an overlay.** Each pane keeps its tabs in
  recently activated order. ctrl-tab activates the next tab in that order
  immediately; repeated presses while ctrl is held walk further, and
  ctrl-shift-tab walks back. The order is committed when ctrl is released,
  so a walk counts as one activation of the tab it ends on. The order is per
  pane, as both VS Code and Zed default to, and is not recorded.
- **Directional panes.** Focus pane and move tab find the neighbor in a
  direction from painted pane bounds, including docks, choosing the pane that
  overlaps the focused pane most along the other axis. Moving a tab when no
  pane lies in that direction splits the focused pane on that side; moving
  the last tab out of a pane collapses it as tab drag does. Split left and up
  join the existing split right and down.
- **Maximize.** Toggle Maximize Pane renders only the focused pane or dock
  over the whole workbench. It is window-local and not recorded. Toggling
  again, splitting, closing the pane, or focusing or moving a tab by direction
  restores the layout.
- **Pinned tabs.** Toggle Pin Tab moves a tab to the end of its pane's pinned
  prefix, or back to the first unpinned position. Pinned tabs render a pin
  marker in place of the close button. Plain Close Tab still closes a pinned
  tab because closing one tab is explicit; Close Other Tabs skips pinned tabs;
  Close Pane closes everything. Dragging a tab keeps it within its pinned or
  unpinned section. `LayoutTab` records `pinned`.
- **Bulk close.** Close Other Tabs and Close Pane run one protected closure
  over a set of tabs, so the Save / Don't Save / Cancel prompts and the
  cancellation guarantee match closing a window: a cancel leaves every tab in
  place.
- **Reopen Closed Tab.** Each window keeps a stack of its last 20 closed tabs,
  each its `LayoutTab` record plus its pane and index. Reopening recreates it
  through the layout restore step: into its pane at its index when that pane
  still exists, otherwise into the focused pane. A document reopens its file;
  a terminal starts a fresh shell in its last directory; a tool shows again.
  Untitled documents are not pushed because their text is gone, and tools are
  not pushed because hiding already remembers them. Bulk closes push every
  closed tab. The stack is not recorded.
- **Preview tabs.** A document opened as a preview replaces the pane's
  current preview tab, if any, in place. A preview becomes regular when its
  document is edited, its tab is pinned, double-clicked, or dragged, or the
  same document is opened again without `preview`. Its title renders in
  italics. Previews are not recorded: restored tabs are regular. The public
  `documents.open` gains `preview?: boolean`; the native explorer's single
  click opens a preview and its double click a regular tab. A native
  `workbench.previewTabs` boolean setting, default true, turns previews off:
  then a preview request opens a regular tab.
- **Move Tool to Dock….** A native picker in the window's transient slot
  lists Left, Right, Bottom, and Document Area for the focused tool and calls
  `ProductShell::place_tool`. It publishes the tool-only default like any
  explicit move.
- **New Terminal moves to ctrl-`** because Reopen Closed Tab takes ⌘⇧T, as in
  browsers and VS Code. Knot has no terminal panel, so the key VS Code and Zed
  use for showing it opens a terminal tab.
- **Terminal pass-through.** On Windows and Linux, a global binding whose
  first stroke is ctrl with a printable character other than backtick stays
  unbound in terminals, as today. Named keys (tab, pageup, pagedown, arrows)
  and ctrl-` stay global, because shells do not use them, so tab navigation
  and New Terminal work from a terminal, as VS Code's terminal allows.

### Bindings

Added to the Knot preset. `secondary-` is cmd on macOS and ctrl elsewhere.

| Command | macOS | Windows / Linux |
| --- | --- | --- |
| Next / Previous Tab | cmd-shift-] / cmd-shift-[ | ctrl-pagedown / ctrl-pageup |
| Next / Previous Recent Tab | ctrl-tab / ctrl-shift-tab | same |
| Go to Tab 1–9 | cmd-1 … cmd-9 | alt-1 … alt-9 |
| Focus Pane Left/Right/Up/Down | secondary-k secondary-arrow | same |
| Move Tab Left/Right/Up/Down | secondary-k shift-arrow | same |
| Split Left / Up | secondary-k left / up | same |
| Toggle Maximize Pane | secondary-k secondary-m | same |
| Grow / Shrink / Equalize Panes | secondary-k = / - / 0 | same |
| Toggle Pin Tab | secondary-k shift-enter | same |
| Close Other Tabs / Close Pane | secondary-k o / secondary-k w | same |
| Reopen Closed Tab | secondary-shift-t | same |
| New Terminal | ctrl-` | same |
| Move Tool to Dock… | unbound | unbound |

## Exclusions

- An open-documents picker: Go to File (D082) ranks open files first, and
  recent-tab switching covers the rest.
- A recent-tab switcher overlay.
- Reopening closed windows, and recording the reopen stack, maximize state,
  or preview state.
- Whole-pane dragging and arbitrary split-tree reshaping (D037).
- Extension APIs for pinning, maximizing, or resizing; extensions reach these
  through the commands.

## Deferred decisions

- The resize step and minimum pane size; pick values that feel right in
  checkpoint 1.
- Whether Close Pane on a dock hides its tools or is unavailable there;
  decide in checkpoint 4 with the dock rules in front of you.

## Checkpoints

### 1. Split ratios and resizing ✅

- Resolved: a step moves 5% of the split's extent, ordinary splits keep at
  least 10% per side, and a pointer drag leaves at least 120px. Growing or
  shrinking a focused dock changes its absolute size by 40px instead. A split
  is addressed by its boundary pane, the first pane of its second child.

- Ratios in `WorkbenchLayout` and `LayoutNode::Split`; the record format
  simply changes, with no version bump.
- Every divider drags and settles through the existing debounce.
- Grow, Shrink, and Equalize Panes; Split Left and Split Up.
- **Validation:** workbench model tests for ratio changes, clamping, equalize,
  and collapse keeping sibling ratios; layout capture/plan round-trip tests
  with ratios; product test that a dragged ordinary divider changes the ratio;
  preset tests for the new bindings on all three platforms.
- **Review gate:** resizing feels right by keyboard and pointer; ratios
  restore after quit.
- **Implementation context:** `WorkbenchLayout` and `split_pane` in
  `src/app/workbench.rs`; render recursion and `render_divider` in
  `src/app/product/render.rs`; dock-divider drag in
  `src/app/product/tab_drag.rs` (mirror `set_dock_size` and its debounce for
  split ratios); `LayoutNode`, `capture`, and `plan` in
  `src/app/window_layout.rs`; commands beside `split_horizontal` in
  `src/app/product/commands.rs`; entries in `SHARED_BINDINGS` in
  `src/app/commands/keymaps/presets.rs`.

### 2. Tab navigation and terminal keys

- Next and Previous Tab, Go to Tab 1–9.
- Per-pane recently used order, ctrl-tab walks, and commit on ctrl release.
- Terminal pass-through limited to ctrl with a printable character other than
  backtick; New Terminal on ctrl-`.
- **Validation:** workbench tests for ordered wrap, go to N and last, and the
  recent order after activation, closure, and relocation; product test of a
  walk followed by modifier release committing one activation; preset tests
  that ctrl-tab, ctrl-pageup, and ctrl-` stay bound in terminals on Windows
  and Linux while ctrl-letter chords pass through.
- **Review gate:** switching tabs from the keyboard, including inside
  terminals.
- **Implementation context:** `Pane` and `activate_tab` in
  `src/app/workbench.rs` hold the recent order; modifier release needs a
  window-level `on_modifiers_changed` in the product shell (see its use in
  `src/app/terminal/view.rs`); `knot_preset` and `starts_with_plain_control`
  in `src/app/commands/keymaps/presets.rs`; `NEW_TERMINAL_COMMAND` binding.

### 3. Directional panes and maximize

- Focus Pane and Move Tab in four directions from painted bounds, splitting
  when no neighbor exists.
- Toggle Maximize Pane and the transitions that restore the layout.
- **Validation:** neighbor selection tests over pane rectangles (pure
  function); product tests for moving the last tab collapsing its pane,
  moving into a dock, splitting at an edge, and maximize ending on split,
  close, and directional focus.
- **Review gate:** navigating a three-pane layout with docks by keyboard.
- **Implementation context:** painted pane geometry collected for tab drag in
  `src/app/product/tab_drag.rs` and `tab_bar_drag_geometry` in
  `src/app/product/render.rs`; pane transfers through
  `src/app/workbench/relocate.rs` and `src/app/product/relocate.rs`;
  maximize as shell render state in `src/app/product/render.rs`.

### 4. Pinned tabs and bulk close

- Toggle Pin Tab, pinned prefix ordering, pin marker, drag constrained to the
  section, and `pinned` in `LayoutTab`.
- A protected-closure scope over a set of tabs; Close Other Tabs and Close
  Pane on it.
- **Validation:** workbench tests for pin ordering and drag constraints;
  layout round-trip with `pinned`; closure tests that a cancelled bulk close
  leaves every tab, that dirty documents losing their last view prompt once,
  and that Close Other Tabs keeps pinned tabs.
- **Review gate:** pin rendering and bulk-close prompts.
- **Implementation context:** `ProtectedCloseKind` in `src/app/product.rs`
  and `build_protected_close_plan` / `commit_protected_close` in
  `src/app/product/closure.rs` (add a tab-set kind beside `Tab` and
  `Window`); tab rendering in `src/app/product/render.rs`; insertion rules in
  `src/app/workbench/relocate.rs`; `LayoutTab` in `src/app/window_layout.rs`.

### 5. Reopen Closed Tab

- The bounded per-window stack, pushed from every protected and tool-free
  closure path, and reopening through the layout restore step.
- **Validation:** product tests that a closed document reopens at its pane and
  index, falls back to the focused pane when its pane is gone, a terminal
  reopens in its directory, untitled documents are skipped, and a bulk close
  reopens in reverse order.
- **Review gate:** reopening after ordinary and bulk closes.
- **Implementation context:** tab capture in `WindowLayout::capture` and
  per-kind recreation used by `plan` in `src/app/window_layout.rs` and the
  shell's layout installation in `src/app/product/view_state.rs` /
  `src/app/product/sessions.rs`; push in `commit_protected_close` and
  `close_tab_after_approval` in `src/app/product/closure.rs`.

### 6. Preview tabs

- Preview state on document tabs, replacement in place, the promotions, the
  italic title, and the `workbench.previewTabs` setting.
- `preview` on `documents.open`; the native explorer opens previews on single
  click.
- **Validation:** workbench tests for replacement and each promotion; settings
  test that a disabled setting opens regular tabs; host test for the
  `preview` option; extension type declarations and schema updated; layout
  capture records previews as regular tabs.
- **Review gate:** browsing files in the explorer with previews.
- **Implementation context:** `open_tab_for_document` in
  `src/app/workbench.rs`; native setting definitions beside `EDITOR_TAB_SIZE`
  in `src/app/settings.rs`; `open_document` in
  `src/app/extensions/host/handlers.rs`; `DocumentDestinationOptions` in
  `src/host/js/knot.d.ts` and `docs/schemas/knot.schema.json`; explorer row
  activation in `src/app/workspace_tree.rs` and `src/app/product/tools.rs`.

### 7. Move Tool to Dock…

- A native picker for the focused tool's destination that calls
  `ProductShell::place_tool` and publishes the tool-only default.
- **Validation:** product test that each choice places the tool and publishes
  the default, and that the command reports `NotApplicable` without a focused
  tool.
- **Review gate:** moving the explorer and a contributed tool.
- **Implementation context:** `place_tool` in `src/app/product/tools.rs`;
  mirror the native picker in `src/app/product/language_mode.rs`.

### 8. Documentation

- Update [workbench and lifecycle](../architecture/workbench-and-lifecycle.md)
  (ratios, recent order, pinned and preview tabs, reopen stack, maximize),
  [commands](../architecture/commands.md#keymaps) (terminal pass-through),
  the [extension guide](../extension-api.md) for `preview`, and
  [decisions](../decisions.md) for the overlay-free recent switching, preview
  semantics, and New Terminal key. Mark D072 ✅ in the index and remove this
  plan.
- **Review gate:** documentation review.
