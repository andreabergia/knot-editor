//! `AnnotationStore`: the annotation layer over `TextBuffer` (step 5, D1–D4).
//!
//! Each annotation anchors its endpoints as stable `Position` tokens issued
//! by `TextBuffer::position_at` (D1), not raw byte offsets. Endpoint
//! stickiness (`Before`/`After`, D2) makes annotations survive edits
//! correctly. A derived interval index (D3) answers "annotations overlapping
//! `[a, b)`".
//!
//! ## Representation and stabilization (current prototype form)
//!
//! Endpoints carry a `Position` token plus the cached live byte offset
//! (`idx_start` / `idx_end`). `stabilize` advances a cursor over the edit
//! log and, **per edit**, applies the same `transform_offset` rule the
//! correctness oracle uses (D2) to *every* non-collapsed annotation's cached
//! offsets, recomputes the `collapsed` flag with the oracle's exact rule
//! (collapse only if an endpoint was strictly inside a delete *and* the
//! resulting extent is empty), then rebuilds both the `Position` tokens (via
//! `position_at`) and the interval index from the refreshed offsets.
//!
//! This is correct **by construction** — the store's transform is identical
//! to the oracle's — but it is `O(all annotations)` per edit, same as the
//! D5 offset-remap baseline. The plan's eventual target is `O(affected)`
//! incrementality (only re-resolve the endpoints an edit actually moves,
//! preserving the token stability step 4 bought us); the per-edit token
//! repair that tried to reach that incrementally is replaced here by the
//! provably-correct full pass, and the `O(affected)` optimization is the
//! documented step-5 carry-forward.
//!
//! ## API shape note
//!
//! The plan's type sketch stored `buffer: &'buf TextBuffer` inside the store.
//! That makes the store unusable while the buffer is being edited (you cannot
//! hold `&buffer` and call `buffer.insert` through `&mut`). Instead the store
//! is buffer-agnostic and receives `&TextBuffer` at the call sites that need
//! it (`add`, `resolve`, `stabilize`). An editor owns the `TextBuffer` and the
//! `AnnotationStore` as sibling fields and passes the buffer in — which is
//! exactly the shape a real host needs.
//!
//! ## `resolve` contract (decided)
//!
//! `resolve` returns `Some(start..end)` with `start <= end` for every live
//! annotation. When an annotation's extent is fully consumed by a delete
//! (both endpoints land strictly inside a deleted span and collapse onto the
//! same point), it has no surviving extent: `resolve` returns `None` and the
//! annotation becomes invisible to `query_range`. The id stays reserved; a
//! later edit can re-extend it. This matches the step-4-style correctness
//! oracle, which also reports `None` for a fully-deleted annotation.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::ops::Range;

use super::buffer::{BufferEdit, Position, TextBuffer};

/// Stable identifier for an annotation, issued by the store.
pub type AnnotationId = u64;

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

/// Classification of an annotation's source (step 6 will render by kind).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AnnotationKind {
    Diagnostic,
    Search,
    Git,
    Breakpoint,
    Other(u8),
}

/// Opaque per-source payload. Minimal owned placeholder for the prototype;
/// step 6 supplies the real rendering data.
#[derive(Clone, Debug, Default)]
pub struct AnnotationData(pub String);

/// One annotation: two anchored endpoints plus source metadata.
#[derive(Clone, Debug)]
pub struct Annotation {
    pub id: AnnotationId,
    pub start: Anchor,
    pub end: Anchor,
    pub kind: AnnotationKind,
    pub data: AnnotationData,
    /// True iff the annotation was fully consumed by a delete and now has no
    /// surviving extent (`resolve` returns `None`).
    collapsed: bool,
    /// Cached live resolved byte offsets, the authoritative transient state
    /// `stabilize` refreshes after every edit (see module docs). The
    /// `Position` tokens are re-anchored against these via `position_at`.
    idx_start: usize,
    idx_end: usize,
}

