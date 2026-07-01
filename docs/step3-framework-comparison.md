# Step 3 — UI Framework Comparison

Execution plan for `docs/roadmap.md` step 3. Step 2 closed the rendering-
*primitive* question (Skia wins) but explicitly deferred the product
question: *what does the editor build its UI on?* This step answers it.

## Goal

Answer the roadmap's question: *which framework (if any) meets the
rendering-maturity, extensibility, and platform bar without importing a
privileged stack?*

The decider is qualitative, not a number. The benchmark builds the same
demanding widget on each candidate, strictly against each framework's
**public** (`pub`) API as if by an extension/widget author, and records where
that was possible and where it required a backdoor (`pub(crate)`, fork,
upstream patch, or a primitive the built-in views use but widgets can't
reach). That record is the literal answer to step 3's question.

Quantitative metrics (frame time, RSS, CPU) are collected for comparability
with step 2 and as a regression floor; they are not the decider. A framework
that wins on frame time but fails the no-internal-path scorecard is rejected.

## Scope

In scope:
- A demanding editor widget (spec below) implemented once per framework,
  against the public API only.
- A 3-pane shell hosting the widget, built on each framework's public
  layout primitive.
- A benchmark harness reusing the step-2 fixtures and metrics.
- A per-framework no-internal-path scorecard + text-rendering-maturity notes,
  recorded in this document.

Out of scope (deferred):
- The real `TextBuffer` and editable buffer model (step 4). The widget's
  buffer is a trivial in-widget string / `Vec<String>`.
- The real `View` trait with scroll/cursor/selection semantics (step 7).
  The widget owns its own presentation state ad hoc.
- A second view of one buffer (minimap, zoom) — step 7's job. The widget is
  single-view.
- Annotation offset stabilization across edits (step 5). The overlay sits at
  a fixed position; no edit workload.
- Annotation composition across multiple sources (step 6). One overlay only.
- Command/keymap dispatch (step 9), capability/provider plumbing (step 10),
  async ergonomics (step 11), real IME state machine beyond preedit display.
- Cross-platform matrix beyond macOS (per step 1); Linux noted as a
  reach criterion, not re-benchmarked.
- Automated tests (per `AGENTS.md`: prototype phase).

## The renderer-primitive carry-forward

Step 2's Skia finding is **subsumed** by the framework choice, not held in
parallel. Whichever framework wins dictates the rendering primitive: floem
can expose a skia backend (step 2 evidence carries forward there); gpui and
iced do not expose Skia directly. Standalone Skia is dropped. If no framework
passes the no-internal-path bar, the fallback is to revisit building on Skia
primitives directly — but that is a step-3 failure outcome, not a planned
branch.

## Candidates

Fixed per roadmap step 3. Evaluated **serially, gpui first**, so early
findings can sharpen the spec for later frameworks.

1. **gpui** (and **gpui-ce** community fork) — Zed's retained-mode GPU UI
   framework; Apache-2.0. Ships a production editor with mature text
   rendering. Open question: whether the text primitives the built-in editor
   view uses are reachable at the same level by an extension/widget author,
   and whether gpui-ce improves standalone (non-Zed) usability. Evaluated
   first because it is the leading candidate and has the most to teach about
   the no-internal-path bar.
2. **iced** — Elm-architecture, wgpu + cosmic-text; MIT. Early research
   (roadmap) confirms it uniquely well-satisfies the no-internal-path
   criterion: cosmic-text is publicly re-exported, a raw `Buffer` primitive
   is first-class, built-in widgets' `draw`/`layout` are `pub`, IME is a
   first-class API. Risk: inherits cosmic-text's CJK shaping defects
   verbatim (step 2 found 40% of CJK glyphs dropped via swash atlas), pinned
   to 0.19, forked winit.
