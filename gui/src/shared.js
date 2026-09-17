// The handful of things both windows have to say the same way.
//
// The list window and the cut editor are separate documents now, so anything
// they both print has to live somewhere neither of them owns. Two wordings
// for one answer is two things to keep in step, and the timecode under a
// clip in the list and the timecode over the picture in the editor are one
// answer.

import { t } from "./i18n.js";

/// A hair over, so that a sum lands on the mark it should be on.
///
/// These are floored on purpose -- a timecode names the hundredth an instant
/// falls in, the way a frame counter does -- and a floor is unforgiving of a
/// number that is a hair under the mark. Two commercial blocks of 60.06 and
/// 30.03 seconds add up to 90.08999999999999, and the list printed the total
/// as 00:01:30.08 while the engine's own line called it 90.090. The nudge is
/// far below a hundredth of a second and far above what a handful of
/// additions can lose.
const HAIR = 1e-6;

/// HH:MM:SS.cc, the way the reference tool writes an instant.
export function fmt(t) {
  if (!isFinite(t)) return "--:--:--.--";
  const sign = t < 0 ? "-" : "";
  // One count of hundredths, and every field read back out of it. Field by
  // field, each with its own floor, a number sitting a hair under a whole
  // second loses the second as well as the hundredths. See [`HAIR`].
  const cc = Math.floor(Math.abs(t) * 100 + HAIR);
  const p = (v) => String(v).padStart(2, "0");
  return `${sign}${p(Math.floor(cc / 360000))}:${p(Math.floor(cc / 6000) % 60)}:${p(
    Math.floor(cc / 100) % 60
  )}.${p(cc % 100)}`;
}

/// HH:MM:SS, for a stretch of time being counted rather than pointed at.
export function clock(t) {
  if (!isFinite(t) || t < 0) return "--:--:--";
  const s = Math.floor(t + HAIR);
  const p = (v) => String(v).padStart(2, "0");
  return `${p(Math.floor(s / 3600))}:${p(Math.floor(s / 60) % 60)}:${p(s % 60)}`;
}

/// "28分5秒", the way the reference tool puts a clip's length.
export function coarse(secs) {
  if (!isFinite(secs)) return "—";
  secs = Math.floor(secs + HAIR);
  const h = Math.floor(secs / 3600);
  const m = Math.floor((secs % 3600) / 60);
  const s = Math.floor(secs % 60);
  return (
    (h ? t("dur.h", { h }) : "") + (h || m ? t("dur.m", { m }) : "") + t("dur.s", { s })
  );
}

/// "2.9 GiB", how much of a disc a clip takes.
///
/// Powers of two, which is what every file manager on both platforms this
/// ships to reports, so that a number read here and a number read there are
/// the same number -- and named for the arithmetic that was done, so that it
/// reads as the same kind of number as the disc gauge, which counts the same
/// way.
export function size(bytes) {
  if (!isFinite(bytes) || bytes < 0) return "—";
  const units = ["B", "KiB", "MiB", "GiB", "TiB"];
  let n = bytes;
  let at = 0;
  while (n >= 1024 && at < units.length - 1) {
    n /= 1024;
    at += 1;
  }
  // Whole bytes are whole; anything scaled is worth one decimal and no more.
  return at === 0 ? `${n} ${units[at]}` : `${n.toFixed(1)} ${units[at]}`;
}

/// How a channel count is written: 5.1 rather than 6, because that is what
/// the recording calls itself and what a player will call it back.
export function chLabel(n) {
  return n === 6 ? "5.1ch" : n === 8 ? "7.1ch" : `${n}ch`;
}

/// How a commercial detection was arrived at and what it came to, in one line.
///
/// Both windows print this: the editor under its own button, the list under
/// the clip a batch detection was run on.
export function cmNote(res) {
  const how =
    res.resets > 0
      ? t("cm.how.captions", { n: res.resets })
      : res.logo_found
        ? t("cm.how.logo")
        : t("cm.how.silence");
  return res.blocks.length
    ? t("cm.found", {
        how,
        n: res.blocks.length,
        total: fmt(res.blocks.reduce((n, b) => n + (b.end - b.start), 0)),
      })
    : t("cm.none", { how });
}

/// Filenames and error messages go into rows built as markup, and a recording
/// named with an ampersand is not an excuse to mangle the list.
export const esc = (t) =>
  String(t).replace(
    /[&<>"]/g,
    (c) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;" })[c]
  );

/// Take the right button away from the webview.
///
/// Its menu is a browser's -- reload, back, "open image in new tab", and on a
/// debug build an inspector -- offering to do things to a page in a program
/// that does not have pages. Neither window has a menu of its own to put
/// there instead, so the button does nothing at all; in the editor it is
/// already the search drag's, which is what wanted this first.
export function noBrowserMenu() {
  window.addEventListener("contextmenu", (ev) => ev.preventDefault());
}

/// Take native drag and drop away from the webview.
///
/// A picture in a page is draggable by default, and a dragged <img> is not
/// the page's drag but the system's: WebKit hands it to the compositor, and
/// from there it is offered to every other window on the screen. Under
/// KDE on Wayland that is plainly visible -- a press-and-move on the film
/// strip, the preview or a keyframe thumbnail lights up whatever it passes
/// over, and a VirtualBox window under the pointer answers as if a file
/// were being dropped into the guest.
///
/// Nothing in either window is meant to be dragged out of it. Every drag
/// this program has -- the clip list's reordering, the scrubber, the film
/// strip's search -- is carried with plain mouse events, so refusing the
/// native one costs nothing. `-webkit-user-drag: none` says the same thing
/// in the stylesheet and is honoured unevenly; this is the part that holds.
///
/// Drops coming the other way are untouched: those are Tauri's, taken at
/// the window before the page is asked, and `dragstart` is only ever a drag
/// that began in here.
export function noNativeDrag() {
  window.addEventListener("dragstart", (ev) => ev.preventDefault());
}
