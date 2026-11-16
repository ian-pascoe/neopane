"""Record the fixtures in this folder: a JSON-RPC client for an hc core that
keeps every line it sent and received, and a herdr-app runner that keeps its
argv, exit code, stdout and stderr.

Run it only in a private sandbox (docs/research/herdr-app.md, "Sandbox"):
HC_SESSION, XDG_STATE_HOME and HERDR_APP_HOME must name your own core and
window, never the user's. Then, from Python:

    from record import Core, app, do
    c = Core(); c.call("hello"); c.call("doc.get", {"name": "herdr-app"})
    c.save("core/nested-doc.get")
    do("pane.split", {"direction": "right", "pane": "p1"}, save="app/do-pane.split")
"""

import json
import os
import select
import socket
import subprocess
import time

SESSION = os.environ["HC_SESSION"]
SOCK = os.path.join(
    os.environ["XDG_STATE_HOME"], "herdr-core", SESSION, f"{SESSION}.sock"
)
OUT = os.environ.get("FIXTURES", os.path.dirname(os.path.abspath(__file__)))


class Core:
    def __init__(self, path=SOCK):
        self.s = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        self.s.connect(path)
        self.buf = b""
        self.n = 0
        self.log = []  # {"send": msg} and {"recv": msg}, in wire order

    def write(self, msg):
        self.log.append({"send": msg})
        self.s.sendall((json.dumps(msg) + "\n").encode())

    def read(self, timeout=None):
        """One line from the core, or None once `timeout` seconds pass."""
        end = None if timeout is None else time.time() + timeout
        while b"\n" not in self.buf:
            left = None if end is None else max(0.0, end - time.time())
            ready, _, _ = select.select([self.s], [], [], left)
            if not ready:
                return None
            chunk = self.s.recv(65536)
            if not chunk:
                raise EOFError("the core closed the socket")
            self.buf += chunk
        line, self.buf = self.buf.split(b"\n", 1)
        msg = json.loads(line)
        self.log.append({"recv": msg})
        return msg

    def call(self, method, params=None):
        self.n += 1
        self.write(
            {"jsonrpc": "2.0", "id": self.n, "method": method, "params": params or {}}
        )
        while True:
            msg = self.read()
            if msg.get("id") == self.n:
                return msg

    def drain(self, seconds):
        """Keep reading (events) for this long."""
        end = time.time() + seconds
        while (left := end - time.time()) > 0 and self.read(left) is not None:
            pass

    def save(self, name):
        with open(os.path.join(OUT, f"{name}.jsonl"), "w") as fh:
            fh.writelines(json.dumps(e) + "\n" for e in self.log)
        self.log = []


def app(*args, save=None):
    """Run herdr-app; with `save`, keep its reply as `<save>.json`."""
    p = subprocess.run(
        ["herdr-app", *args], capture_output=True, text=True, check=False
    )
    try:
        out = json.loads(p.stdout) if p.stdout.strip() else None
    except json.JSONDecodeError:
        out = p.stdout
    rec = {
        "argv": ["herdr-app", *args],
        "exit": p.returncode,
        "stdout": out,
        "stderr": p.stderr,
    }
    if save:
        with open(os.path.join(OUT, f"{save}.json"), "w") as fh:
            json.dump(rec, fh, indent=2)
            fh.write("\n")
    return rec


def do(command, args, save=None):
    """`herdr-app do COMMAND JSON`."""
    return app("do", command, json.dumps(args), save=save)
