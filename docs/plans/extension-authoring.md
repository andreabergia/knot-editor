# D056 — Extension authoring: TypeScript, modules, and tests

Status: 🚧 approved, in progress. Promotes D056 (TypeScript authoring for
bundled extensions) and widens it into one authoring system for every package:
bundled, installed, and examples.

## Problem

`@knot/projects` is one 1,050-line hand-written JavaScript file with JSDoc
types. Its behavior is covered only end to end by the gpui suites
(`src/app/product/tests/projects.rs`, `project_sessions.rs`): every policy case,
including name validation or binding rules, needs a full product shell, a V8
pool, and SQLite. Module-level mutable state (`registry`, `sessions`,
`bindings`, `launched`) and top-level `await` registration make it impossible to
load a piece of the package in isolation.

Nothing about this is specific to projects. The next bundled workflow (the
explorer, D053; cross-file search) would repeat it.

## Goals

- Write extensions in full TypeScript, with plain JavaScript staying valid for
  installed packages. Knot still runs only JavaScript; compilation happens at
  build time, never in the editor.
- One source layout, toolchain, and test harness shared by bundled packages,
  examples, and third-party authors.
- Fast JavaScript tests for extension policy, without a Knot build.
- Split `@knot/projects` into focused modules and cover its policy with them.

Non-goals: npm publishing of the tooling, source maps (D032), a bundler,
archives (D033), or changing the host's module rules.

## Design

### Package layout

```text
extensions/@knot/projects/
  knot.jsonc          "main": "dist/main.js" (unchanged)
  tsconfig.json       extends the shared base
  src/*.ts            sources; main.ts only calls start()
  test/*.test.ts      node:test suites
  dist/               build output, ignored by Git
```

The host already captures every `.js`/`.mjs` under an installed directory,
resolves static relative imports inside the package, and ignores `.ts`. So the
built `dist/` is an ordinary multi-module package, and an installed TypeScript
package is just its `knot.jsonc` plus `dist/`. tsc emits one `.js` per `.ts`
with no bundling, so stack traces stay readable without source maps.

### Compiler settings

A shared `tsconfig.base.json`: `strict`, `exactOptionalPropertyTypes`,
`noUncheckedIndexedAccess`, `verbatimModuleSyntax`, ES2022 modules and target,
`lib: ["ES2022"]` (no DOM). The full language is allowed. Sources import
siblings as `./registry.js`, the specifier that exists after emit.

### Building bundled packages

A `build.rs` builds every `extensions/@knot/*` package and embeds its output:

1. Install the tooling with `npm ci` when `scripts/extensions/node_modules` is
   missing or older than the lockfile.
2. Run the pinned `tsc -p <package>/tsconfig.json --outDir $OUT_DIR/<package>/dist`
   for each package, so the source tree is never written by Cargo.
3. Generate a table of `include_str!` entries for each `knot.jsonc` and every
   emitted `dist/` module, which `src/app/bundled.rs` includes in place of its
   hand-written list. Admission is unchanged.
4. Declare `rerun-if-changed` on package sources, tsconfigs, and the lockfile.

