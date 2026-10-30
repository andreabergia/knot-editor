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

Views read lines from the model instead of keeping a private copy of the text,
which had made every keystroke cost O(document) per view. The line index is
therefore maintained eagerly, and it also records UTF-16 line starts: platform
text input speaks document-wide UTF-16 offsets, and without the index every IME
callback scanned from the start of the document. This keeps the core UTF-8:
text, edits, and positions stay in bytes, and the earlier rejection of a
whole-document UTF-16 table concerned a JavaScript adapter that exceeded the
isolate heap, not one integer per line in Rust. A model-side index would have
repeated the incremental update on each commit path.

Contiguous typing extends the trailing add piece rather than adding one per
keystroke, and the consumed edit log is discarded, so ordinary typing no longer
degrades every buffer operation over a long session. Inserted newlines follow
the buffer's first line terminator rather than a dominant one, which would need
a whole-document scan for a rare mixed-ending case.

Reference: [core and buffers](architecture/core-and-buffers.md).

## Syntax highlighting

Parse natively with tree-sitter over grammars compiled into the binary, and
let extensions contribute language definitions: an LSP language ID, the grammar,
detection patterns, and queries. Running a grammar is a mechanic like shaping:
the tree must follow every commit synchronously and painting never calls
JavaScript, while the definition is data that personal config and packages
replace through the same API the bundled `@knot/languages` package uses.
Grammars come from upstream crates over the `tree-sitter` crate rather than
arborium, whose whole-string highlighter has no tree editing, old-tree reuse,
chunked input, progress callback, or byte-range query. SQL is left out because
its crate pins `cc ~1.2.1` for the whole build; runtime grammar loading is D065.

LSP language IDs and nvim-treesitter's capture names make the pieces a
language client needs already exist: IDs pass through unchanged and semantic
token types map onto the same names. Adopting Neovim's published capture list
lets adapted upstream queries keep their names and stops a missing leaf from
falling back to the wrong parent; Zed, Helix, and Neovim all resolve dotted
names by longest prefix. Decoration names stay outside the capture vocabulary
so a query cannot draw a diagnostic. Queries ship as TypeScript string modules,
normalized to predicates the Rust binding evaluates, so packages stay
JavaScript-only.

Every edit applies to the tree in the commit, then the foreground reparses
within 1 ms and continues in the background from a `TextSnapshot`, painting the
edited previous tree meanwhile. On Apple M1 Pro/macOS release builds, a
3,500-line, 96 KiB Rust file parsed initially in 10.5 ms and reparsed after valid
typing at p50 220 µs, p99 475 µs; a 70,000-line, 1.9 MiB file parsed in 206 ms
and reparsed at p50 5.4 ms, p99 13.4 ms. Typing that leaves an unclosed `(`
forces error recovery over the rest of the file: p50 21 ms and 444 ms. A
60-line highlight query over the visible range cost about 220 µs. The budget
therefore keeps ordinary files inline and moves huge files and broken syntax
off the foreground. Injections are nested layers rather than one combined tree;
a 1,400-line, 21 KiB Markdown file reparsed after typing at p50 115–150 µs,
while a 28,000-line file with 10,000 layers reparsed at p50 6–9 ms, mostly the
Markdown root and editing later layers' trees, and continues in the background.

Snapshots share the original text and sealed 16 KiB add-buffer chunks as
`Arc<str>` and copy the piece list. On a 29 MiB, 932k-line buffer with 9,988
pieces after 5,000 typing bursts, a snapshot cost p50 19 µs, p99 33 µs, and
streaming the whole text 0.39 ms, so a snapshot per budget overrun is cheap.

Queries compile off the foreground at registration: compiling every bundled
query took about 420 ms, C++ alone 130 ms. The grammars grew the release
binary from about 74 MB to 88.5 MB and add about 28 s of parallel C
compilation.

A document's language is buffer state, explicit or detected. An unregistered
explicit ID is kept and paints as plain text, so restoration and personal config
need no ordering against package startup, and the window layout records only
explicit languages, so restores detect again. Set Language Mode is native
because an extension command's palette title is its name.

