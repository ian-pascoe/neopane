#!/usr/bin/env bash
# Runs one latency scenario of docs/latency.md in a Rex session of its own,
# keeps its log, and prints N, median and p95 per criterion.
#
#   scripts/latency.sh SCENARIO [N]
#
# SCENARIO is one of:
#   keys    L4: N keys typed in insert mode
#   split   L1: N `:vsplit`s, each closed again
#   focus   L2: N `<C-w>w`s between two windows, each followed by a key
#   tabs    L3: N `gt`s between two tab pages of two windows each
#   resize  L5: 2N resizes of a split's terminal, smaller and back
#   exec    L6: N launches of `<name> -O a b`, each quit again
#   place   L8: N tab pages of four windows, each closed again
#
# N defaults to 50. BIN is the binary to time, target/release/<name> by
# default: build it first. Its name, the file name of BIN, gives the log
# variable, <NAME>_LOG. The logs go to OUT, /tmp/<name>-latency by default:
# OUT/SCENARIO.log is the one read. The session is killed at the end.
set -euo pipefail

scenario=${1:?usage: scripts/latency.sh SCENARIO [N]}
n=${2:-50}
root=$(cd "$(dirname "$0")/.." && pwd)
bin=${BIN:-}
if [[ -z $bin ]]; then
  name=$(sed -n 's/^name = "\(.*\)"/\1/p' "$root/Cargo.toml" | head -n 1)
  bin=$root/target/release/$name
fi
name=$(basename "$bin")
upper=$(printf '%s' "$name" | tr '[:lower:]' '[:upper:]')
out=${OUT:-/tmp/$name-latency}
[[ -x $bin ]] || {
  echo "no $bin: build it with cargo build --release" >&2
  exit 1
}

# a copy: a binary rebuilt in place under a running one is killed on macOS
work=$out/$scenario
rm -rf "$work"
mkdir -p "$work/tmp" "$work/bin"
cp "$bin" "$work/bin/$name"
bin=$work/bin/$name

created=$(rex new "$name-latency-$scenario" -c "$work" --json)
session=$(printf '%s' "$created" | jq -er '.session_id')
shell=$(printf '%s' "$created" | jq -er '.initial_windows[0].block_ids[0]')
trap 'rex session kill "$session" >/dev/null 2>&1 || true' EXIT

# text to the focused pane, keys to it, a command line run in it. Tried again
# once: the pane Rex had focused may have just closed.
send() { rex send -s "$session" "$1" || { sleep 0.3 && rex send -s "$session" "$1"; }; }
key() { rex send-key -s "$session" "$@" || { sleep 0.3 && rex send-key -s "$session" "$@"; }; }
cmd() {
  send ":$1"
  key enter
}

sleep 1
rex send -s "$session" -b "$shell" \
  "export TMPDIR=$work/tmp ${upper}_LOG=debug; unset XDG_RUNTIME_DIR"
rex send-key -s "$session" -b "$shell" enter
launch() {
  rex send -s "$session" -b "$shell" "$bin --clean $*"
  rex send-key -s "$session" -b "$shell" enter
  sleep 2
}

case $scenario in
keys)
  launch
  send i
  for _ in $(seq "$n"); do
    send x
    sleep 0.05
  done
  key escape
  ;;
split)
  launch
  for _ in $(seq "$n"); do
    cmd vsplit
    sleep 0.6
    cmd close
    sleep 0.4
  done
  ;;
focus)
  launch
  cmd vsplit
  sleep 1
  for _ in $(seq "$n"); do
    send $'\x17w'
    send l
    sleep 0.3
  done
  ;;
tabs)
  launch
  cmd 'vsplit | tabnew | vsplit'
  sleep 2
  for _ in $(seq "$n"); do
    send gt
    sleep 0.4
  done
  ;;
resize)
  launch
  cmd vsplit
  sleep 1
  # the split's terminal: the launcher's has an owner that sizes it
  split=$(rex block ls -s "$session" --json |
    jq -er --arg name "$name" '[.blocks[] | select(.label == $name)][0].block_id')
  resize() {
    rex do -s "$session" -b "$split" com.superlogical.terminal.resize \
      columns="$1" rows="$2" >/dev/null
  }
  for _ in $(seq "$n"); do
    resize 30 20
    sleep 0.3
    resize 40 24
    sleep 0.3
  done
  ;;
exec)
  for _ in $(seq "$n"); do
    launch -O a b
    cmd 'qa!'
    sleep 1
  done
  ;;
place)
  launch
  for _ in $(seq "$n"); do
    cmd 'tabnew | vsplit | vsplit | vsplit'
    sleep 1.2
    cmd tabclose
    sleep 0.6
  done
  ;;
*)
  echo "unknown scenario: $scenario" >&2
  exit 2
  ;;
esac

[[ $scenario == exec ]] || cmd 'qa!'
sleep 2
cat "$work"/tmp/"$name"-*/log >"$out/$scenario.log"
"$bin" debug-latency "$out/$scenario.log"
