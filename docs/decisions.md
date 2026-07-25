# Prototype Decisions

Durable decisions validated by the Knot prototype. This is the default source
for why the current architecture exists. Benchmark reports contain supporting
measurements; Git history contains execution plans and investigation history.

## Platform and UI

- The prototype targets macOS. Windows has passed the framework portability
  checkpoint. Linux remains required before production development.
- gpui 0.2.2 is the UI framework. Its public API supports the editor, shell,
  input, IME, selection, and annotation paths without privileged Zed APIs.
- gpui's macOS text path renders the fixture corpus correctly. Its public
  bidirectional-text ergonomics are incomplete: Knot currently supplies RTL
  alignment and logical-index mapping over public glyph data.
- Skia was the strongest standalone rendering primitive, but Knot uses gpui's
  rendering path rather than maintaining a separate renderer.

Evidence: [renderer benchmark](step2-renderer-benchmark.md) and
[framework evaluation](step3-framework-comparison.md).

## Text buffer and stable positions

- `core::TextBuffer` is a hand-built stable-ID piece table. The prototype needs
  direct control over edit events and stable positions more than a
  general-purpose text-storage abstraction.
- Text offsets and ranges are half-open UTF-8 byte ranges. Presentation layers
  handle grapheme-aware behavior. Protocol field names identify byte offsets
  explicitly.
- A `Position` identifies a piece and offset. Splits preserve the left piece ID
  and allocate a right piece ID; edit-log records repair affected stale
  positions. Unrelated positions require no work.
- The core is single-threaded. Its foreground owner serializes mutations;
  asynchronous producers use revisions and typed messages.
- The line-start index is intentionally replaceable. Its linear suffix update
  dominates large-buffer edit cost, but the prototype has not demonstrated a
  need to replace it.
- Inserted backing storage and the edit log are append-only during the
  prototype. Compaction and reclamation are deferred.

Evidence: [buffer benchmark](step4-buffer-benchmark.md).

## Anchored ranges and contributions

- `AnchoredRangeStore` owns stable range geometry only. Each endpoint combines
  a `Position` with before/after boundary stickiness.
- Stabilization consumes one authoritative buffer edit stream. It repairs only
  affected endpoints; untouched ranges retain stable tokens.
- Query indexes are derived caches, never the source of truth.
- Feature, source, lifecycle, and presentation metadata live in `app`, keyed by
  core range IDs. Overlapping sources require no privileged coordination in
  `core`.
- A fully consumed anchored range is removed. Undoing text does not revive it;
  provider-derived state may be republished under a new ID.
- Contributions are revision-checked, source-owned replacement sets. Replacing,
  disposing, closing, or terminating a source removes its complete set.
- Built-ins and extensions use the same application-owned contribution
  registry.

Evidence: [annotation benchmark](step5-annotation-benchmark.md).

## Reversible edits

- `EditTransaction` proves one reversible group of primitive edits; it is not
  an undo-history manager.
- Undo and redo use ordinary `TextBuffer` mutations in reverse and forward
  order. They preserve the normal edit-log and anchored-range paths rather than
  restoring internal piece identities.
- Transactions bind to one buffer instance and edit sequence. Out-of-band
  mutation and invalid state transitions are programmer errors.
- History trees, grouping, persistence, and view-state restoration remain
  deferred.

## Extension runtime

- Extensions use JavaScript on V8. `deno_core` is prototype machinery contained
  by `host`; Knot-owned types and protocols form the architectural boundary.
- Each prototype extension owns a persistent isolate and OS thread because
  `deno_core::JsRuntime` is thread-affine. The production target remains a
  bounded pool of movable isolates built on `rusty_v8`.
- gpui's foreground thread exclusively owns editor state. A shared Tokio
  runtime performs asynchronous native work; typed request/response messages
  cross the boundary.
- Callbacks are serial within an extension and may run in parallel across
  extensions. Forced interruption is fatal to one extension and cleans up its
  work without restarting it.
- Commands capture an invocation identity. Cancellation and lifecycle checks
  reject late mutations.
- Buffer edits are revision-checked batches. Change events preserve order per
  extension and carry committed edits rather than fresh snapshots.
- Knot remains UTF-8-native. A buffer retains at most one immutable UTF-16
  snapshot allocation keyed by revision and range for sharing with V8 external
  strings. Current evidence does not justify a UTF-16 core or public byte-array
  text API.
- Slow-consumer policy, quota policy, packaging, module resolution, and
  production isolate scheduling remain open.

Evidence: [V8 runtime evidence](step7-v8-runtime.md).

## Views and extension UI

- Text and contributions are shared buffer state. Cursor, selection, scroll,
  focus, folding state, and rendering choices are per-view state.
- Extension UI APIs are native, surface-specific, and semantic. gpui objects,
  drawing callbacks, layout protocols, V8 values, and Rust objects do not cross
  the public boundary.
- Editor extensions publish anchored decorations, gutter markers, and
  command-backed actions.
- Workbench tree providers are asynchronous. Native views own cached data,
  layout, interaction, loading, and errors; rendering and input never call
  JavaScript synchronously.
- Provider generations and extension lifecycles reject stale responses.
- Knot will not build a general declarative widget tree. A WebView remains a
  possible escape hatch for genuinely arbitrary UI.
- Semantic tree state preserves the information needed for accessibility, but
  gpui 0.2.2 lacks the required public platform bridge.

## Terminal

- A terminal is a native, single-view surface, not a `TextBuffer` subtype.
- `TerminalView` initially owns its PTY and emulator session. Independent
  session ownership should be extracted only when persistence without a view
  or another concrete requirement appears.

