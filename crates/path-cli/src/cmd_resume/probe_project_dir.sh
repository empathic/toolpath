#!/bin/sh
# $1 the project directory, $2 the tmux session name, $3 the session
# file. Prints one TP_<NAME>=<value> line per fact and nothing else. No
# single quotes anywhere in this file: RemoteCommand::script passes the
# text to sh -c as one single-quoted word.
set -u
if cd "$1" 2>/dev/null; then p=$(pwd -P); else p=""; fi
printf "TP_PWD=%s\n" "$p"
# The = prefix pins tmux to an exact session-name match; a bare -t NAME
# also matches any session whose name extends it.
if tmux has-session -t "=$2" 2>/dev/null; then s=live; else s=none; fi
printf "TP_SESSION=%s\n" "$s"
if [ -e "$3" ]; then e=yes; else e=no; fi
printf "TP_TARGET=%s\n" "$e"
