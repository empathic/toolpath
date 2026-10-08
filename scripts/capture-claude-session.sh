#!/usr/bin/env bash
# Capture one REAL Claude Code session hermetically, as the input of the claude-session-to-codex golden.
#
# Run it yourself, from your own terminal (not from a Claude session):  scripts/capture-claude-session.sh
# Token: $CLAUDE_CODE_OAUTH_TOKEN or $ANTHROPIC_API_KEY if set, else (macOS) the Keychain item
# `claude-code-oauth-token`. The value is never printed, logged, or written; it is passed only to the
# `env -i` child. The model's words are not deterministic: capture once, commit the input, and the
# TEST (scripts/goldens.sh check) is deterministic over the committed input.
#
# Override the binary with CLAUDE_BIN=/path/to/claude. Progress goes to stderr, one line per step.
# On failure the temp dir is KEPT and its path printed; on success it is removed.
set -euo pipefail
cd "$(dirname "$0")/.."

say() { printf 'capture: %s\n' "$*" >&2; }

PROMPT='List the files in the current directory, then read the file notes.txt and tell me its first line.'
MODEL=claude-haiku-4-5-20251001

# `command -v claude` can name a shell function or alias; resolve a real executable.
CLAUDE_BIN=${CLAUDE_BIN:-$(type -P claude || true)}
if [ -z "$CLAUDE_BIN" ] && [ -x "$HOME/.local/bin/claude" ]; then CLAUDE_BIN=$HOME/.local/bin/claude; fi
[ -n "$CLAUDE_BIN" ] || { say "no claude executable found (set CLAUDE_BIN=...)"; exit 1; }
CLAUDE_BIN=$(readlink -f "$CLAUDE_BIN")
[ -x "$CLAUDE_BIN" ] || { say "$CLAUDE_BIN is not executable"; exit 1; }
say "claude binary: $CLAUDE_BIN"

# Pick the credential variable; never echo the value, only where it came from.
token_var="" token="" source_name=""
if [ -n "${CLAUDE_CODE_OAUTH_TOKEN:-}" ]; then
  token_var=CLAUDE_CODE_OAUTH_TOKEN; token=$CLAUDE_CODE_OAUTH_TOKEN; source_name='$CLAUDE_CODE_OAUTH_TOKEN'
elif [ -n "${ANTHROPIC_API_KEY:-}" ]; then
  token_var=ANTHROPIC_API_KEY; token=$ANTHROPIC_API_KEY; source_name='$ANTHROPIC_API_KEY'
elif command -v security >/dev/null 2>&1 \
  && token=$(security find-generic-password -s claude-code-oauth-token -w 2>/dev/null) && [ -n "$token" ]; then
  token_var=CLAUDE_CODE_OAUTH_TOKEN; source_name='Keychain item claude-code-oauth-token'
fi
if [ -z "$token_var" ]; then
  say "no credential: export CLAUDE_CODE_OAUTH_TOKEN or ANTHROPIC_API_KEY, or add Keychain item 'claude-code-oauth-token'"
  exit 1
fi

