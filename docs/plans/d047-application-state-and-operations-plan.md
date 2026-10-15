# D047 — Application state and document operations

Status: 📝 planned; implementation not started.

Source: checkpoints 2–3 of the
[extension platform and projects plan](extension-platform-and-projects-plan.md)
and the [roadmap](../roadmap.md). Builds on the completed
[D046 API foundation](../archive/plans/d046-public-api-foundation-plan.md).

## Outcome and scope

An ordinary JS extension can inspect open documents and product windows, observe
changes, open files/folders, create untitled documents, present existing documents,
replace workspace roots, and close presentations through native protected closure.
An observing fixture and a JS open command exercise the supported product host.

Keep this proportional to a text editor. Reuse application documents, workbenches,
resource loading, command captures, the extension scheduler, and protected closure.
Add a small foreground observation owner and typed bridge operations, not a second
application model, generic event framework, or transaction engine.

Execute parent checkpoint 2 and stop for review before implementing checkpoint 3.
The numbered tasks within each checkpoint are implementation steps, not additional
approval gates. Make logical commits as work lands; keep this plan current with
✅ marks and validation evidence. Run `cargo fmt` once at the end of Rust work,
before committing. Update architecture references and validated decisions when
behavior lands, rather than documenting this proposal as implemented now.

Relevant current boundaries:
[architecture](../architecture.md), [decisions](../decisions.md),
[documents](../architecture/documents-and-persistence.md),
[workbench/lifecycle](../architecture/workbench-and-lifecycle.md),
[extension host](../architecture/extension-host.md), and
[commands](../architecture/commands.md).
Extend the [declarations](../../src/host/js/knot.d.ts) and
[author guide](../extension-api.md) together with runtime behavior.

## Agreed simplifications

- Observe and capture the initial snapshot in one foreground operation. A
  session-local sequence watermark and ordered notifications are sufficient.
  No public event batches, replay log, acknowledgements, automatic resynchronization,
  observer recovery protocol, or new backpressure policy. A caller needing a fresh
  baseline can dispose and observe again. Reuse scheduler queue/lag diagnostics.
- Workspaces belong to windows and are addressed through live window handles.
  Do not add an independent workspace identity or registry. Empty roots are valid.
- Create untitled documents together with their presentation. No headless-document
  ownership, leases, or extension keep-alive API. Remove a document after its final
  presentation closes, including invalidating its buffer transport handle.
- Use ordered root records and a root-set generation, with zero or one root.
  This is the parent plan's small preparation for D052, not multi-root behavior.
- Queries describe current state. They do not freeze it or grant future validity;
  asynchronous operations retain the existing checks for actual target loss.

## Contract to implement

Finalize spelling alongside the declarations; the responsibilities below are the
intended API, not permission to publish unimplemented methods.

### Queries, identities, and targets

- `documents.list()` / `get(id)` return document metadata: opaque document identity,
  title, kind/state, optional normalized resource URI, dirty flag, revision, and a
  live buffer proxy. `get` returns null for a document that is no longer open.
  Text remains available through the existing buffer API, not application snapshots.
- `windows.list()` / `get(window)` describe live product windows and their panes,
  ordered tabs, selected tabs, and current target. Include non-document tabs as
  surface kinds with no document/buffer, so terminal focus is represented honestly.
  Exclude startup-error and diagnostic windows from the product-window catalog.
- `workspaces.get(window)` returns ordered roots and generation. Each root has an
  opaque stable root ID and normalized URI. No project names or document membership.
- `workbench.observe(listener)` returns the initial application snapshot, its
  watermark, and an awaitable disposer. Observation is application-scoped and valid
  during activation, including before any product window exists.
- Introduce live window/pane/view target types usable by the operations below.
  Qualify window-local identities by their window and preserve exact presentation
  checks. An object referring to a closed/replaced/transferred target cannot select
  a replacement. Reuse existing identities/captures where possible.
- Keep D046 command targets invocation-scoped. Discoverable live targets are a
  separate way to address current product state; do not quietly broaden the
  authority or lifetime of a retained command target.
- Add a persistent-capable opaque document identity distinct from session-local
  document IDs and buffer handles. Allocate it in the application; retain it across
  Save As and additional presentations. No persistence service, URI-derived identity,
  or promise of identity reuse after closure/restart in D047. D048 persists metadata;
  D050 defines recovery resolution. Generated documents remain explicitly generated.

Snapshots contain metadata and presentation associations, not split-tree geometry,
cursor/selection/scroll state, terminal internals, or durable session records.
There is no cross-restart layout contract here.

### Observation

Use a small discriminated event union:

| Event | Meaning and payload |
| --- | --- |
| Document opened | A document enters the application collection; document metadata |
| Document changed | Current metadata with revision/dirty state; covers content, save, title and resource association changes |
| Document closed | The document leaves the collection; its identity |
| Window opened / closed | Product window enters/leaves the catalog; snapshot or identity |
| Presentations changed | Current pane/tab associations and selections for the affected window |
| Workspace changed | Window and its current roots/generation |
| Active target changed | Current active product window and focused target, including null |

