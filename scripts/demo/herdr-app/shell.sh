#!/usr/bin/env bash
# The shell of a Herdr app demo pane, the take's hc core's SHELL: the core
# starts a pane's program itself, with the core's own environment, so the
# pane gets the demo's here, as session.sh gives the multiplexers', then
# the demo shell. Its arguments, such as the -l the core gives a shell,
# are dropped. The core runs with a HOME and XDG directories of its own
# (take.sh): the pane gets yours back, for Neovim, its config and
# rust-analyzer. The core's own variables stay: HC_TERMINAL_ID,
# HC_TERMINAL_SOCKET and HERDR_APP_HOME, for neopane to choose the Herdr
# app by; and HC_SESSION, which the core sets in every terminal itself:
# nothing in the take reads it, but without it an hc run in a pane, with
# your XDG directories back, would reach your default core.
demo="$(cd "$(dirname "$0")/.." && pwd)"
. "$demo/env.sh"
for v in HOME XDG_CONFIG_HOME XDG_STATE_HOME XDG_RUNTIME_DIR; do
  saved="DEMO_$v"
  case "${!saved-}" in
  "") ;;
  unset) unset "$v" ;;
  *) export "$v=${!saved}" ;;
  esac
  unset "$saved"
done
unset v saved
for v in $(compgen -e | grep -E '^(TMUX|ZELLIJ|HERDR_SOCKET|HERDR_PANE|REX_|NEOPANE)'); do
  unset "$v"
done
export NEOPANE_TMPDIR="$demo_tmpdir"
export PATH="$demo_repo/target/release:$PATH"
export SHELL="$demo/shell.sh" EDITOR=neopane
mkdir -p "$NEOPANE_TMPDIR"
cd "$demo_work" || exit 1
exec "$demo/shell.sh"
