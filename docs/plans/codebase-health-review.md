# Knot code-base health review

Reviewed at `9616773` (2026-10-24). This is an onboarding-style assessment: what
is left over from the prototype, whether the design holds up, and where the
technical debt is.

Status: active input. The [extension platform
plan](extension-platform-and-projects-plan.md#follow-up-from-the-codebase-health-review)
schedules each finding: the maintenance interlude before checkpoint 7c,
additions to checkpoints 8 and 9, and post-initiative work. Remove this document
once every scheduled cleanup has landed or moved to the roadmap register;
Git retains it.

## Summary

The repository is in better shape than most prototypes that become products.
The architecture docs match the code. Ownership boundaries (core / app / host)
are real and enforced by the type system. The test suite is large (≈740 Rust
tests) and fast (whole suite ≈4 s on an M-series Mac), and CI is strict
(clippy `-D warnings`, `--locked`, cargo-audit, pinned actions).

The debt falls into four areas:

1. **The editor view is still a prototype renderer.** On every edit it
   materializes the whole document several times and rebuilds a
   `Vec<String>` line projection. The core-buffer benchmarks in
   `decisions.md` don't measure this path, and it is the product's main
   scalability ceiling. (P0)
2. **Prototype surfaces survive behind `#[allow(dead_code)]`.** There are 29
   allowances, including whole modules. Compiling without them gives about
   80 dead items. Most of them are probes, fixtures, or "validated the
   boundary" scaffolding that the product never reaches. (P1, cheap)
3. **Some modules are too large and others too scattered.** `ProductShell`
   has a 3,500-line `impl`. `engine.rs` is 5,100 lines of production code
   plus 5,200 of inline tests. Command routing is spread across six
   modules, and `src/app` is a flat directory of 46 files using name
   prefixes as namespaces. (P1/P2)
4. **There are a few real correctness gaps.** Saving through a symlink
   replaces the symlink. The product has no way to stop a runaway extension
   turn. Find menu items always fail. The core buffer has O(pieces)
   operations that grow with every keystroke. (P0/P1)

None of these calls the overall architecture into question. The
core/app/host split, captured command targets, the typed host boundary,
protected closure, and the recovery store are well reasoned and well tested.

| Area | Health | Notes |
| --- | --- | --- |
| Architecture & docs | 🟢 Good | Docs reflect code; decisions carry rationale and evidence. Minor drift (see §4.8). |
| Core text model | 🟡 Fair | Sound concept; linear-scan piece chain without coalescing; prototype comments. |
| Editor view | 🔴 Weak | Whole-document rebuild per edit; prototype `Seg`/projection model. |
| Workbench / product shell | 🟡 Fair | Behavior well tested; `ProductShell` is a god object. |
| Extension host (V8) | 🟢 Good / 🟡 size | Careful lifecycle design; one 10k-line file; two marshaling styles. |
| Persistence & recovery | 🟢 Good | Thoughtful; one symlink bug in Save. |
| Commands & keymaps | 🟡 Fair | Correct model, fragmented across modules; placeholder Find. |
| Tests & CI | 🟢 Good | Fast, broad, strict; tests inline in huge files hurt navigation. |
| Dead/prototype code | 🟡 Fair | Clearly fenced, but large and silently allowed. |

## 1. Method

- I read the docs (architecture, decisions, roadmap, design, development,
  plans) and the module docs for every `src/app`, `src/core`, and `src/host`
  file.
- To measure dead code, I created a scratch worktree, removed every
  `allow(dead_code)` / `cfg_attr(not(test), allow(dead_code))`, and ran
  `cargo check --lib`. I ran it a second time with `lib.rs` exporting only
  `run()`, so items in `pub` modules (`core`, `host`, `app::model`) are
  checked too. Appendix A has the raw output. Anything listed there is
  reachable **only from tests**.
- I checked hot paths (`EditorView` observe/rebuild, `TextBuffer`
  operations), I/O paths (local provider save), and extension-host
  lifecycle (termination, watchdogs).
- I ran `cargo test --all-targets`: everything passes, 734 lib tests in 3.7 s.

## 2. Prototype and experiment inventory: classification

Legend:
- **Remove**: delete it; Git keeps the history.
- **Test-only**: move behind `#[cfg(test)]` or into a test-support module, so
  production builds and `dead_code` warnings stop carrying it.
- **Wire or remove**: decide now whether it becomes a product feature in the
  current initiative; if not, remove it.
- **Keep**: it is legitimate product or tooling.

### 2.1 Rust sources

| Item | Where | What it is | Verdict |
| --- | --- | --- | --- |
| Generated search results | `src/app/search_results.rs` (whole module), `DocumentCollection::create_search_results`, `Document::search_results`, `ACTIVATE_SEARCH_RESULT_COMMAND` | Native "search results as a generated buffer" probe that validated D027/D007. Never constructed by the product. The command name is never registered. | **Remove.** The design says cross-file search is a bundled extension, so this native controller will not be the shipping path. |
| `DocumentState::Generated` + `create_generated` | `documents.rs`, `recovery.rs`, `product.rs`, `extension_host.rs`, `knot.d.ts` (`kind: "generated"`) | Generated-document state that only tests construct. Contributed text frames (`text_view.rs`) use a private buffer instead. | **Wire or remove.** Removing it also simplifies recovery and the public `DocumentInfo.kind`. Keep it only if bundled search (D007) will open results as documents in this initiative. |
| `CompactCompletionSurface`, `CompletionSurfaceKind`, `EditorView::swap_completion_surface` | `completion.rs`, `editor.rs:1796` | Second completion presentation that proved surfaces are replaceable. | **Remove** (keep the `CompletionSurface` trait if the list surface still uses it; otherwise inline it). |
| `--terminal-fixture` launch mode | `entry.rs`, `terminal_fixture.rs`, `allow(dead_code)` on `terminal_session`/`terminal_view` | Diagnostic window for terminal lifecycle. Ships in the product binary. | **Remove**, or move to `examples/` if it is still used for manual terminal QA. Terminals in the product already cover the same lifecycle, and `terminal_session_tests.rs` covers it automatically. |
| `examples/tab_drag_delivery.rs` | 548 lines | Two-window gpui drag-delivery probe (decision: "macOS delivers pressed drags to the originating window"). | **Remove.** The finding is recorded in `decisions.md`, and `product/tab_drag/cross_window_tests.rs` covers the behavior. |
| `MemoryFileSystemProvider` | `filesystem/memory.rs` (414 lines) | In-memory provider used only by tests. | **Test-only.** It is valuable test support. Gate the module with `#[cfg(test)]`. |
| V8 probe APIs: `RuntimeCapsule::{new, execute, execute_fixture_module, hold_external_utf16}`, `EngineWork::{Script, FixtureModule}`, `evaluate_fixture_module`, `RuntimePool::{load, execute_fixture_module, buffer_change_queue_metrics}`, `*Execution::wait` | `host/engine.rs`, `host/pool.rs` | Low-level engine probes from the V8 exploration (marshaling benchmarks, external UTF-16, fixture modules). | **Test-only.** Many engine tests use them, so don't delete them. Put the `Script`/`FixtureModule` work variants and their methods under `#[cfg(test)]`, as `EngineWork::Panic` already is, and drop the module-level `allow(dead_code)` on `host::engine`. |
| `ExtensionWatchdog` / `ExtensionPool::watchdog` | `host/pool.rs` | Forced termination handle. Never issued in the product. | **Wire it** (see P1-3). |
| Scheduler diagnostics: `ExtensionDiagnostics`, `SchedulerDiagnostics`, `ReadyForContinuation`, `single_worker`, `next_turn`, `state`, `recv`, `worker_count` | `host/scheduler.rs` | Introspection built for the scheduler exploration. | **Test-only** where tests use it, **remove** the rest. Reconsider together with D019. |
| `TextBuffer::{line_count, line_start, line_of_offset, ensure_line_starts, take_edits}` and the lazy line index | `core/buffer.rs` | Line index benchmarked in the exploration. The product never queries it; the editor re-splits the full text instead (P0-1). | **Keep and wire it** as part of P0-1. If P0-1 goes another way, remove it. |
| `AnchoredRangeStore` interval index (`rebuild_index`, `query_range`, `iter_live`, `len`, `sticky`, `Anchor::pos`) | `core/anchored_range.rs` | Query index from the annotation benchmark. Unused by the product. | **Keep for P0-1** (visible-range contribution queries) or remove it. Don't leave it in limbo. |
| `HostDeclaration` metadata (`error_codes`, `handler_type`, `names`, `arguments`, `new`) | `host/declarations.rs` | Introspection for the deferred public command schemas (D020). | **Test-only** (it validates the TS error union in tests). |
| `TransientKind` | `host/protocol.rs:675` | Unused enum. | **Remove.** |
| `ContributionSource::BuiltIn`, `BufferModel::resolve_extension` | `model.rs` | Native contributions that nothing produces. | **Remove.** |
| `RecoveryStore::{retain, release, last_error}`, `write_retained` | `recovery.rs` | Retained checkpoints for checkpoint 7c / D051. | **Keep**: this is scheduled work. Replace the "checkpoint 7c" wording in the allowance reasons (they record roadmap history). |
| Workbench convenience APIs (`Workbench::new`, `open_tab`, `split_focused`, `request_close_tab`, `document_id`, `place …`) | `workbench.rs`, `workbench/close.rs` | Already `cfg_attr(not(test), allow(dead_code))`. | **Test-only**: make them `#[cfg(test)]` outright. |
| Misc. one-offs: `ResourceUri::is_descendant_of`, `UriPathSegments::len`, `WorkspaceState::replace_root`, `filesystem::child_uri`, `LocalFileSystemProvider::{new, root}`, `DocumentId::value`, `persisted_version`, `EditorView::{select_reveal_and_focus, new_with_options}`, `DependencyPlan::execute`, `PersonalConfig.directory`, `CommandExecution` | various | Leftovers. | **Remove** unless D052 (multi-root) needs `is_descendant_of`/`replace_root`; those two are good candidates to keep. |

Rule of thumb for the cleanup: after it, **no module-level
`#[allow(dead_code)]` should remain** in `src/app/mod.rs` or
`src/host/mod.rs`. Item-level allowances should be rare and should name a
scheduled work item. Today the 15 module-level allowances hide about 80
unused items, and they will keep hiding new ones.

### 2.2 Non-Rust assets

| Item | Verdict |
| --- | --- |
| `scripts/linux-framework-smoke.sh` (hard-coded `/tmp/knot-d028-smoke-…`), `scripts/windows-product-smoke.ps1` (`knot-d029-…`) | One-off platform validation for D028/D029. **Remove**, or rename and generalize them if you will repeat platform smokes before D060. |
| `scripts/terminal-links-fixture.py`, `scripts/terminal-mouse-fixture.py` | Manual terminal acceptance fixtures. **Keep**: they are cheap and useful for terminal regressions. Document them in `development.md`. |
| `examples/extensions/@example/*` (5 packages) | Author-facing examples, type-checked in CI. **Keep.** Consider dropping `application-state`/`storage-demo` once `@knot/projects` covers the same APIs as a real reference. |
| `.agents/skills/*/agents/openai.yaml` | Agent tooling, unrelated to the product. Keep it if you use it. |

### 2.3 Prototype vocabulary still in code comments

`AGENTS.md` says comments must describe the current design, not its history.
These modules still read like exploration notes:

- `src/core/buffer.rs` (37 hits), `src/core/transaction.rs`, and
  `src/core/anchored_range.rs` reference `D1/D2/D4/D5/D7`, "step 5's job",
  and "step 6b". **These `D#` labels collide with the roadmap's `D###` IDs**
  (for example, buffer "D7" is the edit log, but roadmap D007 is
  generated-search refresh). That actively misleads readers.
- `src/core/mod.rs`: "Roadmap step 4 onward. Skeleton lives in `buffer`."
- `src/app/recovery.rs`: allowance reasons cite "checkpoint 7c".

**Fix:** rewrite these module docs in present tense and drop the step and
`D#` labels.

## 3. Prioritized findings

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

`replace_text_with_grouping` (`editor.rs:1096`) runs the same rebuild
again after the model update. Coordinate conversions (`to_flat_byte`,
`to_flat_utf16`, `flat_byte_to_position`) are linear scans over `lines`.
Every open view of the buffer repeats all of this.

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

### P0-2: Saving a symlinked file replaces the symlink with a regular file

`LocalFileSystemProvider::replace` → `atomic_replace`
(`src/app/filesystem/local.rs:361`) writes a temp file next to `path` and
`persist`s it, which is a `rename` over the path. Paths are never resolved
through `read_link`/`canonicalize` (they only are in extension discovery).
Editing a dotfile symlinked into a dotfiles repo, which is very common, will
silently detach it from the repo. The same rename approach also drops
hard links, ownership, and extended attributes. It also doesn't fsync the
parent directory after the rename, so the rename itself is not yet durable
when Save reports success.

**Recommendation:** resolve the final target (`std::fs::canonicalize`, or a
`read_link` loop) before choosing the temp-file directory and rename target.
Fsync the parent directory after `persist`. Add a unix test that saves
through a symlink and asserts the link survives.

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

### P1-2: Remove or fence the prototype surfaces (section 2)

Do this mechanically in one or two commits. It shrinks the production
surface, and it means the next real dead-code regression produces a
warning.

### P1-3: The product cannot stop a runaway extension turn

`ExtensionWatchdog` exists and is tested (`host/pool.rs:243`), but the product
never issues one. Termination happens only on heap-limit exhaustion and
on `unload`. The product never unloads except on startup rollback and
shutdown, and no command reloads or disables an extension. A
`while (true) {}` in a command or provider permanently occupies a pool
worker: its commands never complete, and with a small pool other
extensions starve. The UI stays responsive because painting never calls
JS, which is good, but there is no recovery short of quitting.

D019 deliberately defers *policy* (quotas, timeouts). A minimal escape hatch
is still worth having now:

- an "Extensions: Restart/Disable…" command that calls `unload`, which
  already terminates the running turn, or
- a generous per-turn wall-clock watchdog, measured in seconds, that
  terminates the lifecycle and reports it in the startup report.

### P1-4: Find is a stub in the menu and keymap

`editor.find`, `editor.find-next`, and `editor.find-previous`
(`product/commands.rs:268`) are declared with `claim = decline_find`, are
bound to ⌘F / Ctrl-F, and appear in the menu, but always return
`Unavailable`. The roadmap lists current-file Find as deferred (D031).
Either remove the menu items and bindings until D031 is selected, or
promote D031. Most users press ⌘F in their first five minutes.

### P1-5: `ProductShell` is a god object

`src/app/product.rs` has 4,600 production lines. The `impl ProductShell`
alone spans lines 439–3916 and has 91 methods. It owns:

- open/save dialogs and conflict prompts
- protected-close planning (`start_protected_close_plan`, 218 lines)
- tool-layout restore, explorer management, and contributed-view
  attachment
- extension-host observation
- transient interactions
- rendering of panes, layout, dividers, and the extension report
- the application menu and launch

It also carries 8,000 lines of inline tests plus `product/tests/`.

The behavior is well tested, so splitting the file is low-risk. Suggested
cut, following seams that already exist in the method names:

- `product/save.rs`: save, Save As, conflict reload, and prompts
- `product/open.rs`: open dialog, `apply_open_result`, `present_open_document`
- `product/closure.rs`: protected-close plan, commit, and `close_tab*`
- `product/tools.rs`: tool-layout restore, explorer, contributed views
- `product/render.rs`: `Render` impl, `render_pane`, `render_layout`,
  `render_divider`, `render_extension_report`
- `product/launch.rs`: `run`, `open_product_window*`, launch gate, menus

Move the inline `mod tests` into `product/tests/` alongside the existing
files.

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
macro-driven `DeclarationReader` (`engine.rs:3908`), while providers, tree
items, commands, and events still use bespoke per-type functions. The
decision record presents declarations as the way to avoid parallel transport
tables. The bespoke half is exactly such a table.

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

46 files use `terminal_*` (7), `extension_*` (8), `product*`, and
`personal_config*` as namespaces. Move them into `app/terminal/`,
`app/extensions/`, and `app/commands/` (P2-1). Do this as a pure move with
no behavior change. It also makes the routing table in `AGENTS.md` shorter.

### P2-3: Huge inline test modules

`engine.rs` (5,250 test lines), `product.rs` (8,000), `editor.rs`, `model.rs`,
`buffer.rs`, `scheduler.rs`, and `recovery.rs` all carry tests inline after
`mod tests`. `product/tests/` and `terminal_session_tests.rs` already show the
alternative. Move large test modules into sibling `tests.rs`/`tests/` files.
This improves navigation and incremental-compile ergonomics, and it would have
made the dead-code audit in this review simpler.

### P2-4: Three executor models

The app uses:

- the gpui background executor (SQLite stores, recovery)
- a dedicated multi-thread **tokio runtime** built only for `tokio::fs` in
  the local provider (`product.rs:4245`, `filesystem/local.rs:421`)
- the V8 worker pool's own threads

Tokio is otherwise used only for `sync` channels. `tokio::fs` is just
`spawn_blocking` underneath, so the local provider could run `std::fs` on the
gpui background executor like the SQLite stores do. That removes a runtime,
its threads, and the `rt-multi-thread` feature. It's not urgent, but it is
an easy simplification consistent with "keep the design simple."

### P2-5: Dependency hygiene

- `pollster` is used only in tests: move it to `[dev-dependencies]`.
- CI prints a future-incompatibility warning for `block v0.1.6` and
  `proc-macro-error2` (both via gpui). Track them, but there is nothing to
  do locally.
- gpui `0.2` is a pre-1.0 framework pulled from crates.io. Expect API churn.
  Keeping usage behind Knot-owned views, as you do now, is the right hedge.

### P2-6: Single-root workspace limit is enforced deep in the model

`WorkspaceState::replace_roots` rejects more than one root
(`TooManyWorkspaceRoots`). This is expected before D052. Note it so that
checkpoint 9 removes the error type entirely, rather than raising a
limit.

### P2-7: Small items

- The editor normalizes CRLF on input (`editor.rs:1263`) and trims a
  trailing `\r` for line ends, but stores files verbatim. A CRLF file will
  therefore get mixed line endings after edits. Pick a policy: detect the
  dominant ending on load and use it for inserted newlines.
- `DocumentInfo.kind` exposes `"generated"` in the public API
  (`knot.d.ts:50`) for a state the product never produces (see 2.1).
- `CommandOrigin` exists only under `cfg(test)` in `app/mod.rs`. Move it next
  to its tests.

## 4. Design soundness: second opinion by subsystem

### 4.1 Core and buffers 🟡

The design is sound: a piece table with stable IDs, an edit log as the single
anchor-repair path, and undo through ordinary mutations. Separating content
identity from monotonic revisions (so undo can return to clean) is a nice
touch. The implementation is still prototype-shaped (P1-1, §2.3). The core
is also `pub` and exported from the library crate, so its API surface isn't
checked for dead code. Make `core`/`host` `pub(crate)` unless an external
consumer exists. Currently only `tests/handler_linking.rs` might need
`host`, and it can use a narrow re-export.

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
correctly gated on durable capture. Apart from P0-2, my only reservation:
`ResourceVersion` is `(len, mtime, dev, ino)`. Some editors and sync tools
preserve mtime and length, so a same-size external edit within mtime
granularity goes undetected. That is acceptable for a text editor, but a
content hash of the loaded bytes would close the gap cheaply if it ever
shows up.

### 4.4 Workbench and lifecycle 🟢 / 🟡

The model is strong: a split tree with stable IDs, protected closure that
defers mutation until all decisions succeed, and exact presentation captures
for relocation. The tests are extensive (cross-window drag, tear-out,
closure). The debt is purely structural (P1-5). `workbench/tools.rs` (928
lines) and `tool_layout.rs` are cohesive.

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

Concerns are size and organization (P1-6), the missing runaway-turn escape
hatch (P1-3), and the dual marshaling style. The 32 MiB default heap per
isolate is reasonable. Reconsider it when bundled workflows (search,
explorer) move to JS.

### 4.6 Commands and keymaps 🟢 / 🟡

Captured targets with revalidation, explicit participant precedence, and
Knot-side keymap precedence are all the right calls for an Emacs-like
editor. Linking descriptors with `inventory` is a reasonable trade-off, and
it is guarded by `tests/handler_linking.rs` on all three platforms in CI.
The debt is fragmentation (P2-1) and the Find stub (P1-4).

### 4.7 Semantic UI 🟢

Trees, completion aggregation, transient interactions, and text frames sit on
one captured-window route, and the decision log is honest about provisional
parts (D054). Remove the leftover second completion surface (§2.1).

### 4.8 Documentation 🟢

The docs are high quality and mostly accurate. Drift I noticed:

- `docs/development.md` says "GitHub Actions does not build or test on
  macOS", but `ci.yml`'s `handler-platforms` job runs on `macos-latest` and
  `windows-latest`.
- `docs/development.md` doesn't mention the manual terminal fixtures or the
  `--terminal-fixture` mode.
- Code comments in `core` (§2.3).

## 5. Suggested sequencing

None of this needs to block checkpoint 7c. Suggested order:

1. **Cleanup commit series (½–1 day, low risk).** Do section 2: delete
   probes, move test-only code behind `#[cfg(test)]`, remove module-level
   `dead_code` allowances, fix the core comments, move `pollster` to dev
   dependencies, fix the `development.md` drift.
2. **Save through symlinks (P0-2).** Small and user-visible; do it
   immediately.
3. **Find stub (P1-4).** Hide the menu items, or schedule D031.
4. **Extension escape hatch (P1-3).** Add a restart/disable command or a
   coarse watchdog.
5. **Editor view data model (P0-1)** together with **piece-table quick wins
   (P1-1).** This is one focused slice and the prerequisite for D002 syntax
   highlighting and D031 Find. Plan it in `docs/plans/`.
6. **Structural splits (P1-5, P1-6, P2-1, P2-2, P2-3).** These are pure
   moves, best done between checkpoints, one subsystem per commit, so
   history stays readable and the plans don't collide.

## Appendix A: compiler-reported dead items (production build)

These are from `cargo check --lib` with every `dead_code` allowance removed
and only `run()` exported from the library. Tests use most of them.

```text
app/completion.rs        CompletionSurfaceKind, kind, CompactCompletionSurface, attach
app/documents.rs         DocumentId::value, DocumentState::Generated, persisted_version,
                         search_results (field+method), create_generated, create_search_results
app/editor.rs            select_reveal_and_focus, new_with_options, swap_completion_surface
app/extension_load.rs    DependencyPlan::execute
app/filesystem/mod.rs    child_uri
app/filesystem/local.rs  LocalFileSystemProvider::new, root
app/filesystem/memory.rs entire provider (MemoryDirectory, MemoryNode, MemoryFileSystemProvider, helpers)
app/model.rs             ContributionSource::BuiltIn, resolve_extension
app/personal_config.rs   field `directory`
app/recovery.rs          last_error, retain, release, write_retained
app/resource.rs          is_descendant_of, UriPathSegments::len
app/search_results.rs    entire module
app/tool_layout.rs       last_error
app/workbench.rs         new, open_tab, split_focused
app/workbench/close.rs   document_id, request_close_tab
app/workspace.rs         replace_root
core/anchored_range.rs   sticky, Anchor::pos, interval_index, len, is_empty, rebuild_index,
                         query_range, iter_live, several fields
core/buffer.rs           line_count, line_start, line_of_offset, take_edits,
                         ensure_line_starts, BufferEdit field `at`
host/declarations.rs     new, arguments, error_codes, handler_type, names
host/engine.rs           RuntimeErrorKind::kind, buffer_change_queue_metrics, RuntimeCapsule::{new,
                         execute, execute_fixture_module, hold_external_utf16},
                         EngineWork::{Script, FixtureModule}, three `wait` methods, others
host/pool.rs             load, execute_fixture_module, buffer_change_queue_metrics,
                         diagnostics, watchdog, shutdown, ExtensionWatchdog, terminate
host/protocol.rs         TransientKind, one `value` method
host/scheduler.rs        single_worker, ReadyForContinuation, ExtensionDiagnostics,
                         SchedulerDiagnostics, recv, next_turn, state, diagnostics,
                         worker_count, several fields
```
