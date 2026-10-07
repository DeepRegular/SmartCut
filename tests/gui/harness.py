"""The program under test, started and worked the way a person works it.

One `App` is one run of the GUI: a fresh profile (XDG data, cache and config
directories of its own, so no preference, cache or recovery file of the
machine's own session is read or written), a project file naming the
recordings, and a WebDriver session on the program started on that project.

What it reads back is what the program shows or writes: the DOM of either
window, the project file 保存 writes, the mark files beside the recordings,
the outputs. Nothing in the product is there for the tests.

The one thing WebDriver cannot reach is a native GTK dialog -- the question
Escape asks before throwing an edit away is one (`ask_over` in lib.rs). Those
are found by their window type among the program's windows and answered with
xdotool, by their mnemonic: the buttons are GTK's stock はい(_Y)/いいえ(_N),
Yes/No in English.
"""

import json
import os
import re
import shutil
import subprocess
import time

from wd import Driver, Session, WebDriverError


class Failure(Exception):
    """A step that could not be done: the scenario stops and is a FAIL."""


def wait_for(what, fn, timeout=30, every=0.1):
    deadline = time.time() + timeout
    last = None
    while time.time() < deadline:
        try:
            last = fn()
        except WebDriverError:
            # A window going away under the question is an answer of "not
            # yet", the same as a false one.
            last = None
        if last:
            return last
        time.sleep(every)
    raise Failure(f"timed out waiting for {what}")


def sh(cmd, env):
    return subprocess.run(cmd, shell=True, capture_output=True, text=True, env=env).stdout


COUNTER = re.compile(r"^\s*(\d+)\s*/\s*(\d+)")


