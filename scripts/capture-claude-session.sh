#!/usr/bin/env bash
# Capture one REAL Claude Code session hermetically, as the input of the claude-session-to-codex golden.
#
# Run it yourself, from your own terminal (not from a Claude session):  scripts/capture-claude-session.sh
# Token: $CLAUDE_CODE_OAUTH_TOKEN or $ANTHROPIC_API_KEY if set, else (macOS) the Keychain item
# `claude-code-oauth-token`. The value is never printed, logged, or written; it is passed only to the
# `env -i` child. The model's words are not deterministic: capture once, commit the input, and the
# TEST (scripts/goldens.sh check) is deterministic over the committed input.
set -euo pipefail
cd "$(dirname "$0")/.."

PROMPT='List the files in the current directory, then read the file notes.txt and tell me its first line.'
MODEL=claude-haiku-4-5-20251001

CLAUDE_BIN=$(command -v claude) || { echo "claude not found on PATH" >&2; exit 1; }

# Pick the credential variable; never echo the value.
token_var=""
if [ -n "${CLAUDE_CODE_OAUTH_TOKEN:-}" ]; then
  token_var=CLAUDE_CODE_OAUTH_TOKEN; token=$CLAUDE_CODE_OAUTH_TOKEN
elif [ -n "${ANTHROPIC_API_KEY:-}" ]; then
  token_var=ANTHROPIC_API_KEY; token=$ANTHROPIC_API_KEY
elif command -v security >/dev/null 2>&1 \
  && token=$(security find-generic-password -s claude-code-oauth-token -w 2>/dev/null) && [ -n "$token" ]; then
  token_var=CLAUDE_CODE_OAUTH_TOKEN
fi
if [ -z "$token_var" ]; then
  echo "No credential: export CLAUDE_CODE_OAUTH_TOKEN or ANTHROPIC_API_KEY, or add Keychain item 'claude-code-oauth-token'." >&2
  exit 1
fi

TMP=$(mktemp -d)
trap 'rm -rf "$TMP"' EXIT
HOME_T=$TMP/home PROJ=$TMP/project
mkdir -p "$HOME_T" "$PROJ"
printf 'first line of the notes\nsecond line\n' > "$PROJ/notes.txt"
printf 'unrelated\n' > "$PROJ/other.txt"

(
  cd "$PROJ"
  env -i PATH="$(dirname "$CLAUDE_BIN"):/usr/bin:/bin" HOME="$HOME_T" TMPDIR="$TMP" \
    XDG_CONFIG_HOME="$HOME_T/.config" XDG_DATA_HOME="$HOME_T/.local/share" \
    XDG_STATE_HOME="$HOME_T/.local/state" XDG_CACHE_HOME="$HOME_T/.cache" \
    CLAUDE_CONFIG_DIR="$HOME_T/.claude" "$token_var=$token" \
    "$CLAUDE_BIN" -p "$PROMPT" --model "$MODEL" --output-format json \
    --allowedTools "Read" "Glob" "Bash(ls:*)" > "$TMP/result.json"
)

shopt -s nullglob
transcripts=("$HOME_T"/.claude/projects/*/*.jsonl)
if [ "${#transcripts[@]}" -ne 1 ]; then
  echo "expected exactly one transcript under the temp CLAUDE_CONFIG_DIR, found ${#transcripts[@]}" >&2
  exit 1
fi
SRC=${transcripts[0]}
SESSION=$(basename "$SRC" .jsonl)

# Leak check: no real home, no credential, no email address. Reports file and reason only.
bad=0
if grep -qF "$HOME" "$SRC"; then echo "LEAK: transcript mentions the real home directory" >&2; bad=1; fi
if printf '%s' "$token" | grep -qF -f - "$SRC"; then echo "LEAK: transcript contains the credential" >&2; bad=1; fi
if grep -Eq '[A-Za-z0-9._%+-]+@[A-Za-z0-9.-]+\.[A-Za-z]{2,}' "$SRC"; then echo "LEAK: transcript contains an email address" >&2; bad=1; fi
[ "$bad" -eq 0 ] || { echo "refusing to write goldens/claude-session; nothing was copied" >&2; exit 3; }

OUT=goldens/claude-session
mkdir -p "$OUT"
cp "$SRC" "$OUT/input.jsonl"
cat > "$OUT/capture.json" <<EOF
{
  "captured_at": "$(date -u +%Y-%m-%dT%H:%M:%SZ)",
  "session_id": "$SESSION",
  "input_sha256": "$(shasum -a 256 "$OUT/input.jsonl" | cut -d' ' -f1)",
  "claude_version": "$("$CLAUDE_BIN" --version 2>/dev/null | head -n1)",
  "model": "$MODEL",
  "command": "cd <tmp>/project && env -i HOME=<tmp>/home XDG_*=<tmp> CLAUDE_CONFIG_DIR=<tmp>/home/.claude $token_var=<redacted> claude -p <prompt> --model $MODEL --output-format json --allowedTools Read Glob Bash(ls:*)",
  "prompt": "$PROMPT"
}
EOF
echo "captured $SESSION -> $OUT/input.jsonl"
echo "next: scripts/goldens.sh capture   (derives, exports to codex, writes the golden + manifest), then commit goldens/"
