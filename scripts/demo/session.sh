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
herdr)
  # herdr reads its config from XDG_CONFIG_HOME and keeps its client's
  # state, the sidebar's, in XDG_STATE_HOME: both fresh each take, its
  # sidebar shown, as the tape expects. bashrc gives the panes yours back,
  # for Neovim's and neopane's
  rm -rf "$demo_herdr_config"
  mkdir -p "$demo_herdr_config/herdr" "$demo_herdr_config/state"
  printf 'onboarding = false\n' >"$demo_herdr_config/herdr/config.toml"
  export DEMO_XDG_CONFIG_HOME="${XDG_CONFIG_HOME-unset}"
  export DEMO_XDG_STATE_HOME="${XDG_STATE_HOME-unset}"
  export XDG_CONFIG_HOME="$demo_herdr_config" XDG_STATE_HOME="$demo_herdr_config/state"
  export HERDR_SOCKET_PATH="$demo_herdr_socket"
  exec herdr
  ;;
*)
  echo "usage: $0 tmux|zellij|herdr" >&2
  exit 2
  ;;
esac
