#!/usr/bin/env python3
"""Burns a tape's captions and key badges into its recording, for the README.

    scripts/demo/overlay.py TAPE RAW.mp4 OUT.mp4

The marks and their times come from tape.py. The video is padded below with
a bar the captions and badges are drawn in, so they never cover the
terminal; the social cut's focus, highlight and speed marks are ignored.
"""

import os
import subprocess
import sys
import tempfile

from tape import marks_in

BAR = 72
KEYS_HOLD = 1.8
FONT = os.path.expanduser(
    os.environ.get(
        "NEOPANE_DEMO_FONT", "~/Library/Fonts/GeistMonoNerdFont-Medium.otf"
    )
)
BG = "0x1c1e26"


def drawtext(textfile, x, size, color, start, end, box):
    return (
        f"drawtext=fontfile='{FONT}':textfile='{textfile}':x={x}"
        f":y=h-{BAR}+(({BAR}-text_h)/2):expansion=none"
        f":fontsize={size}:fontcolor={color}"
        + (f":box=1:boxcolor={box}:boxborderw=10" if box else "")
        + f":enable='between(t,{start:.3f},{end:.3f})'"
    )


def main():
    tape, raw, out = sys.argv[1:4]
    marks, length = marks_in(tape, raw)
    captions = [(t, v.replace("`", "")) for t, k, v in marks if k == "caption"]
    keys = [(t, v) for t, k, v in marks if k == "keys"]
    tmp = tempfile.mkdtemp(prefix="neopane-overlay-")
    filters = [f"pad=iw:ih+{BAR}:0:0:color={BG}"]

    def text_file(name, text):
        path = os.path.join(tmp, f"{name}.txt")
        with open(path, "w") as f:
            f.write(text)
        return path

    for i, (start, text) in enumerate(captions):
        end = captions[i + 1][0] if i + 1 < len(captions) else length + 60
        if text:
            filters.append(
                drawtext(text_file(f"c{i}", text), "(w-text_w)/2", 30,
                         "0xd5d8da", start, end, None)
            )
    for i, (start, text) in enumerate(keys):
        filters.append(
            drawtext(text_file(f"k{i}", text), "w-text_w-32", 28, "0x1c1e26",
                     start, start + KEYS_HOLD, "0xfab795")
        )
    script = os.path.join(tmp, "filter.txt")
    with open(script, "w") as f:
        f.write(",\n".join(filters))
    subprocess.run(
        ["ffmpeg", "-v", "error", "-y", "-i", raw, "-/filter:v", script,
         "-c:v", "libx264", "-pix_fmt", "yuv420p", "-crf", "20",
         "-movflags", "+faststart", out],
        check=True,
    )
    for start, text in captions:
        print(f"{start:7.2f}s  caption  {text}")
    for start, text in keys:
        print(f"{start:7.2f}s  keys     {text}")
    print(f"{length:7.2f}s  end of the recording")


if __name__ == "__main__":
    main()
