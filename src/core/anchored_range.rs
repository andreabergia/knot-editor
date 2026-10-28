//! Stable byte ranges over [`TextBuffer`].
//!
//! Each endpoint uses a stable [`Position`] token plus endpoint stickiness.
//! Cached byte offsets preserve resolution while the buffer is empty.
//! [`AnchoredRangeStore::stabilize`] follows the buffer edit log and repairs
//! only endpoints whose pieces were affected; untouched endpoints continue to
//! resolve through their existing tokens.
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

/// An endpoint: a stable `Position` token.
#[derive(Clone, Copy, Debug)]
pub struct Anchor {
    pos: Position,
}

impl Anchor {
    pub fn new(pos: Position) -> Self {
        Self { pos }
    }

    /// Resolve this endpoint to a byte offset through the buffer.
    pub fn resolve(&self, buf: &TextBuffer) -> Option<usize> {
        buf.resolve(self.pos)
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

/// Authoritative anchored range store over a `TextBuffer`.
///
/// Buffer-agnostic by design (see module docs): pass `&TextBuffer` to the
/// methods that need it.
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
    /// High-water mark into `buffer`'s edit log.
    cursor: usize,
    next_id: AnchoredRangeId,
}

impl AnchoredRangeStore {
    pub fn new() -> Self {
        Self {
            anchored_ranges: HashMap::new(),
            endpoints_by_piece: BTreeMap::new(),
            unanchored_endpoints: BTreeSet::new(),
            cursor: 0,
            next_id: 1,
        }
    }

    /// Number of anchored ranges currently held.
    #[cfg(test)]
    pub fn len(&self) -> usize {
        self.anchored_ranges.len()
    }

    #[cfg(test)]
    pub fn is_empty(&self) -> bool {
        self.anchored_ranges.is_empty()
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
    /// selection semantics.
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

        let start_a = Anchor::new(s_pos);
        let end_a = Anchor::new(e_pos);

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

        id
    }

    /// Remove an anchored range entirely.
    pub fn remove(&mut self, id: AnchoredRangeId) {
        if let Some(ann) = self.anchored_ranges.remove(&id) {
            self.remove_anchored_range_endpoints(&ann);
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

    /// Iterate over all live anchored ranges.
    #[cfg(test)]
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
        // Invariant: the log must never have been shortened underneath us.
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
                inserted_end,
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
                        *inserted_end,
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
                        self.move_exact_boundary(buffer, *piece, *offset, *inserted_end);
                    } else if pre_first_piece.is_none() {
                        self.move_unanchored_endpoints(buffer, *inserted_end);
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
    /// a provider must re-publish it under a fresh id.
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

#[cfg(test)]
mod tests;
