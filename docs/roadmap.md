# Rust Prototype Roadmap

High-level roadmap for a Rust prototype of Knot. The goal is not to build the editor, but to validate the riskiest claims in `design.md` before committing to them. Each step is scoped to answer a specific question; anything that does not reduce uncertainty about those questions is out of scope.

The order reflects dependency: earlier steps produce abstractions and evidence that later steps rely on. Steps can overlap, but no later step should start before its prerequisites have produced a tentative answer.

---

## 1. Establish the prototype skeleton

- ✅ Crate project skeleton.
- ✅ Crate layout separating core (model), view (rendering), and host (future scripting runtime).
  - Implemented as modules within a single `knot` binary crate (`core`, `view`, `host`, `app`); crate-splitting deferred until step 7's no-internal-path experiment demands compile-time enforcement.
- ✅ A minimal event loop and window capable of presenting a frame.
  - winit event loop opens a macOS window. GPU/rendering deferred to step 2.
- ⏭️ Logging, basic profiling hooks, and a reproducible benchmark harness for later steps.
  - Deferred: nothing to measure yet. Revisit at step 2 (renderer frame times) and step 4 (buffer edits).
- ✅ Decide: which platforms to target for the prototype (at minimum macOS and Linux; Windows deferred unless a BiDi/IME regression appears).
  - macOS only for the prototype. Linux revisited if/when renderer choice (step 2) calls for it.

**Question answered:** can the project stand up a GPU-backed window with the desired layering?

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
- Note constraints each candidate imposes on the future view API and on the no-internal-path commitment (step 7) — a renderer that forces privileged paths into the view is itself a finding.
- Decide primary renderer, or decide that more than one candidate survives and revisit after step 3.

**✅ Done — with a deferral.** The benchmark ran; see `docs/step2-renderer-benchmark.md` for raw numbers and findings. At the rendering-primitive level, Skia is the clear winner (lowest p50 on every fixture; survives BiDi/CJK/emoji without API surgery; cosmic-text has a real CJK defect). However, the benchmark compared rendering *primitives*, not UI frameworks, and could not answer the product-level question of what to build the editor UI on. A framework with its own sufficiently mature text-rendering path might make the primitive-level choice moot. That framework-level evaluation is step 3 below. The "no internal path" objection was overapplied to frameworks: that commitment (step 7) governs the buffer/view boundary, not whether the rendering stack is hand-written.

**Question answered (at the primitive level):** Skia meets the performance bar; the framework-level bar is step 3's question.

---

## 3. UI framework comparison

Step 2 answered the rendering-primitive question (Skia wins) but left open the product question: what does the editor build its UI on? A framework may bundle a mature text-rendering path, making direct use of Skia unnecessary; or it may expose Skia as its primitive, in which case step 2's evidence carries forward. The "renderer primitive" and "UI framework" questions may turn out not to be separable at all. This step evaluates frameworks against criteria including:

- text-rendering maturity (shaping, BiDi, CJK fallback, color emoji);
- extension-level access to the same rendering/text primitives the built-in views use (the sharpened form of the step-7 "no internal path" commitment);
- platform reach and embedding story.

The candidate list and research are deferred to this step.

**Question answered:** which framework (if any) meets the rendering-maturity, extensibility, and platform bar without importing a privileged stack?

---

## 4. Buffer model

- A `TextBuffer` with an editable rope or piece-table backing store.
- Stable positions that survive edits (the foundation annotations depend on).
- Basic operations: insert, delete, replace, read-range, random access by offset.
- Internally benchmark: per-edit cost on a 1M-line buffer, randomized edit streams, concurrent-ish edit sequences.
- Decide on the representation (rope vs piece table vs piece table with stable IDs).

**Question answered:** is there a buffer representation that is fast enough and exposes stable enough positions for the annotation layer?

---

## 5. Annotation offset stabilization

- An annotation model attached to the buffer that tracks positions across edits.
- Implementation candidates: interval tree, stable-ID piece table, offset remap log.
- Stress test: 10k annotations on a 1M-line buffer under a randomized edit workload.
- Measure: stabilization cost per edit, correctness under concurrent feature sources, behavior at edit boundaries (sticky-before vs sticky-after).
- Decide representation and the semantics exposed to extensions.

