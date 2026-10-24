# Architecture Decisions

Validated choices, their rationale, and essential supporting evidence.
[Architecture](architecture.md) and its subsystem references own current runtime
behavior; API contracts live in code. [Roadmap](roadmap.md) and
[active plans](plans/) own deferred and proposed work.

## Platform and UI

Use gpui through public APIs so the editor and shell share a framework rather
than maintaining a separate renderer. Its macOS text path passed styled,
viewport-culled text, selection, IME geometry, annotation, split-pane, Arabic,
CJK, emoji, long-line, and large-file fixtures. Public bidirectional-text
limitations require Knot's RTL alignment and logical-index mapping.

In separate rendering-primitive benchmarks on Apple M1 Pro/macOS release builds,
Skia had lower p50 times on every fixture but higher and less predictable p99
spikes. Those measurements did not compare complete frameworks; gpui met the
product bar, so iced and floem were not evaluated.

macOS is the initial dogfooding platform. Windows 11 passed local startup,
config, editing, persistence, commands, terminal, and protected-closure checks
on one desktop session. Linux passed limited framework and native product smoke
checks on Omarchy/Hyprland Wayland. These establish bounded local evidence, not
broad platform certification. Local macOS packaging stays independent of signing
and distribution credentials.

Reference: [workbench and lifecycle](architecture/workbench-and-lifecycle.md).

## Text buffer and stable positions

Use a hand-built stable-ID piece table for direct control over edit events and
stable positions. A single-threaded UTF-8 core matches foreground ownership;
presentation handles graphemes and the extension boundary adapts strings.

On Apple M1 Pro/macOS release builds with a 1,001,395-line, 29 MiB Rust fixture,
edits measured p50 678 µs with the line index and 75 µs without it. Linear suffix
shifts accounted for about 88% of indexed latency; position issuance and
resolution were sub-microsecond. This supports deferring index replacement until
real workloads justify it; absolute tails included system noise.

Reference: [core and buffers](architecture/core-and-buffers.md).

## Anchored ranges and contributions

Keep stable geometry in core and feature, source, lifecycle, and presentation
policy in the application. Independent features can share one edit stream
without privileged coordination. Query indexes remain derived caches.

On Apple M4 Pro/macOS 15 release builds with 10,000 annotations on a million-line
buffer, token stabilization measured p50 12.2–12.4 µs per edit versus
69.5–69.7 µs for offset remapping across three and six sources. Token p99 was
higher (165–171 µs versus 88–90 µs); resolving all annotations cost about 1.79 ms.
This supports inline stabilization at the tested scale, not uniform tail gains.

Reference: [core and buffers](architecture/core-and-buffers.md).

## Reversible edits

Undo uses ordinary mutations so it shares forward editing's edit-log and anchor
repair path. Session-local history belongs to the shared buffer model so every
view sees one edit history. View snapshots use byte offsets: replay restores the
exact content state, so persistent anchors would add lifetime complexity without
improving stability. View identities avoid retaining closed view entities.

Track content identity separately from monotonic public revisions so undo can
return to clean content without weakening revision-based conflict detection.
History persistence and branching await a demonstrated recovery benefit.

Reference: [core and buffers](architecture/core-and-buffers.md).

## Extension runtime

Use JavaScript on direct V8, with Knot-owned protocols insulating editor models
from runtime machinery. Persistent isolates preserve author state; a bounded
worker pool fixes thread cost as extension count grows. Locked turns permit
worker migration without a Knot-owned unsafe mobility wrapper. Serial callback
roots simplify ownership, while yielding for host work preserves pool capacity.
Forced interruption is fatal to the affected lifecycle, giving cleanup a definite
boundary.

An application-owned pool and awaitable inbox avoid periodic wake loops and
per-extension transport threads. Authoritative teardown releases foreground
resources even when JavaScript cannot run cleanup. Local disposer inactivity
prevents queued callbacks; native acknowledgement orders accepted mutations
without waiting for a self-disposing callback to finish.

Immutable external UTF-16 snapshots serve V8 without changing the UTF-8 core.
A prototype V8 149/deno_core benchmark on Apple M1 Pro/macOS release builds
measured 10 MiB Unicode marshaling at p50 36.0 ms versus 10.6 ms for cold external
UTF-16 and 321 µs cached. A whole-document UTF-16 boundary-table adapter exceeded
its 32 MiB isolate heap cap. This supports revision/range-scoped snapshots and
caching, not a public byte-array API or a current pool-performance claim.

