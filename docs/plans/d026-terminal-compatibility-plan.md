# D026: Terminal Compatibility, Typography, Mouse, and Links

Status: 🚧 checkpoints 1–3 accepted; checkpoint 4 implemented, final native review pending.

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
  control input, cursor application mode (physical keypad deferred below), common function and
  modified navigation keys, bracketed paste, alternate-screen interaction,
  requested focus reporting, and emulator replies needed by the selected
  workflows. Audit the adapter first; preserve behavior already handled by
  Alacritty rather than building a second emulator.
- Preserve command/keymap precedence over raw terminal input. Copy and Paste
  continue through shared commands; raw terminal protocols stay outside the
  command registry. Focus reports describe actual terminal focus transitions.
- Honor application-requested mouse tracking and encoding modes when the
  per-terminal mouse-reporting preference is enabled (the default). Route button,
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

No physical application-keypad input ([D040](../roadmap.md)), general settings
system (D045), advanced keyboard protocols (including Kitty and
modifyOtherKeys) and terminal graphics (D041), arbitrary URI handlers and
filesystem-path links (D042), remote PTYs (D038), shell integration (D039),
session persistence (D043), or simultaneous views (D044). Do not broaden clipboard access through OSC 52 as part of
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
- Terminal cells use the resolved primary font's `M` advance and ascent + absolute descent,
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
  all 487 tests after the signed-descent correction. `cargo fmt` ran once after Rust work; `git diff --check` passed.
- 🛠️ Native review reported vertically clipped shell output. gpui's raw font
  metrics expose signed descent, whereas shaped lines use a positive descent
  distance. Normalize the raw descent before measuring height and baseline;
  regression coverage verifies full glyph height and baseline containment for
  negative descent at every tested scale. Native Core Graphics inspection of
  Menlo confirmed ascent 1901 and descent −483 at 2048 units/em, giving a
  16.296875 logical-pixel height at 14px.
- ✅ User accepted the corrected native rendering on 2026-10-11 (“ok seems
  good now”). Checkpoint 1 review gate passed. Original acceptance checklist:
  ASCII, Arabic, CJK, emoji, combining
  marks, and box drawing for baseline/clipping; resize and transfer a live
  terminal. No native Linux/Windows acceptance is claimed.

## Checkpoint 2: Shell and TUI input/output compatibility

- [x] ✅ Audit the installed Alacritty adapter's mode and event contracts; record
  the bounded input/reply matrix required by shells, Vim, less, and tmux.
- [x] ✅ Implement mode-aware keyboard encoding and actual focus transitions.
  Route required emulator write/reply events through the live PTY channel
  without losing semantic events in repaint coalescing or applying stale
  generation events after restart.
- [x] ✅ Preserve command precedence, plain/bracketed paste, stopped-session
  behavior, and scroll-to-bottom policy for user input.
- [x] ✅ Test byte-level mode/modifier cases and parser-driven reply behavior.
  Use PTY integration coverage for input/reply delivery and gpui tests for
  command precedence and focus transitions. Cover restart/detach/transfer
  boundaries and regress existing Copy/Paste behavior.
- [x] ✅ Document the supported contract and validated adapter decisions.

Review gate: review the compatibility matrix and lifecycle routing. User runs
a shell, Vim, less, and tmux through navigation, function keys, paste,
alternate-screen entry/exit, and focus changes; record application versions
and reproducible gaps. Resolve contract failures before mouse work.

Checkpoint 2 implementation choices and evidence:

- Audited installed `alacritty_terminal` 0.26.0 and gpui 0.2.2 from dependency
  source. The emulator owns parsing, cursor/keypad/paste/focus modes, primary
  and alternate grids, device/status/mode replies, and color/size query formatters.
  Knot owns keyboard bytes and generation-checked semantic delivery. Repaint
  notifications coalesce independently; semantic events remain ordered.
- The user explicitly deferred physical application-keypad support rather than
  vendor gpui (2026-10-11). macOS merges keypad Enter/digits into ordinary keys;
  Linux strips keypad prefixes, and Windows similarly loses physical identity.
  Alacritty still tracks APP_KEYPAD; ordinary numeric input is preserved. D040
  records the remaining requirement. No framework fork was introduced.