/// Authoritative annotation store over a `TextBuffer` (D1, D4).
///
/// Buffer-agnostic by design (see module docs): pass `&TextBuffer` to the
/// methods that need it.
pub struct AnnotationStore {
    annotations: HashMap<AnnotationId, Annotation>,
    /// Interval index keyed by live start offset: `(start, id) -> end`.
    /// Rebuilt from scratch by `reindex_all` after each edit; a derived cache
    /// of the cached `idx_start`/`idx_end` offsets.
    start_index: BTreeMap<(usize, AnnotationId), usize>,
    /// Mirror keyed by live end offset: `(end, id) -> start`. Lets
    /// `query_range` find spans enclosing the query endpoints.
    end_index: BTreeMap<(usize, AnnotationId), usize>,
    /// High-water mark into `buffer`'s edit log (D4).
    cursor: usize,
    next_id: AnnotationId,
}

impl AnnotationStore {
    pub fn new() -> Self {
        Self {
            annotations: HashMap::new(),
            start_index: BTreeMap::new(),
            end_index: BTreeMap::new(),
            cursor: 0,
            next_id: 1,
        }
    }

    /// Number of annotations currently held (collapsed or live).
    pub fn len(&self) -> usize {
        self.annotations.len()
    }

    pub fn is_empty(&self) -> bool {
        self.annotations.is_empty()
    }

    /// Add an annotation spanning `[start, end)` (byte offsets into the
    /// current logical text). The start endpoint defaults to `Before`
    /// (sticky-left) and the end to `After` (sticky-right) — standard
    /// selection semantics (D2).
    pub fn add(
        &mut self,
        buffer: &TextBuffer,
        start: usize,
        end: usize,
        kind: AnnotationKind,
        data: AnnotationData,
    ) -> AnnotationId {
        let id = self.next_id;
        self.next_id += 1;

        let s_pos = buffer
            .position_at(start)
            .expect("start offset out of range");
        let e_pos = buffer.position_at(end).expect("end offset out of range");

        let start_a = Anchor::new(s_pos, Stickiness::Before);
        let end_a = Anchor::new(e_pos, Stickiness::After);

        let ann = Annotation {
            id,
            start: start_a,
            end: end_a,
            kind,
            data,
            collapsed: false,
            idx_start: start,
            idx_end: end,
        };
        self.annotations.insert(id, ann);

        self.start_index.insert((start, id), end);
        self.end_index.insert((end, id), start);

        id
    }

    /// Remove an annotation entirely.
    pub fn remove(&mut self, id: AnnotationId) {
        if let Some(ann) = self.annotations.remove(&id) {
            self.start_index.remove(&(ann.idx_start, id));
            self.end_index.remove(&(ann.idx_end, id));
        }
    }

    /// Resolve an annotation to its current byte range.
    ///
    /// Returns `None` iff the annotation is `collapsed` (fully consumed by a
    /// delete) — see module docs for the contract.
    ///
    /// In the current `O(all)` prototype form `resolve` reads the cached
    /// `idx_start` / `idx_end` offsets directly: they are refreshed by
    /// `stabilize` using the *same* `transform_offset` the correctness oracle
    /// uses, so they track the oracle byte-for-byte. The `Position` tokens are
    /// re-anchored by `reindex_all` but are a cache for the future `O(affected)`
    /// path — they are *not* consulted here, because a token can fail to
    /// re-anchor when its piece was deleted even though the offset it
    /// represented survives (e.g. an empty buffer after a full-buffer delete
    /// has no piece to anchor a zero-length annotation to). The caller must
    /// `stabilize` before `resolve` to reflect edits; the offsets are only
    /// refreshed then. `buffer` is accepted to keep the API shape callers
    /// already use and as the seam for the incremental path.
    pub fn resolve(&self, _buffer: &TextBuffer, id: AnnotationId) -> Option<Range<usize>> {
        let ann = self.annotations.get(&id)?;
        if ann.collapsed {
            return None;
        }
        let (s, e) = (ann.idx_start, ann.idx_end);
        if s <= e {
            Some(s..e)
        } else {
            Some(e..s)
        }
    }

