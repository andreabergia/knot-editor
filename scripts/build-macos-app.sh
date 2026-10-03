#!/usr/bin/env bash
set -euo pipefail

if [[ "$(uname -s)" != "Darwin" ]]; then
    echo "Knot.app can only be built on macOS" >&2
    exit 1
fi

profile=debug
case "${1:-}" in
    "") ;;
    --release)
        profile=release
        shift
        ;;
    *)
        echo "usage: scripts/build-macos-app.sh [--release]" >&2
        exit 2
        ;;
esac
if [[ $# -ne 0 ]]; then
    echo "usage: scripts/build-macos-app.sh [--release]" >&2
    exit 2
fi

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$repo_root"
if [[ "$profile" == release ]]; then
    cargo build --release --bin knot
else
    cargo build --bin knot
fi

target_dir="${CARGO_TARGET_DIR:-$repo_root/target}"
app="$target_dir/$profile/Knot.app"
mkdir -p "$app/Contents/MacOS" "$app/Contents/Resources"
install -m 755 "$target_dir/$profile/knot" "$app/Contents/MacOS/knot"
install -m 644 packaging/macos/Info.plist "$app/Contents/Info.plist"
install -m 644 assets/macos/knot.icns "$app/Contents/Resources/knot.icns"

echo "$app"
