# Product Roadmap and Deferred Work

The selected initiative below has an explicit implementation order. The deferred
register that follows remains unordered. Completed decisions remain in
`decisions.md`; proposed architecture and checkpoint details live in active plans.

Each item has a stable `D###` identifier, a reason for deferral, and a concrete
trigger for reconsideration. IDs are never renumbered or reused. New items take
the next unused ID. When an item is promoted, its slice plan retains the ID and
the item is removed from this register.

## Selected initiative: production extension APIs and JavaScript projects

The [extension platform and project management plan](plans/extension-platform-and-projects-plan.md)
uses an ordinary JavaScript project manager to validate native application APIs,
extension UI, durable storage, multi-root workspaces, and session restoration
including unsaved documents. API foundation, application state/operations, handler
declarations, extension storage, native UI contributions, and the bundled project
registry are complete; approval and validation notes live in that plan.

Remaining order:

| Checkpoint | Work identifiers | Outcome |
| --- | --- | --- |
| 7 | D050 | Unsaved-content recovery independent of extension health |
| 8 | D051, D050 | Safe project switching with durable suspension/restoration |
| 9 | D052 | Multiple roots throughout UI and resource operations |
| 10 | D005, D051 | Layout, view-state, and startup restoration |

Checkpoints 1–6 are complete and approved; start checkpoint 7 with the
[extension platform plan](plans/extension-platform-and-projects-plan.md), whose
D050 sub-checkpoints are detailed in the
[focused recovery plan](plans/durable-document-recovery-plan.md).
Execute one checkpoint at a time and stop at its review gate; detailed scope and
acceptance belong to those plans. The early project registry is an intermediate
validation point, not initiative completion.

This initiative takes priority over the deferred register. Persistent undo (D012),
general UI/WebViews (D021), terminal restoration (D043), and broad scheduling
policy (D019) remain deferred. Exact view-state restoration belongs to selected
D005; tool-only layout memory shipped with the completed D049.

## Deferred register

## Editing and workbench

- **D031 — Current-file Find**
  - Deferred: Safe ordinary editing and workbench behavior can be dogfooded
    without a search UI.
  - Reconsider: Daily editing identifies literal in-file search as a recurring
    need; define focused query, live matching, edit-following navigation, and
    macOS bindings together in a selected slice.

- **D001 — Replace and advanced Find modes**
  - Deferred: These modes depend on a selected current-file Find design.
  - Reconsider: Daily use identifies required replacement, regular-expression,
    case, or whole-word behavior.

- **D002 — Syntax highlighting**
  - Deferred: Plain text is sufficient to validate safe editing and workbench
    ownership.
  - Reconsider: The editing loop is trustworthy and source readability becomes
    the dominant problem.

- **D006 — File drag and drop**
  - Deferred: Open and Save As establish workspace-independent documents first.
  - Reconsider: Platform interaction becomes a selected product slice.

- **D007 — Generated-search refresh and stale-target recovery**
  - Deferred: Immutable search results validated the text-surface boundary.
  - Reconsider: Bundled cross-file search demonstrates that refresh or recovery
    is required.

- **D034 — File explorer**
  - Deferred: The existing workspace tree supports basic directory browsing
    and file opening; a fuller explorer workflow has not been selected.
  - Reconsider: Daily editing needs navigation beyond the current tree, such as
    revealing the active file, refreshing directories, or filtering entries.

- **D037 — Whole-pane dragging and arbitrary split-tree reshaping**
  - Deferred: D004 moves individual tabs into existing panes or new edge splits;
    moving groups of tabs and restructuring whole split branches is a separate
    interaction and layout problem.
  - Reconsider: Daily use shows that moving tabs one at a time or creating edge
    splits is too cumbersome for a common layout change.

## Documents, storage, and history

- **D008 — Live filesystem watching and automatic reload**
  - Deferred: Save-time conflict detection is sufficient for initial safe use.
  - Reconsider: External edits are frequent enough that discovery only at Save
    is disruptive.

- **D009 — Binary buffers and encoding detection**
  - Deferred: The first slice explicitly targets UTF-8 text.
  - Reconsider: A concrete file workflow requires non-UTF-8 or binary
    inspection.

- **D010 — Filesystem mutation beyond create and replace**
  - Deferred: Rename, delete, and directory creation are not needed for text
    editing.
  - Reconsider: Workspace file management becomes a vertical slice.

- **D011 — Remote filesystem providers**
  - Deferred: The URI boundary is validated with local and memory providers.
  - Reconsider: A specific remote workflow is selected.

- **D012 — Persistent undo and history trees**
  - Deferred: Session-local linear undo is sufficient for dogfooding.
  - Reconsider: Users need edit history across restarts or branching history.
    Persistent view-state restoration is selected under D005.

- **D013 — Piece-table compaction and edit-log reclamation**
  - Deferred: Prototype storage is append-only and no real workload has shown
    unacceptable growth.
  - Reconsider: Long editing sessions produce measured memory or stabilization
    problems.