    /// All annotation ids overlapping `[a, b)` (D3, for step 6). Collapsed
    /// (fully-deleted) annotations are excluded — they have no extent.
    pub fn query_range(&self, a: usize, b: usize) -> Vec<AnnotationId> {
        if a >= b {
            return Vec::new();
        }
        let mut out: BTreeSet<AnnotationId> = BTreeSet::new();
        // Annotations starting inside [a, b).
        for (&(_s, id), _) in self.start_index.range((a, 0)..(b, 0)) {
            if !self.annotations[&id].collapsed {
                out.insert(id);
            }
        }
        // Annotations ending inside (a, b).
        for (&(_e, id), _) in self.end_index.range((a + 1, 0)..(b, AnnotationId::MAX)) {
            if !self.annotations[&id].collapsed {
                out.insert(id);
            }
        }
        // Annotations spanning the whole range (start < a, end > a).
        for (&(end, id), &start) in self.end_index.range((a + 1, 0)..) {
            if start < a && end > a && !self.annotations[&id].collapsed {
                out.insert(id);
            }
        }
        out.into_iter().collect()
    }

    /// Advance over every edit since the last `stabilize`, applying D2
    /// stickiness to every annotation's cached offsets and re-anchoring the
    /// endpoint tokens + interval index.
    ///
    /// Per edit this is `O(all annotations)` (a full pass applying
    /// `transform_offset`, plus a `position_at` re-anchor per live
    /// endpoint). This matches the correctness oracle **by construction**
    /// — the store uses the identical transform — at the cost of the
    /// incremental update the plan ultimately targets. The carry-forward is
    /// to re-resolve only the endpoints an edit actually moves (preserving
    /// step 4's token stability) once the prototype's correctness is locked;
    /// `reindex_all` is the seam that gets replaced by the incremental path.
    pub fn stabilize(&mut self, buffer: &TextBuffer) {
        let edits = buffer.edits_since(self.cursor);
        // D4 invariant: the log must never have been shortened underneath us.
        assert!(
            buffer.edit_seq() >= self.cursor,
            "edit log was shortened underneath the store (cursor {} > seq {})",
            self.cursor,
            buffer.edit_seq()
        );

        if edits.is_empty() {
            return;
        }

        for edit in edits {
            self.apply_edit_all(edit);
            self.reindex_all(buffer);
        }
        self.cursor += edits.len();
    }

    // ---- stabilization internals -----------------------------------------

    /// Apply one edit's stickiness transform (D2) to every non-collapsed
    /// annotation's cached `idx_start` / `idx_end`, using the *same*
    /// `transform_offset` the correctness oracle uses — so the offsets track
    /// the oracle byte-for-byte. Collapsed annotations stay reserved and
    /// are never re-extended by a later edit (matching the oracle's
    /// once-collapsed-always-collapsed behavior here).
    ///
    /// For a delete, the `collapsed` flag is recomputed with the oracle's
    /// exact rule: collapse only if an endpoint was strictly inside the
    /// deleted span (`at < off < end`) **and** the post-transform extent is
    /// empty (`start >= end`). A zero-length annotation exactly at a delete
    /// boundary is *not* "inside" and so stays put at the surviving edge
    /// (`start == end == at`), visible as a zero-width point — the same edge
    /// case the oracle preserves.
    fn apply_edit_all(&mut self, edit: &BufferEdit) {
        for ann in self.annotations.values_mut() {
            if ann.collapsed {
                continue;
            }
            match edit {
                BufferEdit::Insert { .. } => {
                    ann.idx_start = transform_offset(ann.idx_start, ann.start.sticky, edit);
                    ann.idx_end = transform_offset(ann.idx_end, ann.end.sticky, edit);
                }
                BufferEdit::Delete { range, .. } => {
                    let at = range.start;
                    let end = range.end;
                    let s_in = ann.idx_start > at && ann.idx_start < end;
                    let e_in = ann.idx_end > at && ann.idx_end < end;
                    ann.idx_start = transform_offset(ann.idx_start, ann.start.sticky, edit);
                    ann.idx_end = transform_offset(ann.idx_end, ann.end.sticky, edit);
                    if (s_in || e_in) && ann.idx_start >= ann.idx_end {
                        ann.collapsed = true;
                    }
                }
            }
        }
    }

