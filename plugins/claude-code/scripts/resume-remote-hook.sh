#!/usr/bin/env bash
# UserPromptExpansion hook on `/path:resume`. `--remote <user@host>` with
# no document sends the current session and blocks the expansion with
# the attach command as the reason. Claude Code records the block after
# the hook returns, so the uploaded session ends with the exchange before
# this prompt. Every other argument list, and every case this script
# does not handle, expands into the `resume.md` command.

set -euo pipefail

ensure_path="${CLAUDE_PLUGIN_ROOT}/scripts/ensure-path.sh"

command -v jq >/dev/null 2>&1 || exit 0
input="$(cat)"
args="$(printf '%s' "$input" | jq -r '.command_args // empty' 2>/dev/null)" || exit 0
session_id="$(printf '%s' "$input" | jq -r '.session_id // empty' 2>/dev/null)" || exit 0
cwd="$(printf '%s' "$input" | jq -r '.cwd // empty' 2>/dev/null)" || exit 0
[ -n "$session_id" ] && [ -n "$cwd" ] || exit 0
# Arguments are split on whitespace; a quote means the user wants
# quoting, which the command handles.
case "$args" in
    *\'* | *\"*) exit 0 ;;
esac

block() {
    jq -nc --arg reason "$1" '{decision: "block", reason: $reason, hookSpecificOutput: {hookEventName: "UserPromptExpansion", suppressOriginalPrompt: true}}'
    exit 0
}

set -f
# shellcheck disable=SC2086
set -- $args
set +f
words=("$@")

# The hook classifies; the CLI parses. A send is `--remote` with no
# document. An option that takes a value is skipped with its value;
# `--` ends the command's own arguments.
remote=no
dry_run=no
while [ $# -gt 0 ]; do
    case "$1" in
        --) break ;;
        --remote) remote=yes; shift; [ $# -eq 0 ] || shift ;;
        --remote=*) remote=yes; shift ;;
        --dry-run) dry_run=yes; shift ;;
        -C | --cwd | --harness | --url | --picker) shift; [ $# -eq 0 ] || shift ;;
        -*) shift ;;
        *) exit 0 ;;
    esac
done
[ "$remote" = yes ] || exit 0

# The hook runs with no permission prompt, so it uses only a `path` that
# is already installed; `which` never downloads one.
bin="$("$ensure_path" which 2>/dev/null)" || exit 0
help="$("$bin" resume --help 2>/dev/null || true)"
case "$help" in
    *--session*) ;;
    *) exit 0 ;;
esac

cmd=("$bin" resume --session "$session_id" --project "$cwd")
[ "$dry_run" = yes ] || cmd+=(--no-attach)
cmd+=("${words[@]}")

out="$(mktemp)"
err="$(mktemp)"
trap 'rm -f "$out" "$err"' EXIT
if "${cmd[@]}" >"$out" 2>"$err"; then
    [ "$dry_run" = no ] || block "$(cat "$err")"
    # The CLI ends its stderr with the "Attach with:" header; the command
    # itself is the last stdout line.
    block "$(cat "$err")
  $(tail -n 1 "$out")

Detach with ctrl-b d."
else
    status=$?
    block "/path:resume --remote failed: path resume --remote exited $status.

$(tail -n 20 "$err")"
fi
