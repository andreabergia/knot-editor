set default-list := true

# Format Rust sources
fmt:
    cargo fmt

# Lint all Rust targets
lint:
    cargo clippy --all-targets

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

# Format, lint, and test all Rust targets
check: fmt lint test