3. **floem** (Lapce) — reactive, Parley + peniko + fontique + swash (the
   Linebender text stack) with vello/skia backends; MIT. Early research
   (roadmap) confirms it meets the no-internal-path bar: the editor lives
   inside Floem, Lapce consumes the same public `PaintCx` / `TextLayout` /
   `draw_glyphs` any `View` receives, IME is handled in Lapce. Risk: BiDi/CJK
   quality inherited from Parley, not separately documented at the Floem
   layer; pre-1.0, forked winit, git-only transitive deps.

Dropped from consideration (roadmap): **makepad** (fails no-internal-path
structurally — script/DSL VM exposes only low-level draw uniforms, not
text shaping/layout primitives), **xilem/masonry** (pre-alpha, not usable
today).

## The widget — one spec, three implementations

Implemented identically on each framework, strictly against the public API.

### Editor view (items 1–5)

1. **Styled text rendering** — multi-attribute segments (bold, italic, color
   per segment). Reuses the step-2 fixtures: Rust source, CJK markdown,
   Arabic prose, emoji chat log, minified JS, 1M-line tiled Rust. Forced so
   every framework exercises its multi-attribute shaping path, not just
   color runs.
2. **Scrolling** — both:
   - **Programmatic auto-advance** (the step-2 workload: 1 line/frame,
     capped at buffer end, wraps). Drives the quantitative benchmark and
     exercises the repaint-after-scroll-cull path.
   - **Keyboard-driven** (arrow keys / page up/down). Exercises the
     framework's input → redraw path, not just forced repaints. This is
     where retained-mode vs immediate-mode differences show up.
3. **Cursor + single selection** — rendered as an overlay on top of the
   text (a caret and a highlighted range). Stresses whether the public text
   primitive lets a widget *co-locate* its own drawing (cursor bar,
   selection fill) alongside the framework's glyph runs, or whether
     overlays require a privileged path the built-in editor uses but
     widgets can't.
4. **IME composition** — the canonical "GPU text renderers fail here"
     fixture from `docs/exploration.md` §"IME, BiDi, and wide-character
     metrics": a live IME preedit string updating in-place, with the
     preedit underline rendered as an overlay. Single most discriminating
     gap surfaced by step 2 (cosmic-text's atlas path); a framework whose
     public IME API is absent or `pub(crate)` is a finding, not a patch-up.
5. **One annotation overlay** — a diagnostic squiggle (or search-match
     highlight) drawn under/over a fixed text range, *without* mutating the
     text. Tests the public-decoration path and seeds step 5/6. The overlay
     is at a fixed position; no edit workload, no stabilization.

### Shell (item 7)

6. **3-pane shell** — left panel, editor center, right panel — built on each
   framework's **public** layout/composition primitive (flex/grid/column/
   whatever it ships), as an extension author would. Panel **content** is a
   **selectable single-row-list widget** (click highlights a row; the
   selection does nothing else — no cross-widget wiring, no features). This
   exercises:
   - nested container layout composition on the public API;
   - whether a resize divider (if the framework exposes one publicly) is
     reachable by a widget author or is a privileged built-in;
   - a minimal widget-composition check (the selectable list is itself a
     small widget built on the public API), without scope-creeping into
     real features.

The point of the shell is *layout reach*, not chrome. The shell itself must
not be a privileged container that extensions can only consume, not
reproduce — that would be a no-internal-path finding. Seeds step 7's
"Windows are layout containers… split horizontally/vertically… arranged in
trees" claim at the framework level.

### Deferral

- **Second view of one buffer** (minimap, zoom) — step 7's load-bearing
  experiment. Building it here would duplicate that work; kept single-view.
- **Editing** beyond what keystrokes/IME drive through the cursor path —
  the buffer is trivial; we are not validating the buffer model.

## Fixtures

Reused verbatim from step 2 (`bench/`), no new fixtures:

