//! `EditTransaction`: one reversible primitive-edit transaction (step 6b).
//!
//! This is a small, public, single-use stateful transaction recorder. It
//! applies primitive edits (`insert` / `delete` / `replace`) to a `TextBuffer`
//! immediately, remembers the bytes needed to invert each one, and exposes
//! `undo` / `redo` for the whole transaction as a unit.
//!
//! ## Scope
//!
//! The transaction is deliberately not a history manager:
//!
//! - It records exactly one applied-then-possibly-undone batch of primitives.
//!   A new edit after `undo` is out of scope; callers create a new transaction
//!   rather than branching history.
//! - The transaction stores raw byte offsets captured at apply time. It
//!   checkpoints both the buffer instance identity and `edit_seq` before
//!   every recorded primitive and before `undo` / `redo`, rejecting an
//!   out-of-band edit or buffer swap in `TextBuffer`'s precondition style.
//! - Undo and redo route exclusively through `TextBuffer::{insert, delete,
//!   replace}`, so each direction emits the normal `BufferEdit` log entries
//!   and preserves line-index maintenance. No piece IDs are restored, the
//!   append-only `Add` backing store is never truncated, and anchored ranges are
//!   not mutated by the transaction. (Because both directions emit new
//!   `BufferEdit`s, the buffer's `edit_seq` advances through undo and redo;
//!   the checkpoint is re-stamped after each pass so the next pass is still
//!   validated.)
//! - `AnchoredRangeStore` remains buffer-agnostic. Its owner calls
//!   `stabilize(&buffer)` after forward, undo, and redo passes just as it does
//!   after every other buffer edit; no transaction-specific anchored-range API is
//!   introduced. A fully consumed anchored range is removed (not tombstoned) by the
//!   store; undoing the text edit does not revive it — a provider must
//!   re-publish under a fresh id.

use std::ops::Range;

use super::buffer::TextBuffer;

/// One recorded primitive edit, retained with the bytes needed to invert it.
#[derive(Clone, Debug)]
enum Primitive {
    /// Bytes inserted at `at`. Inverse: delete `[at, at + text.len())`.
    Insert { at: usize, text: String },
    /// Bytes deleted starting at `at`. Inverse: re-insert `text` at `at`.
    Delete { at: usize, text: String },
    /// Span `[at, at + deleted.len())` replaced with `inserted`. Inverse:
    /// replace `[at, at + inserted.len())` with `deleted`.
    Replace {
        at: usize,
        deleted: String,
        inserted: String,
    },
}

/// Transaction state. The transaction begins `Applied` (with no primitives
/// recorded, both `undo` and `redo` are no-ops on the empty record).
/// After the first recorded mutation it is `Applied`; `undo` flips it to
/// `Undone`; `redo` flips it back. Invalid orderings panic, matching
/// `TextBuffer`'s precondition style.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum State {
    Applied,
    Undone,
}

/// One reversible primitive-edit transaction over a `TextBuffer`.
///
/// See the module docs for the contract and scope.
pub struct EditTransaction {
    primitives: Vec<Primitive>,
    state: State,
    /// Buffer identity and edit-log sequence expected at the entry of the
    /// next recorded operation. `None` until the first recorded primitive,
    /// so empty transactions remain no-ops on any buffer.
    checkpoint: Option<Checkpoint>,
}

#[derive(Clone, Copy, Debug)]
struct Checkpoint {
    buffer_id: u64,
    edit_seq: usize,
}

impl EditTransaction {
    /// Create an empty transaction in the `Applied` state.
    pub fn new() -> Self {
        Self {
            primitives: Vec::new(),
            state: State::Applied,
            checkpoint: None,
        }
    }

    /// Assert that forward mutations are still permitted: the transaction
    /// must be `Applied`. Calling `insert` / `delete` / `replace` after
    /// `undo` panics — branch history by creating a new transaction instead.
    fn assert_mutable(&self) {
        assert_eq!(
            self.state,
            State::Applied,
            "EditTransaction cannot be mutated after undo; create a new transaction"
        );
    }

