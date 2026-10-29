//! `TextBuffer`: stable-ID piece-table backing store.
//!
//! The buffer has monotonic stable piece IDs, insert / delete / replace /
//! read-range operations, an incrementally maintained line index, and a
//! `Position` / `BufferEdit` surface for stable external ranges.
//!
//! ## Line index
//!
//! Every line start is recorded both as a byte offset and as a UTF-16 offset.
//! Text and every public position stay UTF-8; the UTF-16 column lets platform
//! text input, which speaks document-wide UTF-16 offsets, convert in
//! O(log lines + line length) instead of scanning from the buffer start.
//!
//! ## Position tokens
//!
//! A `Position` is an opaque `(PieceId, u32 offset_within_piece)` pair.
//! It is stable across *unrelated* edits: an edit elsewhere in the
//! buffer does not invalidate it. The only failure modes are:
//!
//! - The piece was deleted wholesale → `resolve` returns `None`.
//! - The position sat in the right half of an interior split and
//!   the underlying piece has since been shortened → `resolve` returns
//!   `None`, but the content the position pointed to is recoverable
//!   from the matching `Split` record in the edit log: the new location
//!   is `(Split::new_piece, old_offset - Split::split_offset)`.
//!
//! Default stickiness is *sticky-left*: a `position_at` query at a piece
//! boundary returns the piece ending at that offset (with `offset ==
//! piece.len`), so a subsequent insert at the boundary leaves the
//! position pointing at the same byte it pointed at before. Sticky-right
//! anchors belong to the anchored-range store, which reads the edit log and
//! relocates them.
//!
//! ## Edit log
//!
//! Every `insert` / `delete` (and `replace`, which decomposes into a
//! delete-then-insert pair) pushes one `BufferEdit` event onto the log.
//! Fields are byte offsets into the *pre-edit* logical text, so an observer
//! replaying stale `Position`s can reason about each edit against the buffer
//! state *in which the position was issued*. Each event also records the
//! line and byte column of its edges, which observers such as incremental
//! parsers need and cannot recover once later edits have moved the lines. Sequence numbers are monotonic
//! for the buffer's lifetime; the owner discards entries once every observer
//! has consumed them, so the retained log stays bounded.
//!
//! ## Backing store
//!
//! Immutable backing text backs the piece chain:
//!
//! - `Original`: the buffer's load-time text, shared as an `Arc<str>`. Pieces
//!   referencing it never move; the buffer holds it for its whole lifetime.
//! - `Add`: an append-only sink, stored as sealed `Arc<str>` chunks plus one
//!   mutable tail. Every inserted span is appended to the tail and referenced
//!   by a freshly-issued piece, or by extending the piece that already ends at
//!   the tail's end (see coalescing below). Once the tail reaches
//!   `ADD_CHUNK_SIZE` it is sealed and a new tail starts, so a span never
//!   straddles chunks and old pieces keep their `(Add(chunk), start, len)`
//!   references stable forever.
//!
//! A `Piece` is a reference `(backing, start)` fixed at creation, plus a
//! monotonically-increasing `PieceId` issued from a global counter. Only
//! `len` ever changes: the split rule shortens it, and coalescing extends it.
//!
//! ## Snapshots
//!
//! `snapshot` returns an immutable, `Send` [`TextSnapshot`] for readers off
//! the foreground. It shares the original text and sealed chunks, and copies
//! only the tail and the piece list, so its cost is bounded by the chunk size
//! plus O(pieces) rather than by the document length.
//!
//! ## Split rule
//!
//! Interior edits split a piece; the left half keeps the original id (and
//! original `start`, with `len` shortened to the split offset), the right
//! half gets a fresh id. Degenerate splits (offset 0 or `len`) are elided —
//! the new piece is spliced in directly, no zero-length halves created.
//!
//! ## Coalescing
//!
//! An insert at the end of an `Add` piece whose span ends at the end of the
//! tail extends that piece instead of creating one, so typing grows one piece
//! rather than one per keystroke. Existing positions in the piece keep their
//! offsets, and a sticky-left position at its old end still resolves before
//! the inserted text. A split-shortened piece never qualifies, because its
//! right half still references later bytes of the tail, and neither does a
//! piece in a sealed chunk.
//!
//! ## UTF-8 boundary policy
//!
//! Buffer edits and reads use byte offsets which must land on UTF-8 scalar
//! boundaries. Grapheme-aware caret movement and deletion remain presentation
//! policy above the byte-native core.

