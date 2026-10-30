# D127 — Restored terminals keep their working directory

Status: 🚧 in progress; checkpoint 3 awaiting review. Item of the
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
read when the layout is captured. The field is optional, so the layout
version stays 1: a stored layout without it restores with D121's default, and
bumping would only discard stored sessions. Captures already happen when the per-window debounce
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

### 1. Session working directory query ✅

- `TerminalSession::current_directory()` reports the shell's live directory
  through [terminal/process.rs](../../src/app/terminal/process.rs), else the
  start directory. Live queries were validated on macOS only; the Linux path
  is untested.

### 2. Record and restore the directory ✅

- Terminal tabs record an optional `directory` file URI in the
  [window layout](../architecture/workbench-and-lifecycle.md#window-layouts);
  restoration starts the shell there or falls back to D121's default. The
  layout version stays 1.

### 3. New terminals from a focused terminal ✅ (awaiting review)

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
