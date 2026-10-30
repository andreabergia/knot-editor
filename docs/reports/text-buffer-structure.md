# Text buffer data structure analysis

October 2026. Compares Knot's `TextBuffer` with the buffer structures of
other editors and recommends a replacement representation, tracked as D092 in
the [roadmap](../roadmap.md). Current behavior is owned by
[core and buffers](../architecture/core-and-buffers.md) and
`src/core/buffer.rs`; the rationale for the original choice is in
[decisions](../decisions.md#text-buffer-and-stable-positions).

## Summary

The piece table was the right model. Its stable piece IDs give Knot anchored
positions without a second marker structure, which no other surveyed editor
gets for free, and the anchored-range measurements confirmed the payoff.

The representation is the weak point. The piece chain is a flat
`Vec<Piece>` searched linearly from its start, the line index is a flat
`Vec<LineStart>` whose suffix is shifted on every edit, and snapshots copy the
piece list. Every operation except line lookup is O(pieces), and every edit
is also O(lines). Contiguous typing hides this because coalescing keeps the
piece count low, but replace-all, multi-cursor editing, formatting, and
language-server edit lists add pieces in bulk and multiply both costs.

Recommendation: keep the piece-table semantics, stable IDs, edit log, and
`TextBuffer` API, and store the pieces in a balanced, copy-on-write B-tree
whose nodes summarize byte length, line breaks, and UTF-16 length. This is
VS Code's piece tree with Zed's persistent nodes. It makes offset, line, and
UTF-16 lookups O(log n), removes the separate line index, and makes snapshots
O(1). Do it before or together with the first bulk-edit feature (Replace,
D001, or multi-cursor), after a benchmark confirms the projected costs.

## Current implementation

`TextBuffer` (`src/core/buffer.rs`) holds:

| Field | Content |
| --- | --- |
| `original: Arc<str>` | Load-time text, never copied or moved. |
| `sealed: Vec<Arc<str>>`, `tail: String` | Append-only add sink in 16 KiB chunks, shared with snapshots. |
| `pieces: Vec<Piece>` | The piece chain: `(id, backing, start, len)`. |
| `line_starts: Vec<LineStart>` | Every line start as a byte and UTF-16 offset. |
| `edits: Vec<BufferEdit>` | Edit log consumed by syntax and anchored ranges, then discarded. |

A `Position` is `(PieceId, offset)`. Interior edits split a piece: the left
half keeps the ID, the right half gets a fresh one, and the `Split` record in
the edit log lets `AnchoredRangeStore` move positions from the shortened piece
to the new one. Typing at the end of the tail extends the last piece instead
of adding one.

### Cost of each operation

`p` is the number of pieces, `L` the number of lines, `k` the edited length.

| Operation | Implementation | Cost |
| --- | --- | --- |
| Find offset (`split_at`, `position_at`) | Linear walk accumulating lengths | O(p) |
| Insert / delete a piece | `Vec::insert` / `drain` | O(p) memmove |
| Line index update | `splice` plus shifting every later entry | O(L + k) |
| `line_start`, `line_of_offset`, UTF-16 conversion | Binary search over `line_starts` | O(log L) |
| `read_range` / `chunks` | Walk from the first piece to the range | O(p + k) |
| `resolve(Position)` | Linear search for the piece ID | O(p) |
| `snapshot` | Copy pieces, compute `piece_ends`, copy tail | O(p) + up to 16 KiB |
| Snapshot `chunk_at` | Binary search over `piece_ends` | O(log p) |

### Where the costs land

- **Every commit in a highlighted buffer takes a snapshot.**
  `BufferModel::observe_committed_edits` passes `self.buffer.snapshot()` to the
  foreground parse, so each keystroke pays O(p) for the copy on top of the
  edit itself.
- **Anchored-range stabilization resolves endpoints one by one.**
  `AnchoredRangeStore::set_endpoint_pos` calls `buffer.resolve` for each moved
  endpoint, so an edit touching `a` endpoints costs O(a · p).
- **Painting reads lines near the end of a file through the whole chain.**
  `line_text` → `read_range` → `chunks` walks every piece before the visible
  range, so the cost depends on where the viewport is, not only on its size.
- **Every edit shifts the line index.** On the 1,001,395-line fixture, an
  edit measured p50 678 µs with the line index and 75 µs without it.

### Measured evidence

From [decisions](../decisions.md#text-buffer-and-stable-positions) and
[syntax highlighting](../decisions.md#syntax-highlighting), Apple M1 Pro
release builds:

- 1M-line, 29 MiB file: edit p50 678 µs, of which about 88% is the line-index
  suffix shift; positions issue and resolve in under a microsecond at low
  piece counts.
- 29 MiB file with 9,988 pieces after 5,000 typing bursts: snapshot p50 19 µs,
  p99 33 µs.
- 10,000 anchored ranges on a 1M-line buffer: stabilization p50 about 12 µs
  per edit; resolving all ranges about 1.79 ms.

These numbers are good for typing. None of them exercise a workload that adds
thousands of pieces at once.

### Projected bulk-edit cost

Replace-all over `m` matches is `m` replaces, each a delete plus an insert,
each adding up to two pieces. On the 1M-line file, the line-index shift alone
is about 0.6 ms per primitive edit, so 10,000 matches cost about
2 × 10,000 × 0.6 ms ≈ 12 s before counting the piece walk, which itself grows
to tens of thousands of pieces during the operation. A 1,000-match replace in
the same file still costs over a second. Multi-cursor typing with `c` cursors
is `c` primitive edits per keystroke, so 50 cursors in a large file would cost
tens of milliseconds per key. Undo and redo of these operations replay the
same number of edits. These are projections from the measured per-edit cost;
the benchmark in [validation](#validation) should confirm them.

## Survey of other editors

| Editor | Structure | Line lookup | Snapshots | Stable positions |
| --- | --- | --- | --- | --- |
| Emacs | Gap buffer | Scan from a cached nearby position | Full copy | Markers adjusted per edit; text properties in an interval tree |
| Neovim | Memline: B-tree of line blocks backed by a swap file | Native, by line number | None | Extmarks in a separate B-tree (`marktree`) |
| VS Code | Piece tree: piece table in a red-black tree | O(log n) via cached line-feed counts | Walk of the piece list | Decorations in a separate interval tree |
| Zed | SumTree rope plus a CRDT fragment tree | O(log n) via summaries | O(1), shared `Arc` nodes | Anchors on CRDT insertion IDs, resolved in O(log n) |
| Helix | Ropey rope | O(log n) via summaries | O(1), shared `Arc` nodes | None; positions mapped through each change set |
| Xi / Lapce | `xi-rope` B-tree rope | O(log n) | O(1) | Xi used a CRDT engine on top |
| Atom | `superstring`: base text plus splay-tree patches | Tree lookup | Frozen patch layers | Marker index in a splay tree |
| Kakoune | Vector of reference-counted lines | Native | Shares unchanged lines | Selections adjusted per change |
| Sublime Text | Closed source; internals are not publicly documented | | | |
| Knot | Stable-ID piece table in a `Vec`, separate line-start `Vec` | O(log L), but O(L) maintenance per edit | O(p) copy | Built in: `(piece ID, offset)` |

### Emacs

A single gap buffer with the gap moved to the edit point. Edits near the gap
are O(k); moving the gap is O(distance). Line numbers are found by scanning
for newlines with position caches, which is why very long lines and large
files were historically pathological until the Emacs 29 long-line work.
Markers are adjusted on every edit. The structure is simple and fast for
local editing but offers no snapshots: anything off the main thread copies
the text.

### Neovim

Inherits Vim's memline: lines grouped into data blocks under a tree of pointer
blocks, paged to a swap file that doubles as crash recovery. Line access is
native and cheap; byte offsets are derived. Extmarks live in a dedicated
B-tree that edits adjust. There are no immutable snapshots; the buffer-update
API streams changes to consumers such as LSP clients instead.

### VS Code

The 2018 text-buffer rewrite replaced an array of line strings with a piece
tree: a piece table whose pieces sit in a red-black tree. Each node caches the
length and line-feed count of its left subtree, so offset and line lookups are
O(log n), and each original or added buffer keeps its own line-start array, so
a piece's line count comes from binary search instead of scanning. The rewrite
was motivated by memory and load time on large files. Positions are not stable
identities; decorations live in a separate interval tree adjusted on edit.
This is the closest existing design to the recommendation, minus persistence.

### Zed

Text is a `SumTree`: a persistent B-tree whose items carry summaries (byte
length, UTF-16 length, line/column point) and whose nodes are shared through
`Arc`, so snapshots are O(1) and background work reads them freely. For
collaboration, a second tree of fragments ordered by dense `Locator` keys
records every insertion with its CRDT timestamp; an anchor is
`(insertion ID, offset, bias)`, resolved in O(log n) by finding the insertion's
fragment and seeking the text tree. This is the most capable design surveyed
and also the most complex, because collaboration needs the full CRDT.

### Helix

Uses the Ropey crate: a B-tree rope over UTF-8 leaf chunks tracking bytes,
chars, UTF-16 code units, and line breaks, with `Arc` nodes for O(1) clones.
Positions have no identity; selections and other ranges are mapped through
each transaction's change set, and history is an undo tree. Simple and fast,
but every position consumer must replay changes, which is what Knot's
anchored ranges avoid.

### Atom

Originally an array of lines in JavaScript, replaced by the native
`superstring` library: a base text plus layers of `Patch`, a splay tree of
changes in old and new coordinates. Freezing a layer gave background workers
(search, saving) a consistent snapshot. Markers lived in a separate splay-tree
index. Atom is discontinued, but the layering is a good example of cheap
snapshots without a persistent tree.

### Sublime Text

Closed source, and its buffer representation has not been publicly
documented, so this report draws no conclusion from it.

## Options

### A. Patch the current representation

Replace the line index with a Fenwick tree or chunked index so updates are
O(log L), add batched edit application for bulk operations, and resolve
endpoints in one ordered pass during stabilization.

Smallest change, but it attacks symptoms one at a time: the piece walk,
`Vec` memmove, and O(p) snapshots remain, and each fix adds its own index to
keep consistent.

### B. Piece tree with summaries (recommended)

Store pieces in a B-tree. Each node caches the sum of its subtree's byte
length, line breaks, and UTF-16 length. Nodes are `Arc`-shared and mutated
with copy-on-write, so the foreground edits in place when no snapshot is
outstanding and a snapshot is a root clone.

- Offset, line, and UTF-16 lookups become one O(log n) seek. `line_starts`
  disappears, and with it the O(L) shift.
- Insert, delete, and split are O(log n) plus the edited length.
- `snapshot` is O(1) plus the unsealed tail.
- Piece semantics are unchanged: same backing store, split rule, coalescing,
  `BufferEdit` log, and `Split` records, so `AnchoredRangeStore`,
  `EditTransaction`, syntax, and history need no changes.

Leaves need a line-break count per piece. Each backing chunk can keep its own
line-start array, as VS Code does, so a piece's line breaks and UTF-16 length
come from binary search over its chunk instead of scanning its text.

The one new mechanism is `resolve(Position)`, which must find a piece's
current offset from its ID. A persistent tree cannot hold parent pointers, so
use Zed's technique: give each piece an ordered `Locator` (a dense key chosen
between its neighbors at creation), keep a map from `PieceId` to `Locator`,
and order the tree by locator. Resolving is a map lookup plus an O(log n) seek
by locator. Splits give the right half a fresh locator between the left half
and its old successor.

### C. Adopt a rope crate

Use Ropey or `crop` for the text and drop pieces. Lookups and snapshots
become O(log n) and O(1) with less code to own.

Positions lose their identity: anchors would have to be remapped through the
edit stream, as Helix does, giving up the stabilization design and its
measured advantage, or rebuilt on a separate ID layer, which is option B with
extra steps. Rejected.

### D. Gap buffer or CRDT

A gap buffer has no stable positions or snapshots. A CRDT pays for
collaboration, which is a [design non-goal](../design.md#non-goals). Rejected.

## Validation

Before implementing, add a benchmark harness for `TextBuffer` and record the
current numbers; after implementing, rerun it on the same machine:

- typing bursts in the 1M-line, 29 MiB fixture (edit p50/p99);
- replace-all of 1,000 and 10,000 matches spread through the same file;
- 50 cursors typing 100 characters;
- snapshot cost at 10k and 100k pieces;
- stabilizing and resolving 10,000 anchored ranges at 10k and 100k pieces;
- reading the last screen of lines at 100k pieces;
- resident memory per piece.

Acceptance: typing stays at or below today's p50, the 10,000-match replace-all
drops from seconds to tens of milliseconds, and snapshot and resolve costs no
longer grow with the piece count beyond O(log n).

## Risks

- **Correctness of the tree.** The existing `buffer/tests.rs` and anchored
  range tests cover the public behavior and carry over unchanged. Add a
  randomized test that applies the same edit sequence to the tree and to a
  plain `String`, checking text, line lookups, UTF-16 conversion, and position
  resolution after each step.
- **Small-file overhead.** A tree adds constant overhead for tiny buffers.
  Leaves of 16–64 pieces keep a fresh file at a single leaf.
- **Bulk-edit API.** The tree fixes per-edit cost, but replace-all still pays
  one edit-log entry and one anchored-range stabilization step per primitive.
  That is O(m log n) and acceptable; a batched primitive is not needed unless
  benchmarks show otherwise.
