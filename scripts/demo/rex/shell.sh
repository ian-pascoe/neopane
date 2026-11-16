#!/usr/bin/env bash
# The shell of a Rex demo block: Rex starts a block's program itself, so
# it gets the demo's environment here, as session.sh gives the other
# multiplexers', then the demo shell. Rex's own variables stay, for
# neopane to choose Rex by.
demo="$(cd "$(dirname "$0")/.." && pwd)"
. "$demo/env.sh"
for v in $(compgen -e | grep -E '^(TMUX|ZELLIJ|HERDR_|NEOPANE)'); do
  unset "$v"
done
export NEOPANE_TMPDIR="$demo_tmpdir"
export PATH="$demo_repo/target/release:$PATH"
export SHELL="$demo/shell.sh" EDITOR=neopane
mkdir -p "$NEOPANE_TMPDIR"
cd "$demo_work" || exit 1
exec "$demo/shell.sh"
