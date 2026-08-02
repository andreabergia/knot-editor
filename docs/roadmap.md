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

## 10. Text as a primary surface

Execution plan: [step10-text-surface-plan.md](step10-text-surface-plan.md).

- Implement search results as an inspectable, read-only `BufferModel` backed
  by a normal `TextBuffer`.
- Use the normal editor-view pipeline and attach commands to result regions.
- Retain multiple result buffers and revisit them through the same buffer list
  as the source buffer.
- Try a second generated surface only if search results do not expose useful
  limits.

Decision checkpoint: where does representing generated surfaces as text become
a liability?

## 11. Command and keymap dispatch

- Make commands first-class values with names, arguments, and programmatic
  invocation.
- Capture focused target, optional buffer, window/workspace, and invocation
  identity at dispatch time.
- Route native commands through the focus hierarchy.
- Revalidate captured targets after asynchronous waits.
- Add transient and active keymaps, prefix-style invocation, and command
  composition.
- Exercise one operation from keybinding, command palette, and script with the
  same explicit arguments.

Native view identities remain opaque to extensions. IME and other raw input
protocols do not become registered commands.

Decision checkpoint: does context-targeted focus routing cover text and
non-text surfaces without fabricating buffers?

## 12. Capability aggregation

- Feed one completion surface from two providers with different response times.
- Tag requests and results with revisions; discard stale late results.
- Replace the surface during the session without restarting providers.
- Preserve one provider's results when another fails.
- Use a deliberately completion-specific merge policy.

Decision checkpoint: can providers and replaceable presentation remain
independent in a real asynchronous feature?

## 13. URI and filesystem providers

- Implement an in-memory provider rooted at a non-`file://` URI.
- Support normalization, enumeration, read, write, and stat.
- Open a workspace and buffer through it.
- Find and remove or document paths that assume local files.

Decision checkpoint: does the provider abstraction generalize without leaking
`file://` semantics?

## Prototype exclusions

- Package format, dependency resolution, and load order.
- Production extension scheduling, quotas, and backpressure policy.
- Persistent snapshots and production undo/history.
- Piece-table compaction and long-session reclamation.
- Session persistence and binary buffers.
- Production terminal compatibility beyond step 9.
- Standardized AI integration.