    /// Reject intervening edits or a buffer swap before mutating. Recorded
    /// primitives carry raw byte offsets valid only against the exact buffer
    /// state recorded by the preceding operation. A `None` checkpoint (the
    /// first primitive or an empty transaction) is a no-op.
    fn assert_buffer_unchanged(&self, buffer: &TextBuffer) {
        if let Some(checkpoint) = self.checkpoint {
            assert_eq!(
                buffer.instance_id(),
                checkpoint.buffer_id,
                "EditTransaction requires the same TextBuffer instance for every \
                 recorded operation; another buffer would corrupt the recorded byte offsets"
            );
            assert_eq!(
                buffer.edit_seq(),
                checkpoint.edit_seq,
                "EditTransaction requires its TextBuffer to be unchanged since \
                 the previous operation; an out-of-band edit would corrupt the \
                 recorded byte offsets"
            );
        }
    }

    fn checkpoint(&mut self, buffer: &TextBuffer) {
        self.checkpoint = Some(Checkpoint {
            buffer_id: buffer.instance_id(),
            edit_seq: buffer.edit_seq(),
        });
    }

    /// Insert `text` at byte offset `at` in `buffer` and record the primitive.
    /// An empty `text` is a no-op and is not recorded.
    pub fn insert(&mut self, buffer: &mut TextBuffer, at: usize, text: &str) {
        self.assert_mutable();
        if text.is_empty() {
            return;
        }
        self.assert_buffer_unchanged(buffer);
        buffer.insert(at, text);
        self.primitives.push(Primitive::Insert {
            at,
            text: text.to_string(),
        });
        self.checkpoint(buffer);
    }

    /// Delete the byte `range` from `buffer` and record the primitive. An
    /// empty range is a no-op and is not recorded. The deleted bytes are
    /// captured via `read_range` before the mutation so the inverse can
    /// re-insert them.
    pub fn delete(&mut self, buffer: &mut TextBuffer, range: Range<usize>) {
        self.assert_mutable();
        if range.is_empty() {
            return;
        }
        self.assert_buffer_unchanged(buffer);
        let text = buffer.read_range(range.clone());
        buffer.delete(range.clone());
        self.primitives.push(Primitive::Delete {
            at: range.start,
            text,
        });
        self.checkpoint(buffer);
    }

    /// Replace `range` in `buffer` with `text`. Decomposes into the natural
    /// primitive: an empty range records an `Insert` primitive, an empty
    /// `text` records a `Delete` primitive, and a fully-empty no-op (empty
    /// range and empty text) records nothing. Otherwise a `Replace` primitive
    /// captures both the deleted and inserted spans.
    pub fn replace(&mut self, buffer: &mut TextBuffer, range: Range<usize>, text: &str) {
        self.assert_mutable();
        if range.is_empty() && text.is_empty() {
            return;
        }
        if range.is_empty() {
            self.insert(buffer, range.start, text);
            return;
        }
        if text.is_empty() {
            self.delete(buffer, range);
            return;
        }
        self.assert_buffer_unchanged(buffer);
        let deleted = buffer.read_range(range.clone());
        buffer.replace(range.clone(), text);
        self.primitives.push(Primitive::Replace {
            at: range.start,
            deleted,
            inserted: text.to_string(),
        });
        self.checkpoint(buffer);
    }

