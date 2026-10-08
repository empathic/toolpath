#!/usr/bin/env bash
# Hermetic baseline capture of toolpath's transformations over the repo's own fixtures.
#
# For each fixture: a fresh temp HOME, the fixture placed where the harness's adapter
# expects it, `path p derive <harness> --all` to get the Toolpath document (the IR),
# then `path p project claude -i <doc>` to get the Claude JSONL projection.
# Inputs, IR and outputs are hashed into manifest.tsv so a later run can be diffed.
# Nothing touches the real ~/.codex, ~/.copilot, ~/.claude or ~/.toolpath.
#
# Usage: goldens-baseline/capture.sh [out_dir]   (default: goldens-baseline/<path-version>)
set -euo pipefail
REPO=$(cd "$(dirname "$0")/.." && pwd)
PATH_BIN=$(command -v path)
PV=$($PATH_BIN --version | awk '{print $2}')
OUT=${1:-$REPO/goldens-baseline/path-$PV}
TMP=${TMPDIR:-/tmp}/toolpath-hermetic-$$
mkdir -p "$OUT" "$TMP"
trap 'rm -rf "$TMP"' EXIT
sha() { shasum -a 256 "$1" | cut -c1-16; }
MANIFEST="$OUT/manifest.tsv"
{
  echo "# toolpath baseline capture $(date -u +%Y-%m-%dT%H:%M:%SZ)"
  echo "# path $PV at $(readlink -f "$PATH_BIN"); repo HEAD $(git -C "$REPO" rev-parse --short HEAD) ($(git -C "$REPO" branch --show-current))"
  printf 'harness\tfixture\tinput_sha\tir_docs\tir_sorted_sha\tclaude_lines\tclaude_sha\tstatus\n'
} > "$MANIFEST"

capture() {
  local harness=$1 fixture=$2 rel=$3   # rel: where under $HOME the fixture goes
  shift 3; local extra=("$@")          # extra derive flags, e.g. --project for claude/pi
  local name; name=$(basename "$fixture" | sed 's/\.[^.]*$//')
  local home="$TMP/$harness-$name"; mkdir -p "$home/$(dirname "$rel")"
  cp "$fixture" "$home/$rel"
  local dir="$OUT/$harness/$name"; mkdir -p "$dir"
  cp "$fixture" "$dir/input.$(echo "$fixture" | sed 's/.*\.//')"
  local status=ok
  # Hermetic: an EMPTY environment, not an overridden one. Observed 2026-10-08: with only HOME
  # changed, the Claude adapter still read the real store through CLAUDE_CONFIG_DIR from the shell.
  hermetic() {
    env -i PATH="$(dirname "$PATH_BIN"):/usr/bin:/bin" HOME="$home" TMPDIR="$TMP" \
      XDG_CONFIG_HOME="$home/.config" XDG_DATA_HOME="$home/.local/share" XDG_STATE_HOME="$home/.local/state" XDG_CACHE_HOME="$home/.cache" \
      CLAUDE_CONFIG_DIR="$home/.claude" COPILOT_HOME="$home/.copilot" CODEX_HOME="$home/.codex" \
      "$@"
  }
  if ! hermetic "$PATH_BIN" p derive "$harness" "${extra[@]}" --all > "$dir/ir.json" 2> "$dir/derive.stderr"; then
    status="derive-failed"
  fi
  if [ "$status" = ok ] && [ -s "$dir/ir.json" ]; then
    if ! hermetic "$PATH_BIN" p project claude -i "$dir/ir.json" -o "$dir/claude.jsonl" 2> "$dir/project.stderr"; then
      status="project-failed"
    fi
  else
    [ "$status" = ok ] && status="derive-empty"
  fi
  local ir_lines=0 ir_sha=- cl_lines=0 cl_sha=-
  # The raw IR's key order is not stable between runs (observed 2026-10-08: jq -S equal, bytes differ),
  # so the golden is the key-sorted form; the raw stream is kept beside it.
  if [ -s "$dir/ir.json" ]; then
    jq -S -c . "$dir/ir.json" > "$dir/ir.sorted.json"
    ir_lines=$(wc -l < "$dir/ir.sorted.json" | tr -d ' '); ir_sha=$(sha "$dir/ir.sorted.json")
  fi
  [ -s "$dir/claude.jsonl" ] && { cl_lines=$(wc -l < "$dir/claude.jsonl" | tr -d ' '); cl_sha=$(sha "$dir/claude.jsonl"); }
  printf '%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\n' "$harness" "$name" "$(sha "$fixture")" "$ir_lines" "$ir_sha" "$cl_lines" "$cl_sha" "$status" >> "$MANIFEST"
}

F=$REPO/crates
capture codex   "$F/toolpath-codex/tests/fixtures/sample-codex-python.jsonl"   ".codex/sessions/2026/01/01/rollout-2026-01-01T00-00-00-00000000-0000-0000-0000-000000000001.jsonl"
capture codex   "$F/toolpath-codex/tests/fixtures/compacted_session.jsonl"     ".codex/sessions/2026/01/01/rollout-2026-01-01T00-00-00-00000000-0000-0000-0000-000000000002.jsonl"
capture copilot "$F/toolpath-copilot/tests/fixtures/real-session.jsonl"        ".copilot/session-state/00000000-0000-0000-0000-00000000c0f1/events.jsonl"
capture copilot "$F/toolpath-copilot/tests/fixtures/sample-session.jsonl"      ".copilot/session-state/00000000-0000-0000-0000-00000000c0f2/events.jsonl"
# claude sanitizes the project path by replacing / _ . with -; pi wraps the encoded cwd in --...--
# the claude fixture's entries carry cwd=/work, so the project is /work (dir -work)
capture claude  "$F/toolpath-claude/tests/fixtures/compacted_session.jsonl"    ".claude/projects/-work/00000000-0000-0000-0000-00000000c1a0.jsonl" --project /work
capture pi      "$F/toolpath-pi/tests/fixtures/basic_session.jsonl"            ".pi/agent/sessions/--tmp-fixture--/2026-01-01T00-00-00_00000000-0000-0000-0000-0000000000b1.jsonl" --project /tmp/fixture
capture pi      "$F/toolpath-pi/tests/fixtures/compacted_session.jsonl"        ".pi/agent/sessions/--tmp-fixture--/2026-01-01T00-00-00_00000000-0000-0000-0000-0000000000b2.jsonl" --project /tmp/fixture

# Leak check: no output may mention the real home. A hermetic run cannot have seen it.
if grep -rl "/Users/$(id -un)/" "$OUT" --include='*.json' --include='*.jsonl' >/dev/null 2>&1; then
  echo "LEAK: an output references the real home directory" >&2; grep -rl "/Users/$(id -un)/" "$OUT" >&2; exit 3
fi
column -t -s $'\t' "$MANIFEST"
echo "written: $OUT (leak check: clean)"
