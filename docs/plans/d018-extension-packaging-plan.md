# D018: Installable extension directories

Status: checkpoints 1 and 2 implemented; checkpoint 2 awaits its review gate.

## Outcome and extension shape

A user can place a built JavaScript extension directory in Knot's per-user
extensions location, restart Knot, and use its registered commands and semantic
providers. Each installed extension has one persistent V8 lifecycle. Knot loads
extensions eagerly in dependency order and reports invalid or failed packages
without preventing independent extensions from loading.

```text
extensions/
  @example/
    word-tools/
      knot.jsonc
      dist/main.js
      dist/commands.js
```

```jsonc
{
  "name": "@example/word-tools",
  "displayName": "Word Tools",
  "description": "Commands for editing words and paragraphs.",
  "version": "1.0.0",
  "author": "Example",
  "website": "https://example.com/word-tools",
  "license": "MIT",
  "main": "dist/main.js",
  "requires": ["@example/text-utils"]
}
```

`name`, `version`, and `main` are required. `requires` defaults to empty;
descriptive metadata is optional. `name` is a canonical `@scope/package` ID,
independent of the descriptive `author` field. The package directory follows
that ID. Knot reserves the `@knot` scope and rejects duplicate IDs. Required
extensions are referenced by canonical name only, without version constraints.
Registration and invocation continue to use explicit command names in the
existing global command namespace; package identity adds no implicit prefix.

An extension imports `knot:editor` and files inside its own directory. A
dependency guarantees that the required extension has loaded successfully
before the dependent entry module runs; it does not expose the dependency's
JavaScript modules. Knot does not resolve third-party packages or install
JavaScript libraries. Authors supply built JavaScript, which may contain local
modules. Knot reports generated file locations for failures; source maps are
deferred to D032. Archive installation and unpacking are deferred to D033.

## Checkpoints

### 1. Manifest and installed-directory contract ✅

- Define the JSONC schema, canonical name validation, metadata validation, and
  one platform-appropriate per-user extensions root with an injectable root for
  tests. Define how malformed manifests and directory/name mismatches appear
  in diagnostics. Reject duplicate package IDs and reserved scope use.
- Discover only package directories at `@scope/package` depth. Validate the
  entry path and all local module paths remain within that package, including
  symlink resolution. Treat each package directory as an immutable source graph
  for one load attempt.
- Test JSONC comments, required and optional fields, duplicate names, invalid
  paths, symlink escape, and discovery from temporary directories.
- **Review gate:** approve the manifest example, install layout, error model,
  and path boundary before changing V8 loading.

Checkpoint 1 contract: [`knot.schema.json`](../schemas/knot.schema.json) defines
the manifest fields. The application discovers `@scope/package/knot.jsonc` under
the platform's local application-data `Knot/extensions` directory, with the root
passed explicitly for tests. Package IDs use lowercase ASCII components and
reserve `@knot`; versions use SemVer. Unknown fields, empty descriptive values,
and non-HTTP(S) websites are invalid. Missing roots contain no packages.
Discovery returns valid packages plus per-directory diagnostics containing an
optional parsed package name and a cause. Duplicate IDs invalidate every
candidate with that ID; directory/name mismatches are reported separately.
The source graph captures all `.js` and `.mjs` files at discovery. Main and
local source files must resolve inside the package. File symlinks may resolve within it;
symlinked scope/package directories and symlinked subdirectories are rejected.

### 2. Dependency planning and load outcomes ✅

- Build a deterministic dependency graph from discovered manifests. Reject a
  missing required extension, self-dependency, and cycles with an actionable
  diagnostic. Propagate a dependency's startup failure to its dependents while
  continuing to load unrelated packages.
- Order ready packages deterministically. Consider an extension loaded only
  after its entry module completes successfully; failure or cancellation
  releases any registrations through the existing lifecycle teardown.
- Test ordering, missing dependencies, cycles, duplicate declarations,
  independent-package progress, and failed dependency propagation.
- **Review gate:** inspect the ordered load report and failure behavior before
  product startup uses it.

Checkpoint 2 contract: discovery diagnostics appear first, sorted by directory.
Missing dependencies, self-dependencies, and cycle members are then reported
by package ID. The runner attempts the lexicographically first ready package
at each step and waits for its entry completion before marking it loaded.
Startup failure propagates to dependents with the failed package and cause;
unrelated packages continue. Every attempt owns a rollback guard until entry
completion succeeds. Failure, cancellation, or unwinding invokes the guard,
which the product loader will connect to the existing exact-once lifecycle
teardown. Duplicate `requires` declarations fail manifest validation.

### 3. Package module graph in the V8 host ⬜

- Replace the fixture-only source-registration path with an API that accepts
  one validated, immutable package module graph and entry specifier. Keep V8,
  compiled modules, and JavaScript values inside `host`; pass Knot-owned source
  and diagnostic data across the boundary.
- Permit only `knot:editor` and extension-local relative imports. Reject bare
  imports, imports into other extension directories, unavailable files,
  private bootstrap imports, and dynamic imports. Preserve generated source
  URL, line, and column
  in compilation and runtime errors.
- Test multi-file static imports, top-level await, rejected imports, source
  locations, unload, and startup rollback in the host without gpui.
- **Review gate:** inspect module isolation and error reporting before wiring
  filesystem packages into the application.

### 4. Product startup and usable extension ⬜

- Discover, validate, plan, and eagerly load installed directories when the
  product starts. Run filesystem work away from gpui's foreground thread and
  register capabilities through the existing application-owned bridges.
  Keep `--fixture` as a diagnostic path using the same host load API.
- Provide a visible, inspectable startup report for loaded and failed packages
  with package name and cause; failures must not silently disappear. Exercise
  one real on-disk extension with a local import and a dependency whose command
  it invokes.
- Test the product lifecycle, dependent command availability, failure
  isolation, registration cleanup, and a restart after changing installed
  directories. Run focused host/application tests and the applicable full
  suite. Update architecture and decisions for the resulting boundaries and
  rationale.
- **Review gate:** use the on-disk extension in Knot and review the startup
  report, test results, and docs before marking D018 complete.

## Execution notes

- Mark each checkpoint ✅ as it lands, record material changes to this plan,
  and make logical commits between review gates.
- Run `cargo fmt` once at the end of Rust work, before the final tests and
  commits. Source-map interpretation (D032), archive workflows (D033), runtime
  quotas (D019), version constraints, package registries, and lazy activation
  are outside this slice.