| # | Fixture | Size | Stresses (framework-relevant) |
|---|---|---|---|
| 1 | Real Rust source | ~2k lines | Multi-attribute segments, baseline retained-view repaint |
| 2 | Tiled Rust → 1M lines | 1M | Visible-range culling, retained-view scroll, peak RSS |
| 3 | Chinese markdown | ~1k lines | CJK fallback (the cosmic-text/Parley risk), wide-glyph columns |
| 4 | Arabic prose | ~500 lines | BiDi reordering, RTL paragraph direction, base-direction heuristics |
| 5 | Emoji chat log | ~1k lines | ZWJ sequences, color glyphs, mixed scripts |
| 6 | Minified JS | ~50 lines × ~5k chars | Horizontal scroll, huge shaping runs, line-wrap behavior |

The 1M-line fixture is generated at startup by tiling fixture 1, as in step 2.

## Workload

Two modes, same harness:

- **Auto-scroll** (quantitative): programmatic auto-advance at 1 line/frame,
  5s per fixture per framework, fixed 1200×800 window, vsync off where the
  framework exposes it. Identical to step 2 so numbers are comparable.
- **Interactive** (qualitative): a human drives keyboard scroll + cursor
  movement + IME composition on each framework's widget, on fixtures 1, 3,
  4, 5. Not timed. The finding is whether the paths exist publicly and feel
  correct (cursor lands where expected, IME preedit renders in place, BiDi
  caret motion is sane), not a number.

## Metrics

### Quantitative (per framework, auto-scroll, all fixtures)

Same harness as step 2; collected by the harness, not the widget:

- **Frame time** (p50, p99) — primary comparability signal with step 2.
- **Peak RSS** — answers "does the framework hold the whole buffer in GPU
  memory" and surfaces retained-mode overhead.
- **CPU usage** — averaged over the run; catches "holds 120fps while pegging
  a core."

### Qualitative (the decider — recorded per framework in this document)

- **No-internal-path scorecard**: every place the widget required a
  non-`pub` / `pub(crate)` / fork / upstream patch / a primitive the
  built-in views use but widgets can't reach, listed with severity
  (blocker / workaround / cosmetic). This is the literal answer to step 3.
- **Text-rendering maturity**: did BiDi/Arabic, CJK fallback, color emoji,
  and IME preedit all work *without* surgery? Step 2 flagged cosmic-text's
  40% CJK glyph-drop (an iced risk) and Parley's undocumented BiDi/CJK
  quality (a floem risk) generically; step 3 confirms or refutes at the
  widget level.
