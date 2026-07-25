//! Stable byte ranges over [`TextBuffer`].
//!
//! Each endpoint uses a stable [`Position`] token plus endpoint stickiness.
//! Cached byte offsets preserve resolution while the buffer is empty.
//! [`AnchoredRangeStore::stabilize`] follows the buffer edit log and repairs
//! only endpoints whose pieces were affected; untouched endpoints continue to
//! resolve through their existing tokens.
//!
//! Range queries use a derived interval index. Mutations mark the index dirty,
//! and [`AnchoredRangeStore::query_range`] rebuilds it lazily before querying.
//!
//! A deletion that fully consumes a normal range removes it and its endpoint
//! index entries. Persistent ranges instead collapse at the surviving edit
//! boundary for cursor and selection geometry.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::ops::Range;

use super::buffer::{BufferEdit, PieceId, Position, TextBuffer};

/// Stable identifier for an anchored range, issued by the store.
pub type AnchoredRangeId = u64;

/// Anchored ranges removed while processing buffer edits.
pub struct StabilizationResult {
    pub consumed: Vec<AnchoredRangeId>,
}

/// Which side of an inserted span an endpoint sticks to (D2).
///
/// - `Before` (sticky-left, default for a selection *start*): text inserted
///   at the endpoint's offset lands *before* it; the endpoint keeps its byte.
/// - `After` (sticky-right, default for a selection *end*): text inserted at
///   the endpoint's offset lands *after* it; the endpoint relocates past the
///   inserted span.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Stickiness {
    Before,
    After,
}

/// An endpoint: a stable `Position` token plus a stickiness.
#[derive(Clone, Copy, Debug)]
pub struct Anchor {
    pos: Position,
    sticky: Stickiness,
}

impl Anchor {
    pub fn new(pos: Position, sticky: Stickiness) -> Self {
        Self { pos, sticky }
    }

    /// Resolve this endpoint to a byte offset through the buffer.
    pub fn resolve(&self, buf: &TextBuffer) -> Option<usize> {
        buf.resolve(self.pos)
    }

    pub fn pos(&self) -> Position {
        self.pos
    }

    pub fn sticky(&self) -> Stickiness {
        self.sticky
    }
}

/// One stable range with two anchored endpoints.
#[derive(Clone, Debug)]
pub struct AnchoredRange {
    pub id: AnchoredRangeId,
    pub start: Anchor,
    pub end: Anchor,
    retain_when_empty: bool,
    /// Cached live resolved byte offsets, the authoritative transient state
    /// `stabilize` refreshes after every edit (see module docs). The
    /// `Position` tokens are re-anchored against these via `position_at`.
    idx_start: usize,
    idx_end: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
enum EndpointKey {
    Start(AnchoredRangeId),
    End(AnchoredRangeId),
}

impl EndpointKey {
    fn start(id: AnchoredRangeId) -> Self {
        Self::Start(id)
    }

    fn end(id: AnchoredRangeId) -> Self {
        Self::End(id)
    }

    fn id(self) -> AnchoredRangeId {
        match self {
            Self::Start(id) | Self::End(id) => id,
        }
    }
}

/// Authoritative anchored range store over a `TextBuffer` (D1, D4).
///
/// Buffer-agnostic by design (see module docs): pass `&TextBuffer` to the
/// methods that need it.
#[derive(Clone, Copy, Debug)]
struct IntervalEntry {
    start: usize,
    end: usize,
    id: AnchoredRangeId,
}

pub struct AnchoredRangeStore {
    anchored_ranges: HashMap<AnchoredRangeId, AnchoredRange>,
    /// Endpoint index keyed by stable piece id, then endpoint offset within
    /// the piece. This lets `stabilize` touch only anchors whose piece split,
    /// received a boundary insert, or was deleted.
    endpoints_by_piece: BTreeMap<PieceId, BTreeSet<(u32, EndpointKey)>>,
    /// Endpoints that currently have no surviving piece to anchor to (only
    /// possible when the buffer is empty and a zero-width anchored range survives
    /// at offset 0).
    unanchored_endpoints: BTreeSet<EndpointKey>,
    /// Derived interval index for `query_range` (D3): sorted by `start`,
    /// rebuilt lazily on the next query after edits or mutations.
    interval_index: Vec<IntervalEntry>,
    index_dirty: bool,
    /// High-water mark into `buffer`'s edit log (D4).
    cursor: usize,
    next_id: AnchoredRangeId,
}

impl AnchoredRangeStore {
    pub fn new() -> Self {
        Self {
            anchored_ranges: HashMap::new(),
            endpoints_by_piece: BTreeMap::new(),
            unanchored_endpoints: BTreeSet::new(),
            interval_index: Vec::new(),
            index_dirty: true,
            cursor: 0,
            next_id: 1,
        }
    }

