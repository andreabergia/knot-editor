//! `TextBuffer`: stable-ID piece-table backing store.
//!
//! Step 4, item 1 of `docs/step4-buffer-plan.md`. This is the bare skeleton:
//! piece table with monotonic stable piece IDs, insert / delete / replace /
//! read_range. Line index and `Position` tokens come in subsequent items.
//!
//! ## Backing store (D1 / D2 / D7)
//!
//! Two immutable backing buffers back the piece chain:
//!
//! - `Original`: the buffer's load-time text. Pieces referencing it never
//!   move; the buffer holds it for the lifetime of the `TextBuffer`.
//! - `Add`: an append-only byte sink. Every inserted span is pushed here and
//!   referenced by a freshly-issued piece. Appending lets old pieces keep
//!   their `(Add, start, len)` references stable forever.
//!
//! A `Piece` is a reference `(backing, start, len)` fixed at creation, plus
//! a monotonically-increasing `PieceId` issued from a global counter. Pieces
//! are never mutated in place except for the split rule's "left retains id"
//! case, which only shortens `len` — never moves `start` or changes `backing`.
//!
//! ## Split rule (D7)
//!
//! Interior edits split a piece; the left half keeps the original id (and
//! original `start`, with `len` shortened to the split offset), the right
//! half gets a fresh id. Degenerate splits (offset 0 or `len`) are elided —
//! the new piece is spliced in directly, no zero-length halves created.
//! Adjacent pieces are never coalesced.
//!
//! ## UTF-8 boundary policy — TEMPORARY
//!
//! The plan defers grapheme clusters to step 7 and operates at byte
//! granularity (D3). For the moment, all edit offsets and inserted-text
//! boundaries are debug-asserted to land on UTF-8 char boundaries, and
//! `read_range` is required to span char-aligned ranges. This is the
//! easiest enforcement for the prototype. TODO: revisit once step 5 / step 7
//! clarifies how non-char-aligned positions (e.g. caret inside a grapheme)
//! should be represented; the choice may need a documented policy in D3.

use std::ops::Range;

/// Monotonically-increasing piece identifier, issued from a global counter.
///
/// Stable across unrelated edits: a piece's id never changes for the
/// piece's lifetime, and is never reused (the counter only goes up).
pub type PieceId = u64;

/// Which backing buffer a piece references.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Backing {
    /// The load-time text. Held immutably for the `TextBuffer`'s lifetime.
    Original,
    /// The append-only edit sink. Appended spans never move.
    Add,
}

/// A reference to a contiguous span of one backing buffer.
///
/// `(backing, start, len)` is fixed at creation. The only field ever
/// mutated after creation is `len`, and only under the D7 split rule
/// (left half retains id, shortened to the split offset).
#[derive(Clone, Copy, Debug)]
struct Piece {
    id: PieceId,
    backing: Backing,
    start: usize,
    len: usize,
}

/// Result of `split_at`: the index in `pieces` at which a new piece should
/// be inserted to land at the requested logical offset.
struct SplitLoc {
    insert_at: usize,
}

/// A hand-rolled piece table with stable piece IDs.
///
/// See module docs for the backing-store and split-rule policies.
pub struct TextBuffer {
    /// Immutable load-time text backing the `Original` pieces.
    original: Box<str>,
    /// Append-only sink for inserted spans. Appended spans are never
    /// overwritten or moved; only new bytes go at the end.
    add: String,
    /// The piece chain. Ownership is exclusive (no sharing across threads,
    /// per D5). Operations splice and shorten in place.
    pieces: Vec<Piece>,
    /// Global monotonic piece-id counter. Never reused.
    next_id: PieceId,
}

impl TextBuffer {
    /// Build a `TextBuffer` from load-time text.
    ///
    /// The text becomes the `Original` backing; an initial single piece
    /// references the whole span. The `Add` sink starts empty. No `\r`
    /// normalization is performed (D4) — the bytes are stored verbatim.
    pub fn from_text(text: impl Into<Box<str>>) -> Self {
        let original = text.into();
        let len = original.len();
        let mut buf = TextBuffer {
            original,
            add: String::new(),
            pieces: Vec::new(),
            next_id: 1,
        };
        if len > 0 {
            buf.pieces.push(Piece {
                id: buf.next_id,
                backing: Backing::Original,
                start: 0,
                len,
            });
            buf.next_id += 1;
        }
        buf
    }

