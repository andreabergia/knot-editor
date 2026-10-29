use super::*;
use crate::core::buffer::TextPoint;

// Expected byte-offset transform for the randomized correctness oracle.
fn transform_offset(off: usize, edit: &BufferEdit) -> usize {
    match edit {
        BufferEdit::Insert {
            at, inserted_len, ..
        } => {
            if off < *at {
                off
            } else {
                off + inserted_len
            }
        }
        BufferEdit::Delete { range, .. } => {
            if off <= range.start {
                off
            } else if off >= range.end {
                off - (range.end - range.start)
            } else {
                range.start
            }
        }
    }
}

fn text() -> TextBuffer {
    TextBuffer::from_text("hello world")
}

#[test]
fn add_and_resolve_round_trips() {
    let b = text();
    let mut store = AnchoredRangeStore::new();
    let id = store.add(&b, 0, 5);
    assert_eq!(store.resolve(&b, id), Some(0..5));
    let id2 = store.add(&b, 6, 11);
    assert_eq!(store.resolve(&b, id2), Some(6..11));
}

#[test]
fn remove_drops_anchored_range() {
    let b = text();
    let mut store = AnchoredRangeStore::new();
    let id = store.add(&b, 0, 5);
    store.remove(id);
    assert!(store.resolve(&b, id).is_none());
    assert!(store.is_empty());
}

#[test]
fn insert_before_start_does_not_move_anchored_range() {
    let mut b = text();
    let mut store = AnchoredRangeStore::new();
    let id = store.add(&b, 6, 11);
    b.insert(0, ">>");
    store.stabilize(&b);
    // Both endpoints shifted by 2; the anchored range still covers "world".
    assert_eq!(store.resolve(&b, id), Some(8..13));
}

#[test]
fn insert_at_before_start_keeps_byte() {
    // start is `Before` (sticky-left): insert at the start offset keeps
    // the endpoint put (text lands before it).
    let mut b = text();
    let mut store = AnchoredRangeStore::new();
    let id = store.add(&b, 0, 5);
    b.insert(0, "X");
    store.stabilize(&b);
    // The `Before` start is sticky-left: it keeps pointing at the same
    // content ("h"), which the insert pushed to byte 1. The anchored range
    // still covers "hello", now at bytes 1..6 (it does NOT swallow "X").
    assert_eq!(store.resolve(&b, id), Some(1..6));
}

#[test]
fn insert_at_after_end_grows_anchored_range() {
    // end is `After` (sticky-right): insert at the end offset relocates
    // the endpoint past the inserted span.
    let mut b = text();
    let mut store = AnchoredRangeStore::new();
    let id = store.add(&b, 0, 5);
    b.insert(5, "X");
    store.stabilize(&b);
    assert_eq!(store.resolve(&b, id), Some(0..6));
}

#[test]
fn coalesced_insert_repairs_like_a_piece_boundary() {
    // Typing at the end of a typed span extends its piece instead of
    // adding one; boundary endpoints must move exactly as they do at the
    // end of an ordinary piece.
    let resolve_after_typing = |mut b: TextBuffer, mut store: AnchoredRangeStore| {
        let empty = store.add(&b, 3, 3);
        let ending = store.add(&b, 2, 3);
        let inside = store.add(&b, 1, 2);
        b.insert(3, "d");
        store.stabilize(&b);
        assert_eq!(b.read_range(0..b.len()), "abcd");
        [empty, ending, inside].map(|id| store.resolve(&b, id))
    };
    let mut typed = TextBuffer::from_text("ab");
    let mut typed_store = AnchoredRangeStore::new();
    typed.insert(2, "c");
    typed_store.stabilize(&typed);
    assert_eq!(
        resolve_after_typing(typed, typed_store),
        resolve_after_typing(TextBuffer::from_text("abc"), AnchoredRangeStore::new())
    );
}

