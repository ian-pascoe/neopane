#!/usr/bin/env bash
# Records a demo: makes the throwaway worktree if missing, runs the tape
# with VHS, cleans up its multiplexer, daemons, swap files and edits, and
# burns in the tape's captions and key badges.
#
#   scripts/demo/record.sh tmux-core
#
# It writes target/demo/tmux-core.mp4, captioned for the README, and the
# social cut, target/demo/tmux-core.social.mp4 (social.sh).
set -euo pipefail
here="$(cd "$(dirname "$0")" && pwd)"
. "$here/env.sh"
name="${1:?usage: $0 TAPE-NAME, such as tmux-core}"
tape="$here/$name.tape"
out="$demo_repo/target/demo"
mkdir -p "$out"

cleanup() {
  tmux -L "$demo_tmux_socket" kill-server 2>/dev/null || true
  ZELLIJ_SOCKET_DIR="$demo_zellij_sockets" zellij delete-session --force \
    "$demo_zellij_session" >/dev/null 2>&1 || true
  NEOPANE_TMPDIR="$demo_tmpdir" "$demo_repo/target/release/neopane" kill --all --force \
    >/dev/null 2>&1 || true
  # The daemons' Neovim keeps the demo's unsaved edits in swap files, which
  # would ask about recovery next time
  swap="$(nvim --headless -c 'lua io.write(vim.o.directory)' -c q 2>&1)"
  rm -f "${swap%%//}/${demo_work//\//%}"%*.sw? 2>/dev/null || true
  git -C "$demo_work" checkout -q -- . 2>/dev/null || true
  git -C "$demo_work" clean -qfd 2>/dev/null || true
}

(cd "$demo_repo" && cargo build --release --quiet)
if [ ! -d "$demo_work" ]; then
  git -C "$demo_repo" worktree add --detach "$demo_work" main
fi
# rust-analyzer's build scripts, done before it starts, so it indexes sooner
(cd "$demo_work" && cargo check --quiet)
cleanup
trap cleanup EXIT

(cd "$demo_repo" && vhs "$tape" -o "$out/$name.raw.mp4")
python3 "$here/overlay.py" "$tape" "$out/$name.raw.mp4" "$out/$name.mp4"
echo "$out/$name.mp4"
"$here/social.sh" "$name"