References: [core and buffers](architecture/core-and-buffers.md#syntax-state)
and [semantic UI](architecture/semantic-ui.md#syntax-highlighting-and-languages).

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

V8 values that are not host-operation arguments keep hand-written decoders and
encoders in the engine's marshaling module: provider results, command outcomes
returned by extension handlers, and events and buffer changes delivered to
listeners. They cross in the other direction, where Rust invokes JavaScript, so
no handler descriptor exists to drive them, and each type has exactly one
decoder rather than a parallel registration. The declaration reader delegates
to the same decoders for ranges, edits, contributions, command arguments, and
destinations, so both directions share one conversion per type.

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
drift from the shipped binary. The build compiles their TypeScript, so emitted
JavaScript is never committed.

Bundled packages use the reserved `@knot` scope and the same manifest, API-version,
dependency, and rollback rules as installed packages. The reserved scope makes
bundled identity structural: an installed directory can never claim it. A
rejected installed directory that names a bundled identity is reported as a
directory conflict without rejecting the identity, so a named diagnostic always
means that identity is unavailable. Installed packages may require bundled ones.
A bundled package failure is reported and fails its dependents without stopping
product startup.

Reference: [extension host](architecture/extension-host.md#package-admission-and-startup).

## Extension authoring and tests

Write bundled packages and examples in full TypeScript, compiled per file without
a bundler, so the runtime keeps loading ordinary captured modules and readable
stack traces need no source maps (D032). Compiling in `build.rs` makes Node a
build requirement; that was preferred over committing generated `dist/`, a Rust
TypeScript compiler dependency, or restricting authors to type-erasable syntax.
Installed packages still take built JavaScript, so TypeScript stays authoring
tooling rather than a runtime.

Test extension policy under `node:test` against an in-memory `knot` host rather
than only through product tests, which need a shell, V8 pool, and SQLite for
every policy case. Resolving the `knot` specifier to the fake keeps extension
code free of dependency injection. Typing the fake against the declarations
makes API drift a check failure. Implementing behavior instead of mocks keeps
flow tests readable, and `not faked` failures keep its surface demand-driven.
The fake is not a second host: product tests remain the authority for anything
crossing the native boundary. Packages create state in `start()` so each test
starts fresh without module reloading.

References: [extension guide](extension-api.md#write-and-test-in-typescript)
and [development](development.md).

## Bundled project registry

The bundled `@knot/projects` package owns project records: one application-global
storage record holding JS-generated stable IDs, names, roots, and the recent
selection. Native code owns workspaces and roots; the registry asks the
application to open or replace them, so project identity and policy stay in
JavaScript while the application keeps every private bypass closed.

A window belongs to a project when its roots equal the project's roots, ignoring
order, and the project's session is active. No two projects share a root set,
while one folder may belong to several projects. The registry persists no
per-window binding: the binding lives in memory and is rederived from root sets
at activation, so a restart can never resurrect a stale association.

Projects follow root edits. When a bound window's roots change to a set no other
project uses, the project adopts it and its other windows follow through the
public `replaceRoots`, so a project stays one root set across its windows without
a separate Add Folder to Project command. Clearing the roots, or choosing
another project's set, unbinds the window instead of merging projects.

The Projects tool starts hidden. Its visibility and dock selection are native
tool-layout state: a command-invoked extension show or hide publishes the
fresh-window default like any other explicit user arrangement.

References: [extension guide](extension-api.md) and
[workbench lifecycle](architecture/workbench-and-lifecycle.md#automatic-tool-layout-memory).

## Project sessions

A project's session is active or suspended, recorded per project in a
`session:<id>` record separate from the registry so live tracking writes stay
small. Requiring an active session for membership keeps the approved
root-derived activation while protecting suspended text: opening a suspended
project's folder by hand never binds the window, so tracking can never overwrite
the record holding its captures. Only a switch carries captures; losing the last
window by closure suspends without them because closure already decided about
dirty documents. Startup waits for `launched`, so natively restored windows
rebind by root set first; it then marks the remaining active records suspended
and never releases anything. A record with an unknown schema is preserved rather
than overwritten and blocks orphan reconciliation, because a newer version's
record may reference any capture.

A project may span several windows and restoring it restores all of them:
switching closes every source window and creates every destination window with
its stored layout, so each window's record is self-contained and no source
window's leftover presentation leaks into another project. Active records
describe membership only: dirty active text is protected by automatic recovery and hot exit, and automatic
restores keep their document UUID, so an active record's document references
survive restart. Captures are released only after the record that drops them is
durable; orphans are reconciled at activation rather than released implicitly.

Switching retains text and durably suspends the source before the native commit,
so a failed, cancelled, or interrupted switch never loses text: the source stays
intact and its suspended record is a valid suspension of a still-active project
that the next tracking write reactivates. An edit racing the commit retries with
fresh captures, up to three attempts. A partial restore commits and reports the
skipped entries together, because the source is already durably suspended.
Package commands and listeners are serial roots, so live tracking never records
the intermediate state of a switch.

References: [extension guide](extension-api.md#project-sessions) and
[workbench lifecycle](architecture/workbench-and-lifecycle.md#session-replacement).

## Session replacement contract

`workbench.replaceWindows` is a generic typed operation over windows, documents,
and retained captures, and knows nothing about projects, so an installed package
can build the same workflow. It runs as a protected operation so it cannot
interleave with closure prompts or another replacement. Staging every read
before one foreground commit means observers see only the committed result and
need no operation tag, and staged documents never enter the document collection
early. The commit revalidates the exact suspended set and each final view's
retained revision in Rust, from revisions recorded at retain, so a racing edit or
tab rejects as `SessionConflict` with nothing changed rather than losing text.

A live document always wins over staged content, which keeps one source of truth
per file: documents open elsewhere and documents restored by hot exit are
presented rather than duplicated, and a retained restore's preserved untitled
copy never arises from a session. Restored dirty documents are durably protected
before resolution so callers can release captures immediately.

Switching closes released windows rather than reusing them: windows are always
created with their layout and geometry, which keeps one creation path for
startup and switching, avoids moving existing windows (D062), and makes a
released window's terminals stop as closing any window does. Terminals come
back in place as fresh shells, and the rejected alternative, clearing a
released window that held a terminal, left a window outside every session.

References: [workbench lifecycle](architecture/workbench-and-lifecycle.md#session-replacement)
and [documents](architecture/documents-and-persistence.md#retained-captures).

## Window layouts and restoration by position

One versioned JSON layout serves native window hot exit and extension sessions,
so both restorers share capture, pruning, and installation, and extensions store
it verbatim without a parallel schema. It holds no handles: every tab kind
records only what recreates it in the same position (a document's identity and
URI, a tool's stable ID, nothing for a terminal), and a future kind joins by
adding one recreation step. A tab is pruned only when its kind cannot be
recreated, and pruning lives in one place, so a stored layout can never produce
an invalid workbench. View state is byte offsets that clamp and snap to
character boundaries, which survive changed files without remembering line
models.

The layout is captured on demand and announced through a debounced
`viewStateChanged` event rather than pushed with every change, so cursor
movement costs one capture per settled second and listeners write only changed
records. Restoration never publishes the tool-only default, which keeps restored
windows from overwriting the arrangement new windows inherit.

References: [workbench lifecycle](architecture/workbench-and-lifecycle.md#window-layouts)
and [extension guide](extension-api.md#project-sessions).

## Native window hot exit and launch precedence

Startup restoration is native hot exit extended to windows: the application,
not the project extension, records every product window and restores them before
launch requests open, so a broken or slow extension never loses the user's
windows, matching how unsaved text is protected. Projects rebind to restored
windows by root set after `launched` instead of reopening them, so there is one
restorer at startup and no duplicate windows. The record is marked consumed
durably before restoration commits, trading the arrangement for a guaranteed
clean next launch if restoration itself crashes.

Precedence is recovered documents, then recorded windows, then each queued
request in its own window. Documents resolve live by identity first, so restored
recovery text is presented in place; requests deduplicate against everything
restored; the last request window takes focus because it is what the user just
asked for, else the previously active window. Recovered documents no window
presents open in the first window, so text is never hidden. A reopen after
launch is a request for a window, not a restore. Missing roots are dropped
rather than failing every window. A launch whose config failed records nothing,
so the next successful launch still restores the last good session.

References: [workbench lifecycle](architecture/workbench-and-lifecycle.md#native-window-hot-exit)
and [documents](architecture/documents-and-persistence.md#durable-unsaved-document-recovery).

## Extension restart

A native Restart Extension command is the minimal escape hatch for a stuck turn
without adopting timeout or quota policy (D019). A terminating unload already
ends a running turn and the shared foreground cleanup already removes every
registration, so restart reuses them, rediscovers the one package, and loads a
fresh lifecycle under the same storage and recovery owner. Dependents keep
running because packages share only the global command namespace. Personal
config stays excluded; restarting Knot is its recovery path. A switch hung before
its native commit leaves the source intact, and restarting the package recovers.

Reference: [extension host](architecture/extension-host.md#restarting-a-package).

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
defaults beneath a last-write-wins stack. Retain owner slots so teardown reveals
earlier bindings. Atomic per-turn JavaScript batches prevent partial keymaps
and make startup readiness follow foreground state.

Keybindings and languages do not rank writers by owner. Configuration is code
that runs in order, as in Emacs and Neovim: packages start after `pre-init`
and `post-init` runs after them, so the latest write wins and personal
overrides belong in `post-init`. VS Code and Zed rank the user's keymap above
extensions only because it is a declarative file with no place in a load
order. View-specific bindings still beat global ones, as Emacs mode maps beat
the global map and more specific setting scopes win. Native bindings stay a
base beneath every write, like Zed's base keymap, so switching a keymap preset
(D086) never overrides extension or personal bindings. Languages need no base:
nothing switches bundled languages at runtime. The accepted cost is that a
package restarted or binding late beats earlier personal writes until Knot
restarts; this is rare, and settings (D070) behave the same way.

Reference: [commands](architecture/commands.md).

## Application settings

Settings work like Emacs variables: one in-memory store that personal config,
packages, and native code all write, where a later write replaces an earlier
one whoever made it. This is the keymap rule above applied to values, and
ranking writers by owner was rejected for the same reason. Nothing is
persisted natively, because config and packages rebuild the store on every
launch; a UI that changes a setting is an extension that keeps its choice in
its own storage and sets it again in `start()`.

A scope is a set of facts that must all match a read's context: a document's
language and a view kind, with a file path to come for project settings
(D089). The match with the most facts wins and ties go to the latest write, so
a scoped value beats an unscoped one however late that is written, as an
Emacs buffer-local value beats `setq-default`. Ranking by fact count keeps one
rule that a `path` fact extends without new precedence.

Definitions follow `defcustom`: a key gets a type, a default, and a
description that a future settings UI lists. Values may precede their
definition because `pre-init` runs before packages start; definition
validates them, and keys never defined stay inert, so typos are silent as in
Emacs. Teardown removes a lifecycle's definitions but not the values, which
belong to nobody, so Restart Extension needs no rewrite. A write of the wrong
type is a host error rather than a `TypeError`, because only the native store
knows the definition. A setting that names a contribution, such as a theme,
holds only the name and falls back when it is unregistered, so values never
depend on package load order.

Detection overrides are claims, not a setting. A map-valued
`files.associations` was built and reverted: a map is one value, so one
writer's `set` replaced every other writer's patterns and teardown could not
remove a package's entries, and a merging write would still leave entries
without owners. `languages.associate` makes each claim lifecycle-owned in the
same order as registrations, so claims compose and disposal reveals the
previous one.

References: [extension host](architecture/extension-host.md#settings),
[editor views](architecture/workbench-and-lifecycle.md#editor-views), and
[semantic UI](architecture/semantic-ui.md#syntax-highlighting-and-languages).

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

A new terminal opened from a focused terminal, or by splitting one, starts in
that terminal's current directory, as iTerm, Ghostty, and Zed's split do:
someone working in a terminal wants the next one where they already are.
Otherwise a new terminal starts in the root of the window's most recently
focused document, as Zed's default does, rather than always in the first root,
which lands in the wrong folder of a multi-root window, or asking with a
picker, as VS Code does, which adds a step to every terminal. Remembering the
last focused document lets a terminal opened from a tool still follow the file
being edited. A window without roots uses the home directory rather
than the active document's folder, as most editors do: such a window is usually
a scratch window, and the result should not depend on which file happens to be
open. Daily use may revisit this. The process's own working directory is never
used, because it is `/` when Knot is launched from Finder.

A restored terminal starts in the directory its shell was last in, read from
the shell process when the layout is captured (`proc_pidinfo` on macOS,
`/proc/<pid>/cwd` on Linux). Querying the process needs no shell setup, unlike
OSC 7, which `alacritty_terminal` does not parse and which needs shell
integration (D039); it reads the shell rather than the foreground job, so a
running editor or build does not change the answer. Where the query is
unavailable, as on Windows, the start directory is recorded, so a chosen
directory survives restarts. A deleted directory falls back to the default rule
rather than its nearest existing parent, a rare case where the root is a fine
landing place. A `cd` starts no capture, so a crash can restore an older
directory; quit and project switches always capture fresh. Moving to
libghostty (D063) replaces the query with the emulator's OSC 7 state (D128).

Reference: [workbench and lifecycle](architecture/workbench-and-lifecycle.md).

## Extension tree content and actions

One contributed registration declares its native content kind, provider or
pushed text, and frame actions. Groups are explicit tree headers rather than a
second list model, so grouping, expansion, and invalidation share one provider
tree. Row and group actions carry a command name plus JSON arguments and reuse
the captured-window command route, keeping native controls and activation
mutually exclusive without a parallel dispatch path. Row status names a theme
color key rather than a feature's change-state taxonomy, and action and
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

## Product window chrome and failure notices

A product window shows only product surfaces. The prototype toolbar duplicated
commands, menus, and keys, and its free-form status string was the only place
some failures appeared. Native failures with no other visible effect now use the
native message prompt, at warning level, that extension messages already use.
Success and progress notices were dropped because their effect is visible.
Extension calls get the failure as their result, so a package decides whether
to tell the user. A failed package startup opens the startup report in each
launch window, because nothing else passively shows that a package failed.
Developer tooling shares a `Developer:` title prefix and stays out of the menus.

Reference: [workbench and lifecycle](architecture/workbench-and-lifecycle.md#product-application-launch).

## Generated text surfaces

Use ordinary buffers and views when generated output benefits from text
interaction. Controllers retain semantic identities instead of parsing displayed
text. A native search probe validated this boundary without adding feature
semantics to generic buffer ownership or a second presentation model.

Documents have no generated state. Cross-file search ships as a bundled
extension and contributed text frames use private buffers, so no product path
created a generated document; removing the state also removed its recovery,
closure, Save, and public `DocumentInfo.kind` special cases. Interactive
generated views return as an extension-facing contract (D027) rather than a
native document kind.

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

## Multi-root workspaces

A window holds an ordered list of roots. Proposed sets are deduplicated after
normalization, keeping the first occurrence, rather than rejected: two spellings
of one folder are the same root, so an error would only make callers normalize
first. Retained URIs keep their root IDs, which makes add, remove, and reorder
plain replacements of the whole list with no separate mutation API.

Nested and overlapping roots are allowed, because a monorepo and one of its
packages are both reasonable roots. Each root is browsed independently: explorer
expansion and selection are keyed by root ID and URI so the two copies of a
folder stay independent, while directory listings are cached by URI. Every root
is a top-level row, even when there is only one, so root actions always have an
obvious target and there is one layout and keyboard model. Folder Open keeps
replacing every root, matching its single-root meaning; Add Folder appends.
Removing a root never closes documents, since documents are independent of
workspace containment.

Reference: [documents and persistence](architecture/documents-and-persistence.md#workspaces-and-resource-trees).

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
unreadable baselines, and retained captures remain for File › Recover Unsaved
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

Reference: [documents and
persistence](architecture/documents-and-persistence.md#durable-unsaved-document-recovery).

## Retained recovery captures

Extensions retain document text through application-stamped ownership: the
owner is the canonical package or personal-config identity bound at admission,
and the capture ID is application-generated. An extension therefore cannot name
another owner, choose a table, or overwrite a capture, and a foreign ID behaves
like a missing one. Owning by identity rather than lifecycle keeps captures
addressable across reload, failure, and restart, which is what a session needs.

Each retain creates a new immutable capture, so an owner may hold several
captures of one document: a suspended session and its replacement can reference
different text until the old record is released. Retain resolves only after its
FULL-synchronized commit, which gives sessions the ordering they need without a
separate barrier call: commit the record after retain, release the old capture
after the record.

Prefer orphans to premature deletion. Nothing removes a retained capture except
its owner's release or the user's explicit discard; a cancelled retain may leave
one behind. Owners reconcile their listing against their records, and the native
listing shows every retained capture with its owner, so orphans stay inspectable.
Startup neither restores nor counts retained captures, since their owners decide
when to restore them. Release and discard are the same deletion, so the public
API exposes only release.

Reference: [documents and
persistence](architecture/documents-and-persistence.md#retained-captures).
