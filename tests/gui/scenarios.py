"""The list window and the editor window, worked together.

Every scenario starts the real program on a project of two small recordings
made here with ffmpeg, works it through WebDriver (and xdotool for the native
questions), and ends on something the program wrote: the row's cuts in the
project 保存 writes, the pictures in an output, a mark file beside a
recording. What the windows show along the way is checked where it is cheap,
but the verdict is always the file.

The handshake under test is the one the review passes kept finding S1s in by
reading: the list hands a row to the editor (`editor-open`), the editor
reports its edit back as it goes (`editor-state`), and キャンセル/Escape asks
the list to put the row back as the visit found it (`editor-cancel`).

Run through tests/run_gui_tests.sh, which checks there is a display and a
driver first.
"""

import fcntl
import hashlib
import json
import os
import re
import shutil
import subprocess
import sys
import time
import traceback

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))

from harness import App, Failure, cuts_of, pictures, sh, wait_for  # noqa: E402
from wd import WebDriverError  # noqa: E402

# The two recordings. 600 pictures each at 30000/1001; the .ts has a black
# stretch from 8 s to 11 s for the black detection to find. The .mkv's
# counter says 601: its last count comes from the container's length (see
# reference notes on "N / M frames"); the real pictures are 600.
FRAMES = 600

# OK closes the window 220 ms after it is pressed (main.js, editor-ok), so a
# second press below that lands on the window on its way out. Nobody can
# double-click a row in the other window that soon after OK, so the default
# delays start above it; `ok_then_reopen_fast` tries the machine-speed ones.
DELAYS_AFTER_OK = (0.3, 0.5, 1.0)


class Cfg:
    def __init__(self):
        self.work = os.environ["GUITEST_WORK"]
        self.gui = os.environ["GUITEST_GUI"]
        self.webdriver = os.environ["GUITEST_WEBDRIVER"]
        self.display = os.environ["DISPLAY"]
        self.lang = os.environ.get("GUITEST_LANG", "ja_JP.UTF-8")
        self.media = os.path.join(self.work, "media")
        self.a = os.path.join(self.media, "a.ts")
        self.b = os.path.join(self.media, "b.mkv")
        self.c = os.path.join(self.media, "c.ts")
        self.d = os.path.join(self.media, "d.ts")


def make_media(cfg):
    os.makedirs(cfg.media, exist_ok=True)
    ff = ["ffmpeg", "-nostdin", "-y", "-loglevel", "error"]
    if not os.path.exists(cfg.a):
        subprocess.run(ff + [
            "-f", "lavfi", "-i",
            "testsrc2=size=720x480:rate=30000/1001:duration=20,"
            "drawbox=enable=between(t\\,8\\,11):color=black:t=fill",
            "-f", "lavfi", "-i", "sine=frequency=440:duration=20:sample_rate=48000",
            "-c:v", "mpeg2video", "-g", "15", "-bf", "2", "-b:v", "4M",
            "-c:a", "mp2", "-b:a", "192k", "-f", "mpegts", cfg.a + ".part"], check=True)
        os.replace(cfg.a + ".part", cfg.a)
    if not os.path.exists(cfg.b):
        subprocess.run(ff + [
            "-f", "lavfi", "-i", "testsrc2=size=640x360:rate=30000/1001:duration=20",
            "-f", "lavfi", "-i", "sine=frequency=660:duration=20:sample_rate=48000",
            "-c:v", "libx264", "-g", "30", "-preset", "veryfast", "-pix_fmt", "yuv420p",
            "-c:a", "aac", "-b:a", "128k", "-f", "matroska", cfg.b + ".part"], check=True)
        os.replace(cfg.b + ".part", cfg.b)
    if not os.path.exists(cfg.c):
        # a.ts's pictures with two sound tracks, for the joins `join_verify`
        # makes: a recording with a track the other lacks.
        subprocess.run(ff + [
            "-f", "lavfi", "-i", "testsrc2=size=720x480:rate=30000/1001:duration=20",
            "-f", "lavfi", "-i", "sine=frequency=550:duration=20:sample_rate=48000",
            "-f", "lavfi", "-i", "sine=frequency=880:duration=20:sample_rate=48000",
            "-map", "0", "-map", "1", "-map", "2",
            "-c:v", "mpeg2video", "-g", "15", "-bf", "2", "-b:v", "4M",
            "-c:a", "mp2", "-b:a", "192k", "-f", "mpegts", cfg.c + ".part"], check=True)
        os.replace(cfg.c + ".part", cfg.c)


def clear_sidecars(cfg):
    for f in os.listdir(cfg.media):
        if f not in ("a.ts", "b.mkv", "c.ts", "d.ts"):
            os.remove(os.path.join(cfg.media, f))


class Tally:
    def __init__(self):
        self.passed = 0
        self.failed = 0

    def check(self, name, good, detail=""):
        if good:
            self.passed += 1
            print(f"  ok    {name:<44} {detail}", flush=True)
        else:
            self.failed += 1
            print(f"  FAIL  {name:<44} {detail}", flush=True)
        return good


def clips(cfg):
    return [{"path": cfg.a}, {"path": cfg.b}]


def fmt(cuts):
    return "[" + ", ".join(f"{a:.3f}-{b:.3f}" for a, b in cuts) + "]"


# -- 1 ---------------------------------------------------------------------
def escape_discard(cfg, t):
    """A cut, Escape, 破棄: the row has no cut."""
    with App(cfg, "escape_discard", clips(cfg)) as app:
        app.open_row(1, "a.ts")
        app.cut(150, 300)
        app.escape(discard=True)
        app.gone()
        p = app.save()
        c = cuts_of(p, 1)
        t.check("1 escape+discard drops the cut", c == [], f"row 1 cuts {fmt(c)}")


# -- 2 ---------------------------------------------------------------------
def escape_after_detection(cfg, t):
    """A cut, then the black detection, then Escape, 破棄: the cut goes, the
    finding stays (the detection is the recording's answer, not an edit).

    On a first visit the row has no edit to go back to, so the project keeps
    none; the finding comes back from the detection cache when the row is
    opened again -- which is what is checked (see `finding_kept`)."""
    with App(cfg, "escape_after_detection", clips(cfg)) as app:
        app.open_row(1, "a.ts")
        app.cut(30, 60)
        said = app.detect_blank()
        app.escape(discard=True)
        app.gone()
        p = app.save()
        c = cuts_of(p, 1)
        t.check("2 escape after detection drops the cut", c == [], f"row 1 cuts {fmt(c)}")
        st = finding_kept(app, t, "2 the detection's finding is kept", said)
        t.check("2 reopened without the cut", st["frames"] == FRAMES,
                f"{st['frames']} pictures (want {FRAMES})")
        app.escape(discard=True, expect_dialog=False)
        app.gone()


def finding_kept(app, t, label, said):
    """Reopen row 1 after a discard: the black stretch is still the row's
    answer -- the pass is greyed as answered, and the ≡ menu offers to turn
    the stretch into marks, which it does only where the band is there.

    The marks the pass put down are not asked for: on a second visit they
    go back with 破棄 (known S2, left after the twenty-first pass -- flat
    detections are never `landed`). The band is the finding."""
    app.open_row(1, "a.ts")
    time.sleep(1.0)
    st = app.state()
    greyed = app.s.js("return document.getElementById('detect-blank').disabled")
    app.s.click("#more")
    time.sleep(0.3)
    band = app.s.js("return !document.getElementById('blank-keys').disabled")
    app.s.keys("Escape")  # puts the menu away, and nothing else
    t.check(label, greyed and band,
            f"reopened: pass {'greyed' if greyed else 'offered again'}, band "
            f"{'there' if band else 'gone'}, marks {st['keys']!r}; the detection said: {said}")
    return st


def escape_after_detection_second_visit(cfg, t):
    """The same on a row that already has a cut from an earlier visit (the
    twenty-first pass's S1: the detection made the whole state what 破棄
    went back to, so the new cut stayed). The earlier cut stays, the new one
    goes."""
    with App(cfg, "escape_after_detection_second_visit", clips(cfg)) as app:
        app.open_row(1, "a.ts")
        app.cut(400, 450)
        app.ok()
        app.gone()
        first = cuts_of(app.save(), 1)
        app.open_row(1, "a.ts")
        app.cut(30, 60)
        said = app.detect_blank()
        app.escape(discard=True)
        app.gone()
        c = cuts_of(app.save(), 1)
        t.check("2b second visit: only the new cut is dropped", c == first and len(c) == 1,
                f"row 1 cuts {fmt(c)} (want {fmt(first)})")
        st = finding_kept(app, t, "2b second visit: the finding is kept", said)
        t.check("2b reopened with the earlier cut", st["frames"] == FRAMES - 51,
                f"{st['frames']} pictures (want {FRAMES - 51})")
        app.escape(discard=True, expect_dialog=False)
        app.gone()


def escape_after_list_detection(cfg, t):
    """A cut in the editor, then the black detection run from the LIST on
    that row (Ctrl+B there), landing on the open editor; Escape, 破棄: the
    cut goes, the finding stays."""
    with App(cfg, "escape_after_list_detection", clips(cfg)) as app:
        app.open_row(1, "a.ts")
        app.cut(30, 60)
        was = app.state()["keys"]
        app.to_list()
        app.s.click("#cliplist > li:nth-child(1) .nm")
        app.s.keys("Control+b")
        app.to_editor()
        wait_for("the list's detection on the editor",
                 lambda: app.state()["keys"] != was, 120, 0.25)
        time.sleep(0.5)
        said = app.state()["status"]
        app.escape(discard=True)
        app.gone()
        c = cuts_of(app.save(), 1)
        t.check("2c list detection then escape drops the cut", c == [], f"row 1 cuts {fmt(c)}")
        finding_kept(app, t, "2c the list's finding is kept", said)
        app.escape(discard=True, expect_dialog=False)
        app.gone()


# -- 3 ---------------------------------------------------------------------
def escape_after_second_press(cfg, t):
    """A cut, the same row double-clicked again in the list (the window comes
    forward, same visit), Escape, 破棄: the cut is gone."""
    with App(cfg, "escape_after_second_press", clips(cfg)) as app:
        app.open_row(1, "a.ts")
        first = app.editor()
        left = app.cut(150, 300)
        app.open_row(1, "a.ts", wait=False)
        time.sleep(1.0)
        h = app.to_editor()
        st = app.state()
        t.check("3 second press keeps the window and its cut",
                h == first and st["frames"] == left,
                f"same window {h == first}, pictures {st['frames']} (want {left})")
        app.escape(discard=True)
        app.gone()
        p = app.save()
        c = cuts_of(p, 1)
        t.check("3 escape+discard after a second press", c == [], f"row 1 cuts {fmt(c)}")


# -- 4 ---------------------------------------------------------------------
def ok_then_reopen(cfg, t, delays=DELAYS_AFTER_OK, name="ok_then_reopen"):
    """A cut, OK, the same row opened again at once, Escape (nothing done in
    this visit, so no question; 破棄 if one comes): the first visit's cut
    stays. Tried with the second press at several delays after OK."""
    with App(cfg, name, clips(cfg)) as app:
        have = 0
        for i, delay in enumerate(delays):
            app.open_row(1, "a.ts")
            # A new cut each round, further along: the row should end the
            # round with one more cut than it began with.
            a = 40 + i * 60
            left = app.cut(a, a + 20)
            app.ok()
            pressed = time.time()
            app.to_list()
            time.sleep(delay)
            app.open_row(1, "a.ts", wait=False)
            clicked = time.time() - pressed
            try:
                app.editor_ready("a.ts", timeout=30)
            except Failure:
                t.check(f"4 reopen {int(delay * 1000)} ms after OK comes up", False,
                        "no editor on the row after the second press")
                return
            st = app.state()
            escaped = time.time() - pressed
            asked = app.escape(discard=True, expect_dialog=False)
            app.gone()
            p = app.save()
            c = cuts_of(p, 1)
            t.check(f"4 OK then reopen after {int(delay * 1000)} ms keeps the cut",
                    len(c) == have + 1 and st["frames"] == left,
                    f"row 1 has {len(c)} cut(s) (want {have + 1}); reopened with "
                    f"{st['frames']} pictures (want {left}); second press at "
                    f"{clicked * 1000:.0f} ms, Escape at {escaped * 1000:.0f} ms after OK, "
                    f"{'asked' if asked else 'did not ask'}")
            have = len(c)


def ok_then_reopen_fast(cfg, t):
    """The same at machine speed: the second press, and Escape, inside the
    220 ms the window takes to close after OK. Not in the default run (no
    hand is that quick); kept to show the race. See the report of the run
    that added this suite."""
    ok_then_reopen(cfg, t, (0.0, 0.1, 0.2), "ok_then_reopen_fast")


# -- 5 ---------------------------------------------------------------------
def switch_then_ctrl_h(cfg, t):
    """Row 1 open with its own marks, row 2 double-clicked, Ctrl+H straight
    away: row 2's .keyframe must not get row 1's marks. Tried at several
    delays between the press and the key."""
    a_marks, b_marks = [400, 500], [50, 100]
    a_file = os.path.join(cfg.media, "a.keyframe")
    b_file = os.path.join(cfg.media, "b.keyframe")

    # Written with bare line feeds: the program writes CRLF, so a file it
    # has written over can be told from the one put down here even where the
    # numbers in it are the same.
    def put(path, frames):
        with open(path, "w", newline="") as f:
            f.write("".join(f"{n}\n" for n in frames))

    def read(path):
        with open(path, "rb") as f:
            body = f.read()
        return [int(x) for x in body.split()], b"\r\n" in body

    put(a_file, a_marks)
    put(b_file, b_marks)
    with App(cfg, "switch_then_ctrl_h", clips(cfg)) as app:
        for delay in (0.0, 0.02, 0.1, 0.3, 1.0):
            app.open_row(1, "a.ts")
            # The window is up on row 1 with its marks read from a.keyframe.
            wait_for("row 1's marks", lambda: app.state()["keys"].startswith("2"), 10)
            app.open_row(2, "b.mkv", wait=False)
            time.sleep(delay)
            app.to_editor()
            app.s.keys("Control+h")
            # A .keyframe is there already, so Ctrl+H asks before writing
            # over it (環境設定 quietOverwrite is off by default). Yes: the
            # question is whether what it writes is the right row's.
            app.answer(True, timeout=2)
            app.editor_ready("b.mkv")
            time.sleep(0.5)
            app.answer(True, timeout=0.5)
            (got_b, wrote_b), (got_a, wrote_a) = read(b_file), read(a_file)
            which = ", ".join(n for n, w in (("b", wrote_b), ("a", wrote_a)) if w) or "none"
            t.check(f"5 Ctrl+H {int(delay * 1000)} ms after switching rows",
                    got_b == b_marks and got_a == a_marks,
                    f"b.keyframe {got_b} (want {b_marks}), a.keyframe {got_a} "
                    f"(want {a_marks}); rewritten: {which}")
            put(a_file, a_marks)
            put(b_file, b_marks)
        # And the key does write, once the row is in: otherwise every line
        # above would pass on a Ctrl+H that did nothing at all.
        app.s.keys("Control+h")
        app.answer(True, timeout=3)
        time.sleep(0.5)
        got_b, wrote_b = read(b_file)
        t.check("5 Ctrl+H on the arrived row writes its marks", wrote_b and got_b == b_marks,
                f"b.keyframe {got_b}, {'rewritten' if wrote_b else 'not written'}")


# -- 6 ---------------------------------------------------------------------
def cut_and_export(cfg, t):
    """Cuts made in the editor, OK, 出力開始: each output has the pictures
    that were kept -- on the .ts and on the .mkv, whose millisecond clock is
    where the editor's bounds once lost the picture after OUT."""
    with App(cfg, "cut_and_export", clips(cfg)) as app:
        app.open_row(1, "a.ts")
        keep_a = app.cut(150, 300)
        app.ok()
        app.gone()
        app.open_row(2, "b.mkv")
        shown = app.state()["frames"]
        # Two cuts on the .mkv: one inside, one ending on a GOP's last picture.
        app.cut(100, 199)
        app.cut(350, 389)
        keep_b = FRAMES - 100 - 40
        app.ok()
        app.gone()
        files = app.export()
        names = [os.path.basename(f) for f in files]
        outs = {n: f for n, f in zip(names, files)}
        got_a = pictures(outs["cut_01_a.ts"]) if "cut_01_a.ts" in outs else None
        got_b = pictures(outs["cut_02_b.mkv"]) if "cut_02_b.mkv" in outs else None
        t.check("6 .ts output has the kept pictures", got_a == keep_a,
                f"{got_a} pictures (want {keep_a}); outputs {names}")
        t.check("6 .mkv output has the kept pictures", got_b == keep_b,
                f"{got_b} pictures (want {keep_b}; editor counted {shown} before the cuts)")
        for f in files:
            os.remove(f)


# -- 7 ---------------------------------------------------------------------
def ok_keeps_and_no_keeps(cfg, t):
    """OK keeps the cut; Escape answered いいえ keeps the window and the cut;
    a row opened and left untouched closes on Escape without asking and keeps
    what it had."""
    with App(cfg, "ok_keeps_and_no_keeps", clips(cfg)) as app:
        app.open_row(1, "a.ts")
        left = app.cut(150, 300)
        app.escape(discard=False)
        time.sleep(0.5)
        still = app.editor() is not None and app.state()["frames"] == left
        t.check("7 Escape answered No keeps the window and cut", still,
                f"window {'up' if app.editor() else 'gone'}")
        app.ok()
        app.gone()
        p = app.save()
        c = cuts_of(p, 1)
        t.check("7 OK keeps the cut", len(c) == 1, f"row 1 cuts {fmt(c)}")
        app.open_row(1, "a.ts")
        st = app.state()
        asked = app.escape(discard=True, expect_dialog=False)
        app.gone()
        p = app.save()
        c2 = cuts_of(p, 1)
        t.check("7 untouched visit: Escape keeps the earlier cut",
                c2 == c and not asked and st["frames"] == left,
                f"cuts {fmt(c2)}; reopened with {st['frames']}; "
                f"Escape {'asked' if asked else 'did not ask'}")


def undo_after_detection(cfg, t):
    """A cut, the black detection, Ctrl+Z: the step taken back is the
    detection's marks, not the cut; OK writes the cut."""
    with App(cfg, "undo_after_detection", clips(cfg)) as app:
        app.open_row(1, "a.ts")
        left = app.cut(30, 60)
        keys_before = app.state()["keys"]
        app.detect_blank()
        keys_found = app.state()["keys"]
        app.s.keys("Control+z")
        time.sleep(0.5)
        st = app.state()
        t.check("7 undo after detection takes the detection back",
                st["frames"] == left and st["keys"] == keys_before,
                f"pictures {st['frames']} (want {left}); marks {keys_before!r} -> "
                f"{keys_found!r} -> {st['keys']!r}")
        app.ok()
        app.gone()
        p = app.save()
        c = cuts_of(p, 1)
        t.check("7 OK after the undo keeps the cut", len(c) == 1, f"row 1 cuts {fmt(c)}")


# -- zoom ------------------------------------------------------------------
def zoom_double_press(cfg, t):
    """open_zoom asked twice in one tick, as Z pressed twice before 拡大表示
    is up asks it (zoomOn is set only by the page's `zoom-ready`): one
    拡大表示, and nothing of it left once the editor is closed. Tauri checks
    for a window of the label before it builds and registers it only once
    built, so two builds could both pass the check (the reason open_editor
    holds a lock). Passes on Linux on cf71706 -- the second ask finds the
    first window -- and is kept as a guard."""
    with App(cfg, "zoom_double_press", clips(cfg)) as app:
        app.open_row(1, "a.ts")
        editor = app.editor()
        app.s.js("const i = window.__TAURI_INTERNALS__.invoke;"
                 "i('open_zoom', {title: 'z1'}); i('open_zoom', {title: 'z2'});")
        time.sleep(4.0)
        handles = app.s.handles()
        zooms = [h for h in handles if h not in (app.main, editor)]
        t.check("zoom: asked twice opens one 拡大表示", len(zooms) <= 1,
                f"{len(zooms)} zoom window(s)")
        app.s.switch(editor)
        app.escape(discard=True, expect_dialog=False)
        time.sleep(2.0)
        left = [h for h in app.s.handles() if h != app.main]
        t.check("zoom: closing the editor leaves no window behind", left == [],
                f"{len(left)} window(s) besides the list")



def cross_double_press(cfg, t):
    """open_cross asked twice in one tick from the list: one 継ぎ目の編集, and
    nothing of it left once it is closed. The same shape as
    zoom_double_press (Tauri checks for the label before it builds and
    registers the window only once built); open_cross had no lock either
    until the thirtieth review."""
    with App(cfg, "cross_double_press", clips(cfg)) as app:
        app.s.js("const i = window.__TAURI_INTERNALS__.invoke;"
                 "i('open_cross', {title: 'c1'}); i('open_cross', {title: 'c2'});")
        time.sleep(4.0)
        crosses = [h for h in app.s.handles() if h != app.main]
        t.check("cross: asked twice opens one 継ぎ目の編集", len(crosses) <= 1,
                f"{len(crosses)} seam window(s)")
        app.s.switch(app.main)
        app.s.js("window.__TAURI_INTERNALS__.invoke('close_cross');")
        time.sleep(2.0)
        left = [h for h in app.s.handles() if h != app.main]
        t.check("cross: closing it leaves no window behind", left == [],
                f"{len(left)} window(s) besides the list")


def zoom_then_close(cfg, t):
    """open_zoom and close_editor asked in one tick from the editor, as Z
    pressed in the moment between OK and the list's close_editor asks them:
    no 拡大表示 may be left with no editor behind it. The editor's Destroyed
    handler closes the zoom window by its label, which Tauri registers only
    once the build is done, so a zoom still being built then stayed up over
    nothing (2 of 3 rounds before lib.rs open_zoom asked EditorUp after the
    build)."""
    with App(cfg, "zoom_then_close", clips(cfg)) as app:
        app.open_row(1, "a.ts")
        app.editor()
        app.s.js("const i = window.__TAURI_INTERNALS__.invoke;"
                 "i('open_zoom', {title: 'z1'}); i('close_editor');")
        time.sleep(4.0)
        left = [h for h in app.s.handles() if h != app.main]
        t.check("zoom: asked as the editor closes leaves no window", left == [],
                f"{len(left)} window(s) besides the list")

# -- 8 ---------------------------------------------------------------------
def close_by_cross(app):
    """The editor's title-bar cross: the window manager's close, as a hand
    gives it (wmctrl sends WM_DELETE_WINDOW), not キャンセル and not OK."""
    pid = app.gui_pid()
    for line in subprocess.run(["wmctrl", "-lp"], capture_output=True, text=True,
                               env=app.env).stdout.splitlines():
        f = line.split(None, 4)
        if len(f) == 5 and f[2] == str(pid) and "\u2014" in f[4] and "SmartCut" not in f[4]:
            subprocess.run(["wmctrl", "-i", "-c", f[0]], env=app.env)
            return
    raise Failure("no editor window to close by its cross")


def cross_then_reopen(cfg, t):
    """A cut, the window closed by its cross (which keeps what was done), the
    same row opened again at several delays, Escape: the cut stays. The
    close here is immediate (no 220 ms of OK), so the press races the news
    of the window going -- the `editorAsks` path of the editor-closed
    handler."""
    with App(cfg, "cross_then_reopen", clips(cfg)) as app:
        have = 0
        for i, delay in enumerate((0.0, 0.1, 0.3, 1.0)):
            app.open_row(1, "a.ts")
            a = 40 + i * 60
            left = app.cut(a, a + 20)
            # Past the editor's 150 ms report debounce: a cross that beats it
            # loses the last cut (known, the debounce is not flushed on
            # destroy).
            time.sleep(0.5)
            close_by_cross(app)
            app.to_list()
            time.sleep(delay)
            app.open_row(1, "a.ts", wait=False)
            try:
                app.editor_ready("a.ts", timeout=30)
            except Failure:
                t.check(f"8 reopen {int(delay * 1000)} ms after the cross comes up", False,
                        "no editor on the row after the second press")
                return
            st = app.state()
            asked = app.escape(discard=True, expect_dialog=False)
            app.gone()
            c = cuts_of(app.save(), 1)
            t.check(f"8 cross then reopen after {int(delay * 1000)} ms keeps the cut",
                    len(c) == have + 1 and st["frames"] == left,
                    f"row 1 has {len(c)} cut(s) (want {have + 1}); reopened with "
                    f"{st['frames']} pictures (want {left}); "
                    f"{'asked' if asked else 'did not ask'}")
            have = len(c)


# -- 9 ---------------------------------------------------------------------
def switch_rows_then_escape(cfg, t):
    """Row 1 cut, row 2 double-clicked (the window moves along: row 1's
    visit ends with its cut), a cut on row 2, Escape, 破棄: row 1 keeps its
    cut, row 2 has none. Then row 2 pressed and row 1 pressed straight back
    at several delays: the window ends on row 1 with its cut, and Escape
    asks nothing and changes nothing."""
    with App(cfg, "switch_rows_then_escape", clips(cfg)) as app:
        app.open_row(1, "a.ts")
        left = app.cut(150, 300)
        app.open_row(2, "b.mkv")
        app.cut(100, 199)
        app.escape(discard=True)
        app.gone()
        p = app.save()
        c1, c2 = cuts_of(p, 1), cuts_of(p, 2)
        t.check("9 switch: row 1 keeps its cut, row 2's goes",
                len(c1) == 1 and c2 == [], f"row 1 {fmt(c1)}, row 2 {fmt(c2)}")
        for delay in (0.0, 0.1, 0.5):
            app.open_row(1, "a.ts")
            app.open_row(2, "b.mkv", wait=False)
            time.sleep(delay)
            app.open_row(1, "a.ts", wait=False)
            app.editor_ready("a.ts")
            time.sleep(1.0)
            st = app.state()
            ok_title = "a.ts" in st["title"]
            asked = app.escape(discard=True, expect_dialog=False)
            app.gone()
            p = app.save()
            d1, d2 = cuts_of(p, 1), cuts_of(p, 2)
            t.check(f"9 row 2 then row 1 again after {int(delay * 1000)} ms",
                    ok_title and st["frames"] == left and d1 == c1 and d2 == [] and not asked,
                    f"on {st['title']!r} with {st['frames']} pictures (want {left}); "
                    f"row 1 {fmt(d1)}, row 2 {fmt(d2)}; {'asked' if asked else 'did not ask'}")


# -- 10 --------------------------------------------------------------------
def double_press_first_open(cfg, t):
    """Row 1 double-clicked twice in quick succession on a list with no
    editor up (the second press lands while the window is being built): one
    window, a first visit -- a cut, Escape, 破棄 leaves the row uncut."""
    with App(cfg, "double_press_first_open", clips(cfg)) as app:
        for delay in (0.0, 0.2):
            app.open_row(1, "a.ts", wait=False)
            time.sleep(delay)
            app.open_row(1, "a.ts", wait=False)
            app.editor_ready("a.ts")
            time.sleep(1.0)
            windows = len(app.s.handles())
            app.to_editor()
            app.cut(150, 300)
            app.escape(discard=True)
            app.gone()
            c = cuts_of(app.save(), 1)
            t.check(f"10 two presses {int(delay * 1000)} ms apart, Escape drops the cut",
                    windows == 2 and c == [], f"{windows} windows (want 2); row 1 cuts {fmt(c)}")


# -- 11 --------------------------------------------------------------------
def ok_then_other_row(cfg, t):
    """A cut on row 1, OK, row 2 double-clicked soon after: the editor comes
    up on row 2 (not blank), row 1 keeps its cut, and 破棄 on row 2 does not
    touch row 1."""
    with App(cfg, "ok_then_other_row", clips(cfg)) as app:
        for i, delay in enumerate((0.3, 0.5)):
            app.open_row(1, "a.ts")
            a = 40 + i * 60
            app.cut(a, a + 20)
            app.ok()
            app.to_list()
            time.sleep(delay)
            app.open_row(2, "b.mkv", wait=False)
            try:
                app.editor_ready("b.mkv", timeout=30)
            except Failure:
                t.check(f"11 row 2 {int(delay * 1000)} ms after OK comes up", False,
                        "no editor on row 2")
                return
            app.cut(100, 120)
            app.escape(discard=True)
            app.gone()
            p = app.save()
            c1, c2 = cuts_of(p, 1), cuts_of(p, 2)
            t.check(f"11 OK on row 1, row 2 after {int(delay * 1000)} ms, 破棄",
                    len(c1) == i + 1 and c2 == [], f"row 1 {fmt(c1)}, row 2 {fmt(c2)}")


# -- 12 --------------------------------------------------------------------
def waveform_keeps_env_prefs(cfg, t):
    """Started with SMARTCUT_CLEAN_JOINS and SMARTCUT_AUDIO_FADE and nothing
    stored, 音声波形 turned on and off in the editor: what the backend holds
    afterwards is still what the environment said. The editor's copy of the
    preferences is never seeded at startup, and it sent the built-in
    defaults for the two (prefs.js `tellBackend`)."""
    app = App(cfg, "waveform_keeps_env_prefs", clips(cfg))
    app.env["SMARTCUT_CLEAN_JOINS"] = "1"
    app.env["SMARTCUT_AUDIO_FADE"] = "0.5"
    with app:
        app.to_list()
        now = app.s.js("return window.__TAURI__.core.invoke('prefs_now')")
        t.check("12 the environment is in force at start",
                 now["cleanJoins"] is True and abs(now["audioFade"] - 0.5) < 1e-6,
                 f"cleanJoins {now['cleanJoins']}, audioFade {now['audioFade']}")
        app.open_row(1, "a.ts")
        for _ in range(2):
            app.s.js("document.getElementById('wave-show').click()")
            time.sleep(1.0)
        app.to_list()
        now = app.s.js("return window.__TAURI__.core.invoke('prefs_now')")
        t.check("12 音声波形 in the editor keeps them",
                 now["cleanJoins"] is True and abs(now["audioFade"] - 0.5) < 1e-6,
                 f"cleanJoins {now['cleanJoins']}, audioFade {now['audioFade']}")
        app.to_editor()
        app.escape(discard=True, expect_dialog=False)
        app.gone()


# -- 13 --------------------------------------------------------------------
def divide_then_edit_part(cfg, t):
    """Row 1 divided into two parts (分割, 2 本), the second part opened, a
    cut made and thrown away with Escape/破棄: the part is still a part (its
    first half still cut away). Opened again, a cut and OK, then 出力開始:
    each part's output has the pictures the editor showed for it."""
    with App(cfg, "divide_then_edit_part", clips(cfg)) as app:
        app.to_list()
        app.s.click("#cliplist > li:nth-child(1) .nm")
        wait_for("分割 to be offered", lambda: app.s.js(
            "return !document.getElementById('divide-clip').disabled"), 60)
        app.s.click("#divide-clip")
        wait_for("the divide box", lambda: app.s.js(
            "return !document.getElementById('divide').hidden"), 5)
        app.s.js("const f = document.getElementById('divide-rule'); f.value = 'parts';"
                 "f.dispatchEvent(new Event('change'));"
                 "const v = document.getElementById('divide-value'); v.value = '2';"
                 "v.dispatchEvent(new Event('input'));")
        wait_for("分割 to be pressable", lambda: app.s.js(
            "return !document.getElementById('divide-ok').disabled"), 30)
        app.s.click("#divide-ok")
        wait_for("three rows", lambda: app.rows() == 3, 10)
        app.open_row(1, "a.ts")
        one = app.state()["frames"]
        app.escape(discard=True, expect_dialog=False)
        app.gone()
        app.open_row(2, "a.ts")
        two = app.state()["frames"]
        app.cut(10, 20)
        app.escape(discard=True)
        app.gone()
        p = app.save()
        c2 = cuts_of(p, 2)
        app.open_row(2, "a.ts")
        again = app.state()["frames"]
        t.check("13 a part keeps its part after 破棄",
                one + two in (FRAMES, FRAMES + 1) and again == two and len(c2) == 1,
                f"parts {one} + {two}; part 2 reopened with {again}; part 2 cuts {fmt(c2)}")
        left = app.cut(10, 20)
        app.ok()
        app.gone()
        files = app.export()
        got = sorted(pictures(f) or 0 for f in files)
        want = sorted([one, left, FRAMES])
        t.check("13 each part's output has its pictures", got == want,
                f"outputs {[os.path.basename(f) for f in files]} have {got} (want {want})")
        for f in files:
            os.remove(f)


# -- 14 --------------------------------------------------------------------
def audio_tracks(path):
    out = subprocess.run(
        ["ffprobe", "-v", "error", "-select_streams", "a", "-show_entries",
         "stream=index", "-of", "csv=p=0", path], capture_output=True, text=True).stdout
    # A transport stream's streams are listed again under its program.
    return len(set(out.split()))


def join_verify(cfg, t):
    """ベリファイ on, the list joined into one file (全部を 1 本に): the check
    reads the file back the way the join wrote it. c.ts has two sound tracks
    and a.ts one. With c.ts's first track switched off (its edit's
    dropStreams) the output's one track is c.ts's second, which a.ts has
    nothing for -- it stops where c.ts ends, as asked -- and the check, which
    once paired the tracks by the output's order, called that sound off
    ("音声 #1 が映像より 20 秒短い"). Tried with that master first and second,
    and on a join with nothing switched off (master c.ts second, its second
    track lacked by a.ts): each verifies OK and nothing about the sound."""
    # c.ts: stream 0 is the video, 1 and 2 the sound tracks.
    off = {"cuts": [], "keyframes": [], "dropStreams": [1]}
    cases = [
        ("master 2nd, its 1st track off", [{"path": cfg.a}, {"path": cfg.c, "edit": off}], 1, 1),
        ("master 1st, its 1st track off", [{"path": cfg.c, "edit": off}, {"path": cfg.a}], 0, 1),
        ("master 2nd, nothing off", [{"path": cfg.a}, {"path": cfg.c}], 1, 2),
    ]
    for n, (label, rows, master, tracks) in enumerate(cases):
        app = App(cfg, f"join_verify/{n}", rows, {"joinAll": True, "master": master})
        with app:
            app.to_list()
            # 環境設定 > ベリファイ, as the box sets it.
            app.s.js("const b = document.getElementById('pref-verify');"
                     "if (!b.checked) b.click();")
            # Both rows read, so that the run is the join of the two: the
            # button is pressable as soon as one of them is.
            app.s.click('button.tab[data-screen="out"]')
            wait_for("both rows on the output screen", lambda: app.s.js(
                "return document.querySelectorAll('#out-list > li').length") == 2, 60)
            time.sleep(0.5)
            files = app.export()
            notes = app.s.js(
                "return [...document.querySelectorAll('#out-list > li .note')]"
                ".map((n) => [n.innerText, n.title]);")
            ts = [f for f in files if f.endswith(".ts")]
            got = pictures(ts[0]) if len(ts) == 1 else None
            heard = audio_tracks(ts[0]) if len(ts) == 1 else None
            said = " | ".join(f"{a} [{b}]" for a, b in notes)
            t.check(f"14 {label}: the joined file",
                    len(ts) == 1 and got == 2 * FRAMES and heard == tracks,
                    f"outputs {[os.path.basename(f) for f in files]}, {got} pictures, "
                    f"{heard} sound track(s) (want {2 * FRAMES}, {tracks})")
            t.check(f"14 {label}: ベリファイ OK, nothing about the sound",
                    bool(notes) and all("ベリファイ OK" in a and "注意" not in a and "音声 #" not in b
                                        and "不一致" not in b for a, b in notes),
                    said)
            for f in files:
                os.remove(f)


# -- 16 --------------------------------------------------------------------
def mode_switch_during_run(cfg, t):
    """The other tab of 出力設定 pressed just after 出力開始, as a look at its
    settings: the run goes on writing what it was started as. A disc run
    with 音声のみ left on the file tab once wrote its streams as sound alone
    (or a join of the list, where 全部を 1 本に was on); a file run went on
    as `.m2ts`."""
    def press_other(app, mode):
        app.to_list()
        app.s.click('button.tab[data-screen="out"]')
        # Both rows read: the button is pressable as soon as one of them is,
        # and a run started then writes only that one.
        wait_for("both rows on the output screen", lambda: app.s.js(
            "return document.querySelectorAll('#out-list > li').length") == 2, 60)
        label = wait_for("the start button", lambda: app.s.js(
            "const b = document.getElementById('run-export'); return !b.disabled && b.innerText"), 60)
        time.sleep(0.5)
        app.s.js("document.getElementById('run-export').click();"
                 f"document.querySelector('.modes .tab[data-mode=\"{mode}\"]').click();")
        wait_for("the run to finish", lambda: app.s.js(
            "return document.getElementById('run-export').innerText") == label, 300, 0.5)

    # A disc, the file tab holding 音声のみ and 全部を 1 本に.
    with App(cfg, "mode_switch_during_run/disc", clips(cfg),
             {"mode": "bdav", "container": "sound", "joinAll": True, "discTitle": "t"}) as app:
        press_other(app, "file")
        found = []
        for root, _, names in os.walk(app.out):
            found += [os.path.join(root, n) for n in names]
        streams = sorted(f for f in found if f.endswith(".m2ts"))
        seen = [pictures(f) for f in streams]
        rel = sorted(os.path.relpath(f, app.out) for f in found)
        t.check("16 disc run: two streams with pictures",
                len(streams) == 2 and all(n and n >= FRAMES - 2 for n in seen),
                f"{seen}; files {rel[:12]}")
        t.check("16 disc run: nothing outside the disc",
                rel and all(f.startswith("BDAV" + os.sep) for f in rel), f"{rel[:12]}")
        shutil.rmtree(app.out)
        os.makedirs(app.out)

    # Files, with the disc tab pressed: still `.ts` and `.mkv`.
    with App(cfg, "mode_switch_during_run/file", clips(cfg)) as app:
        press_other(app, "bdav")
        names = sorted(os.listdir(app.out))
        t.check("16 file run: written as files", names == ["cut_01_a.ts", "cut_02_b.mkv"], f"{names}")
        for f in names:
            p = os.path.join(app.out, f)
            shutil.rmtree(p) if os.path.isdir(p) else os.remove(p)


