# D028: Linux framework checkpoint

Status: checkpoint 1 approved. Checkpoint 2 in progress; native user checks
and smoke-test gates remain open.

Source: D028, promoted from the [deferred-work register](../roadmap.md).
Current boundaries: [architecture](../architecture.md),
[workbench lifecycle](../architecture/workbench-and-lifecycle.md), and
[personal configuration decisions](../decisions.md#personal-configuration).
Related work: [D003b configurable keymaps](../archive/plans/d003b-keymaps-plan.md), now integrated.

## Outcome and design

Knot builds and launches in the user's Linux Wayland desktop session. Personal
configuration selects the expected Linux directory, captures its JavaScript
sources, and participates in the existing startup lifecycle. The user can
test one shortcut and perform a small native product smoke test. This is a
framework checkpoint, not a claim of broad Linux product support.

Preserve the existing config policy: choose an existing absolute
`XDG_CONFIG_HOME/knot`, then an existing `~/.config/knot`, then the platform
directory returned by `ProjectDirs`. Verify actual Linux values rather than
testing only an injected native path. Keep config capture off the foreground
thread, pre-init and post-init in their existing shared lifecycle, and config
failure fatal to product startup.

Use **Ctrl-Shift-P to open the command palette** as the single shortcut under
test. It gives the user access to other commands without expanding this task
into a Linux default-keymap project. If the integrated D003b implementation
already permits that binding through personal config, document a minimal
config example using its actual API. Otherwise add only a Linux-native alias
through the current binding mechanism, with one dispatch regression test.
Do not depend on unfinished D003b work or implement its registry/API here.

Keep fixes within the existing application and gpui platform boundaries.
Record concrete blockers and observed results at each review gate; an
unresolved native launch blocker prevents the checkpoint from being marked
complete. Framework patches or dependency changes that materially expand the
work require design review before proceeding.

## Exclusions and deferred decisions

- Configurable keybindings, general Linux default bindings, and cross-worktree
  integration belong to D003b. Only the one shortcut above belongs here.
- X11 validation, other compositors/distributions, Linux packaging,
  installation, desktop associations, and distribution support are deferred.
- Desktop or compositor configuration changes are not part of this plan.
- Broader font, terminal compatibility, accessibility, and platform polish
  remain under their existing roadmap items.
- Exact build prerequisites and any necessary framework adapter are determined
  from the checked-out dependencies and observed Linux failures.

## Checkpoints

### 1. Linux build and personal-config contract ✅

- ✅ Establish a reproducible build/test recipe on this machine. Record the
  distribution, compositor/session, Rust and gpui versions, and required native
  libraries. Existing Ubuntu CI provides build/test coverage, not desktop
  validation; avoid duplicating it without a demonstrated gap.
- ✅ Add Linux tests exercising the real `ProjectDirs`, `BaseDirs`, and
  `user_config_root` path under controlled HOME/XDG environments. Use isolated
  subprocesses rather than mutating the environment of parallel tests.
- ✅ Cover unset, absolute, empty, and relative XDG values; existing-root
  precedence; fallback when preferred roots are absent; and an existing invalid
  preferred root producing a capture error rather than loading a lower root.
  Assert the concrete Linux directory spelling, including application-name
  casing. Preserve the validated selection policy.
- ✅ Exercise selection followed by source capture for optional phases, local
  imports, selected-root symlinks, and rejected escaping imports. Reuse existing
  capture coverage where it already proves the behavior; add tests for gaps.
- ✅ Run focused tests, then the relevant existing Linux suite and Clippy. Record
  any platform corrections and their regression coverage.
- **Review gate:** inspect actual directory values, precedence, source capture,
  and the build/test recipe before native launch work.

### 2. Native startup and one shortcut 🟡

- Launch the actual product in this machine's Wayland session with isolated
  test configuration and extension directories, keeping the user's real config
  and installed extensions intact. Record how the launch selects those roots.
- Verify a missing/empty config opens a product window. Verify a small config
  with a local import runs pre-init and post-init across extension startup and
  exposes an observable result through the existing product API.
- Verify malformed source and phase evaluation failures open the diagnostic
  window, keep product windows unavailable, and support Copy and Quit. Add
  focused lifecycle/UI regression coverage for any Linux-specific correction;
  retain existing startup tests for shared behavior.
- Make Ctrl-Shift-P open the command palette using the minimal route described
  above. Test dispatch automatically and have the user press it in the native
  window; record both outcomes. Leave further shortcut work to D003b.
- **Review gate:** user confirms native startup, the diagnostic path, and the
  single shortcut before completing the product smoke test.

### 3. Small product smoke test and evidence ⬜

- Use the palette and visible controls to check text entry and navigation,
  representative Unicode rendering, selection and clipboard, Open, Save,
  Save As, dirty-close cancellation, and successful closure. Use disposable
  files; verify saved bytes and that cancellation preserves the edited view.
- Open one terminal, type a command, resize it, and close it. Check the process
  shuts down through the existing session lifecycle. This does not expand into
  terminal compatibility work.
- Fix blockers within the agreed scope and cover durable behavior at the
  model, lifecycle, command, or persistence boundary; use UI/integration tests
  where lower-level tests cannot establish the behavior. Record other gaps
  explicitly for later work.
- Run required automated checks after fixes. Run `cargo fmt` once at the end
  of Rust work before committing; keep implementation in logical commits.
- Record the smoke-test matrix, environment, commands, results, and remaining
  limitations in this plan. Update architecture references only if boundaries
  or behavior change, and decisions only for validated choices. Mark completed
  checkpoints with ✅.
- **Review gate:** approve the observed Wayland checkpoint and regression
  coverage, with no broader Linux support claim.

## Checkpoint 1 evidence (2026-10-08)

- Environment: Omarchy 4.0.4 (Arch family), Hyprland 0.56.2-2,
  `XDG_SESSION_TYPE=wayland`, `WAYLAND_DISPLAY=wayland-1`.
  Rust 1.99.0 (`b940084d7`), Cargo 1.99.0 (`5f94df478`), locked gpui 0.2.2,
  directories 6.0.0, V8 152.2.0.
- Native build prerequisites present: GCC 16.2.1, Clang 22.1.8, pkgconf 3.0.7,
  fontconfig 2.18.3, FreeType 2.14.3, Vulkan loader 1.4.357.0,
  Wayland 1.26.0, libX11 1.8.13, libxcb 1.17.0, libxkbcommon and
  libxkbcommon-x11 1.13.2. Arch packages provide headers alongside libraries.
  Ubuntu CI already installs the corresponding development libraries; no new
  CI job is needed.
- Observed build correction: gpui's transitive xattr 0.2.3 references Linux
  `libc::ENOATTR`, removed in libc 0.2.190. Lock libc to 0.2.189 with
  `cargo update -p libc --precise 0.2.189`; no framework patch is required.
  Use `--locked` for subsequent builds to preserve this compatibility choice.
- Enable gpui's public `wayland` feature for the Linux product dependency.
  Its `test-support` feature already enables Wayland and X11 for tests, so
  a passing test build alone did not establish the product backend.
- Linux config contract: the real ProjectDirs fallback is
  `$HOME/.config/knot`, or `$XDG_CONFIG_HOME/knot` for an absolute XDG value.
  Empty and relative values are ignored. An existing default root outranks
  a missing absolute XDG root; an existing absolute root outranks the default.
  Missing roots remain optional. File and dangling-symlink preferred roots
  must fail capture rather than load a lower root.
- New subprocess coverage calls real BaseDirs, ProjectDirs, user_config_root,
  and capture with isolated HOME/XDG values. It covers optional phases, local
  imports, a selected-root symlink, and an escaping static import. Existing
  capture tests additionally cover source immutability, malformed imports,
  escaping source symlinks, unsupported directory symlinks, and deferred
  post-init errors.
- Reproduction from the repository root:
  `cargo test --locked personal_config --lib`,
  `cargo test --locked --all-targets`,
  `cargo clippy --locked --all-targets -- -D warnings`,
  `cargo build --locked --bin knot`.
  Run `cargo fmt` once after Rust edits and before committing.
- ✅ Automated results: focused personal-config tests passed (17 tests, including
  15 isolated HOME/XDG child cases); final `cargo test --locked --all-targets`
  passed all 454 library tests and the binary target. Clippy with warnings denied
  and the separate product binary build passed. `cargo fmt` and `git diff --check`
  completed. The dependency proc-macro-error2 2.0.1 emits Cargo's existing
  future-incompatibility notice; it does not fail these checks.
- Linux suite corrections: keymap tests now look up gpui's canonical key spelling
  (`cmd` on macOS, `super` on Linux) rather than hardcoding macOS output. The
  normalization test explicitly asserts each platform's spelling. Mark the
  test-only palette helper accordingly so the product build passes Clippy.
- The suite also exposed a shared command-cancellation race: a host request could
  be observed before active-command publication, leaving cancellation suspended
  indefinitely. Coordinate publication and cancellation under the existing
  cancellation mutex; consume earlier cancellation in the initial command turn.
  Preserve the existing suspended-request/late-response test and add a barrier
  regression forcing cancellation before publication. Both pass in the full
  suite. This is a contained lifecycle fix; no dependency direction changes.
- ✅ Review gate: user approved checkpoint 1 on 2026-10-08.

## Checkpoint 2 evidence (in progress)

- Empty-config native launch uses `/tmp/knot-d028-native-mw8dgrrf` with
  HOME set to its `home`, XDG_CONFIG_HOME to its `config`, and XDG_DATA_HOME
  to its `data`. The existing `config/knot` selects the isolated config root;
  ProjectDirs selects `data/knot/extensions` for installed extensions. Real
  config and installed extensions remain intact.
- Launch: execute `target/debug/knot` with those environment values while
  retaining the real Wayland and session-bus environment. Hyprland reported
  a mapped, visible, input-accepting window titled Knot, PID 177761,
  `xwayland=false`. That historical exec handle is no longer relied on.
  ✅ On 2026-10-09 the user confirmed “I’ve tested, it works” and reported
  poor terminal fonts. This confirms usability of their tested window; the
  specific shortcut and full smoke matrix still require explicit evidence.
- D003b's integrated personal API supports the required shortcut. Minimal
  `post-init.js`:

  ```js
  import * as knot from 'knot';
  knot.keybinding('ctrl-shift-p', 'workbench.show-command-palette');
  ```

- Added a focused gpui regression loading that actual post-init configuration,
  simulating Ctrl-Shift-P, and asserting the palette is open. ✅ Focused test
  passed; all 455 library tests and the binary target passed; Clippy with
  warnings denied passed. Formatting and diff whitespace checks completed.
- Remaining: config/local-import startup across extension initialization,
  malformed-source and phase-evaluation diagnostic windows with Copy and Quit,
  user shortcut check, and checkpoint 2 review approval.

### Native continuation, 2026-10-09

- Previous temporary fixture root is absent after the session change. A current
  Knot window exists (PID 4772); it is not assumed to be the old isolated launch
  and is left intact.
- New isolated root: `/tmp/knot-d028-startup-krrraf8p`, with HOME=`home`,
  XDG_CONFIG_HOME=`config`, XDG_DATA_HOME=`data` relative to that root.
  `state.mjs` exports a phase array; pre-init imports it, adds `pre`, and registers
  `d028.pre-init-ready`. Extension `@d028/startup` registers
  `d028.extension-ready`. Post-init imports the same array, asserts it contains
  `pre`, registers `d028.post-init-ready`, and installs Ctrl-Shift-P.
- The first version of that test fixture additionally invoked the extension
  command during post-init. It returned `invalidTarget` because product windows
  are unavailable during startup. This fixture error produced the expected
  Post-init diagnostic after the log confirmed extension loading. Remove the
  invocation for the next successful-config launch; use the three visible
  palette registrations as observable results. No product correction is needed.
- User Copy/Quit verification of the phase-evaluation diagnostic is pending.
- Terminal font quality is a user-reported limitation. Broader font polish
  remains deferred; no desktop configuration changes are authorized by this plan.
- ✅ Compositor verification: failed launch PID 11425 has only a mapped native
  `Knot configuration error` window; no product window belongs to that PID.
  Corrected launch PID 11535 has a mapped native `Knot` product window after
  `@d028/startup` logged successful loading. The local-import shared-state
  assertion completed, otherwise startup would have been fatal. User palette
  confirmation of all three registrations and Ctrl-Shift-P remains pending.
- ✅ Malformed-source native case: isolated root
  `/tmp/knot-d028-malformed-25o7kn89`, using the same HOME/XDG layout,
  contains `pre-init.js` with `export const = ;`. Launch logged a Pre-init
  SyntaxError at line 1, column 14. Hyprland reports PID 11657 with only a
  mapped native `Knot configuration error` window; no product window belongs
  to that process. Exec session 30632 remains live. Copy/Quit and review
  confirmation remain pending, as for the phase-evaluation case.

### Shortcut investigation, 2026-10-09

- User reported Ctrl-Shift-P did nothing. Record this as a failed manual check;
  the earlier automated test alone does not establish native usability.
- Compositor bindings contain no Ctrl-Shift-P interception. Two differently
  configured product instances had been open, so window ambiguity is possible
  but is not established as the cause of the user's failure.
- All prior Knot windows were subsequently closed. Reopened only the isolated
  configured fixture (PID 12956, exec session 47862).
- ✅ Targeted native dispatch through Hyprland's `hl.dsp.send_key_state` with
  CTRL SHIFT / P opened the palette in that PID. A cropped native-window capture
  `/tmp/d028-shortcut-after.png` shows all three `d028` startup registrations.
  Dismissed the palette for a fresh physical-key retry. This proves actual
  Wayland dispatch for the configured fixture; user keypress confirmation remains
  pending. No code or desktop configuration correction was needed or applied.
