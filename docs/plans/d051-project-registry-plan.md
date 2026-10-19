# Bundled JavaScript project registry

Status: ⬜ In progress. 6.1 implemented and awaiting its review gate; 6.2 and
6.3 remain unstarted. Owns detailed execution for checkpoint 6 of the
[extension platform and projects plan](extension-platform-and-projects-plan.md).
Work identifier D051.

## Outcome

Knot ships a bundled JavaScript extension, `@knot/projects`, that manages named
projects. Each project has a stable ID, a name, and an ordered roots array
(one root in this checkpoint). Users can create, list, select, rename, and
remove projects from a contributed **Projects** tree and palette commands.
Selecting a project opens its root in the originating window. Missing roots,
storage failures, and malformed records produce actionable errors through the
contributed native UI.

The platform outcome is equally important: bundled packages become a reusable
loading capability through the existing loader, and the folder-selection gap
exposed by building the registry on public APIs is closed with a small public
capability. Root selection is the ceiling here; durable session switching and
dirty-content suspension belong to checkpoint 8.

## Chosen design and boundaries

### Bundled packages are first-class native packages

- A bundled-source registry (`src/app/bundled.rs`) maps each built-in package to
  its canonical ID, manifest text, and immutable source map via `include_str!`.
  Bundled sources live under `extensions/@knot/projects/` and mirror the
  installed-package layout.
- Discovery merges bundled packages into the existing `DiscoveryReport`.
  `InstalledPackage` carries a package origin (installed directory vs bundled)
  and the module root used for its `ModuleGraph`; bundled packages use a stable
  synthetic `file:///knot/bundled/<id>/` root instead of a filesystem path.
- Bundled packages use the same manifest validation, `apiVersion` admission,
  dependency planning/order, startup report, and rollback as installed packages.
  Installed packages may `requires` a bundled ID; bundled packages may require
  each other. `version` stays metadata and `requires` stays name-based.
- Collision policy is structural: `@knot` remains reserved for built-ins.
  Installed `@knot/*` directories stay rejected during discovery, so no
  installed package can claim a bundled ID and no precedence rule is needed.
  Bundled entries always appear in the startup report.
- Bundled load failure is non-fatal: it records a startup-report failure and
  dependents fail through the existing rules, while product startup and native
  recovery continue. Nothing in the product depends on the project manager.
- Storage identity accepts application-supplied built-in `@knot/...` owner names
  while discovery continues rejecting installed `@knot/*`. Bundled records use
  the normal canonical-identity database naming (`packages/@knot/projects.sqlite3`).

### Folder selection becomes a public capability

- New `workspaces.pickFolder(options?)` resolves
  `{ kind: "selected"; uri }` with a provider-normalized directory URI, or
  `{ kind: "dismissed" }`. It uses an explicit `WindowTarget` or the admitting
  command's window and rejects `ContextUnavailable` without either.
- It reuses the native path prompt and the existing one-transient-interaction
  per-window ownership, cancellation, and window-closure rules. Selected paths
  are normalized through the registered providers before returning; files reject
  as wrong kind and unreadable/missing/permission failures reject with typed host
  errors instead of returning a URI.

### Registry behavior uses only public APIs

- One `projects` storage record per owner:
  `{ schema: 1, projects: [{ id, name, roots: [uri], createdAt, lastOpenedAt? }],
  recentProjectId }`. Related state lives in one record for atomic replacement.
- Project IDs are opaque stable strings generated in JavaScript. No crypto
  global is guaranteed in the isolate, so IDs come from a timestamp plus random
  suffix and are never derived from name or root.
- Commands create, select, rename, remove, and list projects (final names set at
  implementation). The contributed **Projects** tree shows one row per project
  with its root as description and the active project marked; row actions rename
  and remove, toolbar actions create and refresh, and row activation selects.
- Active derivation: a project is active for a window when its recorded root
  matches that window's workspace root exactly. Single-root matching is exact
  now; overlapping, duplicate, and multi-root rules belong to checkpoint 9. The
  last selection is remembered as `recentProjectId`. No per-window bindings are
  persisted in this checkpoint.
- Selecting captures the originating window and calls
  `workspaces.openFolder(uri, { target })`. Rejections are reported with
  `workbench.showMessage` and leave the window unchanged.
- Creating picks a folder with `workspaces.pickFolder`, collects a name with
  `workbench.showInput`, writes the record, refreshes the tree, and selects in
  the originating window. Removing confirms first and removes only the project
  record; it does not close documents or clear window roots, because those
  semantics belong to checkpoint 8.