- **D014 — Replaceable line-start index**
  - Deferred: Linear suffix updates passed prototype workloads.
  - Reconsider: Real files demonstrate unacceptable edit latency.

- **D015 — URI deduplication across processes**
  - Deferred: The first slice handles identity within one application process.
  - Reconsider: Multi-process editing or document handoff is introduced.

- **D016 — Sandboxed workspace roots**
  - Deferred: Local roots are trusted and intentionally follow symlinks.
  - Reconsider: Knot opens untrusted workspaces under a security boundary.

## Extensions and automation

- **D032 — Extension source maps**
  - Deferred: D018 can load built JavaScript and report generated source
    locations without mapping them back to authoring files.
  - Reconsider: Debugging built extensions requires errors and stack traces to
    identify the original source locations.

- **D033 — Extension archive installation and unpacking**
  - Deferred: D018 installs extension directories directly.
  - Reconsider: Distributing or updating extensions as single archives becomes
    a concrete workflow.

- **D019 — Quotas, backpressure, slow-consumer policy, and timeouts**
  - Deferred: Correct policy depends on observed extension workloads.
  - Reconsider: Real extensions can affect responsiveness or resource use.

- **D020 — Public command schemas, aliases, user macros, repetition, and concurrent
  task graphs**
  - Deferred: Ordinary commands and awaited composition cover validated cases.
    Native typed handler declarations are implemented; public schema introspection
    and user automation remain deferred.
  - Reconsider: A concrete automation workflow cannot be expressed cleanly.

- **D021 — General extension UI or WebView escape hatch**
  - Deferred: Surface-specific semantic UI remains sufficient.
  - Reconsider: A selected extension requires genuinely arbitrary UI.

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

- **D022 — Rich completion policy and presentation**
  - Deferred: The prototype validated provider aggregation and replaceable
    native presentation.
  - Reconsider: Completion becomes part of a dogfood slice.

- **D023 — Standardized AI integration**
  - Deferred: No concrete user workflow or provider boundary has been selected.
  - Reconsider: An AI-assisted editing slice is defined.

- **D053 — Native explorer as a packaged extension**
  - Deferred: The explorer remains a shell-owned native tool while the extension
    view API is validated; converting it now would mix provider migration with
    the native frame work. Migration is sequenced after the project registry
    establishes bundled extension loading and after multi-root workspace
    semantics settle, so the workspace provider and file-action APIs are shaped
    by real project and root behavior rather than designed speculatively.
  - Reconsider: The extension view API can express workspace providers and
    explorer actions end to end, or maintaining the native explorer duplicates
    behavior that extensions already provide. Revisit as a focused slice once the
    earlier sequencing completes; pulling the migration forward requires
    explicitly re-scoping the native multi-root resource tree to the packaged
    extension.

- **D056 — TypeScript authoring for bundled extensions**
  - Deferred: Bundled packages ship checked plain JavaScript compiled into the
    binary. The extension API check runs `tsc` with `--checkJs` against the
    declarations, and installed packages take built JavaScript directly; nothing
    compiles TypeScript into the shipped sources yet.
  - Reconsider: A bundled workflow's size or shared types make hand-written
    JavaScript and JSDoc the dominant authoring cost. Then define a build step
    that type-checks and emits captured sources for bundled packages, with no
    runtime transpiler, and decide whether installed packages may also ship
    TypeScript.

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

- **D042 — Additional terminal link destinations**
  - Deferred: D026 supports explicit and detected HTTP(S) destinations only.
    Arbitrary URI handlers and filesystem-path links require an opening policy.
  - Reconsider: A concrete workflow needs file navigation or another URI scheme.

- **D043 — Persistent terminal sessions**
  - Deferred: Live local sessions survive view reconstruction, but not application
    exit. Persistent processes/grid restoration need a separate lifecycle contract.
  - Reconsider: Workbench restoration (D005) requires terminal continuity.

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

- **D038 — Remote PTYs**
  - Deferred: Local interactive sessions cover current terminal workflows.
  - Reconsider: Daily use requires remote shells; select transport,
    authentication, and disconnect behavior for a concrete workflow.

- **D039 — Shell integration**
  - Deferred: Ordinary terminal input and output cover current shell workflows.
  - Reconsider: Daily use requires working-directory tracking, command
    boundaries, or exit status; select supported shells and integration setup.

- **D027 — Additional generated text surfaces**
  - Deferred: Search did not expose another architectural boundary.
  - Reconsider: A concrete generated output remains useful as text but stresses
    the current model.

- **D054 — Dedicated read-only generated text surface**
  - Deferred: Contributed text frames embed the ordinary editor view over a
    private read-only buffer. This reuses selection, copy, scrolling, and
    rendering without a second renderer, but couples tool frames to the editor
    key context and command surface.
  - Reconsider: A text frame needs behavior the editor surface cannot express,
    or tool-focused keymaps and editor commands conflict, at which point extract
    a dedicated read-only presentation over the same buffer layer.

## Platforms and accessibility

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
