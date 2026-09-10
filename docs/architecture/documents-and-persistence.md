# Documents, workspaces, and persistence

Part of the [architecture](../architecture.md). Design rationale is recorded in
[decisions](../decisions.md); deferred work lives in [deferred.md](../deferred.md).

## Document ownership

`BufferRegistry` assigns monotonic transport handles and holds weak model
references. Its active-buffer entry is the prototype command context.
An application-global `DocumentCollection` assigns document identities,
strongly owns every open document and its model, and deduplicates normalized
resource URIs across the application. Shell selection is presentation state
and remains outside the collection; `BufferRegistry` remains only a
transport-handle registry.

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
normalized `file://` open request at the platform boundary. A product open
normalizes and stats the resource asynchronously. Existing files are read and
UTF-8-decoded before foreground state changes, missing files create a dirty
destination-associated document, and directories install the window's optional
workspace and tree. Later opens and destroyed captured tabs reject stale
completions. Normalized file URIs deduplicate through the application document
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
       root + generation
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

An optional window-local `WorkspaceState` owns one normalized root and a
monotonic generation. Workspace-tree requests are checked against that root
and revalidate the capture before applying foreground results. Document opens
use the application provider directly and are deliberately independent of the
workspace root.
`WorkspaceTree` additionally versions each directory request. Enumeration is
sorted in application code with directories before files and deterministic
name and URI ordering. Painting and input use only its cached entries,
loading, selection, expansion, and error state.

Opening a file awaits normalization, stat, versioned byte read, and UTF-8
decoding before creating or selecting any model. A later open or captured-tab
replacement rejects the stale result. Documents retain the observed provider
version with their persisted revision. Save captures document, model, resource,
text, revision, version, and a persistence generation. A successful create or
conditional replacement commits the new version and only the captured model
revision while the captured command tab and document state remain live, so
edits racing the write remain dirty. Save As uses the native save panel, checks
application-wide resource ownership, and retargets the document only after I/O
succeeds. Conflicts offer native Reload, Save As, and Cancel choices; Reload
also rejects edits racing its asynchronous read.

Native Save As provides overwrite consent, but replacement still checks the
observed provider version so a later external change becomes a conflict.
Live filesystem watching, binary buffers, and encoding detection remain deferred.

## Fixture boundaries

App startup and benchmark fixture setup may use manifest-relative paths and
synchronous `std::fs`. These are explicit fixture paths, not resource APIs.
The writable local fixture is isolated under the ignored `target` directory.
