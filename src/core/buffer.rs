//! `TextBuffer`: stable-ID piece-table backing store.
//!
//! Steps 4.1 and 4.2 of `docs/step4-buffer-plan.md`: a piece table with
//! monotonic stable piece IDs (insert / delete / replace / read_range)
//! plus a lazy, incrementally-maintained line index (D4).
//! `Position` tokens and the `BufferEdit` log come in subsequent items.
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
    /// Lazy line index per D4:
    /// - `None` — not yet built. Built on first query that needs it.
    /// - `Some(vec)` — kept valid under edits via incremental update
    ///   (`update_line_starts`), never rebuilt wholesale.
    ///
    /// `vec` is sorted ascending. `vec[0] == 0` always (line 0 starts at
    /// byte offset 0); even an empty buffer stores `[0]`. A trailing `'\n'`
    /// creates one extra empty line, so `"abc\n"` has `vec == [0, 4]`.
    line_starts: Option<Vec<usize>>,
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
            line_starts: None,
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
            line_starts: None,
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

        // Keep the line index valid for the edited span.
        self.update_line_starts(at..at, text);
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

        // Keep the line index valid for the edited span.
        self.update_line_starts(range, "");
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

    // ---- Line index (D4) ------------------------------------------------

    /// Number of lines. The buffer always has at least one line (line 0,
    /// possibly empty), so this returns `>= 1`. Builds the line index
    /// lazily on first call.
    pub fn line_count(&mut self) -> usize {
        self.ensure_line_starts();
        self.line_starts.as_ref().unwrap().len()
    }

    /// Byte offset of the start of `line` (0-indexed). `line` must be
    /// `< line_count()`. Builds the line index lazily on first call.
    pub fn line_start(&mut self, line: usize) -> usize {
        self.ensure_line_starts();
        let starts = self.line_starts.as_ref().unwrap();
        assert!(line < starts.len(), "line {line} out of range ({})", starts.len());
        starts[line]
    }

    /// 0-indexed line number of the line containing byte offset `offset`.
    /// `offset` must satisfy `0 <= offset <= len()`. `len()` maps to the
    /// last line. Builds the line index lazily on first call.
    pub fn line_of_offset(&mut self, offset: usize) -> usize {
        assert!(offset <= self.len());
        self.ensure_line_starts();
        let starts = self.line_starts.as_ref().unwrap();
        // First index where `starts[i] > offset`; the line containing
        // `offset` is the one before it. `saturating_sub` covers `offset`
        // landing on or before the first entry (always 0 here).
        starts.partition_point(|&s| s <= offset).saturating_sub(1)
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

    /// Build the line index from scratch by scanning all piece content for
    /// `'\n'` bytes. O(total length). Called once per buffer lifetime, on
    /// the first line-index query; subsequent edits keep it valid via
    /// `update_line_starts`.
    fn ensure_line_starts(&mut self) {
        if self.line_starts.is_some() {
            return;
        }
        let mut starts: Vec<usize> = vec![0];
        let mut acc = 0usize;
        for p in &self.pieces {
            let bytes = self.backing_slice(*p).as_bytes();
            for (i, &b) in bytes.iter().enumerate() {
                if b == b'\n' {
                    starts.push(acc + i + 1);
                }
            }
            acc += p.len;
        }
        self.line_starts = Some(starts);
    }

    /// Incrementally update the line index after an edit that replaced the
    /// byte span `[range.start, range.end)` with `new_text` (whose byte
    /// length is `new_text.len()`).
    ///
    /// No-op if the line index isn't built yet (lazy: edits don't force a
    /// build). Otherwise splices the K-entry delta for the edited span and
    /// fixes up the surviving suffix in O(n−i) — see the implementation
    /// note in `docs/step4-buffer-plan.md`.
    ///
    /// Algorithm:
    /// - `delta = new_text.len() - (range.end - range.start)` (signed).
    /// - `lo`  = first index where `starts[i] >= range.start` → kept
    ///   unchanged.
    /// - `hi`  = first index where `starts[i] > range.end` → suffix to
    ///   shift by `delta`.
    /// - Middle section (replaces `starts[lo..hi]`):
    ///   1. If `starts[lo] == range.start`, the `'\n'` at `range.start - 1`
    ///      survives untouched, so the line at `range.start` survives at
    ///      the same byte offset — re-emit `range.start`.
    ///   2. For each `'\n'` at index `i` in `new_text`, emit a new line
    ///      start at `range.start + i + 1`.
    /// - The new array is `[starts[..lo], middle, [s + delta for s in starts[hi..]]]`.
    fn update_line_starts(&mut self, range: Range<usize>, new_text: &str) {
        let Some(starts) = self.line_starts.as_mut() else {
            return;
        };

        let delta: isize =
            new_text.len() as isize - (range.end - range.start) as isize;
        let lo = starts.partition_point(|&s| s < range.start);
        let hi = starts.partition_point(|&s| s <= range.end);

        // Build the middle section: surviving line at `range.start` (if any)
        // plus new line starts from `'\n'`s in `new_text`.
        let mut middle: Vec<usize> = Vec::new();
        if lo < starts.len() && starts[lo] == range.start {
            middle.push(range.start);
        }
        for (i, &b) in new_text.as_bytes().iter().enumerate() {
            if b == b'\n' {
                middle.push(range.start + i + 1);
            }
        }

        // Assemble the new line index.
        let mut new_starts: Vec<usize> =
            Vec::with_capacity(lo + middle.len() + (starts.len() - hi));
        new_starts.extend_from_slice(&starts[..lo]);
        new_starts.extend(middle);
        for &s in &starts[hi..] {
            new_starts.push((s as isize + delta) as usize);
        }
        *starts = new_starts;
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

    // ---- Line index (D4) ----------------------------------------------

    /// Peek the line-index state directly (assumes built).
    fn ls(b: &TextBuffer) -> &[usize] {
        b.line_starts.as_ref().expect("line_starts built")
    }

    #[test]
    fn line_index_empty_buffer() {
        let mut b = TextBuffer::new();
        assert_eq!(b.line_count(), 1);
        assert_eq!(b.line_start(0), 0);
        assert_eq!(b.line_of_offset(0), 0);
        assert_eq!(ls(&b), &[0]);
    }

    #[test]
    fn line_index_single_line() {
        let mut b = TextBuffer::from_text("hello");
        assert_eq!(b.line_count(), 1);
        assert_eq!(b.line_start(0), 0);
        for off in 0..=b.len() {
            assert_eq!(b.line_of_offset(off), 0, "offset {off}");
        }
        assert_eq!(ls(&b), &[0]);
    }

    #[test]
    fn line_index_multi_line() {
        let mut b = TextBuffer::from_text("ab\ncd\nef");
        // Bytes: a(0) b(1) \n(2) c(3) d(4) \n(5) e(6) f(7). len 8.
        assert_eq!(b.line_count(), 3);
        assert_eq!(b.line_start(0), 0);
        assert_eq!(b.line_start(1), 3);
        assert_eq!(b.line_start(2), 6);
        // Offset → line mappings.
        assert_eq!(b.line_of_offset(0), 0);
        assert_eq!(b.line_of_offset(2), 0); // the '\n' belongs to line 0
        assert_eq!(b.line_of_offset(3), 1); // first byte of line 1
        assert_eq!(b.line_of_offset(5), 1); // trailing '\n' of line 1
        assert_eq!(b.line_of_offset(6), 2);
        assert_eq!(b.line_of_offset(8), 2); // one-past-end maps to last line
        assert_eq!(ls(&b), &[0, 3, 6]);
    }

    #[test]
    fn line_index_trailing_newline_creates_empty_last_line() {
        let mut b = TextBuffer::from_text("abc\n");
        assert_eq!(b.line_count(), 2);
        assert_eq!(b.line_start(0), 0);
        assert_eq!(b.line_start(1), 4);
        assert_eq!(ls(&b), &[0, 4]);
    }

    #[test]
    fn line_index_lazy_until_first_query() {
        // Edits before first query don't build the index; they leave it None.
        let mut b = TextBuffer::from_text("abc");
        b.insert(3, "def");
        b.delete(0..1);
        assert!(b.line_starts.is_none(), "should still be lazy after edits");
        // First query triggers the build.
        assert_eq!(b.line_count(), 1);
        assert!(b.line_starts.is_some());
    }

    #[test]
    fn line_index_insert_with_newlines_interior() {
        let mut b = TextBuffer::from_text("abcdef"); // starts [0]
        b.insert(3, "X\nY");
        // "abcX\nYdef": a(0) b(1) c(2) X(3) \n(4) Y(5) d(6) e(7) f(8). len 9.
        assert_eq!(s(&b), "abcX\nYdef");
        assert_eq!(b.line_count(), 2);
        assert_eq!(b.line_start(0), 0);
        assert_eq!(b.line_start(1), 5);
        assert_eq!(ls(&b), &[0, 5]);
    }

    #[test]
    fn line_index_insert_at_existing_line_start() {
        // Inserting right at a line start must preserve that line start.
        let mut b = TextBuffer::from_text("ab\ncd"); // starts [0, 3]
        b.insert(3, "x"); // "ab\nxcd"
        assert_eq!(s(&b), "ab\nxcd");
        assert_eq!(b.line_count(), 2);
        assert_eq!(b.line_start(0), 0);
        assert_eq!(b.line_start(1), 3);
        assert_eq!(ls(&b), &[0, 3]);
    }

    #[test]
    fn line_index_insert_newline_at_offset_zero() {
        let mut b = TextBuffer::from_text("abc\ndef\nghi"); // [0, 4, 8]
        b.insert(0, "\n");
        // "\nabc\ndef\nghi" → \n(0) a(1) b(2) c(3) \n(4) d(5) e(6) f(7) \n(8) g(9) h(10) i(11)
        assert_eq!(s(&b), "\nabc\ndef\nghi");
        assert_eq!(b.line_count(), 4);
        assert_eq!(ls(&b), &[0, 1, 5, 9]);
    }

    #[test]
    fn line_index_delete_removes_newline_in_span() {
        let mut b = TextBuffer::from_text("abc\ndef\nghi"); // [0, 4, 8]
        // Delete "\ndef" (offsets 3..8), collapse to one line "abcghi"
        b.delete(3..8);
        assert_eq!(s(&b), "abcghi");
        assert_eq!(b.line_count(), 1);
        assert_eq!(ls(&b), &[0]);
    }

    #[test]
    fn line_index_delete_to_end_keeps_trailing_empty_line() {
        // "ab\ncd" → delete [3, 5) (deletes "cd"), leaving "ab\n".
        // The trailing '\n' demarcates an empty final line.
        let mut b = TextBuffer::from_text("ab\ncd"); // [0, 3]
        b.delete(3..5);
        assert_eq!(s(&b), "ab\n");
        assert_eq!(b.line_count(), 2);
        assert_eq!(ls(&b), &[0, 3]);
    }

    #[test]
    fn line_index_delete_entire_buffer() {
        let mut b = TextBuffer::from_text("ab\ncd\nef"); // [0, 3, 6]
        b.delete(0..8);
        assert_eq!(s(&b), "");
        assert!(b.is_empty());
        assert_eq!(b.line_count(), 1);
        assert_eq!(ls(&b), &[0]);
    }

    #[test]
    fn line_index_delete_inside_one_line_no_change_to_index() {
        let mut b = TextBuffer::from_text("ab\ncd\nef"); // [0, 3, 6]
        b.delete(4..5); // delete 'd' from line 1 → "ab\nc\nef"
        assert_eq!(s(&b), "ab\nc\nef");
        assert_eq!(b.line_count(), 3);
        assert_eq!(ls(&b), &[0, 3, 5]); // line 2 shifted by -1
    }

    #[test]
    fn line_index_replace_with_multiline_text() {
        let mut b = TextBuffer::from_text("abc\ndef\nghi"); // [0, 4, 8]
        // Replace "\ndef" → "X\nY": result "abcX\nYghi", starts [0, 5]
        b.replace(3..8, "X\nY");
        assert_eq!(s(&b), "abcX\nYghi");
        assert_eq!(b.line_count(), 2);
        assert_eq!(ls(&b), &[0, 5]);
    }

    #[test]
    fn line_index_replace_collapsing_lines() {
        let mut b = TextBuffer::from_text("abc\ndef\nghi"); // [0, 4, 8]
        // Replace "\ndef\n" (offsets 3..8) with "": result "abcghi", 1 line.
        b.replace(3..8, "");
        assert_eq!(s(&b), "abcghi");
        assert_eq!(b.line_count(), 1);
        assert_eq!(ls(&b), &[0]);
    }

    #[test]
    fn line_index_replace_inserts_new_lines_at_start() {
        let mut b = TextBuffer::from_text("abc"); // [0]
        // Replace whole buffer with multi-line content.
        b.replace(0..3, "ab\ncd\nef");
        assert_eq!(s(&b), "ab\ncd\nef");
        assert_eq!(b.line_count(), 3);
        assert_eq!(ls(&b), &[0, 3, 6]);
    }

    #[test]
    fn line_index_append_newline_at_end() {
        let mut b = TextBuffer::from_text("abc"); // [0]
        b.insert(3, "\n"); // "abc\n" → [0, 4]
        assert_eq!(b.line_count(), 2);
        assert_eq!(ls(&b), &[0, 4]);
    }

    #[test]
    fn line_index_survives_many_random_edits() {
        // Stress: build a buffer, do many inserts/deletes of '\n'-bearing
        // spans, and verify the index matches a fresh scan of the text.
        let text = "the quick brown fox\njumps over\nthe lazy dog\n";
        let mut b = TextBuffer::from_text(text);
        // Force a build so subsequent edits go through the incremental path.
        let _ = b.line_count();

        let mut rng_state: u64 = 0xdead_beef;
        let mut next_rand = || {
            // xorshift64
            rng_state ^= rng_state << 13;
            rng_state ^= rng_state >> 7;
            rng_state ^= rng_state << 17;
            rng_state
        };
        for _ in 0..200 {
            let len = b.len();
            let start = (next_rand() % (len as u64 + 1)) as usize;
            let end = (next_rand() % (len as u64 + 1)) as usize;
            let (start, end) = if start <= end { (start, end) } else { (end, start) };
            // Pick one of a few candidate replacements.
            let pick = next_rand() % 4;
            let new_text = match pick {
                0 => "",
                1 => "x",
                2 => "\n",
                _ => "ab\ncd",
            };
            // Only apply if both ends are char boundaries (we use ASCII
            // source text + ASCII inserts, so every offset is a boundary).
            b.replace(start..end, new_text);
        }

        // Re-derive expected line starts from the final text.
        let final_text = s(&b);
        let mut expected: Vec<usize> = vec![0];
        for (i, byte) in final_text.as_bytes().iter().enumerate() {
            if *byte == b'\n' {
                expected.push(i + 1);
            }
        }
        assert_eq!(b.line_count(), expected.len(), "line count mismatch");
        assert_eq!(ls(&b), &expected[..], "line starts mismatch");
    }
}