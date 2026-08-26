# Deferred and Long-Term Work

This is an unordered register of known work that does not belong to the active
vertical slice. An item moves into a slice plan only when a concrete use case
requires it. Completed decisions remain in `decisions.md`; this document does
not reopen them or imply implementation order.

Each entry records why it is deferred and the trigger for reconsidering it.

## Editing and workbench

| Work | Why deferred | Reconsider when |
| --- | --- | --- |
| In-buffer Find and Replace | Not required for the first editing session. | Daily use makes navigation friction material. |
| Syntax highlighting | Plain text is sufficient to validate safe editing and workbench ownership. | The editing loop is trustworthy and source readability becomes the dominant problem. |
| Configurable and extension-defined keymaps | Fixed native bindings cover initial use. | Dogfooding identifies concrete remapping needs or the extension slice requires bindings. |
| Tab dragging and general docking | Initial windows, panes, tabs, and splits need only command-driven placement. | Rearranging real sessions is common enough to define the interaction. |
| Workbench layout and session restoration | Persistence semantics should follow a workbench people actually use. | Window and split behavior stabilizes through daily use. |
| File drag and drop | Open and Save As establish workspace-independent documents first. | Platform interaction becomes a selected product slice. |
| Project-wide search | Current-buffer Find and workspace behavior should be learned first. | Repository use shows the required result, refresh, and navigation semantics. |
| Generated-search refresh and stale-target recovery | Immutable search results validated the text-surface boundary. | Project search becomes an active slice. |

## Documents, storage, and history

| Work | Why deferred | Reconsider when |
| --- | --- | --- |
| Live filesystem watching and automatic reload | Save-time conflict detection is sufficient for initial safe use. | External edits are frequent enough that discovery only at Save is disruptive. |
| Binary buffers and encoding detection | The first slice explicitly targets UTF-8 text. | A concrete file workflow requires non-UTF-8 or binary inspection. |
| Filesystem mutation beyond create and replace | Rename, delete, and directory creation are not needed for text editing. | Workspace file management becomes a vertical slice. |
| Remote filesystem providers | The URI boundary is validated with local and memory providers. | A specific remote workflow is selected. |
| Persistent undo, history trees, and view-state restoration | Session-local linear undo is sufficient for dogfooding. | Users need recovery across restarts or branching history. |
| Piece-table compaction and edit-log reclamation | Prototype storage is append-only and no real workload has shown unacceptable growth. | Long editing sessions produce measured memory or stabilization problems. |
| Replaceable line-start index | Linear suffix updates passed prototype workloads. | Real files demonstrate unacceptable edit latency. |
| URI deduplication across processes | The first slice handles identity within one application process. | Multi-process editing or document handoff is introduced. |
| Sandboxed workspace roots | Local roots are trusted and intentionally follow symlinks. | Knot opens untrusted workspaces under a security boundary. |

## Extensions and automation

| Work | Why deferred | Reconsider when |
| --- | --- | --- |
| Production V8 isolate pool and scheduling | Persistent thread-affine prototype runtimes validated the API boundary, not production resource policy. | The first production extension slice is selected. |
| Extension packaging, dependencies, resolution, and load order | No installable extension product exists yet. | Extensions must be distributed rather than embedded as fixtures. |
| Quotas, backpressure, slow-consumer policy, and timeouts | Correct policy depends on observed extension workloads. | Real extensions can affect responsiveness or resource use. |
| Command schemas, aliases, macros, repetition, and concurrent task graphs | Ordinary commands and awaited composition cover validated cases. | A concrete automation workflow cannot be expressed cleanly. |
| General extension UI or WebView escape hatch | Surface-specific semantic UI remains sufficient. | A selected extension requires genuinely arbitrary UI. |
| Rich completion policy and presentation | The prototype validated provider aggregation and replaceable native presentation. | Completion becomes part of a dogfood slice. |
| Standardized AI integration | No concrete user workflow or provider boundary has been selected. | An AI-assisted editing slice is defined. |

## Terminals and generated surfaces

| Work | Why deferred | Reconsider when |
| --- | --- | --- |
| Production `TerminalSession` separation | The prototype validated the terminal path with view-owned state. | Terminal use enters the product workbench. |
| Terminal persistence, simultaneous views, remote PTYs, and shell integration | These exceed the validated local interactive-session use case. | Real terminal workflows require them. |
| Full terminal compatibility, mouse reporting, and hyperlinks | The emulator choice is settled; compatibility breadth is product work. | Terminal dogfooding exposes the required behaviors. |
| Additional generated text surfaces | Search did not expose another architectural boundary. | A concrete generated output remains useful as text but stresses the current model. |

## Platforms and accessibility

| Work | Why deferred | Reconsider when |
| --- | --- | --- |
| Linux framework checkpoint | Initial product dogfooding targets macOS. | The macOS editing slice is usable; Linux remains required before broader production claims. |
| Windows product polish | The framework path passed its portability checkpoint, not product validation. | Windows becomes a supported dogfooding platform. |
| Accessibility bridge | gpui lacks the public platform bridge required by the semantic state already retained. | The framework exposes a viable bridge or Knot selects an implementation strategy. |