| Input / event | Supported bytes or behavior |
| --- | --- |
| Text / Space | UTF-8 text; physical Space sends `20` even without text metadata |
| Enter / Tab / Escape / Backspace | CR / HT / ESC / DEL; Control-Backspace sends BS |
| Control letters and punctuation | C0 mappings, including Space/@/2 → NUL and bracket, slash, caret, underscore aliases |
| Arrows / Home / End | CSI A/B/C/D/H/F normally; SS3 equivalents with APP_CURSOR |
| F1–F4 | SS3 P/Q/R/S; modified keys use CSI 1;m P/Q/R/S |
| Insert / Delete / Page Up / Page Down | CSI 2/3/5/6~; modifiers add `;m` |
| F5–F12 | CSI 15/17/18/19/20/21/23/24~; modifiers add `;m` |
| Modified cursor / Home / End | CSI 1;m final, in either cursor mode |
| Modifier parameter | m = 1 + Shift(1) + Alt(2) + Control(4) |
| Shift-Tab | CSI Z; additional modifiers use CSI 1;m Z |
| Alt text/control | ESC prefix; ASCII key identity preserves Meta shortcuts instead of Option-composed text |
| Platform shortcuts | Product command/keymap precedence; unmatched Command/Super input produces no terminal bytes |
| Paste | Shared command; plain CR normalization or emulator-requested bracketed framing with embedded ESC removed |
| Focus | CSI I/O for actual terminal focus + window activation transitions, only with FOCUS_IN_OUT |
| Device / status / cursor / mode queries | Emulator PtyWrite replies, unchanged, through the live generation |
| Color queries | Emulator formatter with drawing palette and current emulator overrides; no clipboard access |
| Size queries | Emulator formatter with authoritative rows/columns and physical cell dimensions |
| Alternate screen | Emulator entry/exit and grid restoration; mouse/alternate scrolling remain checkpoint 3 |
| Physical keypad / advanced keyboard / OSC 52 | Physical keypad deferred by user; Kitty disabled; no new clipboard access |

- Automated coverage includes every Shift/Alt/Control combination for supported
  navigation/function keys; parser-driven modes and ordered replies under
  saturated repaint wakeups; palette replies and OSC 52 exclusion; real PTY
  reply bytes; gpui command precedence, UTF-8/control delivery, focus and window
  activation, and cross-window attachment handoff; stale parser/input generations
  after restart; paste scroll-to-bottom, stopped Copy/Paste, detached commands;
  existing session/product transfer, protected closure, and command regressions.
- Real application fixtures passed on this macOS host: Vim 9.1 (patches 1–1752)
  with clean config edited a file and exited; less 668 paged to the end and
  exited. Both entered alternate screen and restored the primary grid. Fixture
  tests report unavailable executables on hosts lacking them. Host shell is
  zsh 5.9; PTY protocol fixtures use `/bin/sh`. tmux is not installed on this
  host, so no tmux or native Linux/Windows acceptance is claimed.
- ✅ `cargo test --lib -- --test-threads=1` passed all 500 tests on macOS.
  `cargo fmt` ran once after Rust work; `git diff --check` passed.
- ✅ User accepted checkpoint 2 on 2026-10-11 (“all seems good”). Native
  checklist: shell, Vim, less, and tmux navigation, function keys,
  paste, alternate-screen entry/exit, and focus changes. Record the actual
  versions and gaps. No additional application versions were supplied.

## Checkpoint 3: Application mouse reporting and local override

- [x] ✅ Add mode-aware button, release, motion, and wheel routing, including
  required legacy and SGR encoding and coordinate limits. Keep viewport
  reporting coordinates distinct from scrollback selection coordinates.
- [x] ✅ Implement Shift local override, local interaction without tracking, and
  alternate-screen scrolling. Reset transient pressed/drag state on lifecycle
  and mode transitions so stale gestures cannot affect a new attachment.
- [x] ✅ Test encoding and routing across tracking modes, modifiers, cell edges,
  scrollback, wide cells, and wheel accumulation. Add gpui event tests for
  press/drag/release ownership and override behavior, including detach/restart.
- [x] ✅ Update the terminal interaction reference.

Review gate: user exercises a mouse-enabled Vim/tmux session and a deterministic
mouse-report fixture. Confirm clicks, drags, wheel events, Shift selection and
copy, and local scrolling; review that each event has one intended recipient.

Checkpoint 3 implementation choices and evidence:

