#!/usr/bin/env bash
# Build and verify everything locally: contract tests, wasm, embedded specs,
# app tests and the offline demo. No network needed beyond crates.io / npm.
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"
echo "== cargo test (five contracts, Soroban host)"
cargo test
echo "== stellar contract build (wasm32v1-none)"
stellar contract build
ls -la target/wasm32v1-none/release/*.wasm
echo "== embed contract specs into the app"
"$ROOT/scripts/gen-specs.sh"
echo "== app: install, compile, test, demo"
cd "$ROOT/app"
[ -d node_modules ] || npm install --no-audit --no-fund
npm test
npm run demo
