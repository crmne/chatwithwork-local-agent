#!/bin/sh
# Runs as root after the .deb or .rpm is installed. The daemon runs per
# user, so nothing is started here; this only says how.
cat <<'MSG'
Chat with Work Local Agent is installed. As your own user, run:

  cww                                   # the terminal UI: pair, share folders, watch activity

or, step by step:

  cww login                             # pair this computer
  cww roots add ~/Documents             # share a folder
  systemctl --user enable --now cww     # run the daemon in the background
MSG
exit 0
