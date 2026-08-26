# Rust Prototype Roadmap

Knot's prototype validates the riskiest design commitments before production
development. Completed work is summarized here; durable rationale lives in
`decisions.md`, current boundaries in `architecture.md`, and measurements in
the focused evidence reports.

## Completed validations

| Step | Question | Result |
| ---: | --- | --- |
| 1 ✅ | Can the project support a native event loop with the intended module layering? | Yes; macOS prototype skeleton established. |
| 2 ✅ | Which rendering primitives meet the text workload? | Skia cleared the primitive bar; framework selection superseded a standalone renderer. |
| 3 ✅ | Can a public UI framework support Knot's editor and shell? | gpui selected; demanding editor behavior uses public APIs. |
| 3b ✅ | Does the framework path survive Windows? | Yes; platform and UTF boundary fixes used public APIs. Linux remains deferred. |
| 4 ✅ | Which editable-buffer representation fits stable annotations? | Stable-ID piece table with UTF-8 byte offsets and position tokens. |
| 5 ✅ | Can annotations track edits cheaply and correctly? | Token-anchored ranges; affected repair is O(touched), untouched ranges need no work. |
| 6 ✅ | Do independent annotation sources compose? | Yes; core owns geometry, application code owns source and presentation data. |
| 6b ✅ | Can the model support reversible transactions? | Yes; ordinary buffer edits implement one undoable/redoable transaction. |
| 7 ✅ | What scripting boundary should Knot use? | JavaScript on V8 behind a Knot-owned typed host protocol. |
| 8 ✅ | What UI model should extensions receive? | Surface-specific semantic providers and contributions; no general widget protocol. |

Evidence:

- `step2-renderer-benchmark.md`
- `step3-framework-comparison.md`
- `step4-buffer-benchmark.md`
- `step5-annotation-benchmark.md`
- `step7-v8-runtime.md`

## 9. Native terminal view experiment

Status: complete. Alacritty was selected with Knot/gpui rendering; Ghostty VT
was viable but did not justify its additional integration and build burden.

Execution plan: [step9-terminal-plan.md](step9-terminal-plan.md).

Validate terminal support without inventing non-text buffer semantics.

- Use a mature parser/state crate.
- Implement a gpui `TerminalView` owning one local PTY subprocess and emulator
  session.
- Exercise keyboard input, asynchronous output, ANSI color, cursor state,
  scrollback, and resize propagation.
- Run an interactive shell and one alternate-screen full-screen TUI.
- Close and restart the subprocess cleanly.
- Confirm terminal output cannot block the UI event loop.

The prototype uses one authoritative view and grid per session. Multiple views,
headless lifetime, persistence, exhaustive compatibility, mouse reporting,
hyperlinks, shell integration, remote PTYs, and cross-platform polish are out
of scope.

Decision: view-owned PTY lifecycle, emulator state, and native rendering fit
Knot's view and async architecture. Production will separate a stable session
from its disposable presentation so a running shell can move across tabs and
windows without adding persistence after explicit closure.

## 10. Text as a primary surface ✅

Execution plan: [step10-text-surface-plan.md](step10-text-surface-plan.md).

- Implement search results as an inspectable, read-only `BufferModel` backed
  by a normal `TextBuffer`.
- Use the normal editor-view pipeline and attach commands to result regions.
- Retain multiple result buffers and revisit them through the same buffer list
  as the source buffer.
- Try a second generated surface only if search results do not expose useful
  limits.

Decision: generated content remains an ordinary buffer while the text is useful
on its own. Application-owned controllers retain semantic identity and map
emitted ranges to actions without parsing the display. The approach becomes a
liability when a surface needs a second authoritative presentation model or
special behavior in generic buffer ownership or editing; search results needed
neither. Refresh and stale-target recovery remain deferred. A second
generated surface was deliberately skipped because search exposed no useful
boundary requiring it.

## 11. Command and keymap dispatch ✅

Status: complete. Captured focus routing covered editor, tree, and terminal
surfaces without a shared surface model or fabricated buffers.

