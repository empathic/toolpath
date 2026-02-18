#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
WASM_SCRIPT="$ROOT/scripts/build-wasm.sh"

# --- Wasm watcher (polls every 2s for Rust source changes) -------------------
wasm_watch() {
  local flags=("$@")
  while true; do
    "$WASM_SCRIPT" --if-changed "${flags[@]}" 2>&1 | while IFS= read -r line; do echo "$line"; done
    sleep 2
  done
}

# --- Wasm build (best-effort: warn but don't block if emcc missing) ----------
wasm_build_or_warn() {
  if "$WASM_SCRIPT" "$@" 2>&1; then
    return 0
  else
    echo ""
    echo "  Note: wasm build failed — playground will show a fallback message."
    echo "  Install the Emscripten SDK and re-run to enable the wasm playground."
    echo ""
    return 0
  fi
}

cd "$ROOT/site"

case "${1:-dev}" in
  dev)
    wasm_build_or_warn --dev --if-changed
    wasm_watch --dev &
    WASM_PID=$!
    trap 'kill $WASM_PID 2>/dev/null' EXIT
    pnpm run dev
    ;;
  build)
    wasm_build_or_warn
    pnpm run build
    ;;
  install)
    pnpm install
    ;;
  *)
    echo "Usage: scripts/site.sh [dev|build|install]" >&2
    exit 1
    ;;
esac
