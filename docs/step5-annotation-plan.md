# Step 5 — Annotation Offset Stabilization Plan

Execution plan for roadmap step 5. Records the decisions that scope the
prototype before code is written, so the representation choice and the
semantics exposed to extensions are settled before the stress test.

> **Question:** can annotations track positions cheaply and correctly
> enough to be the universal composition mechanism the design claims?

Step 4's buffer already answers the load-bearing sub-question — stable
`Position` tokens + an edit log survive edits with two orders of
magnitude of headroom (`docs/step4-buffer-benchmark.md`, §Findings 1).
Step 5's job is to build the *annotation layer* on top of that surface,
decide its representation, pin down sticky-boundary semantics, and prove
it stays cheap and correct at 10k annotations on a 1M-line buffer under
a randomized, multi-source edit workload.

## Decisions

### D1 — Representation: `Position`-token anchors over a stable-ID piece table

Each annotation stores its endpoints as **`Position` tokens** issued by
`TextBuffer::position_at`, not as raw byte offsets. This is the
"stable-ID piece table" candidate from the roadmap, and it is *the same
mechanism the buffer already uses for internal stability* — annotations
inherit piece-ID stability for free. The roadmap's three listed
candidates collapse as follows:

- **stable-ID piece table** → the chosen representation (D1).
- **interval tree** → the *query index* over the representation (D3),
  not an alternative representation. It is keyed by live resolved byte
  offsets and rebuilt incrementally under edits.
- **offset remap log** → the measurement baseline only (D5), used to
  quantify how much worse a naive offset-based approach would be. Not
  adopted as the production representation: it forces a full remap pass
  per edit and throws away the stability step 4 bought us.

So step 5 builds exactly one production representation (token anchors +
interval index) and measures it against the offset-remap baseline to
answer "which representation."

### D2 — Sticky-boundary semantics

An annotation endpoint is an `Anchor`: a `Position` plus a per-endpoint
stickiness. The buffer's `Position` is **sticky-left by default** (D7):
an insert at the endpoint's byte offset is boundary-elided and the token
stays put, so the *inserted text lands before* the endpoint. That gives
the standard editor behavior for a selection *start* (the start does
not grow to swallow text typed at it). We add a **sticky-right** mode
for endpoints that *should* grow to include inserted text at their
offset (the standard selection *end*, and the natural default for a
diagnostic whose range extends as you type after it).

```rust
pub enum Stickiness { Before, After }
// `Before` == sticky-left (default): inserted text at the offset lands
//   *before* the endpoint; the endpoint keeps its byte.
// `After`  == sticky-right: inserted text at the offset lands *after*
//   the endpoint; the endpoint relocates past the inserted span.
pub struct Anchor { pos: Position, sticky: Stickiness }
```

Boundary behaviors per edit (resolved against the post-edit buffer):

- **Insert at `at`, length `n`**, an anchor resolves to offset `at`:
  - `Before` → unchanged (text inserted before it).
  - `After`  → relocate to `position_at(at + n)` (text inserted after it).
- **Delete `[s, e)`**, an anchor resolves inside `(s, e)`:
  - `Before` / `After` both collapse to `s` is wrong for `After` —
    instead clamp to the *nearer* boundary: `Before` keeps `s`,
    `After` keeps `e` (i.e. the endpoint snaps to the surviving edge on
    its side). Both endpoints clamping to `s` shrinks the annotation to
    zero; to zero-length we keep `start <= end` by letting
    `end` follow `start`.
- Anchors resolving strictly before `s` or strictly after `e` are
  untouched (this is the whole point of token stability).

Stale right-half tokens (D7) are detected via `resolve` returning
`None` and repaired with the edit-log remap
`(old_piece, k) → (new_piece, k - split_offset)`; a `Before` endpoint on
a right-half split keeps its byte content (the remap *is* the
sticky-before behavior), so no extra stickiness logic is needed there.

### D3 — Query / composition index (interval tree)

