# Documents, workspaces, and persistence

Part of the [architecture](../architecture.md). [Decisions](../decisions.md)
records rationale; deferred work lives in the [roadmap](../roadmap.md).

## Document ownership

The application-global `DocumentCollection` strongly owns every open document
and model, deduplicating normalized resource URIs. Each document has a UUID
independent of collection IDs and transport handles. Save As and additional
presentations preserve it; closure/reopening allocates another, with no restart
reuse guarantee. `BufferRegistry` only maps monotonic transport handles to weak
model references; shell selection remains presentation state.

Final document removal explicitly closes the model, invalidates transport access,
and releases contributions and pending changes even if a stale view retains the
entity. The product host removes subscriptions and model observation. Nonfinal
presentation closure and tab transfer keep the shared model open; final-view
coordination belongs to [protected closure](workbench-and-lifecycle.md#workbench-and-protected-closure).

Documents distinguish untitled, destination-associated-but-uncreated, persisted,
and generated state. Untitled cleanliness compares against initial content;
associated-but-uncreated documents remain dirty until saved; persisted documents
compare against their saved content identity. Undo can return to clean content
without making public revisions non-monotonic. Generated documents cannot be
persisted. Resource identity and provider state never enter `BufferModel` or `core`.

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
sibling and install atomically; replacement preserves permissions and rejects
version mismatch. Native filesystem I/O uses a background Tokio runtime.

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

## Observation

Creation, final removal, persistence, and root/tree commits signal
[application observation](extension-host.md#application-observation). Content edits
use model commits. Persistence can change resource/title/dirty metadata while
preserving document UUID; observation does not depend on author buffer lookup.
