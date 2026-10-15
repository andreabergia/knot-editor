# Documents, workspaces, and persistence

Part of the [architecture](../architecture.md). Design rationale is recorded in
[decisions](../decisions.md); deferred work lives in [deferred.md](../deferred.md).

## Document ownership

`BufferRegistry` assigns monotonic transport handles and holds weak model
references. Its active-buffer entry is the prototype command context.
An application-global `DocumentCollection` assigns document identities,
strongly owns every open document and its model, and deduplicates normalized
resource URIs across the application. Each document also has an application-allocated
UUID independent of its session-local collection ID and buffer transport handle.
That opaque identity survives Save As and additional presentations; closure and
reopening allocate a new identity, with no restart reuse guarantee. Shell selection is presentation state
and remains outside the collection; `BufferRegistry` remains only a
transport-handle registry. Final document removal closes the model explicitly,
invalidates transport access even if a stale view retains the entity, and releases
anchored contributions and pending buffer changes. The product host then removes
buffer subscription ownership and its model observer. Nonfinal presentation closure
and tab transfer leave the shared model open.

A document explicitly records whether it is untitled,
destination-associated-but-uncreated, persisted, or generated. Untitled dirty
state compares the model's current content identity with the identity recorded
at its initial clean revision; a destination-associated document is dirty until
persistence succeeds; a persisted document compares against the content
identity at its last successfully persisted revision; and generated documents
are never persistable. This lets undo return to a clean content state while
public model revisions remain monotonic. URI and provider state never enter
`BufferModel` or `core`.

## Opening resources

CLI paths, native Open selections, and macOS open events become the same
normalized `file://` open request at the platform boundary. The product Open operation returns committed document/presentation or workspace
data independently of command outcomes; native command dispatch adapts that typed
completion. Resource failures retain their native variants until that adapter.
The extension host uses the same operation with a file-only resource constraint,
converts failures to structured host codes, and captures document metadata plus
its exact presentation at commit. Window/pane/view destinations are validated on
admission; asynchronous completion revalidates the captured native presentation
and caller lifecycle/cancellation without looking up focus again.
A product open
normalizes and stats the resource asynchronously. Existing files are read and
UTF-8-decoded before foreground state changes, missing files create a dirty
destination-associated document, and directories install the window's optional
workspace and tree. Later opens and lost or replaced captured presentations reject stale
completions, even when replacement retains the same tab/document IDs. Open retains
the destination window and focuses the selected native presentation there after
commit, without looking up application focus again. Normalized file URIs deduplicate through the application document
collection; a window reuses an existing tab when it already presents the
document and otherwise creates an independent view in the captured pane.

## URI workspaces and persistence

`ResourceUri` is the application boundary for persisted resources. It parses
absolute hierarchical URIs and provides stable equality, scheme access, and
segment-aware containment. Scheme-specific providers own normalization. Only
the local-folder provider converts between `file://` URIs and platform paths;
generic workspace, tree, buffer, and shell state contains no `Path`.

The application owns a registry that routes each operation by URI scheme.
Providers implement one asynchronous byte-oriented contract: normalize,
enumerate immediate children, versioned read, no-clobber create, conditional
replace, and stat. Versions are provider-owned opaque values. The memory and
local-folder providers implement the same Knot-owned results and structured
errors. Local creates and replacements write and flush a temporary sibling,
then install it atomically; replacements preserve permissions and reject a
version mismatch. The local provider dispatches native I/O through a Tokio
runtime and converts paths only at that boundary. Product use is unscoped so
user-selected documents do not inherit workspace containment. A scoped local
provider remains available for trusted workspace consumers and fixtures; its
lexical root is an application boundary rather than a sandbox and filesystem
operations follow symlinks.

```text
Application-owned provider registry
        scheme -> provider
                |
       WorkspaceState
       roots + generation
          /          \
         v            v
 WorkspaceTree   resource open/save
 cached UI       captured revision
                         |
                         v
                    Document
                         |
                         v
                    BufferModel
```

A window-local `WorkspaceState` lives for the entire window lifetime, including
when its root set is empty. It owns ordered root records and a monotonic root-set
generation; zero or one root is supported. Retaining a normalized root retains
its opaque root ID. Clearing and reinstalling allocates a fresh root ID without
resetting the generation. Replacements reject extra roots without changing state.
Workspace-tree requests capture the roots and generation and revalidate both
before applying foreground results; clearing also clears cached tree state. Document opens
use the application provider directly and are deliberately independent of the
workspace root.
`WorkspaceTree` additionally versions each directory request. Enumeration is
sorted in application code with directories before files and deterministic
name and URI ordering. Painting and input use only its cached entries,
loading, selection, expansion, and error state.

Opening a file awaits normalization, stat, versioned byte read, and UTF-8
decoding before creating or selecting any model. A later open or captured-tab
replacement rejects the stale result. Open accepts document or terminal surfaces
and revalidates the captured surface identity before presenting results. From a
terminal it opens a document tab in the captured pane and preserves the terminal.
Save and Save As still require a document target. Documents retain the observed provider
version with their persisted revision. Save captures document, model, resource,
text, revision, version, and a persistence generation. A successful create or
conditional replacement commits the new version and only the captured model
revision while the captured command presentation, native window and document state remain live, so
edits racing the write remain dirty. Save As uses the native save panel, checks
application-wide resource ownership, and retargets the document only after I/O
succeeds. Conflicts offer native Reload, Save As, and Cancel choices; Reload
also rejects edits racing its asynchronous read.

Native Save As provides overwrite consent, but replacement still checks the
observed provider version so a later external change becomes a conflict.
Live filesystem watching, binary buffers, and encoding detection remain deferred.

Creation, final removal and successful persistence signal application metadata
observation on the foreground. Persistence changes title/resource/dirty metadata
without changing the document UUID. Content edits use the existing model commit
notifications; observation does not require an author to request a buffer first.
Window root replacement/clear commits roots and generation with tree state, and
signals through the product shell. Rejected extra roots preserve the prior state.