Step 6 needs "annotations overlapping byte range `[a, b)`" cheaply
(e.g. for the minimap, gutter, and composing diagnostics+search+git).
The annotation store maintains an **interval tree** (or a sorted
`Vec<(start, end, id)>` with binary-search query for the prototype — the
tree is only worth it if queries outrun `O(log n)`; benchmark decides)
keyed by *live resolved byte offsets*. On every `stabilize`, endpoints
that moved have their keys updated in the index. The `Position` tokens
remain the source of truth; the index is a derived cache rebuilt from
the tokens, so it can never drift in correctness — only in query
latency, which we measure.

### D4 — Subscription model: one authoritative edit cursor

The store owns a single `cursor: usize` advanced by `edits_since(cursor)`
on each `stabilize`. It assumes **no other caller drains the log**
(`take_edits` is reserved for the store or for the bench's
single-owner scenario) — otherwise a `Split` record needed to repair a
stale token would be lost. This is consistent with D5 (single-threaded
core) from step 4. Documented as a hard invariant; the store panics if
it detects the log was shortened underneath it (`edit_seq()` dropped
below `cursor`).

### D5 — Baseline comparator (measurement only)

The bench harness also runs a **naive offset-remap** store: annotations
keep raw byte ranges and, on each edit, walk `edits_since` applying the
same insert/delete transforms to stored offsets (no piece IDs). This
quantifies the per-edit stabilization cost of the offset-log approach
vs the token approach, answering the "which representation" question
without standing up a second production module. Expected result: token
approach is O(affected) per edit and ~free for untouched annotations;
offset approach is O(all annotations) per edit.

### D6 — Scope

**In scope for step 5:**

- `AnchoredRangeStore` over `Position`-token anchors (D1).
- `Anchor` with `Stickiness` (D2), including stale-token repair via the
  edit log.
- Interval query index (D3).
- Stabilization under a randomized edit stream on the 1M-line fixture,
  with N independent annotation *sources* (step 4's `interleaved`
  two-producer model generalized to N) sharing one edit log.
- Correctness oracle: a brute-force replay on a `String` copy
  (re-apply the same insert/delete stream, recompute every annotation's
  expected byte range from its original offsets) compared against the
  store's resolved ranges after `stabilize`. Must match for every
  annotation, every sticky mode, every edit in the stream.
- Benchmark: per-edit stabilization cost (p50/p99), per-frame
  `resolve`-all cost at 10k annotations, RSS, and the D5 baseline delta.

**Deferred (do not implement in step 5):**

- The composition/rendering itself (roadmap step 6) — only the query
  surface it needs is built here.
- Multiple views / folding (step 8).
- Grapheme-cluster granularity for sticky endpoints (step 3 caveat /
  step 8) — annotations declare stickiness in *byte* terms, as do
  `Position` tokens (D3 of step 4).
- Undo/redo and persistence — unchanged from step 4 deferrals.
- CRDTs and real-time collaborative editing remain an explicit project
  non-goal.

## Type sketch (target API)

```rust
pub type AnchoredRangeId = u64;

pub enum Stickiness { Before, After }       // D2
pub struct Anchor { pos: Position, sticky: Stickiness }

pub struct AnchoredRange {
    pub id: AnchoredRangeId,
    pub start: Anchor,
    pub end: Anchor,
}

pub struct AnchoredRangeStore {
    anchored_ranges: HashMap<AnchoredRangeId, AnchoredRange>,
    endpoints_by_piece: BTreeMap<PieceId, BTreeSet<EndpointKey>>,
    cursor: usize,              // edit_seq() high-water mark (D4)
    next_id: AnchoredRangeId,
}

impl AnchoredRangeStore {
    pub fn new() -> Self;

    /// Issue two `Position` tokens via `position_at`; start defaults to
    /// `Before`, end to `After` (standard selection semantics).
    pub fn add(&mut self, buffer: &TextBuffer, start: usize,
               end: usize) -> AnchoredRangeId;

    /// Advance `cursor` over `buffer.edits_since(cursor)`, repairing only
    /// anchors whose pieces split, receive an insert-boundary relocation,
    /// are deleted, or become temporarily unanchored in an empty buffer.
    pub fn stabilize(&mut self, buffer: &TextBuffer);

    /// Resolve an anchored range to its current byte range; `None` iff a
    /// token's piece was deleted *and* no surviving endpoint could be
    /// recovered (range fully inside a deleted span → zero range).
    pub fn resolve(&self, buffer: &TextBuffer, id: AnchoredRangeId) -> Option<Range<usize>>;

    /// All anchored-range ids overlapping `[a, b)` (D3, for step 6). Current
    /// prototype implementation resolves live token ranges linearly; a
    /// byte-offset interval cache is deferred until query latency dominates.
    pub fn query_range(&self, buffer: &TextBuffer, a: usize, b: usize) -> Vec<AnchoredRangeId>;

    pub fn remove(&mut self, id: AnchoredRangeId);
}
```