# Normalise defensively (observed 2026-10-08: `security -w` prints a value holding a stray newline as
# HEX). Decode an all-hex, even-length value, strip all whitespace, then route by prefix. The value is
# only ever piped, never printed.
token=$(printf '%s' "$token" | tr -d '[:space:]')
if [[ "$token" =~ ^[0-9a-f]+$ ]] && [ $(( ${#token} % 2 )) -eq 0 ]; then
  if ! command -v xxd >/dev/null 2>&1; then say "credential from $source_name looks hex-encoded but xxd is not available to decode it"; exit 1; fi
  token=$(printf '%s' "$token" | xxd -r -p | tr -d '[:space:]')
  say "credential from $source_name was hex-encoded; decoded"
fi
case "$token" in
  sk-ant-oat*) token_var=CLAUDE_CODE_OAUTH_TOKEN ;;
  sk-ant-api*) token_var=ANTHROPIC_API_KEY ;;
  *) say "credential from $source_name does not start with sk-ant-oat or sk-ant-api after normalising (length ${#token}); refusing"; exit 1 ;;
esac
say "credential source: $source_name, kind: $token_var (value not shown)"

TMP=$(mktemp -d)
ok=0
cleanup() {
  if [ "$ok" -eq 1 ]; then rm -rf "$TMP"; else say "FAILED; temp dir kept for inspection: $TMP"; fi
}
trap cleanup EXIT
HOME_T=$TMP/home PROJ=$TMP/project
mkdir -p "$HOME_T" "$PROJ"
printf 'first line of the notes\nsecond line\n' > "$PROJ/notes.txt"
printf 'unrelated\n' > "$PROJ/other.txt"
say "temp dir: $TMP"

say "running claude -p ($MODEL)"
rc=0
(
  cd "$PROJ"
  env -i PATH="$(dirname "$CLAUDE_BIN"):/usr/bin:/bin" HOME="$HOME_T" TMPDIR="$TMP" \
    XDG_CONFIG_HOME="$HOME_T/.config" XDG_DATA_HOME="$HOME_T/.local/share" \
    XDG_STATE_HOME="$HOME_T/.local/state" XDG_CACHE_HOME="$HOME_T/.cache" \
    CLAUDE_CONFIG_DIR="$HOME_T/.claude" "$token_var=$token" \
    "$CLAUDE_BIN" -p "$PROMPT" --model "$MODEL" --output-format json \
    --allowedTools "Read" "Glob" "Bash(ls:*)" < /dev/null > "$TMP/result.json" 2> "$TMP/claude.stderr"
) || rc=$?
if [ "$rc" -ne 0 ]; then
  say "claude exited with status $rc"
  # claude -p reports errors as JSON on STDOUT; show its error/result fields, never the credential.
  if command -v jq >/dev/null 2>&1 && [ -s "$TMP/result.json" ]; then
    jq -r '"capture: error=\(.error // "none") result=\((.result // "") | .[0:300])"' "$TMP/result.json" >&2 2>/dev/null || head -c 300 "$TMP/result.json" >&2
  else
    head -c 300 "$TMP/result.json" >&2 || true
  fi
  [ -s "$TMP/claude.stderr" ] && head -c 300 "$TMP/claude.stderr" >&2
  exit "$rc"
fi

shopt -s nullglob
transcripts=("$HOME_T"/.claude/projects/*/*.jsonl)
if [ "${#transcripts[@]}" -ne 1 ]; then
  say "expected exactly one transcript under the temp CLAUDE_CONFIG_DIR, found ${#transcripts[@]}"
  exit 1
fi
SRC=${transcripts[0]}
SESSION=$(basename "$SRC" .jsonl)
say "transcript found: session $SESSION ($(wc -l < "$SRC" | tr -d ' ') lines)"

# Leak check: no real home, no credential, no email address. Reports the reason only.
say "leak check"
bad=0
if grep -qF "$HOME" "$SRC"; then say "LEAK: transcript mentions the real home directory"; bad=1; fi
if printf '%s' "$token" | grep -qF -f - "$SRC"; then say "LEAK: transcript contains the credential"; bad=1; fi
if grep -Eq '[A-Za-z0-9._%+-]+@[A-Za-z0-9.-]+\.[A-Za-z]{2,}' "$SRC"; then say "LEAK: transcript contains an email address"; bad=1; fi
if [ "$bad" -ne 0 ]; then say "refusing to write goldens/claude-session; nothing was copied"; exit 3; fi

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
  "command": "cd <tmp>/project && env -i HOME=<tmp>/home XDG_*=<tmp> CLAUDE_CONFIG_DIR=<tmp>/home/.claude $token_var=<redacted> claude -p <prompt> --model $MODEL --output-format json --allowedTools Read Glob Bash(ls:*) < /dev/null",
  "prompt": "$PROMPT"
}
EOF
ok=1
say "written: $OUT/input.jsonl and $OUT/capture.json"
say "next: scripts/goldens.sh capture   (derives, exports to codex, writes the golden + manifest), then commit goldens/"
