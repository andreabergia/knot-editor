//! `TextBuffer`: stable-ID piece-table backing store.
//!
//! Steps 4.1—4.3 of `docs/step4-buffer-plan.md`: a piece table with
//! monotonic stable piece IDs (insert / delete / replace / read_range),
//! a lazy incrementally-maintained line index (D4), and the
//! `Position` / `BufferEdit` surface that step 5 will subscribe to.
//!
//! ## Position tokens (D2 / D7)
//!
//! A `Position` is an opaque `(PieceId, u32 offset_within_piece)` pair.
//! It is stable across *unrelated* edits: an edit elsewhere in the
//! buffer does not invalidate it. The only failure modes are:
//!
//! - The piece was deleted wholesale → `resolve` returns `None`.
//! - The position sat in the right half of an interior split (D7) and
//!   the underlying piece has since been shortened → `resolve` returns
//!   `None`, but the content the position pointed to is recoverable
//!   from the matching `Split` record in the edit log: the new location
//!   is `(Split::new_piece, old_offset - Split::split_offset)`.
//!
//! Default stickiness is *sticky-left*: a `position_at` query at a piece
//! boundary returns the piece ending at that offset (with `offset ==
//! piece.len`), so a subsequent insert at the boundary leaves the
//! position pointing at the same byte it pointed at before. Sticky-right
//! is step 5's job — it reads the edit log and relocates.
//!
//! ## Edit log (D7)
//!
//! Every `insert` / `delete` (and `replace`, which decomposes into a
//! delete-then-insert pair) pushes one `BufferEdit` event onto an
//! unbounded `Vec<BufferEdit>`. Fields are byte offsets into the
//! *pre-edit* logical text, so an observer replaying stale `Position`s
//! can reason about each edit against the buffer state *in which the
//! position was issued*. Compaction is deferred — the log only grows,
//! bounded only by a future `take_edits` drain.
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

/// Opaque, stable token for a location in the logical text (D2 / D7).
///
/// `(piece, offset_within_piece)`. Issued by `TextBuffer::position_at`,
/// resolved back to a byte offset by `TextBuffer::resolve`. The token
/// is stable across unrelated edits; failure modes are documented on
/// the module and on `resolve`.
///
/// Field accessors are exposed (rather than the struct being fully
/// opaque) because step 5 needs to *store* tokens keyed by piece id and
/// to apply the deterministic remap described in `Split`; the fields
/// are immutable and the type is `Copy`, so leaking them is safe.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Position {
    piece: PieceId,
    offset: u32,
}

impl Position {
    /// The piece this token is anchored to.
    pub fn piece(&self) -> PieceId {
        self.piece
    }

    /// The offset within `piece`, in UTF-8 bytes. Satisfies `<= piece.len`
    /// for a *valid* token; may exceed it for a *stale* token issued
    /// before the piece was split.
    pub fn offset(&self) -> u32 {
        self.offset
    }

    /// Construct a token from raw `(piece, offset)` fields. Exposed so
    /// callers (step 5's annotation layer, the benchmark's remap
    /// simulation) can build the *remapped* token produced by applying
    /// the D7 rule to a `Split` record — `(Split::new_piece,
    /// old_offset - Split::split_offset)`. Step 4 itself never uses
    /// this; the only in-crate producer of tokens is `position_at`.
    pub fn new(piece: PieceId, offset: u32) -> Self {
        Self { piece, offset }
    }
}

/// One half of an interior split (D7).
///
/// A `Position` whose `piece` is `old_piece` and whose offset is
/// `> split_offset` is *stale but detectable*: its content has moved to
/// `new_piece` at offset `old_offset - split_offset`. Step 5 reads the
/// edit log and applies this transformation; step 4 itself never
/// touches its callers' tokens.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Split {
    /// The piece that was shortened by the split (retains its id).
    pub old_piece: PieceId,
    /// The split point within that piece, equal to its post-split length.
    pub split_offset: usize,
    /// The freshly-issued id of the right (longer-offset) half.
    pub new_piece: PieceId,
}

