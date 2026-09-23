// The handful of things both windows have to say the same way.
//
// The list window and the cut editor are separate documents now, so anything
// they both print has to live somewhere neither of them owns. Two wordings
// for one answer is two things to keep in step, and the timecode under a
// clip in the list and the timecode over the picture in the editor are one
// answer.

import { t } from "./i18n.js";
import * as prefs from "./prefs.js";

/// The name of the pictures pass, which follows what it has been told to
/// look for.
///
/// Both windows print it -- a button in the list, a line in the editor's
/// menu, the sentence a detection ends with -- and all three have to say the
/// same thing as the answer in 環境設定: a button reading 黒白を検出 over a
/// pass that has been told to look for black alone is offering something it
/// will not do.
///
/// Takes the key of the "both" wording and hands back the key in force, so
/// that a caller is a `t(blankKey("side.detectBlank"))` and nothing more.
export function blankKey(base) {
  const shades = prefs.get("blankShades") || "both";
  if (shades === "black") return `${base}Black`;
  if (shades === "white") return `${base}White`;
  return base;
}

/// The same for a key that names one of the two flat detections.
///
/// The pictures pass answers to whichever shades it was told to look for and
/// is named for them; the sound pass has the one name and takes the key it
/// was given. `which` is "blank" or "quiet", which is how the list holds the
/// two apart everywhere else.
export const flatKey = (which, base) => (which === "blank" ? blankKey(base) : base);

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
  // One count of hundredths, and every field read back out of it. Field by
  // field, each with its own floor, a number sitting a hair under a whole
  // second loses the second as well as the hundredths. See [`HAIR`].
  const cc = Math.floor(Math.abs(t) * 100 + HAIR);
  // The sign comes off the count rather than off the number, so that an
  // instant which prints as zero prints without one. A picture's own time is
  // worked out as its timestamp less the recording's start, and the first
  // picture of a recording lands a hair either side of nothing: 拡大表示 was
  // reading -00:00:00.00 under a frame the counter beside it called 0.
  const sign = t < 0 && cc > 0 ? "-" : "";
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

/// How much of the window a dropped list leaves between itself and the edge,
/// and the least it is worth drawing in: below that it scrolls, and a list
/// that scrolls is still a list. Exported because the seam window places a
/// list of its own -- see `pairPicker` -- and two lists placed by different
/// numbers is two lists.
export const MENU_MARGIN = 8;
export const MENU_LEAST = 120;

/// Draw this window's `<select>` popups instead of letting the platform do it.
///
/// A native popup is the platform's to place, and both windows have lists it
/// places badly: the file settings are the bottom panel, so the bitrate
/// ladder ran off the edge with most of itself out of reach, and every list
/// in the program was drawn in the system's own light colours in the middle
/// of a dark window.
///
/// The `<select>` stays exactly where it is and goes on holding the answer:
/// everything that reads a setting off a control, puts one back on opening a
/// project, or translates the options still works, because the control is
/// still there. What is replaced is only what a click on it draws -- and
/// what that draws has to answer a keyboard too, because the control it
/// stands in for did.
///
/// How `wireDrops` takes a control over, for one that did not exist when it
/// ran -- a control drawn into a panel whose text is written afresh.
let adopt = null;

/// Take over a `.drop > select` built after `wireDrops` ran.
export function adoptDrop(select) {
  if (adopt) adopt(select);
}