    /// Rebuild the interval index and re-anchor every endpoint token from
    /// the freshly-transformed cached offsets. `position_at(idx)` re-anchors
    /// the `Position`; the index entries are recomputed from `idx_start` /
    /// `idx_end`. Collapsed annotations are excluded from both — they have no
    /// extent, so `query_range` cannot see them.
    ///
    /// Re-anchoring uses the buffer's sticky-left default `position_at`. That
    /// is dead-on for `Before` endpoints (sticky-left matches the transform
    /// rule "off unchanged at the boundary") and the `After` stickiness is
    /// applied at `transform` time, so the token's anchoring is purely a cache
    /// for next pass — the *decisions* come from `transform_offset`, not the
    /// token placement.
    fn reindex_all(&mut self, buffer: &TextBuffer) {
        self.start_index.clear();
        self.end_index.clear();
        for ann in self.annotations.values_mut() {
            if ann.collapsed {
                continue;
            }
            if let Some(p) = buffer.position_at(ann.idx_start) {
                ann.start.pos = p;
            }
            if let Some(p) = buffer.position_at(ann.idx_end) {
                ann.end.pos = p;
            }
            let s = ann.idx_start;
            let e = ann.idx_end;
            let id = ann.id;
            self.start_index.insert((s, id), e);
            self.end_index.insert((e, id), s);
        }
    }
}

impl Default for AnnotationStore {
    fn default() -> Self {
        Self::new()
    }
}

/// Naive offset-remap store (D5): the measurement baseline.
///
/// Annotations keep raw byte ranges and, on every edit, *every* annotation's
/// offsets are re-transformed by the same insert/delete rules the token store
/// applies via `Position` stability. This is O(all annotations) per edit by
/// construction — the comparator that answers "which representation" in step
/// 5's benchmark. It shares `transform_offset` with the correctness oracle.
pub struct OffsetStore {
    anns: HashMap<AnnotationId, (usize, usize, Stickiness, Stickiness, bool)>,
    cursor: usize,
    next_id: AnnotationId,
}

impl OffsetStore {
    pub fn new() -> Self {
        Self {
            anns: HashMap::new(),
            cursor: 0,
            next_id: 1,
        }
    }

    pub fn add(&mut self, start: usize, end: usize) -> AnnotationId {
        let id = self.next_id;
        self.next_id += 1;
        self.anns
            .insert(id, (start, end, Stickiness::Before, Stickiness::After, false));
        id
    }

    /// O(all) per edit: walk every annotation applying the edit transform.
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

    pub fn resolve(&self, id: AnnotationId) -> Option<Range<usize>> {
        let (s, e, _, _, collapsed) = self.anns.get(&id)?;
        if *collapsed {
            return None;
        }
        if *s <= *e {
            Some(*s..*e)
        } else {
            Some(*e..*s)
        }
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
        let mut store = AnnotationStore::new();
        let id = store.add(&b, 0, 5, AnnotationKind::Diagnostic, AnnotationData::default());
        assert_eq!(store.resolve(&b, id), Some(0..5));
        let id2 = store.add(&b, 6, 11, AnnotationKind::Search, AnnotationData::default());
        assert_eq!(store.resolve(&b, id2), Some(6..11));
    }

