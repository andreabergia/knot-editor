# D127 — Restored terminals keep their working directory

Status: 🚧 approved, not started. Item of the
[daily-driver plan](daily-driver.md); builds on D121's default directory rule
([workbench and lifecycle](../architecture/workbench-and-lifecycle.md#terminal-sessions-and-views),
[decisions](../decisions.md#terminal)).

## Outcome

- A terminal restored in place, by native hot exit or a project switch, starts
  its fresh shell in the directory its shell was last in.
- New Terminal and splitting a terminal pane, when a terminal is focused, start
  in that terminal's current directory, as iTerm, Ghostty, and Zed do. Without
  a focused terminal, D121's rule applies unchanged.
- Processes and scrollback still do not survive (D043).

## Design

**Reading the directory.** A session asks its shell process for its working
directory on demand: `proc_pidinfo(PROC_PIDVNODEPATHINFO)` on macOS and
`/proc/<pid>/cwd` on Linux. The shell's pid is taken from the PTY's child
before the PTY moves into Alacritty's event loop. The query targets the shell,
not the foreground job, so a running `vim` or `cargo` does not change the
answer. It needs no shell setup, unlike OSC 7 (D039), which Alacritty does not
parse. The mechanism is replaced when the terminal engine moves to libghostty
(D128, after D063).

**Fallback.** When the process cannot be queried (Windows, a shell that has
exited, a failed start), the session reports the directory it was started in.
A `terminals.open` directory or the D121 choice therefore stays stable across
restarts instead of being re-derived from the last focused document.

**Recording.** A terminal tab records an optional `directory` as a file URI,
read when the layout is captured. The layout version is bumped; no
compatibility path. Captures already happen when the per-window debounce
settles, at quit, and when a project switch captures layouts, so quit and
switches always see a fresh value. A `cd` does not start a capture, so after a
crash a terminal restores to its directory at the last debounced capture; this
gap is accepted.

**Restoring.** A recorded directory that is still an existing local directory
is used; otherwise the terminal falls back to D121's default. No walk up to
the nearest existing parent.

**New terminals from a terminal.** The window's default terminal directory
first checks whether the focused pane's active tab is a terminal and, if so,
uses that session's current directory. This covers New Terminal, splitting a
terminal pane, and `terminals.open` without an explicit directory, since they
all share the default.

## Exclusions

- OSC 7 and shell integration (D039); remote shells (D038).
- Reading the directory on Windows. It records the start directory.
- Restoring processes, scrollback, or environment (D043).
- Captures triggered by terminal activity.

## Checkpoints

### 1. Session working directory query

- Deliverable: `TerminalSession::current_directory()` returns the shell's live
  working directory on macOS and Linux, otherwise the start directory.
- Validation: session tests that spawn a shell in a temporary directory, send
  `cd <subdir>` and observe the new directory, and that an exited or failed
  session reports its start directory.
- Review gate: query behavior and platform coverage.
- **Implementation context:** `src/app/terminal/session.rs`.
  `RunningTerminal::start` creates the PTY with `tty::new`, then moves it into
  `EventLoop::new`; read `pty.child().id()` (Unix only) before the move and
  keep the pid on `RunningTerminal`. Put the platform query in a small module
  beside `terminal/directory.rs` (`cfg(target_os = "macos")` with `libc`'s
  `proc_pidinfo`/`proc_vnodepathinfo`, `cfg(target_os = "linux")` with
  `read_link`); add `libc` as a direct dependency in `Cargo.toml`. The existing
  `working_directory()` accessor returns the start directory from `Options`.
  Mirror the waiting patterns in `session_tests.rs`.

### 2. Record and restore the directory

- Deliverable: the window layout records each terminal's directory, and
  restoration by hot exit and project switch starts the shell there, falling
  back to D121's default when the directory is gone.
- Validation: `window_layout.rs` round-trip and planning tests for the new
  field and version; product tests that capture a window with a terminal whose
  start directory differs from the default, restore it, and check the new
  session's directory, plus the missing-directory fallback.
- Review gate: layout shape and restore fallback.
- **Implementation context:** `src/app/window_layout.rs`: `LayoutTab::Terminal`
  gains `directory: Option<String>`, capture at the
  `TabSurfaceId::Terminal(_)` arm reads the session through
  `TerminalSessions`, `PlannedTab::Terminal` carries the directory, and
  `WINDOW_LAYOUT_VERSION` goes to 2 (update the JSON fixtures in its tests).
  `src/app/product/sessions.rs`: the `PlannedTab::Terminal` arm calls
  `create_terminal_in` with the recorded path when it is an existing
  directory, else `create_terminal`. Product tests live in
  `src/app/product/tests/terminal.rs` (see `focused_terminal_directory`) and
  the session tests beside `product/sessions.rs`.

### 3. New terminals from a focused terminal

- Deliverable: New Terminal, splitting a terminal pane, and `terminals.open`
  without a directory start in the focused terminal's current directory.
- Validation: product tests in `tests/terminal.rs` beside the D121 directory
  tests: a focused terminal wins over the focused document's root; a focused
  document still uses D121's rule.
- Review gate: behavior and documentation.
- **Implementation context:** `ProductShell::terminal_directory` in
  `src/app/product.rs` checks the focused pane's active tab for a terminal
  surface and returns its session's `current_directory()`, before the existing
  document and root rule. Restoration must not go through this path for a
  recorded directory. Update the terminal section and the window layouts
  section of
  [workbench and lifecycle](../architecture/workbench-and-lifecycle.md), the
  Terminal entry in [decisions](../decisions.md#terminal), and mark D127 ✅ in
  the [daily-driver plan](daily-driver.md); then remove this plan.

## Deferred

- D128: read the directory from libghostty after D063, replacing the process
  query and possibly covering Windows and OSC 7.
