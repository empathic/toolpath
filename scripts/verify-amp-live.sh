#!/usr/bin/env bash
# Live end-to-end verification of the Amp projector against the real CLI.
#
# Amp threads are server-authoritative (no local thread store exists), so
# unlike the copilot variant this script cannot fabricate purely locally:
# the projection step submits the thread to the Amp SERVER under a FRESH
# `T-…` id via the reverse-engineered import seam, and the loader check
# continues that thread with the real `amp` CLI. Local Amp state is fully
# isolated (fresh HOME + the three XDG vars, per the piece-00 Q3 recipe);
# existing threads are never touched.
#
#   1. project   — `path resume --harness amp` with a no-op `amp` shim
#                  (projection + server import happen; the exec is inert);
#   2. loader    — real `amp threads continue <id> -x <probe>`: the thread
#                  must load rather than erroring;
#   3. probe     — the resumed model must answer a question about the
#                  session's own history (proves the context reached it).
#
# Usage:
#   AMP_API_KEY=… scripts/verify-amp-live.sh <doc.json | pathbase-url | cache-id>
#
# Requires: `amp` on PATH, a built `path` binary (cargo build -p path-cli),
# and AMP_API_KEY — MANDATORY: an isolated home without it auto-launches
# Amp's browser login flow, which can complete unattended and mint a real
# token (observed twice during recon; see docs/agents/formats/amp/RECON.md).
#
# ⚠ Costs: continuing a thread spends real credits, and the import creates
# a real (private) thread on the authenticated account.
set -euo pipefail

_input="${1:?usage: AMP_API_KEY=… verify-amp-live.sh <doc.json|url|cache-id>}"
: "${AMP_API_KEY:?AMP_API_KEY must be set — without it the isolated home triggers the unattended amp browser-login flow}"
_repo_root="$(cd "$(dirname "$0")/.." && pwd)"
_path_bin="${PATH_BIN:-${_repo_root}/target/debug/path}"
[ -x "${_path_bin}" ] || { echo "path binary not found at ${_path_bin} — cargo build -p path-cli" >&2; exit 1; }
command -v amp >/dev/null || { echo "amp CLI not on PATH" >&2; exit 1; }

_work="$(mktemp -d -t amp-verify)"
trap 'rm -rf "${_work}"' EXIT
mkdir -p "${_work}/home" "${_work}/data" "${_work}/cache" "${_work}/config" \
         "${_work}/cwd" "${_work}/bin" "${_work}/cfg"
# A no-op `amp` shim so `path resume`'s exec is inert; the real CLI runs below.
printf '#!/bin/sh\ntrue\n' > "${_work}/bin/amp"
chmod +x "${_work}/bin/amp"

# The piece-00 Q3 isolation recipe: HOME plus all three XDG roots, with
# AMP_API_KEY supplied so the login flow is never reached.
_amp_env=(HOME="${_work}/home"
          XDG_DATA_HOME="${_work}/data" XDG_CACHE_HOME="${_work}/cache"
          XDG_CONFIG_HOME="${_work}/config" AMP_API_KEY="${AMP_API_KEY}")

echo "── projecting ${_input} (server import under a fresh T-… id)…"
env "${_amp_env[@]}" TOOLPATH_CONFIG_DIR="${_work}/cfg" PATH="${_work}/bin:${PATH}" \
  "${_path_bin}" resume "${_input}" --harness amp -C "${_work}/cwd" >/dev/null
_artifact="$(find "${_work}/cfg/amp-projected" -name 'T-*.json' | head -1)"
[ -n "${_artifact}" ] || { echo "✗ no projected artifact written" >&2; exit 1; }
_tid="$(basename "${_artifact}" .json)"
echo "   projected thread: ${_tid}"

echo "── loader check (amp threads continue)…"
_probe='In one sentence, what was the most-used tool in this session?'
_out="$(env "${_amp_env[@]}" \
        amp threads continue "${_tid}" -x "${_probe}" --no-archive-after-execute \
        </dev/null 2>&1 || true)"
if echo "${_out}" | grep -qiE "error|not found|invalid|permission|failed"; then
  echo "✗ LOADER REJECTED the projected thread:" >&2
  echo "${_out}" | grep -iE "error|not found|invalid|permission|failed" | head -3 >&2
  echo "   (record the verbatim rejection in docs/agents/formats/amp/writing-compatible.md)" >&2
  exit 1
fi
echo "✓ thread loaded"

echo "── context probe answer:"
echo "${_out}" | head -3
echo
echo "✓ done — judge the probe answer above: a specific, correct answer means the"
echo "  model received the full context; a generic/amnesiac one means it didn't."