Every event has a sequence number. Document metadata events do not duplicate buffer
edit payloads. Detect actual relevant changes rather than forwarding every repaint.
Reuse the existing model commit path; do not consume its pending edits twice or
depend on an extension having first requested a buffer handle.

Publish after the owning foreground transition commits. Document creation happens
once; another presentation changes window state; selecting an existing tab changes
selection/active-target state. Save As preserves document identity and changes
metadata. Folder Open changes roots. Failed/cancelled operations emit no success
event; already committed changes, such as a completed save before a later close
cancellation, remain observable.

Capture subscription registration, snapshot, and watermark together on the
foreground. Subsequent events follow that watermark. Install the JS listener before
deliveries can enter it; the observing activation/command root finishes before
queued listener roots run. Events are notifications of committed changes, not
immutable historical snapshots of the whole application. A later query can see
newer state. Do not promise one event per user gesture or a public transaction ID.

Listeners use the existing asynchronous FIFO scheduler roots. Operations complete
after their foreground commit without awaiting listeners. Listener exceptions are
reported and isolated. Disposal/unload prevents queued author callbacks and removes
native ownership using D046 cleanup semantics; self-disposal and awaiting another
host request must work. Do not introduce listener retries or a separate executor.

### Operations and completion

| Operation | Behavior |
| --- | --- |
| `documents.open(uri, options)` | Open a file or missing file destination through provider normalization/read/decode and existing deduplication; return document and presentation data |
| `documents.createUntitled(options)` | Create an ordinary empty untitled document and present it in the captured pane |
| `documents.present(id, options)` | Present a live shared document, reusing an existing tab in the target window as ordinary Open does |
| `workspaces.openFolder(uri, options)` | Validate an existing directory and replace the captured window's roots |
| `workspaces.replaceRoots(roots, options)` | Validate zero or one root, then commit the replacement together; empty roots clear the tree |
| `workbench.close(view)` | Close the exact tab presentation through existing protected closure; keep documents presented elsewhere live |

Window/pane destinations accept explicit live targets. Where a destination is
optional, capture the originating command window or the active product window at
native request admission, including its selected pane/presentation as needed.
For example, `documents.open(uri, { target })` renders through the ordinary native
document editor tab in that destination. It selects an existing tab for the
document in the target window or creates a document view in the captured pane.
Its result identifies both the document and the actual presentation; JS owns no
rendering surface. Make this destination explicit in the published signature.
Never resolve focus again after I/O. Activation before windows exist gets a
structured missing-context failure; it does not queue work for a future window.
Listeners/providers can pass explicit targets without command context.

Keep ordinary Open semantics: a missing file becomes a dirty destination-associated
document; a missing folder fails. File-only operations reject directories and
folder-only operations reject files. Native generic Open continues dispatching by
resource kind. Opening from a terminal preserves it and adds/selects a document tab.
Use existing blank-tab replacement policy only where the native product uses it;
do not add replace/preview/background/split options without a concrete need.

Root IDs survive retained normalized roots in a replacement; newly introduced roots
get fresh IDs. Keep the generation monotonic for the window lifetime, including
clear/reinstall, and preserve directory-request generation checks. Reject more than
one root. Labels, overlaps, duplicates, reordering and multi-root mutation policy
remain D052. Root changes never close unrelated open documents.

Semantic operations return typed data rather than command execution outcomes.
Adapt shared product results to the two callers; do not extract values from status
strings or invoke a UI command and query afterward to guess its result. Map failures
to typed host error codes, reusing existing categories where suitable. Distinguish
missing/wrong-kind resources, invalid UTF-8, provider failure, unavailable context,
stale targets, and cancellation. A close dismissed with Cancel returns a cancelled
outcome. A successful close returns only after protected closure commits.
Cancellation after a successful mutation cannot undo it. Reuse existing command
cancellation plumbing; do not add a general cancellation framework.

## Parent checkpoint 2 — Observable application state ⬜

1. **Model and targets.** Add document identity and ordered root records; keep
   window-owned workspace state and one-root enforcement. Extend workspace-tree
   captures for the collection/generation model. Derive application queries from
   existing documents and live product shells/workbenches. Add only the live-window
   registration needed to discover targets; avoid a synchronized catalog replica.
2. **Commit notifications and lifetime.** Wire document creation/removal,
   persistence/Save As, model content/dirty changes, workspace installation/clear,
   window lifecycle, presentation mutations, and native focus/activation. Include
   new/split/activate/close and tab relocation across panes/windows. Centralize final
   document removal enough to invalidate buffer handles, subscriptions and
   contributions even when a test or stale view still retains the model entity.
   Preserve documents shared by other windows and ordinary empty-window replacement.
3. **Public observation.** Extend protocol, foreground routing, pool/engine dispatch,
   bootstrap, facade and declarations. Add snapshot queries, atomic observation,
   ordered delivery, and disposal using existing runtime ownership. Share the
   current document model commit source with buffer observation.
