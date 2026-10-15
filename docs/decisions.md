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

Initial product dogfooding targets macOS. Windows 11 is also a local dogfooding
target after native startup, personal configuration, editor, persistence,
command, terminal, and protected-closure checks on one desktop session. This
does not establish a Windows distribution or broad hardware certification.
Linux passed a limited framework and native product smoke checkpoint on
Omarchy/Hyprland Wayland, using personal clipboard and palette bindings.
Broader Linux production claims still require validation.
Evidence: [Windows product checkpoint](archive/plans/d029-windows-product-polish-plan.md)
and [Linux framework checkpoint](archive/plans/d028-linux-framework-plan.md).

Use ordinary Windows Control shortcuts for product defaults. When a platform's
native picker cannot select files and folders together, ask which kind the user
wants before opening its single-kind picker. This preserves both Open flows
through gpui's public platform capability without replacing the picker.

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

Treat disposal as immediate local inactivity followed by native acknowledgement.
Keep one result, including rejection, for repeated calls so a failed cleanup does
not silently become success or create a second mutation. Earlier accepted
mutations/invalidation settle before cleanup acknowledgement; executing callbacks
are excluded from that barrier so self-disposal cannot deadlock. Authoritative
teardown releases remaining foreground ownership without depending on JavaScript
finally blocks or a disposer retry. Host release uses the same cleanup while
buffer models and views may still be retained.

Decode boundary data by recognized typed fields rather than JSON conversion
hooks or diagnostic strings. Public host error codes come from native failure
variants; type and numeric errors remain author-side categories. Preserve native
integer precision instead of silently rounding identities, revisions or offsets.
A bad callback or item remains a recoverable failure of its request.

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

Require an explicit installed-package API version before entry evaluation. API
version describes host contracts; package version and dependencies describe the
package itself. Retain recoverable identity before field validation so version
failures cannot conceal duplicates or turn a known rejected dependency into a
missing package. Personal configuration deliberately stays manifest-free.

Keep native dispatch functions in engine-owned persistent handles, outside the
author's global namespace. Native callback context and buffer proxy construction
must come from admitted native work, and replacing a global function must not
redirect dispatch. Release those handles with the isolate's other owned state.

## Personal configuration

Use two optional JavaScript entry modules in one selected per-user config
directory. Prefer an existing `XDG_CONFIG_HOME/knot`, then an existing
`~/.config/knot`, then the platform config directory for `Knot` (spelled `knot`
on Linux), so a launch has one
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

Contextual extension operations use the native-admitted command tree capture,
including across awaits and inline or cross-lifecycle children. Keep explicit
contextual target arguments as authority checks rather than a focus lookup:
copying or retaining a target does not grant access outside its live tree.
Validate the exact presentation as well as tab identity so replacing an editor
under the same document or transferring it cannot retarget requests. Valid
nontext captures have a null buffer; missing context, stale authority, unsupported
selection and cancellation remain distinct failures. Activation and unrelated
callbacks must use explicit buffers; clipboard writes are application-scoped.
This preserves invocation intent without giving asynchronous roots ambient
access to whichever window happens to be focused.

Awaited command composition runs one root tree at a time. Nested same-extension
frames avoid self-queue deadlock; ancestry checks and one unfinished child per
parent bound composition without introducing a general task scheduler. Commands
return structured execution outcomes, not semantic values; ordinary JavaScript
functions remain preferable when command lookup and routing are unnecessary.

Keep one application-owned keymap registry with a merged effective map. Native,
extension, and personal registrations retain owner records so replacement,
unbinding, failed startup, and unload can reveal lower-priority bindings.
Personal config outranks extensions, which outrank native defaults. Within one
source, view-specific bindings outrank global bindings, and later registrations
win remaining ties. This order is resolved by Knot because gpui's context
depth cannot express source priority. gpui recognizes keys and scopes, while
the command dispatcher handles the selected semantic command. A null unbind
leaves raw view input available. Menus and the palette invoke commands without
consulting the keymap; a global `copy` binding routes to different handlers by
focused view. Raw input protocols stay outside registered commands.
Apply JavaScript mutations as one ordered batch per V8 turn so invalid input
cannot leave a partial keymap and startup readiness follows foreground state.

Reference: [commands](architecture/commands.md). Evidence: [D003b keymap plan](archive/plans/d003b-keymaps-plan.md).

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

Relocate tabs through typed workbench transitions and an application foreground
coordinator. Capture the presentation entity as well as pane, tab, and surface
identities so replacement under unchanged document identity is still stale.
Revalidate and prepare the destination before mutation, keeping transfer
independent of protected closure. Retain document editor entities to preserve
their view state; hand off terminal attachments across windows while preserving
the registered session. Destination-local identities keep captured commands
from retargeting a transferred tab. Creating a destination window first keeps
creation failure from removing the source presentation.

Coordinate drag delivery with a transient application-wide foreground state.
macOS continues delivering pressed drag events to the originating window, so
map its content coordinates to the screen pointer and hit-test painted targets
in the foremost Knot window. Compensate the shared gpui preview for stale mouse
positions in other windows. Clear visual state before deferring the transfer
past input dispatch, allowing either endpoint to receive the release without
borrowing a destination during its own update. Outside-frame releases create
a destination near the pointer before moving the source; invalid space inside
Knot frames does not create a window. Exact endpoint validation remains the
relocation coordinator's responsibility.

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

Use Alacritty's parser-produced device, cursor, color, and size replies through
an ordered generation-scoped foreground queue; coalesce only repaint wakeups.
This keeps terminal protocol interpretation in the emulator and PTY writes in
the session's existing asynchronous channel. Keyboard encoding reads emulator
modes after product command precedence. Focus reports follow both view focus
and native window activation. Physical application-keypad support remains
[D040](roadmap.md) until gpui retains its native identity; the user chose this
bounded limitation over vendoring the framework.

Provide an enabled-by-default, per-terminal mouse-reporting toggle through the
shared command system. Disabling reporting supports ordinary drag + native Copy
inside TUIs, while preserving application-requested modes for re-enabling.
Keep this session-local preference across restart and attachment handoff.

Choose a mouse gesture's recipient on press and retain it through release.
Shift chooses local selection; ongoing application gestures retain their paired
release even if modifiers change. Wheels use their current route independently.
This gives each pointer gesture one owner and prevents a modifier change or
attachment handoff from leaving a reported button pressed. Report viewport
cells separately from scrollback selection coordinates, and suppress coordinates
that a requested legacy encoding cannot represent.

Keep hyperlink recognition in the view's displayed-grid adapter. OSC 8 identity
is authoritative; HTTP(S) detection follows soft wraps and emulator cell widths.
Restrict the initial native opener to HTTP(S), normalize destinations with the
existing URL parser, and trim sentence punctuation while retaining balanced path
delimiters. Modifier-click owns the whole gesture before application reporting;
release revalidates the captured grid and geometry so output or lifecycle changes
cannot open an old destination. Visible-grid detection avoids inventing a
truncated URL at the viewport edge; explicit OSC 8 destinations remain complete.

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

## Application document and root identity

Allocate a UUID per open application document separately from its session-local
collection ID and transport handle. Resource association changes preserve that
identity; reopening after closure allocates another. This supports future durable
metadata without deriving identity from a mutable URI or promising recovery now.

Keep workspace roots owned by each window, including an empty root set. Ordered
root records retain their IDs while their normalized URI is retained. A window's
root-set generation survives clear/reinstall so old asynchronous directory work
cannot become current again. The model accepts at most one root.

Reference: [documents and persistence](architecture/documents-and-persistence.md).
