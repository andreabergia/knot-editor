# D026: Terminal Compatibility, Typography, Mouse, and Links

Status: 🚧 checkpoint 1 implemented; automated verification passed; native review pending.

Promoted from D026 in the [roadmap](../roadmap.md). This slice makes the local
terminal usable with ordinary shells and representative TUIs: Vim, less, and
tmux. Users get consistent typography, mode-aware keyboard and mouse input,
and clickable links. “Full compatibility” means the explicit tested contract
below; it does not promise every terminal protocol or application.

References: [architecture](../architecture.md),
[terminal ownership and lifecycle](../architecture/workbench-and-lifecycle.md#terminal-sessions-and-views),
[command routing](../architecture/commands.md), and
[validated terminal choice](../decisions.md#terminal).

## Agreed design and boundaries

- Keep `alacritty_terminal` and the existing application-owned session, sole
  attached view, PTY worker, and generation-checked lifecycle. Terminal state
  remains outside documents, buffers, and extension protocols.
- Share the editor's font defaults, size, and fallback policy through an
  application-level typography definition. Derive terminal cell dimensions
  and baseline from font metrics. Drawing, selection, link hit testing, mouse
  coordinates, grid dimensions, and PTY pixel sizing use the same geometry;
  fallback glyphs retain the emulator's one- or two-cell allocation.
- Define and test the shell/TUI compatibility contract: ordinary text and
  control input, cursor and keypad application modes, common function and
  modified navigation keys, bracketed paste, alternate-screen interaction,
  requested focus reporting, and emulator replies needed by the selected
  workflows. Audit the adapter first; preserve behavior already handled by
  Alacritty rather than building a second emulator.
- Preserve command/keymap precedence over raw terminal input. Copy and Paste
  continue through shared commands; raw terminal protocols stay outside the
  command registry. Focus reports describe actual terminal focus transitions.
- Honor application-requested mouse tracking and encoding modes. Route button,
  motion, release, and wheel events according to those modes. Shift overrides
  reporting for local selection and scrolling. With no tracking request, use
  local interaction; apply alternate-screen scroll behavior consistently with
  the selected compatibility contract.
- Support explicit OSC 8 links and detected HTTP(S) URLs. Explicit links take
  precedence over detection. Show a hover affordance and open through the
  platform URL opener on Command-click on macOS or Control-click elsewhere.
  A recognized link activation takes precedence over application mouse input;
  Shift retains local interaction. Initial activation supports HTTP(S)
  destinations for both link sources; other schemes remain deferred.
- Link lookup uses the current displayed grid, including scrollback and soft
  wraps. Hover/activation state belongs to the view and must not survive stale
  content, restart, detach, or geometry changes as an actionable old target.
- Retain asynchronous PTY I/O and exact lifecycle ownership. Replies and input
  must target the live session generation; detached views cannot send them.

## Exclusions and deferred choices

No general settings system, advanced keyboard protocols (including Kitty and
modifyOtherKeys), terminal graphics, arbitrary URI handlers, filesystem-path
links, remote PTYs (D038), shell integration (D039), session persistence, or
simultaneous views. Do not broaden clipboard access through OSC 52 as part of
the event audit. Title and bell product features are outside this contract.

Concrete encoding tables, metric rounding, URL boundary rules, and fixture
applications/versions are implementation choices to record at their review
gates. Expand compatibility only for a demonstrated gap in the selected
workflows. macOS is the primary manual acceptance target; preserve portable
behavior and record Windows/Linux evidence separately rather than implying
new platform certification.

## Delivery protocol

Implement one checkpoint at a time and stop at each review gate. Make logical
commits as work lands, mark completed items with ✅, and keep this plan current.
Run focused automated checks and relevant existing regression coverage. Run
`cargo fmt` once at the end of Rust work before committing. Update architecture
references when behavior changes, the system map only when boundaries change,
and `decisions.md` when implementation validates a durable choice.

Manual visual or real-application acceptance complements automated coverage.
The user runs native UI checks; do not use computer-use or UI automation tools.
Programmatic gpui test coverage remains appropriate for event wiring and
lifecycle behavior.

## Checkpoint 1: Consistent terminal typography and geometry

- [x] ✅ Extract shared font defaults/fallbacks without changing editor text
  semantics; use the shared size in the terminal.
- [x] ✅ Replace duplicated fixed 8×16 geometry with one measured terminal layout
  contract, including baseline, wide cells, scale-factor changes, and minimum
  grid bounds. Keep PTY sizing consistent after resize or cross-window transfer.
- [x] ✅ Validate layout and coordinate conversion with automated coverage for
  fractional bounds, scale factors, cell edges, and wide/combining content.
  Add gpui coverage where needed to prove the view uses the measured geometry
  for resize and interaction; retain existing session resize/transfer tests.
- [x] ✅ Update the terminal runtime reference for the new geometry contract.

Review gate: inspect geometry ownership and editor regressions. User checks
ASCII, Arabic, CJK, emoji, combining marks, and box drawing for baseline or
clipping issues, then resizes and transfers a live terminal. Record any font
availability or platform limitations before proceeding.

Checkpoint 1 implementation choices and evidence:

- Shared application defaults preserve editor semantics: Menlo at 14 logical
  pixels with Geeza Pro, PingFang SC, and Apple Color Emoji fallbacks. Editor
  line height remains 20 logical pixels. Missing fonts use gpui's platform font
  resolution; these macOS families do not imply Linux/Windows font availability.
- Terminal cells use the resolved primary font's `M` advance and ascent + descent,
  rounded upward to whole physical pixels, then converted to logical pixels.
  The primary baseline is centered in that height; fallback glyphs share it and
  are clipped to their emulator allocation. Fractional viewport remainders do
  not create extra cells. Emulator minimum grid bounds still apply.
- The view measures on every prepaint, including destination layout after
  transfer. PTY cells use the same integral physical dimensions. The session's
  initial 80×11 grid uses placeholder pixel metrics until its first layout.
- Automated coverage: metric rounding at scales 1, 1.25, 1.5, and 2; fractional
  bounds, cell edges and out-of-bounds coordinates, minimum grids, fallback
  baseline alignment; gpui measured resize and pointer selection of combining
  text beside a wide CJK cell; existing child-PTY resize and terminal transfer,
  attachment, shutdown, restart, and clipboard regressions.
- ✅ Verification on macOS: `cargo test --lib -- --test-threads=1` passed
  all 486 tests. `cargo fmt` ran once after Rust work; `git diff --check` passed.
- ⏳ Native macOS gate remains open: check ASCII, Arabic, CJK, emoji, combining
  marks, and box drawing for baseline/clipping; resize and transfer a live
  terminal. No native Linux/Windows acceptance is claimed. Do not start
  checkpoint 2 until this review passes.

## Checkpoint 2: Shell and TUI input/output compatibility

- [ ] Audit the installed Alacritty adapter's mode and event contracts; record
  the bounded input/reply matrix required by shells, Vim, less, and tmux.
- [ ] Implement mode-aware keyboard encoding and actual focus transitions.
  Route required emulator write/reply events through the live PTY channel
  without losing semantic events in repaint coalescing or applying stale
  generation events after restart.
- [ ] Preserve command precedence, plain/bracketed paste, stopped-session
  behavior, and scroll-to-bottom policy for user input.
- [ ] Test byte-level mode/modifier cases and parser-driven reply behavior.
  Use PTY integration coverage for input/reply delivery and gpui tests for
  command precedence and focus transitions. Cover restart/detach/transfer
  boundaries and regress existing Copy/Paste behavior.
- [ ] Document the supported contract and validated adapter decisions.

Review gate: review the compatibility matrix and lifecycle routing. User runs
a shell, Vim, less, and tmux through navigation, function keys, paste,
alternate-screen entry/exit, and focus changes; record application versions
and reproducible gaps. Resolve contract failures before mouse work.

## Checkpoint 3: Application mouse reporting and local override

- [ ] Add mode-aware button, release, motion, and wheel routing, including
  required legacy and SGR encoding and coordinate limits. Keep viewport
  reporting coordinates distinct from scrollback selection coordinates.
- [ ] Implement Shift local override, local interaction without tracking, and
  alternate-screen scrolling. Reset transient pressed/drag state on lifecycle
  and mode transitions so stale gestures cannot affect a new attachment.
- [ ] Test encoding and routing across tracking modes, modifiers, cell edges,
  scrollback, wide cells, and wheel accumulation. Add gpui event tests for
  press/drag/release ownership and override behavior, including detach/restart.
- [ ] Update the terminal interaction reference.

Review gate: user exercises a mouse-enabled Vim/tmux session and a deterministic
mouse-report fixture. Confirm clicks, drags, wheel events, Shift selection and
copy, and local scrolling; review that each event has one intended recipient.

## Checkpoint 4: Hyperlinks and complete slice acceptance

- [ ] Carry OSC 8 link identity into the displayed snapshot and detect HTTP(S)
  URLs across soft wraps without merging separate hard lines. Keep explicit
  destinations authoritative and define punctuation boundaries.
- [ ] Add hover feedback and modifier-click activation through the native URL
  opener. Revalidate against current grid and geometry; clear transient state
  across output changes, scroll, resize, detach, transfer, and restart.
- [ ] Test explicit links, detected URLs, precedence, wrapped/Unicode content,
  scheme filtering, stale targets, and pointer-policy conflicts. Inject or
  capture the opener in tests so event wiring is verified without launching
  external applications.
- [ ] Run the complete terminal/lifecycle and affected editor/command regression
  coverage. Record the compatibility matrix and acceptance evidence in this
  plan; document remaining gaps explicitly.
- [ ] Update architecture references and validated decisions. Preserve any
  remaining deferred scope in the roadmap under stable IDs.

Review gate: user checks links in live output and scrollback, including
mouse-enabled applications, then repeats the representative shell/TUI flows
through resize, tab switching, cross-window transfer, restart, and protected
closure. Complete D026 only when the agreed contract passes and remaining
limitations are recorded.
