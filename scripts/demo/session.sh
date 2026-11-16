#!/usr/bin/env bash
# Starts a multiplexer for a demo recording, in the throwaway worktree, cut
# off from your own sessions: its own server or session, no inherited
# multiplexer or neopane variables, neopane from target/release, and
# daemons kept under their own NEOPANE_TMPDIR. A tape types it while hidden.
#
#   scripts/demo/session.sh tmux|zellij|herdr
set -euo pipefail
here="$(cd "$(dirname "$0")" && pwd)"
repo="$(cd "$here/../.." && pwd)"
. "$here/env.sh"

for v in $(compgen -e | grep -E '^(REX_|TMUX|ZELLIJ|HERDR_|NEOPANE|TERM_PROGRAM)'); do
  unset "$v"
done
export NEOPANE_TMPDIR="$demo_tmpdir"
export PATH="$repo/target/release:$PATH"
export SHELL="$here/shell.sh" EDITOR=neopane
mkdir -p "$NEOPANE_TMPDIR"
cd "$demo_work"

case "${1:-}" in
tmux) exec tmux -L "$demo_tmux_socket" -f "$here/tmux.conf" new-session -s demo ;;
zellij)
  mkdir -p "$demo_zellij_sockets"
  export ZELLIJ_SOCKET_DIR="$demo_zellij_sockets" ZELLIJ_CONFIG_DIR="$here/zellij"
  exec zellij --session "$demo_zellij_session"
  ;;
*)
  echo "usage: $0 tmux|zellij" >&2
  exit 2
  ;;
esac