class App:
    def __init__(self, cfg, name, clips, settings=None):
        self.cfg = cfg
        self.name = name
        self.dir = os.path.join(cfg.work, "runs", name)
        shutil.rmtree(self.dir, ignore_errors=True)
        os.makedirs(self.dir)
        self.out = os.path.join(self.dir, "out")
        os.makedirs(self.out)
        xdg = {k: os.path.join(self.dir, "xdg", k) for k in ("data", "cache", "config")}
        for d in xdg.values():
            os.makedirs(d)
        self.env = dict(
            os.environ,
            DISPLAY=cfg.display,
            TAURI_WEBVIEW_AUTOMATION="true",
            XDG_DATA_HOME=xdg["data"],
            XDG_CACHE_HOME=xdg["cache"],
            XDG_CONFIG_HOME=xdg["config"],
            GTK_IM_MODULE="gtk-im-context-simple",
            LANG=cfg.lang,
            LC_ALL=cfg.lang,
        )
        self.project = os.path.join(self.dir, "p.scproj")
        body = {
            "smartcut": 1,
            "settings": {"dir": self.out, "subfolder": "", **(settings or {})},
            "clips": clips,
        }
        with open(self.project, "w") as f:
            json.dump(body, f, indent=2)
        self.log = os.path.join(self.dir, "gui.log")
        self.driver = None
        self.s = None

    # -- life ------------------------------------------------------------
    def __enter__(self):
        self.driver = Driver(self.cfg.webdriver, self.env, self.log)
        try:
            self.s = Session(self.driver, self.cfg.gui, [self.project])
            self.main = self.s.handle()
            wait_for("the list window", lambda: "app wired" in self.read_log(), 300, 0.25)
            wait_for("the project's rows", lambda: self.rows() > 0, 60)
        except Exception:
            self.close()
            raise
        return self

    def __exit__(self, *exc):
        self.close()
        return False

    def close(self):
        if self.s:
            self.s.quit()
            self.s = None
        if self.driver:
            pid = self.gui_pid()
            self.driver.stop()
            if pid:
                try:
                    os.kill(pid, 9)
                except ProcessLookupError:
                    pass
            self.driver = None

    def read_log(self):
        try:
            with open(self.log, "rb") as f:
                return f.read().decode("utf-8", "replace")
        except FileNotFoundError:
            return ""

    def gui_pid(self):
        if not self.driver:
            return None
        out = subprocess.run(["pgrep", "-P", str(self.driver.proc.pid)],
                             capture_output=True, text=True).stdout.split()
        return int(out[0]) if out else None

    # -- the list window -------------------------------------------------
    def to_list(self):
        self.s.switch(self.main)

    def rows(self):
        self.to_list()
        return self.s.js("return document.querySelectorAll('#cliplist > li').length")

    def open_row(self, n, name, wait=True):
        """Double-click row `n` (1-based) in the list; with `wait`, until the
        editor is up on `name` and has its counter."""
        self.to_list()
        self.s.double_click(f"#cliplist > li:nth-child({n}) .nm")
        if wait:
            self.editor_ready(name)

    def editor(self):
        """The editor's handle, or None while there is no editor window."""
        others = [h for h in self.s.handles() if h != self.main]
        return others[0] if others else None

    def to_editor(self):
        h = wait_for("the editor window", self.editor, 60)
        self.s.switch(h)
        return h

    def editor_ready(self, name, timeout=120):
        self.to_editor()

        def up():
            st = self.state()
            return st and name in st["title"] and st["frames"] is not None
        wait_for(f"the editor on {name}", up, timeout, 0.2)

    def state(self):
        """What the editor shows: its title, the counter, the plan line, the
        status line, the mark count. Read from the window switched to."""
        v = self.s.js(
            "const t = (id) => { const e = document.getElementById(id); return e ? e.innerText : ''; };"
            "return [t('title'), t('counter'), t('plan-text'), t('status'), t('key-count')];"
        )
        m = COUNTER.match(v[1] or "")
        return {
            "title": v[0].replace("\n", ""),
            "at": int(m.group(1)) if m else None,
            "frames": int(m.group(2)) if m else None,
            "plan": v[2],
            "status": v[3],
            "keys": v[4],
        }

    # -- the editor ------------------------------------------------------
    def goto(self, frame):
        """J, the frame number, Enter: the playhead to a picture of the output."""
        self.s.keys("j")
        wait_for("the jump box", lambda: self.s.js(
            "const b = document.getElementById('counter-jump'); return b && !b.hidden"), 5)
        self.s.keys("Control+a")
        self.s.type(str(frame))
        self.s.keys("Enter")
        wait_for(f"the playhead at {frame}", lambda: self.state()["at"] == frame, 10)

    def cut(self, a, b):
        """IN at output picture `a`, OUT at `b`, Del. Returns the picture
        count after, having checked the cut took out b - a + 1 pictures."""
        before = self.state()["frames"]
        self.goto(a)
        self.s.keys("i")
        self.goto(b)
        self.s.keys("o")
        self.s.keys("Delete")
        want = before - (b - a + 1)
        wait_for(f"the cut {a}-{b} ({before} -> {want} pictures)",
                 lambda: self.state()["frames"] == want, 10)
        return want

    def detect_blank(self):
        """Ctrl+B, and wait for the pass's marks: the recordings made for
        these tests have one black stretch, so the pass puts a mark down at
        each end of it. (The button greys out as the pass starts, so it says
        nothing about whether the answer is in.) Returns the status line."""
        was = self.state()["keys"]
        self.s.keys("Control+b")
        wait_for("the black detection's marks",
                 lambda: self.state()["keys"] != was, 120, 0.25)
        time.sleep(0.3)
        return self.state()["status"]

    def ok(self):
        self.s.keys("Shift+Enter")

    def gone(self, timeout=15):
        """Wait for the editor window to close."""
        wait_for("the editor to close", lambda: self.editor() is None, timeout)

    # -- native dialogs ----------------------------------------------------
    def dialogs(self):
        pid = self.gui_pid()
        if not pid:
            return []
        found = []
        for line in sh("wmctrl -lp", self.env).splitlines():
            f = line.split(None, 4)
            if len(f) < 4 or f[2] != str(pid):
                continue
            kind = sh(f"xprop -id {f[0]} _NET_WM_WINDOW_TYPE", self.env)
            if "DIALOG" in kind:
                found.append(f[0])
        return found

    def answer(self, yes, timeout=10):
        """Answer the dialog that is up, or the one about to be. Returns
        False when none came in `timeout`."""
        try:
            w = wait_for("a dialog", lambda: (self.dialogs() or [None])[0], timeout, 0.2)
        except Failure:
            return False
        sh(f"xdotool windowactivate --sync {w}", self.env)
        time.sleep(0.2)
        sh(f"xdotool key --clearmodifiers {'alt+y' if yes else 'alt+n'}", self.env)
        wait_for("the dialog to go", lambda: w not in self.dialogs(), 10, 0.2)
        return True

    def escape(self, discard=True, expect_dialog=True):
        """Escape in the editor, and the question it asks answered. Returns
        whether a question came."""
        self.s.keys("Escape")
        asked = self.answer(discard, timeout=5 if expect_dialog else 1.5)
        if expect_dialog and not asked:
            raise Failure("Escape asked nothing, although there was a cut to drop")
        return asked

    # -- what the program wrote ------------------------------------------
    def save(self):
        """Ctrl+S in the list window, and the project as it was written."""
        self.to_list()
        was = os.stat(self.project).st_mtime_ns
        self.s.keys("Control+s")
        wait_for("the project to be written",
                 lambda: os.stat(self.project).st_mtime_ns != was, 15)
        time.sleep(0.2)
        with open(self.project) as f:
            return json.load(f)

    def export(self, timeout=300):
        """出力 > 出力開始, until the button reads 出力開始 again. Returns
        the files written."""
        self.to_list()
        self.s.click('button.tab[data-screen="out"]')
        label = wait_for("the start button", lambda: self.s.js(
            "const b = document.getElementById('run-export'); return !b.disabled && b.innerText"), 30)
        self.s.click("#run-export")
        wait_for("the run to start", lambda: self.s.js(
            "return document.getElementById('run-export').innerText") != label, 30, 0.05)
        wait_for("the run to finish", lambda: self.s.js(
            "return document.getElementById('run-export').innerText") == label, timeout, 0.5)
        return sorted(os.path.join(self.out, f) for f in os.listdir(self.out))


def cuts_of(project, n):
    """The cuts of row `n` (1-based) in a saved project, as (a, b) pairs."""
    edit = project["clips"][n - 1].get("edit") or {}
    return [(c["a"], c["b"]) for c in edit.get("cuts") or []]


def pictures(path):
    out = subprocess.run(
        ["ffprobe", "-v", "error", "-count_frames", "-select_streams", "v:0",
         "-show_entries", "stream=nb_read_frames", "-of", "csv=p=0", path],
        capture_output=True, text=True).stdout
    m = re.search(r"\d+", out)
    return int(m.group(0)) if m else None