    /// Undo the whole transaction by applying the recorded inverses in
    /// reverse primitive order. Routes exclusively through
    /// `TextBuffer::{insert, delete, replace}`, so the normal `BufferEdit`
    /// log entries and line-index maintenance apply. Panics if the
    /// transaction is not currently `Applied`, or if the buffer has been
    /// edited out-of-band (or swapped) since the forward pass.
    pub fn undo(&mut self, buffer: &mut TextBuffer) {
        assert_eq!(
            self.state,
            State::Applied,
            "undo requires the transaction to be applied"
        );
        if self.primitives.is_empty() {
            self.state = State::Undone;
            return;
        }
        self.assert_buffer_unchanged(buffer);
        for prim in self.primitives.iter().rev() {
            match prim {
                Primitive::Insert { at, text } => {
                    buffer.delete(*at..*at + text.len());
                }
                Primitive::Delete { at, text } => {
                    buffer.insert(*at, text);
                }
                Primitive::Replace {
                    at,
                    deleted,
                    inserted,
                    ..
                } => {
                    buffer.replace(*at..*at + inserted.len(), deleted);
                }
            }
        }
        self.state = State::Undone;
        self.checkpoint(buffer);
    }

    /// Redo the whole transaction by reapplying the original primitives in
    /// forward order. Like `undo`, routes exclusively through
    /// `TextBuffer::{insert, delete, replace}`. Panics if the transaction
    /// is not currently `Undone`, or if the buffer has been edited
    /// out-of-band (or swapped) since the `undo`.
    pub fn redo(&mut self, buffer: &mut TextBuffer) {
        assert_eq!(
            self.state,
            State::Undone,
            "redo requires the transaction to be undone"
        );
        if self.primitives.is_empty() {
            self.state = State::Applied;
            return;
        }
        self.assert_buffer_unchanged(buffer);
        for prim in &self.primitives {
            match prim {
                Primitive::Insert { at, text } => {
                    buffer.insert(*at, text);
                }
                Primitive::Delete { at, text } => {
                    buffer.delete(*at..*at + text.len());
                }
                Primitive::Replace {
                    at,
                    deleted,
                    inserted,
                } => {
                    buffer.replace(*at..*at + deleted.len(), inserted);
                }
            }
        }
        self.state = State::Applied;
        self.checkpoint(buffer);
    }
}

impl Default for EditTransaction {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::buffer::TextBuffer;

    fn s(b: &TextBuffer) -> String {
        b.read_range(0..b.len())
    }

    #[test]
    fn empty_transaction_undo_redo_are_no_ops() {
        let mut b = TextBuffer::from_text("hello");
        let mut tx = EditTransaction::new();
        tx.undo(&mut b);
        tx.redo(&mut b);
        assert_eq!(s(&b), "hello");
    }

    #[test]
    fn insert_delete_replace_undo_restores_initial() {
        // Multi-primitive transaction with a UTF-8 byte (é = 2 bytes) whose
        // reverse ordering matters: undo must walk primitives back-to-front.
        // Initial layout of "héllo": h(0) é(1-2) l(3) l(4) o(5). len 6.
        let mut b = TextBuffer::from_text("héllo");
        let initial = s(&b);
        let mut tx = EditTransaction::new();
        // P1: insert "AB" at 0 -> "ABhéllo": A0 B1 h2 é3-4 l5 l6 o7.
        tx.insert(&mut b, 0, "AB");
        // P2: delete [2, 5) -> removes "h" + "é" (bytes 2,3,4) -> "ABllo".
        tx.delete(&mut b, 2..5);
        // P3: replace [0, 2) ("AB") with "XY" -> "XYllo".
        tx.replace(&mut b, 0..2, "XY");
        let after = s(&b);
        assert_eq!(after, "XYllo", "final text after forward pass");
        // Undo restores the exact initial text.
        tx.undo(&mut b);
        assert_eq!(s(&b), initial, "undo restores initial text");
        // Redo restores the exact final text.
        tx.redo(&mut b);
        assert_eq!(s(&b), after, "redo restores final text");
    }

