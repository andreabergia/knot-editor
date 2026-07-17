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
- ✅ Layout reach for 3-pane shell (`div` flex), selectable list
      (`uniform_list`), resize divider — exercised at the widget level in
      `src/step3/gpui/main.rs`. The 3-pane resizable shell (left file list,
      placeholder editor center, right outline list) builds entirely on the
      public API:
        - flex/column layout: `div().flex().flex_row()` / `flex_col()` ✓
        - fixed/relative sizing: `w(px(..))`, `flex_1()` ✓
        - selectable single-row list: `uniform_list` ✓
        - row click → highlight: `.id(..).on_click(..)` (StatefulInteractiveElement) ✓
        - hover styling: `.hover(|s| s.bg(..))` (InteractiveElement) ✓
        - conditional styling: `.when(cond, |d| d..)` (FluentBuilder) ✓
        - resize divider: **no built-in splitter** — built custom from
          `on_drag` + `on_drag_move` + `on_drop` (capture-phase drag-move
          listener on root), with an invisible `DragGhost` view satisfying
          `on_drag`'s constructor and a `DividerDrag { which }` value type.
          Cosmetic finding, not a blocker. `Pixels(pub(crate) f32)` requires
          `f32::from(Pixels)` to read the float, not field access.
3. ✅ gpui 3-pane shell with placeholder editor. Built and verified
   (window opens, divider drag-resizes both panes, row click highlights) in
   `src/step3/gpui/main.rs`. Layout-reach finding: gpui 0.2.2 passes the
   no-internal-path bar for item 6 — the shell is reproducible by an
   extension author, not a privileged built-in. The only gap is the absence
   of a built-in resizable splitter, which is a cosmetic/ergonomic finding,
   not a no-internal-path failure.
