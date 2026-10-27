# Editor view data model and piece-table quick wins

Status: 🚧 approved, in progress. Covers health-review findings P0-1, P1-1, and the CRLF
item of P2-7 ([review](codebase-health-review.md)), the first entry in the
[roadmap's selected work](../roadmap.md#selected-work). It must land before
D002 (syntax highlighting) and D031 (current-file Find), which both build on the
view's line model.

## Problem

Typing in a large file costs O(document size) per keystroke, per view, even
though the core buffer itself measured p50 678 µs on a 29 MiB file.

`EditorView` (`src/app/editor.rs`) keeps a private copy of the whole document as
`lines: Vec<String>` with a parallel `segs: Vec<Vec<Seg>>`. On every model
notification it:

- compares `lines.join("\n")` with `model.text()` (two full copies);
- rebuilds the projection from a third copy, allocating one `String` and one
  `Vec<Seg>` per line;
- rebuilds a `line_starts` table to place contributions.

`splice` repeats the rebuild after its own edit, so the editing view pays twice.
Several paths outside the observer are also whole-document:

| Path | Cost today |
| --- | --- |
| `to_flat_byte`, `flat_byte_to_position`, `to_flat_utf16`, `flat_utf16_to_position` | linear scans over `lines` |
| IME `text_for_range`, `resolve_replacement_range`, `replace_and_mark_text_in_range` | `flat_doc()` plus a UTF-16 scan from offset 0 |
| Delete backward/forward, word movement | grapheme or word iteration over `flat_doc()` |
| Copy/Cut, `restore_view_state`, `start_completion` | `flat_doc()` or `model.text()` |
| Scroll wheel | shapes **every line in the document** to find the widest one |
| Undo/redo (`BufferModel::replay_history`) | two `text()` copies diffed by `replacement_between` |

`Seg` (`color/bold/italic`) is left over from the syntax-highlight prototype;
every line has one default segment.

In the core (P1-1), `TextBuffer::len()` sums all pieces and runs inside the
assertions of every insert, delete, read, and `position_at`. Typing appends one
piece per character, and nothing coalesces them, so `resolve`, `position_at`,
`split_at`, `read_range`, and `is_char_boundary` slow down with session length.
The edit log also grows without bound.

CRLF files are stored verbatim, but the editor normalizes inserted text to `\n`,
so editing a CRLF file produces mixed line endings (P2-7).

## Goals

- Typing, movement, selection, IME, and painting cost O(log lines + visible
  text) per view, not O(document).
- The view holds no copy of the document text. It reads lines from the model on
  demand.
- Core costs stop growing with session length in the common typing case.
- Inserted newlines match the file's line ending.
- A test guards against the view copying the whole document again.

Non-goals: a rope or tree-based piece table (D013/D014 stay deferred), soft
wrap, syntax highlighting (D002), splitting `editor.rs` into input, movement,
and painting modules (moved to the roadmap's maintenance entry), and changing
the persistent view-state format.

## Design

### Core line index is always maintained

`TextBuffer` builds its line index at construction and updates it on every edit,
instead of building it lazily on the first query. Line queries become `&self`
(`line_count`, `line_start`, `line_of_offset`), so `BufferModel` can expose them
through `&self` for views. The one-time scan at load is O(text), which loading
already pays. The incremental update is the suffix shift that the decision
record measured at p50 678 µs on one million lines.

The index also records each line's starting **UTF-16 offset**. The platform IME
contract speaks document-wide UTF-16 offsets, and without an index every IME
callback must scan from the start of the document. The UTF-16 starts shift with
the same splice as the byte starts. A delete reads the removed span once to
compute its UTF-16 length.

This revisits the exploration's "keep the editable core UTF-8-native" choice
only in scope, not in substance: text storage, edits, positions, and every
public offset stay UTF-8 bytes, and this is a coordinate index beside the byte
index. The whole-document UTF-16 table the exploration rejected was a
JavaScript-side adapter that exceeded the isolate heap cap; one integer per line
on the Rust side has no such cost. Keeping the index in `BufferModel` instead
would repeat the incremental splice on each of its three commit paths (view
edits, extension batches, and history replay).

`BufferModel` gains `line_count`, `line_range(line) -> Range<usize>` (excluding
the `\n`), `line_of_offset`, `read_range`, `is_char_boundary`, and
`utf16_offset` / `offset_from_utf16` conversions. Conversions find the line by
binary search, then scan within that line only.

### The view reads lines from the model

`EditorView` drops `lines`, `segs`, `Seg`, `flat_doc`, `default_projection`,
`rebuild_projection`, and `replace_text_with_grouping`'s post-splice rebuild.

- The caret and anchor stay `(line, byte_col)`. Conversions to and from flat
  bytes and UTF-16 go through the model's line index.
- Movement, hit testing, shaping, and painting read the needed line with
  `model.read_range(line_range)`. Painting reads only visible lines. gpui's
  frame-to-frame line layout cache already avoids reshaping unchanged lines, so
  the view keeps no shaping cache of its own.
- Grapheme deletion reads the current line. At a line boundary it deletes the
  previous or next line terminator, `\n` or `\r\n`.
- Word movement scans from the caret's line outward, line by line, until it
  finds a word boundary or reaches the document edge.
- Copy/Cut reads the selected range. `restore_view_state` snaps with
  `is_char_boundary`. Completion reads the current line's prefix.
- The model observer no longer compares texts. It clears preedit and preferred
  x when `model.revision()` differs from the revision the view last saw.
- Horizontal scroll clamps to the widest line width the view has shaped so far,
  instead of shaping every line on each wheel event. Painting and caret
  measurement update the maximum from lines they already shape, so it grows as
  long lines scroll into view and does not shrink when they leave. A text change
  resets it to the visible lines. This follows VS Code; Zed instead shapes the
  longest line found through its rope summary, which a flat piece table cannot
  provide cheaply.
- `runs_for` produces one default run per line. D002 introduces styled runs
  when it has a source for them.

### Contributions

The view keeps its rendered decoration, gutter, and action lists, rebuilt per
revision from `resolved_contributions()`. Each contribution is placed with
`line_of_offset` instead of a rebuilt `line_starts` table. That is
O(contributions × log lines), independent of document size.

`AnchoredRangeStore`'s interval index is removed. It is unused, and because
every edit marks it dirty, a visible-range query would re-sort every range on
each keystroke, which is no cheaper than resolving all of them. Contribution
counts are bounded by what extensions publish. Revisit with a real query
structure if a feature publishes enough ranges to matter.

### Undo and redo without whole-text diffs

`EditHistory::undo` / `redo` already apply known primitives. `replay_history`
derives its single `CommittedBufferChange` edit from the span those primitives
touched (the union of their post-edit ranges, read back from the buffer)
instead of diffing two whole-document copies.

### Piece-table quick wins

- **Cached length.** `TextBuffer` keeps `len` up to date in `insert` and
  `delete`.
- **Insert coalescing.** An insert that lands exactly at the end of an `Add`
  piece whose span ends at the end of the add buffer extends that piece instead
  of creating a new one. That is the common typing case. The extended piece
  keeps its id, so existing positions keep resolving to the same offsets, and
  sticky-left positions at its old end stay before the insert. The
  `BufferEdit::Insert` event records the inserted span as
  `(piece, offset_within_piece)`, which is `(new_piece, 0)` normally and
  `(left_piece, old_len)` when coalesced. Sticky-after anchors at the boundary
  relocate to the end of that span. The existing position, anchored-range, and
  transaction tests must pass unchanged, with new cases for coalesced boundaries.
- **Bounded edit log.** `edit_seq` becomes a monotonic counter independent of
  the stored log. `BufferModel` discards entries the anchored-range store has
  consumed after each stabilization, so the log holds at most one commit's
  primitives. `EditTransaction` checkpoints keep working because they compare
  `edit_seq`, not log length.

Pieces still accumulate for non-contiguous edits. That remains D013's trigger.

### Line endings

`BufferModel::line_ending()` reports `\r\n` when the first line terminator in
the buffer is `\r\n`, and `\n` otherwise, including buffers with no terminator.
It is computed on demand from the line index, so it needs no stored state and
follows the text after reload or external edits. The editor converts every
newline in typed, pasted, and IME-committed text to that ending. Extension edits
stay verbatim.

The first terminator rather than the dominant one is deliberate: files with
mixed endings are rare, and scanning for the dominant ending would reintroduce
a whole-document pass.

`\r` stays part of the stored line. `line_end` already keeps the caret before
it. Painting excludes it from the shaped text.

### Guarding the boundary

Under `cfg(test)`, `BufferModel` counts whole-document reads (`text()` and
full-range `read_range`). An editor test builds a 100,000-line buffer, then
types, deletes, moves by word and line, extends a selection, runs an IME
composition, scrolls, and paints, and asserts the count stayed at zero. Product
paths that legitimately need the whole text, such as Save and recovery
capture, are outside the view and unaffected.

## Steps

Each step is one commit, with `cargo test --all-targets` and clippy passing.

1. ✅ **Cached length and eager line index.** `TextBuffer` keeps `len`, builds
   the byte and UTF-16 line index at construction, and exposes `&self` line
   queries. Core tests cover UTF-16 starts across inserts, deletes, undo, and
   non-BMP text.
2. ✅ **Insert coalescing.** Extend the trailing `Add` piece on contiguous
   inserts, with the `BufferEdit::Insert` span change and anchored-range
   relocation. Add boundary tests for sticky-before and sticky-after anchors on
   coalesced inserts.
3. ✅ **Bounded edit log.** Monotonic `edit_seq`, and a drain after
   stabilization in `BufferModel`.
4. ✅ **Model line API and undo replay edit.** Committed with step 5, which
   is its first user. Add the `BufferModel` line,
   range, boundary, and UTF-16 queries, the test-only whole-read counter, and
   the replay edit derived from applied primitives.
5. ✅ **View reads from the model.** Remove `lines`, `segs`, `Seg`, and
   `flat_doc`. Route conversions, movement, deletion, IME, clipboard,
   completion, and painting through the model. Clamp horizontal scroll to
   the widest shaped width. Place contributions with `line_of_offset`. Add the
   no-whole-read test.
6. ⬜ **CRLF policy.** `line_ending()`, newline conversion of editor input, and
   tests for typing, pasting, IME, and deleting across `\r\n` in a CRLF buffer.
7. ⬜ **Remove unused core surface.** Delete the anchored-range interval index,
   make `core` private to the crate, and remove anything the compiler then
   reports as dead.
8. ⬜ **Documentation.** Update the editor-view section of
   [workbench and lifecycle](../architecture/workbench-and-lifecycle.md#editor-views)
   and [core and buffers](../architecture/core-and-buffers.md) (eager line
   index, UTF-16 starts, coalescing, bounded log, line endings). Record the
   UTF-16 index and first-terminator line-ending choices in
   [decisions](../decisions.md). Remove the resolved findings from the health
   review and roadmap, then this plan.
