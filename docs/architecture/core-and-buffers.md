# Core and buffer models

Part of the [architecture](../architecture.md). Design rationale is recorded in
[decisions](../decisions.md); deferred work lives in [roadmap](../roadmap.md).

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
- an optional language ID, explicit or detected, and the `SyntaxState` it
  resolves to;
- open/closed lifecycle and public revision;
- at most one immutable UTF-16 snapshot cache entry.

Presentation metadata stays in `app` and refers to core range IDs. A model
commit advances the public revision once, invalidates the snapshot cache, and
notifies every observing view. All text mutation paths enforce the model's
access policy; read-only models still permit snapshots, view anchors,
contributions, and closure. `core::TextBuffer` remains unconditionally mutable.

Extension buffer proxies remain usable independently of subscription and
contribution ownership. Closed models reject transport access even when retained
by stale views. Disposal, cancellation, and host release follow the shared
[host lifecycle](extension-host.md#disposal-cancellation-and-teardown).

## Stable positions and edit behavior

- Text offsets and ranges are half-open UTF-8 byte ranges. Presentation layers
  handle grapheme-aware behavior. Protocol field names identify byte offsets
  explicitly.
- A `Position` identifies a piece and offset. Splits preserve the left piece ID
  and allocate a right piece ID; edit-log records repair affected stale
  positions. Unrelated positions require no work.
- The line index is maintained on every edit and records each line start as
  both a byte offset and a UTF-16 offset, so views and platform text input
  convert positions without scanning the document. It is intentionally
  replaceable: its linear suffix update dominates large-buffer edit cost, and
  replacement remains deferred pending real workload evidence.
- Inserted backing storage is append-only. Contiguous typing extends the piece
  at the end of the add buffer instead of adding one per keystroke; other
  edits still add pieces, and compaction remains deferred.
- The original text and sealed add-buffer chunks are shared `Arc<str>`s, so an
  immutable `Send` `TextSnapshot` copies only the unsealed tail and the piece
  list. Readers off the foreground use snapshots instead of the live buffer.
- Each primitive edit records its start and old and new end as line and
  byte-column points as well as byte offsets, because a later primitive in
  the same commit makes them unrecoverable; syntax trees are edited with them.
- Edit-log sequence numbers are monotonic. `BufferModel` discards entries once
  its anchored-range store and syntax state have consumed them, so the
  retained log holds at most one commit.

## Anchored ranges

- `AnchoredRangeStore` owns stable range geometry only. Each endpoint is a
  `Position`; an insert exactly at an endpoint moves it past the inserted text,
  so a range excludes text inserted at its start and includes text inserted at
  its end.
- Stabilization consumes one authoritative buffer edit stream. It repairs only
  affected endpoints; untouched ranges retain stable tokens.
- A fully consumed anchored range is removed. Undoing text does not revive it;
  provider-derived state may be republished under a new ID.

## Syntax state

Parsing is a native buffer mechanic, like shaping, so it follows every commit
synchronously and painting never calls JavaScript. A buffer with a resolved
language owns one `SyntaxState`: a tree of tree-sitter parse layers, with the
buffer's language at the root and every injected region (a Markdown fence, an
HTML `<script>`) nested beneath its host, up to a fixed depth.

```text
BufferModel commit
        |
BufferEdit log ---> AnchoredRangeStore::stabilize
        |
        +---------> SyntaxState: edit every affected tree, reparse within
                    a 1 ms foreground budget
                            |
                    out of budget: BackgroundParse over a TextSnapshot
                            |
                    foreground: rebase over later edits, or reject for a
                    replaced state
```

- The model feeds the syntax state in the same step that stabilizes anchors,
  so node positions describe the current text even before a reparse. Commit
  paths need no gpui context; the model observes its own notifications and
  starts a background parse whenever a commit leaves the tree behind.
- While a background parse runs, highlights come from the edited previous
  trees. A result whose revision no longer matches is edited forward and
  reparsed; one for a replaced or closed state is dropped, and replacing or
  dropping a state cancels its parse.
- A reparse reruns a layer's injection query only over its changed and edited
  ranges, and an injection found again where a stale one of the same
  language was reuses its tree, so typing inside a fence stays incremental.
- Each state has a globally unique generation that advances when its trees
  change; views cache visible-range highlight queries by it.
- Which language a buffer parses is application policy: the
  [language registry](semantic-ui.md#syntax-highlighting-and-languages)
  replaces the state whenever the resolved language changes.

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
  invalidates snapshots, and publishes one text edit covering the replayed
  primitives, read from the replaced span only, before its caller
  notifies observing views. A separate content identity follows history replay so
  documents can recognize a return to a saved state without making public
  revisions non-monotonic.
- Typing and directional deletion group by originating editor interaction and
  a short elapsed-time boundary. Cursor or selection movement, focus transfer,
  paste, other explicit commands, and a change of editor or edit kind end the
  current group. A forward edit after undo clears redo.
- An editor-originated history entry records byte-offset caret and selection
  snapshots for that view. Undo restores the group's initial snapshot and redo
  its final snapshot in the originating view, even when replay is invoked from
  another view. Other views retain their independently anchored positions, and
  history does not retain view entities after closure. Restoration is revision
  gated so a newer edit supersedes an unobserved replay snapshot.
- History trees, history persistence, and persistent view-state restoration
  across restarts remain deferred.