    #[test]
    fn undo_redo_two_cycles_same_text() {
        // αβγδε — each Greek letter is 2 UTF-8 bytes; total 10. Offsets:
        // α(0-1) β(2-3) γ(4-5) δ(6-7) ε(8-9).
        let mut b = TextBuffer::from_text("αβγδε");
        let mut tx = EditTransaction::new();
        tx.insert(&mut b, 2, "X"); // -> "αXβγδε"; X at byte 2, β now 3-4.
        tx.delete(&mut b, 3..5); // delete "β" (bytes 3-4) -> "αXγδε".
        tx.replace(&mut b, 0..2, "Z"); // replace "α" (bytes 0-1) -> "ZXγδε".
        let final_text = s(&b);
        assert_eq!(final_text, "ZXγδε");
        // Two undo/redo cycles.
        for cycle in 0..2 {
            tx.undo(&mut b);
            assert_eq!(s(&b), "αβγδε", "undo cycle {cycle} restores initial");
            assert!(b.line_of_offset(b.len()) < b.line_count());
            tx.redo(&mut b);
            assert_eq!(s(&b), final_text, "redo cycle {cycle} restores final");
            assert!(b.line_of_offset(b.len()) < b.line_count());
        }
    }

    #[test]
    fn line_queries_valid_throughout_undo_redo() {
        let mut b = TextBuffer::from_text("ab\ncd\nef"); // [0,3,6]
        let _ = b.line_count();
        let mut tx = EditTransaction::new();
        // P1: insert "X" at byte 0 -> "Xab\ncd\nef"; line starts [0,4,7].
        tx.insert(&mut b, 0, "X");
        assert_eq!(b.line_count(), 3);
        assert_eq!(b.line_start(1), 4);
        // P2: replace [2, 4) ("b\n") with "Y" -> "XaYcd\nef"; line starts [0,6].
        tx.replace(&mut b, 2..4, "Y");
        let forward_lines = b.line_count();
        let forward_lo = b.line_of_offset(6);
        assert_eq!(forward_lines, 2, "P2 collapses two lines into one");
        tx.undo(&mut b);
        assert_eq!(b.line_count(), 3, "undo restores line count");
        assert_eq!(b.line_start(1), 3, "undo restores line starts");
        tx.redo(&mut b);
        assert_eq!(b.line_count(), forward_lines, "redo restores line count");
        assert_eq!(
            b.line_of_offset(6),
            forward_lo,
            "redo restores line_of_offset"
        );
    }

    #[test]
    fn no_op_primitives_not_recorded() {
        let mut b = TextBuffer::from_text("abc");
        let mut tx = EditTransaction::new();
        tx.insert(&mut b, 0, "");
        tx.delete(&mut b, 1..1);
        tx.replace(&mut b, 1..1, "");
        assert_eq!(s(&b), "abc");
        // Undoing the empty transaction is a no-op (in reverse order, still
        // empty), and the state flips cleanly to Undone.
        tx.undo(&mut b);
        assert_eq!(s(&b), "abc");
        tx.redo(&mut b);
        assert_eq!(s(&b), "abc");
    }

    #[test]
    fn replace_empty_range_records_insert_primitive() {
        let mut b = TextBuffer::from_text("abc");
        let mut tx = EditTransaction::new();
        tx.replace(&mut b, 1..1, "X"); // -> "aXbc"
        assert_eq!(s(&b), "aXbc");
        tx.undo(&mut b);
        assert_eq!(s(&b), "abc");
        tx.redo(&mut b);
        assert_eq!(s(&b), "aXbc");
    }

    #[test]
    fn replace_nonempty_range_empty_text_records_delete_primitive() {
        let mut b = TextBuffer::from_text("abcd");
        let mut tx = EditTransaction::new();
        tx.replace(&mut b, 1..3, ""); // -> "ad"
        assert_eq!(s(&b), "ad");
        tx.undo(&mut b);
        assert_eq!(s(&b), "abcd");
        tx.redo(&mut b);
        assert_eq!(s(&b), "ad");
    }

