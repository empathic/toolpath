#!/bin/sh
# $1.. are claude locations relative to $HOME, probed when command -v
# finds nothing. Prints one TP_<NAME>=<value> line per fact and nothing
# else. No single quotes anywhere in this file: RemoteCommand::script
# passes the text to sh -c as one single-quoted word.
set -u
printf "TP_HOME=%s\n" "$HOME"
c=""
if command -v claude >/dev/null 2>&1; then c=$(command -v claude); fi
for probe in "$@"; do
  if [ -z "$c" ] && [ -x "$HOME/$probe" ]; then c="$HOME/$probe"; fi
done
printf "TP_CLAUDE=%s\n" "$c"
if command -v tmux >/dev/null 2>&1; then t=ok; else t=missing; fi
printf "TP_TMUX=%s\n" "$t"