# -- 15 --------------------------------------------------------------------
def cut_shapes_export(cfg, t):
    """The other three ways the editor cuts, written out: the head cut (IN on
    the first picture), Ctrl+Del (the inside of IN..OUT, both marked pictures
    kept), a cut to the end (OUT by End), and 範囲外を削除 (keep IN..OUT) on
    the .mkv's millisecond clock. Each output has the pictures the editor
    counted."""
    with App(cfg, "cut_shapes_export", clips(cfg)) as app:
        app.open_row(1, "a.ts")
        app.cut(0, 49)
        # Ctrl+Del: IN 100, OUT 200 -> 101..199 go.
        app.goto(100)
        app.s.keys("i")
        app.goto(200)
        app.s.keys("o")
        app.s.keys("Control+Delete")
        wait_for("the inner cut", lambda: app.state()["frames"] == 550 - 99, 10)
        # IN 400, End, OUT, Del: 400 to the last picture go.
        app.goto(400)
        app.s.keys("i")
        app.s.keys("End")
        time.sleep(0.5)
        app.s.keys("o")
        app.s.keys("Delete")
        wait_for("the tail cut", lambda: app.state()["frames"] == 400, 10)
        keep_a = app.state()["frames"]
        app.ok()
        app.gone()
        app.open_row(2, "b.mkv")
        app.goto(100)
        app.s.keys("i")
        app.goto(399)
        app.s.keys("o")
        app.s.click("#cut-outside")
        wait_for("the outside cut", lambda: app.state()["frames"] in (300, 301), 10)
        shown_b = app.state()["frames"]
        app.ok()
        app.gone()
        # Back once more and out with OK: the second visit's walk lands on a
        # saved edit whose cuts run to the ends of the recording.
        app.open_row(2, "b.mkv")
        again_b = app.state()["frames"]
        app.ok()
        app.gone()
        files = app.export()
        names = [os.path.basename(f) for f in files]
        outs = {n: f for n, f in zip(names, files)}
        got_a = pictures(outs["cut_01_a.ts"]) if "cut_01_a.ts" in outs else None
        got_b = pictures(outs["cut_02_b.mkv"]) if "cut_02_b.mkv" in outs else None
        t.check("15 .ts head + inner + tail cuts", got_a == keep_a,
                f"{got_a} pictures (want {keep_a}); outputs {names}")
        t.check("15 .mkv keep IN..OUT", got_b == 300,
                f"{got_b} pictures (want 300; editor counted {shown_b}, {again_b} on return)")
        for f in files:
            os.remove(f)


# -- 17 --------------------------------------------------------------------
def disc_fields_under_hand(cfg, t):
    """The disc's index fields on 出力設定 while something redraws the screen
    -- a row finishing its read, a recording's listing arriving: what is being
    typed stays as typed. The name emptied to type a new one came straight
    back as the recording's, and a space typed into the date was trimmed out
    from under the caret. The redraw is asked for here the way those arrivals
    ask for it (renderOutset), by the row picker's own change."""
    with App(cfg, "disc_fields_under_hand", clips(cfg), {"mode": "bdav", "discTitle": "t"}) as app:
        app.to_list()
        app.s.click('button.tab[data-screen="outset"]')
        wait_for("the programme field filled", lambda: app.s.js(
            "return document.getElementById('out-programme').value"), 60)
        redraw = "document.getElementById('outset-clip').dispatchEvent(new Event('change'));"
        value = lambda f: app.s.js(f"return document.getElementById('{f}').value")
        app.s.click("#out-programme")
        app.s.keys("Control+a", "Delete")
        app.s.js(redraw)
        time.sleep(0.3)
        t.check("17 emptied name stays empty while typed in", value("out-programme") == "",
                repr(value("out-programme")))
        app.s.type("New")
        app.s.js(redraw)
        t.check("17 typed name kept", value("out-programme") == "New", repr(value("out-programme")))
        app.s.click("#out-made")
        app.s.keys("Control+a", "Delete")
        app.s.type("2026/08/17 ")
        app.s.js(redraw)
        time.sleep(0.3)
        app.s.type("01:00")
        t.check("17 date typed with its space", value("out-made") == "2026/08/17 01:00",
                repr(value("out-made")))
        # And away from the field, the screen is the row's again.
        app.s.click("#out-channel")
        app.s.js(redraw)
        t.check("17 name kept after leaving", value("out-programme") == "New",
                repr(value("out-programme")))
        # The disc's name: emptied, it stays empty under the hand, and is
        # worked out again once the hand has left it.
        app.s.click("#out-disc-title")
        app.s.keys("Control+a", "Delete")
        app.s.js(redraw)
        time.sleep(0.3)
        t.check("17 emptied disc name stays empty while in it", value("out-disc-title") == "",
                repr(value("out-disc-title")))
        app.s.click("#out-programme")
        time.sleep(0.3)
        t.check("17 emptied disc name filled in on leaving", value("out-disc-title") != "",
                repr(value("out-disc-title")))


# -- 18 --------------------------------------------------------------------
def seam_window(cfg, t):
    """継ぎ目の設定 on a join of the two rows: a dissolve and a fade set in
    the seam window and left with Escape change nothing in the project; set
    again and left with OK, they are the row's `after`; opened a third time,
    the window shows what OK handed back. The window is cross.js, which had
    no scenario: the handshake is `cross-ready` / `cross-open` /
    `cross-done`, the same shape as the editor's."""
    def open_seam(app):
        app.to_list()
        app.s.click('button.tab[data-screen="outset"]')
        wait_for("the seam button", lambda: app.s.js(
            "const r = document.getElementById('row-cross');"
            "const b = document.getElementById('open-cross');"
            # The row shows while one row is still being read, with the
            # button greyed (no join yet): a click then does nothing.
            "return !!r && !r.hidden && !!b && !b.disabled;"), 60)
        before = set(app.s.handles())
        app.s.click("#open-cross")
        seam = wait_for("the seam window", lambda: (
            [h for h in app.s.handles() if h not in before] or [None])[0], 60)
        app.s.switch(seam)
        # Loaded: the pair is in and the span worked out, so the clip time
        # reads something other than nothing.
        wait_for("the join loaded", lambda: app.s.js(
            "return document.getElementById('out-clip-time').textContent") not in
            ("--:--:--.--", "00:00:00.00"), 60)
        return seam

    def set_crossing(app):
        app.s.js(
            "const k = document.getElementById('x-kind'); k.value = 'dissolve';"
            "k.dispatchEvent(new Event('change', {bubbles: true}));"
            "const f = document.getElementById('x-fade-out'); f.value = '1.5';"
            "f.dispatchEvent(new Event('change', {bubbles: true}));")
        time.sleep(0.5)

    def seam_gone(app, seam):
        wait_for("the seam window to close", lambda: seam not in app.s.handles(), 15)
        app.s.switch(app.main)

    with App(cfg, "seam_window", clips(cfg), {"joinAll": True}) as app:
        seam = open_seam(app)
        set_crossing(app)
        try:
            app.s.keys("Escape")
        except WebDriverError:
            # The window goes on the key's way down, and the driver then has
            # no window to let the key up in.
            pass
        seam_gone(app, seam)
        after = app.save()["clips"][0].get("after")
        t.check("18 Escape in the seam window changes nothing", not after, repr(after))

        seam = open_seam(app)
        shown = app.s.js("return document.getElementById('x-kind').value")
        t.check("18 reopened after Escape shows no crossing", shown == "none", repr(shown))
        set_crossing(app)
        app.s.click("#cross-ok")
        seam_gone(app, seam)
        after = app.save()["clips"][0].get("after") or {}
        t.check("18 OK writes the crossing to the row",
                after.get("kind") == "dissolve" and after.get("fadeOut") == 1.5,
                repr(after))

        seam = open_seam(app)
        shown = app.s.js("return [document.getElementById('x-kind').value,"
                         " document.getElementById('x-fade-out').value]")
        t.check("18 reopened after OK shows the crossing",
                shown == ["dissolve", "1.5"], repr(shown))
        # A list dropped from 効果 takes Escape for itself: the list goes and
        # the window stays. The arrows and Enter then choose from it.
        app.s.click("#x-kind")
        dropped = app.s.js("const m = document.querySelector('#x-kind ~ .drop-menu');"
                           "return !!m && !m.hidden;")
        app.s.keys("Escape")
        time.sleep(0.5)
        up = seam in app.s.handles()
        t.check("18 Escape over a dropped list keeps the window",
                dropped and up and app.s.js(
                    "return document.querySelector('#x-kind ~ .drop-menu').hidden"),
                f"dropped {dropped}, window {'up' if up else 'gone'}")
        if up:
            app.s.click("#x-kind")
            app.s.keys("End", "Enter")
            time.sleep(0.5)
            picked = app.s.js("return document.getElementById('x-kind').value")
            t.check("18 the dropped list chooses with the keys", picked == "slide-bottom",
                    repr(picked))
            try:
                app.s.click("#cross-cancel")
            except WebDriverError:
                # The window closes inside its own click, as with Escape.
                pass
            seam_gone(app, seam)
        after = app.save()["clips"][0].get("after") or {}
        t.check("18 キャンセル keeps what OK wrote", after.get("kind") == "dissolve",
                repr(after))


# -- 28 --------------------------------------------------------------------
def seam_follows_list(cfg, t):
    """継ぎ目の編集 open on the join a -> b of a three-row join (a, b, c); in
    the list the first row is moved down (b, a, c) while the window is up.
    The window was left on the old joins, and OK wrote the crossing by the
    row before the join -- onto a, which is now followed by c, a seam nobody
    looked at. The window must be told the new joins (it starts afresh on the
    first, b -> a), and OK then writes onto b."""
    rows = [{"path": cfg.a}, {"path": cfg.b}, {"path": cfg.c}]
    with App(cfg, "seam_follows_list", rows, {"joinAll": True}) as app:
        app.to_list()
        app.s.click('button.tab[data-screen="outset"]')
        wait_for("the seam button", lambda: app.s.js(
            "const r = document.getElementById('row-cross');"
            "const b = document.getElementById('open-cross');"
            "return !!r && !r.hidden && !!b && !b.disabled"
            " && document.querySelectorAll('#out-master option').length === 3;"), 90)
        before = set(app.s.handles())
        app.s.click("#open-cross")
        seam = wait_for("the seam window", lambda: (
            [h for h in app.s.handles() if h not in before] or [None])[0], 60)
        app.s.switch(seam)
        names = lambda: app.s.js(  # noqa: E731
            "return [document.getElementById('name-before').textContent,"
            " document.getElementById('name-after').textContent]")
        first = wait_for("the join's names", lambda: (lambda n: n[0] not in ("", "—") and n)(names()), 60)
        app.s.switch(app.main)
        app.s.click('button.tab[data-screen="input"]')
        app.s.click("#cliplist > li:nth-child(1) .nm")
        wait_for("下に移動", lambda: not app.s.js(
            "return document.getElementById('move-down').disabled"), 15)
        app.s.click("#move-down")
        app.s.switch(seam)
        moved = wait_for("the window on the new joins",
                         lambda: (lambda n: n != first and n)(names()), 15)
        app.s.js(
            "const k = document.getElementById('x-kind'); k.value = 'dissolve';"
            "k.dispatchEvent(new Event('change', {bubbles: true}));")
        time.sleep(0.5)
        try:
            app.s.click("#cross-ok")
        except WebDriverError:
            pass
        wait_for("the seam window to close", lambda: seam not in app.s.handles(), 15)
        app.s.switch(app.main)
        saved = app.save()["clips"]
        order = [os.path.basename(c["path"]) for c in saved]
        afters = [(c.get("after") or {}).get("kind") for c in saved]
        t.check("28 the seam window follows a row moved in the list",
                order == ["b.mkv", "a.ts", "c.ts"] and afters[0] == "dissolve" and not afters[1],
                f"window {first} -> {moved}; order {order}; after {afters}")


# -- 30 --------------------------------------------------------------------
def seam_keeps_unsaved(cfg, t):
    """継ぎ目の編集 open on the join a -> b, a dissolve set in it and not yet
    OK'd; in the list a join is added behind it (b duplicated: a, b, b'),
    which is what a row finishing its read does too while the output
    settings screen is up. The window is told the joins again (see
    `followCross`), and that must not throw away what was set in it for the
    join that is still there, nor move it off that join: OK then writes the
    dissolve onto a."""
    with App(cfg, "seam_keeps_unsaved", clips(cfg), {"joinAll": True}) as app:
        app.to_list()
        app.s.click('button.tab[data-screen="outset"]')
        wait_for("the seam button", lambda: app.s.js(
            "const r = document.getElementById('row-cross');"
            "const b = document.getElementById('open-cross');"
            "return !!r && !r.hidden && !!b && !b.disabled;"), 90)
        before = set(app.s.handles())
        app.s.click("#open-cross")
        seam = wait_for("the seam window", lambda: (
            [h for h in app.s.handles() if h not in before] or [None])[0], 60)
        app.s.switch(seam)
        names = lambda: app.s.js(  # noqa: E731
            "return [document.getElementById('name-before').textContent,"
            " document.getElementById('name-after').textContent]")
        first = wait_for("the join's names", lambda: (lambda n: n[0] not in ("", "—") and n)(names()), 60)
        app.s.js(
            "const k = document.getElementById('x-kind'); k.value = 'dissolve';"
            "k.dispatchEvent(new Event('change', {bubbles: true}));")
        time.sleep(0.5)
        app.s.switch(app.main)
        app.s.click('button.tab[data-screen="input"]')
        app.s.click("#cliplist > li:nth-child(2) .nm")
        wait_for("クリップを複製", lambda: not app.s.js(
            "return document.getElementById('duplicate-clip').disabled"), 15)
        app.s.click("#duplicate-clip")
        wait_for("three rows", lambda: app.s.js(
            "return document.querySelectorAll('#cliplist > li').length") == 3, 15)
        app.s.click('button.tab[data-screen="outset"]')
        time.sleep(1.5)
        app.s.switch(seam)
        now = names()
        kind = app.s.js("return document.getElementById('x-kind').value")
        t.check("30 the seam window keeps a join's unsaved crossing when a join is added",
                # b's name takes the twin's number; it is still a -> b.
                kind == "dissolve" and now[0] == first[0] and now[1].startswith(first[1]),
                f"kind {kind!r}; join {first} -> {now}")
        try:
            app.s.click("#cross-ok")
        except WebDriverError:
            pass
        wait_for("the seam window to close", lambda: seam not in app.s.handles(), 15)
        app.s.switch(app.main)
        saved = app.save()["clips"]
        afters = [(c.get("after") or {}).get("kind") for c in saved]
        t.check("30 OK writes it onto the join it was set on",
                len(saved) == 3 and afters[0] == "dissolve" and not afters[1],
                f"after {afters}")


# -- 31 --------------------------------------------------------------------
def seam_keeps_unsaved_undo(cfg, t):
    """継ぎ目の編集 open on the join a -> b of a three-row join (a, b, c), a
    dissolve set in it and not yet OK'd; in the list b is removed (the
    window is told a -> c) and 削除を元に戻す puts it back. The join a -> b
    is the same two rows again, and the dissolve set on it in the window
    must still be there: it went with the telling that left the join out,
    and OK then wrote nothing onto a."""
    rows = [{"path": cfg.a}, {"path": cfg.b}, {"path": cfg.c}]
    with App(cfg, "seam_keeps_unsaved_undo", rows, {"joinAll": True}) as app:
        app.to_list()
        app.s.click('button.tab[data-screen="outset"]')
        wait_for("the seam button", lambda: app.s.js(
            "const r = document.getElementById('row-cross');"
            "const b = document.getElementById('open-cross');"
            "return !!r && !r.hidden && !!b && !b.disabled"
            " && document.querySelectorAll('#out-master option').length === 3;"), 90)
        before = set(app.s.handles())
        app.s.click("#open-cross")
        seam = wait_for("the seam window", lambda: (
            [h for h in app.s.handles() if h not in before] or [None])[0], 60)
        app.s.switch(seam)
        names = lambda: app.s.js(  # noqa: E731
            "return [document.getElementById('name-before').textContent,"
            " document.getElementById('name-after').textContent]")
        first = wait_for("the join's names", lambda: (lambda n: n[0] not in ("", "—") and n)(names()), 60)
        app.s.js(
            "const k = document.getElementById('x-kind'); k.value = 'dissolve';"
            "k.dispatchEvent(new Event('change', {bubbles: true}));")
        time.sleep(0.5)
        app.s.switch(app.main)
        app.s.click('button.tab[data-screen="input"]')
        app.s.click("#cliplist > li:nth-child(2) .nm")
        wait_for("クリップ削除", lambda: not app.s.js(
            "return document.getElementById('remove-clip').disabled"), 15)
        app.s.click("#remove-clip")
        wait_for("two rows", lambda: app.s.js(
            "return document.querySelectorAll('#cliplist > li').length") == 2, 15)
        app.s.switch(seam)
        gone = wait_for("the window on a -> c", lambda: (lambda n: n != first and n)(names()), 15)
        app.s.switch(app.main)
        wait_for("削除を元に戻す", lambda: not app.s.js(
            "return document.getElementById('undo-remove').disabled"), 15)
        app.s.click("#undo-remove")
        wait_for("three rows", lambda: app.s.js(
            "return document.querySelectorAll('#cliplist > li').length") == 3, 15)
        app.s.click('button.tab[data-screen="outset"]')
        time.sleep(1.5)
        app.s.switch(seam)
        back = wait_for("the window back on a -> b", lambda: (lambda n: n == first and n)(names()), 15)
        kind = app.s.js("return document.getElementById('x-kind').value")
        t.check("31 the seam window keeps a join's unsaved crossing over a removal undone",
                kind == "dissolve", f"kind {kind!r}; join {first} -> {gone} -> {back}")
        try:
            app.s.click("#cross-ok")
        except WebDriverError:
            pass
        wait_for("the seam window to close", lambda: seam not in app.s.handles(), 15)
        app.s.switch(app.main)
        saved = app.save()["clips"]
        afters = [(c.get("after") or {}).get("kind") for c in saved]
        t.check("31 OK writes it onto a after the removal undone",
                len(saved) == 3 and afters[0] == "dissolve" and not afters[1],
                f"after {afters}")


def seam_clear_held_join(cfg, t):
    """seam_keeps_unsaved_undo with 全解除 pressed while b is out of the list:
    a dissolve set on a -> b and not yet OK'd, b removed (the window is told
    a -> c), 全解除 in the window, b put back. 全解除 cleared every join in
    the window, and the one held for a -> b came back with its dissolve; OK
    wrote it onto a."""
    rows = [{"path": cfg.a}, {"path": cfg.b}, {"path": cfg.c}]
    with App(cfg, "seam_clear_held_join", rows, {"joinAll": True}) as app:
        app.to_list()
        app.s.click('button.tab[data-screen="outset"]')
        wait_for("the seam button", lambda: app.s.js(
            "const r = document.getElementById('row-cross');"
            "const b = document.getElementById('open-cross');"
            "return !!r && !r.hidden && !!b && !b.disabled"
            " && document.querySelectorAll('#out-master option').length === 3;"), 90)
        before = set(app.s.handles())
        app.s.click("#open-cross")
        seam = wait_for("the seam window", lambda: (
            [h for h in app.s.handles() if h not in before] or [None])[0], 60)
        app.s.switch(seam)
        names = lambda: app.s.js(  # noqa: E731
            "return [document.getElementById('name-before').textContent,"
            " document.getElementById('name-after').textContent]")
        first = wait_for("the join's names", lambda: (lambda n: n[0] not in ("", "—") and n)(names()), 60)
        app.s.js(
            "const k = document.getElementById('x-kind'); k.value = 'dissolve';"
            "k.dispatchEvent(new Event('change', {bubbles: true}));")
        time.sleep(0.5)
        app.s.switch(app.main)
        app.s.click('button.tab[data-screen="input"]')
        app.s.click("#cliplist > li:nth-child(2) .nm")
        wait_for("クリップ削除", lambda: not app.s.js(
            "return document.getElementById('remove-clip').disabled"), 15)
        app.s.click("#remove-clip")
        wait_for("two rows", lambda: app.s.js(
            "return document.querySelectorAll('#cliplist > li').length") == 2, 15)
        app.s.switch(seam)
        wait_for("the window on a -> c", lambda: (lambda n: n != first and n)(names()), 15)
        app.s.js("document.getElementById('x-clear').click();")
        time.sleep(0.5)
        app.s.switch(app.main)
        wait_for("削除を元に戻す", lambda: not app.s.js(
            "return document.getElementById('undo-remove').disabled"), 15)
        app.s.click("#undo-remove")
        wait_for("three rows", lambda: app.s.js(
            "return document.querySelectorAll('#cliplist > li').length") == 3, 15)
        app.s.click('button.tab[data-screen="outset"]')
        time.sleep(1.5)
        app.s.switch(seam)
        wait_for("the window back on a -> b", lambda: (lambda n: n == first and n)(names()), 15)
        kind = app.s.js("return document.getElementById('x-kind').value")
        t.check("31b 全解除 also clears a join held while its row was out of the list",
                kind == "none", f"kind {kind!r}")
        try:
            app.s.click("#cross-ok")
        except WebDriverError:
            pass
        wait_for("the seam window to close", lambda: seam not in app.s.handles(), 15)
        app.s.switch(app.main)
        saved = app.save()["clips"]
        afters = [(c.get("after") or {}).get("kind") for c in saved]
        t.check("31b OK writes no crossing after 全解除 and the removal undone",
                len(saved) == 3 and not any(afters), f"after {afters}")

def seam_all_untouched_join(cfg, t):
    """seam_clear_held_join with nothing changed before b is removed: b taken
    out of the list while the window is open and untouched (the window is
    told a -> c), a dissolve set and 一括適用 pressed, b put back. Nothing had
    been changed when b went, so its two joins were not held, and they came
    back with the list's crossing (none) rather than the one applied to all;
    OK wrote none onto a and b."""
    rows = [{"path": cfg.a}, {"path": cfg.b}, {"path": cfg.c}]
    with App(cfg, "seam_all_untouched_join", rows, {"joinAll": True}) as app:
        app.to_list()
        app.s.click('button.tab[data-screen="outset"]')
        wait_for("the seam button", lambda: app.s.js(
            "const r = document.getElementById('row-cross');"
            "const b = document.getElementById('open-cross');"
            "return !!r && !r.hidden && !!b && !b.disabled"
            " && document.querySelectorAll('#out-master option').length === 3;"), 90)
        before = set(app.s.handles())
        app.s.click("#open-cross")
        seam = wait_for("the seam window", lambda: (
            [h for h in app.s.handles() if h not in before] or [None])[0], 60)
        app.s.switch(seam)
        names = lambda: app.s.js(  # noqa: E731
            "return [document.getElementById('name-before').textContent,"
            " document.getElementById('name-after').textContent]")
        first = wait_for("the join's names", lambda: (lambda n: n[0] not in ("", "—") and n)(names()), 60)
        app.s.switch(app.main)
        app.s.click('button.tab[data-screen="input"]')
        app.s.click("#cliplist > li:nth-child(2) .nm")
        wait_for("クリップ削除", lambda: not app.s.js(
            "return document.getElementById('remove-clip').disabled"), 15)
        app.s.click("#remove-clip")
        wait_for("two rows", lambda: app.s.js(
            "return document.querySelectorAll('#cliplist > li').length") == 2, 15)
        app.s.switch(seam)
        wait_for("the window on a -> c", lambda: (lambda n: n != first and n)(names()), 15)
        app.s.js(
            "const k = document.getElementById('x-kind'); k.value = 'dissolve';"
            "k.dispatchEvent(new Event('change', {bubbles: true}));"
            "document.getElementById('x-all').click();")
        time.sleep(0.5)
        app.s.switch(app.main)
        wait_for("削除を元に戻す", lambda: not app.s.js(
            "return document.getElementById('undo-remove').disabled"), 15)
        app.s.click("#undo-remove")
        wait_for("three rows", lambda: app.s.js(
            "return document.querySelectorAll('#cliplist > li').length") == 3, 15)
        app.s.click('button.tab[data-screen="outset"]')
        time.sleep(1.5)
        app.s.switch(seam)
        wait_for("the window back on a -> b", lambda: (lambda n: n == first and n)(names()), 15)
        kind = app.s.js("return document.getElementById('x-kind').value")
        t.check("31c 一括適用 reaches a join whose row was out of the list before anything was changed",
                kind == "dissolve", f"kind {kind!r}")
        try:
            app.s.click("#cross-ok")
        except WebDriverError:
            pass
        wait_for("the seam window to close", lambda: seam not in app.s.handles(), 15)
        app.s.switch(app.main)
        saved = app.save()["clips"]
        afters = [(c.get("after") or {}).get("kind") for c in saved]
        t.check("31c OK writes the crossing applied to all onto both joins put back",
                len(saved) == 3 and afters[:2] == ["dissolve", "dissolve"], f"after {afters}")

def seam_follows_editor(cfg, t):
    """継ぎ目の編集 open on a -> b while the list is on 入力設定, and row a
    cut in the cut editor so that its last kept range starts later. The
    window has to be told the join again as the cut lands (`followCross`),
    not only when the editor closes: it previewed and played the seam on the
    old bounds for the whole visit, and with the next row's open hanging the
    visit can go on for as long as somebody works in it. (38th pass)"""
    with App(cfg, "seam_follows_editor", clips(cfg), {"joinAll": True}) as app:
        app.to_list()
        app.s.click('button.tab[data-screen="outset"]')
        wait_for("the seam button", lambda: app.s.js(
            "const r = document.getElementById('row-cross');"
            "const b = document.getElementById('open-cross');"
            "return !!r && !r.hidden && !!b && !b.disabled;"), 60)
        before = set(app.s.handles())
        app.s.click("#open-cross")
        seam = wait_for("the seam window", lambda: (
            [h for h in app.s.handles() if h not in before] or [None])[0], 60)
        app.s.switch(seam)
        wait_for("the join loaded", lambda: app.s.js(
            "return document.getElementById('out-clip-time').textContent") not in
            ("--:--:--.--", "00:00:00.00"), 60)
        app.s.js("window.__seen = [];"
                 "window.__TAURI__.event.listen('cross-open', (e) => window.__seen.push("
                 "(e.payload.joins || []).map((j) => [j.beforeIn, j.beforeOut])));")
        time.sleep(0.5)
        app.s.switch(app.main)
        app.s.click('button.tab[data-screen="input"]')
        up = set(app.s.handles())
        app.s.double_click("#cliplist > li:nth-child(1) .nm")
        editor = wait_for("the editor window", lambda: (
            [h for h in app.s.handles() if h not in up] or [None])[0], 60)
        app.s.switch(editor)
        wait_for("the editor on a.ts", lambda: (lambda st: st and "a.ts" in st["title"]
                                                and st["frames"] is not None)(app.state()), 120, 0.2)
        n = app.state()["frames"]
        app.cut(n - 200, n - 101)
        # Past the editor's report debounce and the list's repaint.
        time.sleep(2)
        app.s.switch(seam)
        during = app.s.js("return window.__seen")
        moved = [j for j in during if j and j[0][0] > 0]
        t.check("38s the seam window follows a cut made in the editor while it is open",
                bool(moved), f"told {during}")
        app.s.switch(editor)
        app.ok()
        wait_for("the editor to close", lambda: editor not in app.s.handles(), 15)
        time.sleep(1.5)
        app.s.switch(seam)
        after = app.s.js("return window.__seen")
        t.check("38s ... and once the editor has closed", any(j and j[0][0] > 0 for j in after),
                f"told {after}")
        try:
            app.s.click("#cross-cancel")
        except WebDriverError:
            pass
        wait_for("the seam window to close", lambda: seam not in app.s.handles(), 15)


# -- 29 --------------------------------------------------------------------
def disc_refuses_opus(cfg, t):
    """A BDAV run of three rows whose second has Opus sound (an H.264 + Opus
    .mkv, as a web download is). A transport stream takes Opus, so the cut
    went ahead, the disc's index refused the stream after it was written, and
    the index failure took the second and third recordings off the disc with
    it. The run must not start, say which row and why, and write nothing."""
    home = os.path.join(cfg.work, "opus")
    os.makedirs(home, exist_ok=True)
    op = os.path.join(home, "op.mkv")
    if not os.path.exists(op):
        subprocess.run(["ffmpeg", "-nostdin", "-y", "-loglevel", "error",
                        "-f", "lavfi", "-i", "testsrc2=size=640x360:rate=30000/1001:duration=10",
                        "-f", "lavfi", "-i", "sine=frequency=440:duration=10:sample_rate=48000",
                        "-c:v", "libx264", "-preset", "veryfast", "-g", "30", "-pix_fmt", "yuv420p",
                        "-c:a", "libopus", "-b:a", "96k", "-f", "matroska", op + ".part"], check=True)
        os.replace(op + ".part", op)
    rows = [{"path": cfg.a}, {"path": op}, {"path": cfg.b}]
    with App(cfg, "disc_refuses_opus", rows, {"mode": "bdav", "discTitle": "t"}) as app:
        app.to_list()
        app.s.click('button.tab[data-screen="out"]')
        wait_for("three rows on the output screen", lambda: app.s.js(
            "return document.querySelectorAll('#out-list > li').length") == 3, 60)
        label = wait_for("the start button", lambda: app.s.js(
            "const b = document.getElementById('run-export'); return !b.disabled && b.innerText"), 60)
        time.sleep(0.5)
        app.s.click("#run-export")
        said = wait_for("the run's sentence", lambda: app.s.js(
            "return document.getElementById('out-note').textContent"), 30, 0.2)
        time.sleep(1.0)
        written = [os.path.join(r, f) for r, _, fs in os.walk(app.out) for f in fs]
        button = app.s.js("return document.getElementById('run-export').innerText")
        t.check("29 nothing cut onto the disc", not written,
                f"{[os.path.relpath(f, app.out) for f in written][:6]}")
        t.check("29 the run names the row and the codec, and is over",
                "op" in said and "opus" in said and button == label,
                f"note {said!r}, button {button!r}")


# -- 29b (rev29 p2gui) ------------------------------------------------------
def _disc_name(at):
    """The name `info.bdav` in `at`/BDAV gives the disc, as raw bytes."""
    try:
        with open(os.path.join(at, "BDAV", "info.bdav"), "rb") as f:
            raw = f.read()
    except OSError:
        return None
    n = raw[64] if len(raw) > 64 else 0
    return raw[65:65 + n]


def disc_keeps_its_name(cfg, t):
    """A disc written with a title typed on the settings screen, then a
    recording added to it with the field left empty: the disc keeps its name
    (the series of what is added names only a new disc), and the field says
    so before the run. A title typed for the addition is still the one
    written. The command line's --bdav does the same."""
    at = os.path.join(cfg.work, "disc_keeps_its_name")
    shutil.rmtree(at, ignore_errors=True)
    os.makedirs(at)
    runs = (("typed", [{"path": cfg.a}], "KEEPNAME"),
            ("left empty", [{"path": cfg.c}], ""),
            ("typed again", [{"path": cfg.b}], "OTHERNAME"))
    for what, rows, title in runs:
        with App(cfg, "disc_keeps_its_name", rows,
                 {"mode": "bdav", "dir": at, "subfolder": "", "discTitle": title}) as app:
            app.to_list()
            wait_for("the row read", lambda: _walked(app, 1), 120, 0.5)
            app.s.click('button.tab[data-screen="outset"]')
            shown = wait_for("the disc's name on the settings screen", lambda: app.s.js(
                "return document.getElementById('out-disc-title').value"), 30, 0.2)
            time.sleep(1.0)
            shown = app.s.js("return document.getElementById('out-disc-title').value")
            app.export()
            name = _disc_name(at) or b""
            want = (title or "KEEPNAME").encode()
            t.check(f"29b the disc's name, {what}", want in name,
                    f"info.bdav {name!r} want {want!r}")
            if not title:
                t.check("29b the settings screen shows the name the disc keeps", shown == "KEEPNAME",
                        f"field {shown!r}")
    shutil.rmtree(at, ignore_errors=True)


def _bdav_bytes(at):
    """What `at`/BDAV holds, every file counted (the after-run check's count)."""
    total = 0
    for root, _, names in os.walk(os.path.join(at, "BDAV")):
        total += sum(os.lstat(os.path.join(root, n)).st_size for n in names)
    return total


def _gauge(app):
    """The disc gauge on 出力設定 as drawn: its sentence, its segments (class,
    share, tooltip) and the sizes beside its bars."""
    return app.s.js(
        "const b = document.getElementById('disc-gauge');"
        "const n = b.querySelector('.gauge-note');"
        "const bar = b.querySelector('.gauge-bar');"
        "return {note: n ? n.textContent : b.textContent, tone: n ? n.className : '',"
        " segs: bar ? [...bar.children].filter((e) => e.classList.contains('gauge-seg'))"
        "   .map((e) => [e.className, e.style.flexGrow, e.title]) : [],"
        " sizes: [...b.querySelectorAll('.gauge-size')].map((e) => e.textContent)};")


def disc_gauge_appends(cfg, t):
    """The disc gauge and 収める on a folder that already holds a disc. A run
    there adds to the disc and the after-run check measures the whole disc,
    so the gauge has to price the list against the room left: the list alone
    against the whole disc said "fits", 収める took nothing off, and the disc
    came out larger than the disc. Fresh folder: drawn exactly as before.
    The folder changed on the screen while the gauge is up: the gauge follows
    it, both ways. The disc's capacity is set small (30 MB) so that a.ts
    (about 12 MB) fills it in three runs."""
    at = os.path.join(cfg.work, "disc_gauge_appends")
    empty = os.path.join(cfg.work, "disc_gauge_appends_fresh")
    for d in (at, empty):
        shutil.rmtree(d, ignore_errors=True)
        os.makedirs(d)
    disc = 30_000_000
    pct = lambda g: float((re.search(r"[（(]([\d.]+)%[）)]", g["note"]) or [0, "nan"])[1])

    def gauge_now(app, why, cond=lambda g: True):
        app.to_list()
        app.s.click('button.tab[data-screen="outset"]')
        g = wait_for(why, lambda: (lambda g: g if "%" in g["note"] and cond(g) else None)(_gauge(app)),
                     60, 0.25)
        time.sleep(0.5)
        return _gauge(app)

    def set_dir(app, d):
        app.s.js("const i = document.getElementById('out-dir'); i.value = arguments[0];"
                 " i.dispatchEvent(new Event('input', {bubbles: true}));", d)

    used = lambda app, d: app.s.js("return window.__TAURI__.core.invoke('bdav_used', {dir: arguments[0]})", d)

    with App(cfg, "disc_gauge_appends", [{"path": cfg.a}],
             {"mode": "bdav", "dir": at, "subfolder": "", "discTitle": "G"}) as app:
        app.to_list()
        wait_for("the row read", lambda: _walked(app, 1), 120, 0.5)
        # A disc of 30 MB, as one more choice of the disc control: a size
        # the list does not offer is put back to one it does when the screen
        # is drawn from a project.
        app.s.click('button.tab[data-screen="outset"]')
        app.s.js("const s = document.getElementById('out-disc'); const o = document.createElement('option');"
                 " o.value = String(arguments[0]); o.textContent = 'test'; s.appendChild(o); s.value = o.value;"
                 " s.dispatchEvent(new Event('input', {bubbles: true}));", disc)
        fresh = gauge_now(app, "the gauge on a fresh folder")
        t.check("40 fresh folder: no disc segment, no note about one",
                all("fixed" not in c for c, _, _ in fresh["segs"]) and "※" not in fresh["note"]
                and used(app, at) == 0, f"{fresh}")
        app.export()
        had = _bdav_bytes(at)
        t.check("40 bdav_used is what the disc holds", had > 0 and used(app, at) == had,
                f"bdav_used {used(app, at)} disc {had}")
        once = gauge_now(app, "the gauge with the disc in the folder",
                         lambda g: g["segs"] and "fixed" in g["segs"][0][0])
        want = pct(fresh) + had / disc * 100
        t.check("40 existing disc: drawn first, counted in",
                once["segs"][0][0] == "gauge-seg fixed" and "出力先のフォルダーにあるディスク" in once["note"]
                and abs(pct(once) - want) < 0.15,
                f"{pct(once)}% want {want:.2f}%; {once}")
        # The folder changed under the gauge: to one with no disc in it and
        # back. The empty one is drawn exactly as the fresh folder was.
        set_dir(app, empty)
        away = wait_for("the gauge to follow the folder", lambda: (lambda g: g if not any(
            "fixed" in c for c, _, _ in g["segs"]) else None)(_gauge(app)), 30, 0.2)
        time.sleep(0.5)
        away = _gauge(app)
        t.check("40 folder changed to an empty one: as on a fresh folder", away == fresh,
                f"{away} vs {fresh}")
        set_dir(app, at)
        back = wait_for("the gauge to follow the folder back", lambda: (lambda g: g if g["segs"] and
            "fixed" in g["segs"][0][0] else None)(_gauge(app)), 30, 0.2)
        time.sleep(0.5)
        back = _gauge(app)
        t.check("40 folder changed back: the disc is counted again", back == once, f"{back} vs {once}")
        # A second run: the third would not fit as it is.
        app.export()
        twice = gauge_now(app, "the gauge with two recordings on the disc",
                          lambda g: abs(pct(g) - (pct(fresh) + _bdav_bytes(at) / disc * 100)) < 0.15)
        t.check("40 the third would not fit: said so", "over" in twice["tone"] and "超過" in twice["note"],
                f"{twice}")
        app.s.click("#out-fit")
        fitted = wait_for("収める's answer", lambda: (lambda g: g if "トランスコード" in g["note"]
                                                     else None)(_gauge(app)), 30, 0.2)
        t.check("40 収める on: the pictures will be made smaller", "収めます" in fitted["note"]
                and len(fitted["sizes"]) == 2, f"{fitted}")
        before = _bdav_bytes(at)
        app.export()
        after = _bdav_bytes(at)
        streams = sorted(os.listdir(os.path.join(at, "BDAV", "STREAM")))
        t.check("40 収める onto a disc already two-thirds full: the disc fits",
                len(streams) == 3 and before < after <= disc,
                f"disc {after} bytes of {disc} (was {before}); streams {streams}")
        # What the count leaves in and out. A stream some other program is
        # writing into the disc is on it; the streams this window's own run has
        # been handed are the list on the gauge itself and are not.
        now = _bdav_bytes(at)
        other = os.path.join(at, "BDAV", "STREAM", "99998.m2ts")
        with open(other, "wb") as f:
            f.write(b"\0" * 1_000_000)
        t.check("40 another program's stream is counted", used(app, at) == now + 1_000_000,
                f"{used(app, at)} vs {now} + 1000000")
        os.remove(other)
        slot = app.s.js("return window.__TAURI__.core.invoke('bdav_prepare', {dir: arguments[0], n: 1})", at)[0]
        with open(slot["path"], "wb") as f:
            f.write(b"\0" * 1_000_000)
        mine = used(app, at)
        app.s.js("return window.__TAURI__.core.invoke('bdav_discard', {dir: arguments[0], clips: [arguments[1]]})",
                 at, slot["clip"])
        t.check("40 this window's own stream is not counted twice", mine == now, f"{mine} vs {now}")
        # A run whose index fails part way: the recording before the failure
        # is indexed and stays on the disc, the failed one is taken off. Both
        # are given back, so what is left on the disc is counted again.
        slots = app.s.js("return window.__TAURI__.core.invoke('bdav_prepare', {dir: arguments[0], n: 2})", at)
        shutil.copyfile(os.path.join(at, "BDAV", "STREAM", streams[0]), slots[0]["path"])
        with open(slots[1]["path"], "wb") as f:
            f.write(b"\0" * 1_000_000)
        entry = lambda s: {"clip": s["clip"], "name": "x", "made": None, "ran": None, "description": None,
                           "channel": None, "channel_number": None, "marks": []}
        failed = app.s.js(
            "return window.__TAURI__.core.invoke('bdav_finish', {dir: arguments[0], title: 'G',"
            " entries: arguments[1]}).then(() => 'wrote', (e) => String(e));",
            at, [entry(s) for s in slots])
        left = _bdav_bytes(at)
        t.check("40 an index that failed part way: what it left is counted",
                failed != "wrote" and os.path.exists(slots[0]["path"])
                and not os.path.exists(slots[1]["path"]) and used(app, at) == left,
                f"finish {failed!r}; bdav_used {used(app, at)} disc {left}")
        t.check("40 no folder, no name: nothing", used(app, os.path.join(empty, "none")) == 0
                and used(app, "") == 0, "")
    for d in (at, empty):
        shutil.rmtree(d, ignore_errors=True)


