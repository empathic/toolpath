#!/bin/bash
set -e

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
WASM_JS="$ROOT/site/wasm/path.js"
WASM_BIN="$ROOT/site/wasm/path.wasm"
EMSDK_DIR="$ROOT/local/emsdk"

# --- Parse flags --------------------------------------------------------------
# --if-changed   Skip build if outputs are newer than all Rust sources
# --dev          Use dev profile (fast incremental builds, no LTO/strip)

DEV=false
IF_CHANGED=false
for arg in "$@"; do
  case "$arg" in
    --dev)        DEV=true ;;
    --if-changed) IF_CHANGED=true ;;
  esac
done

if $DEV; then
  PROFILE=dev
  PROFILE_DIR=debug
  SENTINEL="$ROOT/target/.wasm-dev-built"
else
  PROFILE=wasm
  PROFILE_DIR=wasm
  SENTINEL="$ROOT/target/.wasm-built"
fi

# --- Staleness check ----------------------------------------------------------

wasm_is_stale() {
  [ ! -f "$WASM_JS" ] || [ ! -f "$WASM_BIN" ] || [ ! -f "$SENTINEL" ] && return 0

  [ -n "$(find "$ROOT/crates" "$ROOT/Cargo.toml" "$ROOT/.cargo/config.toml" \
      \( -name '*.rs' -o -name 'Cargo.toml' \) \
      -newer "$SENTINEL" 2>/dev/null | head -1)" ]
}

if $IF_CHANGED; then
  if ! wasm_is_stale; then
    exit 0
  fi
  echo "wasm: Rust sources changed, rebuilding ($PROFILE)..."
fi

# --- Ensure emsdk is available ------------------------------------------------

ensure_emsdk() {
  # Already on PATH?
  if command -v emcc &>/dev/null; then
    return 0
  fi

  # Local install exists? Activate it.
  if [ -f "$EMSDK_DIR/emsdk_env.sh" ]; then
    echo "wasm: Activating local emsdk..."
    source "$EMSDK_DIR/emsdk_env.sh" 2>/dev/null
    return 0
  fi

  # Bootstrap: clone + install + activate
  echo "wasm: Installing emsdk to target/emsdk (one-time)..."
  git clone --depth 1 https://github.com/emscripten-core/emsdk.git "$EMSDK_DIR"
  "$EMSDK_DIR/emsdk" install latest
  "$EMSDK_DIR/emsdk" activate latest
  source "$EMSDK_DIR/emsdk_env.sh" 2>/dev/null
}

ensure_emsdk

# --- Ensure rustup target -----------------------------------------------------

if ! rustup target list --installed 2>/dev/null | grep -q wasm32-unknown-emscripten; then
  echo "wasm: Adding rustup target wasm32-unknown-emscripten..."
  rustup target add wasm32-unknown-emscripten
fi

# --- Build --------------------------------------------------------------------

cd "$ROOT"
cargo build --target wasm32-unknown-emscripten -p toolpath-cli --profile "$PROFILE"

mkdir -p site/wasm
cp "target/wasm32-unknown-emscripten/$PROFILE_DIR/path.js"   site/wasm/path.js
cp "target/wasm32-unknown-emscripten/$PROFILE_DIR/path.wasm" site/wasm/path.wasm
touch "$SENTINEL"

echo "wasm: Built site/wasm/path.{js,wasm}  ($PROFILE)"
ls -lh site/wasm/