use std::mem;
use std::ops::Range;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

pub use snapshot::TextSnapshot;

mod snapshot;

/// Length at which the add sink's tail is sealed into an immutable chunk.
///
/// Snapshots copy the tail, so this bounds their per-call copy; a single
/// larger insert seals as soon as it is appended.
const ADD_CHUNK_SIZE: usize = 16 * 1024;

/// Monotonically-issued identity for each `TextBuffer` instance.
///
/// This is separate from piece IDs: transactions use it to reject a call made
/// with a different buffer that happens to have the same edit-log sequence.
static NEXT_BUFFER_ID: AtomicU64 = AtomicU64::new(1);

/// Monotonically-increasing piece identifier, issued from a global counter.
///
/// Stable across unrelated edits: a piece's id never changes for the
/// piece's lifetime, and is never reused (the counter only goes up).
pub type PieceId = u64;

/// Opaque, stable token for a location in the logical text.
///
/// `(piece, offset_within_piece)`. Issued by `TextBuffer::position_at`,
/// resolved back to a byte offset by `TextBuffer::resolve`. The token
/// is stable across unrelated edits; failure modes are documented on
/// the module and on `resolve`.
///
/// Field accessors are exposed (rather than the struct being fully
/// opaque) because the anchored-range store needs to *store* tokens keyed by piece id and
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
    /// callers can build the *remapped* token produced by applying
    /// the split rule to a `Split` record — `(Split::new_piece,
    /// old_offset - Split::split_offset)`. The buffer itself never uses
    /// this; the only in-crate producer of tokens is `position_at`.
    pub fn new(piece: PieceId, offset: u32) -> Self {
        Self { piece, offset }
    }
}

/// A line and a byte column within it, both 0-indexed.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct TextPoint {
    pub line: usize,
    pub column: usize,
}

/// One half of an interior split.
///
/// A `Position` whose `piece` is `old_piece` and whose offset is
/// `> split_offset` is *stale but detectable*: its content has moved to
/// `new_piece` at offset `old_offset - split_offset`. The anchored-range store reads the
/// edit log and applies this transformation; the buffer itself never
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

/// Per-edit event in the `TextBuffer`'s edit log.
///
/// `at` and `range` are byte offsets into the buffer's *pre-edit*
/// logical text, so an observer replaying stale `Position`s can reason
/// about each edit against the buffer state in which the tokens were
/// issued. `splits` is normally length 0 (the edit landed on a piece
/// boundary and was elided) or 1 (one interior split); a `delete`
/// spanning two interiors may carry up to 2.
///
/// In addition to the splits, each variant carries enough
/// affected-piece metadata for the anchored-range store to repair
/// stale tokens in `O(affected)` time per edit,
/// without walking the whole piece chain: the anchored-range store keys
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
        /// Point of `at`.
        at_point: TextPoint,
        /// Point of the end of the inserted span, after the insert.
        end_point: TextPoint,
        /// Position at the end of the inserted span: `(new_piece,
        /// inserted_len)` for a fresh piece, or the extended piece's new
        /// end when the insert coalesced. Sticky-`After` anchors sitting on
        /// a boundary-elided insert relocate here.
        inserted_end: Position,
        /// The piece ending at logical offset `at` *pre-insert*, and
        /// the offset-within-that-piece corresponding to the boundary
        /// `at`. `None` iff `at == 0` (no piece to the left of the
        /// buffer start). A sticky-`After` anchor whose token is
        /// `(left_piece.id, left_piece_offset)` (i.e. sitting exactly
        /// at the boundary) relocates to `inserted_end`;
        /// a sticky-`Before` anchor keeps its token and resolves to
        /// `at` post-edit, matching sticky-`Before` semantics.
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
        /// `n` post-edit (chain shift), but sticky-`Before` requires them to
        /// stay at `0`.
        pre_first_piece: Option<PieceId>,
        splits: Vec<Split>,
    },
    /// Bytes in `range` were removed. Splits describe any interior
    /// piece splits the edit forced (up to two, at `range.start` and
    /// `range.end`).
    Delete {
        range: Range<usize>,
        /// Point of `range.start`.
        start_point: TextPoint,
        /// Point of `range.end`, before the delete.
        end_point: TextPoint,
        /// Piece ids drained by this delete (the contents of
        /// `[left.insert_at, right.insert_at)` post-both-splits,
        /// pre-drain). Anchors whose token.piece is in this set were
        /// inside the deleted span and are re-bound to a surviving
        /// edge piece below.
        deleted_pieces: Vec<PieceId>,
        /// `(piece_id, post-split_len)` for the drained pieces, in chain
        /// order. The anchored-range store uses this to tell true interior endpoints from
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

/// Start of one line, as UTF-8 byte and UTF-16 code-unit offsets.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct LineStart {
    byte: usize,
    utf16: usize,
}