def disc_name_after_a_run(cfg, t):
    """One window, one list, into a folder that keeps its disc's name: a run
    with a title typed, then the field emptied and the same list run again.
    The name the screen guessed before the first run (the folder had no disc
    then) must not come back: the field shows, and the second run writes, the
    name the disc was just given."""
    at = os.path.join(cfg.work, "disc_name_after_a_run")
    shutil.rmtree(at, ignore_errors=True)
    os.makedirs(at)
    with App(cfg, "disc_name_after_a_run", [{"path": cfg.a}],
             {"mode": "bdav", "dir": at, "subfolder": "", "discTitle": "KEEPNAME"}) as app:
        app.to_list()
        wait_for("the row read", lambda: _walked(app, 1), 120, 0.5)
        app.s.click('button.tab[data-screen="outset"]')
        time.sleep(1.0)
        app.export()
        t.check("29c the first run writes the typed name", b"KEEPNAME" in (_disc_name(at) or b""),
                f"info.bdav {_disc_name(at)!r}")
        app.to_list()
        app.s.click('button.tab[data-screen="outset"]')
        app.s.js("const b = document.getElementById('out-disc-title'); b.value = '';"
                 " b.dispatchEvent(new Event('input', {bubbles: true})); b.blur();")
        time.sleep(1.5)
        shown = app.s.js("return document.getElementById('out-disc-title').value")
        t.check("29c the emptied field shows the disc's name", shown == "KEEPNAME", f"field {shown!r}")
        app.export()
        name = _disc_name(at) or b""
        t.check("29c the second run keeps the disc's name", b"KEEPNAME" in name, f"info.bdav {name!r}")
    shutil.rmtree(at, ignore_errors=True)


def disc_name_after_image_only(cfg, t):
    """As disc_name_after_a_run, but the run makes an image and takes the
    folder away (イメージのみ): the folder then holds no disc, so the emptied
    field must not keep the name just typed -- the next run makes a new disc,
    which its series names."""
    at = os.path.join(cfg.work, "disc_name_after_image_only")
    shutil.rmtree(at, ignore_errors=True)
    os.makedirs(at)
    night = os.path.join(at, "night")
    with App(cfg, "disc_name_after_image_only", [{"path": cfg.a}],
             {"mode": "bdav", "dir": at, "subfolder": "night", "discTitle": "KEEPNAME",
              "image": "2.50", "imageOnly": True}) as app:
        app.to_list()
        wait_for("the row read", lambda: _walked(app, 1), 120, 0.5)
        app.s.click('button.tab[data-screen="outset"]')
        time.sleep(1.0)
        app.export()
        t.check("29d the image is written and the folder taken away",
                os.path.exists(night + ".iso") and _disc_name(night) is None,
                f"iso {os.path.exists(night + '.iso')} info.bdav {_disc_name(night)!r}")
        app.to_list()
        app.s.click('button.tab[data-screen="outset"]')
        app.s.js("const i = document.getElementById('out-image'); i.value = '';"
                 " i.dispatchEvent(new Event('input', {bubbles: true}));"
                 " const b = document.getElementById('out-disc-title'); b.value = '';"
                 " b.dispatchEvent(new Event('input', {bubbles: true})); b.blur();")
        time.sleep(1.5)
        shown = app.s.js("return document.getElementById('out-disc-title').value")
        t.check("29d the emptied field does not keep the dropped disc's name", shown != "KEEPNAME",
                f"field {shown!r}")
        app.export()
        name = _disc_name(night) or b""
        t.check("29d the new disc is not given the dropped one's name", name and b"KEEPNAME" not in name,
                f"info.bdav {name!r}")
    shutil.rmtree(at, ignore_errors=True)


def disc_keeps_an_empty_name(cfg, t):
    """A disc whose name is EMPTY (a recorder's disc nobody named; here made
    by the command line with --disc-title "") in a folder that keeps its
    disc's name: with no name typed, the field shows the empty name and a run
    keeps it -- twice, the second from what the first left. `bdav_title`
    answers null for a folder with no disc and "" for a disc with no name:
    the output folder switched to an empty folder shows a new disc's name,
    to a named disc that name, and back the empty one again."""
    at = os.path.join(cfg.work, "disc_keeps_an_empty_name")
    fresh = os.path.join(cfg.work, "disc_keeps_an_empty_name_fresh")
    named = os.path.join(cfg.work, "disc_keeps_an_empty_name_named")
    for d in (at, fresh, named):
        shutil.rmtree(d, ignore_errors=True)
        os.makedirs(d)
    for d, title in ((at, ""), (named, "NAMEDDISC")):
        subprocess.run([_cli(), cfg.a, "--keep", "0-2", "--bdav", d, "--disc-title", title],
                       stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, timeout=300, check=True)
    t.check("34e-0 the command line made an empty-named disc", _disc_name(at) == b"",
            repr(_disc_name(at)))

    def playlists():
        return len(os.listdir(os.path.join(at, "BDAV", "PLAYLIST")))

    with App(cfg, "disc_keeps_an_empty_name", [{"path": cfg.c}],
             {"mode": "bdav", "dir": at, "subfolder": "", "discTitle": ""}) as app:
        app.to_list()
        wait_for("the row read", lambda: _walked(app, 1), 120, 0.5)
        app.s.click('button.tab[data-screen="outset"]')

        def field():
            return app.s.js("return document.getElementById('out-disc-title').value")

        def settled(want):
            until = time.time() + 20
            got = field()
            while time.time() < until and not want(got):
                time.sleep(0.2)
                got = field()
            return got

        def to_dir(d):
            app.s.js("const i = document.getElementById('out-dir'); i.value = arguments[0];"
                     " i.dispatchEvent(new Event('input', {bubbles: true}));", d)

        shown = settled(lambda v: v == "")
        t.check("34e-1 the field shows the disc's empty name", shown == "", f"field {shown!r}")
        to_dir(fresh)
        shown = settled(lambda v: v != "")
        t.check("34e-2 a folder with no disc: the field names a new disc", shown != "", f"field {shown!r}")
        to_dir(named)
        shown = settled(lambda v: v == "NAMEDDISC")
        t.check("34e-3 a named disc: the field shows its name", shown == "NAMEDDISC", f"field {shown!r}")
        to_dir(at)
        shown = settled(lambda v: v == "")
        t.check("34e-4 back to the unnamed disc: the field is empty again", shown == "", f"field {shown!r}")
        app.export()
        name = _disc_name(at)
        t.check("34e-5 the run keeps the disc's empty name", name == b"" and playlists() == 2,
                f"info.bdav {name!r}, {playlists()} playlists")
        app.to_list()
        app.s.click('button.tab[data-screen="outset"]')
        shown = settled(lambda v: v == "")
        t.check("34e-6 after the run the field is still empty", shown == "", f"field {shown!r}")
        app.export()
        name = _disc_name(at)
        t.check("34e-7 a second run keeps it too", name == b"" and playlists() == 3,
                f"info.bdav {name!r}, {playlists()} playlists")
    for d in (at, fresh, named):
        shutil.rmtree(d, ignore_errors=True)



def disc_name_renamed_during_run(cfg, t):
    """A disc that keeps its name (a folder not named after the disc, no
    title typed), added to by a run during which a row's programme is
    renamed on 出力設定. The disc's name was guessed at the start, keyed by
    the programmes' names; at the end the key no longer matched and the disc
    was indexed under the moment the run ended. It keeps its name."""
    at = os.path.join(cfg.work, "disc_name_renamed_during_run")
    shutil.rmtree(at, ignore_errors=True)
    os.makedirs(at)
    subprocess.run([_cli(), cfg.a, "--keep", "0-2", "--bdav", at, "--disc-title", "KEEPNAME"],
                   stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, timeout=300, check=True)
    info = os.path.join(at, "BDAV", "info.bdav")
    before = os.stat(info).st_mtime_ns
    rows = [{"path": cfg.a}, {"path": cfg.c}, {"path": cfg.a}, {"path": cfg.c}]
    with App(cfg, "disc_name_renamed_during_run", rows,
             {"mode": "bdav", "dir": at, "subfolder": "", "discTitle": ""}) as app:
        app.to_list()
        for n in (1, 2, 3, 4):
            wait_for(f"row {n} read", lambda n=n: _walked(app, n), 120, 0.5)
        app.s.click('button.tab[data-screen="outset"]')
        shown = wait_for("the disc's name", lambda: app.s.js(
            "return document.getElementById('out-disc-title').value") == "KEEPNAME", 30, 0.2)
        app.s.click('button.tab[data-screen="out"]')
        label = wait_for("the start button", lambda: app.s.js(
            "const b = document.getElementById('run-export'); return !b.disabled && b.innerText"), 30)
        app.s.click("#run-export")
        stream = os.path.join(at, "BDAV", "STREAM", "00002.m2ts")
        wait_for("the first recording being cut", lambda: os.path.exists(stream)
                 and os.path.getsize(stream) > 0, 60, 0.02)
        app.s.js("const i = document.getElementById('out-programme');"
                 " i.value = 'RENAMED DURING THE RUN';"
                 " i.dispatchEvent(new Event('input', {bubbles: true}));")
        reached = os.stat(info).st_mtime_ns == before
        wait_for("the run to finish", lambda: app.s.js(
            "return document.getElementById('run-export').innerText") == label, 300, 0.5)
        name = _disc_name(at)
        if not reached:
            t.check("35a NOT REACHED: the index was written before the rename", True, "")
        else:
            t.check("35a a programme renamed during the run: the disc keeps its name",
                    name is not None and name.endswith(b"KEEPNAME"), f"info.bdav {name!r}")
            # And the next run of the same session, with the renamed list:
            # the guess put down at the end of the first run (keyed by the
            # list as it ended) is what this one starts from.
            first = os.stat(info).st_mtime_ns
            app.s.click('button.tab[data-screen="outset"]')
            shown = app.s.js("return document.getElementById('out-disc-title').value")
            app.s.click('button.tab[data-screen="out"]')
            wait_for("the start button again", lambda: app.s.js(
                "const b = document.getElementById('run-export'); return !b.disabled && b.innerText") == label,
                30)
            app.s.click("#run-export")
            wait_for("the second index", lambda: os.stat(info).st_mtime_ns != first, 300, 0.2)
            wait_for("the second run to finish", lambda: app.s.js(
                "return document.getElementById('run-export').innerText") == label, 300, 0.5)
            name = _disc_name(at)
            t.check("35a2 the next run of the renamed list: the disc still keeps its name",
                    shown == "KEEPNAME" and name is not None and name.endswith(b"KEEPNAME"),
                    f"field {shown!r}, info.bdav {name!r}")
    shutil.rmtree(at, ignore_errors=True)


def disc_gauge_overwritable(cfg, t):
    """An overwritable image is laid out as a recorder formats a BD-RE, which
    holds 24,118,755,328 bytes of files on a single layer -- not the
    25,025,314,816 of the "25GB" disc. The gauge prices the list against that
    while イメージ 書き換え可 is chosen, and against the disc's own size with a
    read-only image or none, exactly as before; the check after an
    overwritable run says the same size."""
    at = os.path.join(cfg.work, "disc_gauge_overwritable")
    shutil.rmtree(at, ignore_errors=True)
    os.makedirs(at)
    pct = lambda g: float((re.search(r"[（(]([\d.]+)%[）)]", g["note"]) or [0, "nan"])[1])

    def pick(app, id, value):
        app.s.js("const s = document.getElementById(arguments[0]); s.value = arguments[1];"
                 " s.dispatchEvent(new Event('input', {bubbles: true}));", id, value)

    def gauge_with(app, why, part):
        wait_for(why, lambda: part in _gauge(app)["note"], 30, 0.2)
        time.sleep(0.5)
        return _gauge(app)

    holds = lambda app, access: app.s.js(
        "return window.__TAURI__.core.invoke('disc_holds', {disc: 25025314816, access: arguments[0]})", access)
    with App(cfg, "disc_gauge_overwritable", [{"path": cfg.a}],
             {"mode": "bdav", "dir": at, "subfolder": "night", "discTitle": "OW",
              "image": "2.50", "imageAccess": "read-only"}) as app:
        app.to_list()
        wait_for("the row read", lambda: _walked(app, 1), 120, 0.5)
        app.s.click('button.tab[data-screen="outset"]')
        t.check("32b disc_holds: the disc's own size unless overwritable",
                holds(app, "") == 25025314816 and holds(app, "read-only") == 25025314816
                and holds(app, "overwritable") == 24118755328,
                f"{holds(app, '')} {holds(app, 'read-only')} {holds(app, 'overwritable')}")
        ro = gauge_with(app, "the gauge, read-only", "/ 23.3 GiB")
        pick(app, "out-image-access", "overwritable")
        ow = gauge_with(app, "the gauge, overwritable", "/ 22.5 GiB")
        t.check("32b overwritable: priced against what the BD-RE holds",
                abs(pct(ow) / pct(ro) - 25025314816 / 24118755328) < 0.05 and ow["sizes"] == ro["sizes"],
                f"{ro['note']!r} -> {ow['note']!r}")
        pick(app, "out-image", "")
        none = gauge_with(app, "the gauge, no image", "/ 23.3 GiB")
        t.check("32b no image: drawn as with a read-only one", none == ro, f"{none} vs {ro}")
        pick(app, "out-image", "2.50")
        gauge_with(app, "the gauge, overwritable again", "/ 22.5 GiB")
        app.export()
        said = ""
        for root, _, files in os.walk(app.dir):
            for f in files:
                if f.endswith(".log") or f.endswith(".txt"):
                    with open(os.path.join(root, f), "rb") as fh:
                        said += fh.read().decode("utf-8", "replace")
        t.check("32b the check after the run says the same disc",
                "22.5 GiB のディスクに収まります" in said and os.path.exists(os.path.join(at, "night.iso")),
                f"{[l for l in said.splitlines() if 'ディスクに収' in l][:2]}")
    shutil.rmtree(at, ignore_errors=True)


def _busy(cfg):
    """A recording whose last ten seconds hold more than twice their share of
    its bytes: MPEG-2 at a fixed quantiser, film grain from 20 s on. Made once
    and kept beside the work directory's other recordings (not in `media`,
    which `clear_sidecars` empties of everything but its own four)."""
    at = os.path.join(cfg.work, "fitmedia", "busy.ts")
    if not os.path.exists(at):
        os.makedirs(os.path.dirname(at), exist_ok=True)
        subprocess.run([
            "ffmpeg", "-nostdin", "-y", "-loglevel", "error", "-f", "lavfi", "-i",
            "testsrc2=size=720x480:rate=30000/1001:duration=30,"
            "noise=alls=30:allf=t+u:enable=gte(t\\,20)",
            "-f", "lavfi", "-i", "sine=frequency=440:duration=30:sample_rate=48000",
            "-c:v", "mpeg2video", "-g", "15", "-bf", "2", "-q:v", "6",
            "-c:a", "mp2", "-b:a", "192k", "-f", "mpegts", at + ".part"], check=True)
        os.replace(at + ".part", at)
    return at


def disc_fit_busy_stretch(cfg, t):
    """収める prices a row by what the stretch it keeps holds, not by the
    recording's average rate times the seconds kept. The busy last ten
    seconds of `_busy` hold over twice the average: priced at the average,
    the gauge said a 20 MB disc took them as they were, 収める took nothing
    off, and the disc came out about a third over. Now: the gauge's price is
    the plain cut's size (a little over, never under), 収める's share is the
    command line's --fit share for the same keep, and the disc it writes fits."""
    busy = _busy(cfg)
    plain = os.path.join(cfg.work, "disc_fit_busy_plain")
    at = os.path.join(cfg.work, "disc_fit_busy")
    for d in (plain, at):
        shutil.rmtree(d, ignore_errors=True)
        os.makedirs(d)
    disc = 20_000_000
    pct = lambda g: float((re.search(r"[（(]([\d.]+)%[）)]", g["note"]) or [0, "nan"])[1])
    share = lambda g: float((re.search(r"映像を ([\d.]+)% のサイズ", g["note"]) or [0, "nan"])[1])
    row = {"path": busy, "edit": {"cuts": [{"a": 0.0, "b": 20.0}], "keyframes": []}}
    with App(cfg, "disc_fit_busy_stretch", [row],
             {"mode": "bdav", "dir": plain, "subfolder": "", "discTitle": "F"}) as app:
        app.to_list()
        wait_for("the row read", lambda: _walked(app, 1), 120, 0.5)
        # What the kept stretch really comes to on a disc, cut as it is.
        app.export()
        real = _bdav_bytes(plain)
        app.s.click('button.tab[data-screen="outset"]')
        app.s.js("const i = document.getElementById('out-dir'); i.value = arguments[0];"
                 " i.dispatchEvent(new Event('input', {bubbles: true}));", at)
        app.s.js("const s = document.getElementById('out-disc'); const o = document.createElement('option');"
                 " o.value = String(arguments[0]); o.textContent = 'test'; s.appendChild(o); s.value = o.value;"
                 " s.dispatchEvent(new Event('input', {bubbles: true}));", disc)
        over = wait_for("the gauge on the small disc", lambda: (lambda g: g if "%" in g["note"] and not any(
            "fixed" in c for c, _, _ in g["segs"]) and "/ 0.0 GiB" in g["note"] else None)(_gauge(app)), 60, 0.25)
        time.sleep(0.5)
        over = _gauge(app)
        priced = pct(over) / 100 * disc
        t.check("36f the gauge prices the busy stretch at what it holds",
                0.98 * real <= priced <= 1.10 * real and "超過" in over["note"],
                f"priced {priced / 1e6:.2f} MB, cut as it is {real / 1e6:.2f} MB; {over['note']!r}")
        app.s.click("#out-fit")
        fitted = wait_for("収める's answer", lambda: (lambda g: g if "トランスコード" in g["note"]
                                                     else None)(_gauge(app)), 30, 0.2)
        time.sleep(0.5)
        fitted = _gauge(app)
        # The command line's answer for the same keep, where it is built.
        repo = os.path.abspath(os.path.join(os.path.dirname(cfg.gui), "..", "..", "..", ".."))
        cli = os.path.join(repo, "rust", "target", "release", "smartcut")
        if os.access(cli, os.X_OK):
            said = subprocess.run([cli, busy, "--keep", "20-30", "--fit", str(disc), "--bdav",
                                   os.path.join(at, "cli-not-written"), "--analyze"],
                                  capture_output=True, text=True, timeout=300)
            m = re.search(r"written at ([\d.]+)% of their own size", said.stdout + said.stderr)
            asked = float(m[1]) if m else float("nan")
            t.check("36f 収める's share is the command line's --fit share",
                    "収めます" in fitted["note"] and abs(share(fitted) - asked) <= 0.3,
                    f"GUI {share(fitted)}%, CLI {asked}%")
        else:
            print(f"        (not compared with the command line: {cli} is not built)", flush=True)
        app.export()
        wrote = _bdav_bytes(at)
        t.check("36f 収める onto the small disc: the disc fits", 0 < wrote <= disc,
                f"disc {wrote} bytes of {disc}; cut as it is {real}")
    for d in (plain, at):
        shutil.rmtree(d, ignore_errors=True)


def image_turned_on_during_run(cfg, t):
    """An image asked for while the run is cutting (the image step is made
    for it, see app.js), with 書き換え可 chosen as it is turned on. The kind
    held from the run's start is the one 収める priced; nothing was priced
    with an image here, so the image is made of the kind its control shows
    and the check after the run prices it so -- not the kind the hidden
    control held when the run began."""
    at = os.path.join(cfg.work, "image_turned_on_during_run")
    shutil.rmtree(at, ignore_errors=True)
    os.makedirs(at)

    def pick(app, id, value):
        app.s.js("const s = document.getElementById(arguments[0]); s.value = arguments[1];"
                 " s.dispatchEvent(new Event('input', {bubbles: true}));", id, value)

    rows = [{"path": cfg.a}, {"path": cfg.c}, {"path": cfg.a}, {"path": cfg.c}]
    with App(cfg, "image_turned_on_during_run", rows,
             {"mode": "bdav", "dir": at, "subfolder": "night", "discTitle": "OW",
              "image": "", "imageAccess": "read-only"}) as app:
        app.to_list()
        for n in (1, 2, 3, 4):
            wait_for(f"row {n} read", lambda n=n: _walked(app, n), 120, 0.5)
        app.s.click('button.tab[data-screen="outset"]')
        app.s.click('button.tab[data-screen="out"]')
        label = wait_for("the start button", lambda: app.s.js(
            "const b = document.getElementById('run-export'); return !b.disabled && b.innerText"), 30)
        app.s.click("#run-export")
        stream = os.path.join(at, "night", "BDAV", "STREAM", "00001.m2ts")
        wait_for("the first recording being cut", lambda: os.path.exists(stream)
                 and os.path.getsize(stream) > 0, 60, 0.02)
        pick(app, "out-image", "2.50")
        pick(app, "out-image-access", "overwritable")
        reached = not os.path.exists(os.path.join(at, "night", "BDAV", "info.bdav"))
        wait_for("the run to finish", lambda: app.s.js(
            "return document.getElementById('run-export').innerText") == label, 300, 0.5)
        said = ""
        for root, _, files in os.walk(app.dir):
            for f in files:
                if f.endswith(".log") or f.endswith(".txt"):
                    with open(os.path.join(root, f), "rb") as fh:
                        said += fh.read().decode("utf-8", "replace")
        said = [l for l in said.splitlines() if "ディスクに収" in l or "GiB" in l][:3]
        said = " | ".join(said)
        if not reached:
            t.check("35b NOT REACHED: the index was written before the image was asked for", True, "")
        else:
            t.check("35b an image turned on mid-run: made and checked as the kind shown",
                    "22.5 GiB のディスクに収まります" in said
                    and os.path.exists(os.path.join(at, "night.iso")), repr(said[:300]))
    shutil.rmtree(at, ignore_errors=True)


def _export_or_refused(app, timeout=300):
    """出力開始, until the run is over -- or until it was refused before it
    started, which can be quicker than the button ever changing. Returns the
    note the output screen shows afterwards."""
    app.to_list()
    app.s.click('button.tab[data-screen="out"]')
    label = wait_for("the start button", lambda: app.s.js(
        "const b = document.getElementById('run-export'); return !b.disabled && b.innerText"), 30)
    note = lambda: app.s.js("return document.getElementById('out-note').textContent") or ""
    before = note()
    app.s.click("#run-export")
    seen = {"running": False}

    def over():
        running = app.s.js("return document.getElementById('run-export').innerText") != label
        seen["running"] = seen["running"] or running
        return not running and (seen["running"] or note() != before)
    wait_for("the run to end or be refused", over, timeout, 0.05)
    return note()


def _image_only_twice(cfg, t, name, second):
    """Two runs of イメージのみ into the same folder, as a series written week
    after week under its own name. The first run's folder is taken away once
    its image is made, so the second run would make a new disc of its own list
    -- and its image is named as the first one is. The second run is refused
    before anything is cut, whether it is the same list again or another one:
    the first image is still there as it was, no disc was started in the
    folder, and the screen says why."""
    at = os.path.join(cfg.work, name)
    shutil.rmtree(at, ignore_errors=True)
    os.makedirs(at)
    night = os.path.join(at, "night")
    settings = {"mode": "bdav", "dir": at, "subfolder": "night", "discTitle": "WEEK",
                "image": "2.50", "imageOnly": True}
    with App(cfg, name, [{"path": cfg.a}], settings) as app:
        app.to_list()
        wait_for("the row read", lambda: _walked(app, 1), 120, 0.5)
        app.s.click('button.tab[data-screen="outset"]')
        time.sleep(1.0)
        app.export()
        first = os.stat(night + ".iso") if os.path.exists(night + ".iso") else None
        t.check("32a the first image is written and the folder taken away",
                first is not None and not os.path.exists(os.path.join(night, "BDAV")),
                f"iso {first} BDAV {os.path.exists(os.path.join(night, 'BDAV'))}")
    time.sleep(1.1)
    with App(cfg, name + "_2", [{"path": second}], settings) as app:
        app.to_list()
        wait_for("the row read", lambda: _walked(app, 1), 120, 0.5)
        app.s.click('button.tab[data-screen="outset"]')
        time.sleep(1.0)
        said = _export_or_refused(app)
        now = os.stat(night + ".iso") if os.path.exists(night + ".iso") else None
        isos = sorted(f for f in os.listdir(at) if f.endswith(".iso") or f.endswith(".part"))
        stream = os.path.join(night, "BDAV", "STREAM")
        cut = sorted(os.listdir(stream)) if os.path.isdir(stream) else []
        t.check("32a the second run leaves the first image as it was",
                first is not None and now is not None and now.st_mtime_ns == first.st_mtime_ns
                and now.st_ino == first.st_ino and isos == ["night.iso"],
                f"first {first and (first.st_ino, first.st_mtime_ns)} now "
                f"{now and (now.st_ino, now.st_mtime_ns)}; images {isos}")
        t.check("32a and is refused before anything is cut, saying why",
                not cut and "night.iso" in said and "すでに" in said,
                f"streams {cut}; note {said!r}")
    shutil.rmtree(at, ignore_errors=True)


def image_only_twice(cfg, t):
    """The same list written image-only twice under one name."""
    _image_only_twice(cfg, t, "image_only_twice", cfg.a)


def image_only_twice_other(cfg, t):
    """Image-only under one name, the second time with another recording."""
    _image_only_twice(cfg, t, "image_only_twice_other", cfg.c)

def image_from_its_own_image(cfg, t):
    """A row read out of F/night.iso, written to F with the subfolder night:
    the run's image is the row's own source. Image-only, it was refused before
    the cut as an earlier run's image to move or delete; not image-only, it
    was refused only after the cut. Both must be refused before anything is
    cut, saying that it is the image the list is read from, with the image
    as it was."""
    at = os.path.join(cfg.work, "image_from_its_own_image")
    shutil.rmtree(at, ignore_errors=True)
    os.makedirs(at)
    night = os.path.join(at, "night")
    image = night + ".iso"
    settings = {"mode": "bdav", "dir": at, "subfolder": "night", "discTitle": "WEEK",
                "image": "2.50", "imageOnly": True}
    with App(cfg, "image_from_its_own_image", [{"path": cfg.a}], settings) as app:
        app.to_list()
        wait_for("the row read", lambda: _walked(app, 1), 120, 0.5)
        app.s.click('button.tab[data-screen="outset"]')
        time.sleep(1.0)
        app.export()
    first = os.stat(image) if os.path.exists(image) else None
    t.check("33i the image to read from is written", first is not None, f"iso {first}")
    if first is None:
        shutil.rmtree(at, ignore_errors=True)
        return
    inside = image + "/BDAV/STREAM/00001.m2ts"
    for only in (True, False):
        time.sleep(1.1)
        # The second time behind a row that is not from the image: every
        # row is looked at, not the first.
        rows = [{"path": inside}] if only else [{"path": cfg.c}, {"path": inside}]
        with App(cfg, f"image_from_its_own_image_{int(only)}", rows,
                 dict(settings, imageOnly=only)) as app:
            app.to_list()
            wait_for("the rows read", lambda: _walked(app, len(rows)), 120, 0.5)
            app.s.click('button.tab[data-screen="outset"]')
            time.sleep(1.0)
            said = _export_or_refused(app)
        now = os.stat(image) if os.path.exists(image) else None
        stream = os.path.join(night, "BDAV", "STREAM")
        cut = sorted(os.listdir(stream)) if os.path.isdir(stream) else []
        what = "image-only" if only else "folder and image"
        t.check(f"33i {what}: refused before the cut, as the image read from",
                not cut and ("読み込み元" in said or "read from" in said) and "すでに" not in said
                and now is not None and now.st_ino == first.st_ino
                and now.st_mtime_ns == first.st_mtime_ns,
                f"streams {cut}; note {said!r}; image {now and (now.st_ino, now.st_mtime_ns)}")
    # The folder typed with a `.` step in it (`F/.`): the backend reads it as
    # F and makes F/night.iso all the same, so the check before the cut has
    # to see that name too.
    time.sleep(1.1)
    with App(cfg, "image_from_its_own_image_dot", [{"path": inside}],
             dict(settings, imageOnly=False, dir=at + "/.")) as app:
        app.to_list()
        wait_for("the row read", lambda: _walked(app, 1), 120, 0.5)
        app.s.click('button.tab[data-screen="outset"]')
        time.sleep(1.0)
        said = _export_or_refused(app)
    now = os.stat(image) if os.path.exists(image) else None
    stream = os.path.join(night, "BDAV", "STREAM")
    cut = sorted(os.listdir(stream)) if os.path.isdir(stream) else []
    t.check("34i folder typed F/.: refused before the cut, as the image read from",
            not cut and ("読み込み元" in said or "read from" in said)
            and now is not None and now.st_ino == first.st_ino
            and now.st_mtime_ns == first.st_mtime_ns,
            f"streams {cut}; note {said!r}; image {now and (now.st_ino, now.st_mtime_ns)}")
    shutil.rmtree(at, ignore_errors=True)


def tracks_subtitle_stream(cfg, t):
    """b.mkv with a SubRip track: no cut carries it, and the track menu lists
    it as a subtitle stream SmartCut does not carry -- not as a data broadcast
    (the line the window fell back to before it knew the kind), and without
    the note whose reasons are the crawl's, a menu's and a disc's text
    subtitles'. Opened twice, the second time from what the first left."""
    at = os.path.join(cfg.work, "subtitle_stream")
    os.makedirs(at, exist_ok=True)
    srt = os.path.join(at, "s.srt")
    path = os.path.join(at, "sr.mkv")
    if not os.path.exists(path):
        with open(srt, "w") as f:
            f.write("1\n00:00:01,000 --> 00:00:03,000\nhello\n\n"
                    "2\n00:00:05,000 --> 00:00:07,000\nworld\n\n")
        subprocess.run(["ffmpeg", "-nostdin", "-y", "-loglevel", "error", "-i", cfg.b, "-i", srt,
                        "-map", "0", "-map", "1", "-c", "copy", "-c:s", "srt",
                        "-f", "matroska", path + ".part"], check=True)
        os.replace(path + ".part", path)
    for visit in (1, 2):
        with App(cfg, f"tracks_subtitle_stream_{visit}", [{"path": path}]) as app:
            app.open_row(1, "sr.mkv")
            app.s.click("#tracks")
            text = wait_for("the track list", lambda: app.s.js(
                "const m = document.getElementById('tracks-modal');"
                "const l = document.getElementById('tracks-list');"
                "return !m.hidden && l.children.length > 1 && l.innerText"), 30)
        t.check(f"33s visit {visit}: the SubRip track is a subtitle stream not carried",
                "出力できません: 字幕ストリーム（SmartCut が扱わない形式）" in text
                and "データ放送" not in text and "これらは選べません" not in text,
                repr(text))


# -- 19 --------------------------------------------------------------------
def make_late_head(cfg):
    """A recording with no key picture in its first 32 MB, which is as far as
    the outline looks for the head (core lib.rs `first_picture`): MPEG-2 at
    50 Mbit/s with an I picture every 20 s, and the first 2 MB -- the first I
    with them -- cut off. Its first key picture is about 120 MB in, so the
    editor reads its mark files only once the walk is over. Made only for the
    scenario that needs it: about 190 MB."""
    if os.path.exists(cfg.d):
        return
    full = cfg.d + ".full"
    subprocess.run([
        "ffmpeg", "-nostdin", "-y", "-loglevel", "error",
        "-f", "lavfi", "-i",
        "testsrc2=size=720x480:rate=30000/1001:duration=30,noise=alls=60:allf=t",
        "-f", "lavfi", "-i", "sine=frequency=440:duration=30:sample_rate=48000",
        "-c:v", "mpeg2video", "-g", "600", "-bf", "0",
        "-b:v", "50M", "-minrate", "50M", "-maxrate", "50M", "-bufsize", "8M",
        "-c:a", "mp2", "-b:a", "192k", "-f", "mpegts", full], check=True)
    with open(full, "rb") as f, open(cfg.d + ".part", "wb") as o:
        f.seek(188 * 10000)
        shutil.copyfileobj(f, o, 1 << 20)
    os.remove(full)
    os.replace(cfg.d + ".part", cfg.d)


def ctrl_h_before_marks_read(cfg, t):
    """A first visit to a recording whose head the outline could not find:
    its .keyframe is read once the walk is over, and Ctrl+H in that wait (with
    環境設定 quietOverwrite on, so nothing asks) wrote the empty timeline
    over it -- the file was gone before it was ever read. Ctrl+H is pressed
    over and over from the moment the window is up until the marks are in:
    the file must come through with its marks, the timeline must show them,
    and the refusal must have been said (else the walk was too quick for any
    press to land in it, and the run proves nothing)."""
    make_late_head(cfg)
    marks = [30, 60]
    side = os.path.join(cfg.media, "d.keyframe")
    with open(side, "w", newline="") as f:
        f.write("".join(f"{n}\n" for n in marks))
    with App(cfg, "ctrl_h_before_marks_read", [{"path": cfg.d}]) as app:
        app.s.js("localStorage.setItem('smartcut.quietOverwrite', 'true');")
        app.open_row(1, "d.ts", wait=False)
        app.to_editor()
        said = False
        presses = 0
        until = time.time() + 120
        while time.time() < until:
            try:
                app.s.keys("Control+h")
                presses += 1
                st = app.state()
            except WebDriverError:
                continue
            status = st["status"] or ""
            said = said or "まだ読み込んでいない" in status or "only read once" in status
            if st["keys"].startswith("2") and "読み込んで" not in st["plan"]:
                break
        time.sleep(0.5)
        with open(side, "rb") as f:
            got = [int(x) for x in f.read().split()]
        st = app.state()
        t.check("19 Ctrl+H during the walk leaves the .keyframe", got == marks,
                f"d.keyframe {got} (want {marks}) after {presses} presses")
        t.check("19 the marks reach the timeline", st["keys"].startswith("2"), repr(st["keys"]))
        t.check("19 a press landed before the marks were read", said,
                "refusal said" if said else "the walk ended before any press")



