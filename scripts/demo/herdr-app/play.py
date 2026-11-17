"""Plays a demo tape in a Herdr app window, recording the window while shown.

    python3 play.py TAPE WINDOW-ID OUT.mp4

take.sh runs it with the take's environment: HERDR_APP_HOME names the
private window, and HC_SESSION, XDG_STATE_HOME and XDG_RUNTIME_DIR its hc
core. VHS can't record the Herdr app, a GUI app, so this plays the tape's
steps itself, as rex/play.py does in Rex, into the terminal of the pane
the window focuses, found before each step from `herdr-app status` and
the core's doc: Type a character at a time at the tape's TypingSpeed, and
keys, through the core's `terminal.keys`, and Sleep as itself.

hc's keys can't spell `:` or `_` (FALLBACK): a Type holding either goes
whole to the daemon's Neovim instead, a character at a time by its
`nvim_input`, where it would have landed: the tape types them only into
Neovim. A `HerdrApp ARGS` line, which VHS and tape.py skip, runs
`herdr-app ARGS` for what the app's own keys would do, since keys sent to
a terminal never reach the app. Each step starts when tape.py says it
does, so the marks land where tape.py puts them. winrec records the
window from Show until the tape ends.
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
# letters with Shift. Its shifted symbols are missing: it takes `shift+;`,
# but sends Shift with `;` (kitty's `ESC [59;2u`), not `:`
PLAIN = set("abcdefghijklmnopqrstuvwxyz0123456789;/.,-'`[]\\=")
# What a Type may hold that hc can't spell: the whole Type goes to Neovim
FALLBACK = set(":_")


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

    def press(self, terminal, ch):
        """Types `ch` into `terminal`, as hc's keys spell it."""
        if ch == " ":
            key = "space"
        elif ch in PLAIN:
            key = ch
        elif ch.isascii() and ch.isupper():
            key = f"shift+{ch.lower()}"
        else:
            sys.exit(f"hc's keys can't type {ch!r}, and it isn't in FALLBACK")
        self.call("terminal.keys", {"terminal": terminal, "keys": [key]})


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


def nvim_sock():
    """The socket of the take's one daemon's Neovim, under its directory
    (ADR 0004: `$NEOPANE_TMPDIR/neopane-<uid>/<daemon name>/nvim.sock`)."""
    uid = os.getuid()
    socks = glob.glob(
        os.path.join(os.environ["NEOPANE_TMPDIR"], f"neopane-{uid}", "*", "nvim.sock")
    )
    if len(socks) != 1:
        sys.exit(f"want one daemon's Neovim, found {socks}")
    return socks[0]


def nvim_input(ch):
    """Gives character `ch` to the daemon's Neovim, by msgpack-RPC
    `nvim_input`, and checks its reply."""
    data = ("<lt>" if ch == "<" else ch).encode()
    # [0, 1, "nvim_input", [keys]]: a fixarray, fixints and fixstrs
    msg = b"\x94\x00\x01\xaanvim_input\x91" + bytes([0xA0 | len(data)]) + data
    with socket.socket(socket.AF_UNIX) as s:
        s.connect(nvim_sock())
        s.sendall(msg)
        reply = b""
        while len(reply) < 4:
            more = s.recv(4096)
            if not more:
                break
            reply += more
    # [1, 1, nil, count]: a response to request 1, its error nil
    if reply[:4] != b"\x94\x01\x01\xc0":
        sys.exit(f"nvim_input {ch!r}: {reply!r}")


def key_name(word):
    if word in KEYS:
        return KEYS[word]
    mod, _, key = word.partition("+")
    return f"{mod.lower()}+{KEYS.get(key, key.lower())}"


def tape_root():
    return os.path.abspath(os.path.join(here, "..", "..", ".."))


def main():
    path, window, out = sys.argv[1:4]
    core = Core()
    winrec = os.path.join(tape_root(), "target", "demo", "winrec")
    # the recorder, once Show starts it: stopped however the tape ends
    recording = []
    try:
        at = play(path, core, winrec, window, out, recording)
    finally:
        for recorder in recording:
            recorder.terminate()
            recorder.wait()
    print(f"{at:.3f}")


def play(path, core, winrec, window, out, recording):
    """Plays tape `path`, adding the recorder it starts to `recording`:
    the shown part's length."""
    speed = 0.05
    shown = False
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
            recording.append(recorder)
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
            # one channel for the whole text, so its characters keep
            # their order; the terminal found before its time comes
            to_nvim = any(ch in FALLBACK for ch in text)
            terminal = None if to_nvim else core.focused()
            for ch in text:
                wait()
                if to_nvim:
                    nvim_input(ch)
                else:
                    core.press(terminal, ch)
                at += each
                if not shown:
                    time.sleep(each)
        elif word == "HerdrApp":
            wait()
            herdr_app(*shlex.split(line[len("HerdrApp") :]))
        elif m := tape.KEY_RE.match(line):
            each = tape.seconds(m.group(2)[1:]) if m.group(2) else speed
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
    return at


if __name__ == "__main__":
    main()
