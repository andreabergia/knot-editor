# Step 6b — Reversible Edit Transaction Proof

> **Question:** can the stable-ID piece table and token-anchored annotation
> model support one reversible transaction without violating their established
> invariants?

## Decision

Build a small public `EditTransaction` in `core`, with no history manager.
It records primitive edits as they are applied to a `TextBuffer`, can undo the
whole transaction once, and can redo it. Undo/redo use ordinary buffer edits;
they do not restore piece IDs, truncate the append-only backing store, or
mutate annotations directly.

Annotations are provider-derived state, not transaction state. If a deletion
fully consumes an annotation, `AnchoredRangeStore` removes it rather than keeping
an invisible collapsed tombstone. Undoing the text edit does not revive that
annotation. A provider observing the new buffer revision may publish a fresh
annotation later; provider invocation and revision-aware async results remain
work for steps 7 and 12.

## Public surface

Add `core::transaction` and export:

```rust
pub struct EditTransaction { /* recorded primitives + applied state */ }

impl EditTransaction {
    pub fn new() -> Self;
    pub fn insert(&mut self, buffer: &mut TextBuffer, at: usize, text: &str);
    pub fn delete(&mut self, buffer: &mut TextBuffer, range: Range<usize>);
    pub fn replace(
        &mut self,
        buffer: &mut TextBuffer,
        range: Range<usize>,
        text: &str,
    );
    pub fn undo(&mut self, buffer: &mut TextBuffer);
    pub fn redo(&mut self, buffer: &mut TextBuffer);
}
```

- The mutation methods apply immediately and record the original bytes needed
  to invert a delete or replace. An empty/no-op primitive is not recorded.
- `undo` applies the recorded inverses in reverse primitive order; `redo`
  reapplies the original primitives in forward order. Both route exclusively
  through `TextBuffer::{insert,delete,replace}`, so each direction emits the
  normal `BufferEdit` log entries and preserves line-index maintenance.
- The transaction is single-use stateful: it begins applied after its first
  recorded mutation, `undo` requires that state, and `redo` requires the
  undone state. Invalid ordering panics, matching `TextBuffer`'s existing
  precondition style. A new edit after `undo` is out of scope; callers create
  a new transaction rather than branching history.
- `AnchoredRangeStore` remains buffer-agnostic. Its owner calls
  `stabilize(&buffer)` after the forward transaction, undo, and redo just as
  it does after every other buffer edit; no transaction-specific annotation
  API is introduced.

## Implementation

1. ✅ Add the transaction module and private recorded-primitive representation.
   Capture deleted/replaced text from `read_range` before mutating the buffer;
   retain the applied byte offsets and replacement lengths needed to form the
   inverse. Keep the public API limited to the five methods above
   (`src/core/transaction.rs`, wired in `src/core/mod.rs`). Before every
   recorded primitive and each `undo` / `redo`, the transaction validates the
   buffer's stable instance identity and `edit_seq`; it panics in
   `TextBuffer`'s precondition style on a buffer swap or an out-of-band edit,
   which would otherwise corrupt the recorded byte offsets.
2. ✅ Change annotation consumption semantics: remove fully consumed
   annotations and their endpoint-index entries instead of retaining a
   `collapsed` tombstone. `resolve` returns `None` for a removed ID, and
   `query_range` / `query_range_for_kinds` / `iter_live` cannot return it
   (it is no longer in `self.annotations`, and the interval index is rebuilt
   lazily from that map). The `collapsed` field is gone; the internal
   `collapse` helper is replaced by `consume`, which removes the annotation
   and its endpoint-index entries. Module and `resolve`-contract docs
   describe removal rather than invisibility (`src/core/anchored_range.rs`).
3. ✅ Add focused transaction tests in the transaction module and annotation
   tests for removal, plus the combined forward/undo/redo + annotation
   acceptance test. All `cargo test core` (103 tests) green.

## Acceptance tests

- A transaction containing insert, delete, and replace over a UTF-8 buffer
  produces the expected final text; undo restores the exact initial text; redo
  restores the exact final text. Include at least one multi-byte character and
  multiple primitives whose reverse ordering matters.
- The same transaction may complete at least two undo/redo cycles, with the
  same text on every state transition and valid buffer line queries.
- After each forward, undo, and redo pass, stabilize a store containing
  unaffected, boundary-sticky, and partially-overlapped annotations; their
  resolved ranges follow the established insert/delete semantics and remain
  queryable when live.
- An annotation wholly consumed by the forward deletion is removed after
  stabilization. It stays absent after undo/redo; no hidden tombstone,
  endpoint index entry, or query result remains.
- Run `cargo test core` (or the narrow equivalent if the crate's test layout
  requires it). No benchmark, persistence, history tree, edit grouping,
  command wiring, view-state restoration, memory reclamation, provider
  scheduling, or annotation regeneration is part of this step.

## Assumptions

- Byte offsets and UTF-8 boundary assertions retain the existing `TextBuffer`
  contract; grapheme-aware editing is not added here.
- Append-only piece/backing growth and the unbounded edit log remain accepted
  prototype limitations.
- Provider output is refreshed by its future lifecycle owner, not recreated by
  undo. A regenerated annotation receives a new annotation ID.
