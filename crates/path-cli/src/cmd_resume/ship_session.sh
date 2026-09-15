#!/bin/sh
# $1 is the session file, $2 the byte count of the JSONL on stdin. The
# JSONL lands in a temporary file of its own in the target directory,
# so two ships of one session do not share a name. ln without -f fails
# on an existing target, so a session file that appeared since the
# probe is kept and the ship fails. No single quotes anywhere in this
# file: RemoteCommand::from_script passes the text to sh -c as one
# single-quoted word.
set -u
umask 077
d=$(dirname "$1")
mkdir -p "$d" || exit 1
t=$(mktemp "$d/.ship.XXXXXX") || exit 1
if cat > "$t" && [ "$(wc -c < "$t")" -eq "$2" ] && ln "$t" "$1"; then
  rm -f "$t"
else
  rm -f "$t"
  exit 1
fi
