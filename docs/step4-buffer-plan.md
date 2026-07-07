# Step 4 — Buffer Model Plan

Execution plan for roadmap step 4 (Buffer model). Records the decisions
that scope the prototype before code is written, so later steps can
rely on them. Step 4's question is:

> Is there a buffer representation that is fast enough and exposes
> stable enough positions for the annotation layer?

## Decisions

### D1 — Build, don't reuse

`TextBuffer` is hand-rolled, not a wrapper over ropey/crop. Reason: the
prototype exists to *validate* the representation claim, and the
annotation layer (step 5) needs to probe the representation directly
(stable tokens, edit log, internal invariants). Wrapping a crate
opaque answers "is it fast enough" but not "are positions stable
enough for our annotation model." A hand-rolled piece table performs
at memory-bandwidth limits on the operations we care about; the risk
is implementation bugs, not throughput.

### D2 — Piece table with stable IDs

Backing store is a piece table whose pieces carry monotonically
increasing stable IDs. Chosen over a rope:

- **Stable positions are the load-bearing feature.** The annotation
  layer (step 5) needs positions that survive edits without a remap
  pass. A stable-ID piece table gives opaque `Position` tokens for
  free: a position is `(piece_id, offset_within_piece)`, stable across
  unrelated edits, and resolves to a byte offset on demand.
- A rope answers only the throughput question; positions would have to
  be remapped after every edit, which is exactly what step 5 is trying
  to avoid.
- A char-index rope (ropey) invalidates *all* offsets after an edit
  point — the worst case for 10k annotations on a 1M-line buffer.

This couples step 4's representation choice to step 5's stress test,
which is intentional: the whole point of step 4 is to pick a
representation step 5 can lean on. If step 5 reveals the stable-token
model is insufficient, both steps reopen.

### D3 — Offset units

- **Internal offset:** UTF-8 byte offset into the logical text. Used
  for all buffer-internal offsets and the public byte-offset API.
- **LSP boundary:** `TextBuffer` exposes `to_utf16(offset)` /
  `from_utf16(utf16)` for the LSP adapter. Conversion is cheap and
  only ever needed at the extension boundary.
- **User/column positions:** `(line, utf16_col)` is a *view* concept,
  not a buffer concept. The buffer does not maintain columns; it
  exposes byte-offset and a line index. Views compute columns on
  demand.
- **Grapheme clusters** are a step-7 / view concern (per the step-3
  caveat). The buffer operates at byte granularity; step 5's
  annotations declare their own stickiness in byte terms.

### D4 — Line index

- Text is stored **raw**, no `\r` normalization, no line-ending
  unification. Mixed CRLF/LF is preserved.
- A **line index** maps `line_number → byte_offset_of_line_start` and
  is built lazily on first query, then maintained incrementally
  under edits.
- Line-ending detection for "how many lines" and "what column is
  this offset" uses `\n` (counting a preceding `\r` as part of the
  preceding line, not as a separate terminator).
- The line index is the only secondary index maintained; anything
  richer (word index, UTF-16 cache) is built outside the core by
  features that need it.

> **Implementation note:** the line index is a `Vec<usize>` of sorted
> line-start byte offsets. Edits keep it sorted: binary-search the
> affected range, splice the K-entry delta for the edited span, then
> fix up the suffix in O(n−i). If the suffix shift shows up as a
> bottleneck, the backing swaps to a Fenwick tree or `BTreeMap` for
> O(log n) per edit. The index is built lazily on first query after
> load or after any edit that dirtied it; between builds it is kept
> valid by incremental update, not full rebuild.

### D5 — Single-threaded core

The "concurrent-ish edit sequences" benchmark bullet in the roadmap
is dropped. The core assumes a single gpui thread; interleaved edits
from two feature sources (the realistic case the design cares about)
is modelled by *alternating* calls on that thread, not by true
parallelism. Multi-threaded edits are out of scope for the prototype
and arguably out of scope for the editor as a whole (single embedded
runtime).

### D6 — Scope

**In scope for step 4:**
- `TextBuffer` with stable-ID piece table backing.
- `insert` / `delete` / `replace` / `read_range` / `byte_offset` random access.
- `Position` token (opaque, stable across unrelated edits) and an
  edit log exposing per-edit `BufferEdit` events (see D7 for the struct) so
  step 5 can subscribe and refine.
- Line index (lazy + incremental).
- Internal benchmark: 1M-line fixture, randomized edit streams,
  interleaved-producer scenario, per-edit p50/p99 + RSS.

**Deferred (do not implement in step 4):**
- Undo / redo history.
- CRDT / collaborative editing.
- Non-UTF-8 encodings.
- File save / load I/O.
- Syntax-tree / Tree-sitter integration.
- Annotation model itself (step 5).
- Views, rendering (step 7).
- Persistence semantics (roadmap out-of-scope).

The deferred list is long on purpose: step 4 is the smallest
representation that can answer the throughput-and-stability question.

### D7 — Piece identity on split and merge

**Split:** when an edit lands at the interior of a piece (offset `s`,
`0 < s < piece.len`), the piece splits and **the left half retains the
old id; the right half gets a new, monotonically increasing id.**

