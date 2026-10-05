# Architecture Decisions

Durable choices and their rationale, validated by implementation and exploration.
[Architecture](architecture.md) and its subsystem references describe behavior;
linked exploration reports retain supporting evidence. Production directions
are labeled explicitly. [Deferred work](roadmap.md) tracks open scope and
reconsideration triggers, and [active plans](plans/) track implementation.

## Platform and UI

gpui 0.2.2 supplies the editor and shell through public APIs, avoiding a separate
renderer despite Skia's stronger standalone rendering result. Its macOS text
path passed the fixture corpus; incomplete public bidirectional-text ergonomics
require Knot's RTL alignment and logical-index mapping over glyph data.

Initial product dogfooding targets macOS. Windows passed the framework
portability checkpoint; Linux validation remains required before broader
production claims.

Local macOS builds package the Rust binary as `Knot.app` with the supplied
application icon and a plain-text document declaration. A small repository
script keeps local packaging independent of distribution credentials and lets
the product use gpui's activation, open-URL, and reopen callbacks. Signing and
notarization remain distribution work.

Reference: [workbench and lifecycle](architecture/workbench-and-lifecycle.md). Evidence: [renderer benchmark](archive/exploration/step2-renderer-benchmark.md),[framework comparison](archive/exploration/step3-framework-comparison.md).

## Text buffer and stable positions

Use a hand-built stable-ID piece table to retain direct control over edit events
and stable positions. Keep the core single-threaded and UTF-8-native; the
foreground owner serializes mutation, while presentation handles graphemes and
the extension boundary adapts strings. Measurements do not justify replacing
the line-start index or introducing a UTF-16 core.

Reference: [core and buffers](architecture/core-and-buffers.md). Evidence: [buffer benchmark](archive/exploration/step4-buffer-benchmark.md).

## Anchored ranges and contributions

Keep stable geometry in core and source, feature, lifecycle, and presentation
policy in the application. This lets overlapping features share one edit stream
without privileged coordination in core. Query indexes remain derived caches;
undo restores text through ordinary edits rather than reviving provider state.

Reference: [core and buffers](architecture/core-and-buffers.md). Evidence: [annotation benchmark](archive/exploration/step5-annotation-benchmark.md).

## Reversible edits

Use ordinary buffer mutations for undo and redo so reversal follows the same
edit-log and anchor-stabilization path as forward editing. The reversible edit
primitive is separate from history management, grouping, and view restoration.

Keep session-local linear history in the document's shared `BufferModel`, not
in an editor view. This makes edits and history common to every view of a
document while view identity remains an input to grouping and replay. Group
only adjacent typing and same-direction deletion within a short interaction
interval; focus, movement, paste, and explicit commands are predictable
boundaries. Store the originating view's caret and selection as plain byte
offsets on each entry: replay returns to the entry's exact content state, so
persistent anchors would add lifetime complexity without improving stability.
The model publishes the snapshot by view identity rather than retaining a view
entity, allowing another view to invoke replay without moving itself or keeping
a closed origin alive. Persistent or branching history and persistent
view-state recovery remain deferred until their recovery value justifies the
additional state model.
Track content-state identity separately from the monotonic public revision so
undo can return a document to its initial or persisted clean state without
weakening revision-based conflict and notification ordering.

Reference: [core and buffers](architecture/core-and-buffers.md).

## Extension runtime

Use JavaScript on V8, with Knot-owned protocols insulating the editor from
runtime machinery. The Deno prototype and its thread-per-extension runtime have
been removed. The host now depends directly on V8 152, owns process
initialization, and runs persistent shared isolates through a bounded worker
pool. Each turn enters the isolate under a V8 locker and drains explicit
microtasks before yielding it.

Size the pool to `min(available_parallelism, 4)` by default and allow an
explicit worker count for tests and diagnostics. This fixes thread cost as
extension count grows while still allowing independent extensions to execute
in parallel. Moving a persistent isolate only between locked turns preserves
its JavaScript globals and enables FIFO scheduling across workers without a
Knot-owned unsafe mobility wrapper.

