# Product Roadmap and Deferred Work

The selected work below has an explicit implementation order. The deferred
register that follows is grouped by area, not by priority, but its items are
not independent: [dependency order](#dependency-order) lists the chains, and
each item's **Order** line names what must come before and after it.
Completed decisions remain in `decisions.md`; proposed architecture and
checkpoint details live in active plans.

Each item has a stable `D###` identifier, a reason for deferral, a concrete
trigger for reconsideration, and, when it has one, its order relative to other
items. "After" means the earlier item must be complete or designed together
with this one; "before" is the same constraint seen from the other side. IDs
are never renumbered or reused. New items take the next unused ID. When an
item is promoted, its slice plan retains the ID and the item is removed from
this register. Items of the active [daily-driver](plans/daily-driver.md) and
[package manager](plans/package-manager.md) plans are referenced here by ID
and ordered in their plans.

## Selected work

The production extension APIs and JavaScript projects initiative (D046–D053,
D005) is complete: an ordinary bundled JavaScript extension manages named
multi-root projects, switches them with unsaved text and layouts, and rebinds
to the windows Knot restores at startup. Current behavior lives in the
[architecture](architecture.md) references and rationale in
[decisions](decisions.md).

Extension authoring (D056) is complete: bundled packages and examples are
TypeScript compiled at build time, split into modules, and tested under
`node:test` against an in-memory host.

The editor view data model is complete: views read lines from the model
instead of a private text copy, typing coalesces in the piece table, the
consumed edit log is discarded, and inserted newlines follow the buffer's line
ending. D002 and D031 can build on the view's line model.

The codebase health review is complete. The V8 engine and the editor view are
split by concern, terminal, extension, and command modules live in their own
directories, large test modules sit beside their code, and local filesystem I/O
no longer needs a tokio runtime.

Syntax highlighting (D002) is complete: native incremental tree-sitter
parsing with injections, languages registered through the bundled
`@knot/languages` package, explicit languages that survive restarts and project
sessions, and a layered paint pipeline shaped for later language servers. Its
follow-ups are D064–D067; D066 precedes D058.

The [daily-driver editor](plans/daily-driver.md) initiative is selected: it
removes demo surfaces and adds workbench and editing commands, a richer
palette, themes, Find, navigation, file watching, and bundled explorer,
project-selector, and search extensions, keymap presets, and extension
entitlements. It promotes D007, D008, D010, D031, D034, D053, D060, D064, and
D068 and adds D069–D088.

The [package manager](plans/package-manager.md) initiative is selected: extension
installation becomes an inspectable application-owned lifecycle with a lockfile,
a local archive format, installs from git repository links, version-constrained
dependency resolution, a manager surface, and updates with pinning and rollback.
Git links replace a package registry, so publishing is a push and discoverability
is a website; consent stays with entitlement grants. It promotes D033 and adds
D101–D106.

## Dependency order

Each line is a chain: an item to the left of an arrow comes first. Items not
listed have no ordering constraint; each item's **Order** line is the
complete list for that item.

- **Vim mode and editing extensibility** (see the
  [extensibility report](reports/extensibility.md)):
  - D086 → D110 → D113 → D058
  - D080 → D109 → D112, D113, D027
  - D078 → D112 → D058
  - D074 → D111 → D012, D058
  - D068, D073 → D066 → D058
  - D064, D068 → D114 → D058
  - D092 → multi-selection editing in D109, D001, D067
- **Extension presentation and language tooling:**
  - D064 → D114 → D067 → D022
  - D064 → D055
  - D087 → D067, D023
  - D079 → D115 → format on save with D067
  - D114 before D021, which is reconsidered only if D114 falls short.
- **Find:** D031 → D001, with or after D092.
- **Package manager:** D087 → D101 → D102 → D103 → D104 → D106;
  D064, D071, D103 → D105 → D106; D106 → D107, D108; D103 → D116.
- **Settings:** D070 → D089 → D090; D008 → D090; D070 → D091, D045, D059.
- **Start surface:** D083, D084 → D061.
- **Terminal:** D063 with or before D041.

## Deferred register

## Editing and workbench

- **D001 — Replace and advanced Find modes**
  - Deferred: These modes depend on current-file Find (D031, in the
    [daily-driver plan](plans/daily-driver.md)).
  - Reconsider: Daily use identifies required replacement, regular-expression,
    case, or whole-word behavior.
  - Order: after D031; replace-all is a bulk edit, so with or after D092.

- **D006 — File drag and drop**
  - Deferred: Open and Save As establish workspace-independent documents first.
  - Reconsider: Platform interaction becomes a selected product slice.

- **D058 — Complete Vim mode in JavaScript**
  - Deferred: No modal editing contract has been selected. The native core owns
    raw key input, IME, and text mechanics. Extensions reach commands,
    keybindings, and buffers, but not editor views, selections, key contexts,
    undo grouping, layout, recording, or the syntax tree, so a full emulation
    would need a private host bypass today.
  - Reconsider: A daily Vim workflow is selected, or Knot sets out to reach
    Neovim-level extensibility, which this item is the forcing function for.
    Then build it as an ordinary bundled or installed extension over the
    generic primitives listed in the
    [extensibility report](reports/extensibility.md), keeping registers,
    operators, and Vim's mode names inside the extension. The Ex command line
    adds completion to `showInput`, and the mode indicator and relative line
    numbers use D077 and D068.
  - Order: after D066, D109, D110, D111, D112, D113, and D114's decoration
    keys, and the daily-driver items D068, D072, and D077.

- **D066 — Syntax-tree editing features and public syntax-tree API**
  - Deferred: D002 uses tree-sitter trees only for highlighting. Folding,
    indentation, structural selection expansion, text objects, and
    tree-based motions are not built, and extensions cannot query the tree.
  - Reconsider: Daily editing needs folding or language-aware indentation,
    or D058 is selected. Then add per-language `folds`, `indents`, and
    `textobjects` queries to language registration and expose node queries
    through the extension API without exposing tree-sitter objects.
  - Order: after D073, which adds editing data to the same language
    registration, and D068, whose gutter lanes hold folds; before D058.

- **D059 — Professional typography: ligatures and configurable font features**
  - Deferred: Surfaces use shared fixed typography defaults. Ligatures and
    font features need shaping options per surface, and the terminal assumes
    fallback glyphs stay within emulator cell allocations, so enablement rules
    differ between the editor and the PTY grid.
  - Reconsider: Daily readability or a font choice requires ligatures,
    stylistic sets, variable-font settings, or configurable terminal
    typography. Then define font selection and fallback per surface, feature
    toggles and their persistence scope (with D045), and terminal cell metrics
    for fonts whose shaped advance differs from the default.
  - Order: after D070; with D045.

- **D061 — Empty normal-group start panel**
  - Deferred: A workbench whose normal document group would be empty receives
    an untitled document, so docks never fill the window, including after a
    project switch to a session with no documents. A dedicated empty panel needs
    its own surface, focus, and command-routing rules.
  - Reconsider: The placeholder untitled document gets in the way in daily use,
    or a start surface (recent projects and files, common commands) is
    selected. Then replace the placeholder with a native empty-group panel that
    keeps the normal group's position and accepts opened documents.
  - Order: after D083 and D084, whose recent files and projects a start
    surface lists.

- **D037 — Whole-pane dragging and arbitrary split-tree reshaping**
  - Deferred: D004 moves individual tabs into existing panes or new edge splits;
    moving groups of tabs and restructuring whole split branches is a separate
    interaction and layout problem.
  - Reconsider: Daily use shows that moving tabs one at a time or creating edge
    splits is too cumbersome for a common layout change.

## Documents, storage, and history

- **D009 — Binary buffers and encoding detection**
  - Deferred: The first slice explicitly targets UTF-8 text.
  - Reconsider: A concrete file workflow requires non-UTF-8 or binary
    inspection.

- **D011 — Remote filesystem providers**
  - Deferred: The URI boundary is validated with local and memory providers.
  - Reconsider: A specific remote workflow is selected.

- **D012 — Persistent undo and history trees**
  - Deferred: Session-local linear undo is sufficient for dogfooding.
  - Reconsider: Users need edit history across restarts or branching history.
    Byte-offset view state already survives restarts and project switches.
  - Order: after D111, which defines the undo steps a history records.

- **D092 — Piece tree text buffer**
  - Deferred: `TextBuffer` keeps its pieces in a flat `Vec` searched linearly,
    shifts a flat line-start index on every edit, and copies the piece list
    for each snapshot. Coalesced typing keeps the piece count low, so
    measured typing latency is acceptable. Absorbs the former piece-table
    compaction (D013) and line-start index (D014) items.
  - Reconsider: Before or together with the first bulk-edit feature, such as
    Replace (D001), multi-cursor editing, formatting, or language-server edit
    lists (D067), or when long sessions show measured latency, memory, or
    stabilization problems. Then follow the
    [text buffer analysis](reports/text-buffer-structure.md): benchmark the
    current buffer, store the pieces in a copy-on-write B-tree that summarizes
    bytes, line breaks, and UTF-16 length, resolve positions through ordered
    piece locators, and keep the piece semantics, edit log, and `TextBuffer`
    API unchanged.
  - Order: before or with D001, multi-selection editing in D109, and D067.

- **D015 — URI deduplication across processes**
  - Deferred: The first slice handles identity within one application process.
  - Reconsider: Multi-process editing or document handoff is introduced.

- **D089 — Project-local settings**
  - Deferred: Settings (D070) come only from personal config and packages.
    Emacs, Neovim, VS Code, and Zed all read per-project settings from the
    repository (`dir-locals.el`, `exrc`, `.vscode/settings.json`,
    `.zed/settings.json`).
  - Reconsider: Daily use needs settings that differ per repository. Then add
    the `path` scope fact with its glob syntax and path normalization, decide
    where project settings live (a file in the repository or a
    `@knot/projects` record) and whether they are declarative data or code,
    and decide what trust an untrusted repository's settings require.
  - Order: after D070; before D090 and per-project package sets in D116.

- **D090 — EditorConfig support**
  - Deferred: `.editorconfig` files need file watching (D008), reading
    arbitrary files through the public API, and the `path` scope fact (D089).
  - Reconsider: D008 and D089 are complete, or daily use hits a project that
    relies on `.editorconfig`. Then a bundled package finds the files above
    each opened document, turns each section into a `path`-glob scope, and
    writes them from the root down to the nearest file, so last-write-wins
    reproduces EditorConfig's rule that the nearest file takes precedence.
  - Order: after D008 and D089.

- **D091 — Settings UI**
  - Deferred: Settings (D070) are set from code. Definitions carry a type,
    default, and description so a UI can be built later, but none exists.
  - Reconsider: Discovering or changing settings from code becomes the
    friction in daily use. Then build the UI as an ordinary extension over
    `settings.define`, `get`, `set`, and `observe`. It keeps its choices in
    extension storage and applies them again in `start()`, which beats
    `pre-init` and loses to `post-init`, so it must show when a later write
    overrides a choice. Also decide whether it lists undefined keys that hold
    values, to help spot typos.
  - Order: after D070.

- **D016 — Sandboxed workspace roots**
  - Deferred: Local roots are trusted and intentionally follow symlinks.
  - Reconsider: Knot opens untrusted workspaces under a security boundary.

## Extensions and automation

- **D065 — Runtime grammar loading**
  - Deferred: D002 compiles a fixed tree-sitter grammar set into the binary;
    languages register by native grammar name, so an installed package cannot
    add a grammar Knot does not ship.
  - Reconsider: A user needs a language outside the bundled set. Then load
    WASM grammars on the V8 that already hosts extensions, and let a
    registration ship its grammar. Tree-sitter's own wasm store is built on
    wasmtime, which would add a second engine, and native grammar libraries
    are ruled out, so this needs a Knot-owned store backend over V8's WASM
    API: it provides the grammar's lex functions and external scanner, which
    the native parser calls on every lex step, so start with a spike that
    measures per-token call overhead and isolate ownership on background
    parse threads.

- **D067 — Language servers and semantic tokens**
  - Deferred: No language client exists. D002 prepares for one: LSP language
    IDs, `Send` text snapshots, a highlight vocabulary that semantic token
    types and modifiers map onto, and a semantic-token paint layer above
    syntax.
  - Reconsider: Daily editing needs diagnostics, navigation, hover, or
    semantic highlighting. Then decide whether the client is native or an
    extension, define a whole-range semantic-token contribution that scales
    to file-sized token counts unlike per-item anchored contributions, and
    connect diagnostics to decorations and completion to D022.
  - Order: after D087 if the client is an extension that spawns servers,
    D092 for server edit lists, and D114 for hover, inlay hints, and
    diagnostic text; before D022's language-server completion. Format on
    save builds on D115.

- **D032 — Extension source maps**
  - Deferred: D018 can load built JavaScript and report generated source
    locations without mapping them back to authoring files.
  - Reconsider: Debugging built extensions requires errors and stack traces to
    identify the original source locations.

- **D019 — Quotas, backpressure, slow-consumer policy, and timeouts**
  - Deferred: Correct policy depends on observed extension workloads.
  - Reconsider: Real extensions can affect responsiveness or resource use.

- **D020 — Public command schemas, aliases, and concurrent task graphs**
  - Deferred: Ordinary commands and awaited composition cover validated cases.
    Native typed handler declarations are implemented; public schema
    introspection remains deferred. User macros and repetition moved to D113.
  - Reconsider: A concrete automation workflow cannot be expressed cleanly.

- **D021 — General extension UI or WebView escape hatch**
  - Deferred: Surface-specific semantic UI remains sufficient.
  - Reconsider: A selected extension requires genuinely arbitrary UI.
  - Order: after D114; reconsider only if its surfaces fall short.

- **D107 — Package signing and verified publishers**
  - Deferred: The package manager (D101–D106) installs unsigned archives and git
    repositories and treats an explicit install confirmation plus entitlement
    grants (D087) as consent. A git install is only as trustworthy as the
    repository it came from: the pinned commit proves the content did not change
    after install, but not who wrote it. Archive hashes are published beside the
    file by the same party that serves it.
  - Reconsider: Installs are common enough that "trust this link" is not a
    sufficient story. Then define a signature over the installed content hash and
    commit, publisher keys and their distribution, how a source vouches for a
    publisher, and how revoked or rotated keys surface, without making signatures
    a condition for local installs.
  - Order: after the package manager plan (D101–D106).

- **D108 — Hosted package registry and publishing**
  - Deferred: The package manager (D101–D106) treats a git repository as the
    publishing mechanism, so it owns no artifact server, upload or publication
    flow, accounts, archive storage, download metrics, or in-editor catalog.
    Discoverability and voting live on a separate website that links
    repositories.
  - Reconsider: Git installs prove too risky or awkward (mutable history,
    missing build output in repositories, no version metadata to resolve
    against), or in-editor search and popularity ranking become worth running a
    server. Then define an index schema, publication and ownership, archive
    hosting and hashes, version metadata for resolution, and how it coexists
    with git sources under one lockfile.
  - Order: after the package manager plan (D101–D106).

- **D116 — Additional package sources**
  - Deferred: The package manager (D101–D106) installs from local archives and
    git repositories over public HTTPS, with one version per package identity.
  - Reconsider: A concrete workflow needs private repositories, SSH or token
    credentials, a local filesystem path as a long-lived source, or a
    per-project package set (which depends on D089). Then define credential
    storage, source precedence, and pinning per source rather than adding
    ad-hoc special cases to the lockfile.
  - Order: after D103, whose git sources it extends; a per-project package
    set also after D089.

- **D055 — Extensible icon and status-glyph system (icon packs)**
  - Deferred: Contributed tree items use a small fixed `TreeIcon` set, status
    uses generic severity badges, and actions are label-only. There is no way
    for a theme or an extension to replace or extend item, action, or status
    glyphs, and no icon-pack mechanism. Shipping a demo's verbs as tokens in the
    shared API would bake one feature's vocabulary into the boundary.
  - Reconsider: A theme or extension needs to supply or override glyphs (for
    example a file-type or source-control icon pack). Then define an
    application-owned icon registry with named tokens, theme and
    extension-provided packs, and a deterministic fallback, and migrate the
    fixed `TreeIcon` set and label-only actions onto it.
  - Order: after D064, whose themes would supply packs.

- **D022 — Rich completion policy and presentation**
  - Deferred: The prototype validated provider aggregation and replaceable
    native presentation.
  - Reconsider: Completion becomes part of a dogfood slice.
  - Order: after D064; language-server completion after D067.

- **D023 — Standardized AI integration**
  - Deferred: No concrete user workflow or provider boundary has been selected.
  - Reconsider: An AI-assisted editing slice is defined.
  - Order: after D087, which gates network access.

- **D057 — Extension and product localization (i18n)**
  - Deferred: Contributed view titles, tree labels, status text, native prompt
    text, and the bundled registry's own strings are literal English. Extensions
    and personal config can pass arbitrary strings, but there is no locale
    source, message catalog, or lookup for native-owned product text.
  - Reconsider: Shipping a second language becomes a product requirement. Then
    define locale discovery, a catalog format with fallback, which strings are
    extension-owned versus application-owned, contribution-time versus call-time
    resolution, and how native product text such as "Projects" reaches the same
    mechanism.

## Editor extensibility

These items bring the public API to Neovim-level depth for editing. D058 is
their forcing function; D067 and D027 drive the presentation and hook items.
Keep each primitive generic, so a Helix or Kakoune mode could use it too. The
[extensibility report](reports/extensibility.md) maps Vim's needs onto them.

- **D109 — Editor view, selection, and position API**
  - Deferred: Extensions get a buffer from a command, but no handle on the
    native editor view: they cannot read or set its cursor and selections,
    reveal a position, scroll, query the visible lines, or set the cursor
    style. Anchored positions exist only as decorated contributions, and the
    clipboard can be written but not read.
  - Reconsider: D058 or D027 is selected, or a bundled workflow needs
    cursor-driven editing. Then expose a view handle from a command target
    with several and rectangular selections, reveal and scroll, the visible
    line range, cursor style, undecorated anchored positions for marks, and
    clipboard read. Design it with D080's cursor observation.
  - Order: after D080; multi-selection editing with or after D092; before
    D112, D113, D027, and D058.

- **D110 — Key contexts and ordered key delivery**
  - Deferred: Keybindings are scoped only by focused view kind. An
    extension command runs as an async turn, so keys typed after a
    mode-switching command can reach native input before the turn finishes,
    and every bound key pays a pool round trip.
  - Reconsider: D058 or another modal or sequence-heavy extension is
    selected. Start with a spike that measures key-to-commit latency for an
    extension command and races a context switch against typed text. Then
    choose between extension-set key contexts resolved natively (with counts
    and sequences), native buffering of keys while a mode owner handles one,
    or both, as the report describes.
  - Order: after D086, which settles the keymap model; before D113 and D058.
    Do this spike first among this section's items.

- **D111 — Undo groups across edit batches**
  - Deferred: Each `applyEdits` batch is its own undo step, and an extension
    cannot group several batches with the native typing between them.
  - Reconsider: D058 needs `ciw…<Esc>` as one step, or replace-all (D001) or
    formatting applies several batches. Then add an explicit group that an
    extension opens and closes on a view or buffer, sharing the native
    grouping D074 introduces for editing commands.
  - Order: after D074; before D012 and D058.

- **D112 — Visual-line movement and layout queries**
  - Deferred: Vertical movement uses shaped x positions, and soft wrap
    (D078) defines visual lines in the view's line model. Extensions only see
    bytes.
  - Reconsider: D058 needs `j`/`k`, `gj`, `H`/`M`/`L`, and `zz`, or an
    extension needs screen-relative motion. Then expose native movement as
    parameterized commands (by visual line, page, or screen position) rather
    than exporting shaping data.
  - Order: after D078 and D109; before D058.

- **D113 — Command and input recording**
  - Deferred: Nothing can observe the stream of invoked commands together
    with the text typed between them, so neither repeat nor macros can be
    built. Absorbs user macros and repetition from D020.
  - Reconsider: D058 needs `.` and `q`, or a user macro workflow is selected.
    Then record a view's invoked commands with arguments plus its typed text
    as edits, and replay the recording through ordinary dispatch.
  - Order: after D109 and D110; before D058.

- **D114 — Extension-owned editor presentation**
  - Deferred: Contributions carry only `info`, `warning`, and `error`
    decoration and gutter tokens. Extensions cannot draw virtual text or
    inlay hints, anchor a floating panel to a position, or choose other theme
    decoration keys.
  - Reconsider: D067 needs hover, inlay hints, or diagnostic text, or D058
    needs search and mode highlights. Then add theme decoration keys,
    inline virtual text, and an anchored floating surface that reuses the
    semantic tree and text content kinds.
  - Order: after D064 and D068; before D067 and D058; D021 only if it falls
    short.

- **D115 — Lifecycle hooks and native command overrides**
  - Deferred: Extensions observe document and window changes after they
    commit. They cannot run before save, wrap or replace a native command,
    or react to a language change in time to affect it.
  - Reconsider: Format on save, an LSP client (D067), or a workflow that
    needs to customize a native command is selected. Then add awaited hooks
    at a few named points (before save first) and a way to wrap a native
    command, with a bounded wait so a slow hook cannot block saving.
  - Order: after D079, which defines the save pipeline; before format on
    save in D067.

## Terminals and generated surfaces

- **D040 — Physical application-keypad input**
  - Deferred: gpui 0.2.2 merges numeric-keypad keys with ordinary key identities.
    D026 retains numeric input but cannot emit distinct application-keypad
    sequences. The user chose deferral over vendoring gpui for a platform patch.
  - Reconsider: gpui exposes keypad identity, or a concrete TUI workflow requires
    a framework patch and its platform maintenance cost.

- **D041 — Additional terminal protocols and product events**
  - Deferred: D026 covers legacy keyboard/mouse protocols and HTTP(S) links;
    Kitty/modifyOtherKeys, graphics, OSC 52 clipboard access, titles, and bells
    have no selected product contract.
  - Reconsider: A daily shell/TUI workflow demonstrates a specific missing
    protocol or event; define its compatibility and product behavior together.
  - Order: with or after D063 if libghostty covers the protocol.

- **D042 — Additional terminal link destinations**
  - Deferred: D026 supports explicit and detected HTTP(S) destinations only.
    Arbitrary URI handlers and filesystem-path links require an opening policy.
  - Reconsider: A concrete workflow needs file navigation or another URI scheme.

- **D043 — Persistent terminal sessions**
  - Deferred: Live local sessions survive view reconstruction, but not application
    exit. Persistent processes/grid restoration need a separate lifecycle contract.
  - Reconsider: Restored terminal tabs, which start fresh shells, need their
    processes or scrollback to continue.

- **D044 — Simultaneous terminal views**
  - Deferred: A sole attachment keeps input ownership and PTY resize authority
    unambiguous while supporting cross-window transfer.
  - Reconsider: Daily use requires viewing one live session in multiple panes;
    define independent viewport and shared resize/input policies.

- **D045 — General terminal settings**
  - Deferred: Shared typography and the session-local mouse toggle cover D026;
    a persisted settings system has no selected contract.
  - Reconsider: Daily use needs configurable fonts or terminal preferences;
    define their scope and persistence with the application settings model.
  - Order: after D070; with D059.

- **D063 — Migrate the terminal engine to libghostty**
  - Deferred: The `alacritty_terminal` adapter meets current terminal
    behavior. Ghostty VT was rejected earlier for its separate PTY
    integration, pre-1.0 FFI, Zig packaging, integration size, and poor debug
    parsing performance.
  - Reconsider: libghostty offers a stable embeddable API and packaging that
    fits the Rust build, or it covers terminal behavior Alacritty lacks (such
    as D041 protocols). Then replace the emulator behind the existing
    application-owned session without changing its PTY ownership or
    single-attachment contract.
  - Order: with or before D041 if libghostty covers its protocols.

- **D038 — Remote PTYs**
  - Deferred: Local interactive sessions cover current terminal workflows.
  - Reconsider: Daily use requires remote shells; select transport,
    authentication, and disconnect behavior for a concrete workflow.

- **D039 — Shell integration**
  - Deferred: Ordinary terminal input and output cover current shell workflows.
  - Reconsider: Daily use requires working-directory tracking, command
    boundaries, or exit status; select supported shells and integration setup.

- **D027 — Interactive generated text views**
  - Deferred: Contributed text frames cover read-only generated output, and no
    product path creates a generated document. The extension-facing use case is
    an interactive generated view in the style of magit: a text surface whose
    ranges carry semantic actions (stage, open, expand), that refreshes in place
    with stable positions, and that has no persistence identity.
  - Reconsider: A concrete extension workflow such as Git status or search
    results needs interactive generated text beyond a read-only frame. Then
    define how an extension creates and refreshes the view, attaches per-range
    actions, and how such views behave in closure, recovery, and restoration.
  - Order: after D109, whose cursor and selection access per-range actions
    need.

- **D054 — Dedicated read-only generated text surface**
  - Deferred: Contributed text frames embed the ordinary editor view over a
    private read-only buffer. This reuses selection, copy, scrolling, and
    rendering without a second renderer, but couples tool frames to the editor
    key context and command surface.
  - Reconsider: A text frame needs behavior the editor surface cannot express,
    or tool-focused keymaps and editor commands conflict, at which point extract
    a dedicated read-only presentation over the same buffer layer.

## Platforms and accessibility

- **D062 — Positioning existing windows**
  - Deferred: gpui 0.2.2 sets a window's bounds only at creation and can
    afterwards only resize it. Session restoration and project switching always
    create their windows, so they apply saved geometry at creation and no
    product path needs to move an existing window.
  - Reconsider: A workflow needs to move or re-frame an open window, such as
    window arrangement commands, an extension API, or reusing windows during
    restoration. Then add a small native per-platform window-frame operation
    (`NSWindow` frame on macOS, `SetWindowPos` on Windows, X11 configure
    requests) beside gpui rather than vendoring it. Wayland compositors own
    placement, so positioning there stays best-effort.

- **D035 — macOS distribution**
  - Deferred: Local `Knot.app` builds cover dogfooding without release
    credentials or a distribution channel.
  - Reconsider: Knot is ready to run on other Macs; choose a release artifact,
    then add Developer ID signing, hardened runtime validation for V8,
    notarization, and an install/update path.

- **D030 — Accessibility bridge**
  - Deferred: gpui lacks the public platform bridge required by the semantic
    state already retained.
  - Reconsider: The framework exposes a viable bridge or Knot selects an
    implementation strategy.
