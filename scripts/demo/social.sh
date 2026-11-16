#!/usr/bin/env bash
# Renders a recorded demo's social cut with Remotion: the window, camera,
# captions, key caps, title, end card and music (scripts/demo/video).
#
#   scripts/demo/social.sh tmux-core     # writes target/demo/tmux-core.social.mp4
#
# Music: put a track you may use at scripts/demo/video/public/music.mp3, or
# name another file in public/ with NEOPANE_DEMO_MUSIC. It isn't committed.
set -euo pipefail
here="$(cd "$(dirname "$0")" && pwd)"
. "$here/env.sh"
name="${1:?usage: $0 TAPE-NAME, such as tmux-core}"
out="$demo_repo/target/demo"
public="$here/video/public"
raw="$out/$name.raw.mp4"
[ -f "$raw" ] || { echo "no $raw: run scripts/demo/record.sh $name first" >&2; exit 1; }

cp "$raw" "$public/$name.mp4"
python3 "$here/tape.py" "$here/$name.tape" "$raw" "$public/$name.json"

music="${NEOPANE_DEMO_MUSIC:-music.mp3}"
props='{}'
if [ -f "$public/$music" ]; then
  props="{\"music\": \"$music\"}"
else
  echo "no music: put a track at $public/$music for one" >&2
fi
cd "$here/video"
[ -d node_modules ] || npm ci --silent
npx remotion render "$name" "$out/$name.social.mp4" --props="$props" --log=error
size=$(wc -c <"$out/$name.social.mp4")
echo "$out/$name.social.mp4 ($((size / 1024 / 1024)) MB)"
if [ "$size" -gt $((10 * 1024 * 1024)) ]; then
  echo "over Discord's 10 MB: render again with a higher --crf" >&2
fi