# -- 21 --------------------------------------------------------------------
def marks_over_recording(cfg, t):
    """名前を付けて保存 of the marks, with the recording's own name typed into
    the save dialog and its "replace?" answered yes (GTK asks about a file
    being there, not about which file): the recording must come through
    untouched and the status line say why. A name beside it is written as
    before, so the refusal is not of everything."""
    def digest(path):
        with open(path, "rb") as f:
            return hashlib.md5(f.read()).hexdigest()

    before = digest(cfg.a)
    avs = os.path.join(cfg.media, "a.avs")

    def save_as(app, kind, path):
        """The menu line, and the dialog given `path`; every question after
        it answered with Enter (GTK's replace question defaults to 置換)."""
        app.s.js(f"document.getElementById('status').textContent = '';"
                 f"document.getElementById('save-as-{kind}').click()")
        w = wait_for("the save dialog", lambda: (app.dialogs() or [None])[0], 10, 0.2)
        sh(f"xdotool windowactivate --sync {w}", app.env)
        time.sleep(0.5)
        sh("xdotool key --clearmodifiers ctrl+a", app.env)
        sh(f"xdotool type --delay 5 '{path}'", app.env)
        sh("xdotool key --clearmodifiers Return", app.env)
        for _ in range(2):
            time.sleep(0.8)
            up = app.dialogs()
            if not up:
                break
            sh(f"xdotool windowactivate --sync {up[0]}", app.env)
            sh("xdotool key --clearmodifiers Return", app.env)
        wait_for("the dialogs to go", lambda: not app.dialogs(), 10, 0.2)
        app.to_editor()
        return wait_for("the status line", lambda: app.state()["status"], 10)

    with App(cfg, "marks_over_recording", clips(cfg)) as app:
        app.open_row(1, "a.ts")
        # Beside it first, which also waits out the first visit's read of
        # its mark files (`marksDue`): a press before that is refused for
        # another reason.
        status = ""
        for _ in range(20):
            status = save_as(app, "trim", avs)
            if os.path.exists(avs):
                break
            time.sleep(1)
        t.check("21 a name beside the recording is saved", os.path.exists(avs), repr(status))
        for kind in ("trim", "keyframe"):
            status = save_as(app, kind, cfg.a)
            t.check(f"21 {kind} over the recording is refused",
                    digest(cfg.a) == before and ("録画そのもの" in status or "recording itself" in status),
                    f"status {status!r}, a.ts {'unchanged' if digest(cfg.a) == before else 'WRITTEN OVER'}")
        # And the commands themselves, whoever calls them.
        for cmd, args in (("write_keyframes", "{frames: [1], fps: 29.97}"),
                          ("write_sidecar", "{body: 'x'}")):
            said = app.s.js(
                f"return window.__TAURI__.core.invoke('{cmd}', Object.assign({args}, {{path: arguments[0]}}))"
                ".then(() => 'written', (e) => 'refused ' + JSON.stringify(String(e)));", cfg.a)
            t.check(f"21 {cmd} onto the recording is refused",
                    said.startswith("refused") and digest(cfg.a) == before, said)
        app.escape(discard=True, expect_dialog=False)
        app.gone()


# -- 22 --------------------------------------------------------------------
def rename_during_run(cfg, t):
    """Rows renamed while a run goes, with no numbers in the names: the first
    row (a.ts, written as `cut_a.ts`) renamed `z` once its turn has begun,
    and the second (c.ts) renamed `a` before its turn. The second is named at
    its turn and came out as `cut_a.ts` -- the file this run had just written
    -- and was written over it. It must be refused instead, and the first
    row's file left as it was written (one sound track; c.ts has two)."""
    rename = (
        "const ren = (n, name) => {"
        "  const li = document.querySelector(`#cliplist > li:nth-child(${n})`);"
        "  li.dispatchEvent(new MouseEvent('mousedown', {bubbles: true, button: 0}));"
        "  window.dispatchEvent(new MouseEvent('mouseup', {bubbles: true, button: 0}));"
        "  document.getElementById('rename-clip').click();"
        "  const f = document.querySelector('#cliplist input.rename');"
        "  f.value = name;"
        "  f.dispatchEvent(new KeyboardEvent('keydown', {key: 'Enter', bubbles: true}));"
        "};"
        "ren(1, 'z'); ren(2, 'a');"
        "return document.querySelector('#out-list > li:nth-child(2) .note').innerText;"
    )
    rows = [{"path": cfg.a}, {"path": cfg.c}]
    with App(cfg, "rename_during_run", rows, {"number": False}) as app:
        app.to_list()
        # ベリファイ on, so that the first row's turn lasts long enough for
        # the renames to land inside it.
        app.s.js("const b = document.getElementById('pref-verify');"
                 "if (!b.checked) b.click();")
        app.s.click('button.tab[data-screen="out"]')
        wait_for("both rows on the output screen", lambda: app.s.js(
            "return document.querySelectorAll('#out-list > li').length") == 2, 60)
        label = wait_for("the start button", lambda: app.s.js(
            "const b = document.getElementById('run-export'); return !b.disabled && b.innerText"), 60)
        time.sleep(0.5)
        first = os.path.join(app.out, "cut_a.ts")
        app.s.click("#run-export")
        wait_for("the first row's file", lambda: os.path.exists(first), 60, 0.02)
        second_then = app.s.js(rename)
        wait_for("the run to finish", lambda: app.s.js(
            "return document.getElementById('run-export').innerText") == label, 300, 0.5)
        names = sorted(os.listdir(app.out))
        note = app.s.js("return document.querySelector('#out-list > li:nth-child(2) .note').innerText")
        tracks = audio_tracks(first) if os.path.exists(first) else None
        t.check("22 the second row waited while the rows were renamed",
                "待機" in second_then or "waiting" in second_then.lower(), repr(second_then))
        t.check("22 the first row's file is not written over", tracks == 1,
                f"cut_a.ts has {tracks} sound tracks (want 1); files {names}")
        t.check("22 the second row says why", names == ["cut_a.ts"] and "今回の出力" in note,
                f"files {names}, row 2: {note!r}")


# -- 23 --------------------------------------------------------------------
def no_free_folder(cfg, t):
    """The run's own folder (`night`) with every branch up to `night-999`
    already there, as a hundred weekly runs leave them: no free name can be
    found, and the run used to go ahead under the plain name -- into the
    folder an earlier run wrote, its files laid over that run's. It must not
    start, and say why."""
    rows = [{"path": cfg.a}, {"path": cfg.b}]
    with App(cfg, "no_free_folder", rows, {"subfolder": "night"}) as app:
        for n in range(1, 1000):
            os.makedirs(os.path.join(app.out, "night" if n == 1 else f"night-{n}"))
        app.to_list()
        app.s.click('button.tab[data-screen="out"]')
        wait_for("both rows on the output screen", lambda: app.s.js(
            "return document.querySelectorAll('#out-list > li').length") == 2, 60)
        label = wait_for("the start button", lambda: app.s.js(
            "const b = document.getElementById('run-export'); return !b.disabled && b.innerText"), 60)
        time.sleep(0.5)
        app.s.click("#run-export")
        said = wait_for("the run's sentence", lambda: app.s.js(
            "return document.getElementById('out-note').textContent"), 30, 0.2)
        time.sleep(1.0)
        written = [os.path.join(r, f) for r, _, fs in os.walk(app.out) for f in fs]
        button = app.s.js("return document.getElementById('run-export').innerText")
        t.check("23 nothing written with no free folder name", not written,
                f"{[os.path.relpath(f, app.out) for f in written][:6]}")
        t.check("23 the run says why and is over", "フォルダー名" in said and button == label,
                f"note {said!r}, button {button!r}")


# -- 24 --------------------------------------------------------------------
def keyframe_twins(cfg, t):
    """Two rows both called `x`, from a .ts and a .mkv, written under 入力と同じ
    with the marks list on. The cuts were `cut_x.ts` and `cut_x.mkv` -- two
    names only by their extension, so not numbered -- and both lists (and any
    subtitle side files) were `cut_x.*`: the second row's written over the
    first's. Rows whose names differ only by extension are twins and get
    numbered: `cut_x_1.ts` + `cut_x_1.keyframe` (frame 60 only) and
    `cut_x_2.mkv` + `cut_x_2.keyframe`, and the output screen says so before
    the run."""
    rows = [
        {"path": cfg.a, "renamed": "x", "edit": {"cuts": [], "keyframes": [2.0]}},
        {"path": cfg.b, "renamed": "x", "edit": {"cuts": [], "keyframes": [5.0, 7.0]}},
    ]
    with App(cfg, "keyframe_twins", rows, {"number": False, "keyframes": True, "container": ""}) as app:
        app.to_list()
        app.s.click('button.tab[data-screen="out"]')
        wait_for("both rows on the output screen", lambda: app.s.js(
            "return document.querySelectorAll('#out-list > li').length") == 2, 60)
        label = wait_for("the start button", lambda: app.s.js(
            "const b = document.getElementById('run-export'); return !b.disabled && b.innerText"), 60)
        shown = []
        for k in (0, 1):
            shown.append(wait_for(f"row {k + 1}'s output name", lambda k=k: app.s.js(
                "const s = document.getElementById('outset-clip');"
                f"s.selectedIndex = {k}; s.dispatchEvent(new Event('change'));"
                "return document.getElementById('outset-format').innerText"), 30))
        t.check("24 the output screen names the twins _1 and _2",
                "cut_x_1.ts" in shown[0] and "cut_x_1.keyframe" in shown[0]
                and "cut_x_2.mkv" in shown[1] and "cut_x_2.keyframe" in shown[1],
                f"shown {[s[-160:] for s in shown]!r}")
        time.sleep(0.5)
        app.s.click("#run-export")
        time.sleep(1.0)
        wait_for("the run to finish", lambda: app.s.js(
            "return document.getElementById('run-export').innerText") == label, 300, 0.5)
        names = sorted(os.listdir(app.out))

        def body(name):
            side = os.path.join(app.out, name)
            return open(side).read().split() if os.path.exists(side) else None
        t.check("24 both cuts written, numbered", "cut_x_1.ts" in names and "cut_x_2.mkv" in names,
                f"files {names}")
        first, second = body("cut_x_1.keyframe"), body("cut_x_2.keyframe")
        t.check("24 each row has its own marks list", first == ["60"] and second is not None
                and len(second) == 2, f"cut_x_1.keyframe {first}, cut_x_2.keyframe {second}")


# -- 20 --------------------------------------------------------------------
def disc_join_crossing(cfg, t):
    """Two clips on a disc folder joined with a long dissolve, ベリファイ on.
    A disc's map does not say where its entry points' leading pictures are,
    and the copies either side of a crossing are planned to end or begin
    inside the ranges -- where the transition moves them -- not at the ends
    the list asked for. Unmeasured there, the copy before a 20 s dissolve
    ended on a guess (x264 open GOPs every 12 pictures): pictures doubled or
    lost, and the rest of the join out against its sound. The answer the
    command line gives with every point read (--index scan) is the count
    to match; run twice, the second from the index the first left behind.
    Needs the command line (GUITEST_CLI, default the tree's release build)
    to write the disc, and re-encodes 20 s of 1080p: not in the default list."""
    cli = os.environ.get("GUITEST_CLI", os.path.join(
        os.path.dirname(os.path.abspath(__file__)), "..", "..",
        "rust", "target", "release", "smartcut"))
    disc = os.path.join(cfg.work, "disc_join_crossing")
    clip = os.path.join(disc, "BDAV", "STREAM", "00001.m2ts")
    if not os.path.exists(clip):
        shutil.rmtree(disc, ignore_errors=True)
        src = os.path.join(cfg.work, "disc_join_crossing.ts")
        subprocess.run([
            "ffmpeg", "-nostdin", "-y", "-loglevel", "error",
            "-f", "lavfi", "-i", "testsrc2=size=1920x1080:rate=30000/1001:duration=40",
            "-f", "lavfi", "-i", "sine=frequency=440:sample_rate=48000:duration=40",
            "-c:v", "libx264", "-preset", "veryfast", "-x264-params",
            "open-gop=1:keyint=12:min-keyint=12:scenecut=0:bframes=3:b-adapt=0:"
            "b-pyramid=normal:ref=4", "-b:v", "6M", "-pix_fmt", "yuv420p",
            "-c:a", "aac", "-b:a", "192k", "-f", "mpegts", src], check=True)
        subprocess.run([cli, src, "--bdav", disc], check=True, capture_output=True)
        os.remove(src)
    want_file = os.path.join(cfg.work, "disc_join_crossing.want.ts")
    subprocess.run([cli, clip, "--join", clip, "--transition", "dissolve",
                    "--transition-seconds", "20", "--index", "scan", "-o", want_file],
                   check=True, capture_output=True)
    want = pictures(want_file)
    os.remove(want_file)
    rows = [{"path": clip, "after": {"kind": "dissolve", "seconds": 20}}, {"path": clip}]
    with App(cfg, "disc_join_crossing", rows, {"joinAll": True}) as app:
        app.to_list()
        app.s.js("const b = document.getElementById('pref-verify');"
                 "if (!b.checked) b.click();")
        app.s.click('button.tab[data-screen="out"]')
        wait_for("both rows on the output screen", lambda: app.s.js(
            "return document.querySelectorAll('#out-list > li').length") == 2, 120)
        time.sleep(0.5)
        for visit in ("fresh", "from the cache"):
            files = app.export(timeout=600)
            notes = app.s.js(
                "return [...document.querySelectorAll('#out-list > li .note')]"
                ".map((n) => [n.innerText, n.title]);")
            said = " | ".join(f"{a} [{b}]" for a, b in notes)
            got = pictures(files[0]) if len(files) == 1 else None
            t.check(f"20 dissolve 20 s on a disc clip ({visit})",
                    got is not None and got == want,
                    f"outputs {[os.path.basename(f) for f in files]}, {got} pictures "
                    f"(the CLI with every point read: {want})")
            t.check(f"20 ベリファイ OK ({visit})",
                    bool(notes) and all("ベリファイ OK" in a and "不一致" not in b for a, b in notes),
                    said)
            for f in files:
                os.remove(f)

# -- 27 --------------------------------------------------------------------
def disc_head_recount(cfg, t):
    """A recorder BD-RE clip whose disc map starts its entry points after the
    first key picture at the front of the file (half a second on the clip
    measured): the outline's head and the walk's points[0] disagree. A
    .keyframe beside it, written by this program, counts from points[0]; read
    before the walk (on the outline's head) its marks stood 15 frames early,
    and a Trim line cut 15 frames early. Ctrl+H once the walk is in must
    write back the numbers that were read. Needs such a disc: GUITEST_HEAD_CLIP
    names the clip (a path through an image is fine); not in the default list."""
    clip = os.environ.get("GUITEST_HEAD_CLIP")
    if not clip or not os.path.exists(clip.split(".iso/")[0] + (".iso" if ".iso/" in clip else "")):
        t.check("27 GUITEST_HEAD_CLIP names a clip", False, repr(clip))
        return
    home = os.path.join(cfg.work, "disc_head_recount")
    shutil.rmtree(home, ignore_errors=True)
    os.makedirs(home)
    marks = [300, 600, 900]
    side = os.path.join(home, "h.keyframe")
    for kind, body in (("keyframe", "".join(f"{n}\r\n" for n in marks)),
                       ("trim", "Trim(300,599) ++ Trim(900,1199)\r\n")):
        for f in os.listdir(home):
            os.remove(os.path.join(home, f))
        name = side if kind == "keyframe" else os.path.join(home, "h.m2ts.trim.avs")
        with open(name, "w", newline="") as f:
            f.write(body)
        rows = [{"path": clip, "home": home, "stem": "h"}]
        with App(cfg, "disc_head_recount", rows) as app:
            app.s.js("localStorage.setItem('smartcut.quietOverwrite', 'true');")
            app.open_row(1, "h", wait=False)
            app.to_editor()
            # The plan line says 「録画を読み込んでいます」 until the open is
            # over, recount and all (`opening` holds it).
            wait_for("the walk", lambda: (lambda st: st["frames"] is not None and st["plan"]
                     and "読み込んで" not in st["plan"] and "Reading" not in st["plan"])(app.state()),
                     180, 0.3)
            time.sleep(2)
            keys = "Control+h" if kind == "keyframe" else "Control+Shift+h"
            app.s.keys(keys)
            time.sleep(1.5)
            with open(name, "rb") as f:
                got = f.read().decode()
            want = body
            t.check(f"27 {kind} read and written back the same", got == want,
                    f"{got!r} (want {want!r}); status {app.state()['status']!r}")
            app.escape(discard=True, expect_dialog=False)
            app.gone()
    shutil.rmtree(home, ignore_errors=True)


# -- 28 --------------------------------------------------------------------
def _cli():
    return os.environ.get("GUITEST_CLI", os.path.join(
        os.path.dirname(os.path.abspath(__file__)), "..", "..",
        "rust", "target", "release", "smartcut"))


def _hms(text):
    h, m, s = text.split(":")
    return int(h) * 3600 + int(m) * 60 + float(s)


def _disc_row(cfg, disc, n, name):
    """Row `n` (1-based) of a disc, as the disc chooser hands it to the list:
    asked of the program itself (`resolve_paths`), so the play items and the
    chapters are what the window would have."""
    with App(cfg, name, clips(cfg)) as app:
        app.to_list()
        app.s.js("window.__found = null;"
                 "window.__TAURI__.core.invoke('resolve_paths', {paths: [arguments[0]]})"
                 ".then((r) => { window.__found = r; }, (e) => { window.__found = {error: String(e)}; });",
                 disc)
        got = wait_for("the disc read", lambda: app.s.js("return window.__found"), 300, 0.5)
    found = got[0]["files"][0]
    return found["clips"][n - 1]


def _chapters(path):
    out = subprocess.run(["ffprobe", "-v", "error", "-show_chapters", "-show_entries",
                          "format=duration", "-of", "json", path],
                         capture_output=True, text=True).stdout
    j = json.loads(out or "{}")
    return ([float(c["start_time"]) for c in j.get("chapters", [])],
            float(j.get("format", {}).get("duration", "nan")))


def _written(out):
    return sorted(os.path.join(out, f) for f in os.listdir(out) if f.endswith(".mkv"))


def _same_times(a, b, tol):
    return len(a) == len(b) and all(abs(x - y) <= tol for x, y in zip(a, b))


def disc_plays(cfg, t):
    """A recorder's title cut with no ranges of its own keeps what the disc's
    playlist plays: not the second of the programme before its first IN, the
    stretch either side of every break the recorder was stopped for, or the
    start of the next programme after its last OUT. The list cuts those as the
    row's first cuts once its walk has said where the clock begins, and the
    command line cuts the same (`disc::unplayed`): the row's cuts, the
    editor's counter, and the written file's length and chapters are the
    command line's. The editor shows them cut, 取消 takes them back, and a
    visit that comes before the list's walk (lanes stopped) cuts them itself;
    Escape out of that visit leaves them owed, and OK hands them over. Twice
    over: two rows of the title, the second from the index the first left.

    Needs a disc of recordings: GUITEST_PLAYS_DISC names the image or folder,
    GUITEST_PLAYS_TITLE the title (default 1), GUITEST_PLAYS_OUT a folder with
    room for two cuts of it. Not in the default list."""
    disc = os.environ.get("GUITEST_PLAYS_DISC")
    out = os.environ.get("GUITEST_PLAYS_OUT")
    n = int(os.environ.get("GUITEST_PLAYS_TITLE", "1"))
    if not disc or not os.path.exists(disc) or not out or not os.path.isdir(out):
        t.check("28 GUITEST_PLAYS_DISC/OUT name a disc and a folder", False, f"{disc!r} {out!r}")
        return
    said = subprocess.run([_cli(), disc, "--title", str(n), "--analyze"], text=True,
                          stdout=subprocess.PIPE, stderr=subprocess.STDOUT).stdout
    line = next((l for l in said.splitlines() if l.startswith("plays :")), "")
    want = [tuple(_hms(x) for x in p.strip().split("-")) for p in line.split("cut:", 1)[1].split(",")] \
        if "cut:" in line else []
    total = next((float(l.split(",")[1].split("s output")[0]) for l in said.splitlines()
                  if l.startswith("plan  :")), None)
    fps = next((float(l.split("fps")[0].split()[-1]) for l in said.splitlines()
                if "fps" in l and l.lstrip().startswith(("h264", "mpeg2video", "hevc", "vc1"))), 29.97)
    # And the title kept whole, which is what 取消 goes back to.
    whole_said = subprocess.run([_cli(), disc, "--title", str(n), "--keep", "0-99999", "--analyze"],
                                text=True, stdout=subprocess.PIPE, stderr=subprocess.STDOUT).stdout
    whole = next((float(l.split(",")[1].split("s output")[0]) for l in whole_said.splitlines()
                  if l.startswith("plan  :")), None)
    if not t.check("28 the command line cuts what the playlist leaves out", bool(want) and total and whole,
                   f"{line!r}; plan total {total}, whole {whole}"):
        return
    row = _disc_row(cfg, disc, n, "disc_plays")
    base = {"path": row["path"], "name": "p", "stem": "p", "home": out,
            "chapters": row["chapters"], "plays": row["plays"], "playsOwed": True}
    near = lambda got: len(got) == len(want) and all(
        abs(a - x) < 0.002 and abs(b - y) < 0.002 for (a, b), (x, y) in zip(got, want))
    cli_file = os.path.join(out, "cli.mkv")
    subprocess.run([_cli(), disc, "--title", str(n), "-o", cli_file],
                   check=True, capture_output=True)
    cli_chapters, cli_length = _chapters(cli_file)
    os.remove(cli_file)
    # The same stretches cut as somebody's own cuts: a chapter where each
    # range begins. The playlist's own cuts make none (the maintainer's
    # decision, 2026-10-09): what they keep is the disc's marks, so the
    # chapters are those of the cut by hand less some range starts.
    cut_args = []
    for a, b in want:
        cut_args += ["--cut", f"{a:.6f}-{b:.6f}"]
    subprocess.run([_cli(), disc, "--title", str(n), "-o", cli_file] + cut_args,
                   check=True, capture_output=True)
    hand_chapters, _ = _chapters(cli_file)
    os.remove(cli_file)
    t.check("28 the playlist's cuts make no chapters of their own",
            all(any(abs(c - h) <= 0.1 for h in hand_chapters) for c in cli_chapters)
            and len(cli_chapters) < len(hand_chapters),
            f"{cli_chapters} vs cut by hand {hand_chapters}")

    # The list first: its walk lands, the rows take their cuts.
    with App(cfg, "disc_plays", [dict(base), dict(base, name="q", stem="q")],
             {"dir": out, "container": "mkv"}) as app:
        def taken():
            p = app.save()
            return p if all(cuts_of(p, k) for k in (1, 2)) else None
        p = wait_for("the rows' playlist cuts", taken, 600, 2)
        for k, visit in ((1, "fresh"), (2, "from the index")):
            got = cuts_of(p, k)
            t.check(f"28 list cuts = the command line's ({visit})", near(got),
                    f"{fmt(got)} want {fmt(want)}")
        app.open_row(1, "p")
        wait_for("the walk", lambda: (lambda st: st["plan"] and "読み込んで" not in st["plan"]
                                      and "Reading" not in st["plan"])(app.state()), 300, 0.3)
        f1 = app.state()["frames"]
        t.check("28 editor counter = the command line's plan", abs(f1 - total * fps) <= 0.5 * fps,
                f"{f1} pictures, plan {total:.3f} s = {total * fps:.0f}")
        app.s.keys("Control+z")
        time.sleep(1)
        f2 = app.state()["frames"]
        t.check("28 取消 takes the playlist's cuts back", abs(f2 - whole * fps) <= 0.5 * fps,
                f"{f1} -> {f2} pictures (the title whole: {whole:.3f} s = {whole * fps:.0f})")
        app.escape(discard=True)
        app.gone()
        app.export(timeout=1800)
        files = _written(out)
        t.check("28 both rows written", len(files) == 2, f"{files}")
        for k, f in enumerate(files):
            ch, length = _chapters(f)
            t.check(f"28 written length = the command line's (row {k + 1})",
                    abs(length - cli_length) < 0.1, f"{length:.3f} vs {cli_length:.3f}")
            t.check(f"28 chapters = the command line's (row {k + 1})",
                    _same_times(ch, cli_chapters, 0.1), f"{ch} vs {cli_chapters}")
            os.remove(f)

    # The editor first: the lanes stopped before the walk, so the visit cuts.
    with App(cfg, "disc_plays", [dict(base)], {"dir": out, "container": "mkv"}) as app:
        app.to_list()
        app.s.js("const b = document.getElementById('stop-batch'); if (b && !b.disabled) b.click();")
        time.sleep(1)
        owed = cuts_of(app.save(), 1)
        app.open_row(1, "p")
        wait_for("the walk", lambda: (lambda st: st["plan"] and "読み込んで" not in st["plan"]
                                      and "Reading" not in st["plan"])(app.state()), 300, 0.3)
        time.sleep(2)
        f1 = app.state()["frames"]
        t.check("28 a first visit before the list's walk cuts them",
                owed == [] and abs(f1 - total * fps) <= 0.5 * fps,
                f"list had {fmt(owed)}; {f1} pictures, plan {total * fps:.0f}")
        app.escape(discard=False, expect_dialog=False)
        app.gone()
        p = app.save()
        t.check("28 Escape leaves them owed", cuts_of(p, 1) == [] and p["clips"][0].get("playsOwed"),
                f"cuts {fmt(cuts_of(p, 1))}, owed {p['clips'][0].get('playsOwed')}")
        app.open_row(1, "p")
        wait_for("the walk", lambda: (lambda st: st["plan"] and "読み込んで" not in st["plan"]
                                      and "Reading" not in st["plan"])(app.state()), 300, 0.3)
        time.sleep(2)
        app.ok()
        app.gone()
        p = app.save()
        got = cuts_of(p, 1)
        t.check("28 OK hands the editor's cuts over", near(got) and not p["clips"][0].get("playsOwed"),
                f"{fmt(got)} want {fmt(want)}; owed {p['clips'][0].get('playsOwed')}")


def disc_seam_gap_chapters(cfg, t):
    """A recorder's title whose sequence table overstates a stretch: the
    joined clock runs seconds past the stretch's last picture before the next
    begins, and no cut writes that clock. A row of it with nothing cut (no
    playlist cuts either, as a project from before them or a clip opened
    bare) is written with every chapter where the command line puts it -- the
    list counted the empty clock, and every chapter after it landed that much
    late, the last ones past the end. Needs GUITEST_PLAYS_DISC and
    GUITEST_PLAYS_OUT as `disc_plays`, and GUITEST_GAP_TITLE the title with
    such a seam. Not in the default list."""
    disc = os.environ.get("GUITEST_PLAYS_DISC")
    out = os.environ.get("GUITEST_PLAYS_OUT")
    n = os.environ.get("GUITEST_GAP_TITLE")
    if not disc or not os.path.exists(disc) or not out or not os.path.isdir(out) or not n:
        t.check("29 GUITEST_PLAYS_DISC/OUT/GAP_TITLE", False, f"{disc!r} {out!r} {n!r}")
        return
    n = int(n)
    row = _disc_row(cfg, disc, n, "disc_seam_gap_chapters")
    cli_file = os.path.join(out, "cli.mkv")
    # The whole clip, as the row is: an explicit keep, so no playlist cuts.
    subprocess.run([_cli(), disc, "--title", str(n), "--keep", "0-99999", "-o", cli_file],
                   check=True, capture_output=True)
    cli_chapters, cli_length = _chapters(cli_file)
    os.remove(cli_file)
    rows = [{"path": row["path"], "name": "g", "stem": "g", "home": out, "chapters": row["chapters"]}]
    with App(cfg, "disc_seam_gap_chapters", rows, {"dir": out, "container": "mkv"}) as app:
        for visit in ("fresh", "from the index"):
            app.export(timeout=1800)
            files = _written(out)
            got = _chapters(files[0]) if len(files) == 1 else ([], float("nan"))
            t.check(f"29 chapters across an empty seam = the command line's ({visit})",
                    _same_times(got[0], cli_chapters, 0.1),
                    f"{got[0]} vs {cli_chapters}; length {got[1]:.3f} vs {cli_length:.3f}")
            for f in files:
                os.remove(f)
        app.s.click('button.tab[data-screen="input"]')
        app.open_row(1, "g")
        wait_for("the walk", lambda: (lambda st: st["plan"] and "読み込んで" not in st["plan"]
                                      and "Reading" not in st["plan"])(app.state()), 300, 0.3)
        f1 = app.state()["frames"]
        fps = 30000 / 1001
        t.check("29 editor counter = the written length", abs(f1 - cli_length * fps) <= 0.5 * fps,
                f"{f1} pictures, written {cli_length:.3f} s = {cli_length * fps:.0f}")
        app.escape(discard=True, expect_dialog=False)
        app.gone()


def disc_seam_join_chapters(cfg, t):
    """A recorder's title joined to another clip with a long dissolve, the
    title's kept range running a few seconds past one of its seams. The
    engine plans the range in two at the seam and holds the crossing to half
    the piece after it (`takes_at`); the list held it to half the whole range
    (a seam that leaves no clock unwritten does not split `keepsOf`), so the
    next clip's chapter came out seconds early -- 7.2 s with a 10 s dissolve
    5.6 s past a seam. GUITEST_SEAMJOIN_DISC names the disc (an image is
    fine), GUITEST_SEAMJOIN_TITLE the title, GUITEST_SEAMJOIN_KEEPS the kept
    ranges to try, comma separated (each `a-b` holding a seam near b). The
    clip after is a minute cut from the same title (no conform). Compared
    with the command line's chapter there, which sits within two fields of
    the clip's pictures (measured: 50 ms). Not in the default list. Measured
    on a recorder BD-RE title (seams at 1024.414 with no clock unwritten and
    1968.665 with 18.9 s unwritten): 1900-1990 passes; 1000-1030 fails until
    the list is told the seams (ClipInfo `seams`, see `crossingPieces`)."""
    disc = os.environ.get("GUITEST_SEAMJOIN_DISC")
    n = os.environ.get("GUITEST_SEAMJOIN_TITLE")
    keeps = [k for k in os.environ.get("GUITEST_SEAMJOIN_KEEPS", "").split(",") if k]
    if not disc or not os.path.exists(disc) or not n or not keeps:
        t.check("30 GUITEST_SEAMJOIN_DISC/TITLE/KEEPS", False, f"{disc!r} {n!r} {keeps!r}")
        return
    n = int(n)
    row = _disc_row(cfg, disc, n, "disc_seam_join_chapters")
    b = os.path.join(cfg.work, "seamjoin_b.ts")
    if not os.path.exists(b):
        subprocess.run([_cli(), disc, "--title", str(n), "--keep", "100-160", "-o", b],
                       check=True, capture_output=True)
    want = {}
    took = {}
    for k in keeps:
        f = os.path.join(cfg.work, "seamjoin_cli.mkv")
        subprocess.run([_cli(), disc, "--title", str(n), "--keep", k, "--join", b,
                        "--transition", "dissolve", "--transition-seconds", "10", "-o", f],
                       check=True, capture_output=True)
        want[k] = _chapters(f)
        os.remove(f)
        # And joined with no crossing, where the clip after begins once the
        # range has been written whole: less the chapter above, what the
        # crossing took out of the file.
        subprocess.run([_cli(), disc, "--title", str(n), "--keep", k, "--join", b, "-o", f],
                       check=True, capture_output=True)
        plain = _chapters(f)[0]
        os.remove(f)
        took[k] = plain[-1] - want[k][0][-1] if plain and want[k][0] else float("nan")
    for k in keeps:
        a, z = (float(x) for x in k.split("-"))
        rows = [{"path": row["path"], "name": "g", "stem": "g", "chapters": row["chapters"],
                 "edit": {"cuts": [{"a": 0.0, "b": a}, {"a": z, "b": 99999.0}], "keyframes": []},
                 "after": {"kind": "dissolve", "seconds": 10}},
                {"path": b}]
        with App(cfg, "disc_seam_join_chapters", rows, {"joinAll": True, "container": "mkv"}) as app:
            app.to_list()
            wait_for("the title's walk", lambda: _walked(app, 1), 1800, 1)
            files = app.export(timeout=1800)
            mkv = [f for f in files if f.endswith(".mkv")]
            got = _chapters(mkv[0]) if len(mkv) == 1 else ([], float("nan"))
            # The last chapter is the one where the clip after begins (it has
            # none of its own). The disc's marks inside the range are left
            # out of the comparison: the row comes with an edit of no marks.
            t.check(f"30 the next clip's chapter after a dissolve at a seam = the command line's ({k})",
                    bool(got[0]) and bool(want[k][0]) and abs(got[0][-1] - want[k][0][-1]) <= 0.1,
                    f"{got[0]} vs {want[k][0]}; length {got[1]:.3f} vs {want[k][1]:.3f}")
            for f in files:
                os.remove(f)
            # The seam window holds the crossing it previews to what the file
            # has (`Seam::takes` over the pieces either side): bounded by the
            # whole kept range it showed a 10 s dissolve where the file has
            # under 3 s.
            app.s.click('button.tab[data-screen="outset"]')
            wait_for("the seam button", lambda: app.s.js(
                "const r = document.getElementById('row-cross');"
                "const b = document.getElementById('open-cross');"
                "return !!r && !r.hidden && !!b && !b.disabled;"), 60)
            before = set(app.s.handles())
            app.s.click("#open-cross")
            seam = wait_for("the seam window", lambda: (
                [h for h in app.s.handles() if h not in before] or [None])[0], 60)
            app.s.switch(seam)
            runs = wait_for("the crossing's length", lambda: app.s.js(
                "const v = document.getElementById('out-cross-time').textContent;"
                "return v && v !== '--:--:--.--' && v !== '00:00:00.00' && v;"), 120)
            h, m, sec = runs.split(":")
            runs = int(h) * 3600 + int(m) * 60 + float(sec)
            t.check(f"30 the seam window's crossing = what the file's crossing took ({k})",
                    abs(runs - took[k]) <= 0.1, f"window {runs:.3f} vs file {took[k]:.3f}")
            try:
                app.s.keys("Escape")
            except WebDriverError:
                pass
            wait_for("the seam window to close", lambda: seam not in app.s.handles(), 15)
            app.s.switch(app.main)


def _title_facts(disc, n, out):
    """What the command line says of title `n` cut with no ranges: the
    stretches the playlist leaves out, the first picture, the rate, and the
    written file's length and chapters (an .mkv cut, removed again)."""
    said = subprocess.run([_cli(), disc, "--title", str(n), "--analyze"], text=True,
                          env=dict(os.environ, SMARTCUT_DEBUG="1"),
                          stdout=subprocess.PIPE, stderr=subprocess.STDOUT).stdout
    line = next((l for l in said.splitlines() if l.startswith("plays :")), "")
    want = [tuple(_hms(x) for x in p.strip().split("-")) for p in line.split("cut:", 1)[1].split(",")] \
        if "cut:" in line else []
    dbg = next((l for l in said.splitlines() if l.startswith("  plays ")), "")
    head = float(dbg.split("head=")[1].split()[0]) if "head=" in dbg else None
    dur = float(dbg.split("dur=")[1].split()[0]) if "dur=" in dbg else None
    fps = next((float(l.split("fps")[0].split()[-1]) for l in said.splitlines()
                if "fps" in l and l.lstrip().startswith(("h264", "mpeg2video", "hevc", "vc1"))), 29.97)
    f = os.path.join(out, f"cli{n}.mkv")
    subprocess.run([_cli(), disc, "--title", str(n), "-o", f], check=True, capture_output=True)
    chapters, length = _chapters(f)
    os.remove(f)
    return {"want": want, "head": head, "dur": dur, "fps": fps, "chapters": chapters, "length": length}


def _near_cuts(got, want, tol=0.002):
    return len(got) == len(want) and all(
        abs(a - x) < tol and abs(b - y) < tol for (a, b), (x, y) in zip(got, want))


def _walked(app, n):
    """Row `n` of the list has been read (its cuts, if any, are in)."""
    return app.s.js(
        "const li = document.querySelector(`#cliplist > li:nth-child(${arguments[0]})`);"
        "return !!li && li.innerText.includes('インデックス');", n)


