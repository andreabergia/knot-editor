# Step 9: Native Terminal View Plan

This experiment decides whether a view-owned terminal session fits Knot's gpui
and asynchronous runtime architecture, and which mature terminal implementation
Knot should build on.

## Validated constraints

- ✅ A terminal is an opaque native surface, not a `TextBuffer` or another
  editor model.
- ✅ One `TerminalView` owns one authoritative PTY session and grid.
- ✅ Terminal internals are not exposed through the extension host protocol.
  Extensions cannot enumerate terminal sessions, read their contents, send
  input, or inspect their processes or environments.
- ✅ Native, focus-routed commands may operate on a terminal without exposing
  its contents or session identity to extensions.
- ✅ Fonts, colors, and other user-facing terminal presentation settings are
  configured through Knot.
- ✅ Terminal text should use the same resolved fonts and render identically to
  text elsewhere in Knot. The experiment must test actual output rather than
  infer parity from equivalent font settings.

## Candidates

Only these terminal implementations are in scope:

1. `alacritty_terminal` with Knot/gpui rendering. This is the conservative
   baseline: Rust-native, versioned, cross-platform, and already proven with
   gpui in Zed.
2. `libghostty-vt` with Knot/gpui rendering. This tests Ghostty's emulator,
   Unicode behavior, input encoding, scrollback, and render-state API while
   retaining Knot's rendering pipeline.
3. Full `libghostty` rendering as a bounded comparison. It remains viable only
   if it composes cleanly with gpui and meets the font-parity requirement.

Ghostty's renderer is neither assumed nor excluded. Its Metal/OpenGL lifecycle,
clipping, scaling, input/IME ownership, and future Windows path are part of the
comparison.

## Implementation boundary

Keep the prototype seam narrow enough to replace one candidate without
inventing a production terminal abstraction:

```text
gpui workspace / focus / layout
              |
        TerminalView
              |
   session lifecycle and input
              |
       PTY + emulator grid
```

The view owns focus, dimensions, presentation settings, and restart/close
actions. Blocking PTY reads and writes stay off gpui's foreground thread.
Updates reaching the foreground are bounded or coalesced so sustained terminal
output cannot starve UI input or painting.

## Execution

### 1. Alacritty baseline

- ⬜ Add a `TerminalView` using `alacritty_terminal`.
- ⬜ Start one local interactive shell and route keyboard input to its PTY.
- ⬜ Render ANSI colors, styled cells, cursor state, and scrollback through
  gpui.
- ⬜ Propagate grid resize to the emulator and PTY.
- ⬜ Close, reap, and restart the subprocess cleanly.
- ⬜ Run one alternate-screen full-screen TUI.
- ⬜ Flood the PTY with output while verifying that the UI remains responsive.

### 2. Rendering-parity fixture

- ⬜ Centralize the Knot settings used by editor and terminal rendering: font
  family, resolved face, size, weight, variable axes, font features, fallback,
  line height, and palette.
- ⬜ Render a side-by-side editor/terminal fixture containing ASCII, ligatures,
  Nerd Font symbols, combining marks, emoji, CJK, bold, and italic text.
- ⬜ Compare glyph appearance, fallback, baseline, advance, weight, and
  rasterization at multiple scale factors.

### 3. Ghostty VT spike

- ⬜ Integrate `libghostty-vt` through its C API and record the Zig/build and
  unsafe-FFI cost.
- ⬜ Exercise the same shell, alternate-screen TUI, resize, scrollback, cursor,
  color, input, restart, and output-flood cases.
- ⬜ Render its grid through the same gpui path and run the same parity fixture.
- ⬜ Compare compatibility, Unicode behavior, runtime ownership, API stability,
  integration size, and maintenance burden against the Alacritty baseline.

### 4. Full Ghostty renderer comparison

- ⬜ Verify that Ghostty can render into a gpui-managed view region without
  owning Knot's window or input dispatch.
- ⬜ Test clipping, resize, scale-factor changes, occlusion, focus, and IME
  interaction.
- ⬜ Drive all presentation settings from Knot and run the same parity fixture.
- ⬜ Reject this path if it cannot provide identical font output or introduces
  a second window/rendering lifecycle that does not compose cleanly with gpui.

### 5. Decision and cleanup

- ⬜ Select Alacritty, Ghostty VT, or full Ghostty using the evidence above.
- ⬜ Remove the rejected candidate code rather than retain a permanent
  multi-backend abstraction.
- ⬜ Record the validated choice and rationale in `decisions.md`.
- ⬜ Update `architecture.md` with the resulting ownership, dependency
  direction, and PTY-to-render runtime flow.
- ⬜ Mark roadmap step 9 complete and summarize the result.

## Out of scope

Multiple views of one session, detached or persistent sessions, terminal APIs
for extensions, exhaustive escape-sequence compatibility, mouse reporting,
hyperlinks, shell integration, remote PTYs, and production cross-platform
polish remain deferred.

## Decision checkpoint

Choose the smallest integration that:

- keeps PTY work from blocking the UI;
- preserves view-owned lifecycle and one authoritative grid;
- renders fonts identically with Knot-controlled settings;
- provides sufficient terminal compatibility;
- does not expose terminal authority or contents to extensions; and
- has an acceptable build, FFI, portability, and maintenance cost.
