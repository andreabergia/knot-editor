# Step 9: Native Terminal View Plan

This experiment decides whether a view-owned terminal session fits Knot's gpui
and asynchronous runtime architecture, and which mature terminal implementation
Knot should build on.

## Validated constraints

- ✅ A terminal is an opaque native surface, not a `TextBuffer` or another
  editor model.
- ✅ During candidate validation, one `TerminalView` owns one authoritative PTY
  session and grid.
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
3. Full `libghostty` rendering remains a possible follow-up only if
   `libghostty-vt` is compelling and using Ghostty's renderer offers a concrete
   advantage over gpui rendering.

The working Alacritty implementation is the frozen baseline while Ghostty
viability is tested. Do not deepen its rendering, settings, or interaction
implementation until the Ghostty spike determines which comparison work is
useful.

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

This view-owned session is prototype scaffolding, not the production ownership
model. After selecting the backend, rebuild the terminal around Zed's model:

```text
stable TerminalSession entity
  - PTY, emulator, and process lifecycle
  - protocol input and event ingestion
  - immutable render snapshot
              |
    disposable TerminalView
  - focus and presentation settings
  - pixel layout and interaction
  - gpui rendering
```

The production session is the authoritative terminal identity and may outlive
view reconstruction. The view presents one session without owning its process
or emulator. Preserve one presentation per session initially; independent
multi-view presentation remains out of scope.

## Execution

### 1. Freeze the Alacritty baseline

- ✅ Add a `TerminalView` using `alacritty_terminal`.
- ✅ Start one local interactive shell and route keyboard input to its PTY.
- ✅ Render ANSI colors, styled cells, cursor state, and scrollback through
  gpui.
- ✅ Propagate grid resize to the emulator and PTY.
- ✅ Close, reap, and restart the subprocess cleanly.
- ✅ Make the terminal height draggable so row resizing can be validated with
  a full-screen application.
- ✅ Run one alternate-screen full-screen TUI.
- ✅ Flood the PTY with output while verifying that the UI remains responsive.
- ✅ Preserve this implementation as the behavioral baseline without adding
  further terminal detail during the Ghostty viability spike.

### 2. Ghostty kill gate: build, replay, and render

- ✅ Pin `libghostty-vt` through the `libghostty-vt` 0.2.1 Rust wrapper,
  whose sys crate builds Ghostty commit
  `a887df42c56f6de86c0fe6da9c4eeca37931e083` through its C API.
- ✅ Record the Zig/toolchain, linking, packaging, unsafe-FFI, API-stability,
  and incremental-build costs.
- ✅ Keep the first spike independent of live PTY and session management: feed
  representative recorded terminal byte streams into Ghostty.
- ✅ Inspect each replay fixture through Ghostty's safe `RenderState` API,
  including visible graphemes, style/color presence, and cursor position.
- ✅ Draw the inspected visible rows as an unstyled static gpui grid in the
  spike's `--gui` mode.
- ✅ Preserve cell positions in a Knot-owned snapshot and draw Ghostty-resolved
  foreground, background, and inverse colors through gpui.
- ✅ Draw the replay fixture's bold, italic, underline, and strikethrough cell
  attributes through gpui.
- ✅ Draw Ghostty's visible cursor position, shape, and resolved color through
  gpui.
- ✅ Read Ghostty's render state and draw a minimal grid through gpui, including
  styled cells, colors, cursor state, Unicode graphemes, and scrollback.
- ⬜ Stop and reject Ghostty if the build or FFI burden is disproportionate, or
  if its render-state API does not support Knot-owned gpui rendering cleanly.

This gate answers whether Ghostty can fit Knot's dependency and rendering
boundaries. It does not attempt to prove PTY lifecycle, input completeness,
font parity, or production terminal behavior.

The `ghostty-spike` harness replays deterministic styled Unicode, cursor and
scrollback, and alternate-screen byte streams in small chunks into separate
8-row Ghostty terminals. It inspects their visible cells and cursor through
`RenderState`, and `--gui` draws the visible rows as a static gpui grid with
resolved cell colors and the fixture's font attributes. It has no PTY, shell
process, or session lifecycle.