Execution plan: [step11-command-keymap-plan.md](step11-command-keymap-plan.md).

- Make commands first-class values with names, arguments, and programmatic
  invocation.
- Capture focused target, optional buffer, window/workspace, and invocation
  identity at dispatch time.
- Route native commands through the focus hierarchy.
- Revalidate captured targets after asynchronous waits.
- Exercise transient and active keymap contexts and multi-keystroke invocation
  through gpui's existing keymap machinery.
- Exercise one operation from keybinding, command palette, and script with the
  same explicit arguments.

Native view identities remain opaque to extensions. IME and other raw input
protocols do not become registered commands.

Decision: Knot-owned command values enter one target-preserving dispatcher from
keybindings, the palette, and top-level scripts. gpui routes native handlers
from the captured focus target, while lifecycle-owned extension handlers remain
a global destination and receive only semantic opaque context. Captured targets
are revalidated before delayed mutation, so destroyed origins are rejected
rather than silently retargeted. Configurable keymaps and nested or concurrent
command execution remain deferred.

## 11b. Async command invocation ✅

Status: complete. Awaited command reuse is viable with one serialized root
queue, inherited child targets, runtime-ancestry cycle rejection, and
downward-only cancellation.

Exploration plan:
[step11b-async-command-plan.md](step11b-async-command-plan.md).

- Explore command-to-command invocation separately from focus routing.
- Use ordinary JavaScript as the composition mechanism; do not add a command
  DSL, pipelines, repetition, or decorators.
- Determine whether native, cross-extension, and same-extension invocations can
  be awaited without violating serial extension callbacks.
- Define result, cancellation, context inheritance, and concurrency semantics
  only where concrete composition fixtures require them.

Decision: JavaScript commands can safely reuse native, cross-extension, and
same-extension commands without a second composition language or a general
task graph. Same-extension reuse is an inline nested handler frame; direct
functions remain the simpler choice when command lookup and routing semantics
are unnecessary.

## 12. Capability aggregation ✅

Status: complete. Two independent extension runtimes feed one view-owned
completion controller incrementally while native presentation is replaced.

Execution plan:
[step12-capability-aggregation-plan.md](step12-capability-aggregation-plan.md).

- Feed one completion surface from two providers with different response times.
- Tag requests and results with revisions; discard stale late results.
- Replace the surface during the session without restarting providers.
- Preserve one provider's results when another fails.
- Use a deliberately completion-specific merge policy.

Decision: providers, completion-specific composition, and replaceable native
presentation remain independent. Shell registration order gives deterministic
composition; view generation, buffer revision, registration, lifecycle, and
weak editor identity reject stale work. Provider failure remains isolated, and
surface replacement transfers only the semantic snapshot without restarting
provider requests. Applicability policy and extension-owned presentation remain
deferred.

## 13. URI and filesystem providers ✅

Status: complete. One URI-based asynchronous provider boundary drives the same
workspace tree and resource-backed open/save flow for an in-memory hierarchy
and a trusted local folder.

Execution plan: [step13-uri-filesystem-plan.md](step13-uri-filesystem-plan.md).

- Implement an in-memory provider rooted at a non-`file://` URI.
- Implement a local-folder provider rooted at a `file://` URI.
- Support normalization, enumeration, read, write, and stat.
- Open workspaces and buffers through both providers.
- Find and remove or document paths that assume local files.

Decision: the provider abstraction generalizes without leaking `file://` or
platform paths into generic application state. Scheme-specific normalization
and local path conversion remain provider-owned; workspace and request
generations reject stale asynchronous results; resource metadata stays on open
entries rather than text models. Remaining liabilities include symlink escape
from trusted local roots, non-atomic replacement writes, no external-change
tracking or conflict handling, UTF-8-only opens, and no URI deduplication.

## Prototype exclusions

- Package format, dependency resolution, and load order.
- Production extension scheduling, quotas, and backpressure policy.
- Persistent snapshots and production undo/history.
- Piece-table compaction and long-session reclamation.
- Session persistence and binary buffers.
- Production terminal compatibility beyond step 9.
- Standardized AI integration.