    /// Build an empty buffer.
    pub fn new() -> Self {
        TextBuffer {
            original: Box::from(""),
            add: String::new(),
            pieces: Vec::new(),
            next_id: 1,
        }
    }

    /// Total length of the logical text, in UTF-8 bytes.
    pub fn len(&self) -> usize {
        self.pieces.iter().map(|p| p.len).sum()
    }

    /// Whether the logical text is empty.
    pub fn is_empty(&self) -> bool {
        self.pieces.is_empty() || self.len() == 0
    }

    /// Read a byte range of the logical text into a `String`.
    ///
    /// `range` is inclusive-start / exclusive-end byte offsets into the
    /// logical text. `range.start <= range.end <= len()` is required,
    /// and both ends must land on UTF-8 char boundaries (debug-asserted).
    pub fn read_range(&self, range: Range<usize>) -> String {
        assert!(range.start <= range.end);
        assert!(range.end <= self.len());
        debug_assert!(self.is_char_boundary(range.start));
        debug_assert!(self.is_char_boundary(range.end));

        let mut out = String::with_capacity(range.end - range.start);
        let mut offset = 0usize;
        for p in &self.pieces {
            let p_end = offset + p.len;
            if p_end <= range.start {
                offset = p_end;
                continue;
            }
            if offset >= range.end {
                break;
            }
            // Overlap with [range.start, range.end).
            let slice_start = offset.max(range.start) - offset;
            let slice_end = p_end.min(range.end) - offset;
            let bytes = self.backing_slice(*p);
            out.push_str(&bytes[slice_start..slice_end]);
            offset = p_end;
        }
        out
    }

    /// Insert `text` at byte offset `at`.
    ///
    /// `at` must satisfy `0 <= at <= len()` and land on a UTF-8 char
    /// boundary (debug-asserted). The inserted text itself must be valid
    /// UTF-8 (Rust guarantees this via `&str`).
    pub fn insert(&mut self, at: usize, text: &str) {
        assert!(at <= self.len());
        debug_assert!(self.is_char_boundary(at));

        if text.is_empty() {
            return;
        }

        // Append the span to the Add buffer and record its placement.
        let add_start = self.add.len();
        self.add.push_str(text);

        // Locate the insertion point in the piece chain.
        let split = self.split_at(at);
        let new_piece = Piece {
            id: self.next_id,
            backing: Backing::Add,
            start: add_start,
            len: text.len(),
        };
        self.next_id += 1;
        self.pieces.insert(split.insert_at, new_piece);
    }

    /// Delete the byte range `[start, end)` from the logical text.
    ///
    /// Both ends must land on UTF-8 char boundaries (debug-asserted).
    /// Deleting an empty range is a no-op.
    pub fn delete(&mut self, range: Range<usize>) {
        assert!(range.start <= range.end);
        assert!(range.end <= self.len());
        debug_assert!(self.is_char_boundary(range.start));
        debug_assert!(self.is_char_boundary(range.end));

        if range.is_empty() {
            return;
        }

        // Split the chain at both ends, then drop the pieces in between.
        let left = self.split_at(range.start);
        let right = self.split_at(range.end);

        // The splits guarantee that `left.insert_at` is the index of the
        // first piece strictly at-or-after `range.start`, and
        // `right.insert_at` is the first piece at-or-after `range.end`.
        // Anything in `[left.insert_at, right.insert_at)` lies inside the
        // deleted span and is removed wholesale. The right-edge split may
        // have shortened a piece to the post-end remainder; that remainder
        // sits at `right.insert_at` and survives the drain.
        self.pieces.drain(left.insert_at..right.insert_at);
    }

    /// Replace `[range.start, range.end)` with `text`.
    ///
    /// Implemented as delete-then-insert; `range` and `at` follow the same
    /// UTF-8 boundary rules as those operations. Replacing an empty range is
    /// equivalent to `insert(range.start, text)`.
    pub fn replace(&mut self, range: Range<usize>, text: &str) {
        assert!(range.start <= range.end);
        assert!(range.end <= self.len());
        debug_assert!(self.is_char_boundary(range.start));
        debug_assert!(self.is_char_boundary(range.end));

        if range.is_empty() {
            self.insert(range.start, text);
            return;
        }
        self.delete(range.clone());
        self.insert(range.start, text);
    }