    /// Number of anchored ranges currently held.
    pub fn len(&self) -> usize {
        self.anchored_ranges.len()
    }

    pub fn is_empty(&self) -> bool {
        self.anchored_ranges.is_empty()
    }

    fn mark_index_dirty(&mut self) {
        self.index_dirty = true;
    }

    fn rebuild_index(&mut self, buffer: &TextBuffer) {
        self.interval_index.clear();
        for (&id, ann) in &self.anchored_ranges {
            let s = ann.start.resolve(buffer).unwrap_or(ann.idx_start);
            let e = ann.end.resolve(buffer).unwrap_or(ann.idx_end);
            let (start, end) = if s <= e { (s, e) } else { (e, s) };
            self.interval_index.push(IntervalEntry { start, end, id });
        }
        self.interval_index.sort_unstable_by_key(|e| e.start);
        self.index_dirty = false;
    }

    fn insert_endpoint_index(&mut self, pos: Position, key: EndpointKey) {
        self.unanchored_endpoints.remove(&key);
        self.endpoints_by_piece
            .entry(pos.piece())
            .or_default()
            .insert((pos.offset(), key));
    }

    fn remove_endpoint_index(&mut self, pos: Position, key: EndpointKey) {
        let mut empty = false;
        if let Some(entries) = self.endpoints_by_piece.get_mut(&pos.piece()) {
            entries.remove(&(pos.offset(), key));
            empty = entries.is_empty();
        }
        if empty {
            self.endpoints_by_piece.remove(&pos.piece());
        }
    }

    fn endpoint_pos(&self, key: EndpointKey) -> Option<Position> {
        let ann = self.anchored_ranges.get(&key.id())?;
        match key {
            EndpointKey::Start(_) => Some(ann.start.pos),
            EndpointKey::End(_) => Some(ann.end.pos),
        }
    }

    fn set_endpoint_pos(&mut self, buffer: &TextBuffer, key: EndpointKey, pos: Position) {
        let Some(old_pos) = self.endpoint_pos(key) else {
            return;
        };
        self.remove_endpoint_index(old_pos, key);
        if let Some(ann) = self.anchored_ranges.get_mut(&key.id()) {
            match key {
                EndpointKey::Start(_) => {
                    ann.start.pos = pos;
                    ann.idx_start = buffer.resolve(pos).unwrap_or(ann.idx_start);
                }
                EndpointKey::End(_) => {
                    ann.end.pos = pos;
                    ann.idx_end = buffer.resolve(pos).unwrap_or(ann.idx_end);
                }
            }
        }
        self.insert_endpoint_index(pos, key);
    }

    fn set_endpoint_unanchored(&mut self, key: EndpointKey, fallback_offset: usize) {
        let Some(old_pos) = self.endpoint_pos(key) else {
            return;
        };
        self.remove_endpoint_index(old_pos, key);
        if let Some(ann) = self.anchored_ranges.get_mut(&key.id()) {
            match key {
                EndpointKey::Start(_) => ann.idx_start = fallback_offset,
                EndpointKey::End(_) => ann.idx_end = fallback_offset,
            }
        }
        self.unanchored_endpoints.insert(key);
    }

    fn endpoints_for_piece(&self, piece: PieceId) -> Vec<(u32, EndpointKey)> {
        self.endpoints_by_piece
            .get(&piece)
            .map(|entries| entries.iter().copied().collect())
            .unwrap_or_default()
    }