/// Called once per window, after the markup is up. Every `.drop > select` in
/// the page is taken over; one built later is handed to `adoptDrop`.
export function wireDrops() {
  /// The menu that is up: `{ hide, onKey }`. Only ever one.
  let openDrop = null;

  function closeDrop() {
    if (openDrop) openDrop.hide();
    openDrop = null;
  }

  /// Draw `select`'s options where there is room for them, which the platform's
  /// own popup does not do here.
  function opensUpward(select) {
    const menu = document.createElement("ul");
    menu.className = "drop-menu";
    menu.hidden = true;
    select.parentElement.appendChild(menu);
    let items = [];
    /// Where the cursor is, which the mouse and the arrow keys both move.
    let at = -1;

    const paint = () => {
      items.forEach((li, i) => li.classList.toggle("at", i === at));
      if (items[at]) items[at].scrollIntoView({ block: "nearest" });
    };

    const open = () => {
      // Built on the way up rather than once: the options carry `data-i18n`, so
      // their text is whatever the language is now, not whatever it was when
      // the window was built.
      menu.innerHTML = "";
      items = [...select.options].map((opt) => {
        const li = document.createElement("li");
        li.textContent = opt.textContent;
        li.dataset.value = opt.value;
        // The answer the control is holding, marked whether or not the cursor
        // is on it -- which is what makes a list of sixteen rungs readable.
        if (opt.value === select.value) li.className = "on";
        // One that cannot be written stays on the list and cannot be reached:
        // the cursor steps over it and a click on it does nothing. See
        // `lockUnwritable` for why it is shown at all.
        if (opt.disabled) li.classList.add("off");
        menu.appendChild(li);
        return li;
      });
      at = select.selectedIndex;
      menu.hidden = false;
      // Which side of the control there is room on. Every one of these lists
      // used to sit at the bottom of the window, where the room is above --
      // hence the name of this function. The disc's own settings are at the
      // top of the panel above, and a list opening upward from there is cut
      // off by the head of the panel it is in.
      const box = select.getBoundingClientRect();
      const above = box.top - MENU_MARGIN;
      const below = window.innerHeight - box.bottom - MENU_MARGIN;
      const down = below > above;
      menu.classList.toggle("down", down);
      // And no taller than that room, so a long list scrolls inside itself
      // rather than running off the screen.
      menu.style.maxHeight = `${Math.max(MENU_LEAST, down ? below : above)}px`;
      // Where it lands. The list is a fixed layer -- see `.drop-menu` -- so it
      // is put over the control by hand rather than by being inside it, which
      // is what keeps it out of the hands of a panel that scrolls.
      menu.style.left = `${box.left}px`;
      menu.style.width = `${box.width}px`;
      menu.style.top = down ? `${box.bottom + 3}px` : "auto";
      menu.style.bottom = down ? "auto" : `${window.innerHeight - box.top + 3}px`;
      openDrop = { hide: () => (menu.hidden = true), onKey };
      paint();
    };

    /// Whether the cursor may not stand on this row.
    const off = (i) => !items[i] || items[i].classList.contains("off");

    /// The next row `dir` away that it may, if there is one.
    const step = (dir) => {
      for (let i = at + dir; i >= 0 && i < items.length; i += dir) {
        if (!off(i)) {
          at = i;
          return;
        }
      }
    };

    const commit = (i) => {
      if (items[i] && !off(i)) {
        select.value = items[i].dataset.value;
        // What a click on a real option would have raised, and in the order a
        // real one raises them. Both, because both are subscribed to across
        // these screens -- a list drawn by us has to be indistinguishable from
        // the control it stands in for, and a setting that answered only to
        // `change` was a setting this list could not move.
        select.dispatchEvent(new Event("input", { bubbles: true }));
        select.dispatchEvent(new Event("change", { bubbles: true }));
      }
      closeDrop();
    };

    const onKey = (ev) => {
      switch (ev.key) {
        case "ArrowDown":
        case "ArrowUp":
          step(ev.key === "ArrowUp" ? -1 : 1);
          paint();
          break;
        case "Home":
        case "End":
          at = ev.key === "Home" ? -1 : items.length;
          step(ev.key === "Home" ? 1 : -1);
          paint();
          break;
        case "Enter":
        case " ":
          commit(at);
          break;
        case "Escape":
          closeDrop();
          break;
        default:
          // Tab included: it is leaving, and leaving should still work.
          closeDrop();
          return;
      }
      // Swallowed, so the `<select>` underneath does not answer the same key a
      // second time -- and so a menu being driven does not also reach the
      // window's own shortcuts.
      ev.preventDefault();
      ev.stopPropagation();
    };

    select.addEventListener("mousedown", (ev) => {
      // The one thing that has to happen: without it the platform's own popup
      // opens underneath this one. It costs the click its focus, which is why
      // the focus is given back by hand -- a control that cannot be reached by
      // the keyboard after being clicked is worse than a popup in the wrong
      // place.
      ev.preventDefault();
      if (select.disabled) return;
      select.focus();
      const wasOpen = openDrop && !menu.hidden;
      closeDrop();
      if (!wasOpen) open();
    });

    menu.addEventListener("mousemove", (ev) => {
      const li = ev.target.closest("li");
      if (li && items.indexOf(li) !== at && !li.classList.contains("off")) {
        at = items.indexOf(li);
        paint();
      }
    });

    menu.addEventListener("click", (ev) => {
      const li = ev.target.closest("li");
      // A row that cannot be chosen swallows the click and leaves the menu up,
      // which is what the platform's own popup does with a disabled option:
      // nothing happened, and a menu that shut would say something had.
      if (li && !li.classList.contains("off")) commit(items.indexOf(li));
    });
  }

  document.querySelectorAll(".drop > select").forEach(opensUpward);
  adopt = opensUpward;
  // Anywhere else, and it is not a choice being made.
  window.addEventListener("mousedown", (ev) => {
    if (!ev.target.closest(".drop")) closeDrop();
  });
  window.addEventListener("keydown", (ev) => openDrop && openDrop.onKey(ev), true);
  window.addEventListener("wheel", closeDrop, true);
  // And whatever else moves what is under it: a scrollbar dragged, a key that
  // scrolls a panel. The list is a fixed layer now, so a panel that scrolled
  // out from under it would leave it standing over nothing.
  window.addEventListener("scroll", closeDrop, true);
}
