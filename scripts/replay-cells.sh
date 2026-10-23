#!/usr/bin/env bash
# Build the replay cells crates/toolpath-webmock/tests/replay_agents.rs reads (pr/103):
#   <out>/<agent>/{home,project,sid}
# the same way the cassettes were recorded (cassettes/<agent>/provenance.json): the codex golden
# (goldens/codex/input.jsonl) -> `path p derive codex --all` -> `path resume --harness <agent>` into
# a hermetic HOME. The harness binary is a shim that records its argv and exits, so no agent runs
# and no model is called; the session id is read back from that argv.
#
# usage: scripts/replay-cells.sh <path-binary> <out-dir> [agent ...]
#   default agents: claude copilot pi opencode gemini
set -euo pipefail

PATHBIN=${1:?usage: replay-cells.sh <path-binary> <out-dir> [agent ...]}
OUT=${2:?usage: replay-cells.sh <path-binary> <out-dir> [agent ...]}
shift 2
AGENTS=("$@")
[ ${#AGENTS[@]} -gt 0 ] || AGENTS=(claude copilot pi opencode gemini)

ROOT=$(cd "$(dirname "$0")/.." && pwd)
GOLDEN=$ROOT/goldens/codex/input.jsonl
SID_SRC=019e03b2-11a2-7320-9098-5d831813e66c   # the session id inside the golden

mkdir -p "$OUT"
OUT=$(cd "$OUT" && pwd -P)

henv() { # HENV: the hermetic variables run::hermetic_env sets, for a home dir
  local h=$1
  HENV=("HOME=$h" "TMPDIR=$h" "XDG_CONFIG_HOME=$h/.config" "XDG_DATA_HOME=$h/.local/share"
    "XDG_STATE_HOME=$h/.local/state" "XDG_CACHE_HOME=$h/.cache" "CLAUDE_CONFIG_DIR=$h/.claude"
    "CODEX_HOME=$h/.codex" "COPILOT_HOME=$h/.copilot" "TOOLPATH_CONFIG_DIR=$h/.toolpath"
    "GEMINI_CLI_HOME=$h")
}
mkhome() { mkdir -p "$1"/{.config,.local/share,.local/state,.cache,.claude,.codex,.copilot,.toolpath}; }

# 1. The golden as a codex rollout in a source home, derived into a Toolpath document.
SRC=$OUT/.src-codex
rm -rf "$SRC"; mkhome "$SRC"
mkdir -p "$SRC/.codex/sessions/2026/05/07"
cp "$GOLDEN" "$SRC/.codex/sessions/2026/05/07/rollout-2026-05-07T18-27-42-$SID_SRC.jsonl"
DOC=$OUT/codex-doc.json
henv "$SRC"
( cd "$SRC" && env -i PATH=/usr/bin:/bin "${HENV[@]}" "$PATHBIN" p derive codex --all ) >"$DOC"
[ -s "$DOC" ] || { echo "derive produced nothing" >&2; exit 1; }

# 2. One cell per agent: resume into the cell with the harness replaced by an argv-recording shim.
for agent in "${AGENTS[@]}"; do
  c=$OUT/$agent
  rm -rf "$c"; mkhome "$c/home"; mkdir -p "$c/project" "$OUT/.shim-$agent"
  shim=$OUT/.shim-$agent
  if [ "$agent" = opencode ]; then
    # `path resume` writes into an existing opencode database. Let the pinned opencode create it
    # (all its migrations, offline: `session list` makes no request), so the replay run does not
    # try to migrate a partial schema.
    oc=$(command -v opencode) || { echo "opencode: the pinned opencode must be on PATH to build its cell" >&2; exit 1; }
    henv "$c/home"
    ( cd "$c/project" && env -i PATH=/usr/bin:/bin "${HENV[@]}" OPENCODE_DISABLE_AUTOUPDATE=1 \
        "$oc" session list </dev/null ) >"$OUT/.init-opencode.log" 2>&1 || true
    [ -s "$c/home/.local/share/opencode/opencode-stable.db" ] \
      || { echo "opencode: no database after init (see $OUT/.init-opencode.log)" >&2; exit 1; }
  fi
  # shellcheck disable=SC2016 # the shim's own "$@" and "$a" are meant literally
  printf '#!/bin/sh\nfor a in "$@"; do printf "%%s\\n" "$a"; done > "%s/argv.txt"\nexit 0\n' "$shim" >"$shim/$agent"
  chmod +x "$shim/$agent"
  henv "$c/home"
  ( cd "$c/project" && env -i PATH="$shim:/usr/bin:/bin" "${HENV[@]}" \
      "$PATHBIN" resume --harness "$agent" -C "$c/project" "$DOC" ) >"$OUT/.resume-$agent.log" 2>&1 \
    || { echo "resume --harness $agent failed (see $OUT/.resume-$agent.log)" >&2; exit 1; }
  # The session id: the value of --resume/-r/--session (or --resume=<id>).
  sid=$(awk 'f{print; exit} /^--resume=|^--session=/{sub(/^--[a-z]+=/,""); print; exit} /^(--resume|-r|--session)$/{f=1}' "$shim/argv.txt")
  [ -n "$sid" ] || { echo "$agent: no session id in the harness argv: $(tr '\n' ' ' <"$shim/argv.txt")" >&2; exit 1; }
  printf '%s\n' "$sid" >"$c/sid"
  echo "$agent: cell $c (sid $sid)"
done
