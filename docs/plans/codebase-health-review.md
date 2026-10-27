# Knot code-base health review

Reviewed at `9616773` (2026-10-24). This is an onboarding-style assessment: what
is left over from the prototype, whether the design holds up, and where the
technical debt is.

Status: active input. The [roadmap](../roadmap.md#selected-work)
schedules each open finding. The codebase health maintenance interlude
resolved the prototype inventory, the symlink Save bug, the Find stub, the
`ProductShell` split, and the documentation drift; this document now keeps only
the open findings. The additions to checkpoints 8 and 9 landed; the
post-initiative work remains. Remove this document once every scheduled cleanup has landed or
moved to the roadmap register; Git retains it.

## Summary

The repository is in better shape than most prototypes that become products.
The architecture docs match the code. Ownership boundaries (core / app / host)
are real and enforced by the type system. The test suite is large (≈730 Rust
tests) and fast (whole suite ≈4 s on an M-series Mac), and CI is strict
(clippy `-D warnings`, `--locked`, cargo-audit, pinned actions).

The remaining debt falls into three areas:

1. **The editor view is still a prototype renderer.** On every edit it
   materializes the whole document several times and rebuilds a
   `Vec<String>` line projection. The core-buffer benchmarks in
   `decisions.md` don't measure this path, and it is the product's main
   scalability ceiling. (P0)
2. **Some modules are too large and others too scattered.** `engine.rs` is
   about 5,100 lines of production code plus 5,200 of inline tests. Command
   routing is spread across six modules, and `src/app` is a flat directory of
   about 45 files using name prefixes as namespaces. (P1/P2)
3. **There are a few real correctness gaps.** The product has no way to stop a
   runaway extension turn. The core buffer has O(pieces) operations that grow
   with every keystroke. CRLF files get mixed line endings after edits. (P0/P1)

None of these calls the overall architecture into question. The
core/app/host split, captured command targets, the typed host boundary,
protected closure, and the recovery store are well reasoned and well tested.

| Area | Health | Notes |
| --- | --- | --- |
| Architecture & docs | 🟢 Good | Docs reflect code; decisions carry rationale and evidence. |
| Core text model | 🟡 Fair | Sound concept; linear-scan piece chain without coalescing. |
| Editor view | 🔴 Weak | Whole-document rebuild per edit; prototype `Seg`/projection model. |
| Workbench / product shell | 🟢 Good | Behavior well tested; `ProductShell` split by concern under `product/`. |
| Extension host (V8) | 🟢 Good / 🟡 size | Careful lifecycle design; one 10k-line file; two marshaling styles; no escape hatch. |
| Persistence & recovery | 🟢 Good | Thoughtful; Save follows symlinks and syncs the parent directory. |
| Commands & keymaps | 🟡 Fair | Correct model, fragmented across modules. |
| Tests & CI | 🟢 Good | Fast, broad, strict; tests inline in huge files hurt navigation. |
| Dead/prototype code | 🟢 Good | Test-only code is fenced; `cargo check --lib` covers `app` and `host`. |

## 1. Method

- I read the docs (architecture, decisions, roadmap, design, development,
  plans) and the module docs for every `src/app`, `src/core`, and `src/host`
  file.
- To measure dead code, I removed every `dead_code` allowance in a scratch
  worktree and ran `cargo check --lib`, once as-is and once with `lib.rs`
  exporting only `run()`.
- I checked hot paths (`EditorView` observe/rebuild, `TextBuffer`
  operations), I/O paths (local provider save), and extension-host
  lifecycle (termination, watchdogs).
- I ran `cargo test --all-targets`: everything passed.

## 2. Resolved by the maintenance interlude

These findings landed and are recorded here only for orientation. Git history
carries the details.

- **Prototype inventory (former §2.1/§2.2, P1-2).** The native search-results
  controller, generated document state, the compact completion surface, the
  `--terminal-fixture` mode, the drag-delivery example, the D028/D029 smoke
  scripts, and the unused one-off items are removed. Test-only code is behind
  `#[cfg(test)]`, and no module-level `dead_code` allowance remains. The library
  exports only `run()`, `core`, and the hidden declaration surface (the `host`
  half of the former §4.1 suggestion).
- **Prototype vocabulary in `core` (former §2.3).** Module docs no longer use
  exploration `D#` or step labels.
- **P0-2 symlink Save, P1-4 Find stub, P1-5 `ProductShell` god object, P2-5
  `pollster`, §4.8 `development.md` drift, and the `"generated"` document kind
  (former P2-7 item).**

The runaway-turn escape hatch (former P1-3) landed as the native
Restart Extension command, which unloads and reloads one package.
`ExtensionWatchdog` stays for per-turn timeouts under D019.

Still-retained prototype code, tracked below: the core line index and
anchored-range interval index (P0-1).

## 3. Open findings

### P0-1: The editor view rebuilds the whole document on every edit

`EditorView` (`src/app/editor.rs`) keeps a `lines: Vec<String>` and a parallel
`segs: Vec<Vec<Seg>>` projection. Its model observer (`editor.rs:290`) does
this on **every** model notification:

- `this.flat_doc() != model.text()`: `flat_doc()` is `lines.join("\n")`
  (a full copy), and `model.text()` is `read_range(0..len)` (another full
  copy).
- `rebuild_projection(&model.text(), …)`: a third full copy, then
  `text.split('\n').map(String::from)` allocates one `String` per line, plus a
  `Vec<Seg>` per line.
- `project_contributions` rebuilds `line_starts` from scratch.

`replace_text_with_grouping` runs the same rebuild again after the model
update. Coordinate conversions (`to_flat_byte`, `to_flat_utf16`,
`flat_byte_to_position`) are linear scans over `lines`. Every open view of the
buffer repeats all of this.

The result: every keystroke costs O(document size) in allocations, multiplied
by the number of views. The 29 MiB fixture in `decisions.md` validated the
*core* (p50 678 µs), but this layer makes typing in a large file unusable
long before the core becomes the bottleneck. `Seg` with `color/bold/italic`
is a leftover from the syntax-highlight prototype; nothing but the default is
ever produced.

**Recommendation** (one focused slice, before D002 syntax highlighting builds
on top of it):

- Give the view an incremental line model driven by `CommittedBufferChange`
  (the model already emits edit events), or query the core line index that
  already exists (`TextBuffer::line_start` / `line_of_offset`), which is
  currently unused.
- Keep only per-visible-line shaping caches in the view. Read line text from
  the model on demand.
- Replace `flat_doc()` comparisons with revision comparisons.
- Resolve contributions for the visible range only, using the anchored-range
  interval index that already exists and is also unused.
- Add a benchmark or test with a large buffer that asserts edits don't copy
  the whole text (for example, count `BufferModel::text()` calls in a test
  build).
- If the slice does not use the line index or the interval index, remove them.
  `core` is still `pub`, so the compiler does not report them as dead.

### P1-1: Core piece table: linear operations, no coalescing

`TextBuffer` (`src/core/buffer.rs`) stores pieces in a `Vec<Piece>`:

- `len()` sums every piece (`buffer.rs:325`), and is called from
  `assert!`s on every insert, delete, read, and `position_at`.
- `resolve`, `position_at`, and `split_at` are linear walks.
- Adjacent pieces are never coalesced, so each typed character appends a
  piece, and the chain grows without bound over a session (D013 defers
  compaction).
- The edit log grows without bound too (`take_edits` is unused).

This is fine for the prototype's benchmarks, but cost grows with *session
length*, not file size. A long editing session in one file degrades every
operation.

**Recommendation (cheap first steps):**

- Cache the total length.
- Coalesce an insert into the previous `Add` piece when the insert is
  contiguous both in the logical text and in the add buffer. This is the
  common typing case and needs no change to the stable-ID contract for
  existing positions: the left piece keeps its id, and only positions at
  its old end need the sticky-left rule. Verify against the existing
  position tests.
- Bound the edit log once all consumers track a sequence watermark.

Reconsider a tree (or rope) only after D013/D014 trigger.

### P1-6: `host/engine.rs` mixes four concerns in one 10k-line file

The production half covers:

- isolate lifecycle (`RuntimeCapsule`, about 1,500 lines in one `impl`)
- the pool and finalizer
- module-graph compilation and resolution
- about 1,000 lines of hand-written V8 ↔ protocol marshaling
  (`tree_item_from_v8`, `completion_response_from_v8`,
  `command_argument_from_v8`, `editor_contributions`, …)

The other half is tests.

There are also **two marshaling styles**. Host operations use the
macro-driven `DeclarationReader`, while providers, tree items, commands, and
events still use bespoke per-type functions. The decision record presents
declarations as the way to avoid parallel transport tables. The bespoke half
is exactly such a table.

**Recommendation:**

- Split the file into `engine/{capsule,pool,modules,marshal}.rs` and move
  the tests out.
- Then decide whether provider and tree responses should go through the
  declaration machinery too. If they intentionally stay hand-written,
  record why in `decisions.md`.

### P2-1: Command machinery is spread across six modules

A newcomer has to read all of these to follow one key press to a command
outcome:

- `model.rs` (`CommandCatalog`, `CommandDefinition`, `CommandTarget`)
- `command_declarations.rs` (macro registry and `CommandContext`)
- `product_commands.rs` (gpui `Action`, `ProductCommandTarget`, dispatch)
- `product/commands.rs` (native command bodies)
- `editor/native_commands.rs`
- `extension_commands.rs` (invocation trees)
- `keymaps.rs`

`model.rs` itself is a grab-bag: `BufferModel`, `BufferRegistry`,
`CommandCatalog`, and `BufferSubscriptionRegistry`.

**Recommendation:** create `app/commands/` with `catalog.rs` (out of
`model.rs`), `declarations.rs`, `dispatch.rs` (from `product_commands.rs`),
`extension.rs`, and `keymaps.rs`, and leave command *bodies* next to their
owners. `model.rs` then becomes just the buffer model.

### P2-2: `src/app` is flat, with prefix namespacing

About 45 files use `terminal_*` (7), `extension_*` (8), `product*`, and
`personal_config*` as namespaces. Move them into `app/terminal/`,
`app/extensions/`, and `app/commands/` (P2-1). Do this as a pure move with
no behavior change. It also makes the routing table in `AGENTS.md` shorter.

### P2-3: Huge inline test modules

`engine.rs` (5,250 test lines), `editor.rs`, `model.rs`, `buffer.rs`,
`scheduler.rs`, and `recovery.rs` carry tests inline after `mod tests`.
`terminal_session_tests.rs` shows the sibling-file alternative. This improves
navigation and incremental-compile ergonomics. ✅ The product shell tests are
already split by theme under `product/tests/`; `product/tests.rs` keeps only
shared fixtures.

### P2-4: Three executor models

The app uses:

- the gpui background executor (SQLite stores, recovery)
- a dedicated multi-thread **tokio runtime** built only for `tokio::fs` in
  the local provider (`product/launch.rs`, `filesystem/local.rs`)
- the V8 worker pool's own threads

Tokio is otherwise used only for `sync` channels. `tokio::fs` is just
`spawn_blocking` underneath, so the local provider could run `std::fs` on the
gpui background executor like the SQLite stores do. That removes a runtime,
its threads, and the `rt-multi-thread` feature. It's not urgent, but it is
an easy simplification consistent with "keep the design simple."

### P2-5: Upstream dependency warnings

- CI prints a future-incompatibility warning for `block v0.1.6` and
  `proc-macro-error2` (both via gpui). Track them, but there is nothing to
  do locally.
- gpui `0.2` is a pre-1.0 framework pulled from crates.io. Expect API churn.
  Keeping usage behind Knot-owned views, as you do now, is the right hedge.

### P2-7: Small items

- The editor normalizes CRLF on input (`editor.rs:1254`) and trims a
  trailing `\r` for line ends, but stores files verbatim. A CRLF file will
  therefore get mixed line endings after edits. Pick a policy: detect the
  dominant ending on load and use it for inserted newlines.
- `CommandOrigin` exists only under `cfg(test)` in `app/mod.rs`. Move it next
  to its tests.
- `examples/extensions/@example/*`: consider dropping `application-state` and
  `storage-demo` once `@knot/projects` covers the same APIs as a real reference.

## 4. Design soundness: second opinion by subsystem

### 4.1 Core and buffers 🟡

The design is sound: a piece table with stable IDs, an edit log as the single
anchor-repair path, and undo through ordinary mutations. Separating content
identity from monotonic revisions (so undo can return to clean) is a nice
touch. The implementation is still prototype-shaped (P1-1). `core` is still
`pub` from the library crate, so its API surface isn't checked for dead code;
make it private once P0-1 settles which of its unused indexes survive.

### 4.2 Editor view 🔴

See P0-1. The grapheme- and shaped-x movement, IME handling, and
viewport-culled painting are good. The data model underneath them is not.
`editor.rs` (2,400 production lines) would also benefit from splitting
input/IME, movement, painting, and completion once P0-1 changes the data
model.

### 4.3 Documents, persistence, recovery 🟢

URI-normalized identity, conditional replace with version checks, no-clobber
create, application-wide URI ownership, and a FULL-sync recovery store that
fails closed (falls back to prompts) are all well-judged. Hot exit is
correctly gated on durable capture. My only reservation: `ResourceVersion` is
`(len, mtime, dev, ino)`. Some editors and sync tools preserve mtime and
length, so a same-size external edit within mtime granularity goes undetected.
That is acceptable for a text editor, but a content hash of the loaded bytes
would close the gap cheaply if it ever shows up. Replacement still drops hard
links, ownership, and extended attributes, as rename-based saves do.

### 4.4 Workbench and lifecycle 🟢

The model is strong: a split tree with stable IDs, protected closure that
defers mutation until all decisions succeed, and exact presentation captures
for relocation. The tests are extensive (cross-window drag, tear-out,
closure). `ProductShell` is now split by concern under `src/app/product/`.
`workbench/tools.rs` and `tool_layout.rs` are cohesive.

### 4.5 Extension host 🟢

This is the most carefully engineered part of the codebase:

- persistent isolates on a bounded pool
- lifecycle-scoped finalization with `catch_unwind`
- explicit microtask policy
- dynamic import rejected
- private bootstrap bindings
- exact-integer checks at the boundary
- immutable module graphs
- rollback on failed entry

Concerns are size and organization (P1-6) and the dual marshaling style. The 32 MiB default heap per
isolate is reasonable. Reconsider it when bundled workflows (search,
explorer) move to JS.

### 4.6 Commands and keymaps 🟢 / 🟡

Captured targets with revalidation, explicit participant precedence, and
Knot-side keymap precedence are all the right calls for an Emacs-like
editor. Linking descriptors with `inventory` is a reasonable trade-off, and
it is guarded by `tests/handler_linking.rs` on all three platforms in CI.
The debt is fragmentation (P2-1).

### 4.7 Semantic UI 🟢

Trees, completion aggregation, transient interactions, and text frames sit on
one captured-window route, and the decision log is honest about provisional
parts (D054).

## 5. Suggested sequencing

1. **Editor view data model (P0-1)** together with **piece-table quick wins
   (P1-1)** and the CRLF policy (P2-7). This is one focused
   slice and the prerequisite for D002 syntax highlighting and D031 Find. Plan
   it in `docs/plans/`.
2. **Structural splits (P1-6, P2-1, P2-2, P2-3, P2-4).** These are pure moves,
   best done between slices, one subsystem per commit, so history stays
   readable and the plans don't collide.