#[test]
fn insert_at_after_end_extends() {
    let mut b = text();
    let mut store = AnchoredRangeStore::new();
    let id = store.add(&b, 0, 5);
    // "hello" then insert at offset 5 (right after "hello").
    b.insert(5, ">>>");
    store.stabilize(&b);
    // The After end relocates to 5 + 3 = 8.
    assert_eq!(store.resolve(&b, id), Some(0..8));
}

#[test]
fn delete_spanning_anchored_range_collapses_to_zero() {
    let mut b = text();
    let mut store = AnchoredRangeStore::new();
    let id = store.add(&b, 3, 8);
    // Delete [2, 9) which fully contains [3, 8).
    b.delete(2..9);
    store.stabilize(&b);
    // Fully deleted -> consumed, no extent; anchored range is removed entirely.
    assert_eq!(store.resolve(&b, id), None);
}

#[test]
fn persistent_range_collapses_at_a_full_deletion() {
    let mut b = text();
    let mut store = AnchoredRangeStore::new();
    let id = store.add_persistent(&b, 3, 8);

    b.delete(2..9);
    store.stabilize(&b);

    assert_eq!(store.resolve(&b, id), Some(2..2));
}

#[test]
fn delete_partial_left_collapses_start_to_surviving_edge() {
    let mut b = text();
    let mut store = AnchoredRangeStore::new();
    // "hello world": delete [0, 6) removes "hello " -> surviving "world".
    let id = store.add(&b, 2, 8);
    b.delete(0..6);
    store.stabilize(&b);
    // start (2) inside -> collapses to s=0; end (8) >= e=6 -> shifts to 2.
    assert_eq!(store.resolve(&b, id), Some(0..2));
}

#[test]
fn interior_split_stale_right_half_repaired() {
    // Build a buffer where an anchored range's end sits in the right half of
    // a piece, then split that piece with an insert and confirm the token
    // is repaired (content preserved), not invalidated.
    let mut b = TextBuffer::from_text("abcdefghij"); // single piece, len 10
    let mut store = AnchoredRangeStore::new();
    // Anchor [2, 7): end at 7 is in the right half of the original piece.
    let id = store.add(&b, 2, 7);
    assert_eq!(store.resolve(&b, id), Some(2..7));
    // Insert at offset 4 splits the original piece; "bcdefg" (the content
    // between the endpoints) is now preceded by an extra byte.
    b.insert(4, "X");
    store.stabilize(&b);
    // "abcdXefghij": original [2,7) ("cdefg") is now [2,8).
    assert_eq!(store.resolve(&b, id), Some(2..8));
}

#[test]
fn untouched_anchored_range_costs_nothing() {
    let mut b = text();
    let mut store = AnchoredRangeStore::new();
    let touched = store.add(&b, 0, 5);
    let _untouched = store.add(&b, 6, 11);
    // Edit at the start only affects the first anchored range.
    b.insert(0, "X");
    store.stabilize(&b);
    // `touched` start is `Before` (sticky-left): the "X" lands before it,
    // so "hello" shifts to 1..6.
    assert_eq!(store.resolve(&b, touched), Some(1..6));
    // The second anchored range shifted by the insert but was not specially
    // processed; its token stayed valid (stability, not O(all) work).
    assert_eq!(store.resolve(&b, _untouched), Some(7..12));
}

#[test]
fn zero_width_boundary_survives_empty_delete_and_reanchors_on_insert() {
    let mut b = TextBuffer::from_text("a");
    let mut store = AnchoredRangeStore::new();
    let id = store.add(&b, 1, 1);
    b.delete(0..1);
    store.stabilize(&b);
    assert_eq!(store.resolve(&b, id), Some(0..0));

    b.insert(0, "xy");
    store.stabilize(&b);
    assert_eq!(store.resolve(&b, id), Some(2..2));
}

// ---- Removal-on-consumption semantics ---------------------