    #[test]
    #[should_panic(expected = "undo requires the transaction to be applied")]
    fn double_undo_panics() {
        let mut b = TextBuffer::from_text("abc");
        let mut tx = EditTransaction::new();
        tx.insert(&mut b, 0, "X");
        tx.undo(&mut b);
        tx.undo(&mut b);
    }

    #[test]
    #[should_panic(expected = "redo requires the transaction to be undone")]
    fn double_redo_panics() {
        let mut b = TextBuffer::from_text("abc");
        let mut tx = EditTransaction::new();
        tx.insert(&mut b, 0, "X");
        tx.undo(&mut b);
        tx.redo(&mut b);
        tx.redo(&mut b);
    }

    #[test]
    #[should_panic(expected = "EditTransaction cannot be mutated after undo")]
    fn mutate_after_undo_panics() {
        let mut b = TextBuffer::from_text("abc");
        let mut tx = EditTransaction::new();
        tx.insert(&mut b, 0, "X");
        tx.undo(&mut b);
        tx.insert(&mut b, 0, "Y");
    }

    // ---- Buffer-unchanged precondition --------------------------------

    #[test]
    #[should_panic(expected = "would corrupt the recorded byte offsets")]
    fn out_of_band_edit_before_undo_panics() {
        // The exact corruption the reviewer flagged: a forward insert at
        // offset 1 records that offset; an out-of-band insert at 0 shifts
        // the buffer, so a naive undo would delete the wrong byte. The
        // checkpoint must catch the intervening edit.
        let mut b = TextBuffer::from_text("abc");
        let mut tx = EditTransaction::new();
        tx.insert(&mut b, 1, "X"); // "aXbc"
        b.insert(0, "Y"); // "YaXbc" — out-of-band, advances edit_seq
        tx.undo(&mut b); // must panic, not delete the "a"
    }

    #[test]
    #[should_panic(expected = "would corrupt the recorded byte offsets")]
    fn out_of_band_edit_before_redo_panics() {
        let mut b = TextBuffer::from_text("abc");
        let mut tx = EditTransaction::new();
        tx.delete(&mut b, 0..1); // "bc"
        tx.undo(&mut b); // "abc"
        b.insert(0, "Z"); // out-of-band between undo and redo
        tx.redo(&mut b); // must panic
    }

    #[test]
    #[should_panic(expected = "would corrupt the recorded byte offsets")]
    fn undo_on_different_buffer_panics() {
        let mut b = TextBuffer::from_text("abc");
        let mut tx = EditTransaction::new();
        tx.insert(&mut b, 1, "X");
        // Give the other buffer the same sequence number as the original;
        // the buffer instance identity must still reject it.
        let mut other = TextBuffer::from_text("abc");
        other.insert(0, "Z");
        tx.undo(&mut other);
    }

    #[test]
    #[should_panic(expected = "unchanged since the previous operation")]
    fn out_of_band_edit_between_forward_primitives_panics() {
        let mut b = TextBuffer::from_text("abc");
        let mut tx = EditTransaction::new();
        tx.insert(&mut b, 1, "X");
        b.insert(0, "Y");
        tx.delete(&mut b, 2..3);
    }

    #[test]
    fn empty_transaction_undo_redo_skip_buffer_check() {
        // An empty transaction has no checkpoint, so undo/redo are no-ops
        // that don't require any particular buffer state.
        let mut b = TextBuffer::from_text("abc");
        let mut tx = EditTransaction::new();
        tx.insert(&mut b, 0, ""); // no-op, no checkpoint
        tx.delete(&mut b, 0..0); // no-op, no checkpoint
        tx.replace(&mut b, 1..1, ""); // no-op, no checkpoint
        let mut other = TextBuffer::from_text("xyz");
        tx.undo(&mut other); // allowed: nothing recorded
        tx.redo(&mut b); // allowed: nothing recorded
        assert_eq!(b.read_range(0..b.len()), "abc");
        assert_eq!(other.read_range(0..other.len()), "xyz");
    }

