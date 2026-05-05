#!/usr/bin/env bash
set -euo pipefail

_root="$(cd "$(dirname "$0")/.." && pwd)"
_wasm_script="${_root}/scripts/build-wasm.sh"

# --- Wasm watcher (polls every 2s for Rust source changes) -------------------
wasm_watch() {
  local _flags=("$@")
  while true; do
    "${_wasm_script}" --if-changed "${_flags[@]}" 2>&1 | while IFS= read -r _line; do echo "${_line}"; done
    sleep 2
  done
}

# --- Wasm build (best-effort: warn but don't block if emcc missing) ----------
wasm_build_or_warn() {
  if "${_wasm_script}" "$@" 2>&1; then
    return 0
  else
    echo ""
    echo "  Note: wasm build failed — playground will show a fallback message."
    echo "  Install the Emscripten SDK and re-run to enable the wasm playground."
    echo ""
    return 0
  fi
}

cd "${_root}/site"

case "${1:-dev}" in
  dev)
    wasm_build_or_warn --dev --if-changed
    wasm_watch --dev &
    _wasm_pid=$!
    trap 'kill ${_wasm_pid} 2>/dev/null' EXIT
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
