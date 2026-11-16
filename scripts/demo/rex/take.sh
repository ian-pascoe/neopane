#!/usr/bin/env bash
# Records a Rex tape: record.sh runs it in VHS's place for rex-* tapes.
#
#   scripts/demo/rex/take.sh TAPE OUT.mp4
#
# It quits Rex.app, if open, and opens it again for the take with a larger
# font and no sidebar, its one window on an AeroSpace workspace of its own,
# rexdemo, showing a session of its own. That workspace is shown for a
# moment, for AeroSpace to size the window to its monitor, then hidden
# again: winrec records the window there, so your screen is left alone.
# After, Rex's settings are put back, and Rex.app quit, or opened again if
# it was open. Don't use Rex meanwhile.
set -euo pipefail
here="$(cd "$(dirname "$0")" && pwd)"
demo="$(dirname "$here")"
. "$demo/env.sh"
tape="${1:?usage: $0 TAPE OUT.mp4}"
out="${2:?usage: $0 TAPE OUT.mp4}"
session=neopane-demo
domain=com.superlogical.rex
app="Rex Beta"
exe="/Applications/$app.app/Contents/MacOS/Rex"
winrec="$demo_repo/target/demo/winrec"
# Rex's font size for the take, in points: about tmux's columns, on a
# 2048-point-wide monitor
font_size="${NEOPANE_DEMO_REX_FONT:-23}"
# Rex's title bar, in points, cropped off: the social cut draws its own
title=36

if [ ! -x "$winrec" ] || [ "$here/winrec.swift" -nt "$winrec" ]; then
  swiftc -O -swift-version 5 -o "$winrec" "$here/winrec.swift" 2>/dev/null
fi

pref() { defaults read "$domain" "$1" 2>/dev/null || echo unset; }
put_pref() {
  case "$3" in
  unset) defaults delete "$domain" "$1" 2>/dev/null || true ;;
  *)
    local value="$3"
    # defaults reads a bool back as 0 or 1, and writes only false or true
    if [ "$2" = bool ]; then
      case "$3" in 0) value=false ;; 1) value=true ;; esac
    fi
    defaults write "$domain" "$1" "-$2" "$value"
    ;;
  esac
}
quit_rex() {
  pkill -f "$exe" 2>/dev/null || true
  for _ in $(seq 50); do
    pgrep -f "$exe" >/dev/null || return 0
    sleep 0.1
  done
  pkill -9 -f "$exe" 2>/dev/null || true
}

was_open=
pgrep -f "$exe" >/dev/null && was_open=1
old_font="$(pref Workspace.terminalFontPointSize)"
old_sidebar="$(pref Workspace.sidebarCollapsed)"
focused="$(aerospace list-workspaces --focused)"
visible="$(aerospace list-workspaces --monitor all --visible)"

restore() {
  rex kill "$session" >/dev/null 2>&1 || true
  quit_rex
  put_pref Workspace.terminalFontPointSize float "$old_font"
  put_pref Workspace.sidebarCollapsed bool "$old_sidebar"
  if [ -n "$was_open" ]; then
    open -g -a "$app"
  fi
}
trap restore EXIT

quit_rex
defaults write "$domain" Workspace.terminalFontPointSize -float "$font_size"
defaults write "$domain" Workspace.sidebarCollapsed -bool true
rex kill "$session" >/dev/null 2>&1 || true
rex new "$session" -c "$demo_work" --shell=none -- "$here/shell.sh" >/dev/null

open -g -a "$app"
client=
for _ in $(seq 100); do
  client="$(rex client ls --json 2>/dev/null | python3 -c '
import json, sys
for c in json.load(sys.stdin)["clients"]:
    if c["info"].get("kind") == "app" and c.get("connection_state") == "connected":
        print(c["client_id"])
        break
' || true)"
  [ -n "$client" ] && break
  sleep 0.1
done
[ -n "$client" ] || {
  echo "Rex.app never connected" >&2
  exit 1
}
# Rex.app opens again the windows it had, if any; else opening it in the
# foreground asks it for one, which then goes to rexdemo
rex_window() {
  aerospace list-windows --monitor all --app-bundle-id "$domain" \
    --format "%{window-id}" 2>/dev/null | head -1 || true
}
window=
for i in $(seq 100); do
  window="$(rex_window)"
  [ -n "$window" ] && break
  [ "$i" = 20 ] && open -a "$app"
  sleep 0.1
done
[ -n "$window" ] || {
  echo "Rex.app opened no window" >&2
  exit 1
}
aerospace move-node-to-workspace --window-id "$window" rexdemo
aerospace layout --window-id "$window" tiling 2>/dev/null || true
id="$(rex api call -s "$session" session.view | python3 -c \
  'import json, sys; print(json.load(sys.stdin)["session_id"])')"
rex "do" -C "$client" session.select "session_id=$id" >/dev/null

# AeroSpace sizes a window only on a shown workspace: show it a moment,
# then each monitor's workspace again, the focused one last
aerospace workspace rexdemo
sleep 1
for ws in $visible; do
  [ "$ws" = "$focused" ] || aerospace workspace "$ws"
done
aerospace workspace "$focused"
# Rex's toast for the session's selection goes, and rust-analyzer gets
# a head start
sleep 3

read -r w h < <("$winrec" "$window" size)
python3 "$here/play.py" "$tape" "$session" "$window" "$out.vfr.mp4" >"$out.length"
length="$(cat "$out.length")"
# Rex's title bar off, the rest scaled to the 1920x996 the social cut
# expects, the sides filled in Rex's background, as it's a little taller;
# frames come only as the window changes, so they are filled in to a
# steady 30 a second, up to the tape's length
python3 - "$w" "$h" "$title" >"$out.crop" <<'EOF2'
import sys
w, h, top = (float(a) for a in sys.argv[1:4])
fit = f"scale=-2:996,pad=1920:996:(ow-iw)/2:0:color=0x191a20"
if (h - top) / w < 996 / 1920:
    fit = f"scale=1920:-2,pad=1920:996:0:(oh-ih)/2:color=0x191a20"
print(f"crop=iw:ih*{(h - top) / h:.5f}:0:ih*{top / h:.5f},{fit}")
EOF2
ffmpeg -v error -y -i "$out.vfr.mp4" \
  -vf "$(cat "$out.crop"),fps=30,tpad=stop_mode=clone:stop_duration=10" \
  -t "$length" -c:v libx264 -crf 18 -pix_fmt yuv420p "$out"
rm -f "$out.vfr.mp4" "$out.length" "$out.crop"
