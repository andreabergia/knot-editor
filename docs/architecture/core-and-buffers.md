# Core and buffer models

Part of the [architecture](../architecture.md). Design rationale is recorded in
[decisions](../decisions.md); deferred work lives in [deferred.md](../deferred.md).

## Core editor model

`src/core` contains:

- `TextBuffer`: UTF-8 text, stable positions, line lookup, and the primitive
  edit log, implemented as a stable-ID piece table.
- `AnchoredRangeStore`: stable range geometry without feature, source, or
  presentation metadata.
- `EditTransaction`: one reversible group of primitive edits, not an undo
  history manager.

Anchored ranges consume the buffer edit stream:

```text
TextBuffer mutation
        |
        v
BufferEdit log
        |
        v
AnchoredRangeStore::stabilize
```

## Application buffer ownership

gpui's foreground thread exclusively owns editor state.

Each `BufferModel` owns:

- one `TextBuffer`;
- an application-level editable or read-only access policy;
- one shared `AnchoredRangeStore`;
- one session-local linear undo and redo history;
- source-owned semantic editor contributions;
- open/closed lifecycle and public revision;
- at most one immutable UTF-16 snapshot cache entry.

Presentation metadata stays in `app` and refers to core range IDs. A model
commit advances the public revision once, invalidates the snapshot cache, and
notifies every observing view. All text mutation paths enforce the model's
access policy; read-only models still permit snapshots, view anchors,
contributions, and closure. `core::TextBuffer` remains unconditionally mutable.

## Stable positions and edit behavior

- Text offsets and ranges are half-open UTF-8 byte ranges. Presentation layers
  handle grapheme-aware behavior. Protocol field names identify byte offsets
  explicitly.
- A `Position` identifies a piece and offset. Splits preserve the left piece ID
  and allocate a right piece ID; edit-log records repair affected stale
  positions. Unrelated positions require no work.
- The line-start index is intentionally replaceable. Its linear suffix update
  dominates large-buffer edit cost, but the prototype has not demonstrated a
  need to replace it.
- Inserted backing storage and the edit log are append-only during the
  prototype. Compaction and reclamation are deferred.

## Anchored ranges

- `AnchoredRangeStore` owns stable range geometry only. Each endpoint combines
  a `Position` with before/after boundary stickiness.
- Stabilization consumes one authoritative buffer edit stream. It repairs only
  affected endpoints; untouched ranges retain stable tokens.
- Query indexes are derived caches, never the source of truth.
- A fully consumed anchored range is removed. Undoing text does not revive it;
  provider-derived state may be republished under a new ID.

## Reversible edits

- Undo and redo use ordinary `TextBuffer` mutations in reverse and forward
  order. They preserve the normal edit-log and anchored-range paths rather than
  restoring internal piece identities.
- Transactions bind to one buffer instance and edit sequence. Out-of-band
  mutation and invalid state transitions are programmer errors. The history
  manager may re-stamp a transaction after an adjacent entry restores the exact
  preceding text state, while retaining the buffer-identity check.
- `BufferModel` stores undo and redo entries as `EditTransaction`s. Replay is a
  normal public model commit: it stabilizes anchors, advances the revision,
  invalidates snapshots, and publishes a coherent text edit before its caller
  notifies observing views. A separate content identity follows history replay so
  documents can recognize a return to a saved state without making public
  revisions non-monotonic.
- Typing and directional deletion group by originating editor interaction and
  a short elapsed-time boundary. Cursor or selection movement, focus transfer,
  paste, other explicit commands, and a change of editor or edit kind end the
  current group. A forward edit after undo clears redo.
- History trees, persistence, and view-state restoration remain deferred.
