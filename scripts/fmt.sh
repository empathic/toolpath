#!/usr/bin/env bash
set -euo pipefail

_root="$(cd "$(dirname "$0")/.." && pwd)"

echo "fmt: cargo fmt"
cargo fmt --all --manifest-path "${_root}/Cargo.toml"

echo "fmt: prettier (site/)"
cd "${_root}/site"
npx --yes prettier --write --no-color "**/*.{md,css,json,js}"
