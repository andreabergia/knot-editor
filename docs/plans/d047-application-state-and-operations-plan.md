# D047 — Application state and document operations

Status: ✅ parent checkpoint 2 complete and approved on 2026-10-15.
Parent checkpoint 3 has not started.

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

## Parent checkpoint 2 — Observable application state ✅

1. **Model and targets.** ✅ Add document identity and ordered root records; keep
   window-owned workspace state and one-root enforcement. Extend workspace-tree
   captures for the collection/generation model. Derive application queries from
   existing documents and live product shells/workbenches. Add only the live-window
   registration needed to discover targets; avoid a synchronized catalog replica.
2. **Commit notifications and lifetime.** ✅ Wire document creation/removal,
   persistence/Save As, model content/dirty changes, workspace installation/clear,
   window lifecycle, presentation mutations, and native focus/activation. Include
   new/split/activate/close and tab relocation across panes/windows. Centralize final
   document removal enough to invalidate buffer handles, subscriptions and
   contributions even when a test or stale view still retains the model entity.
   Preserve documents shared by other windows and ordinary empty-window replacement.
3. **Public observation.** ✅ Extend protocol, foreground routing, pool/engine dispatch,
   bootstrap, facade and declarations. Add snapshot queries, atomic observation,
   ordered delivery, and disposal using existing runtime ownership. Share the
   current document model commit source with buffer observation.
4. **Usable fixture and documentation.** ✅ Add a checked JS observer loaded through
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

## Implementation evidence

- ✅ Model foundation: application-allocated document UUIDs survive Save As and URI
  deduplication; reopening allocates a fresh identity. Session-local IDs and buffer
  handles remain separate.
- ✅ Root model: every product shell owns empty/single ordered root records with
  stable retained IDs and monotonic replacement generations. Extra roots reject
  atomically; clearing/reinstalling cannot revive directory captures. Native folder
  Open and workspace-tree captures use the model.
- ✅ Checkpoint 2 implementation, automated acceptance coverage, and manual smoke
  checks accepted; explicit user approval recorded on 2026-10-15.

Model foundation validation (2026-10-15): `cargo test --all-targets` passed
(562 library tests and 5 example tests); `cargo clippy --all-targets -- -D warnings`,
`scripts/check-extension-api.sh`, and `git diff --check` passed. Focused tests cover
identity retention/reopening, normalized retained roots, extra-root rejection,
clear/reinstall generations, and stale directory responses with cached tree state.
This is intermediate validation, not checkpoint acceptance.

- ✅ Final-document invalidation: collection removal closes the model, clears
  contributions and pending buffer changes, rejects transport access immediately,
  and releases host subscriptions/observers. Closing a shared presentation and tab
  transfer preserve the model. Blank-tab replacement also checks other windows
  before removing the replaced document.
- ✅ Added product-host JS coverage retaining a real editor and buffer across
  nonfinal/final close; final reads, edits, subscription admission and contribution
  writes reject `BufferClosed`. Focused bridge coverage asserts subscription and
  handle release; a native replacement regression covers a shared blank document.

Document lifetime validation (2026-10-15): `cargo test --all-targets` passed
(565 library tests and 5 example tests); `cargo clippy --all-targets -- -D warnings`,
`scripts/check-extension-api.sh`, and `git diff --check` passed. Existing protected
closure, cancellation, window sharing and relocation regressions remain green.
Checkpoint 2's observation contracts and review gate are still outstanding.

- ✅ Public metadata queries: `documents.list/get`, `windows.list/get`, and
  `workspaces.get` derive current state from native documents and product shells.
  No catalog replica. Empty startup catalogs, shared document/buffer associations,
  selected/focused presentations, terminals, and diagnostic-window exclusion have
  product-host integration coverage.
- ✅ Discoverable live targets are opaque objects separate from command captures.
  Pane/view targets qualify window-local IDs with workbench and presentation entity
  identity. Runtime and TypeScript coverage reject fabricated and cross-authority
  target use. Public query data is recursively read-only; integer responses reject
  precision loss. Declarations and author guide cover query/identity semantics.

Query/target validation (2026-10-15): `cargo test --all-targets` passed
(566 library tests and 5 example tests); `cargo clippy --all-targets -- -D warnings`,
`scripts/check-extension-api.sh`, and `git diff --check` passed. Checkpoint 2 still
requires commit observation, FIFO delivery/disposal, and the checked observer fixture.

- ✅ Foreground observation: collection commits, native shell transitions, model
  commits, focus/activation and window closure feed metadata comparison. Root
  replacement/clear commits tree state together and preserves generation safety.
  Buffer payloads and metadata observation share one commit drain; author buffer
  lookup is unnecessary. Cached snapshots also reject closed models.
- ✅ Public `workbench.observe`: atomic snapshot/watermark, typed ordered events,
  existing FIFO listener roots, asynchronous host work, self-disposal, failure
  isolation, queued callback suppression, unload and activation rollback cleanup.
- ✅ Checked installed `@example/application-state` observer: zero-window activation,
  later window attachment, edits/undo, shared documents, split/transfer, active
  target, roots/clear/reinstall, closure and disposal run through the product host.
  Author guide, declarations, architecture references and decisions are updated.
- ✅ Completed the checkpoint acceptance audit below, including native/CLI/platform
  Open, failed/stale opens, Save As/save races, reorder and exact live-target tokens.
  Do not begin checkpoint 3 or mark the parent checkpoint complete before approval.

