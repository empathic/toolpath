#!/usr/bin/env bash
set -euo pipefail

cd "$(dirname "$0")/../site"

case "${1:-dev}" in
  dev)   pnpm run dev ;;
  build) pnpm run build ;;
  install) pnpm install ;;
  *) echo "Usage: scripts/site.sh [dev|build|install]" >&2; exit 1 ;;
esac
