#!/usr/bin/env bash
# UserPromptSubmit hook: a prompt that is a single `tag: …` line is a label
# for the message before it, not a request for the model.
#
#   tag: decision auth
#
# Block it, with the line itself as the reason. Nothing is sent to the API
# and nothing enters the context; Claude Code records the block as a
# `system` entry (subtype `informational`) carrying the reason and the
# original prompt, parented to the message the person had just read, and
# `path` derives `meta.tags` on that message from it. Any other prompt
# passes through untouched.
#
# Dependency-free on purpose: this runs on every prompt, so it must never
# resolve or download the CLI, and it cannot assume jq, node, or python.

set -euo pipefail

input="$(cat)"

# Pull the "prompt" string out of the hook's JSON. Only a plain single-line
# prompt can be a tag line, so a value carrying any JSON escape (\n, \", …)
# is rejected outright rather than decoded.
prompt="$(printf '%s' "$input" \
    | sed -nE 's/.*"prompt"[[:space:]]*:[[:space:]]*"(([^"\\]|\\.)*)".*/\1/p' \
    | head -n 1)"
case "$prompt" in
    *\\*) exit 0 ;;
esac

# Trim surrounding whitespace.
prompt="${prompt#"${prompt%%[![:space:]]*}"}"
prompt="${prompt%"${prompt##*[![:space:]]}"}"

case "$prompt" in
    tag:*) ;;
    *) exit 0 ;;
esac

# `tag:` with nothing after it (or only separators) is not a tag line.
rest="${prompt#tag:}"
rest="${rest//,/ }"
if [ -z "${rest//[[:space:]]/}" ]; then
    exit 0
fi

# $prompt holds no backslash or double quote (see above), so it is a valid
# JSON string body as is.
printf '{"decision":"block","reason":"%s"}\n' "$prompt"
