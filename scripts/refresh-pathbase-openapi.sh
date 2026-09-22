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

# Pathbase emits OpenAPI 3.1 (`"type": ["string", "null"]`) but our
# generator stack (progenitor 0.14 / openapiv3 2.x) only understands
# 3.0 (`"type": "string", "nullable": true`). Down-convert nullable
# unions to 3.0 form so the build doesn't panic on `not yet
# implemented: invalid type: null`.
#
# Only handles the single-non-null + "null" pattern. Multi-type
# unions are rejected explicitly so we notice if the spec ever uses
# something more exotic.
jq '
  def downconvert_type_array:
    if type == "object"
       and (has("type"))
       and (.type | type) == "array"
    then
      if (.type | any(. == "null")) and (.type | map(select(. != "null")) | length) == 1
      then .type = (.type | map(select(. != "null"))[0]) | .nullable = true
      elif (.type | any(. == "null"))
      then
        error("multi-type nullable union not supported by openapiv3 0.x: \(.type)")
      else .
      end
    else . end;

  # `oneOf: [{type: null}, {$ref: X}]` (or in either order) is OpenAPI 3.1
  # idiom for "nullable ref". Convert to 3.0: `{nullable: true, allOf: [{$ref: X}]}`.
  def downconvert_nullable_ref:
    if type == "object"
       and has("oneOf")
       and (.oneOf | type) == "array"
       and (.oneOf | length) == 2
       and (.oneOf | any(. == {"type": "null"}))
       and (.oneOf | any(has("$ref")))
    then
      (.oneOf | map(select(has("$ref"))) | .[0]) as $ref_obj
      | del(.oneOf)
      | .nullable = true
      | .allOf = [{"$ref": $ref_obj["$ref"]}]
      | (if $ref_obj.description then .description = $ref_obj.description else . end)
    else . end;

  # Progenitor 0.14 accepts request bodies of application/json,
  # application/x-www-form-urlencoded, application/octet-stream,
  # text/plain and text/x-markdown, and JSON responses only. The
  # streamed-upload routes take application/x-ndjson with schema
  # {type: string}; rewrite that key to text/plain so progenitor
  # generates a `body: String` parameter. The generated client then
  # sends `Content-Type: text/plain`, which the server accepts: those
  # handlers read the raw body and do not check the header. Operations
  # still using an unsupported content type after the rewrite are
  # dropped so the build does not panic on
  # `UnexpectedFormat("unexpected content type: ...")`.
  def rewrite_ndjson_request_body:
    if type == "object"
       and has("requestBody")
       and ((.requestBody.content // {}) | has("application/x-ndjson"))
    then
      .requestBody.content |= with_entries(
        if .key == "application/x-ndjson" then .key = "text/plain" else . end
      )
    else . end;

  # Server doc comments are copied into the rustdoc of the generated
  # client. A rustdoc intra-doc link ([`name`]) to a server-side item
  # does not resolve there and fails `cargo doc -D warnings`; keep the
  # code span, drop the link brackets. (No apostrophes in these
  # comments: the whole program sits in single quotes.)
  def unlink_descriptions:
    if type == "object" and (.description | type) == "string"
    then .description |= gsub("\\[(?<code>`[^`]+`)\\]"; .code)
    else . end;

  def supported_request_body(key):
    key | IN("application/json", "application/x-www-form-urlencoded",
             "application/octet-stream", "text/plain", "text/x-markdown");

  def has_unsupported_content(op):
    ((op.requestBody.content // {}) | keys | any(supported_request_body(.) | not))
    or ((op.responses // {}) | to_entries | any(
      ((.value.content // {}) | keys | any(. != "application/json"))
    ));

  def strip_unsupported_operations:
    .paths |= with_entries(
      .value |= with_entries(
        select(
          (.key | IN("get", "put", "post", "delete", "patch", "options", "head", "trace") | not)
          or (has_unsupported_content(.value) | not)
        )
      )
    ) | .paths |= with_entries(select((.value | length) > 0));

  walk(downconvert_type_array | downconvert_nullable_ref | rewrite_ndjson_request_body | unlink_descriptions)
  | strip_unsupported_operations
' "${_tmp}" > "${_dest}"
echo "refresh: wrote ${_dest} ($(wc -l < "${_dest}") lines, OpenAPI 3.0 form)"