Own one pool at the application boundary and deliver requests and terminal
lifecycle events through an awaitable inbox. This keeps foreground mutation on
gpui, avoids periodic wake loops and per-extension transport threads, and gives
runtime and foreground resources one exact-once teardown signal.

Serial callbacks within an extension simplify ownership; independent runtimes
progress through the bounded pool. Forced interruption is fatal to that
extension so cleanup has a definite lifecycle boundary. Immutable shared
UTF-16 snapshots serve V8 without exposing mutable storage or changing the
UTF-8 core.
Host operations retain V8 promise resolvers and yield the worker until a typed
response makes the isolate runnable again. This keeps asynchronous native work
from consuming pool capacity. The scheduler runs one non-preemptive JavaScript
turn per dequeue, preserves unrelated roots in extension-local FIFO order, and
admits same-tree continuations while a root is pending. It applies no automatic
quotas, deadlines, event dropping, or backpressure; those policies remain D019.

Installed packages supply a validated, immutable source graph to the host before
entry evaluation. Generated file URLs identify modules and preserve useful
failure locations, while graph membership and a package-root check prevent
imports from reaching another package or native filesystem paths. V8 retains
compiled modules and values inside its isolate; the application handles disk
discovery and passes only Knot-owned sources into the host.

Reference: [extension host](architecture/extension-host.md). Evidence: [v8 runtime](archive/exploration/step7-v8-runtime.md).

## Installed extension directories

Use the platform's local application-data `Knot/extensions` root and canonical
`@scope/package` directory names. Built JavaScript and a small JSONC manifest
make installation inspectable and keep package management outside the editor.
Discover and capture sources off gpui's foreground thread, then load eagerly in
deterministic dependency order. A dependency establishes startup order and
command availability without granting module access across isolates. Keep
explicit command names in one global namespace so invocation semantics do not
change with packaging.

Retain the load result for every package in an application-owned startup report
and expose it in each product window. The report makes malformed manifests and
runtime failures visible while independent packages continue. Reuse exact-once
lifecycle teardown for startup rollback so commands and semantic providers do
not outlive a failed entry.

## Personal configuration

Use two optional JavaScript entry modules in one selected per-user config
directory. Prefer an existing `XDG_CONFIG_HOME/knot`, then an existing
`~/.config/knot`, then the platform `Knot` config directory, so a launch has one
unambiguous source root on every platform. Keep config separate from installed
packages: it has no manifest, has its own lifecycle identity, and its pre-init
and post-init turns share JavaScript state across extension startup.

Treat config failure as fatal to startup because continuing after only part of
a personal setup ran would leave the editor in an unpredictable state. Hold
product windows and launch requests until both phases succeed; on failure,
unload all startup lifecycles and present an error window that stays available
for copying the diagnostic or quitting. Installed-package failures keep their
independent reporting and do not stop other packages or post-init.

Reference: [extension host](architecture/extension-host.md) and
[workbench lifecycle](architecture/workbench-and-lifecycle.md).

## Commands and keymaps

Use one Knot-owned command namespace and dispatcher across invocation sources.
The captured window focus path lets native views claim or decline before
workbench and application behavior. Captured targets make palette and delayed
execution preserve user intent across focus changes.
Extensions may handle an existing command for a declared view kind without
owning another public command name. Each live view instance is bound and
validated by Knot; the application participant handles global extension
commands only after the focused view and workbench decline.
Nested extension invocation reuses the parent's captured window for handler
selection, then keeps the bridge's inline same-lifecycle execution and
one-child ancestry rules. This preserves composition without a second window
dispatch for native children.

Awaited command composition runs one root tree at a time. Nested same-extension
frames avoid self-queue deadlock; ancestry checks and one unfinished child per
parent bound composition without introducing a general task scheduler. Commands
return structured execution outcomes, not semantic values; ordinary JavaScript
functions remain preferable when command lookup and routing are unnecessary.

Fixed gpui bindings and semantic key contexts cover the validated keymap cases.
Raw input protocols stay outside registered commands.

