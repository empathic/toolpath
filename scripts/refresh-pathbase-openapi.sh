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

_root="$(cd "$(dirname "$0")/.." && pwd)"
_url="${PATHBASE_URL:-https://pathbase.dev}"
_dest="${_root}/crates/pathbase-client/openapi.json"

echo "refresh: GET ${_url}/api/v1/openapi.json"
_tmp="$(mktemp -t pathbase-openapi.XXXXXX.json)"
trap 'rm -f "${_tmp}"' EXIT
curl -fsSL "${_url}/api/v1/openapi.json" -o "${_tmp}"

# Pretty-print with jq for stable diffs.
jq . "${_tmp}" > "${_dest}"
echo "refresh: wrote ${_dest} ($(wc -l < "${_dest}") lines)"