#[test]
fn fully_consumed_anchored_range_removed_not_tombstoned() {
    let mut b = TextBuffer::from_text("hello world");
    let mut store = AnchoredRangeStore::new();
    let victim = store.add(&b, 6, 7);
    let other = store.add(&b, 0, 2);

    // Delete [5, 8) on "hello world" — fully contains [6, 7).
    b.delete(5..8);
    store.stabilize(&b);
    assert_eq!(b.read_range(0..b.len()), "hellorld");

    // Removed entirely — no tombstone and no iter_live entry.
    assert_eq!(store.resolve(&b, victim), None, "victim fully consumed");
    assert!(
        !store.iter_live().any(|(id, _)| *id == victim),
        "victim must not appear in iter_live"
    );
    assert_eq!(store.len(), 1, "only the unaffected anchored range remains");
    assert_eq!(store.resolve(&b, other), Some(0..2));

    // A later add re-uses no id; the victim's id is gone for good.
    let replacement = store.add(&b, 6, 7);
    assert_ne!(replacement, victim, "monotonic ids; victim id not reused");
    assert_eq!(store.resolve(&b, replacement), Some(6..7));
}

#[test]
fn consumed_anchored_range_stays_removed_through_undo_redo() {
    // The text-edit inverse (re-insert) does not revive a consumed
    // anchored range; the provider must re-publish under a fresh id.
    let mut b = TextBuffer::from_text("hello world");
    let mut store = AnchoredRangeStore::new();
    let victim = store.add(&b, 6, 7);

    b.delete(5..8); // "hellorld", victim fully consumed
    store.stabilize(&b);
    assert_eq!(store.resolve(&b, victim), None);

    // Undo the delete: the log re-emits a forward insert at byte 5;
    // stabilize re-runs through it, but the victim is already removed
    // and is not re-added.
    b.insert(5, " wo");
    store.stabilize(&b);
    assert_eq!(b.read_range(0..b.len()), "hello world");
    assert_eq!(
        store.resolve(&b, victim),
        None,
        "undo does not revive victim"
    );

    // Redo: delete again. The victim is still gone.
    b.delete(5..8);
    store.stabilize(&b);
    assert_eq!(b.read_range(0..b.len()), "hellorld");
    assert_eq!(
        store.resolve(&b, victim),
        None,
        "redo still finds no victim"
    );
}

// ---- Randomized correctness oracle (workload 4 of the plan) ----------

/// xorshift64 — deterministic RNG for the oracle test.
struct Rng(u64);
impl Rng {
    fn new(seed: u64) -> Self {
        Rng(seed.max(1))
    }
    fn next_u64(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x
    }
    fn below(&mut self, n: usize) -> usize {
        if n == 0 {
            return 0;
        }
        (self.next_u64() % n as u64) as usize
    }
}

fn edit_insert(at: usize, len: usize) -> BufferEdit {
    BufferEdit::Insert {
        at,
        inserted_len: len,
        at_point: TextPoint::default(),
        end_point: TextPoint::default(),
        inserted_end: Position::new(0, len as u32),
        left_piece: None,
        pre_first_piece: None,
        splits: vec![],
    }
}
fn edit_delete(s: usize, e: usize) -> BufferEdit {
    BufferEdit::Delete {
        range: s..e,
        start_point: TextPoint::default(),
        end_point: TextPoint::default(),
        deleted_pieces: vec![],
        deleted_piece_lens: vec![],
        left_survivor: None,
        right_survivor: None,
        splits: vec![],
    }
}

