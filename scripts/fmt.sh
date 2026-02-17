#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"

echo "fmt: cargo fmt"
cargo fmt --all --manifest-path "$ROOT/Cargo.toml"

echo "fmt: prettier (site/)"
cd "$ROOT/site"
npx --yes prettier --write --no-color "**/*.{md,css,json,js}"
