# D029 Windows product polish plan

Status: checkpoint 1 ready for review

Source task: [D029 — Windows product polish](../roadmap.md#platforms-and-accessibility)

Relevant boundaries: [architecture](../architecture.md),
[workbench and lifecycle](../architecture/workbench-and-lifecycle.md),
[extension host](../architecture/extension-host.md), and
[decisions](../decisions.md).

## User-visible outcome

Knot builds and launches as a usable Windows 11 dogfooding application. Its
personal configuration resolves through the documented Windows and XDG
precedence, captures Windows-hosted JavaScript sources, and participates in the
existing gated startup lifecycle. A user can exercise the ordinary editor,
persistence, clipboard, command, terminal, and protected-closure paths in a
native Windows session, while fatal personal-config failures remain actionable
and prevent a partially initialized product window.

This validates the current Windows machine and desktop session as a supported
dogfooding target. It is not a broad certification claim for other Windows
versions, hardware, accessibility configurations, or distribution channels.

## Chosen design and boundaries

- Preserve the existing personal-config policy. An existing absolute
  `XDG_CONFIG_HOME\knot` wins, followed by an existing
  `%USERPROFILE%\.config\knot`, then the native `ProjectDirs` location. With
  `directories` 6 on Windows, Knot's native config root is
  `%APPDATA%\Knot\config`; on the current machine that resolves to
  `C:\Users\andre\AppData\Roaming\Knot\config`.
- Verify both halves of that contract: Windows-only coverage observes the real
  `BaseDirs` and `ProjectDirs` values, while controlled temporary candidates
  establish precedence without creating or modifying directories in the real
  user profile. Use subprocesses wherever environment mutation could race with
  parallel tests.
- Exercise source capture through real Windows filesystem and file-URL behavior,
  including drive-letter paths, separators, spaces or non-ASCII names, local
  `.js`/`.mjs` imports, missing or invalid preferred roots, and path escape
  rejection. Do not make privileged symbolic-link creation a prerequisite for
  the Windows suite; existing portable capture tests continue to own the
  platform-independent containment contract.
- Keep startup ownership unchanged: the application selects and captures config
  on its background executor, the host owns graph validation and phase
  evaluation, and the launch gate admits product windows only after pre-init,
  installed extensions, and post-init complete.
- Use a disposable absolute `XDG_CONFIG_HOME` for native fixtures so D029 never
  writes to the user's real personal-config directories. Windows known-folder
  APIs do not provide the same environment-based installed-extension isolation
  as Linux XDG data paths; leave the actual extension root untouched, record its
  discovered state in the evidence, and do not add a production root override
  solely for this smoke test.
- Use ordinary Windows Ctrl conventions for product commands. A temporary
  personal Ctrl-Shift-P binding may provide an unambiguous startup marker, but
  it must not hide failures in expected product defaults such as clipboard,
  Open, Save, or closure commands.
- Fix blockers found by the agreed smoke matrix when they belong to portable
  product behavior or the Windows platform path. Add durable coverage at the
  lowest meaningful model, lifecycle, command, persistence, or UI boundary.
  Record unrelated polish separately instead of expanding this slice.
- Keep the plan current while it is executed. Mark completed work and review
  gates with ✅, retain observed commands and evidence here, and split multiple
  implementation steps into logical commits.

## Explicit exclusions

- An installer, portable release archive, code signing, reputation or
  SmartScreen work, automatic updates, file associations, and Start menu or
  shell integration.
- Certification across Windows 10, Windows on Arm, remote desktop, multiple GPU
  vendors, display scaling combinations, input methods, or assistive
  technologies.
- A general redesign of platform directories, personal configuration,
  extension packaging, startup ordering, keymaps, terminal architecture, or
  rendering.
- Broad visual, font, terminal-compatibility, performance, or accessibility
  polish not required to complete the focused dogfooding workflow.
- Tests that require Developer Mode, administrator privileges, or mutation of
  the user's real profile, config, extension, or shell settings.

## Deferred decisions

- Choose a Windows distribution artifact and installation/update model only
  when Knot is ready to move beyond local dogfooding builds.
- Expand the supported Windows matrix after concrete compatibility reports or a
  release target justify CI and hardware coverage.
- Add a general profile/root override only if a product or repeatable testing
  need beyond D029 establishes its semantics; this plan does not introduce one
  merely to make the native smoke hermetic.

## Checkpoints

### 1. Windows config and source-capture contract

- ✅ Establish and record the Windows edition/build, Rust and Cargo versions,
  locked gpui and `directories` versions, graphics/session facts useful for
  reproduction, and the clean baseline build/test commands.
- ✅ Add Windows-only tests that call the real `BaseDirs`, `ProjectDirs`, and Knot
  config-root code. Assert the Windows native path shape and application-name
  casing, and cover unset, empty, relative, existing absolute, and missing
  absolute `XDG_CONFIG_HOME` behavior without depending on or altering the
  user's real `%USERPROFILE%\.config\knot` state.
- ✅ Cover precedence among controlled existing candidates, including an invalid
  preferred root producing a capture diagnostic rather than silently loading a
  lower-priority root.
- ✅ Exercise selection followed by source capture under a Windows temporary path
  with spaces or Unicode. Cover optional phases, a local import, immutable
  captured bytes, Windows file-URL resolution, malformed or missing imports,
  and an escaping import.
- ✅ Run the focused personal-config tests and any lower-level graph tests affected
  by corrections. Document exact observed directory values and any code changes
  required to preserve the validated cross-platform policy.
- **Review gate:** confirm the concrete Windows directory contract, precedence
  matrix, source-capture behavior, and focused regression coverage before native
  product testing.

Checkpoint 1 evidence (2026-10-09):

- The observed host reports Windows version `10.0.26200.9457`, display version
  `25H2`, and Professional edition. The registry still labels its `ProductName`
  `Windows 10 Pro`; the build and display version identify this Windows 11
  session. Primary display bounds are 2560 × 1440. `SESSIONNAME` is unset in
  the test shell. WMI/PnP graphics queries returned access denied, so the GPU
  and driver remain unrecorded for the later native smoke.
- `rustc 1.99.0 (b940084d7 2026-09-28)` and
  `cargo 1.99.0 (5f94df478 2026-08-27)`; lockfile versions are
  `gpui 0.2.2` and `directories 6.0.0`.
- The real Windows directories observed by the test are
  `BaseDirs::home_dir() = C:\Users\andre`,
  `BaseDirs::config_dir() = C:\Users\andre\AppData\Roaming`, and
  `ProjectDirs::from("", "", "Knot").config_dir() =
  C:\Users\andre\AppData\Roaming\Knot\config`. The test asserts the actual
  `USERPROFILE` and `APPDATA` values, the native path shape, and the `Knot`
  casing. No profile directory was created or changed.
- Subprocess tests cover unset, empty, relative, missing absolute, existing
  absolute, and invalid preferred `XDG_CONFIG_HOME`. Controlled temporary
  candidates verify native < `%USERPROFILE%\.config\knot` <
  `XDG_CONFIG_HOME\knot` precedence. An existing file at the preferred root
  produces a discovery diagnostic even when the lower root contains a valid
  pre-init file. A temporary Unicode path with spaces verifies both optional
  phases, `.mjs` import, immutable captured bytes, percent-encoded Windows file
  URL resolution, missing and malformed imports, and import escape rejection.
- `cargo test --locked app::personal_config::tests -- --nocapture`: 9 passed.
  `cargo build --locked --bin knot`: passed. Both emit linker warning LNK4098;
  the locked dependency set also reports a future-incompatibility warning for
  `proc-macro-error2 2.0.1`.
- `cargo test --locked --all-targets` provides the initial Windows baseline:
  439 passed and 3 failed. The failures are
  `app::entry::tests::launch_accepts_exactly_one_path_and_normalizes_it_at_the_boundary`,
  `app::keymaps::tests::declared_kinds_and_sequences_are_validated_and_normalized`,
  and `app::product::tests::process_exit_during_terminal_handoff_keeps_final_grid`.
  These Windows path, keymap, and terminal findings are tracked for the product
  regression closeout in checkpoint 3. No production config policy change was
  needed for checkpoint 1.

### 2. Native launch, config phases, and fatal diagnostics

- Build the locked product binary on Windows and add a small repeatable
  PowerShell smoke launcher if it can isolate personal config without modifying
  real user data. The launcher may create disposable config fixtures and report
  the real installed-extension root, but must not move, replace, or populate
  that root.
- Launch once with an empty isolated config and verify that a visible,
  input-accepting product window opens. Record the executable, process, selected
  paths, and relevant native environment.
- Launch with pre-init and post-init entries plus a local shared module. Make
  phase ordering and shared JavaScript state observable through registered
  commands, and use a temporary Ctrl-Shift-P binding to verify that applied
  foreground state is available when the product window opens.
- Exercise a malformed pre-init source and a post-init evaluation failure.
  Verify each launch shows only the dedicated configuration diagnostic, reports
  the selected Windows path and useful source location, copies the full
  diagnostic, exits through Quit, and never leaves startup lifecycles or product
  windows alive.
- Fix Windows launch, path, runtime, or lifecycle blockers within the selected
  boundaries and add automated regression coverage wherever native observation
  alone would not make the behavior durable.
- **Review gate:** user confirms the successful configured launch, physical
  shortcut, and diagnostic Copy/Quit behavior before the broader product smoke.

### 3. Windows dogfooding smoke and regression closeout

- In a disposable workspace, exercise text entry and navigation,
  representative Unicode rendering, selection and clipboard, command palette,
  Open, Save, Save As, dirty-close cancellation, and successful tab/window/app
  closure. Verify saved bytes and that cancellation preserves the edited view.
- Open a terminal, run a simple command, paste text, resize it, and close its
  tab and the application. Verify the process/session shuts down through the
  existing lifecycle and no child is orphaned.
- Check expected Windows Ctrl shortcuts directly. Personal-config bindings may
  expose fixture marker commands but must not replace product defaults during
  this portion of the smoke test.
- Correct in-scope blockers and cover each durable behavior at the lowest
  effective boundary, adding UI or integration coverage when the failure cannot
  be established below the native surface. Record any intermittent or deferred
  issue without treating an unexplained retry as a pass.
- Run final focused tests, `cargo test --locked --all-targets`,
  `cargo clippy --locked --all-targets -- -D warnings`, and
  `cargo build --locked --bin knot`. After all Rust edits, run `cargo fmt` once
  and check the final diff.
- Record the final Windows smoke matrix, machine/session details, commands,
  results, fixes, and remaining limitations in this plan. Update architecture
  references only if boundaries or behavioral contracts changed. Once the
  evidence validates Windows dogfooding, update `docs/decisions.md` and retire
  D029 from the deferred roadmap; archive the completed plan separately.
- **Review gate:** approve the observed Windows 11 dogfooding target and its
  regression coverage without broadening the claim to Windows distribution or
  general platform certification.
