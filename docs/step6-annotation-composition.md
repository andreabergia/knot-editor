# Step 6 — Annotation Composition

> **Question:** do independent annotation sources compose at the data/query
> level, without forcing special coordination into the core?

## Answer

**Yes.** Five independent sources coexist in a single `AnnotationStore` with
no coordination between them. Consumers query by kind; sources never need to
know about each other. Adding a sixth source requires zero changes to the
store, buffer, or capability model — just `store.add(&buf, start, end, kind, data)`.

## Design

### Source identity: `AnnotationKind`

Each source is identified by its `AnnotationKind`. The store is source-agnostic:
`add()` accepts any `kind` and stores it as metadata on the annotation. Sources
are just callers of `add()` with different kinds and data payloads.

The `AnnotationKind` enum now has six variants:

```rust
pub enum AnnotationKind {
    Diagnostic,    // compiler/linter diagnostics
    Search,        // text search matches
    Git,           // version-control diff hunks
    Breakpoint,    // debugger breakpoints
    Folding,       // code-folding regions
    Other(u8),     // extensible: add a 6th (7th, …) source without changing the enum
}
```

The `Other(u8)` variant is the escape hatch: a 6th source (e.g. a linter
distinct from the compiler, a bookmarks provider) uses `Other(n)` and requires
zero enum changes.

### Per-kind query surface

Two new methods on `AnnotationStore` enable consumers to pick which source
types they care about:

| Method | Signature | Purpose |
|--------|-----------|---------|
| `query_range_for_kinds` | `(&mut self, &TextBuffer, a: usize, b: usize, kinds: &[AnnotationKind]) -> Vec<AnnotationId>` | Like `query_range` but filtered to the given kind set. |
| `iter_live` | `(&self) -> impl Iterator<Item = (&AnnotationId, &Annotation)>` | Iterate all live (non-collapsed) annotations — full-scan consumer (e.g. minimap heatmap). |

`query_range_for_kinds` extends the existing interval-index fast path: it
builds a `HashSet<AnnotationKind>` from the caller's slice and filters the
index results in one pass. No per-kind pre-index needed for prototype-scale
annotation counts.

### AnnotationData

`AnnotationData(pub String)` remains an opaque owned payload. Different sources
stuff different things into it (diagnostic message, search match text, git hunk
label, fold region name) — the store never inspects it. Rich per-kind payload
types (e.g. diagnostic severity, git line origin) are deferred until the view
layer needs them.

## Composition stress test

The test in `src/core/annotation.rs` exercises the full composition surface:

1. **5 sources** (diagnostics, search, git, breakpoints, folding) populate
   overlapping byte ranges on an 80-line buffer.

2. **3 consumer perspectives** query per-kind:
   - **Editor view** — `query_range` (all kinds): 13 annotations
   - **Gutter** — `query_range_for_kinds(_, [Diagnostic, Breakpoint])`: 5
   - **Minimap** — `query_range_for_kinds(_, [Diagnostic, Search, Git])`: 10

3. **Overlapping annotations from different sources** coexist correctly:
   - Line 5: a diagnostic and a search match overlap; per-kind filter
     returns only the requested source.
   - Line 10: a git hunk and a breakpoint overlap; full query returns
     both.

4. **6th-source proof**: a lint annotation (`AnnotationKind::Other(0)`) is
   added after the initial 5 sources with zero store changes. It appears in
   all-kinds queries and is excluded from gutter queries (which only ask for
   `[Diagnostic, Breakpoint]`).

5. **Post-edit survival**: a second test inserts and deletes text; all three
   source types remain independently queryable after edits.

## No privileged coordination found

The step's question was: "document any case where data-level composition
requires privileged coordination." None was found.

- Sources never intersect: each source calls `store.add()` independently.
  No source needs access to another source's annotations, the edit log, or
  internal state.
- Consumers compose results by querying with a kind filter — the store
  does the filtering, but it doesn't coordinate sources.
- The interval index is source-agnostic. It indexes all annotations by byte
  range; the kind filter is applied at query time. This keeps the index
  cheap to build (one pass) and adds negligible cost to filtered queries
  (a `HashSet` lookup per candidate).

**Potential future friction** (not addressed in this step; deferred to step
10's provider/surface split):

- **Dedup / priority**: if two diagnostics sources flag the same line, a
  consumer may need to pick one. That's a view-layer decision, not a
  data-layer coordination problem.
- **Annotation removal by source**: when a source is torn down (e.g. LSP
  disconnects), its annotations need to be removed. The current store has
  `remove(id)` but no `remove_by_kind(kind)`. Adding it is a trivial
  `retain` pass — deferred until a real source lifecycle exists.
- **Source-aware stabilization**: the store's `stabilize` repairs all
  endpoints regardless of source. Untouched annotations are left alone
  through token stability — this works for any number of sources without
  per-source work.

## Deferred

- Visual precedence, layering, hit-testing, and annotation conflict policy
  (turfed to the real editor view).
- Per-kind rich data payloads (enum variants with severity, line origin, etc.).
- `remove_by_kind` / source lifecycle management.
- Concurrent source registration/deregistration at runtime (step 10).

## Findings summary

| Claim | Result |
|-------|--------|
| Multiple sources coexist in one store | ✅ Confirmed — 5 sources, 13 annotations, no conflicts |
| Adding a 6th source requires no store changes | ✅ Confirmed — `Other(0)` added post-hoc |
| Per-kind query composition | ✅ Confirmed — editor/gutter/minimap each query different kind subsets |
| Overlapping annotations from different sources | ✅ Confirmed — line 5 (diag+search), line 10 (git+bp) compose correctly |
| Sources don't need privileged coordination | ✅ Confirmed — sources are independent callers of `add()` |
| Composition survives edits | ✅ Confirmed — insert/delete shifts don't break per-kind queries |