**Question answered:** can annotations track positions cheaply and correctly enough to be the universal composition mechanism the design claims?

---

## 6. Annotation composition

- Multiple annotation sources (diagnostics, search matches, git hunks, folding, breakpoints) active on a single buffer simultaneously.
- Confirm no precedence surprises and no rendering conflicts when a sixth source is added without re-plumbing.
- Identify any case where composition requires privileged coordination and document it.

**Question answered:** does the "annotations compose naturally" claim hold beyond toy cases?

---

## 7. View abstraction and the no-internal-path stress test

- A `View` trait carrying presentation state: scroll, cursor, selections, rendering options.
- Two views of one buffer (one folded, one not) with independent state, plus a minimap view and a "different zoom" view.
- Verify the buffer never needs to know about folding, scroll, or cursor to serve a view.
- The load-bearing experiment: implement the built-in text editor view *purely* against the public view API, with no backdoor into the buffer or renderer internals.
- If it cannot be done, iterate the API until it can, or record that the claim must be softened before building further.

**Question answered:** is the "no internal path extensions cannot reach" commitment actually achievable for the most demanding built-in surface?

---

## 8. Text-as-universal-surface stress test

- A git-status buffer and a search-results buffer implemented as real editable `TextBuffer`s with backing features, not widgets.
- Drive them through the same view pipeline as source files.
- Identify the point, if any, at which representing a surface as text becomes a liability, and what widgets would then be required.

**Question answered:** does the "text remains central" philosophy hold for non-source surfaces?

---

## 9. Command and keymap dispatch

- Command objects as first-class values: name, arguments, invokable programmatically.
- Keymap resolution with transient and active keymaps.
- Programmable dispatch: prefix-arg or `M-x`-style invocation.
- Composition of commands.
- Verify every user action is addressable as a command object with the same arguments a keypress would supply.

**Question answered:** are commands a sound substrate for keybindings, programmatic invocation, and composition?

---

## 10. Capability aggregation and replaceable UI surface

- Two independent completion providers and one diagnostic provider feeding a single shared UI surface.
- Core merges, dedupes, and routes results; providers can be added and removed at runtime.
- Replace the completion surface mid-session without dropping in-flight results or restarting providers.
- Verify the new surface receives aggregated results with no special wiring.

**Question answered:** is the provider/surface split actually implementable as described, not just as a diagram?

---

## 11. Async ergonomics prototype

This step is portable across host choices and can be deferred until after the renderer/UI questions are settled. It is included here because it constrains the eventual scripting runtime and the core API shape.

- A command that appears synchronous to its author but auto-suspends across await points without callback hell.
- Cancellation propagating cleanly across an await chain spanning a capability provider, an LSP request, and a UI update.
- An extension that hangs; confirm detection, interruption, and isolation without taking down the editor.
- Benchmark against Emacs' synchronous model for writability.

**Question answered:** is the "asynchronous by default" commitment writable by ordinary users, or only by engine authors?

---

## 12. URI / filesystem provider abstraction

- A remote `ssh://` provider participating in workspace enumeration, search, and an LSP session, identically to the built-in `file://` provider.
- Identify any core path that bypasses the provider interface and hard-codes `file://` semantics.
- Fix or document leaks.

**Question answered:** does the filesystem provider abstraction actually generalize, or does it leak `file://` assumptions into the core?

---

## Out of scope for the prototype

The following are explicitly deferred and should not be attempted during the prototype phase:

- Choosing between V8, JavaScriptCore, and WebAssembly as the scripting runtime.
- Packaging format, dependency resolution, load order.
- Session persistence semantics.
- Binary and terminal buffer types.
- AI integration as a standardized capability.
- Any feature whose feasibility is not directly load-bearing on the design commitments.

These become relevant only after the prototype has produced tentative answers to the no-internal-path, renderer, annotation, and async-ergonomics questions.
