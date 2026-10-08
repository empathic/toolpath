#!/usr/bin/env bash
# Hermetic goldens for cross-harness transformations. See goldens/DEMO.md.
#   goldens.sh check    compare fresh hermetic runs to goldens/ byte-for-byte (default)
#   goldens.sh defect   print the pinned known defect (codex -> claude illegal entry types)
#   goldens.sh capture  regenerate goldens/ on purpose, then show what changed
set -euo pipefail
cd "$(dirname "$0")/.."

run() { nix develop --command "$@"; }

case "${1:-check}" in
  check)
    run cargo test -p path-cli --test goldens
    ;;
  defect)
    echo "codex -> claude: top-level entry types outside Claude's vocabulary (type, count)"
    echo "report: ~/.lobby/ops/toolpath-transform-regression-goal.md (2026-09-29)"
    cat goldens/known-defect/codex-to-claude-illegal-types.tsv
    ;;
  capture)
    GOLDENS_UPDATE=1 run cargo test -p path-cli --test goldens
    git status --short goldens
    git diff --stat -- goldens
    ;;
  *)
    echo "usage: $0 [check|defect|capture]" >&2
    exit 2
    ;;
esac
