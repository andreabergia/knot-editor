# Deferred and Long-Term Work

This is an unordered register of known work that does not belong to the active
vertical slice. An item moves into a slice plan only when a concrete use case
requires it. Completed decisions remain in `decisions.md`; this document does
not reopen them or imply implementation order.

Each item has a stable `D###` identifier, a reason for deferral, and a concrete
trigger for reconsideration. IDs are never renumbered or reused. New items take
the next unused ID. When an item is promoted, its slice plan retains the ID and
the item is removed from this register.

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

- **D003 — JavaScript-configurable and extension-defined keymaps**
  - Deferred: Fixed native bindings cover initial use; loading and changing
    bindings through JavaScript is not required by the current slice.
  - Reconsider: Dogfooding identifies concrete remapping needs or the extension
    slice requires JavaScript-defined bindings.

- **D004 — Tab dragging and general docking**
  - Deferred: Initial windows, panes, tabs, and splits need only command-driven
    placement.
  - Reconsider: Rearranging real sessions is common enough to define the
    interaction.

- **D005 — Workbench layout and session restoration**
  - Deferred: Persistence semantics should follow a workbench people actually
    use.
  - Reconsider: Window and split behavior stabilizes through daily use.

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

- **D012 — Persistent undo, history trees, and persistent view-state restoration**
  - Deferred: Session-local linear undo is sufficient for dogfooding.
  - Reconsider: Users need history or view-state recovery across restarts, or
    branching history.

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

- **D020 — Command schemas, aliases, macros, repetition, and concurrent task
  graphs**
  - Deferred: Ordinary commands and awaited composition cover validated cases.
  - Reconsider: A concrete automation workflow cannot be expressed cleanly.

- **D021 — General extension UI or WebView escape hatch**
  - Deferred: Surface-specific semantic UI remains sufficient.
  - Reconsider: A selected extension requires genuinely arbitrary UI.

- **D022 — Rich completion policy and presentation**
  - Deferred: The prototype validated provider aggregation and replaceable
    native presentation.
  - Reconsider: Completion becomes part of a dogfood slice.

- **D023 — Standardized AI integration**
  - Deferred: No concrete user workflow or provider boundary has been selected.
  - Reconsider: An AI-assisted editing slice is defined.

## Terminals and generated surfaces

- **D025 — Terminal persistence, simultaneous views, remote PTYs, and shell
  integration**
  - Deferred: These exceed the validated local interactive-session use case.
  - Reconsider: Real terminal workflows require them.

- **D026 — Full terminal compatibility, mouse reporting, and hyperlinks**
  - Deferred: The emulator choice is settled; compatibility breadth is product
    work. The terminal currently draws Menlo at 13 px in fixed 8×16 cells,
    while the editor draws Menlo at 14 px with explicit Arabic, CJK, and emoji
    fallbacks. Terminal font size, cell metrics, and fallback behavior are not
    driven by shared Knot settings.
  - Reconsider: Terminal dogfooding shows font or glyph mismatch, clipping, or
    a need for mouse reporting, hyperlinks, or wider terminal compatibility.

- **D027 — Additional generated text surfaces**
  - Deferred: Search did not expose another architectural boundary.
  - Reconsider: A concrete generated output remains useful as text but stresses
    the current model.

## Platforms and accessibility

- **D035 — macOS distribution**
  - Deferred: Local `Knot.app` builds cover dogfooding without release
    credentials or a distribution channel.
  - Reconsider: Knot is ready to run on other Macs; choose a release artifact,
    then add Developer ID signing, hardened runtime validation for V8,
    notarization, and an install/update path.

- **D028 — Linux framework checkpoint**
  - Deferred: Initial product dogfooding targets macOS.
  - Reconsider: The macOS editing slice is usable; Linux remains required
    before broader production claims.

- **D029 — Windows product polish**
  - Deferred: The framework path passed its portability checkpoint, not product
    validation.
  - Reconsider: Windows becomes a supported dogfooding platform.

- **D030 — Accessibility bridge**
  - Deferred: gpui lacks the public platform bridge required by the semantic
    state already retained.
  - Reconsider: The framework exposes a viable bridge or Knot selects an
    implementation strategy.
