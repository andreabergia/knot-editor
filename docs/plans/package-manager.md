# Package manager

Status: 🚧 approved, not started. Promotes D033 from the [roadmap](../roadmap.md)
and adds D101–D106; D107, D108, and D116 stay in the deferred register.

## Goal

Make installing, updating, and removing extensions an inspectable operation of
the editor instead of a manual copy into the user extensions root: a lockfile
that records what is installed and where it came from, a local archive format,
installing from a git repository link, dependency versions that resolve
deterministically, a manager surface for install, update, and uninstall, and
rollback when an update is bad. Git links replace a package registry: anyone can
publish by pushing a repository, and discovery lives on a website rather than in
the editor. Consent stays with entitlement grants (D087); signing stays deferred
(D107).

## How this plan is executed

This plan is an index. Each item below is a brief, not a design, and this file
stays high level. Before implementing an item, analyze it in its own session and
write its design in a separate plan, `docs/plans/d0xx-<slug>.md`, however small
the item is. Link that plan from the item here, and never add design detail to
this file. Mark progress here with ✅ and 🚧. Remove each item's plan once it is
complete, and remove this plan once every item's durable behavior lives in the
architecture references and decisions.

Each item lists what it depends on. Items in the same phase are independent
unless their dependencies say otherwise.

Nothing that exists today is a constraint. Knot has no users, so the current
manifest schema, installed-directory layout, startup report, and commands may be
redesigned with no migration path. These briefs name current behavior only as a
starting point.

## Boundaries

- The application owns package state: inventory and lockfile, fetchers and
  unpacking, version resolution, and lifecycle stop and start. It is the only
  code that writes the user extensions root.
- Packages keep their current contract: built JavaScript plus `knot.jsonc`,
  canonical identity, API version checked before evaluation, dependency-driven
  startup order, and durable storage ownership by identity that survives update
  and reinstall.
- Git repositories must contain built JavaScript. There is no build-on-install:
  the manager never runs npm, a compiler, or a package's own scripts. A
  TypeScript package publishes its emitted sources, as bundled packages do.
- Bundled `@knot/...` packages and personal config are never managed here.
  Bundled identity stays structural and cannot be installed or replaced.
- One version of a package identity exists per application, as today. The
  lockfile is the source of truth; installed directories are materialized from
  it.
- Integrity is the pinned git commit for git sources and a content hash for
  archives. Consent is an explicit install confirmation plus the existing
  entitlement grant flow, which repeats when an update adds an entitlement.
  Installing a repository runs its code unreviewed; the confirmation says so.
- Activation reuses the existing terminating unload and restart machinery, so
  install, update, and uninstall take effect without restarting Knot.

## Exclusions

- No sandbox. Package code runs with the full public host API apart from denied
  entitlements; the decision record must say so, matching D087.
- No in-editor catalog, search, ratings, or publication flow. A separate website
  can list, vote on, and rank repositories; the editor only consumes links and
  opens them in a browser.
- No artifact server, upload, accounts, or download counts (D108).
- No signatures or publisher identity (D107), and no private repositories or
  stored git credentials in the first slices.
- No git history browsing beyond what a source record needs, no submodule
  recursion, and no monorepo-wide installation beyond naming one package
  directory (see D103).
- No JavaScript API for installing or updating packages; package operations stay
  application-owned.
- Grammar packages stay with D065. Project-local package sets stay with D089.

## Ordering rationale

- **Inventory before fetchers.** Installing over an unknown directory set cannot
  be transactional, so the lockfile and drift reconciliation come first.
- **Local archive before git.** Both materialize a package through the same
  staged install; proving the transaction on a file we already have keeps the
  first fetch from also being the first install.
- **Git before resolution.** A git link is the publishing mechanism, so a single
  package installs end to end before dependency requirements can pull transitively
  installed packages from their own sources.
- **Resolution before the surface and updates.** The surface must not invent
  resolution rules, and its errors are mostly resolution errors.
- **Updates last.** Update, pin, and rollback compose everything above, and
  rollback policy is only worth designing once install and start behavior are
  proven.

## Phase 1 — Local package management

- **D101 — Package inventory and local installs**
  - A lockfile under the user extensions root records, per installed identity,
    the exact version, source (directory, archive, or git URL plus commit),
    content hash where applicable, resolved dependencies, and install time,
    alongside the directory name it materialized. Discovery reconciles the
    directory set against the lockfile and reports drift (a package present
    without a record, a record without a package, a manifest version that does
    not match) as startup diagnostics without refusing to load.
  - Install from a local built package directory and uninstall by identity, both
    transactional: stage, validate the manifest and API version, swap, sync the
    parent directory, and restart that package's lifecycle. Storage records
    survive both.
  - Commands: Install Package from Directory…, Uninstall Package…, and an
    installed-packages report in the startup report's style.
  - Depends on D087 for the entitlement re-prompt path.
  - Validation: lockfile round trip, drift diagnostics, staged install rollback,
    uninstall stopping the lifecycle, storage preserved across reinstall.

- **D102 — Package archive format, integrity, and unpacking** (promotes D033)
  - A single-file archive containing `knot.jsonc`, built JavaScript, and license
    notice, for offline and out-of-band distribution. Unpacking refuses absolute
    paths, `..`, symlinks, hard links, and duplicate entries, and enforces a size
    ceiling. The archive's content hash is verified before install and recorded
    in the lockfile.
  - Install from archive reuses the D101 transaction. Authoring tooling in
    `scripts/extensions` produces an archive and prints its hash, so the format is
    exercised by the packages the repository already ships.
  - Depends on D101.
  - Validation: traversal and symlink rejection, hash mismatch rejection, an
    interrupted unpack leaving the previous install intact, packaged bundled
    packages passing the normal manifest validation.