    fn endpoints_at(&self, piece: PieceId, offset: usize) -> Vec<EndpointKey> {
        let offset = offset as u32;
        self.endpoints_by_piece
            .get(&piece)
            .map(|entries| {
                entries
                    .range((offset, EndpointKey::start(0))..=(offset, EndpointKey::end(u64::MAX)))
                    .map(|&(_, key)| key)
                    .collect()
            })
            .unwrap_or_default()
    }

    fn endpoints_after(&self, piece: PieceId, offset: usize) -> Vec<(u32, EndpointKey)> {
        let offset = offset as u32;
        self.endpoints_by_piece
            .get(&piece)
            .map(|entries| {
                entries
                    .range((offset.saturating_add(1), EndpointKey::start(0))..)
                    .map(|&(off, key)| (off, key))
                    .collect()
            })
            .unwrap_or_default()
    }

    fn remove_anchored_range_endpoints(&mut self, ann: &AnchoredRange) {
        self.remove_endpoint_index(ann.start.pos, EndpointKey::start(ann.id));
        self.remove_endpoint_index(ann.end.pos, EndpointKey::end(ann.id));
        self.unanchored_endpoints
            .remove(&EndpointKey::start(ann.id));
        self.unanchored_endpoints.remove(&EndpointKey::end(ann.id));
    }

    /// Add an anchored range spanning `[start, end)` (byte offsets into the
    /// current logical text). The start endpoint defaults to `Before`
    /// (sticky-left) and the end to `After` (sticky-right) — standard
    /// selection semantics (D2).
    pub fn add(&mut self, buffer: &TextBuffer, start: usize, end: usize) -> AnchoredRangeId {
        self.add_inner(buffer, start, end, false)
    }

    /// Add a range whose endpoints survive deletion even when its extent
    /// collapses to zero. Cursor and selection geometry uses this behavior to
    /// snap to the surviving edit boundary rather than disappear.
    pub fn add_persistent(
        &mut self,
        buffer: &TextBuffer,
        start: usize,
        end: usize,
    ) -> AnchoredRangeId {
        self.add_inner(buffer, start, end, true)
    }

    fn add_inner(
        &mut self,
        buffer: &TextBuffer,
        start: usize,
        end: usize,
        retain_when_empty: bool,
    ) -> AnchoredRangeId {
        let id = self.next_id;
        self.next_id += 1;

        let s_pos = buffer
            .position_at(start)
            .expect("start offset out of range");
        let e_pos = buffer.position_at(end).expect("end offset out of range");

        let start_a = Anchor::new(s_pos, Stickiness::Before);
        let end_a = Anchor::new(e_pos, Stickiness::After);

        let ann = AnchoredRange {
            id,
            start: start_a,
            end: end_a,
            retain_when_empty,
            idx_start: start,
            idx_end: end,
        };
        self.anchored_ranges.insert(id, ann);
        self.insert_endpoint_index(s_pos, EndpointKey::start(id));
        self.insert_endpoint_index(e_pos, EndpointKey::end(id));
        self.mark_index_dirty();

        id
    }

    /// Remove an anchored range entirely.
    pub fn remove(&mut self, id: AnchoredRangeId) {
        if let Some(ann) = self.anchored_ranges.remove(&id) {
            self.remove_anchored_range_endpoints(&ann);
            self.mark_index_dirty();
        }
    }

    /// Resolve an anchored range to its current byte range.
    ///
    /// Returns `None` iff the anchored range has been removed (e.g. it was
    /// fully consumed by a delete) — see module docs for the contract.
    ///
    /// The caller must `stabilize` before `resolve` to reflect edits.
    pub fn resolve(&self, buffer: &TextBuffer, id: AnchoredRangeId) -> Option<Range<usize>> {
        let ann = self.anchored_ranges.get(&id)?;
        let s = ann.start.resolve(buffer).unwrap_or(ann.idx_start);
        let e = ann.end.resolve(buffer).unwrap_or(ann.idx_end);
        if s <= e { Some(s..e) } else { Some(e..s) }
    }