#### Build integration evidence

Measured on Apple Silicon macOS with Rust 1.97.1 and Zig 0.15.2:

- `libghostty-vt` 0.2.1 contains the unsafe C boundary in its
  `libghostty-vt-sys` dependency; Knot can use safe `Terminal` and
  `RenderState` wrappers. Handles are deliberately `!Send + !Sync`.
- The sys crate fetches its pinned Ghostty source during the Cargo build and
  builds both static and dynamic artifacts. Knot links the static archive by
  default; dynamic and `pkg-config` modes are optional.
- Network-free packaging requires prefetching the Ghostty source and Zig
  packages, then setting `GHOSTTY_SOURCE_DIR` and
  `GHOSTTY_ZIG_SYSTEM_DIR`.
- The C API and both Rust crates are pre-1.0. The wrapper couples checked-in
  bindings to its Ghostty pin, containing API churn at the dependency upgrade
  boundary.
- The first native debug build took about one minute. A repeated no-change
  `cargo check` took 3.1 seconds. Generated source, libraries, and Zig cache
  occupied 327 MB under `target`.

This cost is material but not yet disproportionate for the prototype. Revisit
it after the render-state spike establishes whether the API earns the build
burden.

### 3. Ghostty kill gate: minimal live terminal

Proceed only if the build-and-render gate succeeds.

- ⬜ Add the smallest disposable PTY/session path needed for a local interactive
  shell; do not introduce a production backend abstraction.
- ⬜ Route shell output into Ghostty and encode keyboard input through its API.
- ⬜ Exercise resize, scrollback, cursor state, colors, restart, one
  alternate-screen TUI, and sustained output.
- ⬜ Confirm PTY work remains off the gpui foreground thread and foreground
  updates remain bounded or coalesced.
- ⬜ Compare compatibility, Unicode behavior, runtime ownership, integration
  size, and maintenance burden against the frozen Alacritty baseline.
- ⬜ Stop and reject Ghostty if reaching behavioral parity requires production
  infrastructure or materially more integration machinery than Alacritty.

### 4. Candidate-focused rendering parity

Proceed after the Ghostty kill gates establish which candidates remain viable.

- ⬜ Centralize the Knot settings required by the remaining candidates: font
  family, resolved face, size, weight, variable axes, font features, fallback,
  line height, and palette.
- ⬜ Render a side-by-side editor/terminal fixture containing ASCII, ligatures,
  Nerd Font symbols, combining marks, emoji, CJK, bold, and italic text.
- ⬜ Compare glyph appearance, fallback, baseline, advance, weight, and
  rasterization at multiple scale factors.
- ⬜ Refine only the implementations needed to make the candidate comparison
  fair.

### 5. Optional full Ghostty renderer comparison

Run this only if the VT spike is compelling and evidence suggests Ghostty's
renderer could materially improve correctness, performance, or maintenance.

- ⬜ Verify that Ghostty can render into a gpui-managed view region without
  owning Knot's window or input dispatch.
- ⬜ Test clipping, resize, scale-factor changes, occlusion, focus, and IME
  interaction.
- ⬜ Drive all presentation settings from Knot and run the same parity fixture.
- ⬜ Reject this path if it cannot provide identical font output or introduces
  a second window/rendering lifecycle that does not compose cleanly with gpui.

### 6. Decision and cleanup

- ⬜ Select Alacritty, Ghostty VT, or full Ghostty using the evidence above.
- ✅ Treat the compact terminal frontend as disposable prototype code; its
  current scope and organization are sufficient for candidate validation.
- ⬜ For the production implementation, use Zed's GPL-compatible terminal code
  as an attributed source for the Alacritty adapter, terminal model, mode-aware
  [key mappings](https://github.com/zed-industries/zed/tree/main/crates/terminal/src/mappings),
  gpui renderer, resize flow, and event handling rather than independently
  rebuilding those mature paths. Replace the prototype boundary with the stable
  session/disposable view ownership model above rather than incrementally
  growing `TerminalView`.
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
