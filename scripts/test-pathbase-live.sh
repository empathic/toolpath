#!/usr/bin/env bash
# Live-server smoke test for `path import/export pathbase`. Always runs
# the same two scenarios in the same order; never branches on environment
# state. Fails up-front if its preconditions aren't met.
#
# Usage:
#   scripts/test-pathbase-live.sh <pathbase-url>
#
# Examples:
#   scripts/test-pathbase-live.sh https://pathbase-dev.fly.dev
#   scripts/test-pathbase-live.sh https://pathbase.dev
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

if [[ $# -ne 1 ]]; then
    echo "Usage: $0 <pathbase-url>" >&2
    echo "  e.g. $0 https://pathbase-dev.fly.dev" >&2
    exit 64
fi
URL="${1%/}"
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
EXAMPLE="$ROOT/examples/path-01-pr.path.json"
EXPECTED_STEPS=5  # path-01-pr.path.json has 5 steps; recheck if the fixture changes

cd "$ROOT"

# ── Build ─────────────────────────────────────────────────────────────────

echo "=== build ==="
cargo build -q -p path-cli
PATH_BIN="$ROOT/target/debug/path"

# ── Preconditions ────────────────────────────────────────────────────────

echo
echo "=== preconditions ==="

echo "  fixture: $EXAMPLE"
[[ -f "$EXAMPLE" ]] || { echo "FAIL: missing fixture" >&2; exit 1; }

echo "  reachable: $URL/api/v1/health"
if ! curl -fsS "$URL/api/v1/health" >/dev/null; then
    echo "FAIL: $URL/api/v1/health did not return 2xx" >&2
    exit 1
fi

echo "  authed: $URL"
AUTH_STATUS=$("$PATH_BIN" auth status 2>&1 || true)
if ! grep -qF "Logged in to $URL" <<<"$AUTH_STATUS"; then
    echo "FAIL: not logged into $URL" >&2
    echo "  run: path auth login --url $URL" >&2
    exit 1
fi

# Cleanup: holds temp config dirs created in each scenario.
TMP_DIRS=()
cleanup() {
    for d in "${TMP_DIRS[@]+"${TMP_DIRS[@]}"}"; do
        rm -rf -- "$d"
    done
}
trap cleanup EXIT

# ── Helpers ──────────────────────────────────────────────────────────────

# Re-import a URL into a clean cache and assert the doc parses as a Path
# with the expected step count. Stdout from `import pathbase` is the
# cached file path; stderr summary contains "<n> steps".
assert_imports_with_steps() {
    local label="$1" trace_url="$2" expected_steps="$3"
    local tmp; tmp="$(mktemp -d -t "pb-live-${label}-import.XXXXXX")"
    TMP_DIRS+=("$tmp")
    local stderr; stderr="$(TOOLPATH_CONFIG_DIR="$tmp" "$PATH_BIN" import pathbase "$trace_url" --force 2>&1 1>/dev/null)"
    if ! grep -qE "^Imported path .* \\(${expected_steps} steps\\)" <<<"$stderr"; then
        echo "FAIL[$label]: import did not report ${expected_steps} steps" >&2
        echo "$stderr" >&2
        exit 1
    fi
    echo "  import OK ($expected_steps steps)"
}

# ── 1. Anonymous round-trip ──────────────────────────────────────────────

echo
echo "=== 1. anonymous round-trip ==="

ANON_CFG="$(mktemp -d -t pb-live-anon.XXXXXX)"
TMP_DIRS+=("$ANON_CFG")

ANON_URL=$(TOOLPATH_CONFIG_DIR="$ANON_CFG" PATHBASE_URL="$URL" \
    "$PATH_BIN" export pathbase --input "$EXAMPLE")

case "$ANON_URL" in
    "$URL"/anon/*) echo "  upload OK: $ANON_URL" ;;
    *) echo "FAIL[anon]: expected $URL/anon/... URL, got: $ANON_URL" >&2; exit 1 ;;
esac

assert_imports_with_steps "anon" "$ANON_URL" "$EXPECTED_STEPS"

# ── 2. Authenticated pathstash round-trip ────────────────────────────────

echo
echo "=== 2. authed pathstash round-trip ==="

AUTHED_URL=$(PATHBASE_URL="$URL" "$PATH_BIN" export pathbase --input "$EXAMPLE")

case "$AUTHED_URL" in
    "$URL"/anon/*)
        echo "FAIL[authed]: authed upload landed on anon endpoint: $AUTHED_URL" >&2
        exit 1 ;;
    "$URL"/*/pathstash/*) echo "  upload OK: $AUTHED_URL" ;;
    *) echo "FAIL[authed]: expected $URL/<user>/pathstash/<slug>, got: $AUTHED_URL" >&2; exit 1 ;;
esac

assert_imports_with_steps "authed" "$AUTHED_URL" "$EXPECTED_STEPS"

echo
echo "=== PASS ==="