| Request / interaction | Behavior |
| --- | --- |
| 1000 click tracking | Buttons and wheel; no motion |
| 1002 drag tracking | Buttons/wheel plus motion for presses owned by this view |
| 1003 all-motion tracking | Drag behavior plus unpressed hover motion |
| Legacy encoding | CSI M with byte coordinates; maximum one-based coordinate 223 |
| UTF-8 1005 | CSI M with UTF-8 button and coordinates; maximum one-based coordinate 2015 |
| SGR 1006 | CSI <code;column;row M/m; preserves button on release; grid bounds limit coordinates |
| Button/modifier codes | Left/middle/right 0/1/2; navigation 128/129; Shift/Alt/Control add 4/8/16; motion adds 32 |
| Wheel codes | Up/down/left/right 64/65/66/67; one packet per accumulated step |
| Shift press / reporting disabled / no tracking | Local left selection, including scrollback and wide/combining content |
| Modifier changes during drag | Original recipient retains ownership through release; reported gestures remain paired |
| Shift wheel | Local scroll, even when tracking or alternate scrolling is enabled |
| Alternate screen without tracking | With ALTERNATE_SCROLL, emit current-mode cursor keys; otherwise local scrolling |
| Unknown or unowned drag/release | No PTY report or local selection mutation |
| Outside drag/release | Original owner receives clamped viewport/grid coordinates |
| Cancellation | Balance current application presses before geometry/focus/attachment changes; discard stale generations or modes |

- Motion deduplicates cell/button/modifier state. Reporting uses viewport cells,
  including each wide-cell spacer; selection alone applies display offset and
  emulator wide-cell expansion. Unrepresentable legacy/UTF-8 coordinates never
  wrap; an owned release beyond the limit uses its last representable cell to
  avoid a stuck button. Repeated tracking transitions advance a per-process pointer revision
  even when repaint wakeups coalesce and final flags match the old flags.
- Line-wheel deltas represent rows/steps directly. Precise application and
  alternate scrolling accumulate one measured cell per step; precise local
  scrolling retains four logical pixels per row. A route change clears residue;
  opposite direction drops the old fractional residue. Local horizontal scroll
  has no grid presentation effect. Mouse packets preserve scrollback position.
- Automated coverage: byte tables for buttons, wheel, releases, every modifier,
  encoding limits, and UTF-8 extended button values; fractional wheels and route
  changes; gpui event-to-PTY cases for click/drag/all-motion modes, outside and
  foreign drags, Shift selection/copy and scrolling in scrollback, wide spacers,
  alternate-screen cursor keys, mode disable/re-enable, resize/focus cancellation,
  detach/handoff, and restart. The manual fixture also passed gpui-to-real-PTY
  tests for SGR, legacy, and UTF-8 decoding and mode restoration on quit.
- Native fixture (Python 3): `python3 scripts/terminal-mouse-fixture.py` defaults
  to SGR drag tracking. Use `--tracking clicks|drag|motion`,
  `--encoding legacy|utf8|sgr`, and `--alternate` to check other paths. Hold Shift
  before pressing to start local selection; changing modifiers mid-drag retains
  the original owner. `q` quits and restores terminal modes.
- ✅ macOS verification: `cargo test --lib -- --test-threads=1` passed all
  520 tests after the mouse-reporting toggle addition. Rust formatting and `git diff --check` completed.
- 🛠️ Native review confirmed Shift-drag + Cmd+C works in Vim. Ordinary Vim
  dragging selected within the application and therefore supplied no terminal
  selection to Copy. On 2026-10-13 the user agreed to an enabled-by-default
  per-terminal mouse-reporting toggle. `terminal.toggle-mouse-reporting` appears
  in the command palette as **Toggle Terminal Mouse Reporting**; the header shows
  `mouse: on/off`. Disabling it balances an active reported gesture, permits
  ordinary selection and Copy, and retains emulator modes for re-enabling.
  Session-local preference survives restart and cross-window view reconstruction;
  new sessions start enabled. Alternate scrolling is independent of reporting.
  Tests cover disabling during a reported drag, ordinary selection/Copy,
  re-enabling, detached rejection, restart/handoff retention, and native Copy
  shortcut dispatch through the product command router.
- ✅ User accepted checkpoint 3 on 2026-10-13 (“ok works”), including the
  mouse-reporting toggle and ordinary drag + Copy. Existing host versions are
  Vim 9.1 and Python 3.14.8; tmux is unavailable. No Linux/Windows certification
  is claimed.

## Checkpoint 4: Hyperlinks and complete slice acceptance