## Stabilization algorithm (per `stabilize`)

1. Fetch `edits = buffer.edits_since(self.cursor)`; if
   `buffer.edit_seq() < self.cursor` → panic (D4 invariant violated).
2. For each new `BufferEdit`, for each annotation whose *either* endpoint
   resolves (via `resolve`) to a byte in the edit's affected span
   `[at, at+inserted_len)` / `[range.start, range.end)`, or whose
   endpoint token is stale (`resolve` → `None`):
   - **Repair staleness:** walk `edits` (`split_offset < offset`)
     applying `(old_piece, k) → (new_piece, k - split_offset)` to the
     stale `Position` (D7).
   - **Apply stickiness (D2):** for `Insert{at, n}`:
     `Before` endpoint at `at` → unchanged;
     `After` endpoint at `at` → `pos = position_at(at + n)`.
     For `Delete{[s,e)}`: endpoint inside → snap to `s` (`Before`) or
     `e` (`After`); if both endpoints collapse, force `start <= end`.
3. Re-key the moved endpoints in the interval index; untouched
   annotations cost nothing (token stability).
4. `self.cursor += edits.len()`.

## Sequenced work

1. [✅] `core/anchored_range.rs`: `Stickiness`, `Anchor`, `AnchoredRange`,
   `AnchoredRangeStore::new` / `add` / `remove`. `add` issues `Position`
   tokens via `position_at`. Unit
   tests: add/remove, `position_at` boundary anchoring, token stability
   across unrelated edits (reuse step-4 `Position` tests as a harness),
   `resolve` round-trips.
2. [✅] Stabilization core (`stabilize`, D2 + D7 repair): affected-anchor
   implementation is in place. The store indexes endpoints by stable piece
   id, repairs split right halves, relocates insert-boundary anchors, snaps
   endpoints from deleted pieces to surviving edges, and tracks the empty
   buffer's temporary unanchored endpoint state. Untouched annotations are
   not scanned during `stabilize`; they resolve through stable `Position`
   tokens. Unit tests:
   - insert at a `Before` start → annotation does not grow.
   - insert at an `After` end → annotation grows to include.
   - delete spanning an annotation → collapses to zero at `s`.
   - interior split stale right-half token repaired via log, content
     preserved at the resolved-range level.
   - two annotations, one untouched by an edit, resolves correctly through
     token stability.
   - zero-width boundary annotations survive full-buffer deletes and
     re-anchor when text is inserted into the empty buffer.
3. [✅] Query surface (D3): `query_range` now uses a sorted interval index
   (`Vec<IntervalEntry>`) rebuilt lazily on the first query after edits. The
   index is derived from `Position` tokens (never the source of truth) and
   provides O(log n + k) binary-search lookup. `stabilize` stays O(affected)
   — the index rebuild is deferred to `query_range` (now `&mut self`). Unit
   tests: overlapping ranges returned; non-overlapping excluded; query stays
   consistent after unrelated shifts; fast path matches a linear-scan oracle.
4. [✅] Correctness oracle in the bench: brute-force `String`-replay that
   recomputes every annotation's expected byte range from original
   offsets + the same edit stream; assert equals `store.resolve` after
   each edit for every annotation, every `Stickiness` combination. This
   is the real "correctness under concurrent feature sources" gate.
