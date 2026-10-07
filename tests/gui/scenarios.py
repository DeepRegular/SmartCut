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
import os
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
            "return !!r && !r.hidden;"), 60)
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
            app.s.click("#cross-cancel")
            seam_gone(app, seam)
        after = app.save()["clips"][0].get("after") or {}
        t.check("18 キャンセル keeps what OK wrote", after.get("kind") == "dissolve",
                repr(after))


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
            if st["keys"].startswith("2") and "読み込み中" not in st["plan"]:
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
]


# Selectable by name, not run by default.
EXTRA = [ok_then_reopen_fast, disc_join_crossing]


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
