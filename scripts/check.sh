#!/usr/bin/env bash
# Dev-friendly loop: auto-format, then run the full CI verification.
# For pure verification (no auto-fix), run `just ci` / `scripts/quality_gates.sh`.

set -euo pipefail

_root="$(cd "$(dirname "$0")/.." && pwd)"

"${_root}/scripts/fmt.sh"
"${_root}/scripts/quality_gates.sh" "$@"