def disc_plays_rows(cfg, t):
    """The playlist's cuts (see `disc_plays`) through everything else a row
    goes through: a duplicate; a project saved and opened again (the cuts
    not taken twice, 取消 still there, 破棄 keeping them); rows of a project
    written before the playlist was read (no `plays` in them: their cuts are
    left as they were, and a row nobody cut stays whole); a Trim line written
    by the editor and read back on a first visit; a division into two parts
    written out with their .keyframe files; two titles joined into one file;
    and a disc of recordings written from the row. Every length and chapter
    against the command line's cut of the same title.

    Needs GUITEST_PLAYS_DISC / GUITEST_PLAYS_TITLE / GUITEST_PLAYS_OUT as
    `disc_plays`, and GUITEST_PLAYS_TITLE2 (default the title after) for the
    join. Not in the default list."""
    import re
    disc = os.environ.get("GUITEST_PLAYS_DISC")
    out = os.environ.get("GUITEST_PLAYS_OUT")
    n1 = int(os.environ.get("GUITEST_PLAYS_TITLE", "1"))
    n2 = int(os.environ.get("GUITEST_PLAYS_TITLE2", str(n1 + 1)))
    if not disc or not os.path.exists(disc) or not out or not os.path.isdir(out):
        t.check("30 GUITEST_PLAYS_DISC/OUT name a disc and a folder", False, f"{disc!r} {out!r}")
        return
    f1 = _title_facts(disc, n1, out)
    f2 = _title_facts(disc, n2, out)
    if not t.check("30 the command line cuts both titles", f1["want"] and f2["want"] and f1["head"] is not None,
                   f"{fmt(f1['want'])} / {fmt(f2['want'])} head {f1['head']}"):
        return
    fps = f1["fps"]
    r1 = _disc_row(cfg, disc, n1, "disc_plays_rows")
    r2 = _disc_row(cfg, disc, n2, "disc_plays_rows")
    owed = lambda r, name: {"path": r["path"], "name": name, "stem": name, "home": out,  # noqa: E731
                            "chapters": r["chapters"], "plays": r["plays"], "playsOwed": True}
    old = lambda r, name, edit=None: dict(  # noqa: E731
        {"path": r["path"], "name": name, "stem": name, "home": out, "chapters": r["chapters"]},
        **({"edit": edit} if edit else {}))
    hand = [(100.0, 200.0)]
    for f in os.listdir(out):
        if f.endswith((".trim.avs", ".keyframe", ".mkv")):
            os.remove(os.path.join(out, f))
    settings = {"dir": out, "container": "mkv"}

    # -- the list: four rows, one duplicated, saved --------------------------
    rows = [owed(r1, "a"), old(r1, "o", {"cuts": [{"a": 100.0, "b": 200.0}], "keyframes": []}),
            old(r1, "w"), owed(r2, "c")]
    with App(cfg, "disc_plays_rows", rows, settings) as app:
        def taken():
            p = app.save()
            return p if cuts_of(p, 1) and cuts_of(p, 4) and all(_walked(app, k) for k in (1, 2, 3, 4)) else None
        p = wait_for("the rows' playlist cuts", taken, 900, 2)
        time.sleep(3)
        p = app.save()
        t.check("30 a row owing them takes the playlist's cuts", _near_cuts(cuts_of(p, 1), f1["want"])
                and _near_cuts(cuts_of(p, 4), f2["want"]), f"{fmt(cuts_of(p, 1))} / {fmt(cuts_of(p, 4))}")
        t.check("30 a row from before keeps its own cuts, nothing added", cuts_of(p, 2) == hand,
                fmt(cuts_of(p, 2)))
        t.check("30 a row from before, never cut, stays whole", cuts_of(p, 3) == [], fmt(cuts_of(p, 3)))
        e1 = p["clips"][0].get("edit") or {}
        t.check("30 saved: not yet visited, the playlist's part said, nothing owed",
                e1.get("fresh") is True and e1.get("unplayed") and not p["clips"][0].get("playsOwed"),
                f"fresh {e1.get('fresh')} unplayed {e1.get('unplayed')} owed {p['clips'][0].get('playsOwed')}")
        app.to_list()
        app.s.click("#cliplist > li:nth-child(1) .nm")
        wait_for("クリップを複製", lambda: not app.s.js(
            "return document.getElementById('duplicate-clip').disabled"), 15)
        app.s.click("#duplicate-clip")
        wait_for("five rows", lambda: app.rows() == 5, 15)
        p = app.save()
        t.check("30 a duplicate carries the cuts", _near_cuts(cuts_of(p, 2), f1["want"]),
                fmt(cuts_of(p, 2)))
        saved = p["clips"]

    # -- the project opened again ------------------------------------------
    with App(cfg, "disc_plays_rows", saved, settings) as app:
        wait_for("the rows read", lambda: all(_walked(app, k) for k in range(1, 6)), 900, 2)
        time.sleep(3)
        p = app.save()
        t.check("30 reopened: the cuts as saved, not taken again",
                _near_cuts(cuts_of(p, 1), f1["want"]) and _near_cuts(cuts_of(p, 2), f1["want"])
                and cuts_of(p, 3) == hand and cuts_of(p, 4) == [] and _near_cuts(cuts_of(p, 5), f2["want"]),
                " / ".join(fmt(cuts_of(p, k)) for k in range(1, 6)))
        total = round(sum(b - a for a, b in _keeps_of(f1["want"], f1["dur"])) * fps)
        app.open_row(1, "a")
        wait_for("the walk", lambda: (lambda st: st["plan"] and "読み込んで" not in st["plan"]
                                      and "Reading" not in st["plan"])(app.state()), 300, 0.3)
        time.sleep(2)
        a1 = app.state()["frames"]
        app.s.keys("Control+z")
        time.sleep(1)
        a2 = app.state()["frames"]
        t.check("30 reopened project: the counter, and 取消 takes the playlist's cuts back",
                abs(a1 - f1["length"] * fps) <= 2 and a2 > a1 + 0.5 * fps,
                f"{a1} -> {a2} (written {f1['length'] * fps:.0f}; keeps {total})")
        app.escape(discard=True)
        app.gone()
        app.open_row(1, "a")
        wait_for("the walk", lambda: (lambda st: st["plan"] and "読み込んで" not in st["plan"]
                                      and "Reading" not in st["plan"])(app.state()), 300, 0.3)
        time.sleep(2)
        a3 = app.state()["frames"]
        t.check("30 破棄 of 取消 keeps the playlist's cuts", a3 == a1, f"{a1} then {a3}")
        # The Trim line of the row (Ctrl+Shift+H), beside it in `home`.
        for f in os.listdir(out):
            if f.endswith(".trim.avs"):
                os.remove(os.path.join(out, f))
        app.s.keys("Control+Shift+h")
        trim = wait_for("the Trim line", lambda: ([f for f in os.listdir(out) if f.endswith(".trim.avs")]
                                                  or [None])[0], 15)
        app.escape(discard=True, expect_dialog=False)
        app.gone()
    with open(os.path.join(out, trim), encoding="utf-8-sig") as fh:
        body = fh.read()
    got = [tuple(int(x) for x in m) for m in re.findall(r"Trim\((\d+),\s*(-?\d+)\)", body)]
    keeps = _keeps_of(f1["want"], f1["dur"])
    no = lambda s: round((s - f1["head"]) * fps)  # noqa: E731
    exp = [(max(0, no(a)), no(b) - 1) for a, b in keeps]
    t.check("30 the Trim line keeps what the playlist plays",
            len(got) == len(exp) and all(abs(x - y) <= 1 and (abs(u - v) <= 1 or v >= no(f1["dur"]) - 2)
                                         for (x, u), (y, v) in zip(got, exp)),
            f"{got} want {exp}")

    # -- that Trim line read back on a first visit -------------------------
    with App(cfg, "disc_plays_rows", [old(r1, "a")], settings) as app:
        app.open_row(1, "a")
        wait_for("the walk", lambda: (lambda st: st["plan"] and "読み込んで" not in st["plan"]
                                      and "Reading" not in st["plan"])(app.state()), 300, 0.3)
        time.sleep(3)
        b1 = app.state()["frames"]
        app.ok()
        app.gone()
        p = app.save()
        back = cuts_of(p, 1)
        inner = lambda cs: [c for c in cs if c[0] > f1["head"] + 0.01 and c[1] < f1["dur"] - 0.2]  # noqa: E731
        t.check("30 the Trim line read back gives the same cuts and counter",
                _near_cuts(inner(back), inner(f1["want"]), 1.5 / fps) and abs(b1 - a1) <= 3,
                f"{fmt(back)} want {fmt(f1['want'])}; {b1} pictures vs {a1}")
    os.remove(os.path.join(out, trim))

    # -- divided in two, written out with the marks ------------------------
    with App(cfg, "disc_plays_rows", [owed(r1, "d")], dict(settings, keyframes=True)) as app:
        wait_for("the row's playlist cuts", lambda: cuts_of(app.save(), 1), 600, 2)
        app.to_list()
        app.s.click("#cliplist > li:nth-child(1) .nm")
        wait_for("分割 to be offered", lambda: app.s.js(
            "return !document.getElementById('divide-clip').disabled"), 60)
        app.s.click("#divide-clip")
        wait_for("the divide box", lambda: app.s.js(
            "return !document.getElementById('divide').hidden"), 5)
        app.s.js("const f = document.getElementById('divide-rule'); f.value = 'parts';"
                 "f.dispatchEvent(new Event('change'));"
                 "const v = document.getElementById('divide-value'); v.value = '2';"
                 "v.dispatchEvent(new Event('input'));")
        wait_for("分割 to be pressable", lambda: app.s.js(
            "return !document.getElementById('divide-ok').disabled"), 60)
        app.s.click("#divide-ok")
        wait_for("two rows", lambda: app.rows() == 2, 10)
        p = app.save()
        c1, c2 = cuts_of(p, 1), cuts_of(p, 2)
        t.check("30 both parts keep the playlist's cuts",
                all(any(a <= x + 0.002 and y <= b + 0.002 for a, b in c) for c in (c1, c2) for x, y in f1["want"]),
                f"{fmt(c1)} / {fmt(c2)}")
        for f in os.listdir(out):
            if f.endswith((".mkv", ".keyframe")):
                os.remove(os.path.join(out, f))
        app.export(timeout=1800)
        mkvs = _written(out)
        sides = sorted(os.path.join(out, f) for f in os.listdir(out) if f.endswith(".keyframe"))
        parts = [_chapters(f) for f in mkvs]
        length = sum(l for _, l in parts)
        t.check("30 the two parts are the title's cut", len(mkvs) == 2 and abs(length - f1["length"]) < 0.2,
                f"{[os.path.basename(f) for f in mkvs]}: {length:.3f} vs {f1['length']:.3f}")
        joined = parts[0][0] + [parts[0][1] + c for c in parts[1][0]] if len(parts) == 2 else []
        cli = f1["chapters"]
        t.check("30 the parts' chapters are the title's (and the second part's start)",
                len(parts) == 2 and all(any(abs(c - j) < 0.1 for j in joined) for c in cli)
                and all(any(abs(c - j) < 0.1 for j in cli) or abs(c - parts[0][1]) < 0.1 for c in joined),
                f"{joined} vs {cli}")
        ok = True
        said = []
        for f, (ch, _) in zip(mkvs, parts):
            side = f[:-4] + ".keyframe"
            if side not in sides:
                continue
            with open(side, encoding="utf-8-sig") as fh:
                nums = [int(x) for x in re.findall(r"\d+", fh.read())]
            said.append((os.path.basename(side), nums[:8]))
            ok = ok and bool(nums) and all(any(abs(k / fps - c) <= 0.6 for c in ch) for k in nums)
        t.check("30 each part's .keyframe stands on its chapters", ok and said, f"{said}")
        for f in mkvs + sides:
            os.remove(f)

    # -- two titles joined into one file -------------------------------------
    with App(cfg, "disc_plays_rows", [owed(r1, "a"), owed(r2, "c")],
             dict(settings, joinAll=True)) as app:
        wait_for("both rows' playlist cuts", lambda: (lambda p: cuts_of(p, 1) and cuts_of(p, 2))(app.save()),
                 600, 2)
        app.export(timeout=2400)
        mkvs = _written(out)
        ch, length = _chapters(mkvs[0]) if len(mkvs) == 1 else ([], float("nan"))
        want_ch = f1["chapters"] + [f1["length"] + c for c in f2["chapters"]]
        t.check("30 the join is the two titles' cuts end to end",
                len(mkvs) == 1 and abs(length - f1["length"] - f2["length"]) < 0.2
                and _same_times(ch, want_ch, 0.12),
                f"{length:.3f} vs {f1['length'] + f2['length']:.3f}; {ch} vs {want_ch}")
        for f in mkvs:
            os.remove(f)

    # -- a disc of recordings from the row ----------------------------------
    with App(cfg, "disc_plays_rows", [owed(r1, "a")], {"dir": out, "mode": "bdav", "discTitle": "p"}) as app:
        wait_for("the row's playlist cuts", lambda: cuts_of(app.save(), 1), 600, 2)
        before = set(os.listdir(out))
        app.export(timeout=1800)
    made = [os.path.join(out, f) for f in os.listdir(out) if f not in before]
    folder = next((f for f in made if os.path.isdir(os.path.join(f, "BDAV"))),
                  out if any(os.path.basename(f) == "BDAV" for f in made) else None)
    row = _disc_row(cfg, folder, 1, "disc_plays_rows") if folder else {"chapters": []}
    # Less a mark within half a second of the end, which a file's chapter list
    # leaves out (`chapter_points`) and a disc's playlist does not: the
    # playlist's last item can be two pictures long.
    end = row["chapters"][0] + f1["length"] - 0.5 if row["chapters"] else 0
    got = [c for c in row["chapters"] if c < end]
    steps = lambda c: [round(y - x, 2) for x, y in zip(c, c[1:])]  # noqa: E731
    t.check("30 the disc's chapters are the command line's",
            len(got) == len(f1["chapters"]) and all(abs(x - y) < 0.12 for x, y in
                                                    zip(steps(got), steps(f1["chapters"]))),
            f"{[os.path.basename(f) for f in made]}: {steps(got)} vs {steps(f1['chapters'])}; "
            f"length {row.get('duration')} vs {f1['length']:.3f}")
    for f in made:
        shutil.rmtree(f, ignore_errors=True) if os.path.isdir(f) else os.remove(f)


def _keeps_of(cuts, dur):
    keeps, pos = [], 0.0
    for a, b in sorted(cuts):
        if a > pos + 1e-6:
            keeps.append((pos, a))
        pos = max(pos, b)
    if pos < dur - 1e-6:
        keeps.append((pos, dur))
    return keeps


def disc_plays_switch(cfg, t):
    """A disc row looked at in the editor and left for the next row before
    its walk has landed: the editor reported the row as it stood then (no
    playlist cuts yet, the visit's own cuts nothing), and the row must still
    take the playlist's cuts once the list reads it -- it was written whole
    before. The list's lanes are stopped first so that the editor's visit is
    the first. Needs GUITEST_PLAYS_DISC / GUITEST_PLAYS_TITLE /
    GUITEST_PLAYS_OUT as `disc_plays`. Not in the default list."""
    disc = os.environ.get("GUITEST_PLAYS_DISC")
    out = os.environ.get("GUITEST_PLAYS_OUT")
    n = int(os.environ.get("GUITEST_PLAYS_TITLE", "1"))
    if not disc or not os.path.exists(disc) or not out or not os.path.isdir(out):
        t.check("31 GUITEST_PLAYS_DISC/OUT name a disc and a folder", False, f"{disc!r} {out!r}")
        return
    said = subprocess.run([_cli(), disc, "--title", str(n), "--analyze"], text=True,
                          stdout=subprocess.PIPE, stderr=subprocess.STDOUT).stdout
    line = next((l for l in said.splitlines() if l.startswith("plays :")), "")
    want = [tuple(_hms(x) for x in p.strip().split("-")) for p in line.split("cut:", 1)[1].split(",")] \
        if "cut:" in line else []
    row = _disc_row(cfg, disc, n, "disc_plays_switch")
    rows = [{"path": row["path"], "name": name, "stem": name, "home": out, "chapters": row["chapters"],
             "plays": row["plays"], "playsOwed": True} for name in ("a", "b")]
    for leave in ("editor", "list", "stepped", "cut"):
        with App(cfg, "disc_plays_switch", rows, {"dir": out, "container": "mkv"}) as app:
            app.to_list()
            app.s.js("const b = document.getElementById('stop-batch'); if (b && !b.disabled) b.click();")
            time.sleep(1)
            app.open_row(1, "a")
            time.sleep(0.5)
            early = app.state()["plan"]
            if leave == "stepped":
                # A step along the timeline first, which the editor reports.
                app.s.keys("Right")
                time.sleep(0.6)
            if leave == "cut":
                # A cut of somebody's own during the walk (allowed), which is
                # an edit the list is told of at once.
                # Not `app.cut`, which waits for the count to drop by the
                # cut's pictures exactly: a walk landing in between (the
                # map of a disc read before is quick) takes the playlist's
                # cuts off it at the same time.
                was = app.state()["frames"]
                app.goto(300)
                app.s.keys("i")
                app.goto(329)
                app.s.keys("o")
                app.s.keys("Delete")
                wait_for("the cut 300-329", lambda: app.state()["frames"] <= was - 30, 10)
                time.sleep(0.6)
                early = app.state()["plan"]
            if leave in ("editor", "stepped", "cut"):
                # The next row, from the list, while row 1 is still being read.
                app.open_row(2, "b", wait=False)
                app.editor_ready("b")
            else:
                app.escape(discard=False, expect_dialog=False)
                app.gone()
            app.to_list()
            p0 = app.save()
            e0 = p0["clips"][0].get("edit") or {}
            # Whether the case was reached: a row whose editor's walk landed
            # before it was left comes away with the playlist's cuts already
            # in (the editor's own), and then the check below passes whatever
            # the list does with a row left mid-walk. Said, not failed: how
            # fast a disc's map lands is the machine's, not the program's.
            c0 = cuts_of(p0, 1)
            landed = bool(want) and all(any(a <= x + 0.002 and y <= b + 0.002 for a, b in c0) for x, y in want)
            left = (f"left with cuts {fmt(c0)} fresh {e0.get('fresh')} "
                    f"owed {p0['clips'][0].get('playsOwed')}"
                    f"{' (NOT REACHED: the walk had landed before the row was left)' if landed and leave != 'list' else ''}")
            # The lanes again: the list reads the rows now.
            app.s.js("const b = document.getElementById('stop-batch'); if (b && !b.disabled) b.click();")
            try:
                wait_for("row 1 read", lambda: "インデックス" in app.s.js(
                    "return document.querySelector('#cliplist > li:nth-child(1)').innerText"), 600, 2)
            except Failure:
                pass
            time.sleep(3)
            if app.editor():
                app.to_editor()
                app.escape(discard=True, expect_dialog=False)
                app.gone()
            p = app.save()
            got = cuts_of(p, 1)
            # Every one of them, beside the hand's own cut where there is one.
            t.check(f"31 a row left mid-walk ({leave}) still takes the playlist's cuts",
                    bool(got) and all(any(a <= x + 0.002 and y <= b + 0.002 for a, b in got) for x, y in want)
                    and len(got) <= len(want) + 1,
                    f"{fmt(got)} want {fmt(want)}; plan when left: {early!r}; {left}; "
                    f"owed {p['clips'][0].get('playsOwed')}")


def disc_plays_due(cfg, t):
    """An edit the editor sent before its walk had landed -- a cut made while
    it read, on a first visit that came before the list's walk -- holds the
    hand's cut and none of the playlist's (`playsDue` in it). The list must
    still cut what the playlist leaves out on top of it: when its own walk
    lands, and at once where it has landed already. Before, the list took
    such an edit for the editor having had its say and dropped the
    playlist's cuts, and the row was written with all of it.

    The editor's walk of a disc title is its map, which lands faster than a
    hand can cut, so the edit is sent the way the editor sends it
    (`editor-state`) rather than by racing the walk. Needs
    GUITEST_PLAYS_DISC / GUITEST_PLAYS_TITLE / GUITEST_PLAYS_OUT as
    `disc_plays`. Not in the default list."""
    disc = os.environ.get("GUITEST_PLAYS_DISC")
    out = os.environ.get("GUITEST_PLAYS_OUT")
    n = int(os.environ.get("GUITEST_PLAYS_TITLE", "1"))
    if not disc or not os.path.exists(disc) or not out or not os.path.isdir(out):
        t.check("33 GUITEST_PLAYS_DISC/OUT name a disc and a folder", False, f"{disc!r} {out!r}")
        return
    said = subprocess.run([_cli(), disc, "--title", str(n), "--analyze"], text=True,
                          stdout=subprocess.PIPE, stderr=subprocess.STDOUT).stdout
    line = next((l for l in said.splitlines() if l.startswith("plays :")), "")
    want = [tuple(_hms(x) for x in p.strip().split("-")) for p in line.split("cut:", 1)[1].split(",")] \
        if "cut:" in line else []
    if not t.check("33 the command line cuts what the playlist leaves out", bool(want), line):
        return
    row = _disc_row(cfg, disc, n, "disc_plays_due")
    rows = [{"path": row["path"], "name": "a", "stem": "a", "home": out, "chapters": row["chapters"],
             "plays": row["plays"], "playsOwed": True}]
    hand = (300.0, 329.0)

    def sent(app):
        app.to_list()
        app.s.js("window.__TAURI__.event.emit('editor-state', {id: 1, path: arguments[0],"
                 " cuts: [{a: arguments[1], b: arguments[2]}], keyframes: [], activeKey: null,"
                 " cmBlocks: [], cmNote: '', cmFinding: null, detectedWith: {}, flatRuns: [],"
                 " markFileKind: null, chaptersDue: true, unplayed: [], playsDue: true, touched: true,"
                 " dropStreams: [], poster: null, playhead: 0, selA: 0, selB: 0, selGone: true});",
                 row["path"], hand[0], hand[1])
        time.sleep(1.5)

    def all_in(got):
        return all(any(a <= x + 0.002 and y <= b + 0.002 for a, b in got) for x, y in want) \
            and any(a <= hand[0] + 0.002 and hand[1] <= b + 0.002 for a, b in got)

    for when in ("before the list's walk", "after it"):
        with App(cfg, "disc_plays_due", rows, {"dir": out, "container": "mkv"}) as app:
            app.to_list()
            if when == "before the list's walk":
                wait_for("解析を中止", lambda: app.s.js(
                    "const b = document.getElementById('stop-batch'); return !!b && !b.disabled;"), 30, 0.2)
                app.s.click("#stop-batch")
                time.sleep(1)
                had = cuts_of(app.save(), 1)
                sent(app)
                # Resumed, and only that: a press here while the first one was
                # still stopping -- or where it found nothing running yet --
                # stopped the walk instead, which then never landed and the
                # check failed for a reason of its own.
                wait_for("解析を再開", lambda: app.s.js(
                    "const b = document.getElementById('stop-batch');"
                    "return !!b && !b.disabled && ['解析を再開', 'Resume analysis'].includes(b.textContent);"), 30, 0.3)
                app.s.click("#stop-batch")
                wait_for("row 1 read", lambda: _walked(app, 1), 600, 2)
                time.sleep(2)
            else:
                wait_for("row 1 read", lambda: _walked(app, 1), 600, 2)
                had = cuts_of(app.save(), 1)
                sent(app)
            p = app.save()
            got = cuts_of(p, 1)
            t.check(f"33 an edit sent mid-walk still takes the playlist's cuts ({when})",
                    all_in(got) and not p["clips"][0].get("playsOwed")
                    and not (p["clips"][0].get("edit") or {}).get("playsDue"),
                    f"{fmt(got)} want {fmt(want)} + {hand}; before {fmt(had)}; "
                    f"owed {p['clips'][0].get('playsOwed')} due {(p['clips'][0].get('edit') or {}).get('playsDue')}")


def disc_plays_recount(cfg, t):
    """A disc title's row with the playlist's cuts (taken by the list) and a
    Trim line or a .keyframe beside it that this program wrote for the same
    row: a first visit reads the file before the walk, on the outline's head,
    and on a recorder's clip whose map starts later than that (see
    `disc_head_recount`) reads it again on the walk's head, with the
    playlist's cuts kept on top. Written back at once (Ctrl+Shift+H / Ctrl+H)
    the file must come out as it went in, and the counter as it was. Needs
    GUITEST_PLAYS_DISC / GUITEST_PLAYS_TITLE / GUITEST_PLAYS_OUT as
    `disc_plays`; worth running on a title whose first clip is such a clip.
    Not in the default list."""
    disc = os.environ.get("GUITEST_PLAYS_DISC")
    out = os.environ.get("GUITEST_PLAYS_OUT")
    n = int(os.environ.get("GUITEST_PLAYS_TITLE", "1"))
    if not disc or not os.path.exists(disc) or not out or not os.path.isdir(out):
        t.check("32 GUITEST_PLAYS_DISC/OUT name a disc and a folder", False, f"{disc!r} {out!r}")
        return
    r = _disc_row(cfg, disc, n, "disc_plays_recount")
    rows = [{"path": r["path"], "name": "h", "stem": "h", "home": out, "chapters": r["chapters"],
             "plays": r["plays"], "playsOwed": True}]

    def sides():
        return sorted(f for f in os.listdir(out) if f.startswith("h.") and f.endswith((".keyframe", ".trim.avs")))

    def clear():
        for f in sides():
            os.remove(os.path.join(out, f))

    def walked(app):
        wait_for("the walk", lambda: (lambda st: st["frames"] is not None and st["plan"]
                 and "読み込んで" not in st["plan"] and "Reading" not in st["plan"])(app.state()), 300, 0.3)
        time.sleep(2)

    clear()
    written = {}
    with App(cfg, "disc_plays_recount", rows, {"dir": out}) as app:
        app.s.js("localStorage.setItem('smartcut.quietOverwrite', 'true');")
        wait_for("the row's playlist cuts", lambda: cuts_of(app.save(), 1), 600, 2)
        app.open_row(1, "h")
        walked(app)
        frames = app.state()["frames"]
        for keys in ("Control+Shift+h", "Control+h"):
            app.s.keys(keys)
            time.sleep(1.5)
        for f in sides():
            with open(os.path.join(out, f), "rb") as fh:
                written[f] = fh.read().decode()
        app.escape(discard=True, expect_dialog=False)
        app.gone()
    if not t.check("32 the Trim line and the marks written", len(written) == 2, f"{sorted(written)}"):
        return
    for name, body in sorted(written.items()):
        clear()
        with open(os.path.join(out, name), "w", newline="") as fh:
            fh.write(body)
        with App(cfg, "disc_plays_recount", rows, {"dir": out}) as app:
            app.s.js("localStorage.setItem('smartcut.quietOverwrite', 'true');")
            wait_for("the row's playlist cuts", lambda: cuts_of(app.save(), 1), 600, 2)
            # Twice: the second visit is a first one still (Escape with
            # nothing done), and reads the index the first one left.
            seen = []
            for visit in ("fresh", "from the index"):
                app.open_row(1, "h")
                walked(app)
                got_frames = app.state()["frames"]
                app.s.keys("Control+Shift+h" if name.endswith(".trim.avs") else "Control+h")
                time.sleep(1.5)
                with open(os.path.join(out, name), "rb") as fh:
                    seen.append((fh.read().decode(), got_frames))
                status = app.state()["status"]
                app.escape(discard=True, expect_dialog=False)
                app.gone()
                # The file as it was, for the second visit to read.
                with open(os.path.join(out, name), "w", newline="") as fh:
                    fh.write(body)
            got, got_frames = seen[0]
            if seen[1] != seen[0]:
                got += f" [second visit: {seen[1][0][:120]!r}, {seen[1][1]} pictures]"
        # Every mark back where it was, the one on a playlist item's IN as
        # well: it came back up to half a frame inside the cut in front of
        # it and was dropped (rev27 p2plays: 4 of 8 on one title), until the
        # cuts went on the pictures and a mark read that close to a cut's
        # end went back onto it (`readKeyframeFile`).
        same = got == body
        t.check(f"32 {name} read on a first visit and written back the same",
                same and abs(got_frames - frames) <= 1,
                f"{got[:200]!r} (want {body[:200]!r}); {got_frames} pictures (were {frames}); {status!r}")
    clear()


def _start_time(path):
    out = subprocess.run(["ffprobe", "-v", "error", "-show_entries", "format=start_time",
                          "-of", "csv=p=0", path], capture_output=True, text=True).stdout
    return float(out.strip() or "0")


def plays_owed_left_behind(cfg, t):
    """A row with a playlist's cuts still owed (`plays`, `playsOwed`, as a
    disc row arrives) that the editor is on, read through by the list while
    the editor had it (so the list's walk did not take them), whose last
    report from the editor came before that visit's own walk landed
    (`playsDue`): the editor is moved on to another row, and a duplicate is
    made of it from the list. Neither the row nor its copy may be left with
    the playlist's cuts never taken -- nothing else came to take them, and
    both were written whole. The report is sent the way the editor sends it,
    since a short recording's walk lands faster than a hand can switch rows.
    Any recording will do: the play items are made up on a.ts's clock."""
    st = _start_time(cfg.a)
    plays = [[st + 2.0, st + 8.0], [st + 12.0, st + 18.0]]
    want = [(0.0, 2.0), (8.0, 12.0), (18.0, 20.0)]
    rows = [{"path": cfg.a, "plays": plays, "playsOwed": True}, {"path": cfg.c}]

    def covered(got):
        # Each owed stretch inside a cut (to a picture either side).
        return all(any(a <= x + 0.05 and y - 0.05 <= b for a, b in got) for x, y in want[:2])

    with App(cfg, "plays_owed_left_behind", rows) as app:
        app.to_list()
        wait_for("row 1 read", lambda: _walked(app, 1), 120, 0.5)
        time.sleep(1)
        app.open_row(1, "a")
        time.sleep(2)
        app.to_list()
        # The report of a visit whose walk had not landed: no cuts yet, the
        # playlist's still owed. (The list cannot be made to read a row
        # while the editor is on it -- it starts no walk there -- so the
        # row is read first, and this stands for the report that put it in
        # that state.)
        app.s.js("window.__TAURI__.event.emit('editor-state', {id: 1, path: arguments[0],"
                 " cuts: [], keyframes: [], activeKey: null,"
                 " cmBlocks: [], cmNote: '', cmFinding: null, detectedWith: {}, flatRuns: [],"
                 " markFileKind: null, chaptersDue: true, unplayed: [], playsDue: true, touched: false,"
                 " dropStreams: [], poster: null, playhead: 0, selA: 0, selB: 0, selGone: true});",
                 cfg.a)
        time.sleep(1)
        app.s.click("#cliplist > li:nth-child(1) .nm")
        time.sleep(0.3)
        app.s.click("#duplicate-clip")
        wait_for("the copy", lambda: app.rows() == 3, 10)
        app.open_row(3, "c")
        time.sleep(1.5)
        p = app.save()
        for n, what in ((1, "the row the editor left"), (2, "its copy")):
            got = cuts_of(p, n)
            t.check(f"34 {what} takes the playlist's cuts", covered(got),
                    f"{fmt(got)} want {fmt(want)}; owed {p['clips'][n - 1].get('playsOwed')}")
        app.to_editor()
        app.escape(discard=True, expect_dialog=False)
        app.gone()


# -- 34b (rev29 appjs) -------------------------------------------------------
def plays_owed_removed_undone(cfg, t):
    """`plays_owed_left_behind`'s row (playlist's cuts still owed, read
    through by the list, the editor's last report from before its walk
    landed), taken out of the list while the editor is on it -- which closes
    the editor -- and put back with 削除を元に戻す (`restoreRemoved`). And a
    copy made of it first (`duplicate`), the source then taken out and put
    back. Neither may come back with the playlist's cuts never taken."""
    st = _start_time(cfg.a)
    plays = [[st + 2.0, st + 8.0], [st + 12.0, st + 18.0]]
    want = [(0.0, 2.0), (8.0, 12.0), (18.0, 20.0)]
    rows = [{"path": cfg.a, "plays": plays, "playsOwed": True}, {"path": cfg.c}]

    def covered(got):
        return all(any(a <= x + 0.05 and y - 0.05 <= b for a, b in got) for x, y in want[:2])

    def due(app):
        app.s.js("window.__TAURI__.event.emit('editor-state', {id: 1, path: arguments[0],"
                 " cuts: [], keyframes: [], activeKey: null,"
                 " cmBlocks: [], cmNote: '', cmFinding: null, detectedWith: {}, flatRuns: [],"
                 " markFileKind: null, chaptersDue: true, unplayed: [], playsDue: true, touched: false,"
                 " dropStreams: [], poster: null, playhead: 0, selA: 0, selB: 0, selGone: true});",
                 cfg.a)
        time.sleep(1)

    for copy_first in (False, True):
        with App(cfg, "plays_owed_removed_undone", rows) as app:
            app.to_list()
            wait_for("row 1 read", lambda: _walked(app, 1), 120, 0.5)
            time.sleep(1)
            app.open_row(1, "a")
            time.sleep(2)
            app.to_list()
            due(app)
            app.s.click("#cliplist > li:nth-child(1) .nm")
            time.sleep(0.3)
            if copy_first:
                app.s.click("#duplicate-clip")
                wait_for("the copy", lambda: app.rows() == 3, 10)
                app.s.click("#cliplist > li:nth-child(1) .nm")
                time.sleep(0.3)
            n = app.rows()
            app.s.click("#remove-clip")
            wait_for("the row out", lambda: app.rows() == n - 1, 10)
            app.gone()
            app.to_list()
            app.s.click("#undo-remove")
            wait_for("the row back", lambda: app.rows() == n, 10)
            time.sleep(1.5)
            p = app.save()
            which = ((1, "the row put back"), (2, "its copy")) if copy_first else ((1, "the row put back"),)
            for k, what in which:
                got = cuts_of(p, k)
                t.check(f"34b {what} takes the playlist's cuts ({'copy first' if copy_first else 'removed'})",
                        covered(got), f"{fmt(got)} want {fmt(want)}; owed {p['clips'][k - 1].get('playsOwed')}")


# -- 34c (rev29 p2gui) -------------------------------------------------------
def plays_due_snapped(cfg, t):
    """`plays_owed_left_behind`'s report (the editor's last word on a disc
    row came before its walk landed, `playsDue`), now holding what that
    visit read before its walk: a Trim line written with the playlist's cuts
    in it, each end a little under half a frame off them, a cut of the
    hand's, and a mark just inside the end of one of the playlist's cuts.
    The editor moves on and the list takes the playlist's cuts (`takePlays`
    with `due`). The line's ends must go onto the playlist's (merged raw, the
    two together cut the more of either, a picture lost at each end), all of
    the playlist's cuts must be the playlist's (`unplayed`; counted against
    the line they were nobody's, and their joins became chapters), and the
    mark must stay, on the cut's end. A twin row nobody opened gives the
    playlist's cuts as the list takes them."""
    st = _start_time(cfg.a)
    plays = [[st + 2.0, st + 8.0], [st + 12.0, st + 18.0]]
    rows = [{"path": cfg.a, "plays": plays, "playsOwed": True},
            {"path": cfg.a, "plays": plays, "playsOwed": True}, {"path": cfg.c}]
    off = 0.008  # under half a frame at up to 59.94p
    hand = (4.0, 5.0)

    with App(cfg, "plays_due_snapped", rows) as app:
        app.to_list()
        wait_for("rows read", lambda: _walked(app, 1) and _walked(app, 2), 120, 0.5)
        time.sleep(1)
        u = cuts_of(app.save(), 2)
        if not t.check("34c the twin row takes the playlist's cuts", len(u) >= 2, fmt(u)):
            return
        app.open_row(1, "a")
        time.sleep(2)
        app.to_list()
        line = [{"a": a + off, "b": b + off} for a, b in u] + [{"a": hand[0], "b": hand[1]}]
        line.sort(key=lambda c: c["a"])
        mark = u[1][1] - off
        app.s.js("window.__TAURI__.event.emit('editor-state', {id: 1, path: arguments[0],"
                 " cuts: arguments[1], keyframes: [arguments[2]], activeKey: null,"
                 " cmBlocks: [], cmNote: '', cmFinding: null, detectedWith: {}, flatRuns: [],"
                 " markFileKind: 'trim', chaptersDue: false, unplayed: [], playsDue: true, touched: false,"
                 " dropStreams: [], poster: null, playhead: 0, selA: 0, selB: 0, selGone: true});",
                 cfg.a, line, mark)
        time.sleep(1)
        app.open_row(3, "c")
        time.sleep(1.5)
        p = app.save()
        got = cuts_of(p, 1)
        want = sorted(u + [hand])
        merged = []
        for a, b in want:
            if merged and a <= merged[-1][1]:
                merged[-1] = (merged[-1][0], max(merged[-1][1], b))
            else:
                merged.append((a, b))
        t.check("34c a Trim line read before the walk is put onto the playlist's cuts",
                _near_cuts(got, merged, 1e-5), f"{fmt(got)} want {fmt(merged)}")
        e1 = p["clips"][0].get("edit") or {}
        unplayed = [(c["a"], c["b"]) for c in e1.get("unplayed") or []]
        t.check("34c all of the playlist's cuts are the playlist's",
                _near_cuts(unplayed, u, 1e-5), f"{fmt(unplayed)} want {fmt(u)}")
        keys = e1.get("keyframes") or []
        t.check("34c a mark just inside a playlist cut's end stays, on the end",
                any(abs(k - u[1][1]) < 1e-5 for k in keys), f"{keys} want {u[1][1]:.6f}")
        app.to_editor()
        app.escape(discard=True, expect_dialog=False)
        app.gone()



# -- 34d (rev30 appjs) -------------------------------------------------------
def plays_due_selection(cfg, t):
    """`plays_due_snapped`'s report (the editor's last word on a disc row came
    before its walk landed, `playsDue`), with IN and OUT set during the walk
    at 5 s and 6 s of a timeline with no cuts yet. The editor moves on and the
    list takes the playlist's cuts (`takePlays` with `due`), which take 2 s
    off the head: IN and OUT are places on the output, and left at 5 and 6
    they stood over other pictures (7 s and 8 s of the recording) when the
    row came back to the editor, and a Del took those. They must stay on
    their pictures, at 3 s and 4 s."""
    st = _start_time(cfg.a)
    plays = [[st + 2.0, st + 8.0], [st + 12.0, st + 18.0]]
    rows = [{"path": cfg.a, "plays": plays, "playsOwed": True}, {"path": cfg.c}]
    with App(cfg, "plays_due_selection", rows) as app:
        app.to_list()
        wait_for("row 1 read", lambda: _walked(app, 1), 120, 0.5)
        time.sleep(1)
        app.open_row(1, "a")
        time.sleep(2)
        app.to_list()
        app.s.js("window.__TAURI__.event.emit('editor-state', {id: 1, path: arguments[0],"
                 " cuts: [], keyframes: [], activeKey: null,"
                 " cmBlocks: [], cmNote: '', cmFinding: null, detectedWith: {}, flatRuns: [],"
                 " markFileKind: null, chaptersDue: false, unplayed: [], playsDue: true, touched: true,"
                 " dropStreams: [], poster: null, playhead: 5.5, selA: 5.0, selB: 6.0, selGone: false});",
                 cfg.a)
        time.sleep(1)
        app.open_row(2, "c")
        time.sleep(1.5)
        p = app.save()
        e1 = p["clips"][0].get("edit") or {}
        got = cuts_of(p, 1)
        sel = (e1.get("selA"), e1.get("selB"), e1.get("selGone"))
        ok = (isinstance(sel[0], (int, float)) and isinstance(sel[1], (int, float))
              and abs(sel[0] - 3.0) < 0.15 and abs(sel[1] - 4.0) < 0.15 and not sel[2])
        t.check("34d IN and OUT set during the walk stay on their pictures", ok,
                f"selA/selB/selGone {sel} want ~3.0/~4.0/False; cuts {fmt(got)}")
        app.to_editor()
        app.escape(discard=True, expect_dialog=False)
        app.gone()


