#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."
cargo fmt --all --check
cargo test --workspace --locked
cargo build --workspace --locked
node --test engine/server.test.mjs engine/extensions.test.mjs engine/protocol-editors.test.mjs
