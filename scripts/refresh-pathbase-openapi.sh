#!/usr/bin/env bash
# Re-fetch the Pathbase OpenAPI spec into schema/pathbase-openapi.json.
# Defaults to the dev deployment; override with PATHBASE_URL.
#
# After running this, rebuild crates/pathbase-client to regenerate the
# typed client. Spec drift will surface as a build failure rather than
# an HTML-instead-of-JSON runtime surprise.
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
URL="${PATHBASE_URL:-https://pathbase-dev.fly.dev}"
DEST="$ROOT/schema/pathbase-openapi.json"

echo "refresh: GET $URL/api/v1/openapi.json"
TMP="$(mktemp -t pathbase-openapi.XXXXXX.json)"
trap 'rm -f "$TMP"' EXIT
curl -fsSL "$URL/api/v1/openapi.json" -o "$TMP"

# Pretty-print with jq for stable diffs.
jq . "$TMP" > "$DEST"
echo "refresh: wrote $DEST ($(wc -l < "$DEST") lines)"