/// Per-edit event in the `TextBuffer`'s edit log (D7).
///
/// `at` and `range` are byte offsets into the buffer's *pre-edit*
/// logical text, so an observer replaying stale `Position`s can reason
/// about each edit against the buffer state in which the tokens were
/// issued. `splits` is normally length 0 (the edit landed on a piece
/// boundary and was elided) or 1 (one interior split); a `delete`
/// spanning two interiors may carry up to 2.
///
/// In addition to the splits, each variant carries enough
/// affected-piece metadata for step 5's annotation store to repair
/// stale tokens in `O(affected)` time per edit (D7),
/// without walking the whole piece chain: the annotation store keys
/// anchors by `piece_id`, looks up only the pieces this edit touches,
/// and re-binds them via these fields.
#[derive(Clone, Debug)]
pub enum BufferEdit {
    /// Bytes were inserted at `at`; the inserted span has byte length
    /// `inserted_len`. Splits describe any interior piece splits the
    /// edit forced (at most one, at `at`).
    Insert {
        at: usize,
        inserted_len: usize,
        /// Id of the freshly-inserted piece (the new piece added to
        /// the chain at `split.insert_at`). Sticky-`After` anchors
        /// sitting on a boundary-elided insert relocate to
        /// `(inserted_piece, inserted_len)`.
        inserted_piece: PieceId,
        /// The piece ending at logical offset `at` *pre-insert*, and
        /// the offset-within-that-piece corresponding to the boundary
        /// `at`. `None` iff `at == 0` (no piece to the left of the
        /// buffer start). A sticky-`After` anchor whose token is
        /// `(left_piece.id, left_piece_offset)` (i.e. sitting exactly
        /// at the boundary) relocates to `(inserted_piece, inserted_len)`;
        /// a sticky-`Before` anchor keeps its token and resolves to
        /// `at` post-edit, matching D2 "Before → unchanged".
        ///
        /// For an interior-split insert, `left_piece` is
        /// `(splits[0].old_piece, splits[0].split_offset)` (the
        /// original piece, now shortened to the split offset) —
        /// equivalent boundary semantics, already covered by the split
        /// remap rule. Stabilize only consults `left_piece` when
        /// `splits` is empty (the boundary-elided case).
        left_piece: Option<(PieceId, usize)>,
        /// The `pieces[0]` id *pre-insert*. Used only when `at == 0`
        /// to relocate sticky-`Before` anchors anchored to the buffer's
        /// first piece at offset 0 — those would otherwise resolve to
        /// `n` post-edit (chain shift), but D2 strict requires them to
        /// stay at `0`. Relocate to `(inserted_piece, 0)`.
        pre_first_piece: Option<PieceId>,
        splits: Vec<Split>,
    },
    /// Bytes in `range` were removed. Splits describe any interior
    /// piece splits the edit forced (up to two, at `range.start` and
    /// `range.end`).
    Delete {
        range: Range<usize>,
        /// Piece ids drained by this delete (the contents of
        /// `[left.insert_at, right.insert_at)` post-both-splits,
        /// pre-drain). Anchors whose token.piece is in this set were
        /// inside the deleted span and are re-bound to a surviving
        /// edge piece below.
        deleted_pieces: Vec<PieceId>,
        /// `(piece_id, post-split_len)` for the drained pieces, in chain
        /// order. Step 5 uses this to tell true interior endpoints from
        /// sticky-left tokens exactly at the delete's start/end boundary
        /// when a boundary split was elided.
        deleted_piece_lens: Vec<(PieceId, usize)>,
        /// The piece ending at logical `range.start` (= `s`) post-edit
        /// and its post-edit length. `None` iff `range.start == 0`
        /// (no piece to the left of the delete). Sticky-`Before`
        /// anchors inside `(s, e)` snap to `(left_survivor.0,
        /// left_survivor.1)`, which resolves to `s` post-edit.
        left_survivor: Option<(PieceId, usize)>,
        /// The piece starting at logical `range.start` (= `e` pre-edit,
        /// shifted to `s` post-edit) post-delete. `None` iff the buffer
        /// is empty after the delete. Sticky-`After` anchors inside
        /// `(s, e)` snap to `(right_survivor, 0)`, which resolves to
        /// `s` post-edit.
        right_survivor: Option<PieceId>,
        splits: Vec<Split>,
    },
}

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
    /// Append-only edit log (D7). Each `insert` / `delete` pushes one
    /// `BufferEdit`. `replace` decomposes into a delete-then-insert pair
    /// and pushes two. The log is unbounded; compaction is deferred
    /// behind a benchmark number and `take_edits`.
    edits: Vec<BufferEdit>,
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
            edits: Vec::new(),
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
            edits: Vec::new(),
        }
    }

    /// Total length of the logical text, in UTF-8 bytes.
    pub fn len(&self) -> usize {
        self.pieces.iter().map(|p| p.len).sum()
    }

    /// Number of pieces currently in the piece chain. Diagnostic only:
    /// the chain grows under interior edits and is never coalesced (D7);
    /// this count is exposed so benchmarks can report table size without
    /// a separate internal probe. Not load-bearing for any operation.
    pub fn piece_count(&self) -> usize {
        self.pieces.len()
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

        // Locate the insertion point in the piece chain. Splits are
        // recorded so they can be attached to the BufferEdit event.
        let pre_first_piece = self.pieces.first().map(|p| p.id);
        let mut splits = Vec::new();
        let split = self.split_at(at, &mut splits);

        // Compute the piece ending at logical `at` pre-insert (and the
        // offset-within-that-piece corresponding to the boundary `at`).
        // For interior-split inserts this is `(splits[0].old_piece,
        // splits[0].split_offset)` (post-split, the old_piece is
        // shortened to the split offset); stabilize relies only on the
        // splits list in that case, so this is dormant but kept
        // consistent. For boundary-elided inserts (no splits) the
        // piece chain is unchanged and `pieces[split.insert_at - 1]`
        // is the piece ending at `at`.
        let left_piece = if at == 0 {
            None
        } else {
            let p = self.pieces[split.insert_at - 1];
            Some((p.id, p.len))
        };

        let inserted_piece = self.next_id;
        let new_piece = Piece {
            id: inserted_piece,
            backing: Backing::Add,
            start: add_start,
            len: text.len(),
        };
        self.next_id += 1;
        self.pieces.insert(split.insert_at, new_piece);

        // Keep the line index valid for the edited span.
        self.update_line_starts(at..at, text);

        // Record the edit event (D7). `at` and `inserted_len` are byte
        // offsets into the pre-edit logical text.
        self.edits.push(BufferEdit::Insert {
            at,
            inserted_len: text.len(),
            inserted_piece,
            left_piece,
            pre_first_piece,
            splits,
        });
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
        // Both calls share one `splits` vec so the ordering in the
        // BufferEdit event is "left-end-split, then right-end-split".
        let mut splits = Vec::new();
        let left = self.split_at(range.start, &mut splits);
        let right = self.split_at(range.end, &mut splits);

        // The splits guarantee that `left.insert_at` is the index of the
        // first piece strictly at-or-after `range.start`, and
        // `right.insert_at` is the first piece at-or-after `range.end`.
        // Anything in `[left.insert_at, right.insert_at)` lies inside the
        // deleted span and is removed wholesale. The right-edge split may
        // have shortened a piece to the post-end remainder; that remainder
        // sits at `right.insert_at` and survives the drain.
        //
        // Collect the affected-piece metadata D7 stabilize needs to
        // repair stale tokens in O(affected) without walking the chain.
        // `left_survivor` is the piece ending at `range.start` (= `s`)
        // post-edit — `pieces[left.insert_at - 1]`, which (if a split
        // happened at `s`) is the original piece now shortened to the
        // split offset, or (if no split) the boundary piece unchanged.
        // `right_survivor` is `pieces[right.insert_at]` — the piece that
        // pre-edit starts at `range.end` and post-edit shifts to start
        // at `s`. `None` iff the delete reaches the buffer's start or
        // end respectively (no left/right edge survives).
        let deleted_piece_lens: Vec<(PieceId, usize)> = self.pieces
            [left.insert_at..right.insert_at]
            .iter()
            .map(|p| (p.id, p.len))
            .collect();
        let deleted_pieces: Vec<PieceId> = deleted_piece_lens.iter().map(|(id, _)| *id).collect();
        let left_survivor = if left.insert_at == 0 {
            None
        } else {
            let p = self.pieces[left.insert_at - 1];
            Some((p.id, p.len))
        };
        let right_survivor = if right.insert_at == self.pieces.len() {
            None
        } else {
            Some(self.pieces[right.insert_at].id)
        };

        self.pieces.drain(left.insert_at..right.insert_at);

        // Keep the line index valid for the edited span.
        self.update_line_starts(range.clone(), "");

        // Record the edit event (D7). `range` is the pre-edit byte span.
        self.edits.push(BufferEdit::Delete {
            range,
            deleted_pieces,
            deleted_piece_lens,
            left_survivor,
            right_survivor,
            splits,
        });
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
        assert!(
            line < starts.len(),
            "line {line} out of range ({})",
            starts.len()
        );
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

    // ---- Position tokens (D2 / D7) -------------------------------------

    /// Issue a `Position` token for byte offset `at` in the current
    /// logical text.
    ///
    /// `at` must satisfy `0 <= at <= len()`. Returns `None` iff the
    /// buffer is empty (no piece exists to anchor the token).
    ///
    /// Sticky-left policy (D7): if `at` lies on a piece boundary, the
    /// token is anchored to the piece *ending* at `at`, with
    /// `offset == piece.len`. A subsequent insert at `at` is therefore
    /// boundary-elided (no split), and the position keeps pointing at
    /// the same byte it pointed at before. To point *past* an inserted
    /// span (sticky-right), step 5 issues its own token *after* the
    /// edit, or remaps via the edit log.
    pub fn position_at(&self, at: usize) -> Option<Position> {
        assert!(at <= self.len());
        if self.pieces.is_empty() {
            return None;
        }
        if at == 0 {
            return Some(Position {
                piece: self.pieces[0].id,
                offset: 0,
            });
        }
        let mut acc = 0usize;
        for p in &self.pieces {
            let p_end = acc + p.len;
            if at == p_end {
                // Sticky-left: position anchored to the piece ending at `at`.
                return Some(Position {
                    piece: p.id,
                    offset: p.len as u32,
                });
            }
            if at > acc && at < p_end {
                return Some(Position {
                    piece: p.id,
                    offset: (at - acc) as u32,
                });
            }
            acc = p_end;
        }
        // `at <= len()` and the last piece's `p_end == len()`, so the
        // `at == p_end` branch above is taken for `at == len()`.
        unreachable!("position_at({at}) not resolved");
    }

    /// Resolve a `Position` token back to a byte offset.
    ///
    /// Returns `Some(byte_offset)` iff the position is *valid*: the
    /// piece still exists in the chain and `offset <= piece.len` in its
    /// *current* length. Returns `None` if:
    ///
    /// - The anchor piece has been deleted (no piece with `id` remains).
    /// - The anchor piece exists but `offset > piece.len` — the
    ///   position sat in the right half of an interior split (D7) and
    ///   is now *stale but detectable*. Step 5 reads the matching
    ///   `Split` from the edit log and remaps to
    ///   `(Split::new_piece, offset - Split::split_offset)`.
    pub fn resolve(&self, p: Position) -> Option<usize> {
        let mut acc = 0usize;
        for piece in &self.pieces {
            if piece.id == p.piece {
                let off = p.offset as usize;
                if off > piece.len {
                    return None;
                }
                return Some(acc + off);
            }
            acc += piece.len;
        }
        None
    }

    // ---- Edit log (D7) -------------------------------------------------

    /// High-water mark of the edit log: the sequence number that will be
    /// assigned to the *next* edit (zero on a fresh buffer, incremented
    /// by one per `insert` / `delete`).
    pub fn edit_seq(&self) -> usize {
        self.edits.len()
    }

    /// Slice of all edits with sequence number `>= seq`. `seq` is the
    /// high-water mark returned by `edit_seq`; an observer keeps a
    /// cursor, polls `edits_since(cursor)` each cycle, then advances
    /// its cursor by the returned slice's length.
    ///
    /// `seq` is clamped to the current log length, so a stale cursor
    /// from before a `take_edits` drain simply sees the whole log.
    pub fn edits_since(&self, seq: usize) -> &[BufferEdit] {
        let start = seq.min(self.edits.len());
        &self.edits[start..]
    }

    /// Drain the whole edit log and return it. Useful for tests and for
    /// bounded-memory production callers that have caught up to the
    /// current `edit_seq` and can discard history.
    pub fn take_edits(&mut self) -> Vec<BufferEdit> {
        std::mem::take(&mut self.edits)
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

        let delta: isize = new_text.len() as isize - (range.end - range.start) as isize;
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
    ///
    /// If a strict interior split occurs, a `Split` record is pushed onto
    /// `splits` so the caller can attach it to the `BufferEdit` event it
    /// is constructing. Boundary-elided splits push nothing.
    fn split_at(&mut self, at: usize, splits: &mut Vec<Split>) -> SplitLoc {
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
                let old_id = p.id;

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

                // Record the split for the caller's edit-log event.
                splits.push(Split {
                    old_piece: old_id,
                    split_offset: left_len,
                    new_piece: right_id,
                });
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
            let (start, end) = if start <= end {
                (start, end)
            } else {
                (end, start)
            };
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

    // ---- Position tokens (D2 / D7) -------------------------------------

    #[test]
    fn position_at_empty_buffer_returns_none() {
        let b = TextBuffer::new();
        assert!(b.position_at(0).is_none());
    }

    #[test]
    fn position_at_zero_is_first_piece_offset_zero() {
        let b = TextBuffer::from_text("abc");
        let p = b.position_at(0).unwrap();
        assert_eq!(p.piece(), b.pieces[0].id);
        assert_eq!(p.offset(), 0);
        assert_eq!(b.resolve(p), Some(0));
    }

    #[test]
    fn position_at_end_is_last_piece_len_sticky_left() {
        // Single piece "abc": at==3 returns (piece0, 3).
        let b = TextBuffer::from_text("abc");
        let p = b.position_at(3).unwrap();
        assert_eq!(p.piece(), b.pieces[0].id);
        assert_eq!(p.offset(), 3);
        assert_eq!(b.resolve(p), Some(3));
    }

    #[test]
    fn position_at_interior_of_piece() {
        let b = TextBuffer::from_text("abcde");
        let p = b.position_at(2).unwrap();
        assert_eq!(p.piece(), b.pieces[0].id);
        assert_eq!(p.offset(), 2);
        assert_eq!(b.resolve(p), Some(2));
    }

    #[test]
    fn position_at_piece_boundary_is_sticky_left() {
        // Two pieces from prior edits; insert a boundary to make two
        // pieces. After "ab" + insert(2, "cd") + insert(2, "X") we have:
        //   piece A "ab" (id 1)
        //   piece C "X"  (id 4 — the latest insert)
        //   piece B "cd" (id 3)
        // Boundary between A and C is offset 2; sticky-left → (A, 2).
        let mut b = TextBuffer::from_text("ab");
        b.insert(2, "cd"); // "abcd": A=ab, B=cd
        b.insert(2, "X"); // "abXcd": elided inserts in front of B
        // Layout: pieces = [A(ab), X("X"), B(cd)]. Find offsets.
        // A.len=2, X.len=1, B.len=2 → total 5.
        assert_eq!(s(&b), "abXcd");
        let boundary_offset = 2; // between A and X
        let p = b.position_at(boundary_offset).unwrap();
        // Sticky-left → anchored to A whose end is at offset 2.
        assert_eq!(p.piece(), b.pieces[0].id, "boundary should be sticky-left");
        assert_eq!(p.offset(), 2);
        assert_eq!(b.resolve(p), Some(2));
    }

    #[test]
    fn position_round_trips_through_resolve_for_all_offsets() {
        let b = TextBuffer::from_text("ab\ncd\nef");
        for off in 0..=b.len() {
            let p = b.position_at(off).unwrap();
            assert_eq!(b.resolve(p), Some(off), "offset {off}");
        }
    }

    #[test]
    fn resolve_unknown_piece_id_returns_none() {
        let b = TextBuffer::from_text("abc");
        let bogus = Position {
            piece: 999,
            offset: 0,
        };
        assert!(b.resolve(bogus).is_none());
    }

    #[test]
    fn resolve_offset_past_piece_len_returns_none() {
        // Construct a stale position past the piece's current length.
        let b = TextBuffer::from_text("abc");
        let stale = Position {
            piece: b.pieces[0].id,
            offset: 99,
        };
        assert!(b.resolve(stale).is_none());
    }

    #[test]
    fn left_half_position_survives_interior_split_unchanged() {
        // "hello" → insert "X" at offset 2 splits the original piece:
        // left half retains id (len 2), right half is a fresh piece.
        // A position anchored to the left half stays valid and resolves
        // to the same byte offset.
        let mut b = TextBuffer::from_text("hello");
        let left_pos = b.position_at(1).unwrap(); // (id0, 1) — left half
        b.insert(2, "X");
        assert_eq!(s(&b), "heXllo");
        // (id0, 1) is still valid: id0 still exists with len 2.
        assert_eq!(b.resolve(left_pos), Some(1));
    }

    #[test]
    fn right_half_position_becomes_stale_after_split() {
        // "hello" (id0, len 5). A position at offset 4 sits in the right
        // half of an interior split at offset 2: id0 is shortened to len 2,
        // so (id0, 4) is now stale (offset > current len).
        let mut b = TextBuffer::from_text("hello");
        let right_pos = b.position_at(4).unwrap(); // (id0, 4) — right half
        assert_eq!(right_pos.piece(), b.pieces[0].id);
        assert_eq!(right_pos.offset(), 4);
        b.insert(2, "X");
        // id0 has len 2 now; (id0, 4) is stale → resolve None.
        assert_eq!(b.resolve(right_pos), None);
    }

    #[test]
    fn position_in_deleted_piece_resolves_none() {
        // Build a multi-piece buffer so we can have the token anchored in
        // a piece that gets drained wholesale by a later delete.
        let mut b = TextBuffer::from_text("hello");
        b.insert(5, "world"); // pieces: id0 "hello", id1 "world"
        assert_eq!(s(&b), "helloworld");
        // Position anchored in id1 at offset 2 (= logical offset 7).
        let p = b.position_at(7).unwrap();
        assert_eq!(p.piece(), b.pieces[1].id);
        // Delete from offset 5 to end (whole of id1).
        b.delete(5..10);
        assert_eq!(s(&b), "hello");
        // id1 no longer exists → resolve returns None.
        assert!(b.resolve(p).is_none());
    }

    #[test]
    fn position_survives_unrelated_edit() {
        // A position is stable across edits elsewhere: insert at one end
        // of the buffer, position anchored at the other end stays valid.
        let mut b = TextBuffer::from_text("abc");
        let p = b.position_at(0).unwrap();
        b.insert(3, "XYZ");
        assert_eq!(s(&b), "abcXYZ");
        assert_eq!(b.resolve(p), Some(0));
    }

    // ---- Edit log: BufferEdit surface ---------------------------------

    #[test]
    fn edit_seq_starts_at_zero_and_increments() {
        let mut b = TextBuffer::from_text("abc");
        assert_eq!(b.edit_seq(), 0);
        b.insert(3, "X");
        assert_eq!(b.edit_seq(), 1);
        b.delete(0..1);
        assert_eq!(b.edit_seq(), 2);
        // No-op edits don't push events.
        b.insert(0, "");
        b.delete(2..2);
        assert_eq!(b.edit_seq(), 2);
    }

    #[test]
    fn edits_since_returns_incremental_slice() {
        let mut b = TextBuffer::from_text("abc");
        b.insert(3, "X");
        b.insert(0, "Y");
        let after_one = b.edit_seq() - 1;
        let latest = b.edits_since(after_one);
        assert_eq!(latest.len(), 1);
        assert!(matches!(latest[0], BufferEdit::Insert { at: 0, .. }));
        // Earlier edits still visible too.
        assert_eq!(b.edits_since(0).len(), 2);
        // Stale cursor past the end → empty slice (clamped).
        assert!(b.edits_since(99).is_empty());
    }

    #[test]
    fn take_edits_drains_log() {
        let mut b = TextBuffer::from_text("abc");
        b.insert(3, "X");
        let drained = b.take_edits();
        assert_eq!(drained.len(), 1);
        assert_eq!(b.edit_seq(), 0);
        // Subsequent edits start fresh.
        b.delete(0..1);
        let second = b.take_edits();
        assert_eq!(second.len(), 1);
        assert!(matches!(second[0], BufferEdit::Delete { .. }));
    }

    #[test]
    fn insert_emits_insert_event_with_correct_at_and_len() {
        let mut b = TextBuffer::from_text("abc");
        // Insert at the very end: boundary-elided, no split.
        b.insert(3, "XY");
        let edits = b.edits_since(0);
        assert_eq!(edits.len(), 1);
        match &edits[0] {
            BufferEdit::Insert {
                at,
                inserted_len,
                splits,
                ..
            } => {
                assert_eq!(*at, 3);
                assert_eq!(*inserted_len, 2);
                assert!(splits.is_empty(), "boundary insert must not split");
            }
            other => panic!("expected Insert, got {other:?}"),
        }
    }

    #[test]
    fn interior_insert_emits_one_split() {
        let mut b = TextBuffer::from_text("hello");
        let id0 = b.pieces[0].id;
        b.insert(2, "X"); // strict interior split at offset 2
        let edits = b.edits_since(0);
        match &edits[0] {
            BufferEdit::Insert { splits, .. } => {
                assert_eq!(splits.len(), 1);
                assert_eq!(
                    splits[0],
                    Split {
                        old_piece: id0,
                        split_offset: 2,
                        new_piece: id0 + 1,
                    }
                );
            }
            other => panic!("expected Insert, got {other:?}"),
        }
    }

    #[test]
    fn boundary_insert_emits_no_splits() {
        let mut b = TextBuffer::from_text("hello");
        b.insert(0, "X"); // boundary, elided
        match &b.edits_since(0)[0] {
            BufferEdit::Insert { splits, .. } => {
                assert!(splits.is_empty(), "boundary insert must not split");
            }
            other => panic!("expected Insert, got {other:?}"),
        }
        b.insert(b.len(), "Y"); // insert at end, no split
        // Two insert events, both with empty splits.
        for e in b.edits_since(0) {
            if let BufferEdit::Insert { splits, .. } = e {
                assert!(splits.is_empty());
            }
        }
    }

    #[test]
    fn delete_emits_delete_event_with_correct_range() {
        let mut b = TextBuffer::from_text("hello world");
        // Delete the whole content: both ends are piece-boundary-elided,
        // so the event carries the correct `range` and zero splits.
        b.delete(0..11);
        match &b.edits_since(0)[0] {
            BufferEdit::Delete { range, splits, .. } => {
                assert_eq!(*range, 0..11);
                assert!(splits.is_empty(), "whole-buffer delete must not split");
            }
            other => panic!("expected Delete, got {other:?}"),
        }
        assert!(b.is_empty());
    }

    #[test]
    fn delete_with_two_interior_ends_emits_two_splits() {
        let mut b = TextBuffer::from_text("abcdefghij");
        let id0 = b.pieces[0].id;
        // Two interior splits: range.start=2 on the original piece, and
        // range.end=8 on the right half (R1) created by the first split.
        // R1 starts at logical offset 2 with len 8; the second split is
        // therefore at R1's offset 8-2 = 6.
        b.delete(2..8);
        let r1_id = id0 + 1; // first split's right-half piece
        let r2_id = id0 + 2; // second split's right-half piece
        match &b.edits_since(0)[0] {
            BufferEdit::Delete { range, splits, .. } => {
                assert_eq!(*range, 2..8);
                assert_eq!(splits.len(), 2);
                assert_eq!(splits[0].old_piece, id0);
                assert_eq!(splits[0].split_offset, 2);
                assert_eq!(splits[0].new_piece, r1_id);
                assert_eq!(splits[1].old_piece, r1_id);
                assert_eq!(splits[1].split_offset, 6);
                assert_eq!(splits[1].new_piece, r2_id);
            }
            other => panic!("expected Delete, got {other:?}"),
        }
    }

    #[test]
    fn replace_emits_delete_then_insert_pair() {
        let mut b = TextBuffer::from_text("hello world");
        b.replace(0..5, "goodbye");
        let edits = b.edits_since(0);
        assert_eq!(edits.len(), 2);
        assert!(matches!(edits[0], BufferEdit::Delete { .. }));
        match &edits[1] {
            BufferEdit::Insert {
                at, inserted_len, ..
            } => {
                assert_eq!(*at, 0);
                assert_eq!(*inserted_len, "goodbye".len());
            }
            other => panic!("expected Insert, got {other:?}"),
        }
        assert_eq!(s(&b), "goodbye world");
    }

    // ---- End-to-end remap-by-log (the design payoff of D7) -----------

    /// Simulate step 5's job: hold a position token across an interior
    /// split, find it stale, walk the edit log, remap it to the new
    /// piece, and confirm the remapped token resolves to the same content
    /// the original token pointed at.
    #[test]
    fn stale_right_half_position_remaps_via_edit_log() {
        let mut b = TextBuffer::from_text("hello");
        // Token at byte offset 3 (the second 'l').
        let original = b.position_at(3).unwrap();
        assert_eq!(original.offset(), 3);
        let original_byte = b.resolve(original).unwrap();
        let content_before: char = b
            .read_range(original_byte..original_byte + 1)
            .chars()
            .next()
            .unwrap();
        assert_eq!(content_before, 'l');

        // Split at offset 2: original piece's right half moves to a new
        // piece; (id0, 4) becomes stale.
        b.insert(2, "X");
        assert_eq!(s(&b), "heXllo");

        // Stale detection.
        assert!(b.resolve(original).is_none());

        // Walk the edit log to find the matching Split and remap.
        let remapped = remap_via_log(&b, original).expect("log should contain a matching Split");
        // The remapped token should resolve to the same byte the
        // original would have resolved to before the edit, *plus* the
        // inserted span's length, since the insertion happened before
        // our position (at offset 2 < 4). Note: the design's remap
        // relocates us within the *new* piece; the byte offset shifts
        // by the inserted span length because the insert was at a
        // smaller offset than our position.
        let remapped_byte = b.resolve(remapped).unwrap();
        let content_after: char = b
            .read_range(remapped_byte..remapped_byte + 1)
            .chars()
            .next()
            .unwrap();
        assert_eq!(
            content_after, content_before,
            "remapped token must point at same content"
        );
        // And specifically the byte offset should have advanced by 1
        // (the inserted 'X').
        assert_eq!(remapped_byte, original_byte + 1);
    }

    /// Step-5-style helper: given a stale `Position`, walk the buffer's
    /// edit log forward and apply the D7 remap rule for any matching
    /// `Split`. Returns the remapped token, or `None` if no remap applies
    /// (the position was deleted wholesale, or no longer stale).
    fn remap_via_log(b: &TextBuffer, mut p: Position) -> Option<Position> {
        for edit in b.edits_since(0) {
            let splits = match edit {
                BufferEdit::Insert { splits, .. } | BufferEdit::Delete { splits, .. } => splits,
            };
            for sp in splits {
                if sp.old_piece == p.piece && (p.offset as usize) > sp.split_offset {
                    p = Position {
                        piece: sp.new_piece,
                        offset: p.offset - sp.split_offset as u32,
                    };
                }
            }
        }
        Some(p)
    }

    #[test]
    fn remap_walks_multiple_splits_in_order() {
        // Two interior splits on the same original piece: the original
        // right half (which became a fresh piece) is split again later.
        // A position at the far right of the original piece should
        // remap through both splits.
        let mut b = TextBuffer::from_text("abcdefghij"); // id0, len 10
        let original = b.position_at(9).unwrap(); // anchored at id0, offset 9
        // Split at offset 2: id0 shortens to 2; right half (offsets 2..10) becomes a fresh piece R1.
        b.insert(2, "X"); // "abXcdefghij"
        // Now split R1 at its interior by inserting at offset 5 (within R1, offset 5 - 3 = 2).
        // Logical offset 5 is bytes 0..2 = "ab", "X", then within R1 ("cdefghij", len 8) offset 5 - 3 = 2.
        b.insert(5, "Y"); // "abXcYdefghij"
        // The original token (id0, 9) is stale after the first split;
        // remapping gives (R1, 9-2=7). R1 is then split at its offset 2,
        // so we remap again to (R2, 7-2=5). The final byte offset should
        // point at the 'j' (the last byte).
        assert!(b.resolve(original).is_none());
        let remapped = remap_via_log(&b, original).unwrap();
        let final_byte = b.resolve(remapped).unwrap();
        assert_eq!(
            b.read_range(final_byte..final_byte + 1),
            "j",
            "remapped token must point at 'j'"
        );
    }
}