This is forced by the position model, not a preference. Pieces are
references `(buffer, start, len)` fixed at creation; a `Position` is
`(piece_id, offset_within_piece)`. Whichever half keeps the id
*keeps its original `(start, len)`* and therefore its original content.

- **Left-retains-id:** positions with `offset < s` stay valid **by
  content** with no remap. Positions with `offset ≥ s` become
  out-of-bounds (the shortened piece's new length is `s`), but the
  content they pointed to is now in the new right piece at offset
  `old_offset - s` — a deterministic remap step 5 can apply from the
  edit log.
- **Right-retains-id (rejected):** the kept id now refers to a
  *different* `(start, len)`, so every previously-issued `(P, k)` for
  `k < s` lands on **wrong content**, not a different-but-orderable
  position. Silent content drift is worse than detectable staleness,
  and there's no way to remap issued tokens after the fact (the offset
  `k` is meaningful only against the original slice).

**Degenerate-split elision:** if the split point is at offset 0 or
`piece.len`, splice the new piece in directly without splitting. This
avoids the asymmetric worst case (split-at-0 with the naive rule
would shorten P to len 0 and move all content — and all positions —
to a new piece, invalidating every position in P). After elision,
splits only happen at strict interiors where both halves are
non-empty and the rule is clean.

**Never merge.** Adjacent pieces with contiguous backing are *not*
coalesced, even when trivially mergeable. Coalescing would discard
one piece id and invalidate every outstanding `Position` held against
it. The piece chain only grows; compaction is deferred behind a
benchmark number and can be added later behind a flag without
changing the API.

**Edit log surface** (the only thing step 4 exposes about its
internals to step 5):

```rust
enum BufferEdit {
    Insert { at: usize, inserted_len: usize, splits: Vec<Split> },
    Delete { range: Range<usize>, splits: Vec<Split> },
}
struct Split { old_piece: PieceId, split_offset: usize, new_piece: PieceId }
```

**Concrete types:**
```rust
pub type PieceId = u64;  // monotonically increasing, global counter
pub struct Position {
    piece: PieceId,
    offset: u32,  // offset within piece (< 4 GiB)
}
```

`splits` is normally length 0 (edit on a boundary) or 1 (interior
edit). Recording it is enough for step 5 to remap any stale position
in the right half of a split, deterministically, without step 4 ever
touching its callers' position tokens.

**Default stickiness (silent policy):** a position whose owner does
nothing on edit is *sticky-left*. If it sits in the left half of a
split, it stays put; if it sits in the right half, it's stale-but-
detectable until step 5 remaps it. Sticky-right is step 5's job: read
the log, relocate. The asymmetry is intentional — silent
wrong-content (the alternative) is worse than silent stale-but-
detectable.

## Benchmark harness

Reuse the step-2 fixture corpus where possible. The 1M-line workload
is the existing `rust_sample.kfx --tile 635` path (see
`bench/README.md`). The fixture format (`src/view/fixture.rs`) loads
raw line text usable as buffer input; the segment-table portion is
renderer-only and ignored here.

Add a `[[bin]]` entry to `Cargo.toml`:

```toml
[[bin]]
name = "core-bench"
path = "src/bin/core_bench.rs"
```

This binary links `knot` as a library and drives the piece-table workload,
reusing the CLI argument style from `bench` (`--fixture`, `--tile`, `--duration`)
but measuring per-edit latency and RSS instead of frame time.
Output written to `docs/step4-buffer-benchmark.md` once the run completes.

### Piece-table construction from fixture

The initial buffer is built from a fixture's raw lines joined by `'\n'`,
forming a single contiguous UTF-8 byte buffer. The initial piece table
contains one piece referencing the full buffer. On load, the line index
is empty (lazy — built on first query). No `\r` normalization is
performed (per D4).

## Sequenced work

1. `TextBuffer` skeleton: piece table with stable IDs, insert / delete
   / replace / read_range / byte_offset. No line index, no positions
   yet.
2. Line index (lazy build + incremental update under edits).
3. `Position` token + `BufferEdit` edit-log surface, minimal API
   designed so step 5 can stress it without a rewrite.
4. Benchmark harness + randomized edit streams on the 1M-line fixture;
   write up findings.
5. **Decision checkpoint.** Based on throughput numbers and how the
   position/remap surface felt, confirm rope-vs-stable-ID-PT or reopen.
   Write the answer into `roadmap.md` step 4 and proceed to step 5.

## Carry-forward risks

- **Edit log granularity.** A per-edit `BufferEdit` may not be enough
  for step 5's sticky-before / sticky-after semantics; step 5 may ask
  for richer per-piece provenance. The API is intentionally minimal so
  this can grow without breaking step 4's tests.
- **Stable-token resolution cost.** Resolving a `(piece_id, offset)`
  to a byte offset requires walking the piece table's offset index.
  If this is hot for 10k annotations per frame, step 5 will surface it
  and we add a cached resolver — but not before the benchmark proves
  it's needed.
- **Piece ID churn on split/merge.** Settled in D7: left keeps id on
  split, never merge. Positions in the right half of a split are
  reconstructable from the edit log; step 5 reads the log and remaps.