- Storage failures, malformed or legacy records, duplicate names, missing roots,
  and wrong-kind roots are actionable: the previous in-memory state is retained
  and the failure is reported. Concurrent edits of the single record from
  several windows are last-writer-wins and are documented as such.

## Explicit exclusions

- Durable session switching, suspension of unsaved documents, retained
  captures, per-project sessions, and persisted window bindings: checkpoints 7,
  8, and D050.
- Multi-root behavior, overlapping/duplicate root policy, and root
  add/remove/reorder: checkpoint 9.
- Layout, view-state, and startup restoration: checkpoint 10.
- Explorer migration (D053) and per-bundled-workflow user disable/replace: no
  settings system exists, so both stay deferred.
- No private host bypass. The registry uses only supported public contracts, and
  the `knot.` contribution namespace stays reserved for bundled packages.

## Deferred decisions

- Whether one project may be active in several windows (checkpoint 8).
- Root matching and ownership rules once several or overlapping roots exist
  (checkpoint 9).
- Whether bundled workflows become individually disableable or replaceable
  (settings system, deferred).
- Whether `pickFolder` grows into a general resource picker.

## Checkpoints and review gates

### 6.1 Bundled package loading and storage identity ⬜ awaiting review

- ✅ Compile-in registry (`src/app/bundled.rs`) merged into product discovery via
  `extension_package::discover_all`; `InstalledPackage` carries
  `PackageOrigin` and exposes a module root (installed directory or synthetic
  `file:///knot/bundled/%40knot/<name>/`).
- ✅ Bundled admission reuses installed manifest, `apiVersion`, version, and
  source validation while allowing only the reserved `@knot` scope; installed
  packages still reject `@knot/*` but may now `requires` a bundled identity.
- ✅ Product discovery resolves an installed directory that claims a bundled
  identity into a directory-level conflict diagnostic, so the identity stays
  available and `DependencyPlan` keeps its unconditional named-diagnostic rule.
- ✅ `StorageOwner::bundled` accepts application-supplied built-in identities
  while `StorageOwner::package` keeps rejecting the reserved scope.
- ✅ `@knot/projects` ships as `extensions/@knot/projects/knot.jsonc` plus an
  activation stub; product startup discovers it together with installed packages.
- **Validation:** discovery merge and conflict reporting, installed-identity
  rejection, reserved-scope ownership, bundled admission errors, installed
  packages ordering after a required bundled package, storage owner path, and a
  product startup test confirming the bundled package loads. `cargo fmt`, clippy,
  and focused tests pass.
- **Review gate:** approve the bundled origin, canonical identity, collision
  policy, and storage-owner boundary before building on them.

### 6.2 `workspaces.pickFolder` public capability ⬜

- Add the native host operation, declarations, target and cancellation
  semantics, provider normalization, TypeScript contract, and guide entry.
- **Validation:** handler tests with a fixed dialog for selected, dismissed, and
  failed prompts; normalization to a canonical directory URI; wrong-kind and
  stale-target rejection; checked example in `check-extension-api.sh`; a product
  test where a command picks a folder.
- **Review gate:** approve the public name, namespace, result union, and target
  rules before the registry consumes them.

### 6.3 Project registry extension and dogfood ⬜

- Implement `@knot/projects` behavior, storage schema, commands, tree
  contribution, active derivation, and error surfacing using only public APIs.
- Include bundled sources in the extension API check.
- **Validation:** real bundled package through the product host covering
  create/list/select/rename/remove, restart persistence, missing-root and
  storage-failure handling, independent windows using captured context, and
  cleanup of contributions when the package fails. Ensure existing native
  product flows and multi-window behavior regress cleanly.
- **Review gate:** dogfood the registry, approve the extension/platform
  boundary, and record the API limitations and coupling fixes this checkpoint
  exposed (folder selection, built-in storage identity, bundled module root) in
  the current architecture references and decisions.

## Documentation updates on landing

- [Extension host](../architecture/extension-host.md): bundled admission,
  origin/module root, and collision policy.
- [Documents and persistence](../architecture/documents-and-persistence.md) and
  [workbench lifecycle](../architecture/workbench-and-lifecycle.md) as needed
  for `pickFolder`; public contract in
  [knot.d.ts](../../src/host/js/knot.d.ts).
- [Extension API guide](../extension-api.md): folder selection and a pointer to
  the bundled registry as a worked example.
- [Decisions](../decisions.md): bundled loading, built-in storage identity, and
  the `pickFolder` boundary once validated.
- Parent plan: mark checkpoint 6 execution notes and reduce detail to links.

## Follow-ups captured, not fixed here

- No API reports or sets the active project in the window title.
- Concurrent registry edits from several windows are last-writer-wins.
