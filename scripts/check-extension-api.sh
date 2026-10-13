#!/usr/bin/env sh
set -eu
cd "$(dirname "$0")/extension-api"
npm ci --ignore-scripts --no-audit --no-fund
npm run check
