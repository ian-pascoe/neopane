#!/usr/bin/env bash
# Records a Herdr app tape: record.sh runs it in VHS's place for
# herdr-app-* tapes.
#
#   scripts/demo/herdr-app/take.sh TAPE OUT.mp4
#
# Your own Herdr app window, its hc core and your focus are left alone:
# the take starts a private hc core, its own HC_SESSION, its state,
# config, runtime directory and HOME in a box under /tmp, and a private
# Herdr app window on it, its own HERDR_APP_HOME there, with a larger
# font and no sidebar. That window opens on your focused workspace, so
# it is moved at once to an AeroSpace workspace no monitor shows,
# herdrappdemo, and your workspaces and focused window shown again; it is
# maximized there, with no workspace shown, and winrec records it hidden
# as it is. After, even on failure, the window is quit by its own home,
# the core stopped, and the box removed.
set -euo pipefail
here="$(cd "$(dirname "$0")" && pwd)"
demo="$(dirname "$here")"
. "$demo/env.sh"
tape="${1:?usage: $0 TAPE OUT.mp4}"
out="${2:?usage: $0 TAPE OUT.mp4}"
app=/Applications/Herdr.app/Contents/MacOS/Herdr
box="$demo_herdr_app_box"
winrec="$demo_repo/target/demo/winrec"
hidden=herdrappdemo
# The terminal font size for the take, in points: about tmux's columns on
# a 2048-point-wide monitor
font_size="${NEOPANE_DEMO_HERDR_APP_FONT:-22}"
# The window's tab bar, in points, cropped off: the social cut draws its
# own title bar
title=40

# /opt/homebrew/bin, for aerospace: last, not ahead of your own ffmpeg
PATH="$PATH:/opt/homebrew/bin"
[ -x "$app" ] || {
  echo "the Herdr app take needs Herdr.app in /Applications" >&2
  exit 1
}
command -v aerospace >/dev/null || {
  echo "the Herdr app take needs AeroSpace, to keep its window off screen" >&2
  exit 1
}
hc_exe="$(command -v hc || true)"
[ -n "$hc_exe" ] || {
  echo "the Herdr app take needs hc, the hc core's command" >&2
  exit 1
}
if [ ! -x "$winrec" ] || [ "$demo/rex/winrec.swift" -nt "$winrec" ]; then
  swiftc -O -swift-version 5 -o "$winrec" "$demo/rex/winrec.swift" 2>/dev/null
fi

# What AeroSpace shows now, shown again once the window is hidden
focused="$(aerospace list-workspaces --focused)"
visible="$(aerospace list-workspaces --monitor all --visible)"
focused_window="$(aerospace list-windows --focused --format '%{window-id}' 2>/dev/null || true)"
shown_again=
show_again() {
  [ -n "$shown_again" ] && return 0
  shown_again=1
  for ws in $visible; do
    [ "$ws" = "$focused" ] || aerospace workspace "$ws" 2>/dev/null || true
  done
  aerospace workspace "$focused" 2>/dev/null || true
  if [ -n "$focused_window" ]; then
    aerospace focus --window-id "$focused_window" 2>/dev/null || true
  fi
}

# The box: the core's and the app's environment, with none of yours. The
# panes get yours back (shell.sh), through the DEMO_ variables
rm -rf "$box"
mkdir -p "$box"/{state,config,home,apphome,bin}
mkdir -m 700 "$box/run"
# a copy of hc, not a link: the app may update its core in place
cp "$(readlink -f "$hc_exe")" "$box/bin/herdr-core"
ln -s herdr-core "$box/bin/hc"
ln -s "$app" "$box/bin/herdr-app"
# your fonts, for the app to find its font from the box's HOME
mkdir -p "$box/home/Library"
ln -s "$HOME/Library/Fonts" "$box/home/Library/Fonts"
printf '{"core": "%s", "font": "GeistMono Nerd Font", "font_size": %s, "telemetry": false}\n' \
  "$box/bin/herdr-core" "$font_size" >"$box/apphome/settings.json"
# no welcome overlay, and no panels taking its width
printf '{"frame.welcomed": "skipped", "workspace.panels": []}\n' >"$box/apphome/shared.json"
# no "Updated to" pill: this version has run here before
printf '{"last_run": "%s", "presented": []}\n' \
  "$("$app" --version | awk '{print $2}')" >"$box/apphome/update-notes.json"

for v in $(compgen -e | grep -E '^(HC_|REX_|TMUX|ZELLIJ|HERDR_|NEOPANE|TERM_PROGRAM)'); do
  unset "$v"
