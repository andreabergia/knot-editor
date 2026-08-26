# Step 13: URI and Filesystem Provider Plan

This experiment decides whether workspaces and persisted buffers can use one
URI-based filesystem boundary across an in-memory hierarchy and a local folder,
without leaking `Path` or `file://` assumptions into generic application state.

## Validated constraints

- ✅ Use a parsed, normalized `ResourceUri` backed by the `url` crate rather
  than strings or `PathBuf` at application boundaries.
- ✅ Select providers through a shell-owned registry by URI scheme. A workspace
  retains its normalized root URI; it does not own a concrete provider.
- ✅ Implement both an in-memory provider under a non-`file://` scheme and a
  local-folder provider under `file://`.
- ✅ Make normalization, enumeration, read, write, and stat asynchronous at the
  provider boundary. The memory provider may complete immediately behind the
  same interface.
- ✅ Keep provider content byte-oriented. Opening a text buffer performs UTF-8
  decoding above the provider; binary buffers remain out of scope.
- ✅ Let resource-backed open-buffer entries own their URI and persisted
  revision. `BufferModel` remains independent of URIs, persistence, and
  filesystems; generated buffers remain resource-less.
- ✅ Present provider enumeration through a minimal native workspace tree. Do
  not repurpose the extension-owned `TreeView` or fabricate extension provider
  identity.
- ✅ Treat a workspace root as an application boundary, not a security sandbox.
  Reject lexical `..` escape, but allow the local filesystem to follow symlinks
  and document that they may resolve outside the selected folder.
- ✅ Limit the prototype to UTF-8 files and trusted local folders.

## Prototype boundary

```text
                  shell-owned provider registry
                     scheme -> provider
                            |
         +------------------+------------------+
         |                                     |
 mem:// workspace                         file:// workspace
         |                                     |
 memory provider                         local-folder provider
         +------------------+------------------+
                            |
                 async resource operations
                            |
                 workspace root + URI checks
                    /                 \
                   v                   v
       native workspace tree     resource-backed
                                  open-buffer entry
                                          |
                                          v
                                     BufferModel
```

`ResourceUri` owns generic parsing, absolute-URI validation, and stable equality.
Provider-specific normalization owns scheme semantics: the memory provider
normalizes hierarchical virtual paths, while the local provider is solely
responsible for conversion between `file://` URIs and platform paths. No other
application component converts a resource URI to a `Path`.

The registry resolves the provider for each operation. The workspace validates
that the normalized target belongs to its normalized root before dispatch. URI
scheme and authority comparisons are exact after normalization; containment is
by path segments rather than string prefix.

## Filesystem contract

The smallest provider contract contains:

- normalize one absolute resource URI;
- enumerate the immediate children of a directory;
- read complete file bytes;
- replace complete file bytes;
- stat a resource as file, directory, or missing, with only metadata needed by
  the fixture.

Operations return Knot-owned results and structured errors. Operation errors
distinguish at least invalid URI, outside workspace, not found, wrong resource
kind, invalid UTF-8 at buffer open, and I/O failure. Platform paths and native
I/O errors remain implementation details of the local provider.

Enumeration returns normalized child URIs, names, and kinds. Ordering is an
application policy: the workspace tree sorts directories before files and then
names deterministically, so provider iteration order is not observable.

## Resource-backed buffers

An `OpenBufferEntry` may carry resource metadata containing its normalized URI
and the revision last successfully read or written. A generated buffer has no
resource metadata and cannot be saved through this path.

Opening a resource:

1. captures the workspace and target URI;
2. resolves and awaits the provider read away from the gpui foreground;
3. decodes UTF-8;
4. revalidates the workspace before creating and registering the model; and
5. records revision zero as persisted.

Saving captures the model text, revision, workspace, and URI before awaiting a
complete replacement write. On success, it marks that captured revision as
persisted only if the same open entry and resource identity remain live. Edits
made while the write is pending therefore remain dirty. The prototype does not
attempt atomic writes, conflict detection, or save coordination across two open
entries for the same URI.

Dirty state is derived from `model.revision() != persisted_revision`; it is not
stored in `BufferModel`. URI deduplication and reopening an already open
resource are deliberately deferred.

## Execution

### 1. Add resource identities and the provider boundary

- ✅ Add the `url` dependency and an application-owned `ResourceUri` wrapper
  with absolute parsing, stable display, equality, scheme access, and
  segment-aware descendant checks.
- ✅ Define provider-neutral entry, stat, and error types plus an object-safe
  asynchronous filesystem provider interface.
- ✅ Add a shell-owned provider registry with unique scheme registration,
  lookup, and normalization dispatch.
- ✅ Keep `ResourceUri` and filesystem types out of `core` and the extension
  protocol until a concrete extension API requires them.

### 2. Implement two providers

- ✅ Implement a deterministic in-memory hierarchy rooted at
  `mem://workspace/`, including directories, complete reads and writes, stat,
  and immediate-child enumeration.