# -- 35 (rev28 mainjs) -------------------------------------------------------
def plays_cut_marks(cfg, t):
    """A row that came with a playlist's cuts (made-up play items on a.ts, as
    `plays_owed_left_behind`), the list's walk having taken them: a cut made
    by hand somewhere else in the editor puts down a mark at its own join and
    nowhere else. Every join used to become a mark at every cut, and so the
    ends of the playlist's cuts -- which make no chapter (the maintainer's
    decision, 2026-10-09; `playlistCutBefore` in app.js) -- were marks after
    the first cut by hand, and each of them a chapter in the file."""
    st = _start_time(cfg.a)
    plays = [[st + 2.0, st + 8.0], [st + 12.0, st + 18.0]]
    rows = [{"path": cfg.a, "plays": plays, "playsOwed": True}]
    with App(cfg, "plays_cut_marks", rows) as app:
        wait_for("the playlist's cuts", lambda: len(cuts_of(app.save(), 1)) >= 2, 120, 1)
        app.open_row(1, "a")
        wait_for("the walk", lambda: (lambda st: st["frames"] is not None and st["plan"]
                 and "読み込んで" not in st["plan"] and "Reading" not in st["plan"])(app.state()), 120, 0.3)
        time.sleep(1)
        p0 = app.save()
        before = (p0["clips"][0].get("edit") or {}).get("keyframes") or []
        app.to_editor()
        # Output 2..3 s: source 4..5 s, inside the first stretch played.
        app.cut(60, 89)
        time.sleep(1)
        p = app.save()
        after = (p["clips"][0].get("edit") or {}).get("keyframes") or []
        added = sorted(set(round(k, 3) for k in after) - set(round(k, 3) for k in before))
        t.check("35 a cut by hand marks its own join and not the playlist's",
                len(added) == 1 and abs(added[0] - 5.0) < 0.1
                and all(abs(k - 12.0) > 0.1 and abs(k - 2.0) > 0.1 for k in after),
                f"marks {before} -> {after} (added {added}); cuts {fmt(cuts_of(p, 1))}")
        app.to_editor()
        app.escape(discard=True)
        app.gone()


def disc_plays_trim_first(cfg, t):
    """A disc title's row that the editor reaches before the list's walk
    (lanes stopped), with a Trim line beside it that an earlier visit wrote
    (Ctrl+Shift+H) with the playlist's cuts in it. The line counts frames
    from the first picture, and past a recorder's seam the pictures are not
    on that count, so its ends come back up to half a frame off the cuts'
    (`onBounds`). Read before the walk -- before the visit puts the
    playlist's cuts down -- they were never put back on them: a cut end half
    a frame past one kept a picture fewer (three of seven on one title). The
    row's cuts must come out as the command line's. Needs
    GUITEST_PLAYS_DISC / GUITEST_PLAYS_TITLE / GUITEST_PLAYS_OUT as
    `disc_plays`. Not in the default list."""
    disc = os.environ.get("GUITEST_PLAYS_DISC")
    out = os.environ.get("GUITEST_PLAYS_OUT")
    n = int(os.environ.get("GUITEST_PLAYS_TITLE", "1"))
    if not disc or not os.path.exists(disc) or not out or not os.path.isdir(out):
        t.check("36 GUITEST_PLAYS_DISC/OUT name a disc and a folder", False, f"{disc!r} {out!r}")
        return
    said = subprocess.run([_cli(), disc, "--title", str(n), "--analyze"], text=True,
                          stdout=subprocess.PIPE, stderr=subprocess.STDOUT).stdout
    line = next((l for l in said.splitlines() if l.startswith("plays :")), "")
    want = [tuple(_hms(x) for x in p.strip().split("-")) for p in line.split("cut:", 1)[1].split(",")] \
        if "cut:" in line else []
    if not t.check("36 the command line cuts what the playlist leaves out", bool(want), line):
        return
    r = _disc_row(cfg, disc, n, "disc_plays_trim_first")
    rows = [{"path": r["path"], "name": "f", "stem": "f", "home": out, "chapters": r["chapters"],
             "plays": r["plays"], "playsOwed": True}]

    def sides():
        return sorted(f for f in os.listdir(out) if f.startswith("f.") and f.endswith((".keyframe", ".trim.avs")))

    def walked(app):
        wait_for("the walk", lambda: (lambda st: st["frames"] is not None and st["plan"]
                 and "読み込んで" not in st["plan"] and "Reading" not in st["plan"])(app.state()), 300, 0.3)
        time.sleep(2)

    for f in sides():
        os.remove(os.path.join(out, f))
    # The line, written by a visit after the list's walk.
    with App(cfg, "disc_plays_trim_first", rows, {"dir": out}) as app:
        wait_for("the row's playlist cuts", lambda: cuts_of(app.save(), 1), 600, 2)
        app.open_row(1, "f")
        walked(app)
        app.s.keys("Control+Shift+h")
        wait_for("the Trim line", lambda: sides(), 15)
        app.escape(discard=True, expect_dialog=False)
        app.gone()
    trims = sides()
    with open(os.path.join(out, trims[0]), encoding="utf-8-sig") as fh:
        body = fh.read().strip()
    # The editor first, the line read on that visit.
    with App(cfg, "disc_plays_trim_first", rows, {"dir": out}) as app:
        app.to_list()
        app.s.js("const b = document.getElementById('stop-batch'); if (b && !b.disabled) b.click();")
        time.sleep(1)
        app.open_row(1, "f")
        walked(app)
        status = app.state()["status"]
        app.ok()
        app.gone()
        got = cuts_of(app.save(), 1)
    t.check("36 a Trim line read on an editor-first visit gives the playlist's cuts",
            _near_cuts(got, want), f"{fmt(got)} want {fmt(want)}; line {body[:160]!r}; {status!r}")
    for f in sides():
        os.remove(os.path.join(out, f))


# -- 37 (rev28 p2gui) --------------------------------------------------------
def _main_title(app):
    """The list window's title bar as the window manager has it (the `*` of
    unsaved work is put there by `retitle_main`, not into the page)."""
    pid = app.gui_pid()
    for line in sh("wmctrl -lp", app.env).splitlines():
        f = line.split(None, 4)
        if len(f) == 5 and f[2] == str(pid) and "SmartCut" in f[4]:
            return f[4]
    return ""


def old_project_ok_unchanged(cfg, t):
    """A project written before the editor's edit carried `unplayed` and
    `playsDue` (0.8.22 and older: neither key in it), a row of it opened and
    left with OK without a change. Nothing was done, so the list must not
    take it for unsaved work (`*`, a question on close, a recovery copy):
    the editor always sends both keys, empty and false, and the list
    compared the edit whole."""
    with App(cfg, "old_project_ok_unchanged", [{"path": cfg.a}]) as app:
        app.open_row(1, "a")
        app.cut(60, 89)
        app.ok()
        app.gone()
        saved = app.save()
    for c in saved["clips"]:
        e = c.get("edit") or {}
        e.pop("unplayed", None)
        e.pop("playsDue", None)
    with App(cfg, "old_project_ok_unchanged", saved["clips"]) as app:
        app.to_list()
        time.sleep(2)
        first = _main_title(app)
        app.open_row(1, "a")
        time.sleep(1)
        app.ok()
        app.gone()
        time.sleep(1.5)
        after = _main_title(app)
        t.check("37 an old project's row left with OK unchanged is not unsaved work",
                bool(first) and not first.startswith("*") and not after.startswith("*"),
                f"title {first!r} -> {after!r}")
    # And a row whose `unplayed` says something (a disc title's playlist
    # cuts, made-up play items on a.ts as `plays_owed_left_behind`). Saved
    # once visited, opened again and left with OK: unchanged. (A first visit
    # of a row the list cut and nobody opened is a change of its own: it
    # reads the files beside the recording, `fresh` goes.) And saved before
    # any visit, its cuts taken back there with 取消: changed.
    st = _start_time(cfg.a)
    rows = [{"path": cfg.a, "plays": [[st + 2.0, st + 8.0], [st + 12.0, st + 18.0]], "playsOwed": True}]
    with App(cfg, "old_project_ok_unchanged", rows) as app:
        wait_for("the playlist's cuts", lambda: len(cuts_of(app.save(), 1)) >= 2, 120, 1)
        unvisited = app.save()
        app.open_row(1, "a")
        time.sleep(1)
        app.ok()
        app.gone()
        visited = app.save()
    for saved, undo in ((visited, False), (unvisited, True)):
        had = (saved["clips"][0].get("edit") or {}).get("unplayed")
        with App(cfg, "old_project_ok_unchanged", saved["clips"]) as app:
            app.to_list()
            time.sleep(2)
            first = _main_title(app)
            app.open_row(1, "a")
            time.sleep(1)
            if undo:
                app.s.keys("Control+z")
                time.sleep(1)
            app.ok()
            app.gone()
            time.sleep(1.5)
            after = _main_title(app)
            now = app.save()
            got = cuts_of(now, 1)
            e0 = saved["clips"][0].get("edit") or {}
            e1 = now["clips"][0].get("edit") or {}
            moved = {k: (e0.get(k), e1.get(k)) for k in set(e0) | set(e1)
                     if k not in ("playhead", "selA", "selB", "selGone", "touched") and e0.get(k) != e1.get(k)}
            r0, r1 = saved["clips"][0], now["clips"][0]
            moved.update({k: (r0.get(k), r1.get(k)) for k in set(r0) | set(r1)
                          if k != "edit" and r0.get(k) != r1.get(k)})
        if undo:
            t.check("37 the playlist's cuts taken back with 取消 are unsaved work",
                    bool(had) and not first.startswith("*") and after.startswith("*") and len(got) < 2,
                    f"title {first!r} -> {after!r}; unplayed {had}; cuts after {fmt(got)}")
        else:
            t.check("37 a row with the playlist's cuts left with OK unchanged is not unsaved work",
                    bool(had) and bool(first) and not first.startswith("*") and not after.startswith("*"),
                    f"title {first!r} -> {after!r}; unplayed {had}; edit moved {moved}")



def older_project_ok_unchanged(cfg, t):
    """A project as older versions of the list wrote it: an edit with only
    the keys of that time, in the order the editor of that time built it,
    and the row's keys in another order than this version builds them.
    0.8.14 to 0.8.22 (all but `unplayed`, `playsDue`), 0.8.11 to 0.8.13 (no
    `poster` either), and 0.7 (no cmFinding, detectedWith, flatRuns,
    markFileKind, chaptersDue, touched, selGone). Opened, a row left with OK
    unchanged is not unsaved work; a cut made and left with OK is. The cuts
    are ones this version put on the pictures (saved first), so that a snap
    on open is not taken for a change. Before (rev28 stage 2): the 0.7 one
    showed `*`."""
    with App(cfg, "older_project_ok_unchanged", [{"path": cfg.a}]) as app:
        app.open_row(1, "a")
        app.cut(60, 89)
        app.ok()
        app.gone()
        saved = app.save()
    e = saved["clips"][0]["edit"]
    eras = {
        "0.8.22": ["id", "path", "cuts", "keyframes", "activeKey", "cmBlocks", "cmNote", "cmFinding",
                   "detectedWith", "flatRuns", "markFileKind", "chaptersDue", "touched", "dropStreams",
                   "poster", "playhead", "selA", "selB", "selGone"],
        "0.8.11": ["id", "path", "cuts", "keyframes", "activeKey", "cmBlocks", "cmNote", "cmFinding",
                   "detectedWith", "flatRuns", "markFileKind", "chaptersDue", "touched", "dropStreams",
                   "playhead", "selA", "selB", "selGone"],
        "0.7": ["id", "path", "cuts", "keyframes", "activeKey", "cmBlocks", "cmNote", "dropStreams",
                "playhead", "selA", "selB"],
    }
    for era, keys in eras.items():
        row = dict(reversed(list(saved["clips"][0].items())))
        row["edit"] = {k: e.get(k) for k in keys}
        with App(cfg, "older_project_ok_unchanged", [row]) as app:
            app.to_list()
            time.sleep(2)
            first = _main_title(app)
            app.open_row(1, "a")
            time.sleep(1)
            app.ok()
            app.gone()
            time.sleep(1.5)
            after = _main_title(app)
            e1 = app.save()["clips"][0].get("edit") or {}
            e0 = row["edit"]
            moved = {k: (e0.get(k), e1.get(k)) for k in set(e0) | set(e1)
                     if k not in ("playhead", "selA", "selB", "selGone", "touched") and e0.get(k) != e1.get(k)}
            t.check(f"38 a {era} project's row left with OK unchanged is not unsaved work",
                    bool(first) and not first.startswith("*") and not after.startswith("*"),
                    f"title {first!r} -> {after!r}; edit moved {moved}")
            app.open_row(1, "a")
            time.sleep(1)
            app.cut(10, 20)
            app.ok()
            app.gone()
            time.sleep(1.5)
            changed = _main_title(app)
            got = cuts_of(app.save(), 1)
            t.check(f"38 ... and with a cut made, it is ({era})",
                    changed.startswith("*") and len(got) == 2, f"title {changed!r}; cuts {got}")


def emptied_field_is_unsaved(cfg, t):
    """What 出力設定 has typed about a recording, emptied: an answer ("write
    nothing there"), where nothing typed is "what the recording says" -- the
    project keeps the two apart (`captureProject`). Emptying the description,
    the channel or the date of a project saved without them puts the `*` up,
    and a save writes the empty field. And a setting turned off again after
    a save that had it on is unsaved work too. Before (rev28 stage 3, the
    shape reading every null, false and "" as absent): no `*` for any of the
    three fields, so closing the window lost them without asking."""
    with App(cfg, "emptied_field_is_unsaved", clips(cfg), {"mode": "bdav", "discTitle": "t"}) as app:
        app.to_list()
        app.s.click('button.tab[data-screen="outset"]')
        wait_for("the programme field filled", lambda: app.s.js(
            "return document.getElementById('out-programme').value"), 60)
        saved = app.save()
        time.sleep(1)
        t.check("39 saved: no unsaved work", not _main_title(app).startswith("*"), _main_title(app))
        for field, key in (("out-about", "description"), ("out-channel", "channel"), ("out-made", "made")):
            app.s.click('button.tab[data-screen="outset"]')
            app.s.click(f"#{field}")
            app.s.type("x")
            app.s.keys("Control+a", "Delete")
            time.sleep(0.6)
            title = _main_title(app)
            was = saved["clips"][0].get(key)
            saved = app.save()
            got = saved["clips"][0].get(key)
            time.sleep(0.6)
            t.check(f"39 {key} emptied is unsaved work, and saved as empty",
                    title.startswith("*") and got == "" and not _main_title(app).startswith("*"),
                    f"title {title!r}; {key} {was!r} -> {got!r}")
        app.s.click('button.tab[data-screen="outset"]')
        on = app.s.js("return document.getElementById('out-fit').checked")
        app.s.click("#out-fit")
        time.sleep(0.6)
        app.save()
        time.sleep(0.6)
        app.s.click('button.tab[data-screen="outset"]')
        app.s.click("#out-fit")
        time.sleep(0.6)
        title = _main_title(app)
        t.check("39 a setting turned back off after a save is unsaved work",
                title.startswith("*") and app.s.js("return document.getElementById('out-fit').checked") == on,
                f"title {title!r}")


def real_old_projects_ok_unchanged(cfg, t):
    """Projects written by earlier versions, as they are on disk (named in
    GUITEST_OLD_PROJECTS, separated by `:`): each opened, the first row with
    an edit opened in the editor and left with OK unchanged, is not unsaved
    work. The synthesised eras of `older_project_ok_unchanged`, on real
    files. Not in the default list (needs the files)."""
    names = [p for p in os.environ.get("GUITEST_OLD_PROJECTS", "").split(":") if p]
    if not t.check("40 GUITEST_OLD_PROJECTS names projects", bool(names), repr(names)):
        return
    for name in names:
        with open(name) as fh:
            doc = json.load(fh)
        rows = doc.get("clips") or []
        at = next((k for k, r in enumerate(rows) if r.get("edit") and os.path.exists(r.get("path", ""))), None)
        if at is None:
            t.check(f"40 {os.path.basename(name)} has a row with an edit whose recording is there", False, "")
            continue
        stem = os.path.splitext(os.path.basename(rows[at].get("name") or rows[at]["path"]))[0]
        with App(cfg, "real_old_projects_ok_unchanged", rows, doc.get("settings") or {}) as app:
            app.to_list()
            time.sleep(3)
            first = _main_title(app)
            app.open_row(at + 1, stem)
            time.sleep(2)
            app.ok()
            app.gone()
            time.sleep(1.5)
            after = _main_title(app)
            e0 = rows[at]["edit"]
            e1 = app.save()["clips"][at].get("edit") or {}
            moved = {k: (e0.get(k), e1.get(k)) for k in set(e0) | set(e1)
                     if k not in ("playhead", "selA", "selB", "selGone", "touched", "id", "path")
                     and e0.get(k) != e1.get(k)}
            t.check(f"40 {os.path.basename(name)}: a row left with OK unchanged is not unsaved work",
                    bool(first) and not first.startswith("*") and not after.startswith("*"),
                    f"title {first!r} -> {after!r}; edit moved {moved}")


# -- rev29 mainjs ------------------------------------------------------------
def disc_plays_trim_left(cfg, t):
    """A disc title's row with a Trim line beside it (written by an earlier
    visit: a cut of the hand's and the playlist's cuts), opened in the editor
    before the list's walk (lanes stopped) and left for the next row as soon
    as its counter is up -- before the editor's walk of the title has landed.
    Back on the row, the Trim line must be on it. Where the files beside a
    disc title waited for the walk (`playsDue`), a visit left before it
    landed never read them, and no later visit did either: the row came back
    without the line's cuts. "After the walk" is the same visit left once the
    walk is in, for comparison. Needs GUITEST_PLAYS_DISC / GUITEST_PLAYS_TITLE
    / GUITEST_PLAYS_OUT as `disc_plays`. Not in the default list."""
    disc = os.environ.get("GUITEST_PLAYS_DISC")
    out = os.environ.get("GUITEST_PLAYS_OUT")
    n = int(os.environ.get("GUITEST_PLAYS_TITLE", "1"))
    if not disc or not os.path.exists(disc) or not out or not os.path.isdir(out):
        t.check("m29 GUITEST_PLAYS_DISC/OUT name a disc and a folder", False, f"{disc!r} {out!r}")
        return
    r = _disc_row(cfg, disc, n, "disc_plays_trim_left")
    rows = [{"path": r["path"], "name": nm, "stem": nm, "home": out, "chapters": r["chapters"],
             "plays": r["plays"], "playsOwed": True} for nm in ("f", "g")]

    def sides():
        return sorted(f for f in os.listdir(out) if f.startswith("f.") and f.endswith((".keyframe", ".trim.avs")))

    def walked(app):
        wait_for("the walk", lambda: (lambda st: st["frames"] is not None and st["plan"]
                 and "読み込んで" not in st["plan"] and "Reading" not in st["plan"])(app.state()), 300, 0.3)
        time.sleep(2)

    for f in sides():
        os.remove(os.path.join(out, f))
    # The line: the list's walk first, then a cut by hand, written out.
    with App(cfg, "disc_plays_trim_left", rows, {"dir": out}) as app:
        wait_for("the row's playlist cuts", lambda: cuts_of(app.save(), 1), 600, 2)
        app.open_row(1, "f")
        walked(app)
        app.cut(300, 329)
        app.s.keys("Control+Shift+h")
        wait_for("the Trim line", lambda: sides(), 15)
        app.ok()
        app.gone()
        written = cuts_of(app.save(), 1)
    unplayed = {}
    for leave in ("at once", "after the walk"):
        with App(cfg, "disc_plays_trim_left", rows, {"dir": out}) as app:
            app.to_list()
            app.s.js("const b = document.getElementById('stop-batch'); if (b && !b.disabled) b.click();")
            time.sleep(1)
            app.open_row(1, "f")
            if leave == "after the walk":
                walked(app)
            app.open_row(2, "g", wait=False)
            app.editor_ready("g")
            e0 = app.save()["clips"][0].get("edit") or {}
            reached = bool(e0.get("playsDue"))
            app.open_row(1, "f")
            walked(app)
            status = app.state()["status"]
            app.ok()
            app.gone()
            p1 = app.save()
            got = cuts_of(p1, 1)
            unplayed[leave] = [(c["a"], c["b"]) for c in (p1["clips"][0].get("edit") or {}).get("unplayed") or []]
        # Within a picture rather than to the millisecond: a visit left
        # before its walk landed read the line on the outline's head, and on
        # a recorder's clip whose map starts later (`disc_head_recount`) it
        # is not read again on the walk's -- known, and not what this asks.
        t.check(f"m29 a disc row left {leave} still has its Trim line on the next visit",
                _near_cuts(got, written, 1.0 / 29.97 + 0.002),
                f"{fmt(got)} want {fmt(written)}; status {status!r}; left with {fmt([(c['a'], c['b']) for c in e0.get('cuts') or []])}"
                f"{'' if reached or leave != 'at once' else ' (NOT REACHED: the walk had landed before the row was left)'}")
    # And which of them the playlist made (no chapters at their joins): the
    # same either way.
    t.check("m29 the playlist's cuts are the playlist's whenever the row was left",
            _near_cuts(unplayed["at once"], unplayed["after the walk"], 1.0 / 29.97 + 0.002),
            f"at once {fmt(unplayed['at once'])}; after the walk {fmt(unplayed['after the walk'])}")
    for f in sides():
        os.remove(os.path.join(out, f))



# -- 30 (rev30 p2gui) -------------------------------------------------------
def reopen_keeps_selection_late_head(cfg, t):
    """IN and OUT set once the walk is in, OK, and the row opened again: a
    recording whose outline head is not its walk's first entry point (here the
    outline finds no head at all, so its timeline starts at 0 and the walk's
    20 s later; on a recorder's disc clip whose map starts late, half a
    second). 082badc moved every selection on screen onto its pictures when
    the walk landed -- the one the revisit put back too, which was measured
    on the last visit's walked timeline already -- so it went the head's
    difference the other way (`selArrived` in main.js). The second visit's
    OK must hand back the IN and OUT the first one did."""
    make_late_head(cfg)
    def landed(app):
        st = app.state()
        return st["frames"] is not None and st["plan"] and "読み込んで" not in st["plan"] \
            and "Reading" not in st["plan"]
    with App(cfg, "reopen_keeps_selection_late_head", [{"path": cfg.d}]) as app:
        app.open_row(1, "d.ts", wait=False)
        app.to_editor()
        wait_for("the walk", lambda: landed(app), 180, 0.3)
        time.sleep(1)
        app.goto(100)
        app.s.keys("i")
        app.goto(150)
        app.s.keys("o")
        time.sleep(0.5)
        app.ok()
        app.gone()
        p = app.save()
        e1 = p["clips"][0].get("edit") or {}
        sel1 = (e1.get("selA"), e1.get("selB"), e1.get("selGone"))
        app.open_row(1, "d.ts", wait=False)
        app.to_editor()
        wait_for("the walk again", lambda: landed(app), 180, 0.3)
        time.sleep(1.5)
        app.ok()
        app.gone()
        p = app.save()
        e2 = p["clips"][0].get("edit") or {}
        sel2 = (e2.get("selA"), e2.get("selB"), e2.get("selGone"))
        set1 = (isinstance(sel1[0], (int, float)) and isinstance(sel1[1], (int, float))
                and sel1[1] > sel1[0] and not sel1[2])
        t.check("30 IN/OUT set on the first visit", set1, f"selA/selB/selGone {sel1}")
        same = (set1 and isinstance(sel2[0], (int, float)) and isinstance(sel2[1], (int, float))
                and abs(sel2[0] - sel1[0]) < 1e-6 and abs(sel2[1] - sel1[1]) < 1e-6 and not sel2[2])
        t.check("30 a reopen hands back the same IN and OUT", same, f"first {sel1}, second {sel2}")


def reopen_out_during_walk_late_head(cfg, t):
    """`reopen_keeps_selection_late_head` with O pressed while the second
    visit's walk is still reading: the IN the row came back with is on the
    walked timeline already and must stay where it was; only the OUT set in
    that wait is on the outline's and is moved onto its picture. Pinned as
    one, the IN went the head's difference the wrong way (here to 0). Says
    NOT REACHED if the walk landed before any press."""
    make_late_head(cfg)
    def reading(st):
        return not (st["frames"] is not None and st["plan"] and "読み込んで" not in st["plan"]
                    and "Reading" not in st["plan"])
    with App(cfg, "reopen_out_during_walk_late_head", [{"path": cfg.d}]) as app:
        app.open_row(1, "d.ts", wait=False)
        app.to_editor()
        wait_for("the walk", lambda: not reading(app.state()), 180, 0.3)
        time.sleep(1)
        app.goto(100)
        app.s.keys("i")
        app.goto(150)
        app.s.keys("o")
        time.sleep(0.5)
        app.ok()
        app.gone()
        p = app.save()
        e1 = p["clips"][0].get("edit") or {}
        app.open_row(1, "d.ts", wait=False)
        app.to_editor()
        early = 0
        until = time.time() + 120
        while time.time() < until:
            try:
                st = app.state()
                if st["title"] and "d.ts" in st["title"]:
                    if not reading(st):
                        break
                    app.s.keys("o")
                    early += 1
            except WebDriverError:
                continue
        time.sleep(1.5)
        app.ok()
        app.gone()
        p = app.save()
        e2 = p["clips"][0].get("edit") or {}
        t.check("30b O pressed during the second walk", early > 0,
                f"{early} presses" if early else "NOT REACHED: the walk landed first")
        a1, a2 = e1.get("selA"), e2.get("selA")
        t.check("30b IN/OUT set on the first visit",
                isinstance(a1, (int, float)) and a1 > 0 and not e1.get("selGone"),
                f"selA/selB/selGone {(a1, e1.get('selB'), e1.get('selGone'))}")
        t.check("30b the IN the row came back with stays",
                isinstance(a1, (int, float)) and isinstance(a2, (int, float)) and a1 > 0 and abs(a1 - a2) < 1e-6
                and not e2.get("selGone"),
                f"first {(a1, e1.get('selB'))}, second {(a2, e2.get('selB'), e2.get('selGone'))}")



def reopen_walk_orders_late_head(cfg, t):
    """The orders `reopen_out_during_walk_late_head` does not try, on the same
    late-head recording: the row comes back with IN 100 and OUT 150 (on the
    walked timeline), and during the second walk only IN, only OUT or both
    are moved (30 pictures from the playhead the row came back with), a Del
    is made, or nothing is done; after the landing the selection must stand
    on the pictures it was set on, and 取消/やり直し must step between the
    arrived selection and the one set in the wait. Each visit is left with
    Escape, so every case starts from 100-150. Says NOT REACHED where the walk
    landed before the presses were in."""
    make_late_head(cfg)
    def reading(st):
        return not (st["frames"] is not None and st["plan"] and "読み込んで" not in st["plan"]
                    and "Reading" not in st["plan"])
    def sel(app):
        txt = app.s.js("return document.getElementById('selection').innerText")
        m = re.match(r"\D*(\d+)\s*-\s*(\d+)", txt or "")
        return (int(m.group(1)), int(m.group(2))) if m else txt
    def walking(app):
        def up():
            st = app.state()
            return st["title"] and "d.ts" in st["title"] and st["at"] is not None and reading(st)
        wait_for("the second walk", up, 60, 0.05)
    with App(cfg, "reopen_walk_orders_late_head", [{"path": cfg.d}]) as app:
        app.open_row(1, "d.ts", wait=False)
        app.to_editor()
        wait_for("the walk", lambda: not reading(app.state()), 180, 0.3)
        time.sleep(1)
        app.goto(100)
        app.s.keys("i")
        app.goto(150)
        app.s.keys("o")
        time.sleep(0.5)
        app.ok()
        app.gone()
        # On this recording the row's IN and OUT are shown 20 s early during
        # the walk (on the outline's timeline, which starts at 0), and the
        # playhead it came back with is past them: I there crosses the OUT
        # shown, and `setIn` puts that OUT on the end. An I before the OUT
        # shown is on pictures before the walk's head, so it lands on 0.
        L, R = ["ArrowLeft"] * 30, ["ArrowRight"] * 30
        early = ["Home"] + R + R
        cases = [
            ("nothing", [], (100, 150), []),
            ("IN only", early + ["i"], (0, 150), [("Control+z", (100, 150)), ("Control+y", (0, 150))]),
            ("OUT only", R + ["o"], (100, 180), [("Control+z", (100, 150)), ("Control+y", (100, 180))]),
            ("IN across OUT, then OUT", L + ["i"] + R + R + ["o"], (120, 180),
             [("Control+z", (120, 299)), ("Control+z", (100, 150)), ("Control+y", (120, 299))]),
            ("OUT then IN", R + ["o"] + L + L + ["i"], (120, 180),
             [("Control+z", (100, 180)), ("Control+z", (100, 150))]),
            ("IN across OUT, then Del", L + ["i", "Delete"], None,
             [("Control+z", (120, 299)), ("Control+z", (100, 150))]),
        ]
        for name, during, want, steps in cases:
            app.open_row(1, "d.ts", wait=False)
            app.to_editor()
            walking(app)
            # In one tick, so the walk cannot land halfway through: each key
            # its own press (not a held repeat), as a hand gives them.
            app.s.js("for (const k of arguments[0]) for (const ty of ['keydown', 'keyup'])"
                     " document.body.dispatchEvent(new KeyboardEvent(ty, {key: k, bubbles: true}));",
                     during)
            still = reading(app.state())
            wait_for("the walk", lambda: not reading(app.state()), 180, 0.3)
            time.sleep(1.5)
            got = sel(app)
            t.check(f"31 {name}: pressed during the walk", still,
                    "" if still else "NOT REACHED: the walk landed first")
            if not still:
                app.escape(discard=True, expect_dialog=False)
                app.gone()
                continue
            if want is not None:
                t.check(f"31 {name}: selection after the landing", got == want, f"{got} (want {want})")
            framed = [app.state()["frames"]]
            for key, w in steps:
                app.s.keys(key)
                time.sleep(0.7)
                g = sel(app)
                framed.append(app.state()["frames"])
                t.check(f"31 {name}: {key} gives {w}", g == w, f"{g}")
            if want is None:
                t.check(f"31 {name}: the Del took pictures 120 to the end and 取消 gives them back",
                        framed[:2] == [120, 300], f"frames {framed}, selection after {got}")
            app.escape(discard=True, expect_dialog=False)
            app.gone()


def make_no_length(cfg):
    """A .mkv written down a pipe: libavformat cannot seek back to fill in its
    Duration, so the container states no length (ffprobe says N/A), as a
    browser's MediaRecorder .webm, an .mkv cut short by a crash, or a
    recorder's clip whose clock restarts (libav declines to measure those).
    The outline's length is then 0 and only the walk finds it."""
    at = os.path.join(cfg.work, "nolength")
    os.makedirs(at, exist_ok=True)
    path = os.path.join(at, "e.mkv")
    if not os.path.exists(path):
        with open(path + ".part", "wb") as out:
            subprocess.run(["ffmpeg", "-nostdin", "-y", "-loglevel", "error",
                            "-f", "lavfi", "-i", "testsrc2=size=640x360:rate=30000/1001:duration=20",
                            "-f", "lavfi", "-i", "sine=frequency=660:duration=20:sample_rate=48000",
                            "-c:v", "libx264", "-g", "30", "-preset", "veryfast", "-pix_fmt", "yuv420p",
                            "-c:a", "aac", "-b:a", "128k", "-f", "matroska", "-"],
                           stdout=out, check=True)
        os.replace(path + ".part", path)
    return path


def reopen_no_container_length(cfg, t):
    """A cut in the middle of a recording whose container states no length,
    OK, and the row opened again: the second visit's outline says 0 s, and
    `pointsArrived` took every cut ending at or past the outline's end for a
    cut to the end -- so the cut 150-300 ran to the last picture once the
    walk landed, and the output lost everything after picture 150. The row
    must come back with the cut it left with."""
    path = make_no_length(cfg)
    def landed(app):
        st = app.state()
        return st["frames"] is not None and st["frames"] > 0 and st["plan"] \
            and "読み込んで" not in st["plan"] and "Reading" not in st["plan"]
    with App(cfg, "reopen_no_container_length", [{"path": path}]) as app:
        app.open_row(1, "e.mkv", wait=False)
        app.to_editor()
        wait_for("the walk", lambda: landed(app), 180, 0.3)
        time.sleep(1)
        left = app.cut(150, 300)
        app.ok()
        app.gone()
        c1 = cuts_of(app.save(), 1)
        app.open_row(1, "e.mkv", wait=False)
        app.to_editor()
        wait_for("the walk again", lambda: landed(app), 180, 0.3)
        time.sleep(1.5)
        st = app.state()
        app.ok()
        app.gone()
        c2 = cuts_of(app.save(), 1)
        same = len(c1) == 1 and len(c2) == 1 and all(abs(x - y) < 1e-6 for x, y in zip(c1[0], c2[0]))
        t.check("33 a reopen keeps a mid-recording cut (no container length)",
                same and st["frames"] == left,
                f"first {fmt(c1)}, second {fmt(c2)}; reopened with {st['frames']} pictures (want {left})")


def first_visit_marks_no_container_length(cfg, t):
    """A .keyframe beside a recording whose container states no length, read
    on a first visit: the outline's timeline is 0 s long, so read before the
    walk every mark was past its end and dropped, and the files were not
    read again once the walk landed. The row must come up with the marks."""
    path = make_no_length(cfg)
    side = os.path.splitext(path)[0] + ".keyframe"
    if os.path.exists(side):
        os.remove(side)
    def landed(app):
        st = app.state()
        return st["frames"] is not None and st["frames"] > 0 and st["plan"] \
            and "読み込んで" not in st["plan"] and "Reading" not in st["plan"]
    with App(cfg, "first_visit_marks_no_container_length_1", [{"path": path}]) as app:
        app.open_row(1, "e.mkv", wait=False)
        app.to_editor()
        wait_for("the walk", lambda: landed(app), 180, 0.3)
        time.sleep(1)
        for at in (100, 300, 450):
            app.goto(at)
            app.s.keys("k")
        time.sleep(0.3)
        app.s.keys("Control+h")
        wait_for("the .keyframe beside the recording", lambda: os.path.exists(side), 10)
        time.sleep(0.3)
        app.escape(discard=True, expect_dialog=False)
        app.gone()
    with App(cfg, "first_visit_marks_no_container_length_2", [{"path": path}]) as app:
        app.open_row(1, "e.mkv", wait=False)
        app.to_editor()
        wait_for("the walk", lambda: landed(app), 180, 0.3)
        time.sleep(1.5)
        st = app.state()
        t.check("34 a first visit reads the .keyframe (no container length)",
                st["keys"].startswith("3"), f"marks shown: {st['keys']!r}; status {st['status']!r}")
        app.escape(discard=True, expect_dialog=False)
        app.gone()
    os.remove(side)


def reopen_selection_no_container_length(cfg, t):
    """IN, OUT and the playhead set once the walk is in, OK, and the row
    opened again, on a recording whose container states no length: the
    second visit's outline is 0 s, the saved IN/OUT and playhead were clamped
    onto it, and nothing put them back once the walk landed."""
    path = make_no_length(cfg)
    def landed(app):
        st = app.state()
        return st["frames"] is not None and st["frames"] > 0 and st["plan"] \
            and "読み込んで" not in st["plan"] and "Reading" not in st["plan"]
    def sel(app):
        return app.s.js("return document.getElementById('selection').innerText")
    with App(cfg, "reopen_selection_no_container_length", [{"path": path}]) as app:
        app.open_row(1, "e.mkv", wait=False)
        app.to_editor()
        wait_for("the walk", lambda: landed(app), 180, 0.3)
        time.sleep(1)
        app.goto(100)
        app.s.keys("i")
        app.goto(200)
        app.s.keys("o")
        app.goto(250)
        time.sleep(0.5)
        s1 = sel(app)
        app.ok()
        app.gone()
        e1 = app.save()["clips"][0].get("edit") or {}
        app.open_row(1, "e.mkv", wait=False)
        app.to_editor()
        wait_for("the walk again", lambda: landed(app), 180, 0.3)
        time.sleep(1.5)
        s2 = sel(app)
        at2 = app.state()["at"]
        app.ok()
        app.gone()
        e2 = app.save()["clips"][0].get("edit") or {}
        k = ("selA", "selB", "selGone", "playhead")
        v1 = tuple(e1.get(x) for x in k)
        v2 = tuple(e2.get(x) for x in k)
        t.check("p2gui-1 IN/OUT set on the first visit",
                isinstance(v1[0], (int, float)) and isinstance(v1[1], (int, float)) and v1[1] > v1[0] and not v1[2],
                f"{v1}")
        same = all(isinstance(a, (int, float)) and isinstance(b, (int, float)) and abs(a - b) < 1e-6
                   for a, b in zip(v1[:2] + v1[3:], v2[:2] + v2[3:])) and not v2[2]
        t.check("p2gui-1 a reopen keeps IN/OUT and the playhead (no container length)", same and s1 == s2 and at2 == 250,
                f"first {v1} '{s1}', second {v2} '{s2}' at {at2}")


def ok_during_walk_selection_no_container_length(cfg, t):
    """The same, with OK pressed on the second visit as soon as the title is
    up (during the walk where the machine is slow enough): what the list gets
    must still be the IN/OUT the row came with. Says NOT REACHED if the walk
    landed first."""
    path = make_no_length(cfg)
    def landed(app):
        st = app.state()
        return st["frames"] is not None and st["frames"] > 0 and st["plan"] \
            and "読み込んで" not in st["plan"] and "Reading" not in st["plan"]
    with App(cfg, "ok_during_walk_selection_no_container_length", [{"path": path}]) as app:
        app.open_row(1, "e.mkv", wait=False)
        app.to_editor()
        wait_for("the walk", lambda: landed(app), 180, 0.3)
        time.sleep(1)
        app.goto(100)
        app.s.keys("i")
        app.goto(200)
        app.s.keys("o")
        time.sleep(0.5)
        app.ok()
        app.gone()
        e1 = app.save()["clips"][0].get("edit") or {}
        app.open_row(1, "e.mkv", wait=False)
        app.to_editor()
        wait_for("the title", lambda: "e.mkv" in (app.state()["title"] or ""), 60, 0.05)
        early = not landed(app)
        time.sleep(0.3)
        app.ok()
        app.gone()
        e2 = app.save()["clips"][0].get("edit") or {}
        t.check("p2gui-2 OK pressed during the walk", early, "" if early else "NOT REACHED: the walk landed first")
        v1 = (e1.get("selA"), e1.get("selB"), e1.get("selGone"))
        v2 = (e2.get("selA"), e2.get("selB"), e2.get("selGone"))
        t.check("p2gui-2 OK during the walk keeps IN/OUT (no container length)",
                all(isinstance(x, (int, float)) for x in v1[:2] + v2[:2])
                and abs(v1[0] - v2[0]) < 1e-6 and abs(v1[1] - v2[1]) < 1e-6 and not v2[2],
                f"first {v1}, second {v2}")


