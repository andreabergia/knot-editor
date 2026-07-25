# Rust Prototype Roadmap

High-level roadmap for a Rust prototype of Knot. The goal is not to build the editor, but to validate the riskiest claims in `design.md` before committing to them. Each step is scoped to answer a specific question; anything that does not reduce uncertainty about those questions is out of scope.

The order reflects dependency: earlier steps produce abstractions and evidence that later steps rely on. Steps can overlap, but no later step should start before its prerequisites have produced a tentative answer.

---

## 1. Establish the prototype skeleton

- ✅ Crate project skeleton.
- ✅ Crate layout separating core (model), view (rendering), and host (future scripting runtime).
  - Implemented as modules within a single `knot` binary crate (`core`, `view`, `host`, `app`); crate-splitting deferred until a concrete module boundary benefits from compile-time enforcement.
- ✅ A minimal event loop and window capable of presenting a frame.
  - winit event loop opens a macOS window. GPU/rendering deferred to step 2.
- ⏭️ Logging, basic profiling hooks, and a reproducible benchmark harness for later steps.
  - Deferred: nothing to measure yet. Revisit at step 2 (renderer frame times) and step 4 (buffer edits).
- ✅ Decide: which platforms to target for the prototype (at minimum macOS and Linux; Windows deferred unless a BiDi/IME regression appears).
  - macOS only for the prototype. Linux revisited if/when renderer choice (step 2) calls for it.

**Question answered:** can the project stand up a native event-loop window with the desired module layering? → **Yes.** GPU presentation and renderer performance are answered by step 2.

---

## 2. Renderer prototype and benchmark

The renderer is intentionally undecided. This step evaluates multiple candidates on the same workload before picking one. Candidates worth considering:

- **wgpu** — portable, low-level, gives full control and exposes the most about where the hard problems live.
- **skia** — mature, has a text-shaping path already, used by Chrome/Android; heavier dependency, less control.
- **gpui** — Zed's retained-mode GPU UI framework; opinionated and fast. Reinstated as a candidate in step 3 — coupling is true of all dependencies, and the license is Apache-2.0.
- A retained-mode GPU text shaper (e.g. cosmic-text / glyphon-style), as a fourth comparison point.

This step does not depend on the editable buffer model: a synthetic 1M-line text workload is enough to evaluate renderers. For each viable candidate:

- HarfBuzz-shaped text on a 1M-line workload scrolled at 120fps.
- Measure frame time, RSS, cold-start, and platform availability.
- Note constraints each candidate imposes on the future native view API and extension contribution points (step 8).
- Decide primary renderer, or decide that more than one candidate survives and revisit after step 3.

**✅ Done — with a deferral.** The benchmark ran; see `docs/step2-renderer-benchmark.md` for raw numbers and findings. At the rendering-primitive level, Skia is the clear winner (lowest p50 on every fixture; survives BiDi/CJK/emoji without API surgery; cosmic-text has a real CJK defect). However, the benchmark compared rendering *primitives*, not UI frameworks, and could not answer the product-level question of what to build the editor UI on. A framework with its own sufficiently mature text-rendering path might make the primitive-level choice moot. That framework-level evaluation is step 3 below. Renderer internals do not need to cross the scripting boundary; step 8 instead tests the public contribution points and custom-view API built above them.

**Question answered (at the primitive level):** Skia meets the performance bar; the framework-level bar is step 3's question.

---

## 3. UI framework comparison

Step 2 answered the rendering-primitive question (Skia wins) but left open the product question: what does the editor build its UI on? A framework may bundle a mature text-rendering path, making direct use of Skia unnecessary; or it may expose Skia as its primitive, in which case step 2's evidence carries forward. The "renderer primitive" and "UI framework" questions may turn out not to be separable at all. This step evaluates frameworks against criteria including:

- text-rendering maturity (shaping, BiDi, CJK fallback, color emoji);
- public rendering/text primitives sufficient for extension-owned custom views,
  without requiring extensions to replace the native text editor view;
