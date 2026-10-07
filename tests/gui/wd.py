"""A W3C WebDriver client in the standard library, enough to drive SmartCut.

The GUI is a Tauri 2 program, and on Linux Tauri's webviews are WebKitGTK's.
WebKitGTK ships its own WebDriver (`WebKitWebDriver`, Debian package
webkitgtk-webdriver) and wry hands its first web context to it when the program
is started with TAURI_WEBVIEW_AUTOMATION=true in its environment. That is all
`tauri-driver` does on Linux besides renaming the capabilities, so it is not
needed: this talks to WebKitWebDriver directly and names the program in
`webkitgtk:browserOptions`.

Every window of the program is a top-level browsing context of the one
session: the list window and the editor window are two window handles, and a
command goes to whichever was switched to last.
"""

import json
import os
import signal
import socket
import subprocess
import time
import urllib.error
import urllib.request

# The W3C code points for the keys the scenarios press.
KEY = {
    "Escape": "\ue00c",
    "Enter": "\ue007",
    "Delete": "\ue017",
    "Control": "\ue009",
    "Shift": "\ue008",
    "Alt": "\ue00a",
    "Home": "\ue011",
    "End": "\ue010",
    "Left": "\ue012",
    "Right": "\ue014",
}

ELEMENT = "element-6066-11e4-a52e-4f735466cecf"


class WebDriverError(Exception):
    pass


def free_port():
    s = socket.socket()
    s.bind(("127.0.0.1", 0))
    port = s.getsockname()[1]
    s.close()
    return port


class Driver:
    """The WebKitWebDriver process, started and stopped by us."""

    def __init__(self, binary, env, log_path):
        self.port = free_port()
        self.log = open(log_path, "ab")
        self.proc = subprocess.Popen(
            [binary, f"--port={self.port}"],
            env=env,
            stdout=self.log,
            stderr=subprocess.STDOUT,
            stdin=subprocess.DEVNULL,
            start_new_session=True,
        )
        self.base = f"http://127.0.0.1:{self.port}"
        deadline = time.time() + 20
        while time.time() < deadline:
            try:
                if call("GET", self.base + "/status").get("ready") is not None:
                    return
            except Exception:
                time.sleep(0.2)
        self.stop()
        raise WebDriverError("WebKitWebDriver did not come up")

    def stop(self):
        """The driver and everything it started: the program it launched for
        a session is its child, and is in its process group."""
        if self.proc.poll() is None:
            try:
                os.killpg(self.proc.pid, signal.SIGTERM)
            except ProcessLookupError:
                pass
            try:
                self.proc.wait(10)
            except subprocess.TimeoutExpired:
                os.killpg(self.proc.pid, signal.SIGKILL)
                self.proc.wait(5)
        self.log.close()


def call(method, url, body=None, timeout=120):
    data = None if body is None else json.dumps(body).encode()
    req = urllib.request.Request(url, data=data, method=method)
    req.add_header("Content-Type", "application/json; charset=utf-8")
    try:
        with urllib.request.urlopen(req, timeout=timeout) as r:
            out = json.loads(r.read() or b"{}")
    except urllib.error.HTTPError as e:
        try:
            out = json.loads(e.read())
        except Exception:
            raise WebDriverError(f"{method} {url}: HTTP {e.code}")
        v = out.get("value") or {}
        raise WebDriverError(f"{method} {url}: {v.get('error')}: {v.get('message')}")
    return out.get("value")


class Session:
    def __init__(self, driver, app, args):
        caps = {
            "capabilities": {
                "alwaysMatch": {
                    "webkitgtk:browserOptions": {"binary": app, "args": list(args)},
                }
            }
        }
        v = call("POST", driver.base + "/session", caps, timeout=600)
        self.id = v["sessionId"]
        self.base = f"{driver.base}/session/{self.id}"

    def _(self, method, path, body=None, timeout=120):
        return call(method, self.base + path, body, timeout)

    def quit(self):
        try:
            self._("DELETE", "", timeout=30)
        except Exception:
            pass

    # -- windows ---------------------------------------------------------
    def handles(self):
        return self._("GET", "/window/handles")

    def handle(self):
        return self._("GET", "/window")

    def switch(self, handle):
        self._("POST", "/window", {"handle": handle})

    def title(self):
        return self._("GET", "/title")

    # -- script ----------------------------------------------------------
    def js(self, script, *args):
        return self._("POST", "/execute/sync", {"script": script, "args": list(args)})

    # -- elements --------------------------------------------------------
    def find(self, css):
        v = self._("POST", "/element", {"using": "css selector", "value": css})
        return v[ELEMENT]

    def click(self, css):
        self._("POST", f"/element/{self.find(css)}/click", {})

    def double_click(self, css):
        """Two presses where a hand would put them: on the element, by the
        pointer, so that the page sees mousedown/up/click twice and dblclick."""
        el = {ELEMENT: self.find(css)}
        acts = [{"type": "pointerMove", "origin": el, "x": 0, "y": 0, "duration": 0}]
        for _ in range(2):
            acts += [
                {"type": "pointerDown", "button": 0},
                {"type": "pointerUp", "button": 0},
            ]
        self._("POST", "/actions", {
            "actions": [{"type": "pointer", "id": "mouse",
                         "parameters": {"pointerType": "mouse"}, "actions": acts}]
        })
        self._("DELETE", "/actions")

    # -- keyboard --------------------------------------------------------
    def keys(self, *chords):
        """Each chord is a string like "i", "Escape", "Control+h",
        "Control+Shift+z"; pressed one after another into the focused window."""
        acts = []
        for chord in chords:
            parts = chord.split("+") if len(chord) > 1 else [chord]
            codes = [KEY.get(p, p) for p in parts]
            for c in codes:
                acts.append({"type": "keyDown", "value": c})
            for c in reversed(codes):
                acts.append({"type": "keyUp", "value": c})
        self._("POST", "/actions", {"actions": [{"type": "key", "id": "kbd", "actions": acts}]})
        self._("DELETE", "/actions")

    def type(self, text):
        acts = []
        for ch in text:
            acts += [{"type": "keyDown", "value": ch}, {"type": "keyUp", "value": ch}]
        self._("POST", "/actions", {"actions": [{"type": "key", "id": "kbd", "actions": acts}]})
        self._("DELETE", "/actions")
