#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."
cargo build --release --locked -p recho
if [[ "$(uname -s)" == Darwin ]]; then
  python3 scripts/bundle-macos.py
  codesign --force --sign - app/build/release/recho.app
  echo 'Built: app/build/release/recho.app'
else
  echo 'Built: app/build/release/recho'
fi