done
unset DBUS_SESSION_BUS_ADDRESS
export DEMO_HOME="$HOME"
export DEMO_XDG_CONFIG_HOME="${XDG_CONFIG_HOME-unset}"
export DEMO_XDG_STATE_HOME="${XDG_STATE_HOME-unset}"
export DEMO_XDG_RUNTIME_DIR="${XDG_RUNTIME_DIR-unset}"
export HC_SESSION="$demo_herdr_app_session"
export XDG_STATE_HOME="$box/state" XDG_CONFIG_HOME="$box/config"
export XDG_RUNTIME_DIR="$box/run" HOME="$box/home"
export HERDR_APP_HOME="$box/apphome"
export NEOPANE_TMPDIR="$demo_tmpdir"
export PATH="$box/bin:$demo_repo/target/release:$PATH"
export SHELL="$here/shell.sh"
# No update checks, so no toast offering a newer hc, and no update
# downloaded for the app you share; no sounds
export HERDR_RELEASES_URL=http://127.0.0.1:9 HERDR_APP_NO_SOUND=1

app_pid=
core_pid=
restore() {
  set +e
  if [ -n "$app_pid" ]; then
    # by its own home: never your window
    herdr-app do app.quit >/dev/null 2>&1
    for _ in $(seq 100); do
      kill -0 "$app_pid" 2>/dev/null || break
      sleep 0.1
    done
    kill -9 "$app_pid" 2>/dev/null
  fi
  # with the app gone, or it starts the core again
  hc server stop >/dev/null 2>&1
  if [ -n "$core_pid" ]; then
    for _ in $(seq 100); do
      kill -0 "$core_pid" 2>/dev/null || break
      sleep 0.1
    done
    kill -9 "$core_pid" 2>/dev/null
  fi
  show_again
  rm -rf "$box"
}
trap restore EXIT

core_pid="$(hc server start | python3 -c 'import json, sys; print(json.load(sys.stdin)["pid"])')"
for _ in $(seq 100); do
  hc server status >/dev/null 2>&1 && break
  sleep 0.1
done

(cd "$box" && exec "$app" </dev/null >"$box/app.log" 2>&1) &
app_pid=$!
window=
for _ in $(seq 100); do
  window="$(aerospace list-windows --monitor all --pid "$app_pid" \
    --format '%{window-id}' 2>/dev/null | head -1 || true)"
  [ -n "$window" ] && break
  sleep 0.1
done
[ -n "$window" ] || {
  echo "the Herdr app opened no window" >&2
  exit 1
}
# floating, so it keeps the size it is given while hidden
aerospace layout --window-id "$window" floating 2>/dev/null || true
aerospace move-node-to-workspace --window-id "$window" "$hidden"
show_again

for _ in $(seq 100); do
  herdr-app status >/dev/null 2>&1 && break
  sleep 0.1
done
herdr-app do window.maximize >/dev/null
herdr-app do layout.hide --id sidebar >/dev/null
# A new home's window has no space, and the core's doc no window, until
# its first: one tab of one pane, running shell.sh
herdr-app do space.new --name neopane --folder "$demo_work" --focus true >/dev/null
for _ in $(seq 100); do
  [ -n "$(herdr-app status | python3 -c \
    'import json, sys; print(json.load(sys.stdin)["focused_pane"] or "")')" ] && break
  sleep 0.1
done
# rust-analyzer gets a head start
sleep 3

read -r w h < <("$winrec" "$window" size)
python3 "$here/play.py" "$tape" "$window" "$out.vfr.mp4" >"$out.length"
length="$(cat "$out.length")"
# The tab bar off, the rest scaled to the 1920x996 the social cut
# expects, the sides filled in the app's background, as it's a little
# taller; frames come only as the window changes, so they are filled in
# to a steady 30 a second, up to the tape's length
python3 - "$w" "$h" "$title" >"$out.crop" <<'EOF2'
import sys
w, h, top = (float(a) for a in sys.argv[1:4])
fit = f"scale=-2:996,pad=1920:996:(ow-iw)/2:0:color=0x16181a"
if (h - top) / w < 996 / 1920:
    fit = f"scale=1920:-2,pad=1920:996:0:(oh-ih)/2:color=0x16181a"
print(f"crop=iw:ih*{(h - top) / h:.5f}:0:ih*{top / h:.5f},{fit}")
EOF2
ffmpeg -v error -y -i "$out.vfr.mp4" \
  -vf "$(cat "$out.crop"),fps=30,tpad=stop_mode=clone:stop_duration=10" \
  -t "$length" -c:v libx264 -crf 18 -pix_fmt yuv420p "$out"
rm -f "$out.vfr.mp4" "$out.length" "$out.crop"
