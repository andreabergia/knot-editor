# Durable unsaved-document recovery

Status: ⬜ Proposed. Execution plan for checkpoint 7 of the
[extension platform and projects plan](extension-platform-and-projects-plan.md).
Checkpoints 7a–7c are executed one per implementation request and each stops at
its review gate.

Work identifiers: D050. Parent plan owns initiative scope and ordering; this plan
owns execution detail. [Roadmap](../roadmap.md) owns priority.

## Outcome

Unsaved work survives a crash or a restart. On the next launch Knot silently
restores dirty documents that were not deliberately discarded, as ordinary dirty
documents with an empty undo history and with the on-disk file untouched. Nothing
is written to disk until the user explicitly saves. Recovery is independent of
extension health: the Rust application protects and restores content even when
the bundled project extension fails, is unloaded, or has no session metadata.

The platform side is the other half: the same recovery records back the durable
capture references that checkpoint 8's project sessions retain and release, so an
ordinary JavaScript extension never selects application tables or forges an owner.

## Design and boundaries

Recovery belongs to the application document/persistence layer. `core` stays
storage-independent. A new `app::recovery` module owns the store, capture
bookkeeping, and restore primitives; it does not depend on the extension host.
This follows the existing decision that [extension record storage](../decisions.md#extension-record-storage)
and application recovery metadata stay in separate owners, and reuses the
[automatic tool-layout memory](../architecture/workbench-and-lifecycle.md#automatic-tool-layout-memory)
shape: an application-owned SQLite database written off the foreground, with a
versioned schema, coalesced writes, a quit flush, and non-fatal failure
diagnostics.

**Store.** One application-owned `recovery.sqlite3` beside tool layout and
extension storage (`data_local_dir`). One serialized connection, rollback
journaling with `FULL` synchronization, one versioned `snapshots` table. The
database is opened lazily on the background executor; construction does no I/O.
A missing, corrupt, or incompatible database is preserved and reported, never
replaced, and never blocks editing.

**Record.** A snapshot carries the document UUID, title, optional normalized
resource URI, document state kind, provider version bytes, recovered text, a
per-document generation, and a capture timestamp. The UUID is the stable identity
that survives restart and Save As from checkpoint 2; live handles and collection
IDs do not. Provider version bytes are opaque and persist as bytes.

**Two retention roles.** `kind = automatic` is the replaceable protection for a
live unsaved document: one row per document UUID, replaced atomically per
capture. `kind = retained` is an immutable, opaque-ID checkpoint retained by a
suspended session or another extension owner; retained rows are never overwritten
by later automatic captures. Saving retires the automatic row and cannot
invalidate a retained row. Prefer holding an orphan to deleting work prematurely;
cleanup is explicit and inspectable.

**Dirty-baseline reconstruction.** A recovered document is rebuilt by loading the
current on-disk content as revision 0 and applying the recovered text as one
edit. Dirty state then derives from the existing content-state comparison, and
the restored document needs no persisted revisions, counters, or history.
Recovered documents never reuse session-local revision numbers and are never
considered clean because a counter was reset. Recovered untitled documents are
explicitly dirty: they carry a recovered marker rather than comparing against
initial content. Persistent undo stays excluded (D012).

**Missing and changed resources.** The stored provider version is the
saved-baseline identity. Restore stats the resource: a missing resource becomes a
dirty destination-associated document, and a changed resource restores the
recovered text dirty on top of the current on-disk content while keeping the
capture-time provider version as the restored document's persisted version. The
existing save path then compares that version with the current file and, on a
mismatch, offers its normal Reload / Save As / Cancel conflict handling. That
matches VS Code, which records the file mtime in the backup and restores it so an
external change is not silently clobbered, and Zed, which prompts
Overwrite / Discard Edits / Cancel when a buffer has unsaved edits and the file
changed on disk. Nothing is overwritten or substituted silently.

**URI ownership is the only conflict.** Untitled documents have no URI and cannot
conflict. A conflict exists only when a recovery record targets a URI that a live
document already owns: restoring an older retained capture into a session where
the document is open with newer edits, or two stored records for one URI where an
older orphan coexists with a newer live document. Editors with one model per
resource never create the second case. Knot's automatic restore normally keeps at
most one automatic record per URI live because it preserves the recovered
document's UUID and relies on ordinary URI deduplication; a restore that fails or
is skipped can leave an older duplicate, which newest-first ordering plus dedup
handles by leaving the older record in the store rather than merging or deleting
it. The full ownership contract therefore matters mainly for checkpoint 8's
session restore, which can present an older retained capture while another window
already holds newer content for the same URI. The live document is never
overwritten; the recovered text is preserved separately (default: a separate
untitled document) and the restore reports a typed outcome.

Restore returns committed semantic data (the restored document and presentation,
or a typed reason such as missing destination, changed resource, URI already
owned, or skipped generated document) rather than a status string.

**Generated documents.** Generated documents are always clean, cannot be
persisted, and are never backed up as ordinary editable documents. They are
recreated by their owning feature or skipped with a diagnostic.

**Ownership.** Rust owns snapshot bytes, retention bookkeeping, generation and
revision validation, and orphan policy. JavaScript owns which capture its session
records reference. The extension-facing capture API stamps the owner from the
calling lifecycle identity and accepts only an author-supplied opaque key, so an
extension cannot choose a database, table, or another owner.

**Batching.** Text is copied on the foreground at capture; serialization, SQLite
work, connection locking, and the durable commit run off the foreground. No
foreground disk wait and no final JavaScript callback is required.

## Cadence and durability

- Capture around 1 second after edits settle (trailing-edge debounce), and at
  least every 5 seconds while typing continues. Each capture writes and commits
  its `FULL`-synchronized transaction as it fires, so the advertised crash-loss
  window is the cadence itself, about 5 seconds.
- A fired capture is durable before it is considered accepted: there is no window
  in which the store has taken the text but the bytes are not yet committed.
  `retain` resolves only after its capture is durably committed, which is the
  capture barrier checkpoint 8 commits a session against.
- The quit, close-transition, and command flush only drain a capture still inside
  its debounce window. Deferring durability to save fsyncs is not a trade worth
  making: a `FULL` commit on an SSD costs a fraction of a millisecond to a few
  milliseconds, off the foreground, and losing user text to avoid that is worse.
- Save retires the automatic row; a final-view Don't Save discards it as an
  explicit user decision. Quit and ordinary close prompts keep their existing
  Save / Don't Save / Cancel semantics; broader quit behavior is finalized in
  checkpoint 8 now that recovery exists.
- A crash loses at most the edits since the last durable capture; validation
  demonstrates recovery across an interruption at the commit boundary.

## Startup and recovery UI

Matching Zed and VS Code hot-exit behavior:

- On launch Knot silently restores every unclaimed automatic snapshot as a dirty
  document. No recovery dialog is shown, and undo history starts empty. Restored
  dirty documents are the visible result.
- A CLI/platform open request is honored in addition to restoration, never
  instead of it, so no launch path silently strands unsaved work. Deferring
  restoration because a later session or explicit request will restore the same
  content is a checkpoint 8/10 concern, once sessions can claim records.
- A native File command lists retained and orphaned snapshots with title, URI,
  dirty state, and capture time, and offers Restore and Discard per entry. This is
  the extension-independent path the review gate exercises, the fallback for
  anything not restored automatically (retained captures and records that fail to
  load), and it keeps retention and discard behavior visible. When unrecovered
  records remain after startup, a status message points at the command. The
  command is a deliberate interim fallback, not the long-term recovery surface:
  project sessions in checkpoint 8 and layout restoration in checkpoint 10
  provide the named, discoverable restore users actually see, so this plan does
  not build a second recovery UI.

## Relationship to later checkpoints

Checkpoint 8 persists per-project sessions and window bindings, orchestrates
generic capture/prepare/replace operations, and claims snapshots so unclaimed
ones stay orphaned. Checkpoint 10 owns exact layout and view-state restoration.
Neither redefines the record schema or the retention API established here.

## Exclusions

Persistent undo and history (D012), live filesystem watching and automatic
reload (D008), terminal restoration (D043), session layout and window geometry
(checkpoints 8 and 10), multi-root behavior (checkpoint 9), and any general
recovery UI beyond the listing command.

## Checkpoints

### 7a. Recovery store and automatic protection ⬜

- Implement `app::recovery`: versioned SQLite store, snapshot record types,
  automatic and retained kinds, background writes with debounce and a durable
  commit per capture, quit flush, and non-fatal diagnostics that preserve a
  damaged or incompatible database.
- Publish dirty, non-generated document snapshots on model commits and on
  persistence transitions; dedup one row per document UUID across multiple views.
  Save and Save As update or retire the automatic row; final-view discard removes
  it. Capture copies text on the foreground and writes off it.
- **Validation:** store reopen and schema-version mismatch; write coalescing and
  identical-capture skip; every fired capture is committed durably before the
  next cadence tick, with no accepted-but-uncommitted window; interruption at the
  commit boundary leaves either the old or the new durable row; clean and
  generated documents excluded; multiple views produce one row; Save, Save As, and
  Don't Save lifecycle; disk and file failures keep live state and report without
  blocking editing.
- **Review gate:** approve the record schema, the two retention roles, the capture
  cadence, the acknowledged durability point, and the crash-loss guarantee.

### 7b. Restore, conflicts, discard, and native recovery UI ⬜

- Implement restore: read the baseline through the registered provider (or empty
  for untitled/destination records), create the document, and apply recovered text
  as one edit so dirty state and an empty undo history fall out of existing
  behavior. Preserve URI deduplication with ordinary opening.
- Produce typed outcomes for missing destination, externally changed resource,
  URI already owned by a live document, skipped generated document, and
  unavailable retained capture; preserve both content versions and never write to
  disk.
- Implement explicit discard, silent startup restoration of unclaimed automatic
  snapshots, and the native File listing command with Restore and Discard.
- **Validation:** reopen after a simulated crash; dirty and untitled documents;
  missing and edited files; Save As; an externally changed or deleted resource
  restores over the current content and writes nothing, and a later save hits the
  existing conflict path; a URI already owned by a
  live document; duplicate records for one URI at startup; multiple views sharing
  one restored document; a CLI/platform launch still restores and lists pending
  records; disk failure; extension failure with no session metadata; native
  recovery with the project extension absent.
- **Review gate:** approve exact restore, conflict, retention, and discard
  semantics, and demonstrate native recovery with the bundled project extension
  absent.

### 7c. Capture barrier, retention API, and cleanup ⬜

- Expose opaque retained capture IDs and the retain/release primitives with
  lifecycle-stamped ownership and an acknowledged durable barrier; specify that a
  session record is committed only after its capture is durable and that old
  captures are released only after the new record is durable.
- Define orphan retention and inspectable cleanup: retained and orphaned captures
  survive restart and are removed only by explicit release, explicit discard, or a
  future inspectable cleanup, with counts surfaced in diagnostics or the listing.
- Add the `recovery` namespace to [knot.d.ts](../../src/host/js/knot.d.ts) and the
  [extension guide](../extension-api.md) for list, retain, release, restore, and
  discard; validate owner isolation and non-forgeable ownership with an installed
  fixture package and the authoring check.
- **Validation:** owner isolation and forged-owner rejection; retain before a
  hypothetical session commit; release only after a durable update; orphan
  survival across an extension failure and restart; no durable dangling
  references; concurrent capture generations; unload and cancellation paths.
- **Review gate:** approve the retention/release API, owner-stamping rules, and
  orphan policy, and confirm readiness for checkpoint 8's session orchestration.

## Open questions for review

- Exact retained-capture key shape and whether an owner may retain several
  captures of one document simultaneously; finalized at 7c.
- Whether the native listing uses the existing transient picker surface or a
  dedicated list surface; product review decides at 7b.
- Final advertised crash-loss wording for the UI and documentation; approved at
  the 7a review gate.
