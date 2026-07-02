#!/usr/bin/env bash
# Live end-to-end verification of the Copilot projector against the real CLI.
#
# Projects a toolpath document into an ISOLATED COPILOT_HOME (your real
# ~/.copilot is never touched; its config.json is copied for auth) and runs
# the real `copilot --resume` twice:
#   1. loader check  — the session must load (no "Session file is corrupted");
#   2. context probe — the resumed model must answer a question about the
#      session's own history (proves the context reached the model).
#
# Usage:
#   scripts/verify-copilot-live.sh <doc.json | pathbase-url | cache-id>
#
# Requires: `copilot` on PATH (authenticated), a built `path` binary
# (cargo build -p path-cli), sqlite3.
#
# The TUI *rendering* contract (colorized diffs etc.) can't be checked from
# `-p` mode; see docs/agents/formats/copilot-cli/known-gaps-and-sourcing.md
# ("Verification methodology") for the pty-capture technique.
set -euo pipefail

_input="${1:?usage: verify-copilot-live.sh <doc.json|url|cache-id>}"
_repo_root="$(cd "$(dirname "$0")/.." && pwd)"
_path_bin="${PATH_BIN:-${_repo_root}/target/debug/path}"
[ -x "${_path_bin}" ] || { echo "path binary not found at ${_path_bin} — cargo build -p path-cli" >&2; exit 1; }
command -v copilot >/dev/null || { echo "copilot CLI not on PATH" >&2; exit 1; }
[ -f "${HOME}/.copilot/config.json" ] || { echo "~/.copilot/config.json missing — run copilot once to authenticate" >&2; exit 1; }

_work="$(mktemp -d -t copilot-verify)"
trap 'rm -rf "${_work}"' EXIT
mkdir -p "${_work}/home" "${_work}/cwd" "${_work}/bin" "${_work}/cfg"
cp "${HOME}/.copilot/config.json" "${_work}/home/"
sqlite3 "${_work}/home/session-store.db" \
  "CREATE TABLE sessions (id TEXT PRIMARY KEY, cwd TEXT, repository TEXT, host_type TEXT, branch TEXT, summary TEXT, created_at TEXT, updated_at TEXT);"
# A no-op `copilot` shim so `path resume`'s exec is inert; the real CLI runs below.
printf '#!/bin/sh\ntrue\n' > "${_work}/bin/copilot"
chmod +x "${_work}/bin/copilot"

echo "── projecting ${_input} into isolated COPILOT_HOME…"
TOOLPATH_CONFIG_DIR="${_work}/cfg" COPILOT_HOME="${_work}/home" PATH="${_work}/bin:${PATH}" \
  "${_path_bin}" resume "${_input}" --harness copilot -C "${_work}/cwd" >/dev/null
_sid="$(ls "${_work}/home/session-state" | head -1)"
echo "   projected session: ${_sid}"

echo "── loader check (copilot --resume)…"
_probe='In one sentence, what was the most-used tool in this session?'
_out="$(COPILOT_HOME="${_work}/home" copilot --resume "${_sid}" -p "${_probe}" </dev/null 2>&1 || true)"
if echo "${_out}" | grep -qi "corrupted\|could not be loaded\|missing field"; then
  echo "✗ LOADER REJECTED the projected session:" >&2
  echo "${_out}" | grep -iE "corrupted|missing field|could not be loaded" | head -3 >&2
  echo "   (add the new requirement to docs/agents/formats/copilot-cli/writing-compatible.md)" >&2
  exit 1
fi
echo "✓ session loaded"

echo "── context probe answer:"
echo "${_out}" | head -3
echo
echo "✓ done — judge the probe answer above: a specific, correct answer means the"
echo "  model received the full context; a generic/amnesiac one means it didn't."
