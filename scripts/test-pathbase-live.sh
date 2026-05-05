#!/usr/bin/env bash
# Live-server smoke test for `path import/export pathbase`. Always runs
# the same two scenarios in the same order; never branches on environment
# state. Fails up-front if its preconditions aren't met.
#
# Usage:
#   scripts/test-pathbase-live.sh                    # defaults to https://pathbase.dev
#   scripts/test-pathbase-live.sh <pathbase-url>     # override (e.g. a staging deployment)
#
# Preconditions (checked before any test runs):
#   - <pathbase-url> reachable, returns 2xx on /api/v1/health.
#   - You are logged into <pathbase-url> per `path auth status`.
#     Run `path auth login --url <pathbase-url>` first if not.
#
# Scenarios (always run, in order):
#   1. anonymous   POST /api/v1/anon/paths           (no creds)
#                  GET  /api/v1/repos/anon/.../download
#   2. authed      POST /api/v1/repos/<you>/pathstash/paths   (creds)
#                  GET  /api/v1/repos/<you>/pathstash/paths/<slug>/download
#
# Each scenario asserts the upload returns the expected URL shape and the
# downloaded document re-imports as a Path with the same step count as
# the source fixture. Anything else is a hard failure.

set -euo pipefail

# ── Args ──────────────────────────────────────────────────────────────────

if [[ $# -gt 1 ]]; then
    echo "Usage: $0 [<pathbase-url>]" >&2
    exit 64
fi
_url="${1:-https://pathbase.dev}"
_url="${_url%/}"
_root="$(cd "$(dirname "$0")/.." && pwd)"
_example="${_root}/examples/path-01-pr.path.json"
_expected_steps=5  # path-01-pr.path.json has 5 steps; recheck if the fixture changes

cd "${_root}"

# ── Build ─────────────────────────────────────────────────────────────────

echo "=== build ==="
cargo build -q -p path-cli
_path_bin="${_root}/target/debug/path"

# ── Preconditions ────────────────────────────────────────────────────────

echo
echo "=== preconditions ==="

echo "  fixture: ${_example}"
[[ -f "${_example}" ]] || { echo "FAIL: missing fixture" >&2; exit 1; }

echo "  reachable: ${_url}/api/v1/health"
if ! curl -fsS "${_url}/api/v1/health" >/dev/null; then
    echo "FAIL: ${_url}/api/v1/health did not return 2xx" >&2
    exit 1
fi

echo "  authed: ${_url}"
_auth_status=$("${_path_bin}" auth status 2>&1 || true)
if ! grep -qF "Logged in to ${_url}" <<<"${_auth_status}"; then
    echo "FAIL: not logged into ${_url}" >&2
    echo "  run: path auth login --url ${_url}" >&2
    exit 1
fi

# Cleanup: holds temp config dirs created in each scenario.
_tmp_dirs=()
cleanup() {
    for _d in "${_tmp_dirs[@]+"${_tmp_dirs[@]}"}"; do
        rm -rf -- "${_d}"
    done
}
trap cleanup EXIT

# ── Helpers ──────────────────────────────────────────────────────────────

# Re-import a URL into a clean cache and assert the doc parses as a Path
# with the expected step count. Stdout from `import pathbase` is the
# cached file path; stderr summary contains "<n> steps".
assert_imports_with_steps() {
    local _label="$1" _trace_url="$2" _expected_steps="$3"
    local _tmp; _tmp="$(mktemp -d -t "pb-live-${_label}-import.XXXXXX")"
    _tmp_dirs+=("${_tmp}")
    local _stderr; _stderr="$(TOOLPATH_CONFIG_DIR="${_tmp}" "${_path_bin}" import pathbase "${_trace_url}" --force 2>&1 1>/dev/null)"
    if ! grep -qE "^Imported graph .* \\(1 path, ${_expected_steps} steps\\)" <<<"${_stderr}"; then
        echo "FAIL[${_label}]: import did not report ${_expected_steps} steps" >&2
        echo "${_stderr}" >&2
        exit 1
    fi
    echo "  import OK (${_expected_steps} steps)"
}

# ── 1. Anonymous round-trip ──────────────────────────────────────────────

echo
echo "=== 1. anonymous round-trip ==="

_anon_cfg="$(mktemp -d -t pb-live-anon.XXXXXX)"
_tmp_dirs+=("${_anon_cfg}")

_anon_url=$(TOOLPATH_CONFIG_DIR="${_anon_cfg}" PATHBASE_URL="${_url}" \
    "${_path_bin}" export pathbase --input "${_example}")

case "${_anon_url}" in
    "${_url}"/anon/*) echo "  upload OK: ${_anon_url}" ;;
    *) echo "FAIL[anon]: expected ${_url}/anon/... URL, got: ${_anon_url}" >&2; exit 1 ;;
esac

assert_imports_with_steps "anon" "${_anon_url}" "${_expected_steps}"

# ── 2. Authenticated pathstash round-trip ────────────────────────────────

echo
echo "=== 2. authed pathstash round-trip ==="

_authed_url=$(PATHBASE_URL="${_url}" "${_path_bin}" export pathbase --input "${_example}")

case "${_authed_url}" in
    "${_url}"/anon/*)
        echo "FAIL[authed]: authed upload landed on anon endpoint: ${_authed_url}" >&2
        exit 1 ;;
    "${_url}"/*/pathstash/*) echo "  upload OK: ${_authed_url}" ;;
    *) echo "FAIL[authed]: expected ${_url}/<user>/pathstash/<slug>, got: ${_authed_url}" >&2; exit 1 ;;
esac

assert_imports_with_steps "authed" "${_authed_url}" "${_expected_steps}"

echo
echo "=== PASS ==="