- **Extension-surface ergonomics**: can the widget be registered/swapped
  like a replaceable UI surface (`design.md` §"UI Surfaces vs. Capability
  Providers"), or does the framework force a privileged registration path?
  Seeds step 10.
- **Layout reach**: was the 3-pane shell buildable on the public layout
  API, including any resize divider? Seeds step 7.
- **Platform reach**: macOS runs; is Linux the same code path, or does it
  require a different backend / forked winit? Noted, not re-benchmarked.

## Architecture

One binary per framework, each a self-contained example that opens a window
and hosts the widget inside the shell. No shared `Renderer` trait from step
2 — each framework has its own widget primitive; forcing a common trait
would defeat the comparison. Shared code is limited to fixture loading and
the metric collection (reused from step 2's harness).

```
src/
  step3/
    common/
      fixtures.rs     reuses bench/ fixtures + loader from step 2
      metrics.rs      frame/RSS/CPU collection (reused from step 2)
    gpui/            shell + editor widget on gpui (or gpui-ce)
    iced/            shell + editor widget on iced
    floem/           shell + editor widget on floem
```

Each framework dir is a `[[bin]]` (or example) selected at runtime; compiled
unconditionally per step 2's rationale (link cost doesn't matter for a
benchmark). The framework choice is made by which binary is run.

## Execution order

1. ✅ Plan written (this document).
2. 🔄 gpui API-reachability spike: confirm the public text + layout + IME
   primitives exist at the level the widget needs. Record preliminary
   no-internal-path scorecard. (If a blocker is found here, decide whether
   gpui-ce changes the picture before proceeding to implementation.)

   Progress:
   - ✅ Standalone build from crates.io (`gpui 0.2.2`,
     `default-features = false, features = ["font-kit"]`) — no Zed repo glue.
     Build prerequisite: one-time `xcodebuild -downloadComponent
     MetalToolchain` (macOS ships the shader toolchain separately from Xcode).
   - ✅ Window + declarative render path reachable on the public API:
     `src/step3/gpui/main.rs` (`Application::run`, `App::open_window`,
     `Context::new`, `Render`, `div().flex().child(...)`). Window opens and
     stays alive.
   - ⬜ Text-shaping primitives (`WindowTextSystem::layout_line`, `ShapedLine`,
     `ShapedGlyph`, `StyledText`/`TextRun`, multi-attribute runs) — surveyed in
     docs.rs, not yet exercised at the widget level.
   - ⬜ IME primitives (`ElementInputHandler` / `PlatformInputHandler`,
     `Window::handle_input`, preedit underline) — surveyed only.
   - ⬜ Layout reach for 3-pane shell (`div` flex), selectable list
     (`List`/`UniformList`), resize divider — surveyed only.
3. ⬜ gpui widget + shell implementation, full 6-item spec.
4. ⬜ Run gpui against all fixtures (auto-scroll + interactive); record
   quantitative + qualitative findings in this document.
5. ⬜ iced API-reachibility spike + implementation + runs + findings.
6. ⬜ floem API-reachibility spike + implementation + runs + findings.
7. ⬜ Cross-framework comparison table + decision: which framework (if any)
   meets the bar. Update `docs/roadmap.md` step 3 status.
8. ⬜ If a decision is made, seed the step-7 `View` abstraction from the
   surviving framework's widget surface (noted, not implemented here).

Each step is at least one commit; large steps (e.g. a full framework
implementation) span many commits, split at logical boundaries as the work
progresses. The scorecard for a framework is filled in
as that framework's runs complete, not all at the end — early findings
sharpen the spec for later frameworks.

## Risks / things that may change the plan

- **gpui standalone usability** — gpui is developed inside Zed; pulling it
  out may require non-trivial glue or the gpui-ce fork. If standalone use
  forces `pub(crate)` workarounds, that is itself the central finding, but
  it may also block completing the widget. Mitigation: the API-reachability
  spike (step 2 of execution order) gates implementation.
- **iced's pinned cosmic-text (0.19)** — the CJK glyph-drop defect from step
  2 may persist at the widget level. If it does, iced fails the
  text-rendering-maturity bar and the finding is recorded; we do not patch
  cosmic-text in a prototype.
- **floem's git-only transitive deps + forked winit** — may block clean
  builds or surface version-conflict noise. Mitigation: pin a known-good
  rev; if it blocks build entirely, record as a platform-reach finding
  rather than rearchitecting.
- **IME on macOS** — the preedit fixture requires a live input source; the
  benchmark can drive a synthetic preedit string programmatically to test
  the *rendering* path where a framework's IME *input* API is hard to drive
  headlessly. The synthetic path is the fallback; live IME is the
  interactive-mode check.
- **Spec drift between frameworks** — the temptation to "fix" a framework's
  gap by widening the widget spec (e.g. adding a feature one framework
  makes easy). Resist: the spec is frozen at execution step 2's completion.
  Findings that suggest spec changes are recorded, not applied mid-run.
- **Second-view temptation** — step 7's job. If a framework makes it
  trivially cheap, note it as a positive platform-reach signal but do not
  build it; scope discipline preserves cross-framework comparability.

## Non-goals (explicit)

- No real editor functionality beyond the widget spec above.
- No real `TextBuffer` (step 4).
- No real `View` trait with scroll/cursor/selection semantics (step 7).
- No second view of one buffer (step 7).
- No annotation stabilization (step 5) or composition (step 6).
- No command/keymap dispatch (step 9).
- No capability/provider plumbing (step 10).
- No async ergonomics (step 11).
- No persistence.
- No automated unit tests (per `AGENTS.md`).
- No cross-platform re-benchmark (macOS only, per step 1).