    /// All anchored range ids overlapping `[a, b)` (D3, for step 6). Uses a
    /// lazily-rebuilt interval index sorted by start byte for O(log n + k)
    /// binary-search lookup; the index is rebuilt once on the first query
    /// after edits.
    pub fn query_range(&mut self, buffer: &TextBuffer, a: usize, b: usize) -> Vec<AnchoredRangeId> {
        if a >= b {
            return Vec::new();
        }
        if self.index_dirty {
            self.rebuild_index(buffer);
        }
        let start_idx = self.interval_index.partition_point(|e| e.start < b);
        let out: Vec<AnchoredRangeId> = self.interval_index[..start_idx]
            .iter()
            .filter(|e| e.end > a)
            .map(|e| e.id)
            .collect();
        // Already in start order from the sorted index.
        out
    }

    /// Iterate over all live anchored ranges.
    pub fn iter_live(&self) -> impl Iterator<Item = (&AnchoredRangeId, &AnchoredRange)> {
        self.anchored_ranges.iter()
    }

    /// Advance over every edit since the last `stabilize`, repairing only
    /// endpoints whose token pieces are touched by those edits.
    ///
    /// Untouched anchored ranges are not scanned: their `Position` tokens stay
    /// valid and resolve through the buffer's piece table.
    pub fn stabilize(&mut self, buffer: &TextBuffer) -> StabilizationResult {
        let edits = buffer.edits_since(self.cursor);
        // D4 invariant: the log must never have been shortened underneath us.
        assert!(
            buffer.edit_seq() >= self.cursor,
            "edit log was shortened underneath the store (cursor {} > seq {})",
            self.cursor,
            buffer.edit_seq()
        );

        if edits.is_empty() {
            return StabilizationResult {
                consumed: Vec::new(),
            };
        }

        let mut consumed_ids = Vec::new();
        for edit in edits {
            self.apply_edit_incremental(buffer, edit, &mut consumed_ids);
        }
        self.cursor += edits.len();
        self.mark_index_dirty();
        StabilizationResult {
            consumed: consumed_ids,
        }
    }

    // ---- stabilization internals -----------------------------------------

    fn apply_edit_incremental(
        &mut self,
        buffer: &TextBuffer,
        edit: &BufferEdit,
        consumed_ids: &mut Vec<AnchoredRangeId>,
    ) {
        match edit {
            BufferEdit::Insert {
                inserted_len,
                inserted_piece,
                left_piece,
                pre_first_piece,
                splits,
                ..
            } => {
                for split in splits {
                    self.move_exact_boundary(
                        buffer,
                        split.old_piece,
                        split.split_offset,
                        Position::new(*inserted_piece, *inserted_len as u32),
                    );
                    self.remap_split_right_half(
                        buffer,
                        split.old_piece,
                        split.split_offset,
                        split.new_piece,
                    );
                }
                if splits.is_empty() {
                    if let Some((piece, offset)) = left_piece {
                        self.move_exact_boundary(
                            buffer,
                            *piece,
                            *offset,
                            Position::new(*inserted_piece, *inserted_len as u32),
                        );
                    } else if pre_first_piece.is_none() {
                        self.move_unanchored_endpoints(
                            buffer,
                            Position::new(*inserted_piece, *inserted_len as u32),
                        );
                    }
                }
            }
            BufferEdit::Delete {
                range,
                deleted_pieces,
                deleted_piece_lens,
                left_survivor,
                right_survivor,
                splits,
            } => {
                for split in splits {
                    self.remap_split_right_half(
                        buffer,
                        split.old_piece,
                        split.split_offset,
                        split.new_piece,
                    );
                }

                let right_boundary_split = right_survivor.and_then(|right| {
                    splits
                        .iter()
                        .find(|split| split.new_piece == right)
                        .map(|split| (split.old_piece, split.split_offset as u32))
                });

                let edge = left_survivor
                    .map(|(piece, offset)| Position::new(piece, offset as u32))
                    .or_else(|| right_survivor.map(|piece| Position::new(piece, 0)));

                let mut touched = HashSet::new();
                let mut consumption_candidates = HashSet::new();
                let first_deleted = deleted_piece_lens.first().copied();
                let last_deleted = deleted_piece_lens.last().copied();
                for piece in deleted_pieces {
                    for (offset, key) in self.endpoints_for_piece(*piece) {
                        touched.insert(key.id());
                        let at_right_boundary = right_boundary_split
                            .map(|(p, off)| p == *piece && off == offset)
                            .unwrap_or(false);
                        let at_elided_left_boundary = first_deleted
                            .map(|(p, _)| p == *piece && offset == 0)
                            .unwrap_or(false);
                        let at_elided_right_boundary = last_deleted
                            .map(|(p, len)| p == *piece && offset as usize == len)
                            .unwrap_or(false);
                        if !at_right_boundary
                            && !at_elided_left_boundary
                            && !at_elided_right_boundary
                        {
                            consumption_candidates.insert(key.id());
                        }
                        if let Some(pos) = edge {
                            self.set_endpoint_pos(buffer, key, pos);
                        } else {
                            self.set_endpoint_unanchored(key, range.start);
                        }
                    }
                }

                for id in touched {
                    self.refresh_cached_offsets(buffer, id);
                    if consumption_candidates.contains(&id)
                        && !self.anchored_ranges[&id].retain_when_empty
                        && self.resolved_extent_empty(buffer, id)
                        && self.consume(id)
                    {
                        consumed_ids.push(id);
                    }
                }
            }
        }
    }

