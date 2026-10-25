# Documents, workspaces, and persistence

Part of the [architecture](../architecture.md). [Decisions](../decisions.md)
records rationale; deferred work lives in the [roadmap](../roadmap.md).

## Document ownership

The application-global `DocumentCollection` strongly owns every open document
and model, deduplicating normalized resource URIs. Each document has a UUID
independent of collection IDs and transport handles. Save As and additional
presentations preserve it; closure/reopening allocates another. A document
restored from automatic recovery keeps its UUID across restart, which is the one
restart guarantee: session records may reference it, and other identities are
restart-local. `BufferRegistry` only maps monotonic transport handles to weak
model references; shell selection remains presentation state.

Final document removal explicitly closes the model, invalidates transport access,
and releases contributions and pending changes even if a stale view retains the
entity. The product host removes subscriptions and model observation. Nonfinal
presentation closure and tab transfer keep the shared model open; final-view
coordination belongs to [protected closure](workbench-and-lifecycle.md#workbench-and-protected-closure).

Documents distinguish untitled, destination-associated-but-uncreated, and
persisted state. Untitled cleanliness compares against initial content;
associated-but-uncreated documents remain dirty until saved; persisted documents
compare against their saved content identity. Undo can return to clean content
without making public revisions non-monotonic. Resource identity and provider state never enter `BufferModel` or `core`.

## Opening resources

CLI paths, native Open, and macOS events converge on normalized resource requests.
The shared product operation returns committed document/presentation or workspace
data; native commands and extension handlers independently adapt that result.
Extension file Open constrains resource kind before reading rather than interpreting
a command outcome or looking up focus later.

Admission captures the destination window/pane/exact presentation. Provider work
normalizes and stats asynchronously, then reads and UTF-8-decodes existing files
before changing foreground state. Missing files create dirty destination-associated
documents; native directory Open installs roots and tree. Completion revalidates
caller lifecycle/cancellation, Open generation, native window, and exact
presentation. Later opens, replacement, or transfer reject stale work even if
tab/document IDs are unchanged.

A window reuses its existing tab for an open document or creates an independent
view in the captured pane. Opening from a terminal preserves the terminal.
Untitled creation and presenting a known document use the same destination
admission and return committed document/presentation pairs; neither creates a
headless document lease. Successful operations focus their destination without
resolving application focus again.

## URI providers

`ResourceUri` identifies resources through normalized hierarchical URIs. Providers
own scheme-specific normalization and opaque versions. Only the local provider
converts file URIs to paths; generic workspace, tree, document, and shell state
remains path-independent.

The application registry routes normalization, enumeration, versioned read,
no-clobber create, conditional replace, and stat by scheme. Memory and local
providers implement the same typed results/errors. Local writes flush a temporary
sibling, install atomically, and sync the parent directory before reporting
success. Replacement resolves symlinks first, so it rewrites the link's target
and the link survives; it preserves permissions and rejects version mismatch.
Native filesystem I/O uses a background Tokio runtime.

Product documents use unscoped providers so user-selected resources need not lie
inside workspace roots. Scoped local providers remain trusted application
boundaries, not sandboxes; symlinks may lead outside the lexical root.

## Workspaces and resource trees

Each window retains one `WorkspaceState`, including while empty. It owns ordered
root records and a monotonic generation; currently zero or one root is supported.
Retaining a normalized root preserves its ID. Clear/reinstall allocates another
ID without resetting generation. Invalid replacement preserves existing state.

Folder Open and root replacement address the native window independently of any
selected document. They normalize/stat every proposed directory before one shared
root/tree commit, revalidating caller lifetime, window, Open generation, and root
set generation. Unrelated documents stay open. Results use the committed snapshot.

The public folder picker presents the same native directory prompt one interaction
per window, then normalizes and stats the selected path through providers before
returning a URI. It never commits roots; callers choose whether to open or replace
them. A missing selection rejects `ResourceNotFound` and a file rejects
`ResourceWrongKind`.

`WorkspaceTree` owns cached entries, expansion, selection, loading, and errors.
Enumeration checks captured roots, root generation, and per-directory request
generation before applying results; clearing roots clears the cache. Painting
and input use cached foreground state only. Documents open through the application
provider independently of root containment.

## Saving

Save and Save As require a document presentation. Save captures document/model,
resource, text, content revision, provider version, and persistence generation.
Successful create/conditional replacement records the new provider version and
only the captured content state after revalidating the presentation, native window,
and document. Edits racing I/O therefore remain dirty.

Save As uses the native panel, checks application-wide URI ownership, and changes
resource association only after successful I/O. Overwrite consent does not bypass
provider-version checks. Conflicts offer Reload, Save As, and Cancel; Reload also
rejects edits racing its read. Watching and automatic reload remain deferred.

## Durable unsaved-document recovery

`app::recovery` owns one application `recovery.sqlite3` beside the tool-layout
store. The database opens lazily off the foreground. One versioned `snapshots`
table holds two retention roles keyed by `(kind, capture_key)`: `automatic` is
the replaceable protection for a live dirty document (one row per document
UUID), and `retained` is an immutable opaque-key checkpoint owned by a suspended
session or another extension owner. Retained rows are never overwritten by
later automatic captures.

Every document is observed through its model from creation, so a commit copies
dirty text on the foreground and serializes it off it. Captures
coalesce through a trailing-edge debounce and fire at least every five seconds
while typing continues. A fired capture is written and committed with `FULL`
synchronization before it is accepted, so the crash-loss window is the cadence
itself. A successful persistence transition re-publishes against the new
baseline or retires the automatic row once the document is clean; final-view
discard removes it, and the quit path drains a capture still inside its debounce
window. A missing, corrupt, or incompatible database is preserved and reported,
never replaced, and failures keep pending work and live document state.

Restore reads the snapshot's resource through its registered provider as the
baseline (empty for untitled documents), then applies the recovered text as one
edit with an empty undo history, so dirty state comes from the ordinary content
comparison. Restore never writes a resource. A deleted persisted file becomes a
dirty destination; a changed file keeps the capture-time provider version, so a
later save reaches the existing conflict prompt. A live document is never
replaced: when another document owns the URI, or a retained capture's document is
open, the text opens as a separate untitled copy, and an automatic snapshot's
protection moves to that copy in one transaction. Automatic restores keep the
document UUID; retained restores allocate a fresh one and leave the capture in
place.

Startup restores every automatic snapshot without a live document, newest per
URI, after config and extension startup and before the launch gate opens
windows. Queued launch requests therefore deduplicate against restored
documents. Older duplicates and unreadable baselines remain in the store, and the
first window's status points at File › Recover Unsaved Documents…. Retained
captures are left to their owners and not counted. That native command lists
every leftover automatic snapshot and every retained capture, labelled with its
owner, in the window's transient slot and restores or discards the selection.
None of this depends on the extension host.

Quit does not ask about dirty documents. It captures each one's current text and
drains the store; when every capture commits, the documents stay protected for
the next launch. A failed open, write, or drain makes quit fall back to the
Save / Don't Save / Cancel prompts. Closing a tab or window still prompts.

### Retained captures

Extensions retain captures through the `recovery` namespace. The owner is the
calling lifecycle's canonical identity bound at admission, the same owner that
selects its record database, so it survives reload, restart, and installation
moves; an author supplies only the application-generated capture ID, and a
foreign capture is indistinguishable from a missing one. Retain copies an open
document's text on the foreground, commits a new immutable row off it, and
resolves only after the commit is durable. That is the barrier a session record
commits against: write the record referencing a capture only after retain
resolves, and release the capture it replaced only after the new record is
durable. Each retain allocates a new ID, so an owner may hold several captures of
one document.

Retain also records, for the rest of the session, the capture's owner, document,
and document revision. [Session
replacement](workbench-and-lifecycle.md#session-replacement) validates a final
view's capture against the document's current revision from that record, in
Rust, without reading the store; a capture from an earlier session or an older
revision never satisfies it. Replacement stages a capture and its resource
baseline off the foreground and commits the restored document in its own
foreground turn, then drains automatic recovery for it before resolving.

Retained captures are never removed implicitly: unload, activation failure,
uninstall, and restart keep them, and a cancelled retain may still commit. A
capture its owner no longer references is an orphan. Owners find orphans by
reconciling `recovery.list()` with their own records; users see every retained
capture with its owner in the native listing. Removal happens only through the
owner's release or the user's explicit discard. Restore follows the rules above
and presents the document in the caller's destination; the capture stays in
place until released.

Contracts live in [knot.d.ts](../../src/host/js/knot.d.ts); implementation and
physical schema live in [recovery.rs](../../src/app/recovery.rs).

## Observation

Creation, final removal, persistence, and root/tree commits signal
[application observation](extension-host.md#application-observation). Content edits
use model commits. Persistence can change resource/title/dirty metadata while
preserving document UUID; observation does not depend on author buffer lookup.
