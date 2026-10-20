#!/bin/sh
# Builds dist/: the WebAssembly core, its bindings, and an inlined copy of
# the module so the package loads anywhere without bundler configuration.
set -eu
cd "$(dirname "$0")/../.."
cargo build -p peerconnect-wasm --target wasm32-unknown-unknown --profile wasm
"${WASM_BINDGEN:-wasm-bindgen}" --target web --weak-refs --out-dir js/dist --out-name peerconnect \
  target/wasm32-unknown-unknown/wasm/peerconnect_wasm.wasm
if command -v "${WASM_OPT:-wasm-opt}" >/dev/null 2>&1; then
  "${WASM_OPT:-wasm-opt}" -Os --enable-bulk-memory --enable-nontrapping-float-to-int --enable-sign-ext \
    --enable-mutable-globals --enable-reference-types --enable-multivalue \
    js/dist/peerconnect_bg.wasm -o js/dist/peerconnect_bg.wasm
fi
node js/scripts/inline.mjs js/dist/peerconnect_bg.wasm js/dist/module.js
ls -l js/dist/peerconnect_bg.wasm
