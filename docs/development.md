# Development and CI

The repository pins the Rust version in
[rust-toolchain.toml](../rust-toolchain.toml). Rustup selects this version for
local commands and CI, installing it when it is not already available. The
minimal profile includes the compiler and Cargo; Clippy and rustfmt are added
explicitly.

Run `just check` for local formatting, Clippy, and Rust and extension tests. Run `just app` to build
the macOS app bundle locally.

The [CI workflow](../.github/workflows/ci.yml) runs on pull requests and pushes
to `main`:

- The Rust job uses Ubuntu 24.04. It checks that formatting produces no diff,
  runs Clippy with warnings as errors, and tests all Rust targets. Clippy and
  tests use the committed `Cargo.lock` through `--locked`.
- The handler-platforms job runs on macOS and Windows. It tests handler
  declarations, linked inventory discovery, and doc tests, then builds the
  product binary. It does not run the full suite on those platforms.
- The audit job scans `Cargo.lock` with `cargo-audit` for known RustSec
  advisories. It also runs weekly so newly published advisories can be found
  without a dependency change.

Every job selects the pinned toolchain before restoring its Rust cache. The
Rust jobs cache downloaded crates and compiled dependencies; the audit job
caches Cargo downloads and the `cargo-audit` executable. Pull requests restore
caches, while pushes and scheduled runs can save them. A newer run for the same
branch cancels an older run.

The workflow has read-only repository permissions and pins external actions to
commits. [Dependabot](../.github/dependabot.yml) checks for action updates
weekly.

Building Knot requires Node.js and npm. `build.rs` installs the lockfile-pinned
TypeScript compiler in `scripts/extensions` when it is missing or older than the
lockfile, compiles every bundled package under `extensions/@knot` into Cargo's
output directory, and embeds the emitted modules. It also compiles the TypeScript
examples for the product tests that install them. CI installs a pinned Node in
every job that builds Rust.

`scripts/check-extensions.sh`, run by `just ext` and in CI, uses `npm ci` to check
declarations, every TypeScript package and its tests, authoring samples, and
positive/negative type fixtures, then runs the package tests under `node:test`
with an in-memory host (see the [extension guide](extension-api.md#write-and-test-in-typescript)).
It does not use a global `tsc`. Runtime contracts still require Rust/V8 tests.

## Manual terminal fixtures

Two scripts exercise terminal behavior that automated tests cannot judge. Run
them in a Knot terminal tab:

- `scripts/terminal-links-fixture.py` prints detected, wrapped, Unicode, and
  OSC 8 links for checking hover, Command-click/Control-click opening, and
  unsupported destinations.
- `scripts/terminal-mouse-fixture.py` enables mouse reporting and prints each
  decoded press, release, motion, and wheel report, for checking cell
  coordinates over wide and combining text and Shift-held local selection.
  `--tracking`, `--encoding`, and `--alternate` select the reporting mode,
  report encoding, and alternate screen.
