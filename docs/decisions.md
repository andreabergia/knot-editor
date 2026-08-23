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

## Commands and keymaps

- Commands are Knot-owned values with a globally unique name and explicit
  JSON-like arguments. `CommandCatalog` owns discovery metadata and native or
  extension name ownership, while native handlers remain on gpui's focus
  dispatch path.
- Native and extension commands share one namespace. Native definitions reserve
  their names; each extension definition belongs to one lifecycle and is
  removed with it. Handlers validate arguments because schemas and generated
  argument UI have not been justified.
- Dispatch captures the window, workspace, weak focus target, optional buffer,
  and monotonic invocation identity. Native target identities never cross the
  extension boundary. Editor, tree, and terminal handlers can therefore share
  semantic commands without a common surface type or fabricated buffers.
- gpui routes native commands from the captured focus target through enclosing
  scopes. An extension handler is a lifecycle-validated global destination
  after native routing, rather than an entry in a Knot-owned view routing
  table.
- Focus-stealing UI preserves an explicit target. The palette invokes at the
  weak origin captured before it took focus, without visibly restoring focus;
  a destroyed origin is an invalid target and is never replaced with current
  focus.
- Keybindings, the palette, and top-level scripts enter the same dispatcher and
  produce the same structured outcomes. Fixed gpui bindings and semantic key
  contexts are sufficient for the prototype's base, surface, active,
  transient, and multi-keystroke cases.
- Delayed mutations revalidate invocation, lifecycle, window/workspace, focus,
  and optional buffer identity. They cannot fall back to the current active
  buffer after the captured target disappears.
- Command-to-command invocation uses ordinary awaited JavaScript and the same
  structured outcomes as top-level invocation. The shell admits roots into one
  FIFO queue and runs one root plus its descendants at a time. Child commands
  inherit the captured target; unrelated roots cannot enter while a parent is
  suspended.
- Native children retain gpui focus routing. Cross-extension children run on
  their owning serial runtime. Same-extension children run as catalog-authorized
  nested handler frames inside the current callback, avoiding self-queue
  deadlock without allowing unrelated callbacks to enter.
- Runtime ancestry rejects cross-extension cycles, and registration ancestry
  rejects recursive same-extension command cycles. One unfinished child per
  parent is the deliberate boundary between composition and a task graph.
- Cancellation propagates from a parent to unfinished descendants, actively
  aborts handler signals, rejects late mutations, and does not release the next
  root until the tree settles. Child cancellation does not propagate upward;
  JavaScript observes the cancelled outcome and decides whether to continue.
- Command handlers still do not return semantic values. A handler's successful
  return completes its invocation; unsuccessful children affect a parent only
  when its JavaScript branches or throws. Direct JavaScript functions remain
  preferable when global command lookup, focus routing, or cross-extension
  reuse is unnecessary.
- Configurable keymap loading, extension-defined bindings, argument schemas,
  aliases, macros, repetition, detached children, concurrent roots, priorities,
  and general task-graph scheduling remain deferred.
- IME, text insertion, pointer motion, scrolling, focus changes, and other raw
  input protocols remain outside the registered command model.

Evidence: [command and keymap experiment](step11-command-keymap-plan.md).

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
- Knot uses `alacritty_terminal` for terminal emulation and renders its grid
  through gpui. Ghostty VT proved viable, but showed no concrete behavioral
  advantage sufficient to offset its separate PTY integration, pre-1.0 FFI,
  Zig packaging, larger integration, and poor debug parsing performance.
- The disposable prototype lets `TerminalView` own its PTY and emulator.
  Production separates a stable `TerminalSession` from disposable
  presentation so a running shell survives moving or reconstructing its view
  across tabs and windows.
- Stable session identity does not imply persistence after closure. Explicitly
  closing a terminal terminates its session; reopening creates a new session.
  Detached persistence and simultaneous presentations are deferred.
- Knot will use Zed's GPL-compatible terminal implementation as an attributed
  source for the production Alacritty adapter, model, key mappings, gpui
  renderer, resize flow, and event handling.

Evidence: [terminal evaluation](step9-terminal-plan.md).

## Generated text surfaces

- Generated content defaults to an ordinary `TextBuffer` presented through the
  normal buffer and editor lifecycle when it remains useful as selectable,
  scrollable, copyable text.
- `OpenBufferCollection` strongly owns user-visible titled buffers and their
  selection. `BufferRegistry` remains a separate weak registry for extension
  transport handles.
- Editability is application policy on `BufferModel`; `TextBuffer` remains
  unconditionally mutable. Every model text-mutation path enforces the policy,
  while view state, snapshots, contributions, and closure remain available for
  read-only models.
- Surface-specific controllers own semantic identity and action metadata.
  Generated text is presentation output, never an identity format: activation
  resolves an emitted output range to a recorded source target without parsing
  displayed text.
- The validated search surface duplicates only result ordering and emitted
  range geometry. It does not duplicate previews, grouping, or source identity
  in a second presentation model.
- Generated search buffers are immutable snapshots. Navigation is rejected
  after the source revision changes; refresh, incremental updates, and restored
  view state remain deferred liabilities.
- A second generated surface was unnecessary for this checkpoint because
  search results did not force special behavior into generic buffer ownership,
  the text model, or the editor pipeline.

Evidence: [text-surface experiment](step10-text-surface-plan.md).

## Capability aggregation

- Completion providers are lifecycle-owned, shell-wide registrations with
  monotonic identity and registration order. An invocation snapshots provider
  membership so later registration changes cannot alter in-flight work.
- Each `EditorView` owns at most one completion controller and presentation.
  Completion state does not belong to `BufferModel`, `core`, or an extension
  runtime.
- Independent extension runtimes execute one-shot providers concurrently.
  Incremental results are accepted only after registration, lifecycle, weak
  editor, generation, buffer identity, and revision validation.
- Completion composition is deliberately feature-specific. ASCII prefix
  filtering, case-sensitive then case-insensitive ranking, registration order,
  provider-local order, and insertion-text deduplication produce results that
  do not depend on arrival timing.
- Recoverable failure is local to one provider and request. Other candidates
  remain visible, and the provider participates again on the next invocation
  without re-registration.
- The controller assigns stable semantic item identities and owns acceptance.
  Native list and compact surfaces receive immutable snapshots and own only
  selection, layout, and rendering. Replacing a surface during an in-flight
  request does not restart providers and deliberately resets presentation
  selection.
- Acceptance resolves the selected semantic identity and performs one
  revision-checked prefix replacement. Rendered labels are never parsed back
  into editor semantics.
- Provider applicability, automatic triggers, streaming, active cancellation
  of superseded work, timeouts, backpressure, fuzzy or configurable ranking,
  richer completion edits, production popup polish, and extension-owned
  presentation remain deferred.

Evidence: [capability aggregation experiment](step12-capability-aggregation-plan.md).
