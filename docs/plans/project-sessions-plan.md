# Project sessions and safe switching

Status: ⬜ Not started. Execution plan for checkpoint 8 of the
[extension platform and projects plan](extension-platform-and-projects-plan.md).
One checkpoint with one review gate.

Work identifiers: D051, D050. The parent plan owns initiative scope and ordering;
this plan owns execution detail. [Roadmap](../roadmap.md) owns priority.

## Outcome

Selecting a project in `@knot/projects` switches to it safely. The current
project's windows are suspended with their unsaved and untitled documents
durably retained, and the destination project's windows come back with their
documents in a simple tab arrangement. A project may span several windows, and
restoring a project restores all of them. A failed, cancelled, or interrupted
switch never loses text and leaves the source presentation intact. Removing a
project says what unsaved content it discards. While a project is active, its
session follows the user's tabs live, so a hot-exit quit leaves an accurate
record behind.

A native **Restart Extension…** command stops and reloads any package, so a
stuck turn no longer requires quitting Knot (health review P1-3).

The platform side is equally important. The native transition is a generic,
typed operation over windows, documents, and retained captures. It knows nothing
about projects, and an ordinary installed extension could build the same
workflow.

## Design and boundaries

### Membership and session state

A project's session is either **active** or **suspended**, and that state is
recorded in its session record.

