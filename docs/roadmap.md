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
uses a JavaScript project manager to establish production application APIs,
native semantic UI contributions, durable extension storage, multi-root
workspaces, and session restoration including unsaved documents. D046 is complete
and approved on 2026-10-15. D047 checkpoints 2–3 are complete and approved on
2026-10-15. D053 is complete, approved and archived. D048 is complete, approved and archived
on 2026-10-16. Resume numbered checkpoint 5 (D049) next; the broader initiative
remains in progress.
Execute one checkpoint at a time and stop at its review gate.

| Priority | Checkpoint | Work identifiers | Outcome |
| --- | --- | --- | --- |
| 1 | Public API foundation (✅ complete) | D046 | Documented and typed JS contracts; prototype coupling removed at the API boundary |
| 2 | Application state and events (✅ complete) | D047 | Window/document/workspace queries and race-free observation |
| 3 | Resource and document operations (✅ complete) | D047 | JS opens resources and manages documents through product flows |
| 3a | Handler declarations refactoring (✅ complete) | D053 | Function-defined typed async operations/errors/native commands with automatic registration |
| 4 | Durable extension storage (✅ complete) | D048 | Rust-owned SQLite and lifecycle-independent extension namespaces |
| 5 | Native extension UI | D049 | Dockable native tool frames, extension contributions, and picker/input surfaces |
| 6 | Project registry validation | D051 | A usable JavaScript project manager with one root per workspace |
| 7 | Unsaved-document recovery | D050 | Durable content recovery independent of extension health |
| 8 | Safe project switching | D051, D050 | Durable suspension and document restoration without losing edits |
| 9 | Multiple workspace roots | D052 | Root collections supported throughout UI and opening flows |
| 10 | Layout and startup restoration | D005, D051 | Project/session restoration of windows, panes, tabs, and view state |

The [detailed D046 plan](archive/plans/d046-public-api-foundation-plan.md) retains the
current namespaces, exposes captured command context, and removes global-focus
fallback. There is no backward-compatibility requirement: contracts and repository
consumers change together, with unsupported package API versions diagnosed before
evaluation.

Explicit follow-up tasks within the selected initiative:

- **D047, checkpoint 2:** document/window/workspace queries, live target types
  and application observation are complete and approved on 2026-10-15 in the
  [D047 plan](archive/plans/d047-application-state-and-operations-plan.md).
- **D047, checkpoint 3:** file Open now uses explicit window/pane/view targets or
  an admission-time captured command/active window. Folder/root operations,
  untitled creation, presenting documents and protected closure are implemented;
  automated acceptance passes, and the user approved completion on 2026-10-15.
- **D053, separate maintenance interlude:** the
  [handler declarations refactoring](archive/plans/d053-handler-declarations-refactoring-plan.md)
  is complete, approved and archived on 2026-10-15. All current host capabilities
  and native commands use annotated typed async handlers with automatic registration.
  Focus routing and dynamic JS registration retain their existing ownership.
  Remote Linux/Windows results remain an explicitly recorded validation limitation
  of the D053 interlude.
- **D048, checkpoint 4:** complete and approved on 2026-10-16 in the
  [D048 plan](archive/plans/d048-extension-storage-plan.md). Private JSON read/write/delete/list,
  stable per-owner databases, asynchronous shutdown drain, and interruption/reopen
  validation are implemented. Related state uses atomic replacement of one record.
- **D049, checkpoint 5:** finalize native contribution APIs and pass originating
  window/instance context to providers and actions, with cross-window isolation.
  Execute the [dockable workbench plan](plans/d049-dockable-workbench-plan.md)
  before the [native extension UI plan](plans/d049-native-extension-ui-plan.md).
  Support sidebar placements and mixed document/tool tab groups through the shared
  split/tab model; validate content with a Git-status-style fixture. Floating
  tool windows and persisted layout remain deferred.

Each task includes public declarations, documentation, and boundary tests. Do not
publish speculative namespaces in D046 or use these deferrals to preserve its
implicit global context.

This initiative takes priority over the deferred register. The early project
registry is an intermediate validation checkpoint, not the final scope. D005 is
promoted into the plan. D012 retains persistent undo/history work; view-state
restoration belongs to D005. D034 may follow the multi-root work but is not
required for a fuller file explorer. General widget UI/WebViews (D021), terminal
restoration (D043), and broad scheduling policy (D019) remain deferred.

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
    Internal native-handler argument typing, Rust declaration macros and automatic
    registration are selected under D053; public schema introspection and user
    automation remain here.
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