const BUFFER_START: LineStart = LineStart { byte: 0, utf16: 0 };

/// Which backing buffer a piece references.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Backing {
    /// The load-time text. Held immutably for the `TextBuffer`'s lifetime.
    Original,
    /// One chunk of the append-only edit sink: a sealed chunk, or the tail
    /// when the index equals the number of sealed chunks. Appended spans
    /// never move.
    Add(u32),
}

/// A reference to a contiguous span of one backing buffer.
///
/// `(backing, start)` is fixed at creation. Only `len` changes: the split
/// rule shortens the left half, and coalescing extends a trailing `Add` piece.
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
    /// Stable identity of this buffer instance. Kept crate-visible because
    /// `EditTransaction` must bind its recorded byte offsets to one buffer.
    instance_id: u64,
    /// Immutable load-time text backing the `Original` pieces.
    original: Arc<str>,
    /// Sealed chunks of the append-only add sink, shared with snapshots.
    sealed: Vec<Arc<str>>,
    /// Unsealed end of the add sink. Appended spans are never overwritten or
    /// moved; only new bytes go at the end.
    tail: String,
    /// The piece chain. Operations splice and shorten in place; snapshots
    /// take a copy.
    pieces: Vec<Piece>,
    /// Global monotonic piece-id counter. Never reused.
    next_id: PieceId,
    /// Total length of the logical text, in UTF-8 bytes.
    len: usize,
    /// Line index, kept valid under every edit by `update_line_starts`.
    ///
    /// Sorted ascending. Entry 0 is always the buffer start, even for an empty
    /// buffer. A trailing `'\n'` creates one extra empty line, so `"abc\n"`
    /// has byte starts `[0, 4]`.
    line_starts: Vec<LineStart>,
    /// Retained edit log. Each `insert` / `delete` pushes one `BufferEdit`.
    /// `replace` decomposes into a delete-then-insert pair and pushes two.
    edits: Vec<BufferEdit>,
    /// Sequence number of `edits[0]`: how many edits were discarded.
    discarded_edits: usize,
}

