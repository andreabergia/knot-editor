# Knot code-base health review

Reviewed at `9616773` (2026-10-24). This is an onboarding-style assessment: what
is left over from the prototype, whether the design holds up, and where the
technical debt is.

Status: active input. The [roadmap](../roadmap.md#selected-work)
schedules each open finding. The codebase health maintenance interlude
resolved the prototype inventory, the symlink Save bug, the Find stub, the
`ProductShell` split, and the documentation drift, and the editor view data
model slice resolved the view, piece-table, and CRLF findings. This document
now keeps only the open findings, all structural. Remove it once every
scheduled cleanup has landed or moved to the roadmap register; Git retains it.

## Summary

The repository is in better shape than most prototypes that become products.
The architecture docs match the code. Ownership boundaries (core / app / host)
are real and enforced by the type system. The test suite is large (≈730 Rust
tests) and fast (whole suite ≈4 s on an M-series Mac), and CI is strict
(clippy `-D warnings`, `--locked`, cargo-audit, pinned actions).

The remaining debt is structural: **some modules are too large and others too
scattered.** `engine.rs` is about 5,100 lines of production code plus 5,200 of
inline tests. Command routing is spread across six modules, and `src/app` is a
flat directory of about 45 files using name prefixes as namespaces. (P1/P2)

None of these calls the overall architecture into question. The
core/app/host split, captured command targets, the typed host boundary,
protected closure, and the recovery store are well reasoned and well tested.

| Area | Health | Notes |
| --- | --- | --- |
| Architecture & docs | 🟢 Good | Docs reflect code; decisions carry rationale and evidence. |
| Core text model | 🟢 Good | Cached length, eager byte/UTF-16 line index, coalesced typing, bounded edit log. |
| Editor view | 🟢 Good / 🟡 size | Reads lines from the model; `editor.rs` is still one large module. |
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

The editor view data model slice resolved **P0-1** (whole-document rebuild per
edit), **P1-1** (piece-table length, coalescing, and edit-log growth), and the
**CRLF** item of P2-7. Views now read lines through the core line index, the
unused interval index is gone, and `core` is private to the crate.

## 3. Open findings

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

- `CommandOrigin` exists only under `cfg(test)` in `app/mod.rs`. Move it next
  to its tests.

## 4. Design soundness: second opinion by subsystem

### 4.1 Core and buffers 🟢

The design is sound: a piece table with stable IDs, an edit log as the single
anchor-repair path, and undo through ordinary mutations. Separating content
identity from monotonic revisions (so undo can return to clean) is a nice
touch. A tree representation stays deferred under D013/D014.

### 4.2 Editor view 🟢

The grapheme- and shaped-x movement, IME handling, and viewport-culled
painting are good, and the view now reads lines from the model. `editor.rs`
(about 2,300 production lines) would benefit from splitting input/IME,
movement, painting, and completion; the roadmap schedules it with the
structural splits.

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

1. **Structural splits (P1-6, P2-1, P2-2, P2-3, P2-4).** These are pure moves,
   best done between slices, one subsystem per commit, so history stays
   readable and the plans don't collide.