def held_selection_no_container_length(cfg, t):
    """The IN/OUT and playhead a row whose container states no length comes
    back with (`heldSel` in main.js), through what else can happen to it:
    another row opened during the second walk (the edit the list takes on
    the way out must be the one the row came with), a project reloaded (a
    new process, so the walk is read again), a Del once the walk is in (it
    takes the IN-OUT the row came with), and 取消 (the cut goes, the
    selection comes back)."""
    path = make_no_length(cfg)
    def landed(app):
        st = app.state()
        return st["frames"] is not None and st["frames"] > 0 and st["plan"] \
            and "読み込んで" not in st["plan"] and "Reading" not in st["plan"]
    def sel(app):
        return app.s.js("return document.getElementById('selection').innerText")
    k = ("selA", "selB", "selGone", "playhead")
    def same(e1, e2):
        v1 = tuple(e1.get(x) for x in k)
        v2 = tuple(e2.get(x) for x in k)
        nums = all(isinstance(v1[i], (int, float)) and isinstance(v2[i], (int, float))
                   and abs(v1[i] - v2[i]) < 1e-6 for i in (0, 1, 3))
        return nums and v1[2] is False and v2[2] is False, f"{v1} vs {v2}"
    with App(cfg, "held_selection_no_container_length", [{"path": path}, {"path": cfg.a}]) as app:
        app.open_row(1, "e.mkv", wait=False)
        app.to_editor()
        wait_for("the walk", lambda: landed(app), 180, 0.3)
        time.sleep(1)
        app.goto(100)
        app.s.keys("i")
        app.goto(200)
        app.s.keys("o")
        app.goto(250)
        time.sleep(0.5)
        s1 = sel(app)
        frames = app.state()["frames"]
        app.ok()
        app.gone()
        e1 = app.save()["clips"][0].get("edit") or {}
        app.open_row(1, "e.mkv", wait=False)
        app.to_editor()
        wait_for("the title", lambda: "e.mkv" in (app.state()["title"] or ""), 60, 0.05)
        early = not landed(app)
        app.open_row(2, "a.ts", wait=False)
        app.editor_ready("a.ts")
        time.sleep(1.0)
        app.ok()
        app.gone()
        saved = app.save()
        e2 = saved["clips"][0].get("edit") or {}
        t.check("p3gui-1 another row opened during the walk", early, "" if early else "NOT REACHED: the walk landed first")
        ok, said = same(e1, e2)
        t.check("p3gui-1 a row left during the walk keeps IN/OUT and the playhead (no container length)", ok, said)
    with App(cfg, "held_selection_no_container_length_2", saved["clips"]) as app:
        app.open_row(1, "e.mkv", wait=False)
        app.to_editor()
        wait_for("the walk", lambda: landed(app), 180, 0.3)
        time.sleep(1.5)
        s2 = sel(app)
        at2 = app.state()["at"]
        t.check("p3gui-1 a reloaded project's row comes back with IN/OUT and the playhead",
                s2 == s1 and at2 == 250, f"'{s1}' -> '{s2}' at {at2}")
        app.s.keys("Delete")
        want = frames - 101
        try:
            wait_for("the Del", lambda: app.state()["frames"] == want, 10)
        except Exception:  # noqa: BLE001 -- said below
            pass
        after_del = app.state()["frames"]
        app.s.keys("Control+z")
        try:
            wait_for("the 取消", lambda: app.state()["frames"] == frames, 10)
        except Exception:  # noqa: BLE001 -- said below
            pass
        time.sleep(0.5)
        s3 = sel(app)
        undone = app.state()["frames"]
        app.ok()
        app.gone()
        e3 = app.save()["clips"][0].get("edit") or {}
        t.check("p3gui-1 Del takes the IN-OUT the row came with, 取消 gives it back",
                after_del == want and undone == frames and s3 == s1 and not e3.get("cuts"),
                f"pictures {frames} -> {after_del} (want {want}) -> {undone}; selection '{s3}' (want '{s1}'); "
                f"cuts {e3.get('cuts')}")


def save_marks_during_walk_no_container_length(cfg, t):
    """Ctrl+H and Ctrl+Shift+H on a second visit to a recording whose
    container states no length, before its walk has landed: the outline's
    timeline is 0 s long, so every mark the row came back with and every
    range it keeps lay past its end -- the .keyframe was written empty and
    the Trim line kept nothing, over whatever was beside the recording.
    They must be refused until the walk is in, or written whole. Says NOT
    REACHED if the walk landed first."""
    path = make_no_length(cfg)
    stem = os.path.splitext(path)[0]
    sides = (stem + ".keyframe", path + ".trim.avs")
    def landed(app):
        st = app.state()
        return st["frames"] is not None and st["frames"] > 0 and st["plan"] \
            and "読み込んで" not in st["plan"] and "Reading" not in st["plan"]
    def body(p):
        try:
            with open(p) as f:
                return f.read()
        except FileNotFoundError:
            return None
    for p in sides:
        if os.path.exists(p):
            os.remove(p)
    with App(cfg, "save_marks_during_walk_no_container_length", [{"path": path}]) as app:
        app.open_row(1, "e.mkv", wait=False)
        app.to_editor()
        wait_for("the walk", lambda: landed(app), 180, 0.3)
        time.sleep(1)
        left = app.cut(150, 300)
        for at in (100, 200, 400):
            app.goto(at)
            app.s.keys("k")
        time.sleep(0.3)
        app.s.keys("Control+h")
        wait_for("the .keyframe", lambda: body(sides[0]), 10)
        app.s.keys("Control+Shift+h")
        wait_for("the Trim line", lambda: body(sides[1]), 10)
        time.sleep(0.3)
        want = [body(p) for p in sides]
        app.ok()
        app.gone()
        for p in sides:
            os.remove(p)
        app.open_row(1, "e.mkv", wait=False)
        app.to_editor()
        # Pressed over and over from the moment the window is up: refused
        # (nothing written) while no row is in, so the first file to appear
        # is the first press the window took.
        during = [None, None]
        early = False
        said = False
        until = time.time() + 30
        while time.time() < until and during[0] is None:
            early = early or not landed(app)
            app.s.keys("Control+h")
            during[0] = body(sides[0])
            st = app.state()
            # Its own words, not those of the files a first visit owes.
            said = said or (st["status"] or "") in (
                "解析が終わるまで保存できません", "Not saved: wait for the analysis to finish")
        while time.time() < until and during[1] is None:
            app.s.keys("Control+Shift+h")
            during[1] = body(sides[1])
        status = app.state()["status"]
        wait_for("the walk again", lambda: landed(app), 180, 0.3)
        time.sleep(1)
        st = app.state()
        app.escape(discard=True, expect_dialog=False)
        app.gone()
    for p in sides:
        if os.path.exists(p):
            os.remove(p)
    t.check("p4gui-1 Ctrl+H pressed during the walk", early, "" if early else "NOT REACHED: the walk landed first")
    ok = all(d is None or d == w for d, w in zip(during, want))
    t.check("p4gui-1 marks written during the walk are the row's (no container length)",
            ok and st["frames"] == left,
            f"want {want!r}, written {during!r}; status {status!r}; {st['frames']} pictures (want {left})")
    t.check("p4gui-1 the refusal says to wait for the analysis", said or not early,
            "said" if said else "no refusal seen on the status line")


def save_marks_during_walk_late_head(cfg, t):
    """`save_marks_during_walk_no_container_length` on a recording whose
    head the outline could not find (`make_late_head`): on a second visit,
    before the walk lands, the timeline's frame 0 is the clock's zero, some
    twenty seconds before the first picture, so Ctrl+H wrote every mark and
    every Trim end that many frames late over the files beside the
    recording. They must be refused until the walk is in, or written as
    the walked timeline writes them. Says NOT REACHED if the walk landed
    first."""
    make_late_head(cfg)
    path = cfg.d
    stem = os.path.splitext(path)[0]
    sides = (stem + ".keyframe", path + ".trim.avs")
    def landed(app):
        st = app.state()
        return st["frames"] is not None and st["frames"] > 0 and st["plan"] \
            and "読み込んで" not in st["plan"] and "Reading" not in st["plan"]
    def body(p):
        try:
            with open(p) as f:
                return f.read()
        except FileNotFoundError:
            return None
    for p in sides:
        if os.path.exists(p):
            os.remove(p)
    with App(cfg, "save_marks_during_walk_late_head", [{"path": path}]) as app:
        app.open_row(1, "d.ts", wait=False)
        app.to_editor()
        wait_for("the walk", lambda: landed(app), 180, 0.3)
        time.sleep(1)
        left = app.cut(100, 150)
        for at in (50, 120, 200):
            app.goto(at)
            app.s.keys("k")
        time.sleep(0.3)
        app.s.keys("Control+h")
        wait_for("the .keyframe", lambda: body(sides[0]), 10)
        app.s.keys("Control+Shift+h")
        wait_for("the Trim line", lambda: body(sides[1]), 10)
        time.sleep(0.3)
        want = [body(p) for p in sides]
        app.ok()
        app.gone()
        for p in sides:
            os.remove(p)
        app.open_row(1, "d.ts", wait=False)
        app.to_editor()
        during = [None, None]
        early = False
        said = False
        until = time.time() + 60
        while time.time() < until and during[0] is None:
            try:
                early = early or not landed(app)
                app.s.keys("Control+h")
                during[0] = body(sides[0])
                st = app.state()
            except WebDriverError:
                continue
            said = said or (st["status"] or "") in (
                "解析が終わるまで保存できません", "Not saved: wait for the analysis to finish")
        while time.time() < until and during[1] is None:
            try:
                app.s.keys("Control+Shift+h")
            except WebDriverError:
                continue
            during[1] = body(sides[1])
        status = app.state()["status"]
        wait_for("the walk again", lambda: landed(app), 180, 0.3)
        time.sleep(1)
        st = app.state()
        app.escape(discard=True, expect_dialog=False)
        app.gone()
    for p in sides:
        if os.path.exists(p):
            os.remove(p)
    t.check("34m-1 Ctrl+H pressed during the walk", early, "" if early else "NOT REACHED: the walk landed first")
    ok = all(d is None or d == w for d, w in zip(during, want))
    t.check("34m-1 marks written during the walk are the row's (head unknown)",
            ok and st["frames"] == left,
            f"want {want!r}, written {during!r}; status {status!r}; {st['frames']} pictures (want {left})")
    t.check("34m-1 the refusal says to wait for the analysis", said or not early,
            "said" if said else "no refusal seen on the status line")


def outline_fails_ctrl_h(cfg, t):
    """Row 2 visited once (its marks read), row 1 opened, then row 2's
    recording replaced by something that is not one (the list only asks
    whether the file is there) and row 2 double-clicked: the open fails and says so, and the window still holds
    row 1's recording and marks. Ctrl+H then must not write row 1's marks
    into row 2's .keyframe (the name beside the recording is set before the
    open, and was left on the row that failed)."""
    e = os.path.join(cfg.media, "e.ts")
    shutil.copyfile(cfg.a, e)
    a_marks, e_marks = [400, 500], [50, 100]
    a_file = os.path.join(cfg.media, "a.keyframe")
    e_file = os.path.join(cfg.media, "e.keyframe")

    def put(path, frames):
        with open(path, "w", newline="") as f:
            f.write("".join(f"{n}\n" for n in frames))

    def read(path):
        with open(path, "rb") as f:
            body = f.read()
        return [int(x) for x in body.split()], b"\r\n" in body

    put(a_file, a_marks)
    put(e_file, e_marks)
    try:
        with App(cfg, "outline_fails_ctrl_h", [{"path": cfg.a}, {"path": e}]) as app:
            app.open_row(2, "e.ts")
            wait_for("row 2's marks", lambda: app.state()["keys"].startswith("2"), 30)
            app.open_row(1, "a.ts")
            wait_for("row 1's marks", lambda: app.state()["keys"].startswith("2"), 30)
            time.sleep(1)
            # Still there, so the list's `clip_gone` lets the press through,
            # but no longer a recording: the open itself fails.
            with open(e, "wb") as f:
                f.write(b"\0" * (1 << 20))
            app.open_row(2, "e.ts", wait=False)
            app.to_editor()
            failed = wait_for("the open to fail", lambda: (lambda s: s if (
                "開けません" in s or "annot open" in s) else None)(app.state()["status"] or ""), 60)
            app.s.keys("Control+h")
            asked = app.answer(True, timeout=3)
            time.sleep(1)
            st = app.state()
            got_e, wrote_e = read(e_file)
            t.check("36m-1 the open of the missing recording failed", True, repr(failed))
            t.check("36m-1 Ctrl+H after it leaves row 2's .keyframe",
                    got_e == e_marks and not wrote_e,
                    f"e.keyframe {got_e} (want {e_marks}), {'rewritten' if wrote_e else 'untouched'}; "
                    f"asked {asked}; status {st['status']!r}")
    finally:
        for p in (e, a_file, e_file):
            if os.path.exists(p):
                os.remove(p)


def outline_fails_first_visit(cfg, t):
    """Row 1 up with its marks, then row 2 -- never visited, its recording
    replaced by something that is not one -- double-clicked: the open fails
    and the window still holds row 1. Ctrl+H then answers for row 1, the
    recording on screen: its marks beside it, nothing beside row 2. (The
    failed first visit left `marksDue` set, so Ctrl+H was refused on row 1
    as 「まだ読み込んでいない」.)"""
    e = os.path.join(cfg.media, "e.ts")
    shutil.copyfile(cfg.a, e)
    a_file = os.path.join(cfg.media, "a.keyframe")
    e_file = os.path.join(cfg.media, "e.keyframe")
    with open(a_file, "w", newline="") as f:
        f.write("400\n500\n")
    try:
        with App(cfg, "outline_fails_first_visit", [{"path": cfg.a}, {"path": e}]) as app:
            app.open_row(1, "a.ts")
            wait_for("row 1's marks", lambda: app.state()["keys"].startswith("2"), 30)
            time.sleep(1)
            with open(e, "wb") as f:
                f.write(b"\0" * (1 << 20))
            app.open_row(2, "e.ts", wait=False)
            app.to_editor()
            failed = wait_for("the open to fail", lambda: (lambda s: s if (
                "開けません" in s or "annot open" in s) else None)(app.state()["status"] or ""), 60)
            t.check("36g-1 the first open of the broken recording failed", True, repr(failed))
            app.s.keys("Control+h")
            asked = app.answer(True, timeout=3)
            time.sleep(1)
            st = app.state()
            with open(a_file, "rb") as f:
                got_a = [int(x) for x in f.read().split()]
            t.check("36g-2 Ctrl+H after it writes row 1's marks beside row 1",
                    not os.path.exists(e_file) and got_a == [400, 500] and "a.keyframe" in st["status"],
                    f"e.keyframe {'written' if os.path.exists(e_file) else 'absent'}; a.keyframe {got_a}; "
                    f"asked {asked}; status {st['status']!r}")
    finally:
        for p in (e, a_file, e_file):
            if os.path.exists(p):
                os.remove(p)


def outline_fails_after_success(cfg, t):
    """Row 1 up, row 2 (not a recording) fails, row 3 opens and comes up,
    then row 2 fails again: the window holds row 3, and Ctrl+H writes row 3's
    marks beside row 3 -- not beside row 1, whose snapshot the first failure
    put back (`outlineLeft` must be cleared once an open has come up)."""
    e = os.path.join(cfg.media, "e.ts")
    f3 = os.path.join(cfg.media, "f.ts")
    shutil.copyfile(cfg.a, f3)
    shutil.copyfile(cfg.a, e)
    a_file = os.path.join(cfg.media, "a.keyframe")
    e_file = os.path.join(cfg.media, "e.keyframe")
    f_file = os.path.join(cfg.media, "f.keyframe")
    for path, frames in ((a_file, [400, 500]), (f_file, [70, 80, 90])):
        with open(path, "w", newline="") as f:
            f.write("".join(f"{n}\n" for n in frames))

    def ints(path):
        with open(path, "rb") as f:
            return [int(x) for x in f.read().split()]

    def fail_row2(app):
        # So the first failure's line is not taken for the second's.
        app.s.js("document.getElementById('status').innerText = ''; return 0")
        app.open_row(2, "e.ts", wait=False)
        app.to_editor()
        return wait_for("the open to fail", lambda: (lambda s: s if (
            "開けません" in s or "annot open" in s) else None)(app.state()["status"] or ""), 60)

    try:
        with App(cfg, "outline_fails_after_success",
                 [{"path": cfg.a}, {"path": e}, {"path": f3}]) as app:
            app.open_row(1, "a.ts")
            wait_for("row 1's marks", lambda: app.state()["keys"].startswith("2"), 30)
            time.sleep(1)
            # Broken only now: a row the list could not read is not sent.
            with open(e, "wb") as f:
                f.write(b"\0" * (1 << 20))
            fail_row2(app)
            app.open_row(3, "f.ts")
            wait_for("row 3's marks", lambda: app.state()["keys"].startswith("3"), 30)
            time.sleep(1)
            failed = fail_row2(app)
            t.check("36p-1 the second open of the broken recording failed", True, repr(failed))
            app.s.keys("Control+h")
            asked = app.answer(True, timeout=3)
            time.sleep(1)
            st = app.state()
            got_a, got_f = ints(a_file), ints(f_file)
            t.check("36p-2 Ctrl+H after it answers for row 3 beside row 3",
                    not os.path.exists(e_file) and got_a == [400, 500] and got_f == [70, 80, 90]
                    and "f.keyframe" in st["status"],
                    f"e.keyframe {'written' if os.path.exists(e_file) else 'absent'}; a.keyframe {got_a}; "
                    f"f.keyframe {got_f}; asked {asked}; status {st['status']!r}")
    finally:
        for p in (e, f3, a_file, e_file, f_file):
            if os.path.exists(p):
                os.remove(p)


def outline_fails_same_recording(cfg, t):
    """Two rows on one recording, the second renamed (its marks beside it as
    `xcopy.keyframe`). Row 2 visited, row 1 opened, then the recording made
    unreadable and row 2 double-clicked: the open fails and the window still
    holds row 1. Ctrl+H must answer for row 1 beside row 1 -- not write row
    1's marks into row 2's `xcopy.keyframe`. (The failed open put the shown
    recording's name back only where the paths differed.)"""
    e = os.path.join(cfg.media, "e.ts")
    shutil.copyfile(cfg.a, e)
    e_file = os.path.join(cfg.media, "e.keyframe")
    x_file = os.path.join(cfg.media, "xcopy.keyframe")
    for path, frames in ((e_file, [400, 500]), (x_file, [50, 100])):
        with open(path, "w", newline="") as f:
            f.write("".join(f"{n}\n" for n in frames))

    def read(path):
        with open(path, "rb") as f:
            body = f.read()
        return [int(x) for x in body.split()], b"\r\n" in body

    try:
        with App(cfg, "outline_fails_same_recording",
                 [{"path": e, "name": "erow", "stem": "e"},
                  {"path": e, "name": "xcopy", "stem": "xcopy"}]) as app:
            app.open_row(2, "xcopy")
            wait_for("row 2's marks", lambda: app.state()["keys"].startswith("2"), 30)
            app.open_row(1, "erow")
            wait_for("row 1's marks", lambda: app.state()["keys"].startswith("2"), 30)
            time.sleep(1)
            with open(e, "wb") as f:
                f.write(b"\0" * (1 << 20))
            app.open_row(2, "xcopy", wait=False)
            app.to_editor()
            failed = wait_for("the open to fail", lambda: (lambda s: s if (
                "開けません" in s or "annot open" in s) else None)(app.state()["status"] or ""), 60)
            t.check("37m-1 the open of the broken recording failed", True, repr(failed))
            app.s.keys("Control+h")
            asked = app.answer(True, timeout=3)
            time.sleep(1)
            st = app.state()
            got_x, wrote_x = read(x_file)
            t.check("37m-2 Ctrl+H after it leaves row 2's xcopy.keyframe",
                    got_x == [50, 100] and not wrote_x and "xcopy" not in (st["status"] or ""),
                    f"xcopy.keyframe {got_x} (want [50, 100]), {'rewritten' if wrote_x else 'untouched'}; "
                    f"asked {asked}; status {st['status']!r}")
    finally:
        for p in (e, e_file, x_file):
            if os.path.exists(p):
                os.remove(p)


def outline_fails_edits_kept(cfg, t):
    """Row 1 up, row 2 (its recording made unreadable) double-clicked: the
    open fails and the window goes on showing row 1. A cut made there after
    the failure, then OK, must be on row 1 -- the window had let go of its
    row (`editId` null), so every edit after the failure was sent under no
    row and OK closed with it gone. Then the same again with Escape and
    破棄: row 1 goes back to what it was before that visit, row 2 is
    untouched both times."""
    e = os.path.join(cfg.media, "e.ts")
    shutil.copyfile(cfg.a, e)

    def fail_row2(app):
        app.s.js("document.getElementById('status').innerText = ''; return 0")
        app.open_row(2, "e.ts", wait=False)
        app.to_editor()
        return wait_for("the open to fail", lambda: (lambda s: s if (
            "開けません" in s or "annot open" in s) else None)(app.state()["status"] or ""), 60)

    try:
        with App(cfg, "outline_fails_edits_kept", [{"path": cfg.a}, {"path": e}]) as app:
            app.open_row(1, "a.ts")
            time.sleep(1)
            with open(e, "wb") as f:
                f.write(b"\0" * (1 << 20))
            failed = fail_row2(app)
            t.check("37g-1 the open of the broken recording failed", True, repr(failed))
            left = app.cut(100, 150)
            time.sleep(0.5)
            app.ok()
            app.gone()
            p = app.save()
            c1, c2 = cuts_of(p, 1), cuts_of(p, 2)
            t.check("37g-2 a cut made after the failed open is kept on row 1 by OK",
                    len(c1) == 1 and c2 == [],
                    f"row 1 cuts {fmt(c1)}, row 2 cuts {fmt(c2)} ({left} pictures shown)")
            app.open_row(1, "a.ts")
            wait_for("row 1 with its cut", lambda: app.state()["frames"] == left, 30)
            time.sleep(1)
            fail_row2(app)
            app.cut(300, 350)
            time.sleep(0.5)
            app.escape(discard=True)
            app.gone()
            p = app.save()
            d1, d2 = cuts_of(p, 1), cuts_of(p, 2)
            t.check("37g-3 Escape+破棄 after the failed open puts row 1 back as the visit found it",
                    d1 == c1 and d2 == [],
                    f"row 1 cuts {fmt(d1)} (want {fmt(c1)}), row 2 cuts {fmt(d2)}")
    finally:
        if os.path.exists(e):
            os.remove(e)


# -- 37th pass, stage 3 (p3gui): the window after a failed open ---------------
def _break(path):
    with open(path, "wb") as f:
        f.write(b"\0" * (1 << 20))


def _fail_row(app, n, name):
    """Double-click row `n`, whose recording is no longer one, and wait for
    the editor to say the open failed. Returns the status line."""
    app.to_editor()
    app.s.js("document.getElementById('status').innerText = ''; return 0")
    app.open_row(n, name, wait=False)
    app.to_editor()
    return wait_for("the open to fail", lambda: (lambda s: s if (
        "開けません" in s or "annot open" in s) else None)(app.state()["status"] or ""), 60)


def _editing_rows(app):
    """Which rows of the list wear 編集中 (1-based)."""
    app.to_list()
    flags = app.s.js("return [...document.querySelectorAll('#cliplist > li')]"
                     ".map((li) => li.querySelector('.badge').classList.contains('editing'))")
    return [i + 1 for i, f in enumerate(flags) if f]


def _editor_os_title(app):
    """The editor's title bar, as the window manager has it."""
    pid = app.gui_pid()
    for line in sh("wmctrl -lp", app.env).splitlines():
        f = line.split(None, 4)
        if len(f) == 5 and f[2] == str(pid) and ("カット編集" in f[4] or "Cut editor" in f[4]):
            return f[4]
    return None


def outline_fails_shows_row(cfg, t):
    """Row 1 up, row 2's recording broken, row 2 double-clicked: the open
    fails and the window goes on as row 1. The list then has row 1 as the
    one being edited, the title bar names row 1 (it was retitled for row 2
    on the press), and the header says row 1's recording rather than being
    left blank. Then Ctrl+Z after the failure undoes row 1's own cut, and
    OK hands that over: row 1 ends with no cut."""
    e = os.path.join(cfg.media, "e.ts")
    shutil.copyfile(cfg.a, e)
    try:
        with App(cfg, "outline_fails_shows_row", [{"path": cfg.a}, {"path": e}]) as app:
            app.open_row(1, "a.ts")
            time.sleep(1)
            app.cut(100, 150)
            time.sleep(0.5)
            _break(e)
            _fail_row(app, 2, "e.ts")
            time.sleep(1)
            rows = _editing_rows(app)
            os_title = _editor_os_title(app)
            app.to_editor()
            st = app.state()
            t.check("37q-1 after the failed open the list edits row 1", rows == [1], f"編集中 on {rows}")
            t.check("37q-2 the title bar names row 1", bool(os_title) and "a" in os_title and "e" not in
                    os_title.split("—")[-1], f"title bar {os_title!r}")
            t.check("37q-3 the header names row 1's recording", "a.ts" in st["title"], f"header {st['title']!r}")
            app.s.keys("Control+z")
            wait_for("the cut undone", lambda: app.state()["frames"] == FRAMES, 10)
            time.sleep(0.5)
            app.ok()
            app.gone()
            p = app.save()
            c1, c2 = cuts_of(p, 1), cuts_of(p, 2)
            t.check("37q-4 Ctrl+Z after the failed open, then OK: row 1 has no cut",
                    c1 == [] and c2 == [], f"row 1 cuts {fmt(c1)}, row 2 cuts {fmt(c2)}")
    finally:
        if os.path.exists(e):
            os.remove(e)


def outline_fails_quick_switch(cfg, t):
    """Row 1 up and cut; row 2 (a good recording) and row 3 (broken) double-
    clicked one straight after the other. Row 3 fails, and the window is on
    whichever of rows 1 and 2 got up (row 2's open may be overtaken before
    its outline). A cut made then and OK land on that row and nowhere else;
    the second round does the same with Escape+破棄, which puts that row back
    as its visit found it and leaves the others as they were."""
    e = os.path.join(cfg.media, "e.ts")
    shutil.copyfile(cfg.a, e)
    try:
        with App(cfg, "outline_fails_quick_switch",
                 [{"path": cfg.a}, {"path": cfg.b}, {"path": e}]) as app:
            wait_for("rows read", lambda: app.rows() == 3, 30)
            time.sleep(2)
            _break(e)
            prev = {1: [], 2: [], 3: []}
            for rnd, (lo, hi) in enumerate(((100, 150), (300, 330))):
                app.open_row(1, "a.ts")
                wait_for("row 1 as left", lambda: app.state()["frames"] is not None, 30)
                time.sleep(1)
                row1 = app.cut(lo, hi)
                time.sleep(0.5)
                app.to_list()
                app.s.double_click("#cliplist > li:nth-child(2) .nm")
                app.s.double_click("#cliplist > li:nth-child(3) .nm")
                app.to_editor()
                wait_for("the open to fail", lambda: (lambda s: s if (
                    "開けません" in s or "annot open" in s) else None)(app.state()["status"] or ""), 60)
                time.sleep(1.5)
                st = app.state()
                # Row 2 is b.mkv, a picture size and a length of its own.
                shown = 1 if st["frames"] == row1 and "a.ts" in st["title"] else 2
                rows = _editing_rows(app)
                app.to_editor()
                at_start = {k: list(v) for k, v in prev.items()}
                before_cut = app.state()["frames"]
                app.cut(400, 420)
                time.sleep(0.5)
                if rnd == 0:
                    app.ok()
                else:
                    app.escape(discard=True)
                app.gone()
                p = app.save()
                got = {k: cuts_of(p, k) for k in (1, 2, 3)}
                n1 = len(at_start[1]) + 1 + (1 if shown == 1 and rnd == 0 else 0)
                want_n = {1: n1 if not (shown == 1 and rnd == 1) else len(at_start[1]),
                          2: len(at_start[2]) + (1 if shown == 2 and rnd == 0 else 0),
                          3: 0}
                ok = all(len(got[k]) == want_n[k] for k in (1, 2, 3))
                what = "OK" if rnd == 0 else "Escape+破棄"
                t.check(f"37r-{rnd + 1} quick switch, row 3 fails, {what}: the window's row (row {shown}) "
                        "takes it, no other row",
                        ok and rows == [shown],
                        f"編集中 {rows}; {before_cut} pictures shown; cuts {{1: {fmt(got[1])}, "
                        f"2: {fmt(got[2])}, 3: {fmt(got[3])}}}; want counts {want_n}")
                prev = got
    finally:
        if os.path.exists(e):
            os.remove(e)


def outline_fails_then_fixed(cfg, t):
    """Row 1 up, row 2 broken: the open fails twice running. Then row 2's
    recording is put back and row 2 double-clicked a third time: it opens (a
    failed row sent again is another attempt), and a cut there is row 2's.
    Escape+破棄 after the two failures (on the way) put row 1 back as its
    visit found it, and removing row 2 from the list while the window was
    back on row 1 left the window up and its edits row 1's."""
    e = os.path.join(cfg.media, "e.ts")
    shutil.copyfile(cfg.a, e)
    try:
        with App(cfg, "outline_fails_then_fixed", [{"path": cfg.a}, {"path": e}, {"path": e}]) as app:
            wait_for("rows read", lambda: app.rows() == 3, 30)
            time.sleep(2)
            # Round 1: two failures, then Escape+破棄.
            app.open_row(1, "a.ts")
            time.sleep(1)
            app.cut(100, 150)
            time.sleep(0.5)
            app.ok()
            app.gone()
            p0 = app.save()
            app.open_row(1, "a.ts")
            wait_for("row 1 with its cut", lambda: app.state()["frames"] == FRAMES - 51, 30)
            time.sleep(1)
            app.cut(200, 220)
            _break(e)
            _fail_row(app, 2, "e.ts")
            app.cut(300, 320)
            _fail_row(app, 2, "e.ts")
            app.cut(400, 420)
            time.sleep(0.5)
            app.escape(discard=True)
            app.gone()
            p1 = app.save()
            t.check("37s-1 two failed opens, then Escape+破棄: row 1 as the visit found it",
                    cuts_of(p1, 1) == cuts_of(p0, 1) and cuts_of(p1, 2) == [],
                    f"row 1 {fmt(cuts_of(p1, 1))} (want {fmt(cuts_of(p0, 1))}), row 2 {fmt(cuts_of(p1, 2))}")
            # Round 2: fail, take row 3 out of the list (the failed recording's
            # other row), cut, OK.
            app.open_row(1, "a.ts")
            wait_for("row 1 with its cut", lambda: app.state()["frames"] == FRAMES - 51, 30)
            time.sleep(1)
            _fail_row(app, 3, "e.ts")
            app.to_list()
            app.s.click("#cliplist > li:nth-child(3) .nm")
            time.sleep(0.3)
            app.s.click("#remove-clip")
            wait_for("the row out", lambda: app.rows() == 2, 10)
            time.sleep(0.5)
            still = app.editor() is not None
            app.to_editor()
            app.cut(200, 220)
            time.sleep(0.5)
            app.ok()
            app.gone()
            p2 = app.save()
            t.check("37s-2 the failed row taken out: the window stays, its cut is row 1's",
                    still and len(cuts_of(p2, 1)) == 2 and cuts_of(p2, 2) == [],
                    f"window {'up' if still else 'closed'}; row 1 {fmt(cuts_of(p2, 1))}, row 2 {fmt(cuts_of(p2, 2))}")
            # Round 3: the recording put back, row 2 opens after its failures.
            shutil.copyfile(cfg.a, e)
            app.open_row(1, "a.ts")
            time.sleep(1)
            app.open_row(2, "e.ts")
            st = app.state()
            app.cut(10, 20)
            time.sleep(0.5)
            app.ok()
            app.gone()
            p3 = app.save()
            t.check("37s-3 the recording put back: row 2 opens and takes its own cut",
                    st["frames"] == FRAMES and len(cuts_of(p3, 2)) == 1 and cuts_of(p3, 1) == cuts_of(p2, 1),
                    f"{st['frames']} pictures on arrival; row 1 {fmt(cuts_of(p3, 1))}, row 2 {fmt(cuts_of(p3, 2))}")
    finally:
        if os.path.exists(e):
            os.remove(e)


def outline_fails_during_walk(cfg, t):
    """Row 2 (a longer recording, never read by the editor) double-clicked
    and, as soon as its counter is up -- its walk still reading -- row 1,
    broken, double-clicked: the open fails and overtakes row 2's walk, and
    the window goes on as row 2, unwalked, its plan reading for good. A
    second double-click on row 2 must bring it back (it did while the window
    let go of its row on a failure), and a cut made then is row 2's."""
    e = os.path.join(cfg.media, "e.ts")
    big = os.path.join(cfg.media, "big.ts")
    subprocess.run(["ffmpeg", "-nostdin", "-y", "-loglevel", "error", "-f", "lavfi", "-i",
                    "testsrc2=size=1280x720:rate=30000/1001:duration=120", "-c:v", "mpeg2video",
                    "-g", "15", "-bf", "2", "-b:v", "25M", "-f", "mpegts", big], check=True)
    shutil.copyfile(cfg.a, e)
    reading = "読み込んでいます"
    try:
        with App(cfg, "outline_fails_during_walk", [{"path": e}, {"path": big}]) as app:
            app.to_list()
            wait_for("row 1 read", lambda: "ready" in (app.s.js(
                "return document.querySelector('#cliplist > li .badge').className") or ""), 30)
            _break(e)
            app.open_row(2, "big.ts")
            _fail_row(app, 1, "e.ts")
            time.sleep(3)
            stuck = reading in app.state()["plan"]
            # Cut there, before the second press: the row's own edit, which
            # the reopen has to bring back.
            left = app.cut(100, 150)
            time.sleep(0.5)
            app.open_row(2, "big.ts", wait=False)
            app.to_editor()
            try:
                wait_for("row 2's plan", lambda: reading not in app.state()["plan"]
                         and app.state()["frames"] is not None, 30, 0.25)
                settled = True
            except Failure:
                settled = False
            st = app.state()
            t.check("37w-1 a second double-click on the row brings its walk back", settled,
                    f"plan {st['plan'][:50]!r}" + ("" if stuck else
                                                   "; NOT REACHED: the walk landed before the failed open"))
            again = app.cut(300, 320)
            time.sleep(0.5)
            app.ok()
            app.gone()
            p = app.save()
            c1, c2 = cuts_of(p, 1), cuts_of(p, 2)
            t.check("37w-2 the cut made before it comes back, and one made after is row 2's too",
                    c1 == [] and len(c2) == 2 and st["frames"] == left,
                    f"row 1 {fmt(c1)}, row 2 {fmt(c2)}; {st['frames']} pictures after the reopen "
                    f"(want {left}), {again} after")
    finally:
        for p in (e, big):
            if os.path.exists(p):
                os.remove(p)

def _fail_pipe(path):
    """Something that is not a recording down the pipe at `path`, to every
    reader waiting on it, then the pipe closed."""
    for _ in range(3):
        try:
            fd = os.open(path, os.O_WRONLY | os.O_NONBLOCK)
        except OSError:
            break
        try:
            os.write(fd, b"\0" * 65536)
        except OSError:
            pass
        os.close(fd)
        time.sleep(0.3)


def outline_hangs_divide(cfg, t):
    """Row 1 up and cut; row 2's recording replaced by a pipe nothing writes
    to yet, and row 2 double-clicked: its open hangs on the read (a share
    that stopped answering). Meanwhile row 1 -- no longer the row being
    edited -- is divided in two in the list. Then the open fails. The window
    must not go back to row 1 and hand the whole recording's edit over the
    first part's: the parts stay parts. Then the same with the division put
    back together (分割を元に戻す) while part 1's next row hangs: row 1 must
    come back whole, not as the part the window still shows."""
    e = os.path.join(cfg.media, "e.ts")
    shutil.copyfile(cfg.a, e)

    def hang(app, n):
        if os.path.exists(e):
            os.remove(e)
        os.mkfifo(e)
        app.open_row(n, "e.ts", wait=False)
        time.sleep(1.5)
        app.to_list()

    def after_failure(app):
        _fail_pipe(e)
        time.sleep(3)
        up = app.editor() is not None
        if up:
            app.to_editor()
            app.ok()
            app.gone()
        return up

    try:
        with App(cfg, "outline_hangs_divide", [{"path": cfg.a}, {"path": e}]) as app:
            app.to_list()
            wait_for("rows read", lambda: app.s.js(
                "return [...document.querySelectorAll('#cliplist > li .badge')]"
                ".every((b) => b.classList.contains('ready'))"), 60)
            app.open_row(1, "a.ts")
            time.sleep(1)
            app.cut(100, 150)
            time.sleep(0.5)
            hang(app, 2)
            app.s.click("#cliplist > li:nth-child(1) .nm")
            wait_for("分割 to be offered", lambda: app.s.js(
                "return !document.getElementById('divide-clip').disabled"), 20)
            app.s.click("#divide-clip")
            wait_for("the divide box", lambda: app.s.js(
                "return !document.getElementById('divide').hidden"), 5)
            app.s.js("const f = document.getElementById('divide-rule'); f.value = 'parts';"
                     "f.dispatchEvent(new Event('change'));"
                     "const v = document.getElementById('divide-value'); v.value = '2';"
                     "v.dispatchEvent(new Event('input'));")
            wait_for("分割 to be pressable", lambda: app.s.js(
                "return !document.getElementById('divide-ok').disabled"), 30)
            app.s.click("#divide-ok")
            wait_for("three rows", lambda: app.rows() == 3, 10)
            parts = app.save()
            up = after_failure(app)
            p = app.save()
            c1, w1 = cuts_of(p, 1), cuts_of(parts, 1)
            t.check("37d-1 a row divided while the next row's open hung keeps its part",
                    c1 == w1, f"row 1 {fmt(c1)} (divided as {fmt(w1)}); "
                    f"window {'up' if up else 'closed'} after the failure")
            whole = [(a, b) for a, b in c1 if a > 0.5 and b < 10]
            app.open_row(1, "a.ts")
            time.sleep(1)
            hang(app, 3)
            app.s.click("#undo-remove")
            wait_for("two rows", lambda: app.rows() == 2, 10)
            up = after_failure(app)
            p = app.save()
            c1 = cuts_of(p, 1)
            t.check("37d-2 a division put back together while the next row's open hung stays whole",
                    c1 == whole and app.rows() == 2,
                    f"row 1 {fmt(c1)} (want {fmt(whole)}); window {'up' if up else 'closed'} after the failure")
    finally:
        if os.path.exists(e):
            os.remove(e)

