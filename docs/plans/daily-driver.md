# Daily-driver editor

Status: 🚧 approved, not started. Promotes D007, D008, D010, D031, D034, D053,
D060, D064, D068, and D089 from the [roadmap](../roadmap.md) and adds
D069–D088, D120–D123, and D127.

## Goal

Make Knot an editor its author can use every day: no demo or test surfaces,
complete workbench and editing commands, a discoverable palette, in-file and
cross-file navigation, themes, and a workspace explorer and project selector
built as ordinary bundled extensions.

## How this plan is executed

This plan is an index. Each item below is a brief, not a design, and this
file stays high level. Before implementing an item, analyze it in its own
session and write its design in a separate plan, `docs/plans/d0xx-<slug>.md`,
however small the item is. Link that plan from the item here, and never
add design detail to this file. Mark progress here with ✅ and 🚧. Remove
each item's plan once it is complete, and remove this plan once every item's
durable behavior lives in the architecture references and decisions.

Each item lists what it depends on. Items in the same phase are independent
unless their dependencies say otherwise.

Nothing that exists today is a constraint. Knot has no users, so every
current command, binding, setting, surface, API, and stored format may be
redesigned or removed, with no migration or compatibility path. These briefs
name current behavior only as a starting point for analysis.

## Ordering rationale

The order puts shared foundations ahead of their consumers, so no surface has
to be rebuilt later:

- **Settings before themes, zoom, soft wrap, and save hygiene.** Each of these
  needs somewhere to keep a value and a way for personal config to set it.
- **Themes before any new surface.** The palette, status bar, find bar, gutter,
  and occurrence highlights all take their colors from theme keys, not from
  constants that would need migrating later.
- **The palette and shared picker before every picker consumer.** Go to File,
  Go to Line, recent files, and the project selector all rely on one fuzzy
  matcher and one picker presentation.
- **Language editing configuration before editing commands and brackets.**
  Toggle comment, auto-indent, and auto-closing all read per-language data
  from the same registration.
- **The keymap preset mechanism before new commands; extra presets last.**
  New commands bind into a single Knot preset as they land. The VS Code, Zed,
  IntelliJ, and Sublime presets are filled in once the command set is stable.
- **Entitlements before anything that spawns processes or uses the network.**
  Cross-file search wraps ripgrep and is the first consumer. Git, language
  servers, formatters, and AI integrations follow later.
- **Ignore rules, file watching, filesystem mutation, and file enumeration
  before the explorer, Go to File, and search.** The extensions consume these
  public primitives and need no private bypass.

## Phase 1 — Clean slate

