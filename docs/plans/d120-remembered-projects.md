# D120 — Remembered projects

Status: 🚧 approved, not started. Item of the
[daily-driver plan](daily-driver.md); depends on D070 (settings) and D084
(project selector modal).

## Goal

Every folder set a user opens is remembered as a project, as in VS Code and
Zed, without asking for a name. Reopening it, from the project picker, from
Open Folder, or from the command line, brings back its windows, tabs, layouts,
and unsaved text. Naming a project becomes optional labelling rather than the
way to opt in.

## Design

- **Inferred names.** A project's stored name is `string | null`. `null` means
  the name is derived from the roots when displayed: the first root's folder
  name, plus `+N` for further roots (today's `rootsLabel`). Renaming sets an
  explicit name; renaming to an empty string returns to the inferred one.
  Inferred names follow root edits without a write.
- **Names are not unique.** The uniqueness check and lookup by name go away.
  The D084 picker identifies projects by ID and shows the roots as the
  description, so two folders called `app` are distinguishable by path. The
  Projects tree, whose row activation resolves projects by label, is already
  gone with D084.
- **Automatic creation.** After launch, a window whose non-empty root set
  matches no project creates an unnamed project with an active session and
  binds to it. Windows with no roots never create one. `projects.create`
  disappears: Open Folder is how a project starts.
- **Pruning.** A `projects.rememberedLimit` setting (D070), defined by the
  package, bounds unnamed projects, defaulting to 30, by most recent
  `lastOpenedAt`. Pruning runs when a project is created. It never removes a
  named project, an active project, or a project whose suspended session holds
  retained captures; a pruned project's session record is deleted with it.
- **Open Folder opens a new window** unless the current window has no roots,
  as in Zed. Root changes inside a window then come only from Add or Remove
  Folder, so the existing rule that a bound project adopts its window's edited
  roots stays correct and never turns one remembered folder into another.
  Switching a window in place remains the picker's job, which suspends and
  restores through `switchTo`. `workspaces.openFolder` keeps its explicit
  replace semantics for extensions.
- **Opening a suspended project's folder restores it.** When an unbound
  window's roots equal a suspended project's roots, tracking switches that
  window to the project with the existing `switchTo`: a reusable window is
  replaced, otherwise the project opens in a new window. This also applies at
  `launched` to launch-request windows, after restored windows have rebound.
  Membership still requires an active session, so tracking never overwrites a
  suspended record; restoring goes through the switch, which owns captures.

## Exclusions

- Inferring a project from an opened file's VCS root (D123).
- Project-local settings (D089) and a project file in the repository: records
  stay in extension storage.
- A start surface listing recent projects (D061).

## Deferred decisions

- Whether pruning also considers age, not only count. Count alone until daily
  use says otherwise.

## Checkpoints

### 1. Open Folder opens a new window

- Deliverables: the native Open Folder command opens the chosen folder in a
  new product window when the target window has any root, and replaces the
  roots of a window without roots. Launch requests keep their own window.
- Validation: gpui product tests for both branches, including that the
  original window keeps its roots and tabs.
- **Implementation context:** the folder branch of the open dialog in
  `src/app/product/open.rs` (`start_open_dialog_with_options` and the
  directory result path); window creation mirrors the launch path in
  `src/app/product/launch.rs`. `workspaces.openFolder` in
  `src/app/extensions/host/handlers.rs` is unchanged. Update the multi-root
  decision in `docs/decisions.md`, which says Folder Open replaces every root,
  and the workspaces section of
  `docs/architecture/documents-and-persistence.md`.
- Review gate: confirm the window choice feels right before the projects
  package relies on it.

### 2. Unnamed projects and inferred names

- Deliverables: registry schema 2 with nullable names (no migration; an older
  record is reset like any unsupported schema), a display-name function used
  by the picker and every report message, rename-to-empty restoring the
  inferred name, and no uniqueness rule. `projects.create` is removed.
- Validation: `node:test` registry and command tests for inferred names,
  multi-root labels, renaming in both directions, and duplicate names.
- **Implementation context:** `extensions/@knot/projects/src/registry.ts`
  (`Project`, `registryFromJson`, `rootsLabel`, `nameTaken`, `projectByName`),
  `commands.ts` (rename, remove, the D084 picker), `switch.ts` and
  `report.ts` messages that interpolate `project.name`. Tests in
  `extensions/@knot/projects/test/`.
- Review gate: names read well in the picker for one and several roots.

### 3. Automatic creation and pruning

- Deliverables: tracking creates and binds an unnamed active project for an
  unbound window with an unknown non-empty root set, after launch and for
  windows present at `launched`; the `projects.rememberedLimit` setting and
  pruning on creation with the protections above.
- Validation: `node:test` tracking tests for creation on Open Folder, on
  launch, no creation for empty roots, and pruning that skips named, active,
  and capture-holding projects. A product test covers creation through a
  real window open, if the fake host cannot express the settings definition.
- **Implementation context:** `tracking.ts` (`track`, `afterLaunch`,
  `suspendUnbound`) and `bindings.ts` (`syncBindings`, which binds unbound
  windows to active projects); `state.ts` for `commit` and `writeSession`;
  `session.ts` for an initial active session. The setting uses D070's
  `settings.define` and `settings.get` from the package's `start()` in
  `extension.ts`. Extend the fake host in `scripts/extensions/` only as far
  as these calls need.
- Review gate: the registry stays small in practice and nothing with unsaved
  text is ever pruned.

### 4. Restoring on a hand-opened folder

- Deliverables: an unbound window matching a suspended project switches to
  it, both during tracking and at `launched` after restored windows rebind.
- Validation: `node:test` switch and tracking tests for a clean reusable
  window being replaced, a window with other content opening the project in a
  new window, retained text coming back, and a project with an unreadable
  session record staying untouched. A product test opens a suspended
  project's folder with Open Folder and sees its tabs return.
- **Implementation context:** call `switchTo` in `switch.ts` from `track` and
  `afterLaunch` in `tracking.ts`; `isReusable` in `layout.ts` already decides
  window reuse. Commands and listeners are serial roots, so the switch cannot
  interleave with tracking.
- Review gate: restoring never loses text and never fires for a window that
  is already bound.

### 5. Documentation

- Deliverables: the bundled project registry and project sessions decisions
  updated for unnamed projects, inferred names, automatic creation, pruning,
  restore on open, and the reason Open Folder opens a new window; the
  extension guide's project section and the projects package's module comment
  in `extension.ts` match the new behavior.
- Review gate: remove this plan and mark D120 ✅ in the daily-driver plan.
