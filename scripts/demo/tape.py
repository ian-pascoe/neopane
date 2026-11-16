"""Reads a demo tape's markers and the times VHS gives them.

A tape marks what the edits show with comments, which VHS ignores:

    # caption: text        shown from here until the next caption
    # caption:             clears it
    # keys: Ctrl-b %       a key badge, shown from here for a moment
    # focus: X Y W H       the social cut's camera zooms onto this part of
                           the recording, as fractions of its width and height
    # focus: all           and back out to all of it
    # highlight: X Y W H   the social cut outlines this part for a moment
    # speed: 2             the social cut plays from here at this speed

Their times come from replaying the tape's timing: VHS records only while
shown, a Type takes its TypingSpeed per character, a key its TypingSpeed per
press, and Sleep its time. So the shown part may not use Wait, whose time
can't be known. VHS runs a little slower than the tape says, evenly, so the
times are stretched to the recording's own length.
"""

import json
import re
import subprocess

KEY_RE = re.compile(
    r"^(Enter|Escape|Space|Backspace|Delete|Tab|Up|Down|Left|Right|PageUp|PageDown"
    r"|Home|End|Insert|(?:Ctrl|Alt|Shift)\+\S+)(@\S+)?(?:\s+(\d+))?$"
)
MARK_RE = re.compile(r"#\s*(caption|keys|focus|highlight|speed):\s*(.*)$")


def seconds(text):
    m = re.fullmatch(r"([\d.]+)(ms|s|m)?", text)
    if not m:
        raise ValueError(f"not a time: {text}")
    n = float(m.group(1))
    return {"ms": n / 1000, "s": n, "m": n * 60, None: n}[m.group(2)]


def rect(text, where):
    if text == "all":
        return None
    parts = [float(p) for p in text.split()]
    if len(parts) != 4:
        raise ValueError(f"{where}: a rectangle is X Y W H, or all")
    return parts


def read(tape):
    """The tape's marks, as (seconds into the shown part, kind, value), and
    the shown part's length."""
    speed = 0.05  # VHS's default TypingSpeed
    t = 0.0
    shown = True
    marks = []
    with open(tape) as f:
        lines = f.read().splitlines()
    for lineno, raw in enumerate(lines, 1):
        line = raw.strip()
        where = f"{tape}:{lineno}"
        if m := MARK_RE.match(line):
            if not shown:
                continue
            kind, value = m.group(1), m.group(2).strip()
            if kind in ("focus", "highlight"):
                value = rect(value, where)
                if kind == "highlight" and value is None:
                    raise ValueError(f"{where}: highlight needs a rectangle")
            elif kind == "speed":
                value = float(value)
            marks.append((t, kind, value))
            continue
        if not line or line.startswith("#"):
            continue
        word = line.split()[0]
        if word == "Set" and line.split()[1] == "TypingSpeed":
            speed = seconds(line.split()[2])
        elif word == "Hide":
            shown = False
        elif word == "Show":
            shown = True
        elif not shown:
            continue
        elif word == "Sleep":
            t += seconds(line.split()[1])
        elif word.startswith("Type"):
            each = seconds(word[5:]) if word.startswith("Type@") else speed
            m = re.match(r'Type\S*\s+(["`\'])(.*)\1$', line)
            if not m:
                raise ValueError(f"{where}: can't read {line}")
            t += each * len(m.group(2))
        elif m := KEY_RE.match(line):
            each = seconds(m.group(2)[1:]) if m.group(2) else speed
            t += each * int(m.group(3) or 1)
        elif word.startswith("Wait"):
            raise ValueError(f"{where}: Wait while shown; use Sleep")
    return marks, t


def duration(video):
    out = subprocess.run(
        [
            "ffprobe",
            "-v",
            "error",
            "-show_entries",
            "format=duration",
            "-of",
            "csv=p=0",
            video,
        ],
        check=True,
        capture_output=True,
        text=True,
    )
    return float(out.stdout.strip())


def marks_in(tape, video):
    """The tape's marks at their times in the recording, and its length."""
    marks, length = read(tape)
    real = duration(video)
    stretch = real / length
    return [(t * stretch, kind, value) for t, kind, value in marks], real


def timeline(tape, video):
    """What the social cut needs: the recording's segments, each with its
    speed, and every mark at its time in the cut."""
    marks, real = marks_in(tape, video)
    changes = [(0.0, 1.0)] + [(t, v) for t, k, v in marks if k == "speed"]
    segments = []
    out = 0.0
    for i, (start, rate) in enumerate(changes):
        end = changes[i + 1][0] if i + 1 < len(changes) else real
        if end > start:
            segments.append({"from": start, "to": end, "rate": rate, "at": out})
            out += (end - start) / rate

    def cut_time(t):
        for s in segments:
            if s["from"] <= t <= s["to"]:
                return s["at"] + (t - s["from"]) / s["rate"]
        return out

    events = {"caption": [], "keys": [], "focus": [], "highlight": []}
    for t, kind, value in marks:
        if kind == "speed":
            continue
        key = "text" if kind in ("caption", "keys") else "rect"
        events[kind].append({"at": round(cut_time(t), 3), key: value})
    return {
        "segments": [{k: round(v, 3) for k, v in s.items()} for s in segments],
        "captions": events["caption"],
        "keys": events["keys"],
        "focus": events["focus"],
        "highlights": events["highlight"],
        "duration": round(out, 3),
    }


if __name__ == "__main__":
    import sys

    tape, video, out = sys.argv[1:4]
    data = timeline(tape, video)
    with open(out, "w") as f:
        json.dump(data, f, indent=2)
    print(f"{out}: {data['duration']:.1f}s of recording in the cut")
