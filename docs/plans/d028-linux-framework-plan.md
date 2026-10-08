# D028: Linux framework checkpoint

Status: checkpoint 1 implementation verified; awaiting review. Native launch
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

### 1. Linux build and personal-config contract 🟡 (implementation ✅; review pending)

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

### 2. Native startup and one shortcut ⬜

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
- Review gate: pending inspection of directory values, capture coverage, and
  the build/test recipe. Native startup has not been attempted.