    fn move_exact_boundary(
        &mut self,
        buffer: &TextBuffer,
        piece: PieceId,
        offset: usize,
        to: Position,
    ) {
        for key in self.endpoints_at(piece, offset) {
            self.set_endpoint_pos(buffer, key, to);
        }
    }

    fn remap_split_right_half(
        &mut self,
        buffer: &TextBuffer,
        old_piece: PieceId,
        split_offset: usize,
        new_piece: PieceId,
    ) {
        for (old_offset, key) in self.endpoints_after(old_piece, split_offset) {
            let new_offset = old_offset as usize - split_offset;
            self.set_endpoint_pos(buffer, key, Position::new(new_piece, new_offset as u32));
        }
    }

    fn move_unanchored_endpoints(&mut self, buffer: &TextBuffer, to: Position) {
        let keys: Vec<_> = self.unanchored_endpoints.iter().copied().collect();
        for key in keys {
            if let Some(ann) = self.anchored_ranges.get_mut(&key.id()) {
                match key {
                    EndpointKey::Start(_) => {
                        ann.start.pos = to;
                        ann.idx_start = buffer.resolve(to).unwrap_or(ann.idx_start);
                    }
                    EndpointKey::End(_) => {
                        ann.end.pos = to;
                        ann.idx_end = buffer.resolve(to).unwrap_or(ann.idx_end);
                    }
                }
            }
            self.insert_endpoint_index(to, key);
        }
    }

    fn refresh_cached_offsets(&mut self, buffer: &TextBuffer, id: AnchoredRangeId) {
        if let Some(ann) = self.anchored_ranges.get_mut(&id) {
            if let Some(s) = ann.start.resolve(buffer) {
                ann.idx_start = s;
            }
            if let Some(e) = ann.end.resolve(buffer) {
                ann.idx_end = e;
            }
        }
    }

    fn resolved_extent_empty(&self, buffer: &TextBuffer, id: AnchoredRangeId) -> bool {
        let Some(ann) = self.anchored_ranges.get(&id) else {
            return false;
        };
        let s = ann.start.resolve(buffer).unwrap_or(ann.idx_start);
        let e = ann.end.resolve(buffer).unwrap_or(ann.idx_end);
        s >= e
    }

    /// Fully consume an anchored range whose extent has collapsed to zero (both
    /// endpoints snapped onto the same surviving edge of a delete). Removes
    /// the anchored range, its endpoint-index entries, and any cached offsets.
    /// `resolve` will return `None` for `id` thereafter; there is no
    /// tombstone. Undoing the text edit does not revive the anchored range —
    /// a provider must re-publish it under a fresh id (step 6b).
    fn consume(&mut self, id: AnchoredRangeId) -> bool {
        if let Some(ann) = self.anchored_ranges.remove(&id) {
            self.remove_anchored_range_endpoints(&ann);
            true
        } else {
            false
        }
    }
}

impl Default for AnchoredRangeStore {
    fn default() -> Self {
        Self::new()
    }
}

/// Naive offset-remap store (D5): the measurement baseline.
///
/// Anchored ranges keep raw byte ranges and, on every edit, *every* anchored range's
/// offsets are re-transformed by the same insert/delete rules the token store
/// applies via `Position` stability. This is O(all anchored ranges) per edit by
/// construction — the comparator that answers "which representation" in step
/// 5's benchmark. It shares `transform_offset` with the correctness oracle.
pub struct OffsetStore {
    anns: HashMap<AnchoredRangeId, (usize, usize, Stickiness, Stickiness, bool)>,
    cursor: usize,
    next_id: AnchoredRangeId,
}

impl OffsetStore {
    pub fn new() -> Self {
        Self {
            anns: HashMap::new(),
            cursor: 0,
            next_id: 1,
        }
    }