## Phase 2 — Git sources and dependencies

- **D103 — Git repository package sources**
  - Install from a repository link: a full HTTPS URL or `owner/repo` shorthand,
    an optional git ref (tag or branch, default branch otherwise), and an
    optional package directory for repositories that hold more than one package.
    Resolution pins an exact commit in the lockfile, so reinstall and update are
    reproducible even when a branch moves. The repository's `knot.jsonc` supplies
    identity, API version, version, and dependencies like any installed package.
  - Clone into a cache, then install through the D101 transaction, so lifecycle
    restart, entitlement prompts, and storage ownership behave identically.
    Whether cloning uses a native git library or the installed `git` binary is
    deferred to the item's design; either way, a missing or failing tool reports
    a clear cause instead of a partial install. Public HTTPS only: private
    repositories, SSH, and stored credentials are deferred. Updates stay manual
    (D106) rather than following a branch at startup.
  - Depends on D101, whose transaction it reuses, and D102, which lands the
    unpacking and content-hash side of the same install path.
  - Validation: URL and shorthand parsing, ref and subdirectory resolution,
    commit pinning, a failed or unreachable clone leaving the installed package
    untouched, a missing git tool reported clearly, and repository content that
    is not a valid package failing without a partial install.

- **D104 — Dependency version constraints and resolution**
  - Manifests express dependency version requirements and may name a git source
    for a dependency, so installing one package can install its requirements
    transitively. Resolution produces one version per identity: pinned by the
    lockfile when it satisfies every requirement, otherwise the highest
    satisfying candidate from the declared sources. Bundled packages satisfy
    requirements without a source. Conflicts fail the requesting packages with a
    diagnostic naming both constraints, leaving independent packages loaded.
    Dependency cycles stay rejected, as today.
  - Depends on D101 and D103.
  - Validation: transitive install from declared sources, resolution choice and
    ordering, conflict diagnostics naming both constraints, dependent failure
    isolation, installed set unchanged when resolution fails.

## Phase 3 — Manager surface and lifecycle

- **D105 — Package manager surface**
  - A manager surface lists installed packages with identity, version, source,
    and pinned commit, and runs install from a git link or an archive, plus
    uninstall, showing progress and failures from the startup report. It
    re-prompts entitlements an update adds and explains why a package is
    unavailable (API version, dependency conflict, denied entitlement, bad
    source) instead of failing silently.
  - Build it as a bundled extension over a small application-owned package
    service, mirroring `@knot/projects`: the application keeps install and
    resolution ownership while the surface stays replaceable. Confirm that
    boundary during the item's design. There is no catalog to browse; a website
    link opens in the browser.
  - Depends on D101, D102, D103, D071 for the picker, and D064 for the surface's
    colors.
  - Validation: `node:test` coverage of the surface's policy over the fake host,
    and product tests for install-then-load, uninstall-then-stop, and the
    entitlement prompt.

- **D106 — Updates, pinning, and rollback**
  - Check for updates by fetching each package's tracked ref and comparing it
    with the pinned commit. Update one or all packages explicitly, and pin a
    package to a ref or commit so updates skip it. Keep the previous commit
    materialized beside the current install: a package that fails to start after
    an update is rolled back and reported with both versions, and storage records
    carry over.
  - Depends on D103, D104, and D105.
  - Validation: update transaction and pin behavior, automatic rollback of a
    package that fails to start, storage preserved across versions, the retained
    previous commit removed only after a successful start, and a disappeared
    commit (force-push) reported without losing the installed copy.

## Deferred decisions

- **Signing.** D107 covers keys, a signature format, verified publishers, and
  how a source can vouch for content. Reconsider when installs are common enough
  that "trust this URL" is not a sufficient story.
- **Hosted registry.** D108 covers an artifact index, upload and publication,
  accounts, and in-editor search. Reconsider if git installs prove too risky or
  awkward, or if discoverability in the editor is worth owning a server.
- **Git transport implementation.** A native git library, a bundled binary, or
  the installed `git` binary are all viable; pick one during D103's design and
  record the packaging, build, and error-reporting cost.
- **Private repositories and other sources.** D116 covers credentials, SSH
  keys, a platform keychain, filesystem-path sources, and per-project package
  sets; all are separate consent and storage problems.
- **Public package API.** Whether a third-party extension may install or update
  packages stays closed. Reconsider only with a concrete automation workflow,
  and only with an entitlement that gates it.

## Shared code seams

- `src/app/extensions/package.rs` and `src/app/extensions/package/` — manifest,
  discovery, user extensions root; gain lockfile records, source records,
  archive unpacking, and git fetching. Network and clone work runs on the
  background executor, never on the foreground thread.
- `src/app/extensions/load.rs` — dependency planning and startup outcomes; gains
  version-constrained resolution and drift diagnostics.
- `src/app/extensions/host.rs` and `host/` — existing per-package restart and
  unload are the activation path for install, update, and uninstall.
- `src/app/extensions/storage.rs` — durable ownership by identity that must
  survive replaces; the lockfile is an inspectable JSON file in the user
  extensions root, not SQLite.
- `src/app/product/commands.rs` and `src/app/commands/` — package manager
  commands and the transient picker used by the startup report and restart.
- `extensions/@knot/` — the bundled manager surface alongside
  `@knot/projects`; `scripts/extensions` gains archive production.
- `src/app/extensions/host/handlers.rs` and `src/host/js/knot.d.ts` — only the
  surface's calls into the application-owned package service.
