// 拡大表示: the cut editor's picture, at the recording's own pixels.
//
// What it is for is the question a scaled-down preview cannot answer. Whether
// a frame is interlaced is a matter of single lines -- the comb at a moving
// edge is one line out of step with the next -- and the stage's picture has
// been scaled to the width of the stage, which takes the comb out with it.
// So the editor fetches the picture again at the size the recording holds it
// (`zoom_shot`) and hands the url over; nothing is decoded in here.
//
// This window decides two things and no more: how far to magnify, and what to
// draw. Which picture and which part of it are the editor's to say, and they
// arrive as events:
//
//   * `zoom-frame`  a new picture -- the playhead moved, or a first one
//   * `zoom-point`  where the pointer is over it, as fractions of the picture
//
// Nothing is drawn smooth. `imageSmoothingEnabled` is off and the pixels come
// out as squares, because a magnifier that interpolates is a magnifier that
// invents the very detail it was opened to check.

const T = window.__TAURI__ || {};
const invoke = T.core && T.core.invoke;
const listen = T.event && T.event.listen;
const emit = T.event && T.event.emit;
const jlog = (m) => invoke && invoke("log", { msg: String(m) });

window.addEventListener("error", (e) => jlog(`zoom error ${e.message}`));
window.addEventListener("unhandledrejection", (e) => jlog(`zoom reject ${e.reason}`));

import { fmt, noBrowserMenu, noNativeDrag } from "./shared.js";
import { t as tr, applyStatic, setLang, onLangChange } from "./i18n.js";
import * as prefs from "./prefs.js";

const el = (id) => document.getElementById(id);
const face = el("face");
const ctx = face.getContext("2d");

/// The picture being magnified, once one has arrived.
let pic = null;
/// Its time in the recording, for the line under the canvas.
let at = null;
/// Where the pointer is over it, in fractions of its width and height. The
/// middle until the editor says otherwise -- a window opened while the
/// pointer is somewhere else entirely still has to show something.
let spot = { x: 0.5, y: 0.5 };
/// How far to magnify. Settled here rather than in 環境設定: it is the one
/// question this window exists to answer, and the answer changes with what
/// is being looked at.
let scale = Number(prefs.get("zoomScale")) || 4;

/// Whether the editor has said anything yet. See `announceReady`.
let answered = false;

const wiring = [];
const hear = (name, fn) => wiring.push(listen(name, fn));

function paint() {
  const box = face.parentElement.getBoundingClientRect();
  const dpr = window.devicePixelRatio || 1;
  const w = Math.max(1, Math.round(box.width * dpr));
  const h = Math.max(1, Math.round(box.height * dpr));
  if (face.width !== w || face.height !== h) {
    face.width = w;
    face.height = h;
  }
  ctx.imageSmoothingEnabled = false;
  ctx.fillStyle = "#000";
  ctx.fillRect(0, 0, w, h);
  el("empty").hidden = !!pic;
  if (!pic || !pic.naturalWidth) return;

  // How much of the picture fits, at this magnification, in the window as it
  // stands. The device pixel ratio is in it on purpose: a 4x magnification
  // means four screen pixels per source pixel, and on a 200% display that is
  // eight device pixels.
  const sw = w / (scale * dpr);
  const sh = h / (scale * dpr);
  // Centred on the pointer, and pulled back inside the picture at its edges
  // rather than padded with black: at the corner of a frame what somebody
  // wants to see is the corner, not a quarter of it in the middle of a black
  // square.
  const sx = clamp(spot.x * pic.naturalWidth - sw / 2, 0, Math.max(0, pic.naturalWidth - sw));
  const sy = clamp(spot.y * pic.naturalHeight - sh / 2, 0, Math.max(0, pic.naturalHeight - sh));
  const dw = Math.min(w, pic.naturalWidth * scale * dpr);
  const dh = Math.min(h, pic.naturalHeight * scale * dpr);
  ctx.drawImage(
    pic,
    sx,
    sy,
    Math.min(sw, pic.naturalWidth),
    Math.min(sh, pic.naturalHeight),
    Math.round((w - dw) / 2),
    Math.round((h - dh) / 2),
    Math.round(dw),
    Math.round(dh)
  );
}

const clamp = (v, lo, hi) => Math.max(lo, Math.min(hi, v));

/// What is on screen, said in the corner: where in the recording the picture
/// is, how far it is magnified, and the source pixel under the middle of the
/// view. The last one is what tells two visits to the same edge apart.
function paintFoot() {
  const px = pic ? Math.round(spot.x * pic.naturalWidth) : null;
  const py = pic ? Math.round(spot.y * pic.naturalHeight) : null;
  el("at").textContent =
    at === null || px === null
      ? "—"
      : tr("zoom.at", { time: fmt(at), scale, x: px, y: py });
}

function draw() {
  paint();
  paintFoot();
}

if (listen) {
  // A picture to magnify: the editor fetched it at the recording's own size.
  // Held as an `Image` rather than drawn straight away -- the url has to be
  // loaded before there is anything to put on the canvas, and the one already
  // up stays there until it is (the alternative is a black flash at every
  // frame while somebody steps through a seam).
  hear("zoom-frame", (ev) => {
    answered = true;
    const said = ev.payload || {};
    if (!said.url) return;
    at = typeof said.time === "number" ? said.time : null;
    const next = new Image();
    next.onload = () => {
      pic = next;
      draw();
    };
    next.onerror = () => jlog(`zoom: ${said.url} did not load`);
    next.src = said.url;
    paintFoot();
  });

  // Where the pointer is over the editor's picture. Fractions rather than
  // pixels: the stage is whatever size that window happens to be, and what
  // this window needs is the place in the *recording*.
  hear("zoom-point", (ev) => {
    answered = true;
    const said = ev.payload || {};
    if (typeof said.x !== "number" || typeof said.y !== "number") return;
    spot = { x: clamp(said.x, 0, 1), y: clamp(said.y, 0, 1) };
    draw();
  });

  hear("lang-changed", (ev) => setLang(ev.payload, false));
}

el("scale").addEventListener("change", (ev) => {
  scale = Number(ev.target.value) || 4;
  prefs.set("zoomScale", scale);
  draw();
});

// The canvas is sized to the window, so every resize is a redraw.
window.addEventListener("resize", draw);

onLangChange(() => paintFoot());

noBrowserMenu();
noNativeDrag();
applyStatic();
el("scale").value = String(scale);
draw();

/// Ask the editor for a picture, and go on asking until one arrives.
///
/// The same handshake the cut editor makes with the list, for the same
/// reason: a listener is only in the backend's registry once the call that
/// asked for it has been there and back, so a window that announces itself
/// from its last line can be answered before it can hear the answer. See
/// `announceReady` in `main.js`.
async function announceReady() {
  if (!emit) return;
  await Promise.all(wiring).catch((e) => jlog(`zoom wiring: ${e}`));
  for (let i = 0; i < 8 && !answered; i++) {
    emit("zoom-ready", null);
    await new Promise((go) => setTimeout(go, 500));
  }
}

announceReady();