- A window belongs to a project when the window's root matches the project's
  root *and* the project's session is active. This keeps the root-derived
  activation [already approved in checkpoint 6](../decisions.md#bundled-project-registry)
  and adds no persisted window binding. The registry rejects a second project
  with an existing project's root.
- A suspended session becomes active only when the user selects that project.
  If the user manually opens a suspended project's folder, the window is not
  bound and the suspended record is never overwritten. This rule is what keeps
  the retained captures of a suspended record safe.
- An active project becomes suspended when it loses its last window, whether by
  a switch, a window close, or Open Folder elsewhere. A suspension caused by a
  switch carries captures. One caused by closure carries none, because closure
  already prompted for dirty documents.
- At startup, `@knot/projects` marks every active record without a bound window
  as suspended, keeping its entries. This writes state only and never releases
  anything.

### Session record

Each project has one extension storage key, `session:<projectId>`, separate from
the `projects` registry record so that tracking writes stay small:

```text
{ schema: 1, state: "active" | "suspended",
  windows: [ { entries: [ Entry ], selected: number | null } ] }
Entry = { uri?: string, document?: DocumentId, capture?: CaptureId }
```

- A persisted file is stored as `{ uri }`. A dirty file, or a dirty destination
  whose file is missing, stores `{ uri, document, capture? }`. An untitled
  document stores `{ document, capture? }`. Clean, empty untitled documents are
  skipped.
- Only suspended records carry `capture`. Active records describe membership
  only: a dirty active document is protected by automatic recovery and hot exit.
- `document` is meaningful across restart because automatic restores keep their
  document UUID. Checkpoint 8 promotes that from an implementation fact to a
  documented guarantee in `knot.d.ts` and the documents reference: *a document
  restored from automatic recovery keeps its identity*. Other identities stay
  restart-local.
- **Capture lifetime.** After a durable session write, release the captures the
  previous version referenced and the new one does not. Nothing else releases
  captures implicitly; orphans are reconciled through `recovery.list()` at
  activation. This follows the
  [retained capture](../decisions.md#retained-recovery-captures) ordering.

### Native transition: `workbench.replaceWindows`

One new host operation replaces a set of windows' document presentations in a
single foreground commit. The shape below is a sketch; implementation finalizes
the names in `knot.d.ts`.

```ts
replaceWindows(options: {
  /** Every document presented in the reused and released windows. */
  suspend: readonly { document: DocumentId; capture: CaptureId | null }[];
  /** Destination windows in order: reuse an existing window, or create one. */
  windows: readonly { target?: WindowTarget; roots: readonly string[];
                      entries: readonly SessionEntry[]; selected?: number | null }[];
  /** Source windows leaving the session. */
  release?: readonly WindowTarget[];
  /** Index of the destination window that receives focus. */
  focus?: number;
}): Promise<{ windows: readonly { window: WindowTarget;
                                  entries: readonly EntryOutcome[] }[] }>;
```

The operation runs in three phases.

1. **Admission.** It is admitted as a protected operation, so it cannot
   interleave with closure prompts or with another replacement; a concurrent
   request rejects with `OperationInProgress`. Window targets are captured at
   admission.
2. **Staging (off the foreground).** It validates roots, reads destination
   resources, and loads destination captures without creating live documents or
   presentations. No observation event fires during staging.
3. **Revalidation and commit (one foreground turn).**
   - **Revalidate.** Every target window must still be live, and the set of
     documents presented in reused or released windows must equal `suspend`
     exactly. Each document whose final live view is being removed, and that is
     dirty or a non-empty untitled document, must name a capture this session
     retained at the document's *current* revision. The application records each
     capture's revision when `recovery.retain` runs, so revision validation stays
     in Rust. Any mismatch rejects with a typed conflict, and nothing changes.
   - **Commit.** Create any new windows first, using the existing "prepare the
     destination window, discard it on failure" pattern from tab transfer. Then
     remove the source document tabs without prompts, because their text is
     retained. Final-view documents are removed through the normal document
     removal path, which retires their automatic rows. Install the roots, present
     the staged documents in each window's normal document group, select tabs,
     and focus.
   - **Protect before resolving.** Restored dirty documents get a durable
     automatic capture before the operation resolves, so callers can release the
     retained captures immediately.
   - Observers see only the committed result, so events need no operation tag.

**Entry resolution** keeps one source of truth per file. A session refers to
application documents; it never creates a second copy of an open one. Each entry
reports one typed outcome, resolved in this order:

1. If `document` is live, or another live document owns `uri`, present that
   shared model and ignore any capture. The live document is current: it covers
   documents also open elsewhere and documents restored by hot exit. The capture
   is released with the record that replaces it. Session restore never produces
   checkpoint 7's preserved untitled copy.
2. If there is a capture, restore it with the existing outcomes: untitled,
   unchanged, missing, or changed.
3. If there is only a `uri`, open it. A missing clean file reports `missing` and
   is skipped; it does not become a dirty destination.
4. A failure while staging one entry (provider, decoding, or recovery
   unavailable) reports `failed` and skips that entry. The rest of the switch
   still commits. This is the reviewed **partial-restore result**: the source is
   already durably suspended, so committing loses nothing.

**Released windows** close unless they hold a terminal tab. A window with a
terminal is instead cleared: its document tabs are removed, its roots are set to
empty, and it stays open with its terminals and tools. A switch never stops a
terminal. Terminal and tool tabs in reused windows stay where they are; only
document tabs are replaced, and document splits collapse into the normal
document group.

**`windows.focus(target)`** is a small addition that activates a window. When
the selected project is already active, `@knot/projects` focuses that project's
first window instead of switching.

### JavaScript orchestration in `@knot/projects`

Switches are serialized through one promise chain. To switch to B from the
command's window W:

1. If B is active, focus its first window and stop.
2. Determine the source.
   - If W belongs to active project A, the source is all of A's windows, with W
     first.
   - Otherwise, W is reused only if it holds nothing beyond a clean, empty
     untitled document. If it holds more, B opens in new windows and W is left
     untouched.
3. Retain every dirty and non-empty untitled document in the source windows
   whose final view would be removed. Then write A's suspended record with those
   captures and wait until it is durable.
4. Call `replaceWindows`. Source windows are paired with B's windows in order:
   W gets B's first window, extra B windows are created, and surplus source
   windows are released. B with no recorded session gets one window with B's root
   and no documents.
5. If step 4 raced an edit, go back to step 3, up to three attempts, then report.
   For any other failure, report it; the source is unchanged, and A's record is a
   valid suspension of a still-active project. The next tracking write marks the
   record active again, without captures.
6. On success, write B's active record, reconcile and release B's superseded
   captures, release A's superseded captures, update the registry selection, and
   report any `missing` or `failed` entries in one message.

**Live tracking.** Observation events only *trigger* tracking. A debounced task
recomputes each active project's membership from `windows.list()` and
`documents.list()`, then writes the records that changed. It never writes a
suspended record. It defers while a switch is in progress, so intermediate states
cannot be saved; after the commit, it records the final state.

- Closing a window that is not the project's last removes that window from the
  session.
- Closing the last window suspends the project and keeps its last membership,
  so reselecting the project brings its documents back.

**Removal.** Removing an active project deletes its records, and its windows
stay as they are. Removing a suspended project whose record holds captures adds
"N unsaved documents in its suspended session will be discarded" to the
confirmation. The captures are released only after the registry and session
deletes commit.

### Hot-exit interaction

Quit is unchanged. Active records already describe each window's documents. On
the next launch, hot exit restores dirty and untitled documents with their
UUIDs, and startup marks the projects suspended. Selecting a project presents the
live restored models through the `document` entries, so no text is duplicated or
lost. Until checkpoint 10 restores projects at startup, those documents also
remain as loose tabs in the first window, and because that window is not empty,
the project opens in new windows. Implementation verifies that quit publishes no
`windowClosed` events to listeners, so tracking cannot record a quit as closure.

### Extension restart

The native `extensions.restart` command ("Restart Extension…") lists the admitted
bundled and installed packages, including failed ones, in the window's transient
picker. Restarting a package:

1. performs a terminating `pool.unload` and the existing lifecycle teardown;
2. rediscovers that one package (bundled sources, or the installed directory
   re-read from disk);
3. revalidates its manifest and API version;
4. loads a fresh lifecycle and activates it.

The outcome replaces that package's entry in the retained startup report and is
shown in a message. Dependents keep running, since they share only the global
command namespace. Personal config is excluded, because restarting Knot is its
recovery path. `ExtensionWatchdog`, its `dead_code` allowances, and any per-turn
timeout remain D019. If a stuck turn hangs a switch before the native commit, the
source stays intact, and restarting `@knot/projects` recovers.

## Exclusions and deferred decisions

- Window geometry, document split structure, view state, and startup restoration
  of projects belong to checkpoint 10 (D005). Multiple roots belong to
  checkpoint 9 (D052). The record's per-window shape is chosen so neither
  requires a migration beyond adding fields.
- No terminal state is persisted, and terminal tabs are never part of a session.
- No per-turn watchdog, extension disable, or personal-config restart.
- Moving hot-exit-restored tabs out of the first window waits for checkpoint 10's
  startup restoration.

## Checkpoint 8 — Safe JS project switching and document sessions ⬜

Deliverables:

- ⬜ Record capture revisions at `recovery.retain` and expose a revalidation
  query to the replacement path. Document identity stability for automatic
  restores in `knot.d.ts` and the documents reference.
- ⬜ `workbench.replaceWindows` with admission, off-foreground staging, a single
  revalidated foreground commit, window creation, released-window close or clear,
  entry outcomes, and protection of restored documents before resolution. Add
  `windows.focus`.
- ⬜ `@knot/projects`: session records, active/suspended state, the switch
  flow with racing-edit retry, live tracking, startup suspension, root-uniqueness
  check, removal with retained-content confirmation, and partial-restore
  reporting.
- ⬜ `extensions.restart` native command with single-package rediscovery and
  report update.
- ⬜ Docs: [workbench lifecycle](../architecture/workbench-and-lifecycle.md)
  (session replacement and release policy),
  [documents and persistence](../architecture/documents-and-persistence.md)
  (identity guarantee, capture revisions),
  [extension host](../architecture/extension-host.md) (restart),
  [extension guide](../extension-api.md) (sessions workflow). Add
  [decisions](../decisions.md) entries for multi-window projects, the
  replacement contract, and restart once approved. Update the parent plan and the
  health-review table (P1-3).

**Validation:**

- **Native replacement (product tests).** Covers these cases:
  - A→B→A with dirty persisted, dirty destination, and untitled documents;
  - a document shared with an unbound window, and a file reopened under a new
    identity: the live model is presented, its capture ignored, and no copy
    created;
  - two windows per project, in both directions, with surplus, created, and
    released windows;
  - a released window with a terminal is cleared, and its session keeps running;
  - an edit racing between retain and commit, a stale or closed target, an
    unexpected new tab, and a concurrent replacement or closure, all of which
    leave the source untouched;
  - caller cancellation and unload before commit;
  - missing and changed destination files and a failed staged entry (partial
    result commits);
  - the recovery store unavailable during retain, so nothing is removed;
  - restored documents durably protected when the operation resolves;
  - no events during staging, and one committed set of events.
- **Interruption.** Simulate a crash after the source record is durable and
  before the commit. On relaunch, hot exit plus the suspended record restore
  every text exactly once.
- **Extension contract.** Add `replaceWindows`/`windows.focus` to
  `scripts/extension-api/contract.test.ts`. Add bundled `@knot/projects` product
  tests for:
  - the switch flow and retry;
  - tracking not overwriting a suspended record;
  - a manual Open Folder of a suspended project's root;
  - closing the last window, which suspends without captures;
  - startup suspension;
  - removal with and without captures (released only after commit);
  - capture reconciliation;
  - listeners not saving intermediate state;
  - quit producing no tracking write.
- **Restart.** Test a looping command terminated by Restart Extension, its
  registrations and views replaced, and an installed package re-read from disk.
  Also test that a failed restart is reported while dependents keep running, and
  that a switch whose JS hangs before the commit leaves the source intact and
  recovers after a restart.

**Review gate:** dogfood switching between two multi-window projects with unsaved
and untitled documents, quit and relaunch, project removal, and Restart
Extension on a stuck package. Approve the partial-restore reporting, the
released-window terminal treatment, and the lifecycle semantics before
checkpoint 9.

**Implementation context:**

- **Host operations** go in `src/app/extension_host/handlers.rs`, next to
  `presentDocument`/`closePresentation`/`recoveryRetain`; mirror their
  `#[host_operation]` adapters and error enums. Semantic product operations go in
  `src/app/product/operations.rs` (`capture_window`, destination admission,
  `OperationCaller::is_live`). Update the JS surface in `src/host/js/knot.js` and
  `knot.d.ts` together, plus `HostErrorCode` if a conflict code is added.
- **Protected admission** reuses `begin_protected_operation` and the closure
  coordinator in `src/app/product/closure.rs`. `collect_product_views` already
  counts final views across windows; `commit_protected_close` shows the
  prompt-free removal and the terminal stop ordering to avoid for cleared
  windows.
- **Window creation** mirrors `relocate_tab_to_new_window_with`
  (`src/app/product/relocate.rs`) and `open_tab_transfer_window`
  (`src/app/product/launch.rs`). Root installation and generations live in
  `src/app/workspace.rs`, and normal-group presentation goes through the shared
  open path in `src/app/product/open.rs`.
- **Recovery:** `retain`, `restore_retained`, and `restore_owned` are in
  `src/app/recovery.rs`. Add the in-memory capture→revision map beside the
  foreground retain bookkeeping, and an immediate automatic-capture drain for
  restored documents (reuse the quit and close drain).
- **Observation** diffing is in `src/app/application_observation.rs`. Staged
  documents must stay out of `ApplicationDocuments` until commit.
- **Restart:** single-package load and rollback are in
  `src/app/extension_host.rs` (startup load near `execute_with_progress`, and
  `remove_lifecycle`). Discovery and validation are in
  `src/app/extension_package.rs`, `extension_load.rs`, and `bundled.rs`. Declare
  the command like `file.recover` in `src/app/product/commands.rs`, with the
  picker through `src/app/transient.rs`, as in `product/recovery_listing.rs`.
- **The projects package** is `extensions/@knot/projects/dist/main.js`, checked
  JS with JSDoc types. Product tests go in `src/app/product/tests/projects.rs` and
  `tests/recovery.rs`, sharing fixtures from `src/app/product/tests.rs`.