Reference: [commands](architecture/commands.md). Evidence: [command keymap plan](archive/exploration/step11-command-keymap-plan.md).

## Views and extension UI

Share text and contributions through buffer models while keeping interaction
and presentation per view. Extensions publish surface-specific semantic data;
native views retain rendering and input ownership so JavaScript cannot block
painting. A general declarative widget tree is not planned; a WebView remains a
possible escape hatch if a concrete arbitrary-UI need appears.

Keep horizontal movement and deletion on grapheme boundaries, word navigation
on Unicode word boundaries, and vertical movement on a remembered shaped x
coordinate. Byte columns remain storage coordinates; they cannot preserve the
visual column across ASCII, combining text, CJK, and emoji lines.

Reference: [semantic ui](architecture/semantic-ui.md).

## Workbench layout and view lifetime

Use a binary split tree with tabbed leaves and stable pane/tab identities.
Application-owned documents can outlive individual views; application-wide
last-view knowledge is therefore required for correct closure.

Coordinate tab, window, and quit closure through one application-level flow.
Defer presentation changes until all decisions and saves succeed, preventing
partial closure on cancellation or failure. Empty-workbench replacement is
product-shell policy, not a layout invariant.

Reference: [workbench and lifecycle](architecture/workbench-and-lifecycle.md).

## Terminal

Use a native terminal surface backed by `alacritty_terminal`, separate from
text buffers. Ghostty VT offered no concrete behavioral advantage sufficient to
offset separate PTY integration, pre-1.0 FFI, Zig packaging, integration size,
and poor debug parsing performance.

The application owns a registry of stable terminal sessions; each workbench
terminal tab owns the sole attached view. The process and grid survive view
reconstruction and transfer to another window. One attached view keeps resize
authority unambiguous. Releasing a view does not stop its session. Successful
tab, window, and application closure removes affected sessions from the registry
and shuts them down after dirty-document decisions; natural exit leaves the
final grid visible until the tab is closed or restarted. PTY shutdown and child
reaping run away from the foreground thread.

The existing Alacritty adapter supplied the required session and product
behavior without copying Zed code. Zed's GPL-compatible implementation remains
an attributed reference for concrete input, rendering, or event behavior if
later compatibility work exposes a gap.

Reference: [workbench and lifecycle](architecture/workbench-and-lifecycle.md). Evidence: [terminal plan](archive/exploration/step9-terminal-plan.md).

## Generated text surfaces

Use ordinary buffers and editor views when generated output remains useful as
selectable, scrollable, copyable text. Editability belongs to application policy,
and surface controllers retain semantic identity instead of parsing displayed
text. Search validated this boundary without special cases in generic buffer
ownership or a second generated surface.

Reference: [semantic ui](architecture/semantic-ui.md). Evidence: [text surface plan](archive/exploration/step10-text-surface-plan.md).

## Capability aggregation

Keep completion aggregation feature-specific and view-owned. Deterministic
merging makes results independent of provider arrival order, while local provider
failure preserves other results. Stable semantic item identities let native
presentations be replaced without restarting provider work or recovering
semantics from labels.

Reference: [semantic ui](architecture/semantic-ui.md). Evidence: [capability aggregation plan](archive/exploration/step12-capability-aggregation-plan.md).

## URI workspaces and filesystem providers

Use normalized URIs and one asynchronous byte-oriented provider boundary so
generic document and workspace code is independent of platform paths. Local and
memory providers validate the same contract. Product documents are independent
of workspace containment because Open and Save As can select any user resource.

Use no-clobber creation, conditional replacement, and atomic local installation
to protect existing content and expose external conflicts. Commit persistence
identity only after successful I/O, and mark only the captured revision saved
so racing edits stay dirty. Resource ownership is application-wide to avoid
multiple open documents claiming the same normalized destination.

Local workspace roots are trusted application boundaries, not security
sandboxes: symlinks deliberately remain usable even when they lead outside the
root.

Reference: [documents and persistence](architecture/documents-and-persistence.md). Evidence: [uri filesystem plan](archive/exploration/step13-uri-filesystem-plan.md).
