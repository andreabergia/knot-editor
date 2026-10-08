# D028: Linux framework checkpoint

Status: planned. Design agreed; no checkpoint implemented.

Source: D028, promoted from the [deferred-work register](../roadmap.md).
Current boundaries: [architecture](../architecture.md),
[workbench lifecycle](../architecture/workbench-and-lifecycle.md), and
[personal configuration decisions](../decisions.md#personal-configuration).
Related work: [D003b configurable keymaps](d003b-keymaps-plan.md), being
implemented in a separate worktree.

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

### 1. Linux build and personal-config contract ⬜

- Establish a reproducible build/test recipe on this machine. Record the
  distribution, compositor/session, Rust and gpui versions, and required native
  libraries. Existing Ubuntu CI provides build/test coverage, not desktop
  validation; avoid duplicating it without a demonstrated gap.
- Add Linux tests exercising the real `ProjectDirs`, `BaseDirs`, and
  `user_config_root` path under controlled HOME/XDG environments. Use isolated
  subprocesses rather than mutating the environment of parallel tests.
- Cover unset, absolute, empty, and relative XDG values; existing-root
  precedence; fallback when preferred roots are absent; and an existing invalid
  preferred root producing a capture error rather than loading a lower root.
  Assert the concrete Linux directory spelling, including application-name
  casing. Preserve the validated selection policy.
- Exercise selection followed by source capture for optional phases, local
  imports, selected-root symlinks, and rejected escaping imports. Reuse existing
  capture coverage where it already proves the behavior; add tests for gaps.
- Run focused tests, then the relevant existing Linux suite and Clippy. Record
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