5. [✅] Benchmark harness (`src/bin/anno_bench.rs`, `[[bin]]`):
    - Reuse step 4's `rust_sample.kfx --tile 635` 1M-line fixture and
      `build_buffer` path.
    - N annotation sources (default 3, then 6 to foreshadow step 6),
      each seeding ~3.3k annotations (→ 10k total) at random byte
      ranges; sources issue interleaved edits (step 4 `interleaved`
      model generalized to N producers on one thread, D4/D5 of step 4).
    - Workloads: `stabilize` (per-edit p50/p99 over the stream),
      `resolve-all` (resolve every annotation, 10k/frame, p50),
      `query` (random `query_range` hits), `baseline-offset-remap`
      (D5 naive comparison). Output one TSV line per workload; wrote
      findings to `docs/step5-annotation-benchmark.md`.
    - Measure RSS with 10k annotations live.
    - ✅ Harness exists and smoke-runs.
    - ✅ Full 1M-line / 10k-annotation findings recorded in
      `docs/step5-annotation-benchmark.md`.
    - ✅ Smoke and full-run data confirm stabilization beats the offset-remap
      baseline: 5.7× faster at 10k annotations, 6 sources indistinguishable
      from 3. Query p50 ~15.5 µs with the interval index. Resolve-all
      p50 ~1.79 ms for 10k annotations (~179 ns per annotation).
6. [✅] Real affected-anchor implementation:
   - ✅ Replace the full-pass `apply_edit_all` / `reindex_all` path with an
     edit-driven anchor index keyed by piece id and endpoint offset.
   - ✅ Use `BufferEdit` metadata (`inserted_piece`, `left_piece`,
     `pre_first_piece`, `deleted_pieces`, `deleted_piece_lens`,
     `left_survivor`, `right_survivor`, `splits`) to repair only anchors
     touched by an edit or made stale by a split/delete.
   - ✅ Leave untouched annotations unscanned during stabilization.
   - ✅ Keep the current oracle and query linear-scan tests as the semantic
     guardrail while optimizing.
    - ✅ Re-run the full 1M-line / 10k-annotation benchmark and wrote
      `docs/step5-annotation-benchmark.md`.
7. [✅] Decision checkpoint: `roadmap.md` step 5 filled in with the chosen
    representation (token anchors, D1) and the extension-facing semantics
    (`Stickiness::{Before,After}`, defaults documented). The "which
    representation" question is answered.

## Carry-forward risks

- **Query indexing remains intentionally simple.** A live byte-offset
  interval index is not free with token-stable annotations: inserts and
  deletes before an annotation shift its resolved byte range even when its
  token does not move. The current query surface resolves token ranges
  linearly to keep stabilization `O(affected)`. If step 6 makes query
  latency hot, add a buffer-aware interval cache with explicit invalidation
  or a piece-indexed query structure.
- **Edit-log retention vs `take_edits`.** The store's stale-token
  repair needs the `Split` records (D4/D7). If any other subsystem
  drains the log, repair silently loses information. Mitigation: the
  store is the sole owner of `take_edits` in the prototype; documented
  invariant, with a panic if `edit_seq()` regresses. Revisit if step 6
  or step 10 needs to drain independently (solution: a shared
  pub/sub cursor registry, or copy-on-subscribe edit log).
- **Interval index structure.** A sorted `Vec` is fine until queries
  outrun `O(log n)`; the benchmark will tell us. Swapping to a real
  interval tree is contained behind `query_range`.
- **`Position` resolution cost at 10k/frame.** Step 4 measured `resolve`
  sub-µs on warm chains; 10k annotations × sub-µs ≈ low-ms/frame, within
  budget. If a session pushes tens of thousands on a 10k+ piece chain,
  add a cached resolver (step 4 carry-forward). Not before the bench
  proves it.
- **Line-index upgrade still deferred.** Step 4's line-index `Vec`
  remains the only bottleneck; step 5 does not need `line_*` queries
  per annotation, so the swap stays deferred until step 6/7 surfaces a
  need (step 4 recommendation 3).