Node and npm become build requirements, and `build.rs` fails with a clear
message when they are missing. CI installs a pinned Node in every job that
builds Rust. On Windows the script invokes `npm.cmd`. Bundled sources stay
compiled into the binary, so development and release still load identical code
without packaging steps, as the
[bundled packages decision](../decisions.md#bundled-extension-packages)
requires.

### Shared tooling

`scripts/extension-api` becomes `scripts/extensions`, the single npm project
holding the TypeScript pin and lockfile, `tsconfig.base.json`, the contract
fixtures, and the test harness. Scripts:

| Command | Effect |
| --- | --- |
| `npm run check` | contract fixtures plus a no-emit check of every package and its tests |
| `npm test` | compile packages, tests, and the fake to a scratch directory, then `node --test` |
| `npm run build <dir>` | emit one package's `dist/` in place, for installing a TypeScript example |

`just ext` runs check and test; CI runs it in the existing extension step. The
Rust build compiles bundled packages itself, so `npm run build` is only for
installing packages by hand.

### Test harness: a fake `knot` module

Extension code imports `"knot"`. Tests run the compiled JavaScript under
`node --test` with a resolve hook, registered through `--import`, that maps the
specifier to the compiled `scripts/extensions/testing/knot.ts`, an in-memory
fake. Extension code stays idiomatic and needs no dependency injection.

The fake is typed against `knot.d.ts` (`satisfies typeof import("knot")` per
namespace), so a declaration change that the fake no longer matches fails
`npm run check`. It implements behavior, not mocks:

- `storage` and `recovery`: in-memory maps with a JSON round trip, so tests see
  the same value semantics as SQLite records.
- `windows`, `documents`, `workspaces`: a mutable model the test arranges
  (windows with roots, panes, tabs; documents with kind, dirty, text).
  `replaceWindows` applies a minimal commit and can be told to reject with a
  given `HostError` code.
- `workbench`: scripted answers for picker, input, confirmation, and folder
  pick; recorded messages; an `observe` that the test drives with events.
- `commands`: a registry plus `invoke`, running handlers serially like the
  host's FIFO roots.

Anything not implemented throws `not faked: <name>`, and the fake grows only
as tests need it. A `reset()` export clears it between tests.

Fresh state per test requires one convention for every package: **no mutable
module-level state and no top-level side effects except in `main.ts`**. A
package exports `start(): Promise<void>` that creates its state and registers
its contributions; `main.ts` is `await start()`. Tests call `reset()` then
`start()`.

### Test layers

| Layer | Runs | Covers |
| --- | --- | --- |
| Pure unit tests | node | Record parsing, root equality, binding decisions, session entries, tree items, messages |
| Fake-host tests | node + fake `knot` | Command flows, storage ordering, capture release order, switch retries, launch handling |
| Product tests | Rust/gpui + V8 | The package against the real host: commits, conflicts, recovery, restart, quit |

The fake does not reproduce host semantics; the Rust suites remain the
authority for anything crossing the boundary (`replaceWindows` revalidation,
durable recovery, restoration). A Rust test moves to JavaScript only when it
exercises package policy alone.

### Splitting `@knot/projects`

Policy becomes pure functions over plain data; one state object replaces the
module-level variables.

| Module | Contents |
| --- | --- |
| `errors.ts` | `describe`, `errorCode` |
| `registry.ts` | `Project`, `Registry`, parsing, name rules, lookups, labels, IDs (pure) |
| `session.ts` | `Session`, parsing incl. schema 1, captures, suspended references (pure) |
| `layout.ts` | Presented documents, view counts, roots equality, window entries and selection (pure) |
| `bindings.ts` | Pure binding plan (keep, unbind, bind, adopt) and its application |
| `state.ts` | `ProjectsState`, reload/commit, session writes and capture release |
| `tracking.ts` | Live tracking, launch handling, unbound suspension, reconciliation |
| `switch.ts` | `switchTo` and its retry loop |
| `tree.ts` | Tree items from state (pure) |
| `commands.ts` | Command registrations |
| `extension.ts` | `start()` |
| `main.ts` | `await start()` |

### Examples

No Rust test loads `examples/extensions`; they are type-checked and installed by
hand. Most predate real consumers:

| Example | Demonstrates | Proposal |
| --- | --- | --- |
| `application-state` | Observation and document/window/workspace calls dumped to the clipboard | Drop: `@knot/projects` is a real consumer of every call |
| `storage-demo` | Storage read/write/delete/list | Drop: covered by projects |
| `text-utils` + `word-tools` | `requires`, cross-package `commands.invoke`, a relative import | Drop: the manifest guide and Rust admission tests cover `requires`; projects covers imports |
| `git-status` | Grouped tree, badges, row/group/frame actions, text frame, transient prompts | Keep, convert to TypeScript with tests: the installed-package path and second harness consumer |

The contract fixtures under `scripts/extension-api/examples` are type tests, not
installed examples, and move with the tooling.

## Steps

1. ⬜ **Toolchain.** Move `scripts/extension-api` to `scripts/extensions`, add
   the base config, check/test/build scripts, and `just ext`. Add `build.rs`
   and the generated bundled table, ignore `dist/`, and install Node in CI.
   Convert projects mechanically to a single `src/main.ts` with no behavior
   change. Rust suites pass.
2. ⬜ **Examples.** Drop the obsolete examples, convert `git-status` to
   TypeScript, and update the install recipe to build it first.
3. ⬜ **Split projects** into the modules above with `start()` and one state
   object. No behavior change; Rust suites pass.
4. ⬜ **Fake `knot` and pure tests.** Add the resolve hook and fake, and pure
   tests for registry, session, layout, bindings, and tree.
5. ⬜ **Fake-host tests** for create, rename, remove (release only after both
   deletes), select, switch retry on `SessionConflict`, source reactivation on
   failure, launch suspension, and unreadable-record preservation, plus a
   `git-status` suite. Trim Rust cases that now duplicate pure policy coverage.
6. ⬜ **Docs.** Authoring and testing sections in the extension guide, the
   toolchain in `development.md`, the bundling note in the extension-host
   reference, and a decision entry (TypeScript authoring, build-time compilation,
   fake host). Remove this plan.
