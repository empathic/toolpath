#!/bin/sh
# $1 is the project slug directory. Prints one
# TP_SEGMENT=<stem><TAB><bytes><TAB><first sessionId> line per .jsonl
# file in it and nothing else; a missing directory prints nothing. The
# first sessionId is the first non-empty "sessionId" value within the
# first 10 lines, and empty when no line carries one. This approximates
# ConversationReader::read_first_session_id, which takes the top-level
# field of a line that parses as an entry, so the two can differ on a
# file whose first 10 lines hold a malformed line, or a nested object
# with a sessionId key before the top-level one. No single quotes
# anywhere in this file: RemoteCommand::from_script passes the text to
# sh -c as one single-quoted word.
set -u
[ -d "$1" ] || exit 0
for f in "$1"/*.jsonl; do
  [ -f "$f" ] || continue
  stem=${f##*/}
  stem=${stem%.jsonl}
  bytes=$(wc -c < "$f" | tr -d " ")
  sid=$(head -n 10 "$f" | grep -o -m 1 "\"sessionId\"[[:space:]]*:[[:space:]]*\"[^\"][^\"]*\"" | head -n 1 | cut -d "\"" -f 4)
  printf "TP_SEGMENT=%s\t%s\t%s\n" "$stem" "$bytes" "$sid"
done