    // ---- Internals -------------------------------------------------------

    /// Backing slice for a piece. Lifetime ties to `self`.
    fn backing_slice(&self, p: Piece) -> &str {
        let buf = match p.backing {
            Backing::Original => self.original.as_ref(),
            Backing::Add => self.add.as_str(),
        };
        &buf[p.start..p.start + p.len]
    }

    /// True iff `offset` is a UTF-8 char boundary of the logical text.
    fn is_char_boundary(&self, offset: usize) -> bool {
        // Walk the pieces and stitch a logical view; reading a full char
        // would be wasteful, so we reconstruct boundary-ness by inspecting
        // the byte at the piece boundary. Cheap path: the offset falls
        // strictly inside a piece (not at its edges) — defer to that
        // piece's backing, which is valid UTF-8.
        // Cheap path 2: offset is at the very start or end of the buffer.
        if offset == 0 || offset == self.len() {
            return true;
        }
        let mut acc = 0usize;
        for p in &self.pieces {
            if offset == acc {
                // At the start of this piece; valid iff the byte before
                // (last byte of the previous logical position) is not a
                // UTF-8 continuation. Walk back is unnecessary: piece
                // boundaries are always char boundaries by construction
                // (splits only happen at char boundaries via debug-asserts).
                return true;
            }
            if offset > acc && offset < acc + p.len {
                let bytes = self.backing_slice(*p);
                return bytes.is_char_boundary(offset - acc);
            }
            acc += p.len;
        }
        false
    }

    /// Split the piece chain at logical offset `at`, returning the index
    /// at which a new piece should be inserted.
    ///
    /// `at` must satisfy `0 <= at <= len()`.
    ///
    /// Degenerate splits — `at` at the start or end of an existing piece —
    /// are elided per D7: the new piece is spliced in directly, no
    /// zero-length halves created. Otherwise the containing piece is split,
    /// the left half retains its id (with `len` shortened to the split
    /// offset) and the right half is inserted as a fresh piece behind it.
    fn split_at(&mut self, at: usize) -> SplitLoc {
        // Empty buffer or insertion at the very end.
        if at == self.len() {
            return SplitLoc {
                insert_at: self.pieces.len(),
            };
        }
        if at == 0 {
            return SplitLoc { insert_at: 0 };
        }

        let mut acc = 0usize;
        for (i, p) in self.pieces.iter_mut().enumerate() {
            let p_end = acc + p.len;
            if at == acc {
                // Insertion at the start of piece `i`: elide; no split.
                return SplitLoc { insert_at: i };
            }
            if at == p_end {
                // Insertion at the end of piece `i`: elide; insert before
                // the next piece (or at the tail if `i` is last).
                return SplitLoc { insert_at: i + 1 };
            }
            if at > acc && at < p_end {
                // Strict interior split — D7's only non-degenerate case.
                let left_len = at - acc;
                let right_len = p.len - left_len;
                let right_start = p.start + left_len;
                let backing = p.backing;

                // Shorten the left half in place (retains id, start, backing).
                p.len = left_len;

                // Append the right half as a fresh piece after index `i`.
                let right_id = self.next_id;
                self.next_id += 1;
                let insert_at = i + 1;
                self.pieces.insert(
                    insert_at,
                    Piece {
                        id: right_id,
                        backing,
                        start: right_start,
                        len: right_len,
                    },
                );
                return SplitLoc { insert_at };
            }
            acc = p_end;
        }
        // `at` was beyond `self.len()` — caller's `assert!` should have
        // caught this; reaching here is a bug.
        unreachable!("split_at({at}) past end of len {}", self.len())
    }
}

impl Default for TextBuffer {
    fn default() -> Self {
        Self::new()
    }
}

