# Shared by the demo scripts: where a recording's worktree, daemons and
# multiplexer servers live, so record.sh can clean up after session.sh.
# shellcheck shell=bash
demo_repo="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
demo_work="${NEOPANE_DEMO_WORK:-$demo_repo/.worktrees/demo}"
demo_tmpdir="${NEOPANE_DEMO_TMPDIR:-/tmp/neopane-demo}"
demo_tmux_socket=neopane-demo
# zellij finds a session by its socket directory, which must stay short
demo_zellij_sockets=/tmp/neopane-demo-zs
demo_zellij_session=neopane-demo
# herdr's server, by its socket, and its config home: herdr's defaults,
# not yours, and its session state kept out of your ~/.config/herdr
demo_herdr_socket=/tmp/neopane-demo-h.sock
demo_herdr_config="$demo_tmpdir/herdr-config"
# The Herdr app's take: its hc core, by its session, and a box holding the
# core's state, config, runtime directory and home, and the app's home,
# none of them yours. Under /tmp: the core's socket path must stay short
demo_herdr_app_box=/tmp/neopane-demo-ha
demo_herdr_app_session=neopane-demo
