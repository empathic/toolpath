#!/usr/bin/env bash
# One-line wrapper: every goldens command is `path goldens ...` (see goldens/DEMO.md).
#   scripts/goldens.sh check | diff <name> | list | init | capture ... | capture-claude
set -euo pipefail
cd "$(dirname "$0")/.."
# The dev shell is a flake on wip only (main moved nix packaging off in #332);
# without one, the pinned rustup toolchain runs cargo directly.
if [ -f flake.nix ] && command -v nix >/dev/null 2>&1; then
    exec nix develop --command cargo run -q -p path-cli -- goldens "${@:-check}"
fi
exec cargo run -q -p path-cli -- goldens "${@:-check}"
