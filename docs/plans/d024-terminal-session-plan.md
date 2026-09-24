# D024: Product Terminal Sessions

Status: planned.

This slice brings the validated local terminal into the product workbench. A
stable `TerminalSession` owns the PTY, emulator, process, and terminal events;
one disposable `TerminalView` owns focus, layout, input presentation, and gpui
rendering. A running session survives rebuilding its view or moving its tab to
another window. Closing its terminal tab explicitly ends the session.

The slice succeeds when a user can open a terminal tab, interact with a local
shell, split from that tab, move the live terminal to another window, and close
tabs, windows, or the application without orphaning a child process or losing
dirty document protection. A move transfers the sole presentation; it never
creates a second view of the session.

## Selected scope and design

- Target macOS product dogfooding. Keep existing portable boundaries, but do
  not claim Windows or Linux product validation in this slice.
- Extract the current `alacritty_terminal` implementation first. Consult Zed's
  attributed terminal implementation for concrete adapter, input, rendering,
  resize, or event behavior where the existing code proves insufficient. Do
  not wholesale-port it as a prerequisite for the ownership change. Record
  any resulting change to the production source decision in `decisions.md`.
  The reference checkout is `~/src/libs/zed` at commit
  `baee1ca6de0feb9b87d4e5288cf40b02bdf4c370`; it is outside Knot and
  is not a vendored dependency.
- Keep terminal state in `app`, outside `BufferModel`, documents, persistence,
  and extension protocol. An application-owned session registry holds each
  live session by stable identity; a workbench tab refers to that identity and
  owns its single presentation. Dropping or rebuilding a view does not
  implicitly close the process.
- Use a typed workbench tab payload for document editors and terminals. Keep
  pane/tab IDs and layout shared, while document identity, dirty-state checks,
  Open/Save, and editor commands apply only to document tabs.
- Give each terminal one authoritative grid size. Its attached view sends
  resize updates; during a transfer, keep the session alive and switch the
  sole attachment to the destination view. No simultaneous views or detached
  background persistence.
- Opening a terminal creates a new local session in the focused pane. Splitting
  a terminal pane creates a *new independent terminal session* in the new pane;
  splitting a document continues to create another view of that document.
- Provide an explicit Move Terminal to New Window action as the narrow
  cross-window transfer path. General tab dragging/docking remains D004.
- Terminal close, window close, and Quit use the existing application-level
  protected-close coordination. Dirty document decisions complete before any
  terminal is terminated or moved. A successful close shuts down each affected
  session once and reaps its child off the gpui foreground thread. Natural
  process exit leaves the tab visible with its final grid and status until the
  user closes or restarts it.
- Keep one coalesced foreground wakeup path per session. PTY reads, writes,
  shutdown, and child reaping must not block gpui. Stale wakeups and exit events
  cannot affect a replacement session or transferred view.
- Keep existing terminal input/rendering behavior as a baseline. D025 covers
  persistence, simultaneous views, remote PTYs, and shell integration; D026
  covers broader compatibility, mouse reporting, and hyperlinks.

## Invariants to review

- A live session has at most one attached `TerminalView` and one registry
  entry. Rebuilding a view preserves session identity and process; explicit
  closure ends both.
- Captured command targets name the exact tab and surface kind. A delayed
  document command cannot act on a terminal or a replacement tab, and a
  terminal command cannot retarget after a transfer or close.
- Document view counts and dirty-close deduplication ignore terminal tabs.
  Closing a mixed window or quitting leaves all tabs and sessions intact if a
  document save is cancelled or fails.
- Transfer either installs one destination presentation and removes the source
  or leaves the source intact. A failed destination window does not stop the
  session or strand it without a view.
- Process exit, restart, explicit close, and application shutdown each settle
  lifecycle work once. No PTY join runs on the foreground thread.

## Delivery protocol

Execute each checkpoint as a separate `/goal`. In each goal, make logical
commits as work lands, mark completed checklist items with ✅, run focused
tests and nearby affected tests, then stop for code review and the manual gate.
Do not begin the next checkpoint in the same goal. Run `cargo fmt` once at the
end of each goal that changes Rust, immediately before final tests and commits.
Keep the plan current, including any scope decisions made at a gate.

## Checkpoint 1: Make workbench tabs surface-aware

- [ ] Model document and terminal tab payloads without assigning a fake
  `DocumentId` or `BufferModel` to terminals. Preserve stable pane/tab IDs,
  activation, layout, and editor-view behavior.
- [ ] Make snapshots, application-wide document view counts, and close planning
  inspect only document tabs for dirty document decisions.
- [ ] Define captured product target validation by exact tab identity and
  surface kind. Document-only commands return an explicit unavailable or
  invalid-target outcome for terminal tabs as appropriate; native terminal
  input remains view-owned.