- ✅ Implement a local-folder provider for `file://` URIs using asynchronous
  filesystem operations and keep every URI-to-path conversion inside it.
- ✅ Normalize separators, dot segments, trailing directory slashes, and
  percent encoding consistently enough for stable identity and containment.
- ✅ Reject unsupported URI shapes, non-file operations, wrong resource kinds,
  and lexical attempts to leave the workspace root.
- ✅ Exercise local symlinks as trusted filesystem behavior and document that
  they are not constrained as a security boundary.

### 3. Introduce URI-rooted workspace state and enumeration

- ✅ Add application-owned workspace state containing one normalized root URI
  and the generation needed to reject stale asynchronous results.
- ✅ Add a minimal native workspace tree with cached foreground state for
  expansion, selection, loading, and errors.
- ✅ Populate the tree only through provider `stat` and enumeration results;
  rendering and input must never perform filesystem work synchronously.
- ✅ Sort directories before files and names deterministically in application
  code rather than depending on provider order.
- ✅ Replace or clearly separate the prototype's fixture-only local load path
  from the workspace flow without changing the extension-owned outline tree.

### 4. Open and save resource-backed buffers

- ✅ Extend open-buffer entries with optional resource metadata: normalized URI
  and persisted revision.
- ✅ Open a selected workspace file asynchronously, decode it as UTF-8, create
  a normal editable `BufferModel`, register it, and select its editor view.
- ✅ Save an edited resource-backed buffer through its provider and advance the
  persisted revision only for the exact revision successfully written.
- ✅ Surface loading, invalid UTF-8, provider, and save failures without
  replacing the active buffer or blocking the foreground thread.
- ✅ Keep generated search buffers resource-less and preserve their existing
  ownership and read-only behavior.

### 5. Add observable fixtures for both implementations

- ✅ Seed a nested `mem://workspace/` hierarchy and open it through the normal
  workspace path.
- ✅ Open a repository-local fixture folder through the same path using the
  local-folder provider; do not depend on the process working directory.
- ✅ Enumerate, expand, and open files from both workspace trees through the
  same application controller and presentation.
- ✅ Edit and save one file through each provider, then read it through the
  provider to verify the new bytes. Keep local fixture writes isolated from
  checked-in source files.
- ✅ Make provider kind, normalized URI, loading/error state, and buffer dirty
  state visible enough to inspect in the running prototype.

### 6. Audit assumptions and validate the decision

- ✅ Audit application workspace, open-buffer, title, and persistence paths for
  `Path`, `PathBuf`, `std::fs`, current-directory, and `file://` assumptions;
  remove them or document why they are fixture, benchmark, or extension-module
  concerns outside this boundary.
- ✅ Add focused tests for URI normalization and containment, registry routing,
  provider parity, deterministic enumeration, async stale-result rejection,
  UTF-8 rejection, dirty revision handling, and edits racing a save.
- ✅ Use the running fixture to verify both trees, open/edit/save flows, errors,
  and foreground responsiveness rather than adding exhaustive UI tests.
- ✅ Update `architecture.md` with URI, provider-registry, workspace,
  resource-backed-buffer, and asynchronous open/save ownership if the
  experiment validates them.
- ✅ Record the validated choices and remaining liabilities in `decisions.md`
  and mark roadmap step 13 complete with its checkpoint result.
- ✅ Update this plan with ✅ markers, deliberately skipped work, and the final
  result.
- ✅ Run focused tests, run `cargo fmt` once at the end of Rust work, then run
  the full test suite.

## Result

The experiment validated the URI and provider boundary across both the
in-memory and local-folder implementations. The same workspace tree and
open/edit/save flow operate on both schemes without leaking local paths into
resource identity, buffer persistence, or provider-neutral application state.
Async results are guarded by workspace, tree, buffer, and revision identities.

No planned execution work was deliberately skipped. The exclusions below
remain deferred by design. Remaining `Path`, `std::fs`, and `file://` uses in
the application are confined to local fixture bootstrap, renderer fixture
loading, the local provider itself, terminal current-directory state, or V8
module identifiers; they do not define workspace or buffer persistence.

Verification completed with focused URI, provider, workspace, open-buffer, and
end-to-end resource-flow tests, followed by `cargo fmt`, `cargo check --lib`,
and the full `cargo test` suite (240 library tests passed; one doc test ignored).

## Explicit exclusions

- Filesystem watching and external-change reload.
- Rename, delete, copy, recursive enumeration, and workspace search.
- Multiple roots, nested provider mounts, provider replacement, and URI aliases.
- Binary buffers, encoding detection, streaming reads, and partial writes.
- Atomic-save strategy, backup files, autosave, save-as, and conflict handling.
- Permissions UI, authentication, quotas, and untrusted-provider sandboxing.
- Symlink confinement beneath the selected local root.
- Extension-facing filesystem registration or access.
- Deduplicating multiple opens of one URI and coordinating their writes.
