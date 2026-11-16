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
