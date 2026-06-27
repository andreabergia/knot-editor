# Rust Prototype Roadmap

High-level roadmap for a Rust prototype of Knot. The goal is not to build the editor, but to validate the riskiest claims in `design.md` before committing to them. Each step is scoped to answer a specific question; anything that does not reduce uncertainty about those questions is out of scope.

The order reflects dependency: earlier steps produce abstractions and evidence that later steps rely on. Steps can overlap, but no later step should start before its prerequisites have produced a tentative answer.

---

## 1. Establish the prototype skeleton

- Crate project skeleton.
- Crate layout separating core (model), view (rendering), and host (future scripting runtime).
- A minimal event loop and window capable of presenting a frame.
- Logging, basic profiling hooks, and a reproducible benchmark harness for later steps.
- Decide: which platforms to target for the prototype (at minimum macOS and Linux; Windows deferred unless a BiDi/IME regression appears).

**Question answered:** can the project stand up a GPU-backed window with the desired layering?

---

## 2. Renderer prototype and benchmark

The renderer is intentionally undecided. This step evaluates multiple candidates on the same workload before picking one. Candidates worth considering:

- **wgpu** — portable, low-level, gives full control and exposes the most about where the hard problems live.
- **skia** — mature, has a text-shaping path already, used by Chrome/Android; heavier dependency, less control.
- **gpui** — Zed's retained-mode GPU UI framework; opinionated and fast, but couples the project to Zed's architectural choices and licensing trajectory.
- A retained-mode GPU text shaper (e.g. cosmic-text / glyphon-style), as a fourth comparison point.

This step does not depend on the editable buffer model: a synthetic 1M-line text workload is enough to evaluate renderers. For each viable candidate:

- HarfBuzz-shaped text on a 1M-line workload scrolled at 120fps.
- Measure frame time, RSS, cold-start, and platform availability.
- Note constraints each candidate imposes on the future view API and on the no-internal-path commitment (step 7) — a renderer that forces privileged paths into the view is itself a finding.
- Decide primary renderer, or decide that more than one candidate survives and revisit after step 3.

**Question answered:** which renderer meets the performance and platform-reach bar without undermining other commitments?

---

## 3. IME, BiDi, and wide-character metrics

- A single view handling all three simultaneously: IME composition, Arabic/Hebrew BiDi reordering, CJK wide-character column alignment.
- Validate cursor movement, selection, and column alignment under each.
- Run on every surviving renderer from step 2; BiDi/IME support varies enough across candidates that this step may eliminate some of them.
- Identify failure modes and whether they are renderer-level or API-level.

**Question answered:** can the renderer and future view API support the full set of text-shaping requirements that are explicitly non-deferrable?

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
