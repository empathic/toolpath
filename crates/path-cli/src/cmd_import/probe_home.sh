#!/bin/sh
# Prints one TP_HOME=<value> line and nothing else. No single quotes
# anywhere in this file: RemoteCommand::from_script passes the text to
# sh -c as one single-quoted word.
set -u
printf "TP_HOME=%s\n" "$HOME"
