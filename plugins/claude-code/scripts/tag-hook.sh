#!/usr/bin/env bash
# UserPromptSubmit hook: a prompt that is a single tag line is a label for
# the message before it, not a request for the model. Two spellings:
#
#   ptag: decision auth
#   /path:tag decision auth
#
# Block it, with the canonical `ptag: …` line as the reason. Nothing is sent
# to the API and nothing enters the context; Claude Code records the block
# as a `system` entry (subtype `informational`) carrying the reason and the
# original prompt, parented to the message the person had just read, and
# `path` derives `meta.tags` on that message from the reason line. Any other
# prompt passes through untouched. The `/path:tag` spelling needs no command
# file: hooks see the raw prompt before any slash-command lookup.
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
    ptag:*) tags="${prompt#ptag:}" ;;
    /path:tag) tags="" ;;
    "/path:tag "*) tags="${prompt#/path:tag }" ;;
    *) exit 0 ;;
esac

# Nothing after the prefix (or only separators) is not a tag line.
tags="${tags#"${tags%%[![:space:]]*}"}"
tags="${tags%"${tags##*[![:space:]]}"}"
check="${tags//,/ }"
if [ -z "${check//[[:space:]]/}" ]; then
    exit 0
fi

# $tags holds no backslash or double quote (see above), so the reason is a
# valid JSON string body as is.
printf '{"decision":"block","reason":"ptag: %s"}\n' "$tags"
