# Knot Code Architecture

This document maps the code that exists today. It complements `design.md`,
which describes the intended product, and `roadmap.md`, which records the
experiments used to choose the architecture. Knot is still a prototype, so
some directories are completed experiments rather than parts of the editor's
current runtime path.

## At a glance

Knot is one Rust package with a library and several binaries. The intended
dependency direction is:

```text
platform / gpui shell and views       future scripting runtime
               |                              |
               +------------+-----------------+
                            |
                  editor model and host API
                            |
             TextBuffer + AnnotationStore
```

The bottom layer is the most developed product code. The shell and host API
are currently placeholders or spikes: the default `knot` binary only opens an
empty winit window, while the feature-complete editor widget lives in the
standalone gpui framework experiment.

The crate modules exported by `src/lib.rs` are:

- `core`: editor-owned data models. This is independent of windowing and
  rendering.
- `app`: the current minimal winit application entry point.
- `host`: placeholder for the future scripting runtime and extension-facing
  host API.
- `view`: the step-2 renderer benchmark abstraction, not the future editor
  `View` API.

`core` itself contains `buffer` (the piece table and edit log), `annotation`
(token-anchored ranges over the buffer), and `transaction` (one reversible
primitive-edit transaction that routes through `buffer`).

## Build targets

`Cargo.toml` defines one library and five binaries:

| Target | Entry point | Role |
| --- | --- | --- |
| `knot` | `src/main.rs` | Default prototype. Calls `app::run` and opens a blank winit window. |
| `step3-gpui` | `src/step3/gpui/main.rs` | Chosen-framework spike: an interactive gpui editor and three-pane shell. |
| `bench` | `src/bin/bench.rs` | Step-2 rendering benchmark driver. |
| `core-bench` | `src/bin/core_bench.rs` | Piece-table and line-index benchmark. |
| `anno-bench` | `src/bin/anno_bench.rs` | Annotation stabilization, resolution, and query benchmark. |

There is deliberately no workspace or crate split yet. Module boundaries are
being validated before they are enforced as separate crates.

## Core editor model

`src/core` contains the architecture selected by roadmap steps 4–6. It has no
dependency on gpui, winit, or a renderer.

### Text buffer

`core::buffer::TextBuffer` is a hand-written, stable-ID piece table. Logical
text is a chain of `Piece` records that refer to one of two stores:

- `Original`: immutable text supplied when the buffer is created.
- `Add`: an append-only string containing all inserted text.

An edit splices the piece chain. Interior edits split a piece: the left half
keeps its `PieceId`, while the right half receives a new, monotonically
increasing ID. Pieces are not coalesced and the add store is not compacted, so
memory and piece count currently grow with edit history.

All public offsets and ranges are UTF-8 byte offsets. Character-boundary
requirements are currently debug assertions; grapheme-level editing is not a
buffer responsibility yet.

The main surfaces are:

- construction and reads: `new`, `from_text`, `len`, `read_range`;
- mutation: `insert`, `delete`, `replace`;
- line lookup: `line_count`, `line_start`, `line_of_offset`;
- stable locations: `position_at` and `resolve`;
- observation: `edit_seq`, `edits_since`, and `take_edits`.

The line index is a lazily-created `Vec<usize>` of line starts. Once created,
edits maintain it incrementally. Its suffix shifts are the known large-buffer
bottleneck, but the public lookup API allows the representation to be replaced
later.

### Stable positions and the edit log

`Position` is a `(PieceId, offset-within-piece)` token. It stays valid across
edits to unrelated pieces. A token becomes stale when its piece is deleted or
when its content moves into the new right half of a split.

Every primitive mutation appends a `BufferEdit`; `replace` appends a delete
and then an insert. Events use pre-edit logical offsets and include split and
affected-piece metadata. Consumers replay the log to repair only tokens on
touched pieces. The log is currently unbounded, and draining it is safe only
when no consumer still has a cursor into it.

### Annotations

`core::annotation::AnnotationStore` owns ranges that survive buffer edits. An
`Annotation` contains two `Anchor`s, and each anchor combines a stable
`Position` with `Stickiness::{Before, After}`. The default start is `Before`
and the default end is `After`.

The store maintains three kinds of derived state:

- an endpoint index keyed by piece ID, used to find anchors affected by an
  edit without scanning all annotations;
- cached logical offsets, used while an anchor is temporarily unresolvable;
- a lazily rebuilt interval index for overlapping-range queries.

The required lifecycle is:

```text
TextBuffer mutation
        |
        v
BufferEdit appended
        |
        v
AnnotationStore::stabilize(&buffer)
        |
        +-- repair endpoints on touched pieces
        +-- remove annotations fully consumed by a delete
        +-- mark the interval index dirty
        |
        v
resolve / query_range / query_range_for_kinds
```

Callers must stabilize a store before expecting it to reflect new edits.
Annotations from diagnostics, search, git, breakpoints, folding, and other
providers share one store. `AnnotationKind` is a category, not provider
identity; payload data remains an opaque string. Consumers compose sources by
querying all kinds or a selected set. A fully consumed annotation (both
endpoints land strictly inside a deleted span and collapse onto the same
surviving edge) is removed entirely — its id, endpoint-index entries, and
cached offsets are dropped. `resolve` returns `None` for that id; queries and
`iter_live` cannot return it. Undoing the consuming text edit does not revive
the annotation; a provider re-publishes under a fresh id later. `OffsetStore`
is the intentionally naive O(all annotations) comparison implementation used
by benchmarks, not the selected product model.

### Edit transaction