- [ ] Cover mixed-tab activation, split policy, stale captured targets,
  document view counts, and mixed-window/quit close decisions with workbench
  and product command tests. A lightweight terminal tab test double may be
  used before live PTY integration.

Automated checks: focused `app::workbench`, `app::product_commands`, and
protected-close tests; `cargo check --all-targets`.

Manual review gate: inspect tab ownership and target validation. Confirm no
terminal path enters document persistence or changes dirty-document prompts.

## Checkpoint 2: Extract a stable terminal session

- [ ] Move PTY, Alacritty grid, child status, coalesced wakeups, input/resize
  transport, restart, and shutdown out of `TerminalView` into a stable session
  model. Keep layout, focus, scroll interaction, and gpui drawing in the view.
- [ ] Make view attachment/detachment explicit. Rebuilding a view over one
  session preserves its grid and process; dropping the view alone does not
  terminate the child.
- [ ] Ensure explicit close and restart invalidate prior wakeups and exit
  events, terminate the right child, and reap asynchronously. Define behavior
  for startup failure and natural exit while retaining final visible output.
- [ ] Add tests using a controllable local child/PTY fixture for view rebuild,
  output and exit delivery, resize, close/restart races, and exact-once
  shutdown. Preserve the prototype's sustained-output responsiveness check.

Automated checks: focused terminal lifecycle tests; `cargo check --all-targets`.

Manual review gate: inspect session/view references and thread boundaries.
Run a local shell and a full-screen TUI, rebuild its view, and confirm the
process and grid survive. Check process exit and restart in Activity Monitor
or an equivalent process listing.

## Checkpoint 3: Make terminals usable in the product workbench

- [ ] Add a discoverable New Terminal command and product UI entry point. Open
  a terminal tab in the captured pane, focus it, and keep editor commands from
  consuming terminal keystrokes.
- [ ] Render terminal tabs and active terminal views in ordinary workbench
  panes. Switching tabs retains the session. Split from a terminal creates a
  new session; split from a document keeps existing shared-document behavior.
- [ ] Route Close Tab, Close Window, and Quit through mixed-surface closure.
  Finish dirty-document decisions before terminal teardown; closing the final
  tab follows the product shell's empty-workbench policy.
- [ ] Cover command entry, focus and activation, independent split sessions,
  close/reopen, mixed close cancellation and save failure, and window/quit
  shutdown with product-level tests.

Automated checks: focused product, command, workbench, and terminal tests;
`cargo check --all-targets`.

Manual review gate: open a shell from the product UI; type, scroll, resize,
switch tabs, split, and close. Cancel a dirty-document close in a mixed window
and confirm the terminal stays running. Confirm closing a terminal ends its
child without freezing the UI.

## Checkpoint 4: Transfer a live session to a new window

- [ ] Add Move Terminal to New Window for a captured terminal tab. Transfer
  the same session and final grid to one new view, preserving process identity.
- [ ] Make destination creation and source removal one validated operation
  with a safe failure path. Revalidate tab/session identity across any deferred
  work and prevent a second active presentation during handoff.
- [ ] Keep resize and focus owned by the destination view after transfer.
  Closing the source window after a move cannot shut down the transferred
  session; closing the destination terminal can.
- [ ] Cover successful transfer, stale target, destination creation failure,
  source close, destination close, and process exit during handoff.

Automated checks: focused transfer, close, and terminal lifecycle tests;
`cargo check --all-targets`.

Manual review gate: run a shell command that exposes its PID, move the terminal
to a new window, and verify the PID, output, scrollback, input, and resize
behavior remain continuous. Close the original window, then close the terminal
and confirm the child exits.

## Checkpoint 5: Product acceptance and documentation

- [ ] Exercise an interactive shell, alternate-screen TUI, ANSI styling,
  cursor, scrollback, resize, natural exit, restart, and sustained output in
  the product window. Confirm editor/terminal font appearance against the
  selected Knot settings; record concrete parity gaps without expanding this
  slice into D026.
- [ ] Run the full applicable test suite and `cargo check --all-targets`.
  Resolve regressions in document editing, protected close, command routing,
  and terminal lifecycle.
- [ ] Update `docs/architecture.md` if ownership or dependency direction
  changes, and update `docs/architecture/workbench-and-lifecycle.md` and
  `docs/architecture/commands.md` for implemented flows. Update
  `docs/decisions.md` for the final Zed-source and session-lifetime rationale;
  align any stale terminal wording in `docs/design.md`.
- [ ] Mark this plan completed only after automated checks and the manual
  product gate pass. Record any remaining defects under the relevant deferred
  item with a concrete trigger.

Manual review gate: dogfood a mixed editor/terminal workbench across two
windows, including save cancellation and Quit. Inspect process cleanup and UI
responsiveness before declaring D024 complete.