- platform reach and embedding story.

The candidate list is now fixed. Frameworks to evaluate:

- **gpui** (and the **gpui-ce** community fork) — Zed's retained-mode GPU UI framework; Apache-2.0. Reinstated in step 2. The leading candidate: ships a production editor with mature text rendering. Open questions: whether the text primitives the built-in editor view uses are reachable at the same level by an extension/widget author, and whether the community fork meaningfully improves standalone (non-Zed) usability.
- **iced** — Elm-architecture, wgpu + cosmic-text; MIT. Powers COSMIC. Early research confirms it uniquely well-satisfies the no-internal-path criterion (cosmic-text is publicly re-exported; a raw `Buffer` primitive is first-class; the built-in widgets' `draw`/`layout` are `pub`); IME is a first-class API. Risk: inherits cosmic-text's CJK shaping defects verbatim (pinned to 0.19, uses harfrust), and depends on a forked winit.
- **floem** (Lapce) — reactive, Parley + peniko + fontique + swash (the Linebender text stack) with vello/skia backends; MIT. Early research confirms it meets the no-internal-path bar: the editor lives *inside* Floem, and Lapce consumes the same public `PaintCx` / `TextLayout` / `draw_glyphs` that any `View` receives; IME is already handled in Lapce. Risk: BiDi/CJK quality is inherited from Parley and not separately documented at the Floem layer; pre-1.0, with a forked winit and git-only transitive deps.

Dropped from consideration after initial research:

- **makepad** — fails the no-internal-path commitment structurally: the script/DSL VM exposes only low-level draw uniforms, not the text shaping/layout primitives, while built-in Rust widgets consume those primitives directly. Adopting it would import a privileged text path.
- **xilem/masonry** — would satisfy the no-internal-path criterion in principle (Parley + Vello + Fontique), but Xilem is pre-alpha, Masonry is pre-alpha and churning, and Vello is alpha. Not usable as a framework today; the vello+parley primitives could be revisited as a build-it-ourselves sub-path if no framework survives step 3.

✅ Done — **gpui** is the framework.

The gpui spike built the full demanding editor widget (styled text, scroll,
caret, selection, IME preedit + editing, wavy annotation overlay) and a
3-pane resizable shell, strictly against gpui 0.2.2's *public* API — no
`pub(crate)`, no fork, no upstream patch. It passed all 5 fixtures
(arabic/bidi, cjk, emoji/ZWJ, minified long-line, ~5000-line rust) with
manual verification.

The no-internal-path scorecard is all-green. The one significant finding
was architectural, not privileged-primitive: gpui 0.2.2 has no explicit
RTL/bidi API surface (no `writing_direction`, no `TextAlign::Right` on
`ShapedLine::paint`). Core Text auto-detects bidi so *rendering* is
correct out of the box, but `ShapedLine::paint` left-aligns and
`x_for_index` is broken for RTL. Both were worked around with public
fields only — a per-line `is_rtl_line` heuristic + right-align offset,
and a custom `x_for_index_dir` that walks the public
`LineLayout.runs` / `ShapedGlyph.index` / `ShapedGlyph.position` with
*per-character* direction detection (per-run is unreliable: gpui merges
CTRuns by font, mixing RTL and LTR glyphs in one run).

The investigation was short-circuited here: the original plan was to
spike iced and floem next for a cross-framework comparison. With gpui
clearing the bar cleanly and the implementation proving pleasant and
fast to build, the comparison is unnecessary for a decision. The iced
and floem spikes (steps 6 and 7 of the execution plan) are cancelled;
their findings noted only as research risk in `docs/step3-framework-
comparison.md`. Step 9 (seed the `View` abstraction from the surviving
framework's widget surface) is left for step 8 proper.

Caveats carrying forward: the RTL workaround requires undocumented
knowledge of the glyph layout contract (the widget author reimplements
`x_for_index` on the raw glyph array); a real editor will want a
grapheme cluster boundary iterator rather than UTF-8 code-point
granularity for caret/selection over ZWJ sequences.

**Question answered:** which framework (if any) meets the rendering-maturity, extensibility, and platform bar without importing a privileged stack? → **gpui 0.2.2.**

---

## 3b. Linux gpui portability smoke test

This is an opportunistic checkpoint, not a reopening of step 2 or step 3. It
should happen before step 8 invests heavily in a gpui-shaped native `View`
abstraction, because step 3's successful bidi/IME/text spike was verified on
macOS and the RTL workaround leans on gpui/Core Text layout behavior.

- Run the step-3 editor widget on Linux.
- Verify the same text fixtures: Arabic/bidi, CJK, emoji/ZWJ, minified long-line, and large Rust source.
- Check whether the RTL alignment and custom `x_for_index_dir` workaround still hold outside Core Text.
- Smoke-test IME if the local environment makes that practical.
- Record any platform-specific shaping, fallback-font, input-method, or windowing assumptions before step 8 bakes the native view API around them.

⏭️ Opportunistic / before serious step-8 work.

**Question answered:** does the gpui decision rely on macOS-only text or input behavior that would materially change the view abstraction?

---

## 3c. Windows gpui portability smoke test

This is the Windows counterpart to 3b. It happened before step 8 work, on a
Windows 10 development machine, using the same step-3 editor widget and
fixtures.

⚠️ **Partial result — Windows is not yet a clean pass.**

- ✅ **Startup:** the first launch failed with
  `STATUS_ENTRYPOINT_NOT_FOUND` for `TaskDialogIndirect`. The cause was the
  crate's `default-features = false` dependency configuration excluding
  gpui's `windows-manifest` feature, so the executable did not request Common
  Controls v6. Enabling `windows-manifest` embeds gpui's supplied manifest and
  the widget launches on Windows.
- ✅ **CJK mouse insertion:** a click in CJK text initially inserted Latin
  text at the end of the line. The widget mixed UTF-8 byte columns (used by
  gpui shaping) with UTF-16 offsets (used by the input API). Converting byte
  columns to UTF-16 at the input boundary fixed the observed CJK insertion
  case.
- ⚠️ **Arabic/BiDi selection:** horizontal selection remains logical-order
  based: `Shift+Right` in a pure RTL line expands visually left. The intended
  editor behavior is visual left/right navigation and selection, so this is
  an unresolved widget-policy/API problem.
- ❌ **Emoji editing:** the emoji fixture still has incorrect Backspace/Delete
  behavior around emoji, including ZWJ and modifier sequences, after making
  cursor motion and hit testing grapheme-aware. This is a blocking correctness
  failure for the current widget; do not treat the macOS text/IME result as
  portable to Windows.

**Decision:** keep the gpui framework decision, but do not bake its current
editor-widget input behavior into the step-8 native `View` abstraction. Before
serious step-8 work, reproduce the Windows emoji deletion failure in a
minimal case and either correct it using public gpui APIs or record an
upstream/framework limitation. Run the Linux checkpoint only after that
Windows blocker has a conclusion.

**Question answered:** does the macOS gpui spike transfer unchanged to
Windows? → **No.** Window startup and CJK insertion are viable, but the
current widget fails the Windows Unicode-editing bar.

---

## 4. Buffer model

Plan and decisions recorded in `docs/step4-buffer-plan.md`.

- ✅ `TextBuffer` with a stable-ID piece-table backing store (built by hand, not wrapped — see D1/D2).
- ✅ `Position` token (opaque, stable across unrelated edits) + a `BufferEdit` edit-log surface that step 5 can subscribe to.
- ✅ Basic operations: insert, delete, replace, read_range, random access by byte offset.
- ✅ Line index (lazy build, incremental under edits); raw text storage, no `\r` normalization (D3/D4).
- ✅ Internal benchmark: per-edit cost on a 1M-line buffer (reuse step-2 `rust_sample.kfx --tile 635` fixture), randomized edit streams, interleaved-producer (single-threaded, D5) scenario. Output in `docs/step4-buffer-benchmark.md`.
- ✅ Decision checkpoint: confirm rope-vs-stable-ID-PT, write answer here, proceed to step 5. → **Stable-ID piece table confirmed.** The piece table alone runs at ~10k edits/sec on a 1M-line buffer (p50 75 µs on a 79k-piece chain), two orders of magnitude above single-user editing rates. `Position` tokens resolve in sub-µs on realistic warm chains, and the D7 remap-by-log path is ~41 ns idle / ~100 ns per matching `Split` — negligible. The only bottleneck is the `Vec<usize>` line index (~88% of per-edit latency with the index live is `update_line_starts`' O(line_count) suffix shift); that's a localized swap to a `BTreeMap`/Fenwick behind the existing `line_count` / `line_start` / `line_of_offset` API, deferred until step 5 surfaces a real need. A rope would have answered only the throughput question and forced the annotation layer (step 5) to remap every offset after every edit — exactly what step 5 is trying to avoid.

**Accepted prototype limitation:** because pieces split but never merge, piece
count and retained inserted text grow with edit history (`O(edits)`). Stable
positions make transparent compaction an architectural follow-up rather than a
localized optimization. The existing benchmark supplies sufficient constants
for the prototype; no additional long-session benchmark is planned unless
ordinary prototype use exposes the problem.

**Deferred out of step 4:** full undo/redo history; non-UTF-8 encodings; file
save/load I/O; syntax-tree integration; the annotation model itself (step 5);
views (step 8).

**Non-goal:** CRDTs and real-time collaborative editing will not be implemented.
The prototype and its APIs should not reserve complexity for them.

**Question answered:** is there a buffer representation that is fast enough and exposes stable enough positions for the annotation layer? → **Yes.** Proceed with the stable-ID piece table.

---

## 5. Annotation offset stabilization

- An annotation model attached to the buffer that tracks positions across edits.
- Implementation candidates: interval tree, stable-ID piece table, offset remap log.
- Stress test: 10k annotations on a 1M-line buffer under a randomized edit workload.
- Measure: stabilization cost per edit, correctness under concurrent feature sources, behavior at edit boundaries (sticky-before vs sticky-after).
- Decide representation and the semantics exposed to extensions.

✅ **Decision checkpoint complete.** Plan and decisions recorded in `docs/step5-annotation-plan.md`.

**Chosen representation:** `Position`-token anchors (the stable-ID piece table
approach, D1). Each annotation stores its endpoints as `Position` tokens issued
by `TextBuffer::position_at`, inheriting piece-ID stability from the buffer.
Annotations are not scanned during edits unless their piece is touched —
untouched annotations resolve through their stable tokens.

**Endpoint semantics exposed to extensions:**

| Concept | Implementation |
|---------|---------------|
| Anchor | `Position` token + `Stickiness::{Before, After}` |
| Stickiness | `Before` (sticky-left, default for selection start): insert at endpoint → text lands before it, endpoint keeps its byte. `After` (sticky-right, default for selection end): insert at endpoint → endpoint relocates past the inserted span. |
| Delete behavior | Endpoints inside a delete span snap to the nearer surviving edge (`Before` → left edge, `After` → right edge). Fully-deleted annotations are removed entirely (step 6b: no tombstone; undo does not revive them). |
| Stale tokens | Right-half split tokens detected via `resolve` → `None` and repaired through the edit-log `Split` records. |
| Query | `query_range(&mut self, buffer, &[a, b))` returns all annotation ids overlapping `[a, b)`, backed by a lazy-rebuilt sorted interval index for O(log n + k) lookup. |

**Benchmark:** ✅ Full stress test completed (`src/bin/anno_bench.rs`; findings
in `docs/step5-annotation-benchmark.md`). On a 1M-line buffer with 10k
annotations, token stabilization measured p50 12.2–12.4 µs across the 3-source
and 6-source workloads, versus 69.5–69.7 µs for the offset-remap baseline
(5.6–5.7× faster). Resolve-all measured p50 1.79 ms and range queries measured
p50 15.5–15.6 µs.

**Question answered:** can annotations track positions cheaply and correctly enough to be the universal composition mechanism the design claims? → **Yes.** Token-anchored annotations with piece-ID stability survive arbitrary edits with zero per-annotation work for untouched ranges; affected-anchor repair is O(touched) per edit. Proceed to step 6 (annotation composition).

---

## 6. Annotation composition

- Multiple annotation sources (diagnostics, search matches, git hunks, folding, breakpoints) active on a single buffer simultaneously.
- Confirm that adding a sixth source does not require re-plumbing the annotation store, buffer model, or capability model.
- Exercise basic query composition for overlapping sources across editor, minimap, and gutter-style consumers.
- Document any case where data-level composition requires privileged coordination.
- Defer rich visual precedence, layering, hit-testing, and annotation conflict policy until the real editor view needs them.

✅ **Done.** Plan and findings recorded in `docs/step6-annotation-composition.md`.

**Changes made:**
- `AnnotationKind` extended with `Folding` variant and `Hash` derive.
- `AnnotationStore::query_range_for_kinds(&mut self, buffer, a, b, kinds: &[AnnotationKind])` — per-kind query filtering for consumers (gutter, minimap).
- `AnnotationStore::iter_live(&self)` — iterate all live annotations.
- Composition stress test: 5 sources overlapping on an 80-line buffer, queried by 3 consumer perspectives (editor: all, gutter: diag+bp, minimap: diag+search+git). A 6th source (`Other(0)`) added with zero store changes.
- Per-source `AnnotationData(String)` remains opaque; rich payloads deferred to view layer.
- No privileged coordination required: sources never intersect, consumers filter by kind at query time.

**Question answered:** do independent annotation sources compose at the static
data/query level, without forcing special coordination into the core? → **Yes.**
Sources are independent callers of `add()`. Consumers compose per-kind with
`query_range_for_kinds`. `AnnotationKind` describes category, not provider
ownership; add an opaque source identity when a real provider lifecycle needs
teardown rather than treating that routine modeling detail as a separate
experiment.

---

## 6b. Reversible edit transaction proof

- Represent one transaction as one or more primitive buffer edits.
- Record enough information to invert insert, delete, and replace.
- Undo and redo one transaction; verify text correctness and pass annotations
  through the inverse edits using their existing stabilization rules.
- Defer history trees, grouping heuristics, persistence, view-state restoration,
  command integration, and memory reclamation.

✅ **Done.** Plan and findings recorded in `docs/step-6b-plan.md`.

**Changes made:**
- `core::transaction::EditTransaction` (`src/core/transaction.rs`): a single-use
  stateful recorder over `TextBuffer` with `insert` / `delete` / `replace` /
  `undo` / `redo`. The mutation methods apply immediately, capture the bytes
  needed to invert each primitive via `read_range` before mutating, and skip
  no-op primitives. `undo` applies recorded inverses in reverse primitive
  order; `redo` reapplies the originals in forward order. Both route
  exclusively through `TextBuffer::{insert, delete, replace}`, so each
  direction emits the normal `BufferEdit` log entries and preserves line-index
  maintenance. The transaction checkpoints both the originating buffer's
  stable instance identity and `edit_seq` before every recorded primitive and
  before `undo` / `redo`, panicking on an out-of-band edit or buffer swap
  rather than applying stale raw offsets. Invalid orderings panic (double-`undo`, `redo` without
  `undo`, mutate-after-`undo`), matching `TextBuffer`'s precondition style.
- Annotation consumption semantics: a fully consumed annotation is now
  **removed** (annotation id, both endpoint-index entries, cached offsets)
  instead of being retained as a `collapsed` tombstone. `resolve` returns
  `None` for that id, `query_range` / `query_range_for_kinds` / `iter_live`
  cannot return it, and the id is never reused (monotonic counter). Undoing
  the consuming text edit does not revive the annotation — a provider may
  re-publish under a fresh id later (step 10's lifecycle owner).
- `core::transaction` acceptance tests: multi-primitive UTF-8 round-trip,
  two-cycle undo/redo, line-index validity through undo/redo, no-op
  primitives not recorded, `replace`-decomposition into insert/delete
  primitives, panic tests for invalid orderings, and a combined
  forward/undo/redo + annotation flow that verifies unaffected /
  boundary-sticky / partially-overlapped annotations remain queryable while
  the wholly-consumed annotation is removed at every pass.
- `cargo test core` green: 103 tests, 0 failures.

**Question answered:** can the selected buffer and stable-position model support
reversible transactional edits without violating their core invariants? →
**Yes.** `EditTransaction` routes everything through the existing `TextBuffer`
mutation surface, so piece IDs, the append-only `Add` store, the unbounded edit
log, line-index maintenance, and annotation stabilization all retain their
invariants across forward, undo, and redo. Annotation removal-on-consumption
keeps the store free of invisible collapsed tombstones; undo does not need to
revive annotations because annotations are provider-derived state, not
transaction state — a provider observing a new buffer revision publishes a
fresh annotation (step 7 / 10).

---

## 7. V8 scripting runtime and extension boundary

✅ **Complete.** Knot uses JavaScript on V8 for configuration, built-in
behavior, and extensions. The prototype starts from `deno_core`, kept behind a
strict Knot-owned V8 wrapper; this is not a generic scripting-engine
abstraction and does not preserve hypothetical language or engine
replaceability. Evidence and detailed decisions are recorded in
`docs/step7-v8-runtime.md`.

Explore three concerns separately rather than treating the host boundary as
one vertical slice.

### Runtime foundation

- Run each extension in its own V8 isolate.
- The target architecture schedules isolates on a bounded V8 worker pool, but
  `deno_core::JsRuntime` is thread-affine. For this prototype, give each
  extension one OS thread for its lifetime; retain the bounded movable-isolate
  pool as the later `rusty_v8` design. Use a shared Tokio runtime for
  asynchronous host work, timers, cancellation, and message routing.
- Prove that callbacks from different extensions execute in parallel while
  never executing two callbacks in the same isolate concurrently.
- Measure isolate startup and memory overhead. Exercise V8 interruption, CPU
  limits, memory limits, and noisy-neighbor behavior enough to validate that
  the pool can isolate failures; production quota policy is not required.
  "Basic limits" means proving that runaway CPU and heap growth can be stopped;
  budget accounting, configurability, diagnostics, and quota policy are
  production concerns.
- Confirm that an extension can fail or be disposed without taking down other
  isolates or leaking its registered work.

### Commands

- Register and invoke a scripted command.
- Let a command read and edit a buffer through the host API.
- Run across an await point while the editor stays responsive; cancel the
  command and prevent a late state update.
- Report thrown and rejected errors without crashing the editor.

### Events

- Subscribe to and dispose a buffer-change listener.
- Deliver committed, revisioned changes to multiple extension isolates in
  parallel while preserving ordering within each extension.
- Measure queue growth and lag for a finite slow-consumer workload; defer the
  production backpressure policy.

Views are a separate exploration in the next section. Other scope boundaries
for this experiment are maintained in `docs/step7-v8-runtime.md`.

### UTF-16 boundary snapshot spike

Before closing the runtime decision, test whether Knot can retain its UTF-8
editable core while avoiding repeated UTF-8 → V8-string conversion:

- [x] Add a bounded, revision-aware cache of immutable UTF-16 snapshot storage
  at the editor/runtime boundary. Invalidate it on edits and never let V8
  borrow mutable `TextBuffer` storage. ✅
- [x] Expose the cached storage to V8 as an external two-byte string whose
  lifetime is independent of the foreground-owned buffer and safe across
  extension isolates. A focused test proves two isolates retain and release
  independent references to one backing allocation. ✅
- [x] Separate and compare cold UTF-8 → UTF-16 cache construction, cache-hit
  external-string creation, and the existing UTF-8/serde path for ASCII and
  Unicode payloads through 10 MiB. Record retained-memory costs. ✅
- [x] Decide from the measurements whether to adopt this boundary cache,
  investigate a UTF-16 core, or retain ordinary scoped conversion. ✅ Retain
  the UTF-8 core and the bounded boundary cache; the cache-hit path reduces a
  10 MiB snapshot to about 0.3–0.4 ms and shares one allocation across
  isolates. Do not infer a UTF-16 core from this result: cold 10 MiB ASCII
  conversion is about 4.2× slower and retains 20 MiB of UTF-16 beside the
  UTF-8 core. If this path survives the prototype, add an external one-byte
  backing for ASCII rather than forcing it through UTF-16.

**Question answered:** which language and engine will Knot use? → **JavaScript
on V8.**

**Question answered:** does the thread-per-extension `deno_core` prototype
provide useful parallelism and bounded failure isolation, and do commands and
events form a sound first public API without leaking `deno_core` details? →
**Yes for the prototype boundary.** Isolates execute in parallel, callbacks are
serial within an extension, cancellation and fatal interruption contain late
work, and the combined gpui failure proof leaves another extension and the
foreground heartbeat responsive. Commands, revisioned batched edits, and
ordered events are a sound first API.

One OS thread per extension is not the production topology: an incremental
isolate costs about 4.2 ms and 2.3 MiB idle RSS before its reserved thread. Keep
the bounded movable-isolate `rusty_v8` pool as the target, preserving the
validated Knot-owned protocol and lifecycle semantics. Current string evidence
supports scoped strings plus batched edits and the bounded UTF-16 snapshot
cache; it does not justify a public byte-oriented API.

---

## 8. Extension view API

Plan and agreed decisions are recorded in
`docs/step8-extension-view-plan.md`.

Views are a separate exploration from commands and events. Extension code runs
away from the UI thread and must not receive gpui or native rendering objects,
so the important question is what public UI model crosses that boundary.

Explore two related but distinct semantic surfaces:

- **Native tree-provider surface:** replace the outline fixture with a native
  tree view populated asynchronously by an extension-owned data provider.
  Native code owns layout, selection, focus, scrolling, interaction state, and
  future accessibility mapping. Rendering must never wait on synchronous
  JavaScript execution; failure and disposal remove provider state cleanly.
- **Native editor contributions:** keep text shaping, selection, IME,
  accessibility, and low-level rendering native, while extensions contribute
  anchored decorations, gutter markers, and command-backed actions through
  public APIs. Built-in high-level features should use the same native
  contribution registry.

Use two views of one buffer to confirm that cursor, selection, folding, scroll,
and rendering choices remain per-view state. Investigate whether the chosen
semantic tree model preserves enough information for a plausible accessibility
path; production accessibility support is not part of the prototype.

Do not build a general Knot widget tree or declarative layout language.
Text-oriented surfaces should remain buffer-backed. Arbitrary custom UI and a
possible WebView escape hatch are deferred.

Do not mix capability-provider aggregation or replaceable capability surfaces
into this experiment; those need their own concrete feature later.

**Question answered:** are semantic native provider/contribution APIs
expressive and responsive across the isolate/UI-thread boundary without Knot
inventing a general UI framework, and where is the boundary between shared
buffer data and per-view presentation state?

---

## 9. Terminal buffer and view experiment

Terminal support is a required part of Knot. This experiment validates its
architectural fit without building a production-complete terminal emulator.

- Use a mature terminal parser/state crate; do not implement VT escape parsing
  from scratch.
- Implement a `TerminalBuffer` that owns a local pseudoterminal subprocess and
  terminal state, plus a native terminal view rendered through gpui.
- Run an interactive shell with keyboard input, asynchronous output, ANSI
  color, cursor state, and scrollback.
- Resize the view and propagate the new terminal dimensions to the PTY.
- Run one full-screen TUI using the alternate screen to expose assumptions that
  a line-oriented shell session would miss.
- Close and restart the subprocess cleanly, and confirm terminal output cannot
  block the UI event loop.
- Defer exhaustive escape-sequence compatibility, mouse reporting, hyperlinks,
  shell integration, remote PTYs, and cross-platform polish.

**Question answered:** do `TerminalBuffer`, PTY lifecycle, terminal state, and a
native terminal view fit Knot's buffer/view and async architecture well enough
to make first-class terminal support viable?

---

## 10. Text-as-primary-surface stress test

- Implement one representative generated surface, initially search results, as
  an inspectable `TextBuffer` rather than a widget. It may be read-only.
- Drive it through the normal text-view pipeline, attach commands to result
  regions, and refresh its contents once.
- Try a second surface such as git status only if search results do not expose
  meaningful limitations.
- Identify the point, if any, at which representing a surface as text becomes a liability, and what widgets would then be required.

**Question answered:** does the "text remains central" philosophy hold for non-source surfaces?

---

## 11. Command and keymap dispatch

- Command objects as first-class values: name, arguments, invokable programmatically.
- Keymap resolution with transient and active keymaps.
- Programmable dispatch: prefix-arg or `M-x`-style invocation.
- Composition of commands.
- Invoke the same semantic operation from a keybinding, command palette, and
  script code with the same explicit arguments.
- Confirm raw input protocols such as IME composition can update interaction
  state without becoming registered commands.

**Question answered:** are commands a sound substrate for keybindings, programmatic invocation, and composition?

---

## 12. Capability aggregation and replaceable UI surface

- Two independent completion providers feed one shared UI surface; one responds
  immediately and one later.
- Update the surface as results arrive. Tag requests/results with the relevant
  request or document revision and discard a late result after a newer request
  makes it stale.
- Replace the completion surface mid-session without restarting providers; the
  new surface receives the current aggregate with no special wiring.
- An error from one provider does not erase the other's result.
- Use a deliberately simple completion-specific merge policy. Do not infer a
  universal ranking, deduplication, streaming, or backpressure model.

**Question answered:** is the provider/surface split actually implementable as described, not just as a diagram?

---

## 13. URI / filesystem provider abstraction

- A simple in-memory provider rooted at a non-`file://` URI, supporting URI
  normalization, directory enumeration, read, write, and stat.
- Open a workspace and buffer through it despite the resource having no
  corresponding OS path.
- Identify any core path that bypasses the provider interface and hard-codes `file://` semantics.
- Fix or document leaks.

**Question answered:** does the filesystem provider abstraction actually generalize, or does it leak `file://` assumptions into the core?

---

## Out of scope for the prototype

The following are explicitly deferred and should not be attempted during the prototype phase:

- Packaging format, dependency resolution, load order.
- Production extension isolation, forced interruption, workers/realms, and memory limits.
- Persistent buffer snapshots and production undo/history semantics beyond the reversible-transaction proof.
- Piece-table compaction and long-session reclamation policy.
- Session persistence semantics.
- Binary buffer types.
- Production terminal compatibility and polish beyond the step-9 experiment.
- AI integration as a standardized capability.
- Any feature whose feasibility is not directly load-bearing on the design commitments.

These become relevant only after the prototype has produced tentative answers
to the renderer, annotation, scripting-boundary, view-contribution, and async
programming-model questions.