// ----------------------------------------------------------------------
// Unit tests
// ----------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn s(b: &TextBuffer) -> String {
        b.read_range(0..b.len())
    }

    #[test]
    fn empty_buffer() {
        let b = TextBuffer::new();
        assert!(b.is_empty());
        assert_eq!(b.len(), 0);
        assert_eq!(s(&b), "");
        assert_eq!(b.read_range(0..0), "");
    }

    #[test]
    fn from_text_round_trips() {
        let b = TextBuffer::from_text("hello world");
        assert_eq!(b.len(), 11);
        assert_eq!(s(&b), "hello world");
        assert_eq!(b.read_range(0..5), "hello");
        assert_eq!(b.read_range(6..11), "world");
        assert_eq!(b.read_range(3..8), "lo wo");
    }

    #[test]
    fn read_range_empty() {
        let b = TextBuffer::from_text("abc");
        assert_eq!(b.read_range(0..0), "");
        assert_eq!(b.read_range(1..1), "");
        assert_eq!(b.read_range(3..3), "");
    }

    #[test]
    fn insert_at_start() {
        let mut b = TextBuffer::from_text("world");
        b.insert(0, "hello ");
        assert_eq!(s(&b), "hello world");
    }

    #[test]
    fn insert_at_end() {
        let mut b = TextBuffer::from_text("hello");
        b.insert(5, " world");
        assert_eq!(s(&b), "hello world");
    }

    #[test]
    fn insert_at_interior() {
        let mut b = TextBuffer::from_text("hello world");
        b.insert(5, ",");
        assert_eq!(s(&b), "hello, world");
    }

    #[test]
    fn insert_empty_is_noop() {
        let mut b = TextBuffer::from_text("hello");
        b.insert(2, "");
        assert_eq!(s(&b), "hello");
        assert_eq!(b.len(), 5);
    }

    #[test]
    fn insert_into_empty_buffer() {
        let mut b = TextBuffer::new();
        b.insert(0, "first");
        assert_eq!(s(&b), "first");
        b.insert(0, "second ");
        assert_eq!(s(&b), "second first");
        b.insert(b.len(), " last");
        assert_eq!(s(&b), "second first last");
    }

    #[test]
    fn delete_whole_piece() {
        let mut b = TextBuffer::from_text("hello world");
        b.delete(6..11);
        assert_eq!(s(&b), "hello ");
        assert_eq!(b.len(), 6);
    }

    #[test]
    fn delete_interior_split_left() {
        // "hello world" (len 11): delete indices 2..8 removes "llo wo",
        // leaving "he" + "rld" = "herld".
        let mut b = TextBuffer::from_text("hello world");
        b.delete(2..8);
        assert_eq!(s(&b), "herld");
    }

    #[test]
    fn delete_start_boundary() {
        let mut b = TextBuffer::from_text("hello world");
        b.delete(0..6);
        assert_eq!(s(&b), "world");
    }

    #[test]
    fn delete_empty_is_noop() {
        let mut b = TextBuffer::from_text("hello");
        b.delete(2..2);
        assert_eq!(s(&b), "hello");
    }

    #[test]
    fn delete_entire_buffer() {
        let mut b = TextBuffer::from_text("abc");
        b.delete(0..3);
        assert!(b.is_empty());
        assert_eq!(s(&b), "");
    }

    #[test]
    fn replace_basic() {
        let mut b = TextBuffer::from_text("hello world");
        b.replace(0..5, "goodbye");
        assert_eq!(s(&b), "goodbye world");
    }

    #[test]
    fn replace_empty_range_inserts() {
        let mut b = TextBuffer::from_text("hello world");
        b.replace(5..5, ",");
        assert_eq!(s(&b), "hello, world");
    }

    #[test]
    fn replace_with_empty_deletes() {
        let mut b = TextBuffer::from_text("hello, world");
        b.replace(5..6, "");
        assert_eq!(s(&b), "hello world");
    }

    #[test]
    fn repeated_inserts_preserve_text() {
        let mut b = TextBuffer::from_text("abc");
        for (i, ch) in "def".chars().enumerate() {
            b.insert(3 + i, &ch.to_string());
        }
        assert_eq!(s(&b), "abcdef");
    }

    #[test]
    fn interleaved_insert_delete() {
        let mut b = TextBuffer::from_text("the quick brown fox");
        b.insert(0, "ADDED ");
        assert_eq!(s(&b), "ADDED the quick brown fox");
        b.delete(0..6);
        assert_eq!(s(&b), "the quick brown fox");
        b.replace(4..9, "slow");
        assert_eq!(s(&b), "the slow brown fox");
    }

    #[test]
    fn multi_byte_utf8() {
        let mut b = TextBuffer::from_text("héllo"); // é is 2 bytes
        assert_eq!(b.len(), 6); // h(1)+é(2)+l(1)+l(1)+o(1)
        assert_eq!(s(&b), "héllo");
        b.insert(1, "X"); // insert at byte 1, before é
        assert_eq!(s(&b), "hXéllo");
        b.insert(4, "Y"); // insert between é and l (byte 4 = after é)
        assert_eq!(s(&b), "hXéYllo");
    }

    #[test]
    fn read_range_multibyte() {
        let b = TextBuffer::from_text("αβγδε"); // 5 chars, 10 bytes
        assert_eq!(b.len(), 10);
        assert_eq!(b.read_range(0..2), "α");
        assert_eq!(b.read_range(2..4), "β");
        assert_eq!(b.read_range(0..10), "αβγδε");
        assert_eq!(b.read_range(2..6), "βγ");
    }

    #[test]
    fn delete_multibyte() {
        let mut b = TextBuffer::from_text("αβγδε");
        b.delete(2..4); // delete β
        assert_eq!(s(&b), "αγδε");
        assert_eq!(b.len(), 8);
    }

    #[test]
    fn piece_chain_grows_under_interior_edits() {
        // Multiple interior inserts force several splits; the chain grows
        // but the logical text stays correct.
        let mut b = TextBuffer::from_text("aaaa");
        b.insert(1, "b"); // "abaaa"
        b.insert(3, "c"); // "abacaa"
        b.insert(5, "d"); // "abacada"
        b.insert(7, "e"); // at end == len, append -> "abacadae"
        let mut e = String::from("aaaa");
        e.insert_str(1, "b"); // "abaaa"
        e.insert_str(3, "c"); // "abacaa"
        e.insert_str(5, "d"); // "abacada"
        e.insert_str(7, "e"); // "abacadae"
        assert_eq!(s(&b), e);
        assert_eq!(b.len(), 8);
        assert!(b.pieces.len() > 1);
    }

    #[test]
    fn piece_ids_are_monotonic() {
        let mut b = TextBuffer::from_text("hello");
        let id0 = b.pieces[0].id;
        b.insert(2, "X");
        // The original split: left half retains id0, right half gets a
        // higher id; the inserted piece gets an even higher id.
        let ids: Vec<PieceId> = b.pieces.iter().map(|p| p.id).collect();
        assert!(ids.iter().all(|&id| id >= id0));
        // All ids distinct (counter never reuses).
        let mut sorted = ids.clone();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(sorted.len(), ids.len());
    }

    #[test]
    fn left_half_retains_id_on_split() {
        let mut b = TextBuffer::from_text("hello");
        let orig_id = b.pieces[0].id;
        b.insert(2, "X");
        // First piece is the left half: same id, shortened to len 2.
        assert_eq!(b.pieces[0].id, orig_id);
        assert_eq!(b.pieces[0].len, 2);
        assert_eq!(b.backing_slice(b.pieces[0]), "he");
    }

    #[test]
    fn boundary_insert_elides_split() {
        let mut b = TextBuffer::from_text("hello");
        b.insert(0, "X");
        // No new piece from splitting — original piece intact, inserted piece
        // spliced in front.
        assert_eq!(b.pieces.len(), 2);
        assert_eq!(s(&b), "Xhello");

        let mut b = TextBuffer::from_text("hello");
        b.insert(5, "X");
        assert_eq!(b.pieces.len(), 2);
        assert_eq!(s(&b), "helloX");
    }

    #[test]
    fn delete_crossing_multiple_pieces() {
        // Build a multi-piece buffer then delete across piece boundaries.
        let mut b = TextBuffer::from_text("abcdefghij");
        b.insert(3, "1"); // "abc1defghij"
        b.insert(7, "2"); // "abc1def2ghij"
        assert_eq!(s(&b), "abc1def2ghij");
        // Logical: a b c 1 d e f 2 g h i j
        //          0 1 2 3 4 5 6 7 8 9 ...
        // range 2..9 removes "c1def2g", leaving "ab" + "hij".
        b.delete(2..9);
        assert_eq!(s(&b), "abhij");
    }
}