impl TextBuffer {
    /// Build a `TextBuffer` from load-time text.
    ///
    /// The text becomes the `Original` backing; an initial single piece
    /// references the whole span. The `Add` sink starts empty. No `\r`
    /// normalization is performed — the bytes are stored verbatim.
    pub fn from_text(text: impl Into<Arc<str>>) -> Self {
        let original = text.into();
        let len = original.len();
        let mut line_starts = vec![BUFFER_START];
        scan_line_starts(&original, BUFFER_START, &mut line_starts);
        let mut buf = TextBuffer {
            instance_id: NEXT_BUFFER_ID.fetch_add(1, Ordering::Relaxed),
            original,
            sealed: Vec::new(),
            tail: String::new(),
            pieces: Vec::new(),
            next_id: 1,
            len,
            line_starts,
            edits: Vec::new(),
            discarded_edits: 0,
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
            instance_id: NEXT_BUFFER_ID.fetch_add(1, Ordering::Relaxed),
            original: Arc::from(""),
            sealed: Vec::new(),
            tail: String::new(),
            pieces: Vec::new(),
            next_id: 1,
            len: 0,
            line_starts: vec![BUFFER_START],
            edits: Vec::new(),
            discarded_edits: 0,
        }
    }

    /// Total length of the logical text, in UTF-8 bytes.
    pub fn len(&self) -> usize {
        self.len
    }

    /// Stable identity of this `TextBuffer` instance.
    ///
    /// This is crate-visible rather than public API: it guards recorded edit
    /// transactions against being replayed against another buffer.
    pub(crate) fn instance_id(&self) -> u64 {
        self.instance_id
    }

    /// Whether the logical text is empty.
    pub fn is_empty(&self) -> bool {
        self.len == 0
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
        for chunk in self.chunks(range) {
            out.push_str(chunk);
        }
        out
    }

