#!/usr/bin/env bash
# Build crates/lookout-web to WebAssembly and bind it for the browser.
#
# Needs the wasm32 target and a wasm-bindgen CLI matching the crate's
# wasm-bindgen version (Cargo.lock):
#   rustup target add wasm32-unknown-unknown
#   cargo install wasm-bindgen-cli --version <version>
set -euo pipefail
here="$(cd "$(dirname "$0")" && pwd)"
workspace="$here/../../clients/desktop/src-tauri"
cd "$workspace"
cargo build -p lookout-web --target wasm32-unknown-unknown --profile wasm-release
mkdir -p "$here/dist"
wasm-bindgen --target web --typescript \
  --out-dir "$here/dist" \
  "${CARGO_TARGET_DIR:-target}/wasm32-unknown-unknown/wasm-release/lookout_web.wasm"
# wasm-opt is optional: use it when present for a smaller binary.
if command -v wasm-opt >/dev/null 2>&1; then
  wasm-opt -Os -o "$here/dist/lookout_web_bg.wasm" "$here/dist/lookout_web_bg.wasm"
fi
ls -la "$here/dist"
