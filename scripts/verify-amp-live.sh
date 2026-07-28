#!/usr/bin/env bash
# Live end-to-end verification of the Amp writer against the real CLI.
#
# Amp threads are server-authoritative — there is no local thread store to
# fabricate into — so this script exercises the real writer path:
#
#   1. project — `path p export amp --project` creates a FRESH thread with
#                `amp threads new` and seeds it with the prior session's
#                rendered transcript (one execute turn);
#   2. probe   — the real `amp threads continue <id> -x …` must answer a
#                question about that prior session correctly, proving the
#                context reached the model.
#
# Local Amp state is isolated (fresh HOME + all three XDG roots, the
# piece-00 Q3 recipe). Existing threads are never touched: the only thread
# involved is the one created here.
#
# Usage:
#   AMP_API_KEY=… scripts/verify-amp-live.sh <doc.json | pathbase-url | cache-id>
#
# Requires: `amp` on PATH, a built `path` binary (cargo build -p path-cli),
# and AMP_API_KEY — MANDATORY: an isolated home without it auto-launches
# Amp's browser login flow, which can complete unattended and mint a real
# token (observed twice during recon; see docs/agents/formats/amp/RECON.md).
#
# ⚠ Costs: creates one real (private) thread and spends credits on two
# turns — the seeding turn and the probe.
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
         "${_work}/cwd" "${_work}/cfg"

# The piece-00 Q3 isolation recipe: HOME plus all three XDG roots, with
# AMP_API_KEY supplied so the login flow is never reached. `path` shells out
# to `amp`, which inherits this environment.
_amp_env=(HOME="${_work}/home"
          XDG_DATA_HOME="${_work}/data" XDG_CACHE_HOME="${_work}/cache"
          XDG_CONFIG_HOME="${_work}/config" AMP_API_KEY="${AMP_API_KEY}")

echo "── projecting ${_input} into a fresh Amp thread…"
env "${_amp_env[@]}" TOOLPATH_CONFIG_DIR="${_work}/cfg" \
  "${_path_bin}" p export amp --input "${_input}" --project "${_work}/cwd"
_artifact="$(find "${_work}/cfg/amp-projected" -name 'T-*.json' | head -1)"
[ -n "${_artifact}" ] || { echo "✗ no projected artifact written" >&2; exit 1; }
_tid="$(basename "${_artifact}" .json)"

echo "── context probe (amp threads continue)…"
_probe='In one sentence, what was the most-used tool in this session?'
_out="$(env "${_amp_env[@]}" \
        amp threads continue "${_tid}" -x "${_probe}" </dev/null 2>&1)" || {
  echo "✗ amp REJECTED the projected thread:" >&2
  echo "${_out}" | head -5 >&2
  echo "   (record the verbatim rejection in docs/agents/formats/amp/writing-compatible.md)" >&2
  exit 1
}
echo "✓ thread ${_tid} loaded and answered"
echo
echo "── probe answer:"
echo "${_out}" | head -3
echo
echo "✓ done — judge the answer above: a specific, correct claim about the prior"
echo "  session means the context transferred; a generic/amnesiac one means it didn't."
