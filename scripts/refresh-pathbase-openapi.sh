#!/usr/bin/env bash
# Re-fetch the Pathbase OpenAPI spec into crates/pathbase-client/openapi.json.
# Defaults to the dev deployment; override with PATHBASE_URL.
#
# After running this, rebuild crates/pathbase-client to regenerate the
# typed client. Spec drift will surface as a build failure rather than
# an HTML-instead-of-JSON runtime surprise.
#
# The spec lives inside the pathbase-client crate (rather than at
# schema/) so `cargo publish` packages it alongside the source.
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
URL="${PATHBASE_URL:-https://pathbase.dev}"
DEST="$ROOT/crates/pathbase-client/openapi.json"

echo "refresh: GET $URL/api/v1/openapi.json"
TMP="$(mktemp -t pathbase-openapi.XXXXXX.json)"
trap 'rm -f "$TMP"' EXIT
curl -fsSL "$URL/api/v1/openapi.json" -o "$TMP"

# Pretty-print with jq for stable diffs.
jq . "$TMP" > "$DEST"
echo "refresh: wrote $DEST ($(wc -l < "$DEST") lines)"
