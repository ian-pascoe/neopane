"""Plays a demo tape in a Rex session, recording the Rex window while shown.

    python3 play.py TAPE SESSION WINDOW-ID OUT.mp4

VHS can't record Rex, a GUI app, so this plays the tape's steps itself:
Type through `rex send`, a character at a time at the tape's TypingSpeed,
keys through `rex send-key`, and Sleep as itself. A `Rex ARGS` line, which
VHS and tape.py skip, runs `rex ARGS` for what Rex's own keys would do,
since `rex send-key` reaches the terminal and not the app: `{session}`,
`{session_id}`, `{window_id}` and `{shell}` in it are filled in. Each step
starts when tape.py says it does, so the marks land where tape.py puts
them. winrec records the window from Show until the tape ends.
"""

import json
import os
import shlex
import subprocess
import sys
import time

here = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, os.path.dirname(here))
import tape

KEYS = {
    "Enter": "enter",
    "Escape": "escape",
    "Space": "space",
    "Backspace": "backspace",
    "Delete": "delete",
    "Tab": "tab",
    "Up": "up",
    "Down": "down",
    "Left": "left",
    "Right": "right",
}


def rex(*args):
    subprocess.run(["rex", *args], check=True, stdout=subprocess.DEVNULL)


def key_name(word):
    if word in KEYS:
        return KEYS[word]
    mod, _, key = word.partition("+")
    return f"{mod.lower()}+{KEYS.get(key, key.lower())}"


def main():
    path, session, window, out = sys.argv[1:5]
    view = json.loads(
        subprocess.run(
            ["rex", "api", "call", "-s", session, "session.view"],
            check=True,
            capture_output=True,
            text=True,
        ).stdout
    )
    fill = {
        "session": session,
        "session_id": view["session_id"],
        "window_id": view["active_window_id"],
        "shell": shlex.quote(os.path.join(here, "shell.sh")),
    }
    winrec = os.path.join(tape_root(), "target", "demo", "winrec")
    speed = 0.05
    shown = False
    recorder = None
    start = 0.0
    at = 0.0  # seconds into the shown part this step starts at

    def wait():
        # hidden steps run as they come; shown ones at their time
        if shown:
            delay = start + at - time.monotonic()
            if delay > 0:
                time.sleep(delay)

    with open(path) as f:
        lines = f.read().splitlines()
    for raw in lines:
        line = raw.strip()
        if not line or line.startswith("#"):
            continue
        word = line.split()[0]
        if word == "Set" and line.split()[1] == "TypingSpeed":
            speed = tape.seconds(line.split()[2])
        elif word in ("Set", "Require", "Output"):
            continue
        elif word == "Hide":
            shown = False
        elif word == "Show":
            recorder = subprocess.Popen(
                [winrec, window, "record", out, "30"],
                stdout=subprocess.PIPE,
                text=True,
            )
            if recorder.stdout.readline().strip() != "recording":
                sys.exit("winrec didn't start")
            shown = True
            start = time.monotonic()
            at = 0.0
        elif word == "Sleep":
            pause = tape.seconds(line.split()[1])
            if shown:
                at += pause
                wait()
            else:
                time.sleep(pause)
        elif word.startswith("Type"):
            each = tape.seconds(word[5:]) if word.startswith("Type@") else speed
            text = line.split(None, 1)[1][1:-1]
            for ch in text:
                wait()
                rex("send", "-s", session, ch)
                at += each
                if not shown:
                    time.sleep(each)
        elif word == "Rex":
            wait()
            args = shlex.split(line[len("Rex") :].strip().format(**fill))
            rex(*args)
        elif m := tape.KEY_RE.match(line):
            each = tape.seconds(m.group(2)[1:]) if m.group(2) else speed
            for _ in range(int(m.group(3) or 1)):
                wait()
                rex("send-key", "-s", session, key_name(m.group(1)))
                at += each
                if not shown:
                    time.sleep(each)
        else:
            sys.exit(f"{path}: can't play {line}")
    wait()
    if recorder:
        recorder.terminate()
        recorder.wait()
    print(f"{at:.3f}")


def tape_root():
    return os.path.abspath(os.path.join(here, "..", "..", ".."))


if __name__ == "__main__":
    main()
