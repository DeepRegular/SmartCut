#!/usr/bin/env bash
# The list window and the editor window, worked together through the real GUI.
#
# The four review passes before this suite kept finding S1s in the handshake
# between the two windows -- a cut thrown away with Escape that was written
# anyway, a second visit that took back what OK had handed over, a mark file
# written under the wrong recording -- and every one of them was a race that
# reading the code could argue either way. These run it instead: the program is
# started on a project of two small recordings made here, worked with the keys
# and the mouse through WebDriver, and judged by what it writes.
#
# Needs: an X display (DISPLAY), the GUI built (gui/src-tauri: cargo build
# --release), WebKitGTK's WebDriver (Debian: webkitgtk-webdriver, which puts
# WebKitWebDriver on PATH; or point GUITEST_WEBDRIVER at the binary), xdotool,
# wmctrl, xprop, ffmpeg/ffprobe, python3. No tauri-driver: see tests/gui/wd.py.
#
# The program is started with a profile of its own (XDG directories under the
# work directory), so the desktop session's preferences and caches are neither
# read nor touched. Each scenario starts it afresh and stops it, and the driver
# with it, before the next.
#
#   tests/run_gui_tests.sh                    all scenarios
#   tests/run_gui_tests.sh escape_discard ...  only those (names in scenarios.py)
#
# GUITEST_WORK   where the recordings, profiles and outputs go
#                (default ~/.cache/smartcut-guitest; not /tmp, a small tmpfs on
#                the dev VM). About 15 MB, outputs deleted as they are checked.
# GUITEST_LANG   the locale the program is started in (default ja_JP.UTF-8).
set -u
cd "$(dirname "$0")/.."
GUI=gui/src-tauri/target/release/gui
WORK="${GUITEST_WORK:-$HOME/.cache/smartcut-guitest}"

refuse() { echo "run_gui_tests: $*" >&2; exit 2; }

[ -n "${DISPLAY:-}" ] || refuse "no DISPLAY: these drive the real GUI and need an X display"
command -v xdpyinfo >/dev/null && { xdpyinfo >/dev/null 2>&1 || refuse "cannot open display $DISPLAY"; }
[ -x "$GUI" ] || refuse "build the GUI first: (cd gui/src-tauri && cargo build --release)"
WD="${GUITEST_WEBDRIVER:-$(command -v WebKitWebDriver || true)}"
[ -n "$WD" ] && [ -x "$WD" ] ||
  refuse "no WebKitWebDriver: install webkitgtk-webdriver (or set GUITEST_WEBDRIVER)"
for tool in xdotool wmctrl xprop ffmpeg ffprobe python3; do
  command -v "$tool" >/dev/null || refuse "$tool is not installed"
done
# One program on the display at a time: a SmartCut already up would take the
# keys meant for the one under test, and its dialogs would be answered here.
if pgrep -x gui >/dev/null; then
  refuse "a SmartCut window is already running on this machine; close it first"
fi

mkdir -p "$WORK"
echo "GUI: list and editor windows ($(stat -c %y "$GUI" | cut -d. -f1) build)"
started=$(date +%s)
GUITEST_WORK="$WORK" GUITEST_GUI="$PWD/$GUI" GUITEST_WEBDRIVER="$WD" \
  python3 tests/gui/scenarios.py "$@"
status=$?
echo "($(( $(date +%s) - started ))s)"
exit $status
