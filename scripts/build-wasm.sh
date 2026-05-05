#!/bin/bash
set -e

_root="$(cd "$(dirname "$0")/.." && pwd)"
_wasm_js="${_root}/site/wasm/path.js"
_wasm_bin="${_root}/site/wasm/path.wasm"
_emsdk_dir="${_root}/local/emsdk"

# --- Parse flags --------------------------------------------------------------
# --if-changed   Skip build if outputs are newer than all Rust sources
# --dev          Use dev profile (fast incremental builds, no LTO/strip)

_dev=false
_if_changed=false
for _arg in "$@"; do
  case "${_arg}" in
    --dev)        _dev=true ;;
    --if-changed) _if_changed=true ;;
  esac
done

if ${_dev}; then
  _profile=dev
  _profile_dir=debug
  _sentinel="${_root}/target/.wasm-dev-built"
else
  _profile=wasm
  _profile_dir=wasm
  _sentinel="${_root}/target/.wasm-built"
fi

# --- Staleness check ----------------------------------------------------------

wasm_is_stale() {
  [ ! -f "${_wasm_js}" ] || [ ! -f "${_wasm_bin}" ] || [ ! -f "${_sentinel}" ] && return 0

  [ -n "$(find "${_root}/crates" "${_root}/Cargo.toml" "${_root}/.cargo/config.toml" \
      \( -name '*.rs' -o -name 'Cargo.toml' \) \
      -newer "${_sentinel}" 2>/dev/null | head -1)" ]
}

if ${_if_changed}; then
  if ! wasm_is_stale; then
    exit 0
  fi
  echo "wasm: Rust sources changed, rebuilding (${_profile})..."
fi

# --- Ensure emsdk is available ------------------------------------------------

ensure_emsdk() {
  # Already on PATH?
  if command -v emcc &>/dev/null; then
    return 0
  fi

  # Local install exists? Activate it.
  if [ -f "${_emsdk_dir}/emsdk_env.sh" ]; then
    echo "wasm: Activating local emsdk..."
    # shellcheck source=/dev/null
    source "${_emsdk_dir}/emsdk_env.sh" 2>/dev/null
    return 0
  fi

  # Bootstrap: clone + install + activate
  echo "wasm: Installing emsdk to target/emsdk (one-time)..."
  git clone --depth 1 https://github.com/emscripten-core/emsdk.git "${_emsdk_dir}"
  "${_emsdk_dir}/emsdk" install latest
  "${_emsdk_dir}/emsdk" activate latest
  # shellcheck source=/dev/null
  source "${_emsdk_dir}/emsdk_env.sh" 2>/dev/null
}

ensure_emsdk

# --- Ensure rustup target -----------------------------------------------------

if ! rustup target list --installed 2>/dev/null | grep -q wasm32-unknown-emscripten; then
  echo "wasm: Adding rustup target wasm32-unknown-emscripten..."
  rustup target add wasm32-unknown-emscripten
fi

# --- Build --------------------------------------------------------------------

cd "${_root}"
cargo build --target wasm32-unknown-emscripten -p path-cli --profile "${_profile}"

mkdir -p site/wasm
cp "target/wasm32-unknown-emscripten/${_profile_dir}/path.js"   site/wasm/path.js
cp "target/wasm32-unknown-emscripten/${_profile_dir}/path.wasm" site/wasm/path.wasm
touch "${_sentinel}"

echo "wasm: Built site/wasm/path.{js,wasm}  (${_profile})"
ls -lh site/wasm/