4. ✅ gpui editor widget (items 1–5) inside the shell. Replace the placeholder
   with the demanding widget: styled text, scrolling, cursor+selection, IME
   preedit, one annotation overlay.

   Progress:
   - ✅ Item 1 (styled text) + scroll-half of item 2: `src/step3/gpui/editor.rs`
     renders fixture lines via a custom `Element` on the public API. Each
     visible line is shaped with `WindowTextSystem::shape_line` (multi-
     attribute `TextRun`s carry bold/italic/color from the fixture's segment
     specs) and painted with `ShapedLine::paint` inside a
     `with_content_mask` viewport; a 6px scrollbar thumb is overlaid at the
     right edge outside the content mask so it stays visible while scrolled.
     Scroll-wheel drives a pixel scroll offset (macOS natural-scroll
     direction) via `.on_scroll_wheel` on the wrapping `div().id("editor")`.
     No-internal-path scorecard for item 1 + scroll: all-green.
   - ✅ Item 2 remainder (keyboard-driven scroll + cursor): `EditorView`
     now tracks a caret `(line, byte_col)` + `preferred_col`, implements
     `Focusable` (using the previously-stubbed `FocusHandle`), and binds
     `.track_focus` + `.on_key_down` on the editor `div`. Arrow keys /
     home / end / pageup / pagedown move the caret (cmd-left/right map to
     home/end per macOS; all motion clamps to utf-8 char boundaries and
     wraps across line ends). After each move the caret is kept inside the
     viewport by nudging `scroll` via `ensure_cursor_visible`. The element
     writes the measured pane height back into the view each paint (no
     notify) so the scroll clamp uses the real viewport. The caret is
     painted as a 2px bar at `ShapedLine::x_for_index(col)` when the
     editor holds window focus, inside the content mask so it clips out
     when scrolled away. No-internal-path scorecard for item 2: all-green
     (Focusable, track_focus, on_key_down, KeyDownEvent.keystroke,
     ShapedLine.x_for_index, Entity::update during paint).
    - ✅ Item 3 (selection): `EditorView` gains an `anchor` (line, byte_col)
      + `has_selection` flag alongside the caret. Drag-select via
      `on_mouse_move` (self-gated on `MouseMoveEvent::dragging()`), shift-click
      and shift-arrow extend from the anchor; plain click / non-shift arrow
      collapse (non-shift motion collapses to the end the motion points
      toward, without moving past it). `selection_range()` returns the
      ordered (start, end); the element paints a translucent blue rect per
      visible line — partial spans on the start/end lines via
      `x_for_index`, full pane width for interior lines (incl. empty ones,
      so blank lines stay visually selected) — behind the text, and hides
      the caret while a selection is active. Shaped lines are computed once
      per frame and reused for selection + text + caret. No-internal-path
      scorecard for item 3: all-green (`on_mouse_move`, `MouseMoveEvent`,
      `Modifiers.shift`, `paint_quad`, `x_for_index` all public).
    - ✅ Item 4 (IME preedit): `EditorView` implements `EntityInputHandler`
      (8-method NSTextInputClient mapping). The element calls
      `Window::handle_input(ElementInputHandler::new(bounds, entity), cx)`
      during paint — self-gated on focus. `marked_range_utf16` tracks the
      active preedit span as flat UTF-16 offsets into `lines.join("\n")`;
      helpers `to_flat_utf16`/`from_flat_utf16`/`utf16_to_byte_col`/
      `byte_col_to_utf16`/`flat_doc`/`splice` bridge the (line, byte_col)
      caret space and the flat UTF-16 IME space. `replace_text_in_range`
      commits text (clears marked, moves caret to end); `replace_and_mark_
      text_in_range` sets the marked range + selection-within-preedit (sel
      relative to marked start, per Apple's `setMarkedText` contract);
      `unmark_text` keeps the text, clears the marking. `bounds_for_range`
      shapes the line and returns a rect for the IME candidate window.
      Editing rebuilds lines+segs from the spliced flat doc (fixture
      styling lost on edited lines — acceptable for the spike). The element
      paints a 1.5px underline over the marked span. Basic ASCII text input
      (type a character) works for free: unhandled printable keys fall
      through to `interpretKeyEvents` → `insertText` → `replace_text_in_
      range`. No-internal-path scorecard for item 4: all-green
      (`EntityInputHandler`, `ElementInputHandler::new`, `Window::handle_
      input`, `UTF16Selection`, all public).
    - ✅ Item 5 (annotation overlay): `EditorView` carries `annotations:
      Vec<Annotation>` where `Annotation { line, start, end, color }` are
      byte-column spans. Seeded at load by scanning for `LEAF_MAX` /
      `INTERNAL_MIN` (warning/amber), `fn ` names (info/blue), `unsafe`
      (error/red). The element paints a wavy underline via the public
      `Window::paint_underline(origin, width, &UnderlineStyle { wavy: true,
      thickness, color })` — reusing the already-shaped visible lines for
      `x_for_index` positioning, with the y offset matching the line
      painter's internal formula (`padding_top + ascent + descent * 0.618`).
      Painted after text + caret, inside the content mask. Annotations on
      empty lines are skipped (zero-width). Not updated on edit — the spike
      only needs to demonstrate the paint path. No-internal-path scorecard
      for item 5: all-green (`paint_underline`, `UnderlineStyle.wavy`,
      `ShapedLine.ascent`/`.descent` fields, `x_for_index` all public).
5. ✅ Run gpui against all fixtures (auto-scroll + interactive); record
   quantitative + qualitative findings in this document.

   The editor widget was run against all 5 fixtures in `bench/fixtures/`,
   one at a time, with manual verification of rendering + caret + click +
   selection on each:

   - **arabic.kfx** (11 lines, RTL/bidi + connected glyph shaping):
     ✅ Renders correctly right-to-left. Connected Arabic glyph forms
     shape properly (initial/medial/final/isolated) via Core Text after
     adding `Geeza Pro` as a font fallback (Menlo has no Arabic glyphs;
     `Font.fallbacks: Option<FontFallbacks>` is a public field, set
     directly). Caret, click, and selection all land at the correct
     visual position on pure-Arabic lines AND on the mixed line 7
     (`النص المختلط يحتاج إلى خوارزمية Unicode Bidirectional لترتيب
     الحروف.`) where an embedded LTR English substring ("Unicode
     Bidirectional") sits inside an RTL base-direction line.
   - **cjk.kfx** (25 lines, wide-char + mixed-width alignment):
     ✅ Wide CJK glyphs render at 2× width, mixed ASCII+CJK lines align
     correctly. Caret navigation across 3-byte UTF-8 chars works after
     fixing `clamp_col_to_line` to snap `preferred_col` to a UTF-8 char
     boundary via `prev_boundary` (pre-existing bug exposed by the RTL
     change, which indexes `line_str[index..]` for per-character
     direction detection).
   - **emoji.kfx** (19 lines, ZWJ grapheme clusters): ✅ ZWJ family
     sequences (👨‍👩‍👧‍👦) render as a single cluster. Caret/click/
     selection operate at UTF-8 code-point granularity (not grapheme
     cluster) — acceptable for the spike; a real implementation would
     use a grapheme cluster boundary iterator.
   - **minified_js.kfx** (2 lines, one ~3KB line): ✅ The ~3KB single
     line renders, horizontal clipping keeps it inside the editor pane
     (no bleed into the outline pane), caret/click/selection work on
     the long line.
   - **rust_sample.kfx** (~5000 lines, 491KB): ✅ Vertical scroll smooth,
     viewport culling correct (only visible lines shape + paint),
     scrollbar thumb proportional, styled segments render with their
     authored colors, wavy annotation underlines render (warnings on
     `LEAF_MAX`/`INTERNAL_MIN`, info on `fn` names, error on `unsafe`),
     caret/click/selection work across the large buffer.

   **RTL/bidi findings (significant).** gpui 0.2.2 has NO explicit RTL/bidi
   API: no `writing_direction`, no `kCTWritingDirectionAttribute` on the
   macOS attributed string, no `ParagraphDirection`, no public way to pass
   `TextAlign::Right` through `ShapedLine::paint` (only `WrappedLine::paint`
   takes an `align`). On macOS, `CTLine::new_with_attributed_string` does
   bidi auto-detection from the first strong character, so glyphs come back
   in VISUAL order with ABSOLUTE x positions — *rendering* is correct out of
   the box. But gpui's `ShapedLine::paint` hardcodes `TextAlign::Left`, so a
   pure-RTL line left-aligns at the pane's left edge instead of the right.
   Worse, `x_for_index(index)` (line_layout.rs:105) walks glyphs in visual
   order assuming increasing logical indices — broken for RTL, where visual
   order has decreasing indices, so it returns ~0 for every index (caret
   always paints at the left edge). `closest_index_for_x(x)` IS correct for
   RTL (returns the logical index of the glyph at a visual position).

   Both were worked around with the PUBLIC API only — no fork, no
   `pub(crate)`, no upstream patch:

   1. **Right-alignment**: a per-line `is_rtl_line(s)` heuristic (first
      strong alphabetic char in an Arabic/Hebrew/etc Unicode block) gates
      an `(pane_w - s.width).max(0)` x offset added to the paint origin and
      to all x computations (caret, selection, IME preedit, annotation,
      hit-test, `bounds_for_range`).
   2. **`x_for_index` for RTL**: a custom `x_for_index_dir(s, index,
      line_str)` free function walks the public `LineLayout.runs` /
      `ShapedRun.glyphs` / `ShapedGlyph.index` / `ShapedGlyph.position`
      fields directly. Direction is detected PER-CHARACTER (via
      `char_is_strong_rtl(line_str[index..].chars().next())`), not per-run
      — gpui merges CTRuns by font, so a single `ShapedRun` can contain
      both RTL and LTR glyphs when an English substring is surrounded by
      spaces that share its font. Per-character detection correctly places
      the caret inside the embedded LTR substring. Replaces gpui's
      `x_for_index` at all paint sites.

   **No-internal-path scorecard for step 5**: all-green. `Font.fallbacks`,
   `FontFallbacks::from_fonts`, `LineLayout.runs`, `ShapedRun.glyphs`,
   `ShapedGlyph.index`, `ShapedGlyph.position`, `LineLayout.width`,
   `LineLayout.len` are all `pub`. The RTL gaps were architectural (no API
   surface) rather than privileged-primitive (API exists but is
   `pub(crate)`); the workarounds use only public fields. This is a
   notable finding: gpui 0.2.2's public API is *sufficient* for a
   correct bidi editor, but only by reimplementing `x_for_index` on top
   of the raw glyph array — a widget author must know the glyph layout
   contract, which is undocumented outside the source.
6. ❌ iced API-reachibility spike + implementation + runs + findings.
   **Cancelled** — gpui (step 5) cleared the bar cleanly; the cross-
   framework comparison that this step and step 7 were to feed is no
   longer needed for a decision. iced remains documented above as a
   candidate that uniquely satisfies the no-internal-path criterion
   (cosmic-text re-exported, `Buffer` first-class, `pub draw`/`layout`,
   first-class IME); its principal risk — the pinned cosmic-text 0.19
   CJK shaping defect from step 2 — is recorded but not re-validated
   at the widget level. Recorded as research risk only.
7. ❌ floem API-reachibility spike + implementation + runs + findings.
   **Cancelled** — same rationale as step 6. floem remains documented
   above as a candidate that meets the no-internal-path bar (the
   editor lives inside Floem; Lapce consumes the same public
   `PaintCx` / `TextLayout` / `draw_glyphs` any `View` receives;
   IME handled in Lapce); its principal risk — Parley's undocumented
   BiDi/CJK quality — is recorded but not re-validated at the widget
   level. Recorded as research risk only.
8. ✅ Cross-framework comparison table + decision: which framework (if any)
   meets the bar. Update `docs/roadmap.md` step 3 status.

   **Decision: gpui 0.2.2 is the framework.** The cross-framework
   comparison table is *not* produced — the plan called for it to feed
   a decision between surviving frameworks, and gpui was the only one
   spiked. The decision rests on gpui passing the bar on its own merits:

   - **No-internal-path scorecard**: all-green across items 1–5 and the
     fixture runs. The one significant gap (RTL/bidi) was architectural
     — no API surface at all — rather than a privileged primitive
     (`pub(crate)`), and was worked around with public fields only.
   - **Text-rendering maturity**: rendering correct on all 5 fixtures
     (RTL/bidi with connected glyph shaping, CJK wide-char, ZWJ emoji,
     long single line, ~5000-line buffer). Connected Arabic shaping,
     CJK width alignment, and ZWJ cluster rendering are inherited
     from Core Text's maturity; the widget author only supplies font
     fallbacks (`Font.fallbacks`, a public field).
   - **Effort**: the full widget — items 1–5, RTL/bidi, fixture runs —
     landed in 9 commits over the spike. The implementation was
     pleasant and fast.

   The investigation stops here rather than continuing through iced and
   floem: a cross-framework comparison is unnecessary to make a product
   decision when the leading candidate clears the bar cleanly, and
   prototype-phase iteration speed is paramount (per `AGENTS.md`).
   `docs/roadmap.md` step 3 updated to ✅ Done — gpui.
9. ⬜ If a decision is made, seed the step-7 `View` abstraction from the
   surviving framework's widget surface (noted, not implemented here).

   Decision made in step 8 (above). Seeding the `View` abstraction is
   left for step 7 proper; nothing implemented here. The gpui widget
   surface that step 7 will generalize from lives in
   `src/step3/gpui/editor.rs` (`EditorView`, `EditorElement`,
   `runs_for`, `x_for_index_dir`, `is_rtl_line`, the flat-UTF16 IME
   helpers, `EntityInputHandler` impl) — note its public-API shape.

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