`core::transaction::EditTransaction` is a single-use, stateful recorder over a
`TextBuffer` (roadmap step 6b). Its `insert` / `delete` / `replace` apply
immediately and record the bytes needed to invert each primitive (captured via
`read_range` before mutating). `undo` applies the recorded inverses in reverse
primitive order; `redo` reapplies the originals in forward order. Both route
exclusively through `TextBuffer::{insert, delete, replace}`, so each direction
emits the normal `BufferEdit` log entries and preserves line-index
maintenance. No piece IDs are restored, the append-only `Add` store is never
truncated, and annotations are not mutated by the transaction — the
`AnnotationStore` owner calls `stabilize(&buffer)` after forward, undo, and
redo just as it does after any other buffer edit. The transaction checkpoints
both its originating buffer's stable instance identity and `edit_seq` before
every recorded primitive and before each `undo` / `redo` pass, and panics on
an out-of-band edit or buffer swap rather than applying stale raw byte
offsets. Invalid orderings (double `undo`, `redo`
without `undo`, mutate-after-`undo`) panic, matching `TextBuffer`'s
precondition style. It is not a history manager: branching history after
`undo` is out of scope, callers create a new transaction.

## Application, UI, and rendering

There are currently three distinct UI paths. They should not be merged
conceptually:

### Default application skeleton

`src/main.rs` calls `app::run`. `src/app.rs` owns a winit `EventLoop` and one
optional `Window`; it creates the window on resume and exits on close. It is
the roadmap step-1 skeleton and does not yet connect to `TextBuffer`, gpui, or
rendering.

### Selected gpui framework spike

`src/step3/gpui` is the evidence behind the decision to use gpui, but it is
still a standalone binary rather than the default app.

- `main.rs` builds a resizable three-pane `Shell`, loads a renderer fixture,
  and hosts an `EditorView`.
- `editor.rs` owns spike-local lines, styled segments, annotations, cursor,
  selection, scroll, focus, and IME state. It implements gpui `Render`,
  `Focusable`, and `EntityInputHandler`, plus a custom `Element` for shaping,
  hit testing, clipping, and painting.
- Unicode grapheme movement is handled with `unicode-segmentation`. RTL
  alignment and hit testing use a public-gpui workaround documented in the
  source and `docs/step3-framework-comparison.md`.

The spike does **not** use `core::TextBuffer` or `AnnotationStore`; its local
text and annotation structures are disposable experiment code. A future view
integration should connect gpui presentation state to the core models rather
than promote those local structures.

### Renderer benchmark harness

`src/view` compares rendering primitives from roadmap step 2. Its `Renderer`
trait accepts visible styled lines and has implementations for:

- `stub`, which measures harness overhead;
- `wgpu_cosmic`, a low-level wgpu + cosmic-text renderer;
- `skia`, a macOS-only Metal/Skia renderer.

`view::fixture` parses `.kfx` styled-text fixtures and can tile them into
large workloads. `view::bench` owns the winit lifecycle, scrolls a viewport,
times frames, and prints summary metrics. This trait predates the gpui choice
and is explicitly not the future native editor `View` abstraction.

## Scripting and extension boundary

`src/host/mod.rs` is empty except for documentation. No scripting engine,
command registry, extension lifecycle, capability provider, or async host
bridge exists yet. JavaScript on V8 is selected, with one isolate per extension
scheduled over a bounded worker pool and Tokio handling asynchronous host work.
That target topology is recorded but not implemented; roadmap step 7 will test
it before the host module takes shape.

The intended boundary from `design.md` is that performance-sensitive buffer,
view, scheduling, and host integration remain native, while built-ins and
extensions use the same public editor APIs wherever practical. That is a
design constraint, not yet an implemented module graph.

## Where to make changes

| Change | Primary location | Notes |
| --- | --- | --- |
| Text storage, edits, positions, or line lookup | `src/core/buffer.rs` | Preserve byte-offset and edit-log invariants. |
| Annotation tracking or range composition | `src/core/annotation.rs` | Stabilize after edits; do not build on `OffsetStore`. Fully consumed annotations are removed, not tombstoned. |
| Reversible primitive-edit transaction | `src/core/transaction.rs` | Routes through `TextBuffer::{insert, delete, replace}` only; no history tree or annotation API. |
| Current executable lifecycle | `src/app.rs` | Still the blank winit skeleton. |
| gpui behavior or framework investigation | `src/step3/gpui` | Spike-local state is not the core model. |
| Renderer benchmark or fixtures | `src/view`, `src/bin/bench.rs` | Comparative experiment only. |
| Buffer/annotation performance probes | `src/bin/core_bench.rs`, `src/bin/anno_bench.rs` | Findings live in the matching docs. |
| Future scripting host | `src/host` | Follow roadmap step 7's recorded decision. |

## Architectural invariants and known gaps

- Dependencies should point from UI/host layers toward `core`; `core` must not
  depend on platform, renderer, or scripting details.
- A buffer owns text and edit history. Cursor, selection, folding, scroll, and
  zoom are view state and must not be added to `TextBuffer`.
- Stable positions and `BufferEdit` metadata are a coupled contract. Changes
  to piece splitting or deletion must keep annotation repair possible.
- `AnnotationStore` is the selected composition mechanism. Visual precedence,
  hit testing, and rich presentation belong to the future view layer.
- gpui is the selected UI framework, but the default app has not migrated to
  it and the real core-backed `View` abstraction does not exist yet.
- Undo/redo transactions (one-shot, no history tree) live in
  `core::transaction`; history trees, edit grouping, persistence, and
  view-state restoration remain roadmap work. Scripting, terminal state,
  filesystem providers, commands, and capability aggregation remain roadmap
  work, not hidden subsystems in the current code.

When one of these facts changes, update this document in the same change as
the code. Use `design.md` for the target product philosophy and `roadmap.md`
plus step-specific documents for decisions, evidence, and experiment history;
keep this file focused on the current implementation.