#[test]
fn randomized_correctness_oracle() {
    // ASCII source text: every offset is a UTF-8 char boundary, so all
    // random edits are valid.
    let src = "the quick brown fox jumps over the lazy dog and then some more text here";
    let mut b = TextBuffer::from_text(src);
    let mut store = AnchoredRangeStore::new();
    let mut rng = Rng::new(0x1234_ABCD);

    // Seed anchored ranges at random byte ranges.
    const N: usize = 200;
    let mut o_start = Vec::with_capacity(N);
    let mut o_end = Vec::with_capacity(N);
    let mut o_collapsed = [false; N];
    let mut ids = Vec::with_capacity(N);
    for _ in 0..N {
        let s = rng.below(b.len() + 1);
        let e = rng.below(b.len() + 1);
        let (s, e) = if s <= e { (s, e) } else { (e, s) };
        let id = store.add(&b, s, e);
        o_start.push(s);
        o_end.push(e);
        ids.push(id);
    }

    // Replay a randomized edit stream on both the buffer and the oracle,
    // asserting store.resolve matches the oracle after every edit.
    const EDITS: usize = 2000;
    let mut text = String::from(src);
    for _ in 0..EDITS {
        let len = b.len();
        let at = rng.below(len + 1);
        let pick = rng.next_u64() % 3;
        // delete/replace need at least 1 byte to remove; at end-of-buffer
        // fall back to an insert (append) to avoid out-of-range spans.
        let pick = if pick != 0 && at >= len { 0 } else { pick };
        match pick {
            0 => {
                let ins = "xy";
                b.insert(at, ins);
                text.insert_str(at, ins);
                let e = edit_insert(at, ins.len());
                for i in 0..N {
                    if o_collapsed[i] {
                        continue;
                    }
                    o_start[i] = transform_offset(o_start[i], &e);
                    o_end[i] = transform_offset(o_end[i], &e);
                }
            }
            1 => {
                let span = 1 + rng.below(4.min(len - at).max(1));
                let end = at + span;
                b.delete(at..end);
                text.replace_range(at..end, "");
                let e = edit_delete(at, end);
                for i in 0..N {
                    if o_collapsed[i] {
                        continue;
                    }
                    // Only collapse if the delete actually consumed an
                    // endpoint (a zero-length anchored range untouched by the
                    // delete stays valid at its point).
                    let s_in = o_start[i] > at && o_start[i] < end;
                    let e_in = o_end[i] > at && o_end[i] < end;
                    o_start[i] = transform_offset(o_start[i], &e);
                    o_end[i] = transform_offset(o_end[i], &e);
                    if (s_in || e_in) && o_start[i] >= o_end[i] {
                        o_collapsed[i] = true;
                    }
                }
            }
            _ => {
                // replace (delete then insert) — exercise both rules.
                let span = 1 + rng.below(4.min(len - at).max(1));
                let end = at + span;
                let ins = "zz";
                b.replace(at..end, ins);
                text.replace_range(at..end, ins);
                let d = edit_delete(at, end);
                let ii = edit_insert(at, ins.len());
                for i in 0..N {
                    if o_collapsed[i] {
                        continue;
                    }
                    let s_in = o_start[i] > at && o_start[i] < end;
                    let e_in = o_end[i] > at && o_end[i] < end;
                    o_start[i] = transform_offset(o_start[i], &d);
                    o_end[i] = transform_offset(o_end[i], &d);
                    if (s_in || e_in) && o_start[i] >= o_end[i] {
                        o_collapsed[i] = true;
                        continue;
                    }
                    o_start[i] = transform_offset(o_start[i], &ii);
                    o_end[i] = transform_offset(o_end[i], &ii);
                }
            }
        }

        store.stabilize(&b);

        // Assert every anchored range against the oracle.
        for i in 0..N {
            let expected = if o_collapsed[i] {
                None
            } else if o_start[i] <= o_end[i] {
                Some(o_start[i]..o_end[i])
            } else {
                Some(o_end[i]..o_start[i])
            };
            let got = store.resolve(&b, ids[i]);
            assert_eq!(
                got, expected,
                "ann {i} diverges after edit at={at} pick={pick} (buffer='{}')",
                text
            );
        }
        // Cross-check the buffer text itself stayed faithful.
        assert_eq!(b.read_range(0..b.len()), text, "buffer text drifted");
    }
}