def outline_hangs_cut_ok(cfg, t):
    """Row 1 up; row 2's open hangs (a pipe); a cut made in the window, which
    still shows row 1, then OK while row 2 is still hanging. The cut is row
    1's and is kept. It was dropped (37th pass, S2, pre-existing):
    `captureEdit` sent nothing while `swapping`, and OK closed the window
    before the open failed. See outline_hangs_cut_escape for Escape."""
    e = os.path.join(cfg.media, "e.ts")
    shutil.copyfile(cfg.a, e)
    try:
        with App(cfg, "outline_hangs_cut_ok", [{"path": cfg.a}, {"path": e}]) as app:
            app.to_list()
            wait_for("rows read", lambda: app.s.js(
                "return [...document.querySelectorAll('#cliplist > li .badge')]"
                ".every((b) => b.classList.contains('ready'))"), 60)
            app.open_row(1, "a.ts")
            time.sleep(1)
            os.remove(e)
            os.mkfifo(e)
            app.open_row(2, "e.ts", wait=False)
            time.sleep(1.5)
            app.to_editor()
            st = app.state()
            try:
                left = app.cut(100, 150)
            except Failure as x:
                left = f"cut refused: {x}"
            time.sleep(0.5)
            app.ok()
            app.gone()
            _fail_pipe(e)
            time.sleep(2)
            p = app.save()
            c1, c2 = cuts_of(p, 1), cuts_of(p, 2)
            t.check("37h-1 a cut on the row shown while the next one hangs, then OK, is kept",
                    len(c1) == 1 and c2 == [],
                    f"title {st['title']!r}; {left} pictures; row 1 {fmt(c1)}, row 2 {fmt(c2)}")
    finally:
        if os.path.exists(e):
            os.remove(e)


def outline_hangs_cut_escape(cfg, t):
    """outline_hangs_cut_ok with Escape and 破棄 instead of OK: row 1 cut,
    row 2's open hanging (a pipe), another cut on row 1 -- still what the
    window shows -- then Escape, 破棄. What is thrown away is what is on
    screen: row 1 goes back to how its visit found it (no cut at all, the
    one made before the hang included), and row 2 has nothing. Before the
    decision of the 37th pass the cancel named only row 2, and row 1 kept
    the cut made before the hang."""
    e = os.path.join(cfg.media, "e.ts")
    shutil.copyfile(cfg.a, e)
    try:
        with App(cfg, "outline_hangs_cut_escape", [{"path": cfg.a}, {"path": e}]) as app:
            app.to_list()
            wait_for("rows read", lambda: app.s.js(
                "return [...document.querySelectorAll('#cliplist > li .badge')]"
                ".every((b) => b.classList.contains('ready'))"), 60)
            app.open_row(1, "a.ts")
            time.sleep(1)
            app.cut(100, 150)
            time.sleep(0.5)
            os.remove(e)
            os.mkfifo(e)
            app.open_row(2, "e.ts", wait=False)
            time.sleep(1.5)
            app.to_editor()
            st = app.state()
            try:
                left = app.cut(300, 320)
            except Failure as x:
                left = f"cut refused: {x}"
            time.sleep(0.5)
            asked = app.escape(discard=True, expect_dialog=False)
            app.gone()
            _fail_pipe(e)
            time.sleep(2)
            p = app.save()
            c1, c2 = cuts_of(p, 1), cuts_of(p, 2)
            t.check("37h-2 Escape+破棄 while the next row hangs puts the shown row back as its visit found it",
                    asked and c1 == [] and c2 == [],
                    f"title {st['title']!r}; {left} pictures; asked {asked}; row 1 {fmt(c1)}, row 2 {fmt(c2)}")
    finally:
        if os.path.exists(e):
            os.remove(e)


def outline_hangs_divide_cut(cfg, t):
    """Row 1 up and cut; row 2's open hangs (a pipe); meanwhile row 1 is
    divided in two in the list, and then a cut is made in the window, which
    still shows the whole of row 1. Twice: once left by OK, once by Escape
    and 破棄. The window sends that cut under row 1 (`showingLeft`, 37th
    pass), and the list must pass over it: row 1 is now the first part, and
    the whole recording's edit written over it would make that part the
    whole recording again (outline_hangs_divide's S2, by way of a cut). Escape
    must not put the first part back to the undivided row either: the
    division took that visit away (`leftVisits`)."""
    e = os.path.join(cfg.media, "e.ts")

    def run(name, leave):
        shutil.copyfile(cfg.a, e)
        with App(cfg, name, [{"path": cfg.a}, {"path": e}]) as app:
            app.to_list()
            wait_for("rows read", lambda: app.s.js(
                "return [...document.querySelectorAll('#cliplist > li .badge')]"
                ".every((b) => b.classList.contains('ready'))"), 60)
            app.open_row(1, "a.ts")
            time.sleep(1)
            app.cut(100, 150)
            time.sleep(0.5)
            os.remove(e)
            os.mkfifo(e)
            app.open_row(2, "e.ts", wait=False)
            time.sleep(1.5)
            app.to_list()
            app.s.click("#cliplist > li:nth-child(1) .nm")
            wait_for("分割 to be offered", lambda: app.s.js(
                "return !document.getElementById('divide-clip').disabled"), 20)
            app.s.click("#divide-clip")
            wait_for("the divide box", lambda: app.s.js(
                "return !document.getElementById('divide').hidden"), 5)
            app.s.js("const f = document.getElementById('divide-rule'); f.value = 'parts';"
                     "f.dispatchEvent(new Event('change'));"
                     "const v = document.getElementById('divide-value'); v.value = '2';"
                     "v.dispatchEvent(new Event('input'));")
            wait_for("分割 to be pressable", lambda: app.s.js(
                "return !document.getElementById('divide-ok').disabled"), 30)
            app.s.click("#divide-ok")
            wait_for("three rows", lambda: app.rows() == 3, 10)
            parts = app.save()
            app.to_editor()
            try:
                left = app.cut(300, 320)
            except Failure as x:
                left = f"cut refused: {x}"
            time.sleep(0.5)
            if leave == "ok":
                asked = None
                app.ok()
            else:
                asked = app.escape(discard=True, expect_dialog=False)
            app.gone()
            _fail_pipe(e)
            time.sleep(2)
            p = app.save()
            got = [cuts_of(p, n) for n in (1, 2, 3)]
            want = [cuts_of(parts, n) for n in (1, 2, 3)]
            return got, want, left, asked, app.rows()

    try:
        for leave, tag in (("ok", "37x-1"), ("escape", "37x-2")):
            got, want, left, asked, rows = run(f"outline_hangs_divide_cut_{leave}", leave)
            t.check(f"{tag} a cut in the window after its row was divided while the next row's open hung "
                    f"leaves the parts as divided ({'OK' if leave == 'ok' else 'Escape+破棄'})",
                    got == want and rows == 3 and got[0] != [] and got[2] == [],
                    f"{left} pictures; asked {asked}; rows {rows}; "
                    + "; ".join(f"row {n + 1} {fmt(g)} (divided as {fmt(w)})"
                                for n, (g, w) in enumerate(zip(got, want))))
            if os.path.exists(e):
                os.remove(e)
    finally:
        if os.path.exists(e):
            os.remove(e)


def outline_hangs_cut_then_fails(cfg, t):
    """Row 1 up and cut; row 2's open hangs (a pipe); another cut on row 1,
    which the window still shows; then row 2's open fails and the window goes
    back to row 1 (`editor-back`). Left by OK, row 1 keeps both cuts; left by
    Escape and 破棄, row 1 goes back to how its visit found it (no cut) --
    the cut made during the hang is that visit's like the one before it."""
    e = os.path.join(cfg.media, "e.ts")

    def run(name, leave):
        shutil.copyfile(cfg.a, e)
        with App(cfg, name, [{"path": cfg.a}, {"path": e}]) as app:
            app.to_list()
            wait_for("rows read", lambda: app.s.js(
                "return [...document.querySelectorAll('#cliplist > li .badge')]"
                ".every((b) => b.classList.contains('ready'))"), 60)
            app.open_row(1, "a.ts")
            time.sleep(1)
            app.cut(100, 150)
            time.sleep(0.5)
            os.remove(e)
            os.mkfifo(e)
            app.open_row(2, "e.ts", wait=False)
            time.sleep(1.5)
            app.to_editor()
            try:
                left = app.cut(300, 320)
            except Failure as x:
                left = f"cut refused: {x}"
            time.sleep(0.5)
            _fail_pipe(e)
            time.sleep(3)
            up = app.editor() is not None
            asked = None
            if up:
                app.to_editor()
                st = app.state()["title"]
                if leave == "ok":
                    app.ok()
                else:
                    asked = app.escape(discard=True, expect_dialog=False)
                app.gone()
            else:
                st = None
            p = app.save()
            return [cuts_of(p, 1), cuts_of(p, 2)], left, asked, up, st

    try:
        for leave, tag in (("ok", "37y-1"), ("escape", "37y-2")):
            (c1, c2), left, asked, up, st = run(f"outline_hangs_cut_then_fails_{leave}", leave)
            good = (len(c1) == 2 if leave == "ok" else c1 == []) and c2 == [] and up
            t.check(f"{tag} a cut on the shown row while the next row's open hangs, the open failing, then "
                    f"{'OK keeps it' if leave == 'ok' else 'Escape+破棄 drops the visit'}",
                    good, f"{left} pictures; window {'up' if up else 'closed'} after the failure, title {st!r}; "
                    f"asked {asked}; row 1 {fmt(c1)}, row 2 {fmt(c2)}")
            if os.path.exists(e):
                os.remove(e)
    finally:
        if os.path.exists(e):
            os.remove(e)


def _hang_next(app, e, n):
    """Row `n`'s recording (`e`) replaced by a pipe nothing writes to, and the
    row double-clicked: its open hangs while the window still shows the row
    before. Leaves the driver on the editor."""
    if os.path.exists(e):
        os.remove(e)
    os.mkfifo(e)
    app.open_row(n, os.path.basename(e), wait=False)
    time.sleep(1.5)
    app.to_editor()


def outline_hangs_marks(cfg, t):
    """What is done in the window while the next row's open hangs is the
    shown row's (37th pass, decided), so it has to be done with the shown
    row's facts, not the ones the hanging open has already set for itself.
    (1) 録画のチャプター on row 1 put row 2's chapters onto row 1, and OK kept
    them. (2) Row 1 carries a cut the disc's playlist made (`unplayed`), which
    makes no mark at its join; a cut made during the hang put a mark there
    (the playlist's cut judged against row 2's, none), and a mark there is a
    chapter in the output. (38th pass)"""
    e = os.path.join(cfg.media, "e.ts")
    shutil.copyfile(cfg.a, e)
    keys = lambda p, n: (p["clips"][n - 1].get("edit") or {}).get("keyframes") or []
    try:
        with App(cfg, "outline_hangs_marks", [{"path": cfg.a}, {"path": e, "chapters": [6.4, 13.4]}]) as app:
            app.to_list()
            wait_for("rows read", lambda: app.s.js(
                "return [...document.querySelectorAll('#cliplist > li .badge')]"
                ".every((b) => b.classList.contains('ready'))"), 60)
            app.open_row(1, "a.ts")
            time.sleep(1)
            _hang_next(app, e, 2)
            app.s.js("document.getElementById('chapter-keys').click();")
            time.sleep(0.5)
            st = app.state()
            app.ok()
            app.gone()
            _fail_pipe(e)
            time.sleep(2)
            p = app.save()
            k1 = keys(p, 1)
            t.check("38m-1 録画のチャプター while the next row hangs puts no other row's chapters down",
                    k1 == [], f"row 1 marks {k1}; status {st['status']!r}")
    finally:
        if os.path.exists(e):
            os.remove(e)
    shutil.copyfile(cfg.a, e)
    saved = {"path": cfg.a, "cuts": [{"a": 5.0, "b": 7.0}], "unplayed": [{"a": 5.0, "b": 7.0}],
             "keyframes": [], "activeKey": None, "playhead": 0.0, "selA": 0.0, "selB": 0.0,
             "selGone": True}
    try:
        with App(cfg, "outline_hangs_marks_plays", [{"path": cfg.a, "edit": saved}, {"path": e}]) as app:
            app.to_list()
            wait_for("rows read", lambda: app.s.js(
                "return [...document.querySelectorAll('#cliplist > li .badge')]"
                ".every((b) => b.classList.contains('ready'))"), 60)
            app.open_row(1, "a.ts")
            time.sleep(1)
            _hang_next(app, e, 2)
            try:
                left = app.cut(300, 320)
            except Failure as x:
                left = f"cut refused: {x}"
            time.sleep(0.5)
            app.ok()
            app.gone()
            _fail_pipe(e)
            time.sleep(2)
            p = app.save()
            k1, c1 = keys(p, 1), cuts_of(p, 1)
            at_plays = [k for k in k1 if abs(k - 7.0) < 0.02]
            t.check("38m-2 a cut while the next row hangs puts no mark on the playlist's cut",
                    len(c1) == 2 and not at_plays and len(k1) == 1,
                    f"{left} pictures; row 1 cuts {fmt(c1)}, marks {k1}")
    finally:
        if os.path.exists(e):
            os.remove(e)


def outline_fails_plays_escape(cfg, t):
    """A row carrying a playlist (`plays`, as a disc title's row does; here on
    a.ts) opened before the list's walk reached it: the visit cuts what the
    playlist leaves out. Row 3 double-clicked next and its open fails (its
    file no longer a recording) or hangs (a pipe), and Escape: row 2 goes
    back to how the visit found it -- no edit, the playlist's cuts still owed
    -- and takes them once the list reads it. Leaving the row for row 3 had
    already marked them taken (`takePlays` on an opened edit), and the row
    put back to no edit was written whole, the stretches the playlist leaves
    out included. (38th pass, app.js leftVisits / editor-back.) Row 1 is the
    large recording, so the list's walk is busy on it until the lanes are
    stopped."""
    make_late_head(cfg)
    e = os.path.join(cfg.media, "e.ts")
    st = float(subprocess.run(["ffprobe", "-v", "error", "-show_entries", "format=start_time",
                               "-of", "csv=p=0", cfg.a], capture_output=True, text=True).stdout.strip())
    plays = [[st + 2.0, st + 12.0]]
    for leave in ("fails", "hangs", "fails, then OK"):
        shutil.copyfile(cfg.a, e)
        rows = [{"path": cfg.d}, {"path": cfg.a, "plays": plays, "playsOwed": True}, {"path": e}]
        try:
            with App(cfg, f"outline_fails_plays_escape_{leave.split(',')[0]}", rows) as app:
                app.to_list()
                wait_for("解析を中止", lambda: app.s.js(
                    "const b = document.getElementById('stop-batch'); return !!b && !b.disabled;"), 30, 0.1)
                app.s.click("#stop-batch")
                time.sleep(1)
                reached = not _walked(app, 2)
                app.open_row(2, "a.ts")
                wait_for("the walk", lambda: (lambda s: s["plan"] and "読み込んで" not in s["plan"]
                                              and "Reading" not in s["plan"])(app.state()), 120, 0.3)
                time.sleep(1.5)
                shown = app.state()["frames"]
                if leave != "hangs":
                    with open(e, "wb") as f:
                        f.write(b"\0" * (1 << 20))
                    app.open_row(3, "e.ts", wait=False)
                    app.to_editor()
                    wait_for("the open to fail", lambda: (lambda s: s if (
                        "開けません" in s or "annot open" in s) else None)(app.state()["status"] or ""), 60)
                    time.sleep(1)
                else:
                    _hang_next(app, e, 3)
                if leave == "fails, then OK":
                    asked = None
                    app.ok()
                else:
                    asked = app.escape(discard=True, expect_dialog=False)
                app.gone()
                if leave == "hangs":
                    _fail_pipe(e)
                time.sleep(1)
                app.to_list()
                wait_for("解析を再開", lambda: app.s.js(
                    "const b = document.getElementById('stop-batch');"
                    "return !!b && !b.disabled && ['解析を再開', 'Resume analysis'].includes(b.textContent);"),
                    30, 0.3)
                app.s.click("#stop-batch")
                wait_for("row 2 read", lambda: _walked(app, 2), 300, 1)
                time.sleep(2)
                p = app.save()
                got = cuts_of(p, 2)
                good = len(got) == 2 and abs(got[0][1] - 2.0) < 0.05 and abs(got[1][0] - 12.0) < 0.05
                good = good and not p["clips"][1].get("playsOwed")
                t.check(f"38a-1 Escape after the next open {leave}: the row still takes the playlist's cuts",
                        good and reached,
                        f"row 2 {fmt(got)} (want 0-2, 12-end); {shown} pictures shown; asked {asked}; "
                        f"owed {p['clips'][1].get('playsOwed')}"
                        f"{'' if reached else ' (NOT REACHED: the list read row 2 first)'}")
        finally:
            if os.path.exists(e):
                os.remove(e)


def outline_hangs_remove_undo(cfg, t):
    """Row 1 up and cut; row 2's open hangs (a pipe); meanwhile row 1 -- the
    row the window still shows -- is taken out of the list and put back with
    削除を元に戻す. The row is back as it was, and the visit on screen is still
    its visit: (1) a cut made after that and OK are kept; (2) Escape+破棄
    puts the row back as the visit found it (no cut); (3) the open failing,
    the window goes back to row 1 and Escape+破棄 there does the same. (38th
    pass, stage 3: stage 2's remove() let go of the visit, so the cut and OK
    were dropped and 破棄 kept the visit's cuts.)"""
    e = os.path.join(cfg.media, "e.ts")

    def run(name, leave):
        shutil.copyfile(cfg.a, e)
        with App(cfg, name, [{"path": cfg.a}, {"path": e}]) as app:
            app.to_list()
            wait_for("rows read", lambda: app.s.js(
                "return [...document.querySelectorAll('#cliplist > li .badge')]"
                ".every((b) => b.classList.contains('ready'))"), 60)
            app.open_row(1, "a.ts")
            time.sleep(1)
            app.cut(100, 150)
            time.sleep(0.5)
            os.remove(e)
            os.mkfifo(e)
            app.open_row(2, "e.ts", wait=False)
            time.sleep(1.5)
            app.to_list()
            app.s.click("#cliplist > li:nth-child(1) .nm")
            wait_for("クリップ削除", lambda: not app.s.js(
                "return document.getElementById('remove-clip').disabled"), 15)
            app.s.click("#remove-clip")
            wait_for("one row", lambda: app.rows() == 1, 10)
            wait_for("削除を元に戻す", lambda: not app.s.js(
                "return document.getElementById('undo-remove').disabled"), 15)
            app.s.click("#undo-remove")
            wait_for("two rows", lambda: app.rows() == 2, 10)
            up, asked, left = True, None, None
            if leave == "fails":
                _fail_pipe(e)
                time.sleep(3)
                up = app.editor() is not None
                if up:
                    app.to_editor()
                    asked = app.escape(discard=True, expect_dialog=False)
                    app.gone()
            else:
                app.to_editor()
                try:
                    left = app.cut(300, 320)
                except Failure as x:
                    left = f"cut refused: {x}"
                time.sleep(0.5)
                if leave == "ok":
                    app.ok()
                else:
                    asked = app.escape(discard=True, expect_dialog=False)
                app.gone()
                _fail_pipe(e)
                time.sleep(2)
            p = app.save()
            return cuts_of(p, 1), cuts_of(p, 2), up, asked, left

    try:
        for leave, tag in (("ok", "38r-1"), ("escape", "38r-2"), ("fails", "38r-3")):
            c1, c2, up, asked, left = run(f"outline_hangs_remove_undo_{leave}", leave)
            good = (len(c1) == 2 if leave == "ok" else c1 == []) and c2 == [] and up
            t.check(f"{tag} the shown row taken out and put back while the next row's open hangs, then "
                    + {"ok": "a cut and OK: kept", "escape": "Escape+破棄: the visit dropped",
                       "fails": "the open failing and Escape+破棄: the visit dropped"}[leave],
                    good, f"{left} pictures; window {'up' if up else 'closed'}; asked {asked}; "
                    f"row 1 {fmt(c1)}, row 2 {fmt(c2)}")
            if os.path.exists(e):
                os.remove(e)
    finally:
        if os.path.exists(e):
            os.remove(e)


def outline_hangs_detection(cfg, t):
    """Row 1 up and cut, the commercial detection pressed in the editor, and
    at once row 2 double-clicked with its recording a pipe nothing writes to
    (its open hangs). The pass is about row 1 and the window has moved on:
    whichever way it goes -- landed before the switch, or stopped by it --
    row 1 must say the same thing as its timeline. Left by OK: a finding the
    row says it has is on the row's edit, once; left by Escape+破棄: the cut
    goes, and the row is not left saying "done" over a timeline with no
    finding (the pass greyed with nothing to show). Reopened, the editor's
    button and note agree with the row. (38th pass, stage 3.)"""
    e = os.path.join(cfg.media, "e.ts")

    def run(name, leave, landed_first):
        shutil.copyfile(cfg.a, e)
        with App(cfg, name, [{"path": cfg.a}, {"path": e}]) as app:
            app.to_list()
            wait_for("rows read", lambda: app.s.js(
                "return [...document.querySelectorAll('#cliplist > li .badge')]"
                ".every((b) => b.classList.contains('ready'))"), 60)
            app.open_row(1, "a.ts")
            time.sleep(1)
            app.cut(100, 150)
            time.sleep(0.5)
            pre = app.s.js("return document.getElementById('cm-note').textContent")
            app.s.js("document.getElementById('detect-cm').click();")
            if landed_first:
                busy = app.s.js("return document.getElementById('cm-note').textContent")
                wait_for("the detection's answer", lambda: app.s.js(
                    "return document.getElementById('cm-note').textContent") not in ("", busy), 180, 0.25)
                time.sleep(0.5)
            _hang_next(app, e, 2)
            time.sleep(8)
            during = app.s.js("return document.getElementById('cm-note').textContent")
            asked = None
            if leave == "ok":
                app.ok()
            else:
                asked = app.escape(discard=True, expect_dialog=False)
            app.gone()
            _fail_pipe(e)
            time.sleep(2)
            p = app.save()
            row = p["clips"][0]
            edit = row.get("edit") or {}
            app.open_row(1, "a.ts")
            time.sleep(2)
            note = app.s.js("return document.getElementById('cm-note').textContent")
            greyed = app.s.js("return document.getElementById('detect-cm').disabled")
            keys = app.state()["keys"]
            app.escape(discard=True, expect_dialog=False)
            app.gone()
            return dict(cuts=cuts_of(p, 1), note=edit.get("cmNote") or "", marks=edit.get("keyframes") or [],
                        pre=pre,
                        pending=row.get("cmPending"), during=during, asked=asked,
                        reopened_note=note, greyed=greyed, keys=keys)

    try:
        for leave, first, tag in (("ok", False, "38d-1"), ("escape", False, "38d-2"),
                                  ("ok", True, "38d-3"), ("escape", True, "38d-4")):
            r = run(f"outline_hangs_detection_{leave}{'_landed' if first else ''}", leave, first)
            marks = r["marks"]
            once = len(marks) == len(set(round(k, 3) for k in marks))
            if leave == "ok":
                cuts_ok = len(r["cuts"]) == 1
            else:
                cuts_ok = r["cuts"] == []
            # Greyed on reopening means the row and the window hold an answer:
            # then it has to be on screen.
            agree = (not r["greyed"]) or bool(r["reopened_note"].strip())
            # And while the open hangs, the note under the strip is the row's
            # own sentence or the pass's answer, not the count of a pass the
            # switch stopped ("まとめています" stood there).
            agree = agree and (r["during"] == r["pre"] or "CM" in r["during"])
            if first and leave == "ok":
                # Landed before the switch and kept by OK: the row's edit
                # carries the finding, and the reopened window shows it.
                cuts_ok = cuts_ok and bool(r["note"]) and r["greyed"]
            t.check(f"{tag} a detection {'landed' if first else 'pressed'} just before the next row's open hung, then "
                    f"{'OK' if leave == 'ok' else 'Escape+破棄'}: row 1 says what its timeline holds",
                    cuts_ok and once and agree, f"{r}")
            if os.path.exists(e):
                os.remove(e)
    finally:
        if os.path.exists(e):
            os.remove(e)


def sound_join_named_by_master(cfg, t):
    """全部を 1 本に with 音声のみ: the file is named by the sound written,
    which is the master's (a reel unlike it is made over into it). a.ts has
    MP2 and b.mkv AAC; with b.mkv the master at 入力と同じ the file was named
    `.mp2` off the first row, and the engine refused the whole run. A codec
    chosen names it whatever the master; a join with pictures keeps the first
    row's container."""
    def codec_of(path):
        out = subprocess.run(
            ["ffprobe", "-v", "error", "-select_streams", "a:0", "-show_entries",
             "stream=codec_name", "-of", "csv=p=0", path], capture_output=True, text=True).stdout
        return out.strip().split("\n")[0] if out.strip() else None

    def seconds(path):
        # The packets' own times: an ADTS file's stated length is a guess
        # from its bit rate.
        out = subprocess.run(
            ["ffprobe", "-v", "error", "-select_streams", "a:0", "-show_entries",
             "packet=pts_time,duration_time", "-of", "csv=p=0", path],
            capture_output=True, text=True).stdout
        try:
            last = [ln for ln in out.split() if ln][-1].split(",")
            return round(float(last[0]) + float(last[1]), 3)
        except (ValueError, IndexError):
            return None

    rows = [{"path": cfg.a}, {"path": cfg.b}]
    cases = [
        ("master b.mkv (AAC), 入力と同じ", {"container": "sound", "master": 1}, ".aac", "aac"),
        ("master a.ts (MP2), 入力と同じ", {"container": "sound", "master": 0}, ".mp2", "mp2"),
        ("master b.mkv, AC-3 chosen", {"container": "sound", "master": 1, "audio": "reencode",
                                       "audioCodec": "ac3"}, ".ac3", "ac3"),
    ]
    for n, (label, extra, ext, codec) in enumerate(cases):
        with App(cfg, f"sound_join_named_by_master/{n}", rows, {"joinAll": True, **extra}) as app:
            app.to_list()
            app.s.click('button.tab[data-screen="out"]')
            wait_for("both rows on the output screen", lambda: app.s.js(
                "return document.querySelectorAll('#out-list > li').length") == 2, 60)
            time.sleep(0.5)
            files = app.export()
            notes = app.s.js(
                "return [...document.querySelectorAll('#out-list > li .note')].map((n) => n.innerText);")
            got = [f for f in files if os.path.isfile(f)]
            one = got[0] if len(got) == 1 else None
            sec = seconds(one) if one else None
            c = codec_of(one) if one else None
            t.check(f"36g sound join, {label}: one {ext} of {codec}, both rows",
                    one is not None and one.endswith(ext) and c == codec and sec is not None
                    and 39 < sec < 41,
                    f"outputs {[os.path.basename(f) for f in files]}, codec {c}, {sec} s; notes {notes}")
            for f in files:
                shutil.rmtree(f) if os.path.isdir(f) else os.remove(f)

    # With pictures: the first row's container, whatever the master.
    with App(cfg, "sound_join_named_by_master/pictures", rows, {"joinAll": True, "master": 1}) as app:
        app.to_list()
        app.s.click('button.tab[data-screen="out"]')
        wait_for("both rows on the output screen", lambda: app.s.js(
            "return document.querySelectorAll('#out-list > li').length") == 2, 60)
        time.sleep(0.5)
        files = app.export(600)
        ts = [f for f in files if f.endswith(".ts")]
        got = pictures(ts[0]) if len(ts) == 1 else None
        t.check("36g join with pictures, master b.mkv: still the first row's .ts",
                len(files) == 1 and len(ts) == 1 and got == 2 * FRAMES,
                f"outputs {[os.path.basename(f) for f in files]}, {got} pictures (want {2 * FRAMES})")
        for f in files:
            shutil.rmtree(f) if os.path.isdir(f) else os.remove(f)


def overtaken_walk_stops(cfg, t):
    """Row 2 (a longer recording, its seek index taken away so the editor
    has to walk it) double-clicked, and the window closed with OK while that
    walk is still reading. The walk has nobody left to answer: it must stop
    rather than read the rest of the recording (38th pass, tauri). The same
    with row 1 double-clicked at once instead. Measured as the bytes the
    program reads after the press; a walk that goes on reads the rest of
    row 2's file. Not in the default list: on a local disc the walk of
    row 2 takes about a second, so how far it got before the press -- and so
    the margin -- depends on the machine. Measured on the dev VM: 233 / 283 MB
    read before the fix, 167 / 216 MB after (row 2 185 MB)."""
    import glob
    big = os.path.join(cfg.media, "big.ts")
    subprocess.run(["ffmpeg", "-nostdin", "-y", "-loglevel", "error", "-f", "lavfi", "-i",
                    "testsrc2=size=1280x720:rate=30000/1001:duration=120", "-c:v", "mpeg2video",
                    "-g", "15", "-bf", "2", "-b:v", "25M", "-f", "mpegts", big], check=True)
    size = os.path.getsize(big)

    def rchar(pid):
        with open(f"/proc/{pid}/io") as f:
            return int(next(l for l in f if l.startswith("rchar:")).split()[1])

    try:
        with App(cfg, "overtaken_walk_stops", [{"path": cfg.a}, {"path": big}]) as app:
            app.to_list()
            wait_for("rows read", lambda: app.s.js(
                "return [...document.querySelectorAll('#cliplist > li .badge')]"
                ".every((b) => b.classList.contains('ready'))"), 120)
            time.sleep(2)
            pid = app.gui_pid()

            def forget():
                for d in glob.glob(os.path.join(app.dir, "xdg", "cache", "*", "index")):
                    for f in os.listdir(d):
                        os.remove(os.path.join(d, f))

            def after(since):
                seen = []
                for _ in range(24):
                    time.sleep(0.25)
                    seen.append(rchar(pid) - since)
                return seen

            forget()
            start = rchar(pid)
            app.open_row(2, "big.ts")
            walking = "読み込んでいます" in app.state()["plan"]
            before = rchar(pid)
            app.ok()
            app.gone()
            seen = after(before)
            whole = seen[-1] + before - start
            t.check("38t-1 a walk for a window closed with OK stops",
                    whole < size,
                    f"{whole / 1e6:.1f} MB read from the press on, {seen[-1] / 1e6:.1f} MB of it after "
                    f"OK; row 2 is {size / 1e6:.1f} MB; by quarter seconds {[round(x / 1e6) for x in seen[:12]]}"
                    + ("" if walking else "; NOT REACHED: row 2's walk had landed"))
            time.sleep(2)
            forget()
            start = rchar(pid)
            app.open_row(2, "big.ts")
            walking = "読み込んでいます" in app.state()["plan"]
            before = rchar(pid)
            app.open_row(1, "a.ts", wait=False)
            seen = after(before)
            whole = seen[-1] + before - start
            t.check("38t-2 a walk overtaken by the next row stops",
                    whole < size + 4 * os.path.getsize(cfg.a),
                    f"{whole / 1e6:.1f} MB read from the first press on, {seen[-1] / 1e6:.1f} MB of it after "
                    f"the second; row 2 is {size / 1e6:.1f} MB, "
                    f"row 1 {os.path.getsize(cfg.a) / 1e6:.1f} MB; by quarter seconds "
                    f"{[round(x / 1e6) for x in seen[:12]]}"
                    + ("" if walking else "; NOT REACHED: row 2's walk had landed"))
            app.editor_ready("a.ts")
            app.ok()
            app.gone()
    finally:
        if os.path.exists(big):
            os.remove(big)


SCENARIOS = [
    escape_discard,
    escape_after_detection,
    escape_after_detection_second_visit,
    escape_after_list_detection,
    escape_after_second_press,
    ok_then_reopen,
    switch_then_ctrl_h,
    cut_and_export,
    ok_keeps_and_no_keeps,
    undo_after_detection,
    zoom_double_press,
    cross_double_press,
    zoom_then_close,
    cross_then_reopen,
    switch_rows_then_escape,
    double_press_first_open,
    ok_then_other_row,
    divide_then_edit_part,
    waveform_keeps_env_prefs,
    join_verify,
    mode_switch_during_run,
    cut_shapes_export,
    disc_fields_under_hand,
    seam_window,
    ctrl_h_before_marks_read,
    marks_over_recording,
    rename_during_run,
    no_free_folder,
    keyframe_twins,
    seam_follows_list,
    seam_keeps_unsaved,
    seam_keeps_unsaved_undo,
    seam_clear_held_join,
    seam_all_untouched_join,
    seam_follows_editor,
    disc_refuses_opus,
    disc_keeps_its_name,
    disc_gauge_appends,
    disc_name_after_a_run,
    disc_name_after_image_only,
    disc_keeps_an_empty_name,
    disc_name_renamed_during_run,
    image_only_twice,
    image_only_twice_other,
    image_from_its_own_image,
    tracks_subtitle_stream,
    disc_gauge_overwritable,
    disc_fit_busy_stretch,
    image_turned_on_during_run,
    plays_owed_left_behind,
    plays_owed_removed_undone,
    plays_due_snapped,
    plays_due_selection,
    plays_cut_marks,
    old_project_ok_unchanged,
    older_project_ok_unchanged,
    emptied_field_is_unsaved,
    reopen_keeps_selection_late_head,
    reopen_out_during_walk_late_head,
    reopen_walk_orders_late_head,
    reopen_no_container_length,
    first_visit_marks_no_container_length,
    reopen_selection_no_container_length,
    ok_during_walk_selection_no_container_length,
    held_selection_no_container_length,
    save_marks_during_walk_no_container_length,
    save_marks_during_walk_late_head,
    outline_fails_ctrl_h,
    outline_fails_first_visit,
    outline_fails_after_success,
    outline_fails_same_recording,
    outline_fails_edits_kept,
    outline_fails_shows_row,
    outline_fails_quick_switch,
    outline_fails_then_fixed,
    outline_fails_during_walk,
    outline_hangs_divide,
    outline_hangs_cut_ok,
    outline_hangs_cut_escape,
    outline_hangs_divide_cut,
    outline_hangs_cut_then_fails,
    outline_hangs_marks,
    outline_fails_plays_escape,
    outline_hangs_remove_undo,
    outline_hangs_detection,
    sound_join_named_by_master,
]


# Selectable by name, not run by default.
EXTRA = [ok_then_reopen_fast, disc_join_crossing, disc_head_recount, disc_plays,
         disc_seam_gap_chapters, disc_seam_join_chapters, disc_plays_rows, disc_plays_switch,
         disc_plays_recount, disc_plays_due, disc_plays_trim_first,
         real_old_projects_ok_unchanged, disc_plays_trim_left,
         overtaken_walk_stops]


def main():
    cfg = Cfg()
    os.makedirs(cfg.work, exist_ok=True)
    # One suite at a time on a work directory. run_gui_tests.sh refuses while
    # a SmartCut is up, but between two scenarios none is, and a second suite
    # started then shared the recordings, the sidecars `clear_sidecars` takes
    # away and the run directories `App` empties -- and the display.
    lock = open(os.path.join(cfg.work, ".suite.lock"), "w")
    try:
        fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
    except BlockingIOError:
        print(f"run_gui_tests: another suite is running in {cfg.work}; try again when it ends",
              file=sys.stderr)
        return 2
    make_media(cfg)
    t = Tally()
    only = sys.argv[1:]
    for fn in SCENARIOS + EXTRA:
        if (only and fn.__name__ not in only) or (not only and fn in EXTRA):
            continue
        print(fn.__name__, flush=True)
        clear_sidecars(cfg)
        started = time.time()
        failed = t.failed
        try:
            fn(cfg, t)
        except Exception as e:  # noqa: BLE001 -- a broken step is a FAIL, the suite goes on
            t.check(f"{fn.__name__} ran to the end", False, f"{type(e).__name__}: {e}")
            log = os.path.join(cfg.work, "runs", fn.__name__)
            print(f"        (gui log and project under {log})", flush=True)
            if os.environ.get("GUITEST_TRACE"):
                traceback.print_exc()
        print(f"        {time.time() - started:.1f}s", flush=True)
        # A run that passed has nothing worth keeping: its profile, index
        # caches and project. A failed one is kept for its log.
        if t.failed == failed:
            shutil.rmtree(os.path.join(cfg.work, "runs", fn.__name__), ignore_errors=True)
    clear_sidecars(cfg)
    print(f"=== {t.passed} passed, {t.failed} failed ===")
    return 0 if t.failed == 0 else 1


if __name__ == "__main__":
    sys.exit(main())
