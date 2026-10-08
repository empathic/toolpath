#!/usr/bin/env bash
# One-line wrapper: every goldens command is `path goldens ...` (see goldens/DEMO.md).
#   scripts/goldens.sh check | diff <name> | list | init | capture ... | capture-claude
set -euo pipefail
cd "$(dirname "$0")/.."
exec nix develop --command cargo run -q -p path-cli -- goldens "${@:-check}"
