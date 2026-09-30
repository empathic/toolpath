#!/bin/sh
# $1 is the tmux session name, $2 the project directory, $3 the claude
# command, already quoted for the shell tmux hands it to. remain-on-exit
# failed (tmux 3.2+) keeps the pane only after a non-zero exit, so an
# attach shows the startup error of claude while a clean quit removes
# the session. The window option needs -w and a session: target; a bare
# -t "=$1" makes set-option look for a window. No single quotes
# anywhere in this file: RemoteCommand::from_script passes the text to
# sh -c as one single-quoted word.
set -u
exec tmux new-session -d -s "$1" -c "$2" "$3" \; set-option -w -t "=$1:" remain-on-exit failed
