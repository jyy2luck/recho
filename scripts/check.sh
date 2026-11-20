#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."
cargo fmt --all --check
cargo build --workspace --locked
# Warm the same interpreter used by engine hooks before their execution deadlines.
/usr/bin/python3 -c 'import base64, contextlib, email.policy, http.client, json, traceback, types, urllib.parse, uuid'
cargo test --workspace --locked
node --test engine/server.test.mjs engine/extensions.test.mjs engine/protocol-editors.test.mjs
