"""Plays a demo tape in a Herdr app window, recording the window while shown.

    python3 play.py TAPE WINDOW-ID OUT.mp4

take.sh runs it with the take's environment: HERDR_APP_HOME names the
private window, and HC_SESSION, XDG_STATE_HOME and XDG_RUNTIME_DIR its hc
core. VHS can't record the Herdr app, a GUI app, so this plays the tape's
steps itself, as rex/play.py does in Rex, into the terminal of the pane
the window focuses, found each step from `herdr-app status` and the
core's doc: Type a character at a time at the tape's TypingSpeed, and
keys, through the core's `terminal.keys`, and Sleep as itself.

hc's keys can't spell a shifted symbol, `:` or `_`: Shift+; reaches the
terminal as Shift+; and not `:`. Those few go to the daemon's Neovim
instead, by its `nvim_input`, where they'd have landed: the tape types
them only into Neovim. A `HerdrApp ARGS` line, which VHS and tape.py
skip, runs `herdr-app ARGS` for what the app's own keys would do, since
keys sent to a terminal never reach the app. Each step starts when
tape.py says it does, so the marks land where tape.py puts them. winrec
records the window from Show until the tape ends.
"""

import glob
import json
import os
import shlex
import socket
import subprocess
import sys
import time

here = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, os.path.dirname(here))
import tape

KEYS = {
    "Enter": "enter",
    "Escape": "esc",
    "Space": "space",
    "Backspace": "backspace",
    "Delete": "delete",
    "Tab": "tab",
    "Up": "up",
    "Down": "down",
    "Left": "left",
    "Right": "right",
}

# What hc's keys spell as typed: a US keyboard's unshifted keys, and
# letters with Shift
PLAIN = set("abcdefghijklmnopqrstuvwxyz0123456789;/.,-'`[]\\=")


def herdr_app(*args):
    out = subprocess.run(
        ["herdr-app", *args], check=True, capture_output=True, text=True
    ).stdout
    return json.loads(out) if out.strip() else None


class Core:
    """The take's hc core, on a connection of its own: one JSON-RPC
    request a line."""

    def __init__(self):
        run = os.environ["XDG_RUNTIME_DIR"]
        path = os.path.join(run, "herdr-core", os.environ["HC_SESSION"] + ".sock")
        self.sock = socket.socket(socket.AF_UNIX)
        self.sock.connect(path)
        self.reader = self.sock.makefile("rb")
        self.id = 0

    def call(self, method, params):
        self.id += 1
        line = {"jsonrpc": "2.0", "id": self.id, "method": method, "params": params}
        self.sock.sendall(json.dumps(line).encode() + b"\n")
        while True:
            reply = json.loads(self.reader.readline())
            if reply.get("id") == self.id:
                break
        if "error" in reply:
            sys.exit(f"hc core {method}: {reply['error']}")
        return reply["result"]

    def focused(self):
        """The terminal of the pane the window focuses."""
        pane = herdr_app("status")["focused_pane"]
        doc = self.call("doc.get", {"name": "herdr-app"})["value"]
        found = find(doc, pane)
        if not found:
            sys.exit(f"pane {pane} has no terminal in the doc")
        return found


def find(node, pane):
    if isinstance(node, dict):
        if node.get("id") == pane and isinstance(node.get("content"), dict):
            return node["content"].get("terminal")
        nodes = node.values()
    elif isinstance(node, list):
        nodes = node
    else:
        return None
    for child in nodes:
        if found := find(child, pane):
            return found
    return None


def nvim_input(keys):
    """Gives `keys` to the daemon's Neovim, by msgpack-RPC `nvim_input`."""
    uid = os.getuid()
    socks = glob.glob(
        os.path.join(os.environ["NEOPANE_TMPDIR"], f"neopane-{uid}", "*", "nvim.sock")
    )
    if len(socks) != 1:
        sys.exit(f"want one daemon's Neovim, found {socks}")
    data = keys.encode()
    assert len(data) < 32
    # [0, msgid, "nvim_input", [keys]]: a fixarray, fixints and fixstrs
    msg = b"\x94\x00\x01\xaanvim_input\x91" + bytes([0xA0 | len(data)]) + data
    with socket.socket(socket.AF_UNIX) as s:
        s.connect(socks[0])
        s.sendall(msg)
        s.recv(4096)


def key_name(word):
    if word in KEYS:
        return KEYS[word]
    mod, _, key = word.partition("+")
    return f"{mod.lower()}+{KEYS.get(key, key.lower())}"


def main():
    path, window, out = sys.argv[1:4]
    core = Core()
    winrec = os.path.join(here, "..", "..", "..", "target", "demo", "winrec")
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

    def press(terminal, ch):
        if ch == " ":
            core.call("terminal.keys", {"terminal": terminal, "keys": ["space"]})
        elif ch in PLAIN:
            core.call("terminal.keys", {"terminal": terminal, "keys": [ch]})
        elif ch.isupper():
            core.call(
                "terminal.keys", {"terminal": terminal, "keys": [f"shift+{ch.lower()}"]}
            )
        else:
            nvim_input(ch)

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
            wait()
            terminal = core.focused()
            for ch in text:
                wait()
                press(terminal, ch)
                at += each
                if not shown:
                    time.sleep(each)
        elif word == "HerdrApp":
            wait()
            herdr_app(*shlex.split(line[len("HerdrApp") :]))
        elif m := tape.KEY_RE.match(line):
            each = tape.seconds(m.group(2)[1:]) if m.group(2) else speed
            wait()
            terminal = core.focused()
            for _ in range(int(m.group(3) or 1)):
                wait()
                core.call(
                    "terminal.keys",
                    {"terminal": terminal, "keys": [key_name(m.group(1))]},
                )
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


if __name__ == "__main__":
    main()