4. **Usable fixture and documentation.** Add a checked JS observer loaded through
   the real product host. Demonstrate activation with zero windows, later window
   attachment, and document/presentation updates. Update identity/context guidance
   and affected architecture references; record validated rationale at completion.

Primary code: `src/app/documents.rs`, `workspace.rs`, `workspace_tree.rs`,
`model.rs`, `product.rs`, `product/relocate.rs`, `workbench.rs`,
`extension_buffers.rs`, `extension_host.rs`, and the existing `src/host/` protocol,
engine/pool and JS facade. A focused application-observation module is reasonable;
unrelated module restructuring is excluded.

**Automated validation:**

- Initial snapshot plus subsequent ordered events, including a change immediately
  around observation admission; no startup gap or duplicated initial changes.
- First open versus another window/pane presentation versus existing-tab activation;
  split, reorder/transfer, focus change, window closure, and empty-workbench creation.
- Native/CLI/platform open convergence; missing destinations and failed/stale opens;
  Save As identity/URI changes; edits, undo to clean, save and edits racing a save.
- Final-view removal closes transport access and releases subscriptions/contributions;
  nonfinal closure and transfer keep the shared document/buffer usable.
- Empty/single roots, retained root IDs, rejection of extra roots, clear/reinstall
  generation safety, and stale directory responses.
- A listener awaits a query/mutation and its own disposal; listener failure, queued
  delivery after disposal, unload, and failed activation use existing isolation.

Use focused model tests and gpui product-host integration tests for cross-boundary
paths. Assert meaningful event order/state, not incidental notification counts.

**Review gate:** inspect the observer fixture and approve snapshots, event taxonomy,
identity/root model, live targets, and final-document invalidation. Stop here before
adding programmatic mutations. Mark the parent checkpoint complete only on approval.

## Parent checkpoint 3 — Programmatic resources and documents ⬜

1. **Shared semantic completion.** Refactor only the product entry points needed
   to return typed open/present/root/close results. Keep provider work off the
   foreground; keep target capture, generation validation and commit with the
   existing owners. Native and JS callers use the same implementation.
2. **Open and present.** Implement file/folder opening, untitled creation, and
   presenting a live document. Admit explicit destinations and optional-window
   capture; return metadata and the actual selected/created presentation. Validate
   after asynchronous work before mutating the captured destination.
3. **Roots and close.** Expose validated root replacement/clear and exact
   presentation closure through current protected closure. No unconditional close,
   direct document removal API, all-window document-close policy, or new window
   management API. Continue publishing through checkpoint 2 commit boundaries.
4. **Public integration.** Complete declarations, structured error conversion,
   checked examples and author guide. Add a JS open command that uses its captured
   destination and consumes semantic results. Update affected subsystem references
   and validated decisions.

**Automated validation:**

- Native/JS parity for file/folder opens, missing file destinations, invalid UTF-8,
  provider errors, wrong resource kinds, and application-wide URI deduplication.
- Explicit targets from activation/listeners, missing active window, focus changes
  while awaiting, replaced/transferred views, closed panes/windows during I/O,
  and cancellation before commit. Assert no retargeting or leaked new documents.
- Present one document across windows, reuse its existing tab, and open from a
  terminal without replacing it. Create untitled with normal clean/dirty behavior.
- Root replacement/clear leaves documents open; failed validation preserves roots
  and the tree. Existing stale-request checks still reject prior generations.
- Save / Don't Save / Cancel through JS closure, shared dirty documents, save
  failure/racing edits, and concurrent close rejection. Successful final close
  invalidates its buffer; cancelled close preserves its presentation/content.
- Operation completion does not await event listeners; a listener can itself open
  or close without scheduler deadlock. Preserve existing command outcome behavior.

**Review gate:** use the JS open command and observing fixture in a real product
build. Manually check two-window targeting, file/folder Open, untitled creation,
and dirty-close cancellation. No UI automation is required. Approve semantic
results, target rules and failure behavior before marking parent checkpoint 3 done.

## Validation and completion

At each implemented parent checkpoint run `cargo test --all-targets`,
`cargo clippy --all-targets -- -D warnings`, `scripts/check-extension-api.sh`, and
`git diff --check`. Add focused tests as described above; reuse existing coverage
where it already proves the behavior. Record actual commands/results and manual
review evidence here. Planning-only changes need document/link validation only.

D047 finishes when both parent review gates are approved, the fixture runs through
the product host, and declarations/docs match implemented behavior. Update parent
progress and roadmap links as appropriate; archive this subsidiary plan only after
completion. Do not mark the broader extension/project initiative complete.

Deferred: durable metadata/storage (D048), contributed UI and provider instance
context (D049), content recovery (D050), project policy/switching (D051), multi-root
behavior (D052), layout restoration (D005), scheduling policy (D019), persistent
undo, filesystem watching, new resource providers, and terminal restoration.
