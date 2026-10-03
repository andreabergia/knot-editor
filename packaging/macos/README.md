# Local macOS app

From the repository root, run `scripts/build-macos-app.sh` for a debug build or
`scripts/build-macos-app.sh --release` for a release build. The output is
`target/debug/Knot.app` or `target/release/Knot.app` respectively. If
`CARGO_TARGET_DIR` is set, the app is created there instead.

Launch the bundle with `open target/debug/Knot.app`. Open a file with
`open -a target/debug/Knot.app path/to/file.txt`. The bundle is for local use;
distribution is tracked in D035 of [the roadmap](../../docs/roadmap.md).