- [x] ✅ Carry OSC 8 link identity into the displayed snapshot and detect HTTP(S)
  URLs across soft wraps without merging separate hard lines. Keep explicit
  destinations authoritative and define punctuation boundaries.
- [x] ✅ Add hover feedback and modifier-click activation through the native URL
  opener. Revalidate against current grid and geometry; clear transient state
  across output changes, scroll, resize, detach, transfer, and restart.
- [x] ✅ Test explicit links, detected URLs, precedence, wrapped/Unicode content,
  scheme filtering, stale targets, and pointer-policy conflicts. Inject or
  capture the opener in tests so event wiring is verified without launching
  external applications.
- [x] ✅ Run the complete terminal/lifecycle and affected editor/command regression
  coverage. Record the compatibility matrix and acceptance evidence in this
  plan; document remaining gaps explicitly.
- [x] ✅ Update architecture references and validated decisions. Preserve any
  remaining deferred scope in the roadmap under stable IDs.

Review gate: user checks links in live output and scrollback, including
mouse-enabled applications, then repeats the representative shell/TUI flows
through resize, tab switching, cross-window transfer, restart, and protected
closure. Complete D026 only when the agreed contract passes and remaining
limitations are recorded.

Checkpoint 4 implementation choices and evidence:

| Link / interaction | Supported behavior |
| --- | --- |
| OSC 8 | Preserve identity and destination; explicit link labels override detection |
| Detected HTTP(S) | Case-insensitive scheme, host required, visible grid including scrollback |
| Soft / hard lines | Join emulator soft wraps; never join hard line breaks |
| Unicode | Map UTF-8, wide cells/spacers, and combining marks to displayed cells |
| Boundaries | Whitespace/control/quotes/angle brackets/backticks end detection; trim trailing `.,;:!?` and unmatched `)]}`; retain balanced delimiters |
| Clipped continuation | Do not open a detected URL extending below the viewport; explicit OSC 8 remains complete |
| Schemes | HTTP(S) only; reject control characters; unsupported explicit schemes suppress label detection |
| Hover / activation | Underline hovered link; modifier shows pointing hand; Command-click on macOS / Control-click elsewhere opens on release |
| Pointer precedence | Recognized modifier-click owns press/move/release before application reporting; Shift remains local; ordinary clicks retain existing routing |
| Validation | At most 4 logical pixels of motion; recheck displayed cells, offset, identity, generation, bounds, layout, release location, and modifiers |
| Cancellation | Output notifications, scroll, focus/geometry changes, detach, handoff, and restart discard hover/armed targets; cancelled link gestures send no mouse packets |

- Added eight parser/grid tests and five gpui event-to-PTY/opener tests. Coverage
  includes explicit destination/identity precedence, blocked schemes and controls,
  punctuation, soft/hard lines, Unicode cell mapping, clipped continuations,
  displayed scrollback lookup and activation, real mouse-report precedence,
  ordinary clicks, Shift selection, movement cancellation, changed output,
  repaint invalidation, wheel/resize/detach cancellation, cross-window attachment
  handoff, and restart. Injected openers capture destinations without launching
  external applications. Scrollback fixtures settle the emulator's scroll
  notification before starting their activation gesture.
- ✅ macOS verification: `cargo test --lib -- --test-threads=1` passed all 533
  tests, including editor, command, protected-closure, terminal lifecycle,
  real Vim/less, typography, keyboard, mouse, and link regressions. `cargo fmt`
  ran once after Rust work; `git diff --check` passed.
- Deferred contract extensions retain roadmap IDs: physical keypad D040,
  additional protocols/events D041, additional link destinations D042, persistent
  sessions D043, simultaneous views D044, general terminal settings D045,
  remote PTYs D038, and shell integration D039. No tmux acceptance or
  Linux/Windows certification is claimed; this host lacks tmux.
- Native fixture: run `python3 scripts/terminal-links-fixture.py` in Knot.
  Check detected, wrapped, Unicode, explicit-label, and unsupported-scheme links
  with hover and Cmd-click. Scroll the output into history and repeat; use
  mouse-enabled Vim to check recognized link precedence and Shift selection.
- ⏳ Final native gate remains open: links in live output and scrollback,
  mouse-enabled applications, then shell/TUI behavior through resize, tab
  switching, cross-window transfer, restart, and protected closure. Keep the
  plan active until the user accepts this gate; archive in a separate commit
  only afterward.
