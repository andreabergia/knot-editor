#!/usr/bin/env bash
set -euo pipefail

# Launch the Linux framework smoke with disposable personal configuration.
repo_root=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
smoke_root=${KNOT_LINUX_SMOKE_ROOT:-/tmp/knot-d028-smoke-ht6y53ey}
if [[ ! -x "$repo_root/target/debug/knot" ]]; then
  echo "Build the product first: cargo build --locked --bin knot" >&2
  exit 1
fi
mkdir -p "$smoke_root/home" "$smoke_root/config/knot" "$smoke_root/data/knot/extensions"
cat > "$smoke_root/config/knot/post-init.js" <<'JS'
import * as knot from 'knot';
knot.keybinding('ctrl-shift-p', 'workbench.show-command-palette');
knot.keybinding('ctrl-v', 'editor.paste');
JS
printf 'Smoke root: %s\n' "$smoke_root"
exec env HOME="$smoke_root/home" XDG_CONFIG_HOME="$smoke_root/config"   XDG_DATA_HOME="$smoke_root/data" "$repo_root/target/debug/knot"