    /// The logical text in `range` as borrowed piece slices, in order.
    fn chunks(&self, range: Range<usize>) -> impl Iterator<Item = &str> {
        let mut offset = 0usize;
        self.pieces.iter().map_while(move |p| {
            let p_start = offset;
            offset += p.len;
            if p_start >= range.end {
                return None;
            }
            let slice_start = p_start.max(range.start).min(offset) - p_start;
            let slice_end = offset.min(range.end) - p_start;
            Some(&self.backing_slice(*p)[slice_start..slice_end])
        })
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

        // Append the span to the add tail and record its placement.
        let tail_chunk = self.sealed.len() as u32;
        let add_start = self.tail.len();
        self.tail.push_str(text);

        let at_point = self.point_of_offset(at);

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

        let coalesce = splits.is_empty()
            && split.insert_at > 0
            && matches!(
                self.pieces[split.insert_at - 1],
                Piece { backing: Backing::Add(chunk), start, len, .. }
                    if chunk == tail_chunk && start + len == add_start
            );
        let inserted_end = if coalesce {
            let piece = &mut self.pieces[split.insert_at - 1];
            piece.len += text.len();
            Position::new(piece.id, piece.len as u32)
        } else {
            let id = self.next_id;
            self.next_id += 1;
            self.pieces.insert(
                split.insert_at,
                Piece {
                    id,
                    backing: Backing::Add(tail_chunk),
                    start: add_start,
                    len: text.len(),
                },
            );
            Position::new(id, text.len() as u32)
        };
        self.len += text.len();
        if self.tail.len() >= ADD_CHUNK_SIZE {
            self.sealed.push(Arc::from(mem::take(&mut self.tail)));
        }

        // Keep the line index valid for the edited span.
        self.update_line_starts(at..at, text, 0);
        let end_point = self.point_of_offset(at + text.len());

        // Record the edit event. `at` and `inserted_len` are byte
        // offsets into the pre-edit logical text.
        self.edits.push(BufferEdit::Insert {
            at,
            inserted_len: text.len(),
            at_point,
            end_point,
            inserted_end,
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
        let removed_utf16 = self.chunks(range.clone()).map(utf16_len).sum();
        let start_point = self.point_of_offset(range.start);
        let end_point = self.point_of_offset(range.end);

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
        // Collect the affected-piece metadata stabilization needs to
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
        self.len -= range.len();

        // Keep the line index valid for the edited span.
        self.update_line_starts(range.clone(), "", removed_utf16);

        // Record the edit event. `range` is the pre-edit byte span.
        self.edits.push(BufferEdit::Delete {
            range,
            start_point,
            end_point,
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

    // ---- Line index ------------------------------------------------

    /// Number of lines. The buffer always has at least one line (line 0,
    /// possibly empty), so this returns `>= 1`.
    pub fn line_count(&self) -> usize {
        self.line_starts.len()
    }

    /// Byte offset of the start of `line` (0-indexed). `line` must be
    /// `< line_count()`.
    pub fn line_start(&self, line: usize) -> usize {
        assert!(
            line < self.line_starts.len(),
            "line {line} out of range ({})",
            self.line_starts.len()
        );
        self.line_starts[line].byte
    }

    /// 0-indexed line number of the line containing byte offset `offset`.
    /// `offset` must satisfy `0 <= offset <= len()`. `len()` maps to the
    /// last line.
    pub fn line_of_offset(&self, offset: usize) -> usize {
        assert!(offset <= self.len());
        // First index where `starts[i] > offset`; the line containing
        // `offset` is the one before it. Entry 0 is always 0.
        self.line_starts.partition_point(|s| s.byte <= offset) - 1
    }

    /// Line and byte column of `offset`, which must satisfy
    /// `0 <= offset <= len()`.
    pub fn point_of_offset(&self, offset: usize) -> TextPoint {
        let line = self.line_of_offset(offset);
        TextPoint {
            line,
            column: offset - self.line_starts[line].byte,
        }
    }

    /// UTF-16 code-unit offset of byte offset `offset`, which must be a char
    /// boundary `<= len()`.
    pub fn utf16_of_offset(&self, offset: usize) -> usize {
        debug_assert!(self.is_char_boundary(offset));
        let start = self.line_starts[self.line_of_offset(offset)];
        start.utf16
            + self
                .chunks(start.byte..offset)
                .map(utf16_len)
                .sum::<usize>()
    }

    /// Byte offset of UTF-16 code-unit offset `utf16`. An offset inside a
    /// surrogate pair rounds up to the end of its scalar, and an offset past
    /// the end clamps to `len()`.
    pub fn offset_of_utf16(&self, utf16: usize) -> usize {
        let line = self.line_starts.partition_point(|s| s.utf16 <= utf16) - 1;
        let start = self.line_starts[line];
        let end = self
            .line_starts
            .get(line + 1)
            .map_or(self.len, |next| next.byte);
        let mut units = start.utf16;
        let mut offset = start.byte;
        for ch in self.chunks(start.byte..end).flat_map(str::chars) {
            if units >= utf16 {
                break;
            }
            units += ch.len_utf16();
            offset += ch.len_utf8();
        }
        offset
    }

    // ---- Position tokens -------------------------------------

    /// Issue a `Position` token for byte offset `at` in the current
    /// logical text.
    ///
    /// `at` must satisfy `0 <= at <= len()`. Returns `None` iff the
    /// buffer is empty (no piece exists to anchor the token).
    ///
    /// Sticky-left policy: if `at` lies on a piece boundary, the
    /// token is anchored to the piece *ending* at `at`, with
    /// `offset == piece.len`. A subsequent insert at `at` is therefore
    /// boundary-elided (no split), and the position keeps pointing at
    /// the same byte it pointed at before. To point *past* an inserted
    /// span (sticky-right), the anchored-range store issues its own token *after* the
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
    ///   position sat in the right half of an interior split and
    ///   is now *stale but detectable*. The anchored-range store reads the matching
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

    // ---- Edit log -------------------------------------------------

    /// High-water mark of the edit log: the sequence number that will be
    /// assigned to the *next* edit (zero on a fresh buffer, incremented
    /// by one per `insert` / `delete`).
    pub fn edit_seq(&self) -> usize {
        self.discarded_edits + self.edits.len()
    }

    /// Slice of all edits with sequence number `>= seq`. `seq` is the
    /// high-water mark returned by `edit_seq`; an observer keeps a
    /// cursor, polls `edits_since(cursor)` each cycle, then advances
    /// its cursor by the returned slice's length.
    ///
    /// `seq` must not precede discarded edits. A `seq` past the end yields
    /// an empty slice.
    pub fn edits_since(&self, seq: usize) -> &[BufferEdit] {
        assert!(
            seq >= self.discarded_edits,
            "edits before {} were discarded (requested {seq})",
            self.discarded_edits
        );
        let start = (seq - self.discarded_edits).min(self.edits.len());
        &self.edits[start..]
    }

    #[cfg(test)]
    pub(crate) fn retained_edits(&self) -> usize {
        self.edits.len()
    }

    /// Discard every edit with sequence number `< seq`, once all observers
    /// have consumed them. `edit_seq` is unaffected.
    pub fn discard_edits_before(&mut self, seq: usize) {
        let count = seq
            .saturating_sub(self.discarded_edits)
            .min(self.edits.len());
        self.edits.drain(..count);
        self.discarded_edits += count;
    }

    // ---- Internals -------------------------------------------------------

    /// Backing slice for a piece. Lifetime ties to `self`.
    fn backing_slice(&self, p: Piece) -> &str {
        let buf = match p.backing {
            Backing::Original => &self.original,
            Backing::Add(chunk) => self
                .sealed
                .get(chunk as usize)
                .map_or(self.tail.as_str(), |sealed| sealed),
        };
        &buf[p.start..p.start + p.len]
    }

    /// Incrementally update the line index after an edit that replaced the
    /// byte span `[range.start, range.end)`, which held `removed_utf16` UTF-16
    /// code units, with `new_text`. Called after the piece chain changed.
    ///
    /// Splices the K-entry delta for the edited span and fixes up the
    /// surviving suffix in O(n−i).
    ///
    /// Algorithm:
    /// - `delta = new_text.len() - (range.end - range.start)` (signed), and
    ///   likewise in UTF-16 code units.
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
    fn update_line_starts(&mut self, range: Range<usize>, new_text: &str, removed_utf16: usize) {
        let byte_delta = new_text.len() as isize - range.len() as isize;
        let utf16_delta = utf16_len(new_text) as isize - removed_utf16 as isize;
        let lo = self.line_starts.partition_point(|s| s.byte < range.start);
        let hi = self.line_starts.partition_point(|s| s.byte <= range.end);

        // Build the middle section: surviving line at `range.start` (if any)
        // plus new line starts from `'\n'`s in `new_text`. The text before
        // `range.start` is unchanged, so its UTF-16 offset is still valid.
        let mut middle = Vec::new();
        if self
            .line_starts
            .get(lo)
            .is_some_and(|start| start.byte == range.start)
        {
            middle.push(self.line_starts[lo]);
        }
        if new_text.contains('\n') {
            let base = LineStart {
                byte: range.start,
                utf16: self.utf16_of_offset(range.start),
            };
            scan_line_starts(new_text, base, &mut middle);
        }

        let tail = lo + middle.len();
        self.line_starts.splice(lo..hi, middle);
        for start in &mut self.line_starts[tail..] {
            start.byte = start.byte.wrapping_add_signed(byte_delta);
            start.utf16 = start.utf16.wrapping_add_signed(utf16_delta);
        }
    }

    /// Whether `offset` is a UTF-8 scalar boundary in the logical text.
    ///
    /// This is crate-visible so the editor host can validate untrusted byte
    /// ranges before calling the buffer's assertion-based mutation and read
    /// operations.
    pub(crate) fn is_char_boundary(&self, offset: usize) -> bool {
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
    /// are elided by the split rule: the new piece is spliced in directly, no
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
                // Strict interior split — the split rule's only non-degenerate case.
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

/// Append the start of every line that follows a `'\n'` in `text`, where
/// `text` begins at `base`.
fn scan_line_starts(text: &str, base: LineStart, out: &mut Vec<LineStart>) {
    let mut utf16 = base.utf16;
    for (i, ch) in text.char_indices() {
        utf16 += ch.len_utf16();
        if ch == '\n' {
            out.push(LineStart {
                byte: base.byte + i + 1,
                utf16,
            });
        }
    }
}

fn utf16_len(text: &str) -> usize {
    text.chars().map(char::len_utf16).sum()
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
mod tests;
