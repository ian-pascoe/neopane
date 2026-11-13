#!/usr/bin/env bash
# Times the built-in tmux pane provider's acts, as docs/latency.md's tmux
# scenario says: each run from its start to its exit, as the daemon runs it,
# on a private tmux server of two panes started here and killed at the end.
#
#   scripts/latency-tmux.sh [N]
#
# For each of focus, focus_direction and open_beside, N runs (50 by
# default), then N, median and p95 (nearest rank) in milliseconds; an act
# the build or the tmux doesn't have says so. BIN is the binary to time,
# target/release/<name> by default: build it first. TMUX_BIN is the tmux,
# the one on PATH by default.
set -euo pipefail

n=${1:-50}
root=$(cd "$(dirname "$0")/.." && pwd)
bin=${BIN:-}
if [[ -z $bin ]]; then
  name=$(sed -n 's/^name = "\(.*\)"/\1/p' "$root/Cargo.toml" | head -n 1)
  bin=$root/target/release/$name
fi
upper=$(basename "$bin" | tr '[:lower:]' '[:upper:]')
[[ -x $bin ]] || {
  echo "no $bin: build it with cargo build --release" >&2
  exit 1
}
[[ -n ${EPOCHREALTIME:-} ]] || {
  echo "needs bash 5 or newer, for EPOCHREALTIME" >&2
  exit 1
}
if [[ -n ${TMUX_BIN:-} ]]; then
  PATH=$(dirname "$TMUX_BIN"):$PATH
fi

work=$(mktemp -d)
sock=$work/sock
t() { tmux -S "$sock" -f /dev/null "$@"; }
trap 't kill-server >/dev/null 2>&1 || true; rm -rf "$work"' EXIT
unset TMUX TMUX_PANE
t new-session -d -x 120 -y 40
t split-window -d -h -t %0
pid=$(t display-message -p -t %0 '#{pid}')
export TMUX="$sock,$pid,0"
echo "tmux $(tmux -V | sed 's/^tmux //'), $(uname -sm), N=$n"

# the time `$bin provider builtin tmux CAP` takes with the act's variables,
# in microseconds; its exit status goes to $work/status, its stderr to
# $work/err
run() {
  local cap=$1
  shift
  local start end
  start=${EPOCHREALTIME/./}
  set +e
  env "${upper}_PROTOCOL=1" "${upper}_CAPABILITY=$cap" "$@" \
    "$bin" provider builtin tmux "$cap" >/dev/null 2>"$work/err"
  local status=$?
  set -e
  end=${EPOCHREALTIME/./}
  echo "$status" >"$work/status"
  echo $((end - start))
}

# N, median and p95 of the microseconds in $work/times, in milliseconds
summary() {
  sort -n "$work/times" | awk -v cap="$1" '
    { t[NR] = $1 }
    END {
      p95 = int(NR * 0.95 + 0.999999)
      printf "%-16s %4d  median %7.3f  p95 %7.3f\n", cap, NR, t[int((NR + 1) / 2)] / 1000, t[p95] / 1000
    }'
}

act() {
  local cap=$1
  : >"$work/times"
  local i
  for ((i = 0; i < n; i++)); do
    local took
    case $cap in
    focus)
      took=$(run focus "${upper}_TERMINAL=%$((i % 2))")
      ;;
    focus_direction)
      local dir=right
      ((i % 2)) && dir=left
      took=$(run focus_direction "${upper}_DIRECTION=$dir" "${upper}_COUNT=1")
      ;;
    open_beside)
      took=$(run open_beside "${upper}_ATTACH=sleep 600" "${upper}_ORIGIN=%0" \
        "${upper}_DIRECTION=down" "${upper}_SIZE=5" "${upper}_RATIO=0.25" \
        "${upper}_COLS=60" "${upper}_ROWS=5" "${upper}_TITLE=t" "${upper}_PANE=0-t")
      ;;
    esac
    local status
    status=$(<"$work/status")
    if [[ $status -ne 0 ]]; then
      printf '%-16s exit %s on run %d: %s\n' "$cap" "$status" $((i + 1)) "$(head -n 1 "$work/err")"
      return
    fi
    echo "$took" >>"$work/times"
    if [[ $cap == open_beside ]]; then
      # the new pane goes again, untimed
      t list-panes -t %0 -F '#{pane_id}' | grep -v -x -e %0 -e %1 | while read -r p; do
        t kill-pane -t "$p"
      done
    fi
  done
  summary "$cap"
}

for cap in focus focus_direction open_beside; do
  act "$cap"
done