- ✅ **D069 — Remove demo and test-only commands**
  - Move Terminal to New Window and the explorer placement commands are gone;
    tab drag moves terminals, and `ProductShell::place_tool` is the seam for
    D072's Move Tool to Dock…. Developer tooling carries a `Developer:` title
    prefix. The window toolbar and status string are gone: failures use a
    warning prompt, and a failed package startup opens the extension report
    ([workbench and lifecycle](../architecture/workbench-and-lifecycle.md#product-application-launch)).

## Phase 2 — Foundations

- ✅ **D070 — Application settings**
  - One in-memory `settings` store shared by personal config and packages:
    the latest write wins, scoped values (`language`, `view`) beat unscoped
    ones, and definitions give keys a type, default, and description. Editors
    resolve `editor.tabSize` per language and view and repaint on change.
    `languages.associate` claims file patterns for a language, so detection
    overrides compose instead of living in one map setting
    ([extension host](../architecture/extension-host.md#settings),
    [decisions](../decisions.md#application-settings)).

- ✅ **D121 — Terminals start in the workspace root**
  - A new terminal starts in the root of the window's most recently focused
    document, else the first root, else home. Restored terminals fall back to
    the same rule (D127), and `terminals.open` takes an explicit directory
    ([workbench and lifecycle](../architecture/workbench-and-lifecycle.md#terminal-sessions-and-views),
    [decisions](../decisions.md#terminal)).

- ✅ **D127 — Restored terminals keep their working directory**
  - A terminal restored in place starts in the directory its shell was last
    in, read from the shell process and recorded in the window layout; a
    missing directory falls back to D121's rule. New terminals opened from a
    focused terminal start in its directory. D128 moves the query to
    libghostty
    ([workbench and lifecycle](../architecture/workbench-and-lifecycle.md#terminal-sessions-and-views),
    [decisions](../decisions.md#terminal)).

- **D089 — Project-local settings** (depends on D070, D122)
  - Settings read from a file in the repository, attached to folders rather
    than to projects, because a folder may belong to several projects. Add
    D070's `path` scope fact with its glob syntax and path normalization.
  - Decide the file's location and format (declarative data or code), and
    whether settings for one particular root set live in the
    `@knot/projects` record. Workspace trust (D124) stays deferred, so
    project settings must not run code until it exists.

- ✅ **D064 — Theming system**
  - Themes are extension contributions registered like languages, and every
    native surface paints from color keys whose native defaults are also the
    fallback. Extensions define their own keys and name them in decorations,
    gutter markers, and badges. `theme.mode`, `theme.dark`, and `theme.light`
    select the theme, and the bundled `@knot/themes` package ships One Dark,
    One Light, Catppuccin, Tokyo Night, and Solarized with a remembered Select
    Theme command
    ([semantic UI](../architecture/semantic-ui.md#themes-and-color-keys),
    [decisions](../decisions.md#themes)).

- ✅ **D071 — Command palette and shared picker**
  - One native fuzzy-matching picker (`nucleo-matcher`) serves the palette,
    native pickers, and `workbench.showPicker`, with highlighted matches,
    item detail and hints, and pulled highlight previews that Select Theme
    uses. Palette rows show titles, descriptions, and bindings, hide input
    commands, and rank recently used commands first across restarts, kept in
    the application state database
    ([semantic UI](../architecture/semantic-ui.md#extension-transient-interactions),
    [commands](../architecture/commands.md#dispatch),
    [decisions](../decisions.md#extension-transient-interactions)).

- ✅ **D086 — Keymap presets** (promotes D060)
  - Default bindings are a preset in one slot beneath every extension and
    personal binding. The native Knot preset is correct on macOS, Windows, and
    Linux; packages register further presets, and `keymap.preset` selects
    one. Extensions read `knot.platform`. Show Keybindings lists every binding
    with its owner and overrides, and menus show the effective keys
    ([commands](../architecture/commands.md#keymaps),
    [decisions](../decisions.md#commands-and-keymaps),
    [extension guide](../extension-api.md#register-a-keymap-preset)).

- **D072 — Workbench navigation and layout commands** (depends on D086)
  - Default bindings:
    - next and previous tab in order (⌘⇧] / ⌘⇧[)
    - most recently used tab switching on ctrl-tab, as in VS Code and Zed
    - go to tab 1–9
    - focus pane left/right/up/down
    - move tab to the adjacent pane, creating a split when there is none
    - split left and up, beside the existing split right and down
    - maximize the focused pane or dock, and toggle back
    - grow, shrink, and equalize panes from the keyboard
    - pin and unpin tab
    - close other tabs
    - close pane
    - reopen closed tab
  - Bindings go into the Knot preset. Reopen closed tab takes ⌘⇧T, so New
    Terminal needs another key.
  - Generic Move Tool to Dock… replaces the explorer-specific placement
    commands and works for any tool.
  - Reopen closed tab keeps a bounded per-window stack of what each closed tab
    needs to be recreated, reusing the window layout's tab records.
  - Pinned tabs sit first in their pane, survive close other tabs, and are
    recorded in the window layout. Decide whether a pinned tab refuses plain
    Close Tab and how it renders.
  - Preview tabs, as in VS Code and Zed: a document opened as a preview
    replaces the pane's previous preview until it is edited, pinned, or
    double-clicked. Opening as a preview is an option of the public open
    API, so the explorer and search extensions use it. Decide whether the
    window layout records preview state.
  - An open-documents picker lists the documents open in the window, for
    switching by name. Decide whether Go to File (D082) covers it instead.
  - Maximize is window-local presentation state; decide whether the window
    layout records it.

## Phase 3 — Editing

- **D073 — Language editing configuration**
  - Language registration gains line and block comment tokens, bracket pairs,
    auto-closing pairs, and indentation rules (increase and decrease
    patterns). `@knot/languages` supplies them for every bundled language.
  - Data only. D074 and D075 consume it.

- **D074 — Editing commands** (depends on D073)
  - Auto-indent on Enter: keep the current indentation, and adjust it with
    the indentation rules.
  - Tab and Shift-Tab indent and outdent a multi-line selection.
  - Toggle line comment (⌘/) and block comment.
  - Duplicate, delete, and move lines up/down.
  - Join lines.
  - Smart Home.
  - Double-click selects a word, triple-click selects a line, and dragging
    after either extends by that unit.
  - Each command is one undo step.

- **D075 — Bracket matching and auto-closing** (depends on D073, D064)
  - Highlight the bracket matching the one at the cursor, and add a jump to
    matching bracket command.
  - Typing an opening pair inserts its closer. Typing the closer steps over
    it. Backspace inside an empty pair deletes both characters. Wrapping a
    selection surrounds it.
  - Decide whether matching uses the syntax tree or a text scan. The tree
    avoids false matches inside strings and comments.

- **D068 — Editor gutter and line numbers** (depends on D064)
  - The gutter becomes ordered native lanes: line numbers, extension markers,
    and folds later. Lane widths come from content, such as the number of
    digits in the line count.
  - Extensions contribute markers through `EditorContribution.gutter`.
    Enablement is per surface, so read-only text frames can omit the gutter.
  - Each lane defines its own click and hover behavior. Highlight the current
    line's number.

- **D076 — Go to Line** (depends on D071)
  - Bound to ctrl-G. Accepts `line` and `line:column`, and previews the target
    while typing.

- **D077 — Status bar** (depends on D064)
  - A native per-window bar showing line:column, the selection size, the
    language (click to open Set Language Mode), line ending, indentation, and
    the dirty state.
  - Extensions contribute items: short text with an optional command, ordered
    within a left or right side. D058's mode indicator and D117's progress
    need them, and Git will later.

- **D078 — Zoom, font size, and soft wrap** (depends on D070)
  - ⌘+ / ⌘- / ⌘0 zoom the current view or every view (decide which). The base
    font size comes from settings.
  - Toggle Soft Wrap, plus a setting for its default. Wrapping changes vertical
    movement, scroll geometry, IME positioning, and gutter numbering, so define
    visual lines in the view's line model.

- **D079 — Save hygiene** (depends on D070)
  - Trim trailing whitespace and ensure a final newline on save, both
    configurable. The cleanup is one undoable edit applied before the content
    snapshot is captured.
  - Whether a formatter hook belongs here is decided in its session; language
    servers (D067) remain out of scope.

- **D031 — Current-file Find** (depends on D064)
  - Bound to ⌘F. The query field takes focus. Matching is live and highlights
    every match. Enter and ⇧Enter (and ⌘G / ⌘⇧G) move between matches, and the
    current match follows edits. Esc closes the bar.
  - Use selection as the find string (⌘E).
  - Match highlighting uses a layer that D080 can reuse.
  - Replace and the advanced modes (D001) stay deferred. Revisit D001 in this
    item's session, because case and whole-word options are cheap once
    matching exists.

- **D080 — Word occurrence highlighting extension** (depends on D064, D031)
  - When the cursor rests on a word, a bundled JavaScript extension highlights
    every other occurrence in the buffer. It uses the syntax tree or word
    boundaries, debounced after cursor movement.
  - Requires a new public observation API for cursor and selection changes.
    Today only the debounced `viewStateChanged` window event exists. Size
    the API for other cursor-driven extensions and keep it off the painting
    path.
  - Highlights are anchored contributions naming a theme color key, so
    they follow edits without help from the extension.

## Phase 4 — Files and workspace

- **D008 — File watching and automatic reload**
  - Native watching per workspace root and per open document, in the resource
    provider layer.
  - A clean document reloads in place and keeps its view state. A dirty
    document is marked as changed on disk and offers Reload or Keep. Save
    already reaches the conflict prompt.
  - A public event lets extensions refresh trees. Events are coalesced, and
    Knot's own saves are ignored.

- **D010 — Filesystem mutation API**
  - Create file, create directory, rename or move, and delete (to the trash
    where the platform supports it), through providers and a public API.
  - Open documents follow renames, and deletion of their file is reported to
    them.

- **D087 — Extension entitlements**
  - Packages declare entitlements in `knot.jsonc`. Start with process
    spawning and outbound HTTP.
  - Who grants them:
    - An installed package prompts once, on its first load, and again when an
      update adds an entitlement. The grant is stored by canonical package
      identity.
    - Bundled packages declare entitlements through the same path but are
      granted by shipping with the binary.
    - Personal config is implicitly granted.
  - A missing or denied entitlement makes the API reject with a typed error
    instead of failing the package. Provide a way to review and revoke grants.
  - The APIs:
    - spawning a process with arguments, working directory, and environment,
      streaming stdout and stderr, writing stdin, observing exit, and killing
      the process (processes are cleaned up when the lifecycle tears down)
    - a `fetch`-shaped HTTP client
  - This provides consent and visibility, not a sandbox. Runtime isolation is
    not a security boundary, and the decision record must say so.
  - Decide how bundled consumers locate tools: bundle `rg` with Knot, or
    require it on PATH.

- **D122 — Extension file read, list, and stat API**
  - A public API to read a file's bytes or text, list a directory, and stat
    a URI through the registered resource providers, as VS Code's
    `workspace.fs` does. The explorer, project-local settings (D089),
    EditorConfig (D090), and project inference (D123) need it, and D010 and
    D081 cover only mutation and enumeration.

- **D081 — Workspace ignore rules and file enumeration**
  - Ignore policy is contributed and evaluated natively. Rules come from
    `.gitignore` (and possibly `.ignore`) plus configured patterns, at two
    levels:
    - *hidden*: never shown (`.git`, `.DS_Store`)
    - *ignored*: dimmed in the explorer, and skipped by Go to File, search,
      and watching
  - Native code evaluates the rules with the `ignore` crate, which gives the
    same semantics as ripgrep. Hot native paths (watching, enumeration,
    listings) therefore never call into JavaScript.
  - Extensions contribute extra rules through a lifecycle-owned
    registration, like languages. They can query whether a path is ignored,
    and read the effective rules to pass to external tools (`rg --glob`).
    Decide whether the defaults live in a tiny bundled package or in D070
    settings.
  - Enumeration is an ignore-aware walk of every root, exposed as a public
    API that streams or pages results. Decide whether a native walk is
    still needed once `rg --files` is available through D087, or whether Go
    to File uses ripgrep too.

- **D034 / D053 — Explorer as a bundled extension** (depends on D008, D010, D122,
  D081, D072)
  - A bundled `@knot/explorer` package replaces the native explorer tool and
    its commands, using the contributed tree API.
  - It provides:
    - multi-root browsing
    - single click opens a preview tab, double click a regular one (D072)
    - new, rename, and delete actions
    - reveal the active file
    - refresh on watch events
    - filtering
  - Removing the native explorer also removes its private workspace-tree path.
    Any gap this exposes in the public tree API (such as context menus,
    keyboard actions on rows, or inline rename) is closed here.
  - It is an ordinary tool for tool-layout memory: it has a stable tool ID
    and a default placement.

- **D082 — Go to File** (depends on D071, D081)
  - A bundled extension bound to ⌘P that fuzzy-matches workspace paths.
  - Multi-root paths are labeled by root, and open and recent files rank
    first.
  - Decide whether the picker's complete-item-list contract holds up on large
    repositories, or whether the picker needs incremental items.

- **D083 — Recent files** (depends on D071)
  - A bundled extension that records opened documents in extension storage
    and offers Open Recent….
  - Reopen closed tab (D072) is unrelated: it is native per-window state.

- **D084 — Project selector modal** (depends on D071)
  - `@knot/projects` replaces its Projects tool with a picker modal: recent
    projects first, then switch, new, rename, and delete as picker actions or
    companion commands.
  - Remove the Projects tool registration and its tree code.

- **D120 — Remembered projects** (depends on D070, D084)
  - Every opened root set is remembered as a project with an inferred name,
    opening a suspended project's folder restores it, and Open Folder opens
    a new window unless the current one has no roots.
  - 🚧 [Plan](d120-remembered-projects.md)

- **D123 — Project inference extension** (depends on D122)
  - A bundled extension that finds the version-control root above an opened
    document outside the window's roots and offers to open it as a project,
    as Emacs's project.el infers projects. Decide which markers count
    (`.git` first), whether a window without roots switches without asking,
    and how often the offer may repeat for the same folder.

- **D085 — Cross-file search** (depends on D087, D081, D071, D072; promotes
  D007)
  - A bundled extension that wraps ripgrep through process spawning (D087)
    and parses its `--json` output. It passes the effective ignore rules
    (D081).
  - It provides:
    - a query input with case, regex, whole-word, and glob options
    - streamed results in a tree grouped by file (contributed tree groups
      and row actions already exist)
    - opening a result at its range, as a preview tab
    - cancelling a superseded search
    - refresh
  - Stale-target recovery after edits (D007) is decided here.

## Phase 5 — Keymap presets

- **D088 — VS Code, Zed, IntelliJ, and Sublime presets** (depends on D086 and
  the command set from earlier phases)
  - Map each editor's default bindings onto Knot's commands, per platform.
    Commands with no equivalent stay unbound. Add a test that every preset
    binding names a registered command.
  - Add a Select Keymap Preset picker. Decide whether first launch asks for a
    preset.