Immutable captured module graphs separate filesystem discovery from execution
and preserve useful source locations without granting cross-package imports.
Private native dispatch handles prevent author globals from fabricating callback
context. Typed field decoding and error variants avoid JSON conversion hooks,
diagnostic-string protocols, and silent numeric precision loss.

Reference: [extension host](architecture/extension-host.md).

## Handler declarations

Annotated async handlers keep argument, result, and error types paired at their
definition. Generated adapters and linked descriptors avoid parallel registration
and transport tables. Deterministic assembly diagnoses duplicates; descriptor
metadata checks the TypeScript error union without another membership list.
Explicit error codes survive Rust renames.

Host operations and native commands share the owned async call contract, while
foreground owners remain independent of response transport. Short foreground
requests run in the dispatch turn to preserve key-command/text-input ordering;
suspended work holds no gpui borrow. Synchronous participant selection preserves
focus precedence and inline extension composition.

References: [extension host](architecture/extension-host.md) and
[commands](architecture/commands.md).

## Installed extension directories

Use inspectable installed directories containing built JavaScript and a JSONC
manifest; package management stays outside the editor. Dependencies establish
startup order and command availability, not module access across isolates.
A global command namespace keeps invocation independent of packaging.

Require an explicit API version before evaluation, separate from package version.
Recoverable manifest identity must survive validation failure so rejected
packages cannot conceal duplicates or appear as missing dependencies. Retained
startup outcomes expose failures while independent packages remain usable.

Reference: [extension host](architecture/extension-host.md).

## Bundled extension packages

Compile bundled workflow packages into the binary instead of installing them
beside it or copying them into user data. Sources stay in the repository, work
identically in development and release without packaging steps, and cannot
drift from the shipped binary.

Bundled packages use the reserved `@knot` scope and the same manifest, API-version,
dependency, and rollback rules as installed packages. The reserved scope makes
bundled identity structural: an installed directory can never claim it. A
rejected installed directory that names a bundled identity is reported as a
directory conflict without rejecting the identity, so a named diagnostic always
means that identity is unavailable. Installed packages may require bundled ones.
A bundled package failure is reported and fails its dependents without stopping
product startup.