Observation implementation validation (2026-10-15): `cargo test --all-targets`
passed (572 library tests and 5 example tests); `cargo clippy --all-targets -- -D warnings`,
`scripts/check-extension-api.sh`, and `git diff --check` passed. New coverage includes
atomic baseline/watermark comparison, event admission before observe response,
activation-root ordering, awaited listener queries/mutations/self-disposal,
queued-delivery suppression, listener failure isolation, native root retain/clear/
reinstall and rejected replacement, cross-window transfer, active focus, window/final
closure, unload ownership, and failed installed activation cleanup. The observer
fixture ran as an installed package through the real product host. The full
checkpoint acceptance audit and explicit parent review approval remain outstanding.

## Checkpoint 2 acceptance audit — approved

Previous goal turns made concrete progress through model/lifetime, queries/targets,
and observation implementation commits. This audit adds focused product-boundary
regressions in `src/app/product/tests/observation.rs`, without a production replay log.

| Required behavior | Current evidence |
| --- | --- |
| Atomic initial snapshot/watermark; ordered following events without startup gaps | `baseline_watermark_and_ordered_changes_have_no_duplicate_initial_event`; V8 `application_observation_installs_before_queued_delivery_and_self_disposes` queues delivery before the observe response and verifies activation finishes first |
| First open, additional presentation, existing-tab activation, split, reorder/transfer, native focus and closure | `open_entry_points_share_document_identity_and_publish_only_committed_resources`; `reordering_retains_exact_targets_while_replacement_and_transfer_change_them`; installed observer fixture covers split/cross-window transfer and window/final closure |
| Empty workbenches and honest non-document/generated surfaces | `empty_workbench_and_generated_surfaces_keep_their_actual_metadata`; public query integration covers terminal/null buffer associations and diagnostic-window exclusion |
| Native/CLI/platform Open convergence, missing destinations, failed/stale opens | New product observation tests exercise CLI path parsing, platform URL and native picker into one shared document; missing files, invalid UTF-8, provider failure, picker cancellation, superseded reads, same-document view replacement and closed windows |
| Save As retains identity/transport and publishes URI/title/dirty metadata; racing edits remain dirty | `save_as_and_racing_edits_publish_current_metadata_without_changing_identity`; `save_as_rejects_a_replaced_view_even_when_the_document_and_tab_are_unchanged`; existing provider conflict, persistence and stale-save regressions |
| Successful save remains observable before later close cancellation | `completed_save_remains_observable_when_a_later_close_prompt_is_cancelled` asserts clean saved metadata while every tab/window remains open |
| Edits/undo-to-clean; one shared commit source for buffer and metadata listeners | `application_metadata_and_buffer_listeners_share_each_edit_once` runs ordinary JS through the real product host and checks both revision streams and dirty restoration |
| Final versus nonfinal lifetime, retained entities and transfer | `document_removal_invalidates_retained_models_and_releases_buffer_ownership`; public JS `final_document_close_invalidates_js_buffer_even_with_retained_editor`; new `retained_closed_shell_does_not_keep_the_last_live_document_open`; native/JS transfer regressions |
| Empty/single roots, retained IDs, extra-root rejection, clear/reinstall generation and stale directory replies | Workspace and workspace-tree contract tests plus installed observer fixture's root retain/clear/reinstall and rejection assertions |
| Listener await/query/mutation/self-disposal, failure, queued disposal, unload and activation rollback | V8 admission/disposal test; `application_listener_can_query_edit_fail_and_dispose_without_blocking_queued_roots`; installed fixture plus failed observer activation cleanup |
| Opaque identities, command authority separation, immutable metadata, declarations and checked fixture | Public query/context integration, pinned TypeScript contract checks, declaration/author-guide/runtime agreement; actual checked-in package loaded by the product-host example test |

Audit fixes:

- ✅ Bind weak workbench registrations to live native windows. Closing a retained
  shell no longer inflates view counts and prevents final-document invalidation.
- ✅ Native Open retains exact presentation and destination-window identity;
  native Save and invocation-scoped command captures revalidate exact presentation
  identity, focused surface identity and window/workbench lifetime.
  Same-document view replacement and closed-window I/O cannot mutate replacements.
- ✅ Open focuses its actual selected/created editor in the captured window.
  Existing-tab activation now publishes matching selection and actual focus.

Review approved on 2026-10-15 for snapshots, event taxonomy, opaque document/root/live
identity rules and final-document invalidation, following inspection of the checked
[`@example/application-state` fixture](../../examples/extensions/@example/application-state/dist/main.js)
and the published [declarations](../../src/host/js/knot.d.ts). Parent checkpoint 2
is complete. No checkpoint 3 mutations are published; D047 remains in progress.

Final checkpoint 2 automated validation (2026-10-15): `cargo test --all-targets`
passed (583 library tests and 5 example tests); `cargo clippy --all-targets -- -D warnings`,
`scripts/check-extension-api.sh`, `git diff --check`, and added Markdown-link
validation passed. The full product suite includes the checked installed observer
fixture and both public JS and native commit-boundary tests.

Manual product validation (2026-10-15): user reports confirmed shared document
identity across presentations, contiguous event sequences, Save As from `justfile`
to `foot` preserving UUID and revision while becoming clean, and observer disposal
freezing event history while live queries continued updating. The user explicitly
approved checkpoint 2 after these checks. Checkpoint 3 has not started; its review
gate and D047 completion remain outstanding.