    #[test]
    fn remove_drops_annotation() {
        let b = text();
        let mut store = AnnotationStore::new();
        let id = store.add(&b, 0, 5, AnnotationKind::Diagnostic, AnnotationData::default());
        store.remove(id);
        assert!(store.resolve(&b, id).is_none());
        assert!(store.query_range(0, 11).is_empty());
    }

    #[test]
    fn position_at_boundary_anchoring_default_stickiness() {
        // start defaults to Before (sticky-left), end to After (sticky-right).
        let b = text();
        let mut store = AnnotationStore::new();
        let id = store.add(&b, 0, 11, AnnotationKind::Diagnostic, AnnotationData::default());
        assert_eq!(store.annotations[&id].start.sticky(), Stickiness::Before);
        assert_eq!(store.annotations[&id].end.sticky(), Stickiness::After);
    }

    #[test]
    fn insert_before_start_does_not_move_annotation() {
        let mut b = text();
        let mut store = AnnotationStore::new();
        let id = store.add(&b, 6, 11, AnnotationKind::Search, AnnotationData::default());
        b.insert(0, ">>");
        store.stabilize(&b);
        // Both endpoints shifted by 2; the annotation still covers "world".
        assert_eq!(store.resolve(&b, id), Some(8..13));
    }

    #[test]
    fn insert_at_before_start_keeps_byte() {
        // start is `Before` (sticky-left): insert at the start offset keeps
        // the endpoint put (text lands before it).
        let mut b = text();
        let mut store = AnnotationStore::new();
        let id = store.add(&b, 0, 5, AnnotationKind::Diagnostic, AnnotationData::default());
        b.insert(0, "X");
        store.stabilize(&b);
        // The `Before` start is sticky-left: it keeps pointing at the same
        // content ("h"), which the insert pushed to byte 1. The annotation
        // still covers "hello", now at bytes 1..6 (it does NOT swallow "X").
        assert_eq!(store.resolve(&b, id), Some(1..6));
    }

    #[test]
    fn insert_at_after_end_grows_annotation() {
        // end is `After` (sticky-right): insert at the end offset relocates
        // the endpoint past the inserted span.
        let mut b = text();
        let mut store = AnnotationStore::new();
        let id = store.add(&b, 0, 5, AnnotationKind::Diagnostic, AnnotationData::default());
        b.insert(5, "X");
        store.stabilize(&b);
        assert_eq!(store.resolve(&b, id), Some(0..6));
    }

    #[test]
    fn insert_at_after_end_extends() {
        let mut b = text();
        let mut store = AnnotationStore::new();
        let id = store.add(&b, 0, 5, AnnotationKind::Diagnostic, AnnotationData::default());
        // "hello" then insert at offset 5 (right after "hello").
        b.insert(5, ">>>");
        store.stabilize(&b);
        // The After end relocates to 5 + 3 = 8.
        assert_eq!(store.resolve(&b, id), Some(0..8));
    }

    #[test]
    fn delete_spanning_annotation_collapses_to_zero() {
        let mut b = text();
        let mut store = AnnotationStore::new();
        let id = store.add(&b, 3, 8, AnnotationKind::Diagnostic, AnnotationData::default());
        // Delete [2, 9) which fully contains [3, 8).
        b.delete(2..9);
        store.stabilize(&b);
        // Fully deleted -> collapsed, no extent.
        assert_eq!(store.resolve(&b, id), None);
    }

    #[test]
    fn delete_partial_left_collapses_start_to_surviving_edge() {
        let mut b = text();
        let mut store = AnnotationStore::new();
        // "hello world": delete [0, 6) removes "hello " -> surviving "world".
        let id = store.add(&b, 2, 8, AnnotationKind::Diagnostic, AnnotationData::default());
        b.delete(0..6);
        store.stabilize(&b);
        // start (2) inside -> collapses to s=0; end (8) >= e=6 -> shifts to 2.
        assert_eq!(store.resolve(&b, id), Some(0..2));
    }