Reference: [extension host](architecture/extension-host.md#package-admission-and-startup).

## Bundled project registry

The bundled `@knot/projects` package owns project records: one application-global
storage record holding JS-generated stable IDs, names, roots, and the recent
selection. Native code owns workspaces and roots; the registry asks the
application to open or replace them, so project identity and policy stay in
JavaScript while the application keeps every private bypass closed.

A project is active in a window when its recorded root matches that window's
workspace root, and the registry persists no per-window binding yet.
Multi-window activation is finalized with durable project sessions, and
overlapping or multi-root matching with multi-root workspaces.

The Projects tool starts hidden. Its visibility and dock selection are native
tool-layout state: a command-invoked extension show or hide publishes the
fresh-window default like any other explicit user arrangement.

References: [extension guide](../extension-api.md) and
[workbench lifecycle](architecture/workbench-and-lifecycle.md#automatic-tool-layout-memory).

## Personal configuration

Use one manifest-free config directory with optional pre-init and post-init
modules sharing JavaScript state. Prefer an existing XDG location so users have
one unambiguous source root across platforms.

Config failure is fatal to startup: partially applied personal setup would leave
unpredictable behavior. Installed-package failures remain independent. Product
windows wait for both phases, and a dedicated error window keeps diagnostics
available when config startup fails.

References: [extension host](architecture/extension-host.md) and
[workbench lifecycle](architecture/workbench-and-lifecycle.md).

## Commands and keymaps

Use one semantic namespace and dispatcher across keys, menus, palette, and
scripts. Captured presentation authority preserves intent across focus changes
and awaits without granting ambient access to whichever window is active later.
Explicit buffer access remains useful independently of a presentation.

Bounded awaited command composition supports reuse without a general task graph:
inline same-extension children avoid self-queue deadlock, while ancestry and
one-child rules constrain composition. Outcomes describe execution; ordinary
functions remain preferable when semantic return values are needed.

Resolve keymap precedence in Knot because gpui context depth cannot express
personal-over-extension-over-native priority. Retain owner slots so removal
reveals lower-priority bindings. Atomic per-turn JavaScript batches prevent
partial keymaps and make startup readiness follow foreground state.

Reference: [commands](architecture/commands.md).

## Views and extension UI

Share text and contributions through buffer models; keep interaction per view.
Surface-specific semantic data lets extensions customize behavior without
blocking native painting. A general widget tree or WebView needs a concrete
unmet use case first.

Grapheme-aware horizontal movement and shaped-x vertical movement preserve
visual behavior that byte columns cannot express across combining text, CJK,
and emoji.

References: [semantic UI](architecture/semantic-ui.md) and
[editor views](architecture/workbench-and-lifecycle.md#editor-views).

## Workbench layout and view lifetime

Use a binary split tree with tabbed leaves and stable pane/tab identities.
Application-wide last-view knowledge protects shared documents. One protected
closure flow defers presentation changes until all decisions and saves succeed,
avoiding partial closure on cancellation. Empty-workbench replacement is shell
policy rather than a layout invariant.

Tab relocation preserves editor state and terminal sessions independently of
closure. Exact presentation captures reject replacement and transfer; preparing
a new destination window first preserves the source if creation fails.
Application-wide drag coordination handles macOS delivering pressed drags to the
originating window, including cross-window hit testing and preview placement.

Native tool frames retain presentation identity independently of tab attachments.
Dock groups use the existing split tree and typed tabs so tools share lifecycle,
observation, and command targeting with document and terminal presentations.
Keeping hidden definitions in the workbench preserves native state without
retaining closed documents or rebuilding obsolete document groups. Model and
gpui integration tests cover these ownership and closure boundaries, and the
docking foundation is approved; extension-contributed frames now render through
the shared [semantic UI](architecture/semantic-ui.md) tree and text surfaces.

Automatic tool-layout memory persists one versioned tool-only snapshot in
application-owned SQLite, separate from extension storage: stable namespaced
tool IDs, dock location, grouping and order, sizes, visibility, per-dock
selection, and hidden tools' remembered locations. Tools in document groups
flatten to the normal document group of a fresh window; document splits,
terminal contents, and live handles are never serialized. Fresh windows inherit
the last user-customized arrangement while open windows stay independent, and
only explicit user arrangements publish a new default: moves, resizes, and
show/hide changes, including a command-invoked extension show, hide, or focus.
Restore, activation, listener callbacks, automatic presentation, and document
structure changes never publish. The cache updates synchronously for immediate
inheritance while writes
go off the foreground with debounce; quit flushes a pending write before exit.
Reset Layout returns to product defaults with no profiles or layered preferences.
Fresh windows auto-show stored-visible tools even with no folder open, while a
customized flag preserves the empty first-run window. A tool alone in an
ordinary split pane is remembered as the dock on the side its split faces;
groups shared with documents still flatten so document structure is never
reproduced.

Replacing the native explorer's fixed sidebar with tool frames leaves one shell
focus/command-view resolution path: `CommandView` supplies the focus handle and
key-context kind for every tab surface, so capture, keybinding routing, and close
targets no longer branch per surface. Contributed tree and text frames now render
through the shared `TreeView`/`EditorView` surfaces under that same path. The
native explorer stays a shell-owned tool until a bundled JavaScript extension
replaces it (D053), so its workspace providers and file actions are shaped by the
project registry and multi-root semantics rather than by this plan's internals.

Reference: [workbench and lifecycle](architecture/workbench-and-lifecycle.md).

## Terminal

Use `alacritty_terminal` as a native surface separate from text buffers. Ghostty
VT offered insufficient behavioral advantage to offset separate PTY integration,
pre-1.0 FFI, Zig packaging, integration size, and poor debug parsing performance.
The Alacritty adapter met the required behavior without copying Zed code; Zed
remains an attributed reference for later concrete compatibility gaps.

An application-owned session outlives its presentation. One attachment keeps
resize and input authority unambiguous. Ordered generation-scoped protocol events
preserve emulator replies while repaint wakeups coalesce. Gesture ownership from
press through release prevents modifier changes from leaving reported buttons
pressed. A session-local mouse toggle makes selection and Copy usable inside TUIs.

HTTP(S)-only opening bounds link policy. Explicit OSC 8 identity takes precedence
over detected text, and release-time validation prevents changing output from
opening a stale destination. Physical application-keypad support remains deferred
rather than vendoring gpui solely to recover its missing native key identity.

Reference: [workbench and lifecycle](architecture/workbench-and-lifecycle.md).

## Extension tree content and actions

One contributed registration declares its native content kind, provider or
pushed text, and frame actions. Groups are explicit tree headers rather than a
second list model, so grouping, expansion, and invalidation share one provider
tree. Row and group actions carry a command name plus JSON arguments and reuse
the captured-window command route, keeping native controls and activation
mutually exclusive without a parallel dispatch path. Row status uses a generic
theme severity rather than a feature's change-state taxonomy, and action and
status glyphs stay out of the boundary until an extension-replaceable icon
system exists (D055).

Contributed text frames push a complete read-only value into a private buffer
rendered by the ordinary editor surface. This avoids a second text renderer and
reuses selection, copy, and scrolling, while the buffer stays outside document
identity and persistence. The bridge retains the last pushed text so later
windows present current content. The editor embedding is provisional; a
dedicated read-only presentation is tracked as D054.

Reference: [semantic UI](architecture/semantic-ui.md).

## Extension transient interactions

Picker, text input, and confirmation/message interactions resolve with typed
values under `workbench`, so dismissal is an ordinary result and operational
failure stays a host error. The window is resolved before any focus change from
an explicit `WindowTarget` or the admitted command capture, which keeps an
awaited picker inside a command bound to its originating window.

Pickers receive a complete item list and filter locally, deliberately avoiding
streaming providers and a second scheduler. One extension interaction owns a
window at a time and a second request rejects `OperationInProgress`; the
command palette stays an independent native surface rather than sharing that
slot. Window closure, extension unload, and caller cancellation settle the
interaction as cancellation, and the previous live focus is restored on close.

Confirmations and messages reuse the native gpui prompt. The platform already
tests these answers, restores focus, and covers the contract, so a parallel
rendered dialog would add surface without behavior.

One shared single-line `TextInput` implements gpui's input-handler contract and
backs both transient fields and the palette query, replacing the palette's
key-event string assembly. Typing, IME preedit, and clipboard use gpui's native
path; the field owns only ordinary movement, selection, and pointer handling.

Reference: [semantic UI](architecture/semantic-ui.md).

## Generated text surfaces

Use ordinary buffers and views when generated output benefits from text
interaction. Controllers retain semantic identities instead of parsing displayed
text. Search validated this boundary without adding feature semantics to generic
buffer ownership or a second presentation model.

Reference: [semantic UI](architecture/semantic-ui.md).

## Capability aggregation

Keep aggregation feature-specific and view-owned. Deterministic merging removes
arrival-order dependence; provider-local failure preserves other results. Stable
semantic item identities allow presentation replacement without restarting work
or recovering semantics from labels.

Reference: [semantic UI](architecture/semantic-ui.md).

## URI workspaces and filesystem providers

Normalized URIs and an async byte-oriented provider boundary keep generic
workspace and document code independent of platform paths. Documents remain
independent of workspace containment so Open and Save As can select any resource.
Local roots are trusted application boundaries, not symlink sandboxes.

No-clobber creation, conditional replacement, and atomic local installation
protect existing content. Persistence identity changes only after successful I/O;
saving the captured content state leaves racing edits dirty. Application-wide URI
ownership prevents two open documents from claiming the same destination.

Reference: [documents and persistence](architecture/documents-and-persistence.md).

## Application document and root identity

Document UUIDs separate logical identity from mutable URIs, collection IDs, and
transport handles without promising restart recovery before durable metadata
exists. Window-owned root identities and monotonic generations prevent
clear/reinstall from making old asynchronous directory work current again.

Reference: [documents and persistence](architecture/documents-and-persistence.md).

## Application observation

Atomic registration, initial snapshot, and sequence watermark prevent gaps.
Compare metadata at committed transitions instead of creating a second catalog
or exposing text-edit payloads as application events. Queries derive current
state from its existing owners.

Existing asynchronous FIFO roots let listeners await host work or self-disposal
without blocking commits. A query may see newer state than its event; ordinary
editor observation does not require replay, acknowledgement, or recovery logs.

Reference: [extension host](architecture/extension-host.md).

## Semantic resource operations

Share product operations between native commands and extension APIs, returning
committed semantic data independently of execution outcomes. A status string or
later focus query cannot reliably identify an opened presentation. Root mutations
address windows independently of selected documents; protected closure preserves
native dirty-document decisions. Caller cancellation prevents later mutation but
does not roll back a save already accepted by the shared persistence path.

References: [documents](architecture/documents-and-persistence.md) and
[protected closure](architecture/workbench-and-lifecycle.md#workbench-and-protected-closure).

## Folder selection

Expose folder selection through `workspaces.pickFolder` rather than a general
resource picker or implicit reuse of the Open dialog. A workflow that manages
project roots needs a normalized directory before deciding whether to open or
replace them, and the operation deliberately never commits roots itself.

The native directory prompt occupies the existing per-window transient interaction
slot, so ownership, cancellation, and target resolution match other extension
interactions. The selected path is normalized and stat-checked through registered
providers, so callers receive a usable URI or a typed `ResourceNotFound` /
`ResourceWrongKind` failure instead of revalidating the path themselves.

## Extension record storage

Separate SQLite databases by canonical package identity, with a distinct config
owner, so durable ownership survives reload and installation-path changes.
Application-selected owners keep extensions out of other databases and separate
extension records from application recovery metadata.

Bundled SQLite, serialized reused connections, and synchronous calls on the
existing background executor avoid a dedicated storage service. Rollback
journaling with FULL synchronization provides committed-write acknowledgement.
One JSON record is the atomic replacement unit; extensions own value schemas.
Incompatible or damaged databases fail without replacement.

Drain admitted work before calling gpui quit because its final shutdown hook's
short foreground wait cannot supply this guarantee. The drain runs independently
of its awaiting caller, preserving accepted native work without a final JS
callback.

Reference: [extension host](architecture/extension-host.md#extension-record-storage).

## Unsaved-document recovery store

Protect unsaved text in an application-owned recovery database, independently of
extension health and separate from the extension record stores. An `automatic`
row per document UUID is replaceable protection for a live dirty document; a
`retained` row is an immutable opaque-key checkpoint owned by a suspended session
or another extension owner. Later automatic captures never overwrite retained
rows, and saving retires the automatic row without invalidating a retained one.

Capture copies text on the foreground and commits off it. A trailing-edge
debounce settles about a second after edits stop and fires at least every five
seconds while typing continues. Each fired capture commits with FULL
synchronization before it is accepted, so the advertised crash-loss window is the
cadence itself. Deferring durability to save is rejected: a FULL commit off the
foreground is far cheaper than losing text to avoid it. The quit, close, and
command paths only drain a capture still inside its debounce window.

The database opens lazily off the foreground, and a missing, corrupt, or
incompatible database is preserved and reported rather than replaced. Dirty
documents are observed through their shared model, so multiple views produce one
row and no extension or session metadata is required.

Reference: [documents and
persistence](architecture/documents-and-persistence.md#durable-unsaved-document-recovery).

## Unsaved-document restore and hot exit

Rebuild a recovered document by loading its current resource as revision 0 and
applying the recovered text as one edit with an empty undo history. Dirty state
then comes from the ordinary content comparison, so no revisions, counters, or
history are persisted, and text equal to its baseline restores clean. Restore
never writes a resource. A changed file keeps the capture-time provider version,
so Save reaches the existing Reload / Save As / Cancel conflict prompt instead of
silently clobbering, as VS Code and Zed do; a deleted file becomes a dirty
destination.

A live document is never replaced. When another document owns the URI, or a
retained capture's document is open, the text opens as a separate untitled copy;
for an automatic snapshot one transaction moves the protection to that copy, so
the text is never unprotected and is not listed twice. Automatic restores keep
the document UUID so later captures replace the same row; retained restores take
a fresh one because the automatic role may still protect newer text.

Startup restores unclaimed automatic snapshots before the launch gate opens
windows, so launch requests deduplicate against them deterministically instead
of racing them. Only the newest snapshot per URI is restored; older duplicates,
retained captures, and unreadable baselines remain for File › Recover Unsaved
Documents…, a native picker in the window's transient slot followed by a Restore
/ Discard prompt. That interim listing reuses existing surfaces rather than
adding a recovery UI that project sessions will supersede.

Quit is a hot exit, matching Sublime Text, VS Code, and Zed: once every dirty
document is durably committed, quit skips the Save / Don't Save / Cancel prompts
and the next launch restores them. Without this, the prompt's Don't Save is an
explicit discard and recovery would only help after crashes. A store that fails
to open, write, or drain falls back to the prompts so quit never loses text
silently. Closing a tab or window still prompts because it is a deliberate
removal rather than leaving the application.

Generated documents need no restore outcome: they are never captured and the
schema has no generated state.

Reference: [documents and
persistence](architecture/documents-and-persistence.md#durable-unsaved-document-recovery).