    // ---- Step 6b acceptance: transaction flow with anchored ranges ---------

    #[test]
    fn anchored_ranges_survive_or_get_consumed_through_forward_undo_redo() {
        use crate::core::anchored_range::AnchoredRangeStore;

        // "hello world": h0 e1 l2 l3 o4 SP5 w6 o7 r8 l9 d10. len 11.
        let mut b = TextBuffer::from_text("hello world");
        let mut store = AnchoredRangeStore::new();
        // Unaffected by the delete: stays put.
        let unaffected = store.add(&b, 0, 2);
        // Both ends around the delete; end past `e` shifts by delete_len.
        let bsticky = store.add(&b, 3, 9);
        // Spans across the delete boundary on both sides.
        let partial = store.add(&b, 4, 10);
        // Wholly inside the delete: end + start strictly inside (s, e).
        let victim = store.add(&b, 6, 7);

        // Forward: delete " wo" (bytes 5..8) -> "hellorld".
        let mut tx = EditTransaction::new();
        tx.delete(&mut b, 5..8);
        store.stabilize(&b);
        assert_eq!(s(&b), "hellorld");

        // Victim is wholly consumed and removed — no tombstone anywhere.
        assert_eq!(store.resolve(&b, victim), None, "victim consumed");
        assert!(
            !store.query_range(&b, 0, b.len()).contains(&victim),
            "victim absent from query_range"
        );
        assert_eq!(store.len(), 3, "only three live anchored ranges remain");

        // Live anchored ranges follow the established insert/delete semantics:
        // - unaffected: offsets <= s keep their bytes.
        // - bsticky [3, 9): start 3 <= s stays at 3; end 9 >= e shifts by
        //   delete_len 3 -> 6; final [3, 6).
        // - partial [4, 10): start 4 <= s stays at 4; end 10 >= e shifts to
        //   10 - 3 = 7; final [4, 7).
        assert_eq!(store.resolve(&b, unaffected), Some(0..2));
        assert_eq!(store.resolve(&b, bsticky), Some(3..6));
        assert_eq!(store.resolve(&b, partial), Some(4..7));

        // All three live anchored ranges remain queryable.
        let visible = store.query_range(&b, 0, b.len());
        assert_eq!(visible.len(), 3);
        assert!(visible.contains(&unaffected));
        assert!(visible.contains(&bsticky));
        assert!(visible.contains(&partial));

        // Undo restores the deleted text. Victim stays removed — undo does
        // not revive a consumed anchored range.
        tx.undo(&mut b);
        store.stabilize(&b);
        assert_eq!(s(&b), "hello world");
        assert_eq!(
            store.resolve(&b, victim),
            None,
            "victim not revived by undo"
        );
        let after_undo = store.query_range(&b, 0, b.len());
        assert!(after_undo.contains(&unaffected));
        assert!(after_undo.contains(&bsticky));
        assert!(after_undo.contains(&partial));
        assert!(!after_undo.contains(&victim));

        // Redo re-applies the delete. Live anchored ranges follow the same
        // forward semantics; the victim is still gone.
        tx.redo(&mut b);
        store.stabilize(&b);
        assert_eq!(s(&b), "hellorld");
        assert_eq!(
            store.resolve(&b, victim),
            None,
            "victim still absent after redo"
        );
        assert_eq!(store.resolve(&b, unaffected), Some(0..2));
        assert_eq!(store.resolve(&b, bsticky), Some(3..6));
        assert_eq!(store.resolve(&b, partial), Some(4..7));
        let after_redo = store.query_range(&b, 0, b.len());
        assert_eq!(after_redo.len(), 3);
        assert!(after_redo.contains(&unaffected));
        assert!(after_redo.contains(&bsticky));
        assert!(after_redo.contains(&partial));
    }
}
