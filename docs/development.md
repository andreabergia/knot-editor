# Development and CI

The repository pins the Rust version in
[rust-toolchain.toml](../rust-toolchain.toml). Rustup selects this version for
local commands and CI, installing it when it is not already available. The
minimal profile includes the compiler and Cargo; Clippy and rustfmt are added
explicitly.

Run `just check` for local formatting, Clippy, and tests. Run `just app` to build
the macOS app bundle locally. GitHub Actions does not build or test on macOS.

The [CI workflow](../.github/workflows/ci.yml) runs on pull requests and pushes
to `main`:

- The Rust job uses Ubuntu 24.04. It checks that formatting produces no diff,
  runs Clippy with warnings as errors, and tests all Rust targets. Clippy and
  tests use the committed `Cargo.lock` through `--locked`.
- The audit job scans `Cargo.lock` with `cargo-audit` for known RustSec
  advisories. It also runs weekly so newly published advisories can be found
  without a dependency change.

Both jobs select the pinned toolchain before restoring their Rust caches. The
Rust job caches downloaded crates and compiled dependencies; the audit job
caches Cargo downloads and the `cargo-audit` executable. Pull requests restore
caches, while pushes and scheduled runs can save them. A newer run for the same
branch cancels an older run.

The workflow has read-only repository permissions and pins external actions to
commits. [Dependabot](../.github/dependabot.yml) checks for action updates
weekly.

The D046 draft extension contract has a separate authoring check:
`scripts/check-extension-api.sh`. With Node/npm installed, it uses `npm ci` and a
lockfile-pinned TypeScript compiler to check declarations, installed JavaScript
examples, draft samples and positive/negative type fixtures without emitting code.
It does not use a global `tsc`. Runtime contracts still require Rust/V8 tests;
see the [draft guide](extension-api.md) for the current audit and implementation
status. CI integration follows in D046 checkpoint 4.
