# Areas to explore

This document enumerates experiments intended to validate the choices made in `design.md`. Each area lists concrete prototypes, the question each is meant to answer, and what would count as evidence for or against the current design.

The areas are not independent. Two concerns cut across all of them and should be prototyped first, because their answers constrain everything else:

- **Async ergonomics** — whether the "asynchronous by default" commitment is writable by ordinary users, not just engine authors.
- **The "no internal path extensions cannot reach" commitment** — whether the built-in UI can actually be built on the public API, or whether it silently requires a backdoor. If it does, the commitment should be softened before the rest of the design is built on top of it.

---

## Buffer/view/annotation model

### Anchored-range offset stabilization

Annotations must track their positions across edits to the underlying text. This is the classic hard problem (Emacs overlays, VS Code decorations all struggle with it).

- Prototype: a buffer carrying 10k annotations, subjected to a randomized sequence of edits.
- Measure: stabilization cost, correctness under concurrent edits, behavior at edit boundaries (sticky-before vs sticky-after).
- Decide: representation (interval tree? piece table with stable IDs? offset remap log?), and the semantics exposed to extensions.

### Anchored-range composition across features

The design claims annotations compose naturally because views decide rendering. Confirm this holds beyond toy cases.

- Prototype: a single line carrying diagnostics, search matches, git hunks, and folding regions simultaneously.
- Add a sixth feature (e.g. breakpoints) without re-plumbing the view, and confirm no rendering conflicts and no precedence surprises.

### Multiple views of one buffer

The text-buffer/editor-view split is justified by divergent presentation.
Confirm the split carries its weight without requiring every view to have a
buffer.

- Prototype: two editor views of the same buffer, one folded and one not, with
  independent scroll and cursor state, plus a minimap view and a preview at a
  different zoom.
- Verify: presentation state lives entirely on the view; the buffer never
  needs to know about folding to serve a folded view.

### Text as the universal surface

The "text remains central" claim is load-bearing for the whole philosophy. Stress-test it.

- Prototype: a git-status buffer and a search-results buffer implemented as real editable `TextBuffer`s with backing features (not widgets).
- Identify the point, if any, at which representing a surface as text becomes a liability rather than a convenience, and document what widgets are then needed.

---

## UI

### No-internal-path stress test

The native text editor owns shaping, hit testing, selection, IME,
accessibility, and low-level rendering. The extensibility claim applies to its
high-level behavior and contribution points, not to those internals.

- Prototype: built-in and extension-authored decorations, gutters, and
  contextual behavior using the same public contribution APIs.
- Identify any user-visible high-level behavior that still requires a
  privileged path.

### Extension view boundary

Extension views cross from isolated JavaScript execution to the native UI
thread, so exposing framework objects directly is not viable.

- Prototype: one nontrivial custom non-editor view through a Knot-owned public
  UI model, including updates, input, failure, and disposal.
- Verify: rendering never waits on synchronous JavaScript execution, and the
  model has a plausible accessibility path.
- Decide: where the API boundary lies between custom views and contributions
  to the native text editor view.

### Renderer choice

The choice of renderer constrains performance and platform reach.

- Prototype: a 1M-line buffer rendered with HarfBuzz-shaped text, scrolled at 120fps, on wgpu, on skia, and on a retained-mode GPU text shaper.
- Measure: frame time, RSS, cold-start, and platform availability.

### IME, BiDi, and wide-character metrics

GPU text renderers tend to fail here, and these are not optional.

- Prototype: a single view handling IME composition, Arabic/Hebrew BiDi reordering, and CJK wide-character column alignment simultaneously.
- This must be validated early; it cannot be deferred.

### Replaceable UI surface

- Prototype: the completion popup replaced by an extension-provided implementation, while the built-in capability providers continue to contribute results.
- Verify: the new surface receives aggregated results from all providers with no special wiring, and the swap can occur without losing provider state.

---

## Scripting

This area has the largest cluster of unknowns and should be broken into sub-experiments.

### Runtime choice

✅ JavaScript on V8 is selected, using `deno_core` behind a Knot-owned wrapper.
The remaining measurements concern isolate startup and memory cost rather than
engine selection.

### Concurrency model

✅ **Target chosen, validation pending:** one isolate per extension, scheduled
over a bounded V8 worker pool. Tokio handles asynchronous host work and message
routing rather than executing CPU-bound JavaScript.

- Prototype: prove parallel callbacks across isolates, safe movement between
  workers, interruption, basic CPU/memory limits, and failure isolation.
- Measure: isolate startup/RSS, scheduling and host-call overhead, and
  noisy-neighbor behavior.

### Core ↔ script FFI ergonomics

- Prototype: the buffer API as seen from the script side. Is it ergonomic, typed, and not a performance cliff?
- Decide: are TypeScript types generated from the Rust core, hand-maintained, or derived from a shared schema?
- Identify any operation whose FFI cost is disproportionate to its frequency (e.g. per-character text access during rendering).

### Packaging

- Prototype: a `knot` package file format, version declaration, dependency resolution, and load order.
- Confirm: loading user config, a package, and a built-in module proceeds through the same code path, as the design claims.

---

## Extension mechanism

### Capability aggregation

- Prototype: two independent completion providers and one diagnostic provider, feeding a single shared UI surface.
- Verify: the core merges, dedupes, and routes results; providers can be added and removed at runtime without restarting the surface.

### UI-surface hot replacement

- Prototype: replacing the active completion surface mid-session, without dropping in-flight provider results and without restarting providers.
- This validates the provider/surface split as actually implemented, not just as described.

### Package discovery and conflict

- Prototype: two packages defining the same command name and the same keybinding.
- Decide: resolution rule (load order, explicit override, user-config precedence), and how a conflict is surfaced to the user without a crash.

### Same-path loading

- Prototype: loading user configuration, a third-party package, and a built-in module through the *same* loader, and confirm no code path distinguishes them.

---

## Cross-cutting: async ergonomics

Pulled out of "Scripting" because it interacts with UI, capabilities, and LSP, and because its answer constrains everything else. Likely the highest-leverage experiment in this list.

- Prototype: a command that appears synchronous to its author but is auto-suspended across await points without callback hell.
- Prototype: cancellation propagating cleanly across an await chain that spans a capability provider, an LSP request, and a UI update.
- Prototype: an extension that hangs. Confirm it can be detected, interrupted, and isolated without taking down the editor.
- The benchmark is Emacs' synchronous model, which is loved precisely because it is simple to write. The async story must be ergonomic enough that users actually adopt it rather than fighting it.

---

## Cross-cutting: command and keymap dispatch

Commands are a stated pillar of the design but are not covered by any of the four areas above.

- Prototype: keymap resolution with transient and active keymaps, programmable
  dispatch (e.g. a prefix-arg or a `M-x`-style invocation), and composition of
  commands.
- Prototype: route a generic command through editor, terminal, and tree views,
  then fall back through window, workspace, and global scopes.
- Confirm: every user action is addressable as a command object, invokable
  programmatically with the same arguments a keypress would supply.
- Confirm: invocation captures the focused view target and optional associated
  text buffer. An asynchronous command must not retarget itself when focus
  changes while it awaits.

---

## Cross-cutting: URI / filesystem provider abstraction

A cheap sanity check that either validates the abstraction or reveals where it leaks.

- Prototype: a remote `ssh://` provider participating in workspace enumeration, search, and an LSP session, identically to the built-in `file://` provider.
- Identify any core path that bypasses the provider interface and hard-codes `file://` semantics; these are leaks to fix.
