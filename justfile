set default-list := true

# Format Rust sources
fmt:
    cargo fmt

# Lint all Rust targets
lint:
    cargo clippy --all-targets -- -D warnings

# Run all Rust tests
test:
    cargo test --all-targets

# Run Knot
run:
    cargo run --bin knot

# Build a local macOS app bundle
app:
    scripts/build-macos-app.sh

# Build a release macOS app bundle
app-release:
    scripts/build-macos-app.sh --release

# Type-check and test extension packages
ext:
    scripts/check-extensions.sh

# Format, lint, and test all Rust targets and extension packages
check: fmt lint test ext

# Builds the example extensions and copies them into the macOS extensions directory
install-example-apps-mac:
    npm --prefix scripts/extensions ci --ignore-scripts --no-audit --no-fund
    npm --prefix scripts/extensions run build examples/extensions/@example/git-status
    rm -rf ~/Library/Application\ Support/Knot/extensions/@example
    mkdir -p "$HOME/Library/Application Support/Knot/extensions"
    cp -av examples/extensions/@example "$HOME/Library/Application Support/Knot/extensions"