    pub fn add(&mut self, start: usize, end: usize) -> AnchoredRangeId {
        let id = self.next_id;
        self.next_id += 1;
        self.anns.insert(
            id,
            (start, end, Stickiness::Before, Stickiness::After, false),
        );
        id
    }

    /// O(all) per edit: walk every anchored range applying the edit transform.
    pub fn stabilize(&mut self, buffer: &TextBuffer) {
        let edits = buffer.edits_since(self.cursor);
        assert!(
            buffer.edit_seq() >= self.cursor,
            "edit log shortened underneath the baseline store"
        );
        for (_, ann) in self.anns.iter_mut() {
            let (s, e, ss, es, collapsed) = ann;
            if *collapsed {
                continue;
            }
            for edit in edits {
                *s = transform_offset(*s, *ss, edit);
                *e = transform_offset(*e, *es, edit);
            }
            if *s >= *e {
                *collapsed = true;
            }
        }
        self.cursor += edits.len();
    }

    pub fn resolve(&self, id: AnchoredRangeId) -> Option<Range<usize>> {
        let (s, e, _, _, collapsed) = self.anns.get(&id)?;
        if *collapsed {
            return None;
        }
        if *s <= *e { Some(*s..*e) } else { Some(*e..*s) }
    }
}

impl Default for OffsetStore {
    fn default() -> Self {
        Self::new()
    }
}

/// Apply one edit to a single endpoint's byte offset under its stickiness
/// (D2). Shared by `OffsetStore` and the correctness oracle so the baseline
/// and the test agree on the expected transform semantics.
///
/// ## Stickiness in the offset-tracking model
///
/// In a pure byte-offset model `Before` and `After` produce the **same**
/// surviving byte offset at an edit's boundary:
///
/// - **Insert at `at`, length `n`**: a position at `at` (the insert point)
///   shifts with the content it was anchoring — the byte that was at `at`
///   is now at `at + n` — so *both* stickiness modes land at `at + n`. This
///   is the content-tracking reading D2's narrative ("the endpoint keeps
///   pointing at the same content") settles on, and the one the unit tests
///   pin: insert-before-`Before`-start moves the start with the content.
/// - **Delete `[s, e)`**: a position strictly inside `(s, e)` snaps to the
///   surviving edge. Whichever side, the post-delete byte that was at `e`
///   has shifted to `s`, so `Before` and `After` agree numerically at `s`.
///
/// The genuine `Before`/`After` distinction lives in the *piece-anchoring*
/// layer (sticky-left `position_at` for `Before`, explicit relocate for
/// `After`) — the offset-tracking oracle proves the store's resolved byte
/// ranges match a brute-force replay, not the per-mode anchoring choice.
pub fn transform_offset(off: usize, _sticky: Stickiness, edit: &BufferEdit) -> usize {
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

#[cfg(test)]
mod tests {
    use super::*;

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
        assert!(store.query_range(&b, 0, 11).is_empty());
    }

    #[test]
    fn position_at_boundary_anchoring_default_stickiness() {
        // start defaults to Before (sticky-left), end to After (sticky-right).
        let b = text();
        let mut store = AnchoredRangeStore::new();
        let id = store.add(&b, 0, 11);
        assert_eq!(
            store.anchored_ranges[&id].start.sticky(),
            Stickiness::Before
        );
        assert_eq!(store.anchored_ranges[&id].end.sticky(), Stickiness::After);
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
    fn query_range_returns_overlapping_excludes_nonoverlapping() {
        let b = text();
        let mut store = AnchoredRangeStore::new();
        let a = store.add(&b, 0, 5);
        let c = store.add(&b, 6, 11);
        let _far = store.add(&b, 9, 11);
        let hits = store.query_range(&b, 4, 7);
        assert!(hits.contains(&a), "a [0,5) overlaps [4,7)");
        assert!(hits.contains(&c), "c [6,11) overlaps [4,7)");
        assert_eq!(hits.len(), 2);
    }

    #[test]
    fn query_range_consistent_with_linear_scan() {
        let b = text();
        let mut store = AnchoredRangeStore::new();
        for i in 0..5 {
            store.add(&b, i, i + 2);
        }
        // Every query must match a brute-force linear scan over resolved ranges.
        for a in 0..=11 {
            assert!(
                store.query_range(&b, a, a).is_empty(),
                "empty range must match nothing"
            );
            for bnd in (a + 1)..=11 {
                let q = store.query_range(&b, a, bnd);
                let mut expected = Vec::new();
                for id in store.anchored_ranges.keys().copied() {
                    if let Some(r) = store.resolve(&b, id) {
                        if r.start < bnd && r.end > a {
                            expected.push(id);
                        }
                    }
                }
                let mut q = q;
                q.sort();
                expected.sort();
                assert_eq!(q, expected, "query [{a},{bnd}) mismatch");
            }
        }
    }

    #[test]
    fn query_range_stays_correct_after_unrelated_shift() {
        let mut b = text();
        let mut store = AnchoredRangeStore::new();
        let id = store.add(&b, 6, 11);
        b.insert(0, ">>");
        store.stabilize(&b);
        assert_eq!(store.resolve(&b, id), Some(8..13));
        assert_eq!(store.query_range(&b, 8, 13), vec![id]);
        assert!(store.query_range(&b, 6, 8).is_empty());
    }

    #[test]
    fn query_range_with_index_fast_path_matches_linear_scan() {
        let b = text();
        let mut store = AnchoredRangeStore::new();
        for i in 0..5 {
            store.add(&b, i, i + 2);
        }
        // A no-op stabilize marks the index dirty (lazy rebuild on next query).
        store.stabilize(&b);
        assert!(
            store.index_dirty,
            "index should be dirty after stabilize (lazy)"
        );
        // Every query must match a brute-force linear scan oracle.
        for a in 0..=11 {
            for bnd in (a + 1)..=11 {
                let q = store.query_range(&b, a, bnd);
                let mut expected = Vec::new();
                for id in store.anchored_ranges.keys().copied() {
                    if let Some(r) = store.resolve(&b, id) {
                        if r.start < bnd && r.end > a {
                            expected.push(id);
                        }
                    }
                }
                let mut q = q;
                q.sort();
                expected.sort();
                assert_eq!(q, expected, "index fast-path query [{a},{bnd}) mismatch");
            }
        }
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

    // ---- Step 6b: removal-on-consumption semantics ---------------------

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

        // Removed entirely — no tombstone, no query presence, no iter_live
        // entry.
        assert_eq!(store.resolve(&b, victim), None, "victim fully consumed");
        assert!(
            !store.query_range(&b, 0, b.len()).contains(&victim),
            "victim must not appear in query_range"
        );
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
            inserted_piece: 0,
            left_piece: None,
            pre_first_piece: None,
            splits: vec![],
        }
    }
    fn edit_delete(s: usize, e: usize) -> BufferEdit {
        BufferEdit::Delete {
            range: s..e,
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
        let mut o_collapsed = vec![false; N];
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
                        o_start[i] = transform_offset(o_start[i], Stickiness::Before, &e);
                        o_end[i] = transform_offset(o_end[i], Stickiness::After, &e);
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
                        o_start[i] = transform_offset(o_start[i], Stickiness::Before, &e);
                        o_end[i] = transform_offset(o_end[i], Stickiness::After, &e);
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
                        o_start[i] = transform_offset(o_start[i], Stickiness::Before, &d);
                        o_end[i] = transform_offset(o_end[i], Stickiness::After, &d);
                        if (s_in || e_in) && o_start[i] >= o_end[i] {
                            o_collapsed[i] = true;
                            continue;
                        }
                        o_start[i] = transform_offset(o_start[i], Stickiness::Before, &ii);
                        o_end[i] = transform_offset(o_end[i], Stickiness::After, &ii);
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
}