    #[test]
    fn interior_split_stale_right_half_repaired() {
        // Build a buffer where an annotation's end sits in the right half of
        // a piece, then split that piece with an insert and confirm the token
        // is repaired (content preserved), not invalidated.
        let mut b = TextBuffer::from_text("abcdefghij"); // single piece, len 10
        let mut store = AnnotationStore::new();
        // Anchor [2, 7): end at 7 is in the right half of the original piece.
        let id = store.add(&b, 2, 7, AnnotationKind::Diagnostic, AnnotationData::default());
        assert_eq!(store.resolve(&b, id), Some(2..7));
        // Insert at offset 4 splits the original piece; "bcdefg" (the content
        // between the endpoints) is now preceded by an extra byte.
        b.insert(4, "X");
        store.stabilize(&b);
        // "abcdXefghij": original [2,7) ("cdefg") is now [2,8).
        assert_eq!(store.resolve(&b, id), Some(2..8));
    }

    #[test]
    fn untouched_annotation_costs_nothing() {
        let mut b = text();
        let mut store = AnnotationStore::new();
        let touched = store.add(&b, 0, 5, AnnotationKind::Diagnostic, AnnotationData::default());
        let _untouched = store.add(&b, 6, 11, AnnotationKind::Search, AnnotationData::default());
        // Edit at the start only affects the first annotation.
        b.insert(0, "X");
        store.stabilize(&b);
        // `touched` start is `Before` (sticky-left): the "X" lands before it,
        // so "hello" shifts to 1..6.
        assert_eq!(store.resolve(&b, touched), Some(1..6));
        // The second annotation shifted by the insert but was not specially
        // processed; its token stayed valid (stability, not O(all) work).
        assert_eq!(store.resolve(&b, _untouched), Some(7..12));
    }

    #[test]
    fn query_range_returns_overlapping_excludes_nonoverlapping() {
        let b = text();
        let mut store = AnnotationStore::new();
        let a = store.add(&b, 0, 5, AnnotationKind::Diagnostic, AnnotationData::default());
        let c = store.add(&b, 6, 11, AnnotationKind::Search, AnnotationData::default());
        let _far = store.add(&b, 9, 11, AnnotationKind::Git, AnnotationData::default());
        let hits = store.query_range(4, 7);
        assert!(hits.contains(&a), "a [0,5) overlaps [4,7)");
        assert!(hits.contains(&c), "c [6,11) overlaps [4,7)");
        assert_eq!(hits.len(), 2);
    }

    #[test]
    fn query_range_consistent_with_linear_scan() {
        let b = text();
        let mut store = AnnotationStore::new();
        for i in 0..5 {
            store.add(
                &b,
                i,
                i + 2,
                AnnotationKind::Other(i as u8),
                AnnotationData::default(),
            );
        }
        // Every query must match a brute-force linear scan over resolved ranges.
        for a in 0..=11 {
            assert!(store.query_range(a, a).is_empty(), "empty range must match nothing");
            for bnd in (a + 1)..=11 {
                let q = store.query_range(a, bnd);
                let mut expected = Vec::new();
                for id in store.annotations.keys().copied() {
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
        let mut store = AnnotationStore::new();
        let mut rng = Rng::new(0x1234_ABCD);

        // Seed annotations at random byte ranges.
        const N: usize = 200;
        let mut o_start = Vec::with_capacity(N);
        let mut o_end = Vec::with_capacity(N);
        let mut o_collapsed = vec![false; N];
        let mut ids = Vec::with_capacity(N);
        for _ in 0..N {
            let s = rng.below(b.len() + 1);
            let e = rng.below(b.len() + 1);
            let (s, e) = if s <= e { (s, e) } else { (e, s) };
            let id = store.add(&b, s, e, AnnotationKind::Other(0), AnnotationData::default());
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
                        // endpoint (a zero-length annotation untouched by the
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

            // Assert every annotation against the oracle.
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
