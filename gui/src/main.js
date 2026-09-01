// Frontend for the smart-rendering cutter, arranged after TMPGEnc MPEG Smart
// Renderer 6's cut editor.
//
// Two separate ideas, kept separate on purpose:
//   * キーフレーム -- marks you navigate by. They are not edits.
//   * カット       -- ranges taken out. These are the edits.
//
// Everything on screen is the *edited* timeline. Cutting does not grey a
// stretch out, it removes it: the scrubber shortens, the film strip closes
// over the hole, and the frame counter counts what will be written. Source
// times live only in `cuts`, `keyframes`, `scenes` and the calls into the
// engine; `outToSrc` / `srcToOut` are the only places the two meet.

window.addEventListener("error", (e) => jlog(`error ${e.message}`));
window.addEventListener("unhandledrejection", (e) => jlog(`reject ${e.reason}`));

const T = window.__TAURI__ || {};
const invoke = T.core && T.core.invoke;
const listen = T.event && T.event.listen;
const dialog = T.dialog;
const jlog = (m) => invoke && invoke("log", { msg: String(m) });
jlog("main.js start");

const el = (id) => document.getElementById(id);
const track = el("track");
const ctx = track.getContext("2d");

let src = null;
let playhead = 0; // source time, always on material that still exists
let selA = 0; // selection, in output time
let selB = 0;
let dragging = null;
let cuts = []; // source ranges taken out
let cutHistory = [];
let keeps = []; // [{a, b, at}] source ranges that survive, with output offset
let gops = []; // output times where a GOP starts
let seams = []; // output times of the joins in `joinTimes()`
let outDur = 0;
let keyframes = []; // source times
let activeKey = null; // source time of the selected mark, null for none
let cmBlocks = [];
let scenes = [];
let warmed = false;
/// Whether there are held pictures to read -- which happens well before
/// `warmed`, because the pass that makes them hands them over as it goes.
/// The film strip, the scroll search and the mark cards want this one; only
/// the scene index has to wait for the pass to end.
let held = false;
let proxied = false; // whether the pictures now come from a proxy
let interval = 0.5;
let previewToken = 0;
let stripToken = 0;
let hoverToken = 0;
let stripShots = [];
let stripCache = null;
/// Mark time -> a promise for that mark's picture. Promises rather than URLs
/// so that a re-render during a decode joins the decode already running.
const cardThumbs = new Map();

const clamp = (v, lo, hi) => Math.min(hi, Math.max(lo, v));
const frame = () => (src && src.fps > 0 ? 1 / src.fps : 1 / 30);
const frameNo = (t) => Math.round(t * (src ? src.fps : 30));

/// HH:MM:SS.cc, the way the reference tool writes it.
function fmt(t) {
  if (!isFinite(t)) return "--:--:--.--";
  const sign = t < 0 ? "-" : "";
  t = Math.abs(t);
  const p = (v) => String(v).padStart(2, "0");
  return `${sign}${p(Math.floor(t / 3600))}:${p(Math.floor((t % 3600) / 60))}:${p(
    Math.floor(t % 60)
  )}.${p(Math.floor((t % 1) * 100))}`;
}

// --- the edited timeline ------------------------------------------------

function normalise(list) {
  const sorted = list.filter((r) => r.b > r.a).slice().sort((x, y) => x.a - y.a);
  const out = [];
  for (const r of sorted) {
    const last = out[out.length - 1];
    if (last && r.a <= last.b) last.b = Math.max(last.b, r.b);
    else out.push({ a: r.a, b: r.b });
  }
  return out;
}

/// Recompute what survives the cuts, and where each surviving piece lands in
/// the output.
function rebuildTimeline() {
  keeps = [];
  gops = [];
  seams = [];
  outDur = 0;
  if (!src) return;
  // Material begins at the first access point, not at zero: nothing before
  // it can be decoded, and the planner clamps to it anyway. Starting the
  // timeline there is what makes the frame counter agree with the file that
  // actually gets written.
  let pos = src.points.length ? src.points[0] : 0;
  for (const c of cuts) {
    if (c.a > pos + 1e-6) keeps.push({ a: pos, b: Math.min(c.a, src.duration) });
    pos = Math.max(pos, c.b);
  }
  if (pos < src.duration - 1e-6) keeps.push({ a: pos, b: src.duration });
  for (const k of keeps) {
    k.at = outDur;
    outDur += k.b - k.a;
  }
  // Where a GOP begins, in output time. The film strip is divided on these:
  // they are the picture boundaries the format actually has, and the only
  // places a cut costs nothing. A cut's own join counts too -- the first
  // surviving picture of a segment starts a run whatever it is.
  for (const k of keeps) {
    gops.push(k.at);
    for (const p of src.points) {
      if (p > k.a + 1e-9 && p < k.b - 1e-9) gops.push(k.at + (p - k.a));
    }
  }
  gops.sort((a, b) => a - b);
  // The same joins `joinTimes()` reports, on the output clock. One list, so
  // that "is this a join?" cannot answer differently for the mark in the
  // sidebar and the cell in the strip -- they are asking about one instant,
  // and both have to decode it rather than reach for the nearest key picture.
  seams = joinTimes().map(srcToOut).filter((o) => o !== null);
}

/// Source ranges to hand the engine, kept inside the file whatever rounding
/// the index arrived with.
const outputRanges = () =>
  keeps.map((k) => [Math.max(0, k.a), Math.min(src ? src.duration : k.b, k.b)]);
const outFrames = () => Math.round(outDur * (src ? src.fps : 30));

/// Source time to output time; null when the material has been cut away.
///
/// A range is `[a, b)` -- the picture at `b` is the first one the cut took --
/// so a time sitting exactly on a join belongs to what follows it, not to
/// what came before. Getting this wrong leaves the first cut picture on
/// screen and in the strip, still being counted as if it survived.
function srcToOut(s) {
  for (let i = 0; i < keeps.length; i++) {
    const k = keeps[i];
    if (s < k.a - 1e-9) continue;
    // The end of the recording is the one place `b` is inclusive: there is
    // no picture after it to belong to. A segment that ends because a cut
    // ended it is exclusive like any other -- its `b` is the first picture
    // the cut took, and treating it as still present leaves that frame on
    // screen at the end of the timeline.
    const openEnd =
      i === keeps.length - 1 && src !== null && k.b >= src.duration - 1e-6;
    if (s < k.b - 1e-9 || (openEnd && s <= k.b + 1e-9)) return k.at + (s - k.a);
  }
  return null;
}

/// As above, but a time inside a cut answers with the seam it fell into,
/// which is where the playhead belongs once its material is gone.
function srcToOutSeam(s) {
  const exact = srcToOut(s);
  if (exact !== null) return exact;
  let best = 0;
  for (const k of keeps) if (k.a <= s) best = k.at + (k.b - k.a);
  return best;
}

function outToSrc(o) {
  if (!keeps.length) return 0;
  o = clamp(o, 0, outDur);
  for (let i = 0; i < keeps.length; i++) {
    const k = keeps[i];
    const len = k.b - k.a;
    const last = i === keeps.length - 1;
    // A time exactly on a join is the first picture *after* the cut.
    if (o < k.at + len - 1e-9 || (last && o <= k.at + len + 1e-9)) return k.a + (o - k.at);
  }
  return keeps[keeps.length - 1].b;
}

/// A stretch of output time, as the source ranges it is made of.
function outRangeToSrc(a, b) {
  const out = [];
  for (const k of keeps) {
    const len = k.b - k.a;
    const s = Math.max(a, k.at);
    const e = Math.min(b, k.at + len);
    if (e > s + 1e-9) out.push({ a: k.a + (s - k.at), b: k.a + (e - k.at) });
  }
  return out;
}

/// A stretch of source time, as the pieces of it that still exist.
function srcRangeToOut(a, b) {
  const out = [];
  for (const k of keeps) {
    const s = Math.max(a, k.a);
    const e = Math.min(b, k.b);
    if (e > s + 1e-9) out.push([k.at + (s - k.a), k.at + (e - k.a)]);
  }
  return out;
}

const playOut = () => srcToOutSeam(playhead);

function applyCuts(next) {
  cutHistory.push(cuts);
  if (cutHistory.length > 50) cutHistory.shift();
  cuts = normalise(next);
  afterCutsChanged();
}

/// Source times where the timeline has closed over a cut. Cutting the head of
/// the recording leaves only one segment and so no *internal* join -- but its
/// new beginning is a join like any other, and counts here.
function joinTimes() {
  const list = keeps.slice(1).map((k) => k.a);
  const head = src ? src.points[0] ?? 0 : 0;
  if (keeps.length && keeps[0].a > head + frame() / 2) list.push(keeps[0].a);
  return list.sort((a, b) => a - b);
}

function afterCutsChanged() {
  const before = playOut();
  const had = joinTimes();
  rebuildTimeline();
  // Every join a cut leaves behind is worth a mark: it is exactly the place
  // you will want to come back to and check.
  const joins = joinTimes();
  // The join this edit just opened is the place to look, so select its mark --
  // whether the join brought the mark with it or landed on one that was
  // already there, as it does when you cut a detected break out. Compared
  // against the joins from before the edit rather than against the marks, so
  // that undoing a cut selects nothing and leaves the selection alone.
  const fresh = joins.filter((t) => !had.some((o) => Math.abs(o - t) < frame() / 2));
  if (fresh.length) activeKey = fresh[fresh.length - 1];
  const all = keyframes.concat(joins).sort((a, b) => a - b);
  keyframes = all.filter((t, i) => i === 0 || t - all[i - 1] > frame() / 2);
  el("undo-cut").disabled = cutHistory.length === 0;
  playhead = outToSrc(clamp(before, 0, outDur));
  selA = clamp(selA, 0, outDur);
  selB = clamp(selB, selA, outDur);
  stripCache = null;
  renderKeyframes();
  updateReadouts();
  draw();
  scheduleStrip();
  schedulePlan();
}

// --- keyframes ----------------------------------------------------------

/// The selected mark is remembered by time, not by its place in the list:
/// cutting inserts joins and renumbers everything below them.
const isActive = (t) => activeKey !== null && Math.abs(t - activeKey) < frame() / 2;

/// Is this output time a join a cut left behind? Cutting the head of the
/// recording leaves no *internal* join, but its new first picture is one all
/// the same -- which is why this reads the list rather than the segments.
const isJoin = (o) => seams.some((s) => Math.abs(s - o) < 1e-9);

/// A decoded picture is the frame at that instant whatever the edit around it
/// looks like, so the time alone identifies it: unlike a held picture, it does
/// not have to be thrown away when a cut turns the mark into a join.
const cardKey = (t) => t.toFixed(3);

/// Fill a card while its decode runs.
///
/// The held pictures are key pictures, so the nearest one to a mark is up to
/// half a GOP away -- seven frames, on broadcast material. Close enough to say
/// "about here" for the moment it is up, and it costs nothing, the picture
/// being already in memory. Not close enough to keep: `paintCards` replaces it.
///
/// A mark on a join gets nothing instead. There the nearest key picture is
/// usually the last one the cut took away -- material that is no longer in the
/// recording at all, which is worse to show than a moment of blank.
async function paintHeld(t, img) {
  if (!held || cardThumbs.has(cardKey(t))) return;
  const o = srcToOut(t);
  if (o !== null && isJoin(o)) return;
  try {
    const shot = await invoke("hover_thumb", { time: t });
    if (shot && !img.dataset.exact) img.src = shot.url;
  } catch {
    /* the decode below is the one that has to arrive */
  }
}

/// The frame at each mark's own time, decoded.
///
/// A card captions itself with the mark's time, so the picture beside it has
/// to be the frame at that time rather than a key picture near it. Marks do
/// not sit on key pictures: the flag button takes the playhead where it is,
/// and CM detection reports the frame the break is actually on.
///
/// One call for all of them. `thumbs_at` walks a run of nearby times in a
/// single pass and seeks between the rest, and against a proxy each one is
/// tens of milliseconds. Every mark's picture is cached as the promise for it,
/// so a re-render while the batch is still in flight waits on that batch
/// instead of asking for the same pictures again.
function paintCards(times, imgs) {
  if (!src) return;
  const want = times.map((_, i) => i).filter((i) => !cardThumbs.has(cardKey(times[i])));
  if (want.length) {
    const batch = invoke("thumbs_at", {
      times: want.map((i) => times[i]),
      width: 200,
      exact: true,
    }).catch((e) => {
      jlog(`thumbs_at: ${e}`);
      return [];
    });
    want.forEach((i, k) => {
      const key = cardKey(times[i]);
      cardThumbs.set(
        key,
        batch.then((shots) => {
          const url = shots[k]?.url ?? null;
          // A decode that failed is not an answer worth keeping: drop it, so
          // the next render asks again rather than leaving the card blank for
          // as long as the file is open.
          if (!url) cardThumbs.delete(key);
          return url;
        })
      );
    });
  }
  times.forEach((t, i) => {
    cardThumbs.get(cardKey(t))?.then((url) => {
      if (!url) return;
      imgs[i].src = url;
      imgs[i].dataset.exact = "1";
    });
  });
}

/// `focus` is the mark to leave selected. Adding one by hand selects it; a
/// batch (CM detection) selects nothing, there being no one mark it is about.
function addKeyframes(times, focus = null) {
  const all = keyframes.concat(times.filter((t) => isFinite(t)));
  all.sort((a, b) => a - b);
  keyframes = all.filter((t, i) => i === 0 || t - all[i - 1] > frame() / 2);
  if (focus !== null && isFinite(focus)) activeKey = focus;
  renderKeyframes();
  draw();
  scheduleStrip();
}

/// Only the marks whose material is still there.
///
/// A cut takes its keyframes with it, and undoing the cut brings them back.
/// Marks either side of a cut land on the same instant once it closes up --
/// the head of a commercial break and the return to the programme become one
/// join -- so only the later of them is kept: its picture is the one that
/// still exists.
function liveKeyframes() {
  const live = keyframes.filter((t) => srcToOut(t) !== null);
  return live.filter((t, i) => {
    const next = live[i + 1];
    return next === undefined || srcToOut(next) - srcToOut(t) > frame() / 2;
  });
}

function renderKeyframes() {
  const list = el("keyframes");
  const live = liveKeyframes();
  el("key-count").textContent = live.length ? `${live.length} 個` : "";
  list.innerHTML = "";
  if (!live.length) {
    const p = document.createElement("div");
    p.className = "clips-empty";
    p.textContent =
      "まだありません。「⚑ キーフレーム」でいまの位置を登録できます。CM を検出すると、本編と CM それぞれの先頭が自動で並びます。";
    list.append(p);
    return;
  }
  const imgs = [];
  live.forEach((t, i) => {
    const li = document.createElement("li");
    if (isActive(t)) {
      li.className = "active";
      // The list scrolls, and a mark a cut just made is often below the fold.
      requestAnimationFrame(() => li.scrollIntoView({ block: "nearest" }));
    }
    const img = document.createElement("img");
    img.alt = "";
    imgs.push(img);
    paintHeld(t, img);
    const box = document.createElement("div");
    const no = document.createElement("div");
    no.className = "no";
    no.textContent = `#${String(i + 1).padStart(2, "0")}`;
    const at = document.createElement("div");
    at.className = "at";
    at.textContent = fmt(srcToOut(t));
    box.append(no, at);
    const kill = document.createElement("button");
    kill.className = "kill";
    kill.textContent = "✕";
    kill.title = "このキーフレームを消す";
    kill.addEventListener("click", (ev) => {
      ev.stopPropagation();
      keyframes = keyframes.filter((x) => x !== t);
      if (isActive(t)) activeKey = null;
      renderKeyframes();
      draw();
      scheduleStrip();
    });
    li.append(img, box, kill);
    li.addEventListener("click", () => {
      activeKey = t;
      renderKeyframes();
      showFrame(t);
    });
    list.append(li);
  });
  paintCards(live, imgs);
}

// --- access points and scenes -------------------------------------------

/// Nearest access point, i.e. the nearest place a cut is free.
function nearestPoint(t, dir = 0) {
  if (!src || !src.points.length) return t;
  if (dir > 0) return src.points.find((p) => p > t + 1e-6) ?? t;
  if (dir < 0) return [...src.points].reverse().find((p) => p < t - 1e-6) ?? t;
  let best = src.points[0];
  for (const p of src.points) if (Math.abs(p - t) < Math.abs(best - t)) best = p;
  return best;
}

const atPoint = (t) => src && src.points.some((p) => Math.abs(p - t) < frame() / 2);
const nearScene = (t, w) => scenes.some((s) => Math.abs(s - t) <= w);

// --- scrubber -----------------------------------------------------------

const TOP = 14;
const HGT = 32;
const MID = TOP + HGT / 2;
const TRACK_H = 84;

function layout() {
  const ratio = window.devicePixelRatio || 1;
  const w = track.clientWidth;
  track.width = w * ratio;
  track.height = TRACK_H * ratio;
  ctx.setTransform(ratio, 0, 0, ratio, 0, 0);
  return w;
}

const timeToX = (t, w) => (outDur > 0 ? (t / outDur) * w : 0);
const xToTime = (x, w) => (outDur > 0 ? clamp((x / w) * outDur, 0, outDur) : 0);

function arrowDown(x, y, size) {
  ctx.beginPath();
  ctx.moveTo(x, y + size);
  ctx.lineTo(x - size * 0.55, y);
  ctx.lineTo(x + size * 0.55, y);
  ctx.closePath();
  ctx.fill();
  ctx.fillRect(Math.round(x) - 0.5, y - size * 0.7, 1, size * 0.7);
}

function draw() {
  const w = layout();
  ctx.clearRect(0, 0, w, TRACK_H);
  if (!src || outDur <= 0) return;

  // Scene changes first, as a fine row under everything: useful to have, but
  // there are hundreds of them and they must not shout over the selection.
  ctx.fillStyle = "rgba(240,160,32,.45)";
  let lastX = -9;
  for (const s of scenes) {
    const o = srcToOut(s);
    if (o === null) continue;
    const x = Math.round(timeToX(o, w));
    if (x === lastX) continue;
    lastX = x;
    ctx.fillRect(x, 62, 1, 5);
  }

  // the whole trough is what will be written; nothing else is left
  ctx.fillStyle = "#2f7d5a";
  ctx.fillRect(0, TOP, w, HGT);

  ctx.fillStyle = "rgba(200,120,60,.9)";
  for (const b of cmBlocks) {
    for (const [a, e] of srcRangeToOut(b.start, b.end)) {
      const x = timeToX(a, w);
      ctx.fillRect(x, TOP + HGT - 6, Math.max(1, timeToX(e, w) - x), 5);
    }
  }

  const x1 = timeToX(selA, w);
  const x2 = timeToX(selB, w);
  ctx.fillStyle = "rgba(20,184,212,.42)";
  ctx.fillRect(x1, TOP + 1, Math.max(2, x2 - x1), HGT - 2);
  ctx.fillStyle = "#14b8d4";
  ctx.fillRect(x1, TOP + 1, Math.max(2, x2 - x1), 3);

  // seams: where a cut closed up. The material is gone, so all that is left
  // to show is the join.
  ctx.fillStyle = "#d05a5a";
  for (const k of keeps.slice(1)) ctx.fillRect(Math.round(timeToX(k.at, w)) - 1, TOP, 2, HGT);

  ctx.strokeStyle = "#4a4a4a";
  ctx.strokeRect(0.5, TOP + 0.5, w - 1, HGT - 1);

  ctx.fillStyle = "#c9c9c9";
  for (const t of liveKeyframes()) arrowDown(Math.round(timeToX(srcToOut(t), w)), 2, 8);

  const tab = (x, left) => {
    ctx.fillStyle = "#d8d8d8";
    ctx.beginPath();
    const d = left ? 1 : -1;
    ctx.moveTo(x, TOP + HGT + 1);
    ctx.lineTo(x - 10 * d, TOP + HGT + 10);
    ctx.lineTo(x, TOP + HGT + 10);
    ctx.closePath();
    ctx.fill();
    ctx.fillRect(Math.round(x) - (left ? 0 : 2), TOP + HGT + 1, 2, 10);
  };
  tab(x1, true);
  tab(x2, false);

  const px = timeToX(playOut(), w);
  ctx.beginPath();
  ctx.arc(px, MID, 7, 0, Math.PI * 2);
  ctx.fillStyle = "#fff";
  ctx.fill();
  ctx.beginPath();
  ctx.arc(px, MID, 3, 0, Math.PI * 2);
  ctx.fillStyle = "#1b1b1b";
  ctx.fill();

  ctx.fillStyle = "#8a8a8a";
  ctx.font = "10px system-ui";
  ctx.fillText(fmt(0), 2, 81);
  const end = fmt(outDur);
  ctx.fillText(end, w - ctx.measureText(end).width - 2, 81);
}

// --- picture ------------------------------------------------------------

/// How many pixels wide the picture on the stage actually is.
///
/// Asking for a fixed 960 was asking for the wrong thing twice: on a stage
/// wider than that the picture was blown up by the browser and looked soft
/// however good the proxy was, and on a narrow one it was decoded and
/// encoded at a size nothing would ever show. The stage is laid out with
/// `object-fit: contain`, so its own width is the ceiling; device pixels
/// rather than CSS ones, because that is what the screen has.
///
/// Rounded down to a step so that dragging a window edge does not ask for a
/// different size on every frame it passes through.
const STAGE_STEP = 64;
function stageWidth(cap = 1920) {
  const box = el("preview").clientWidth || 960;
  const want = box * (window.devicePixelRatio || 1);
  return clamp(Math.round(want / STAGE_STEP) * STAGE_STEP, 320, cap);
}

async function showFrame(t) {
  if (!src) return;
  if (playing && t !== playhead) stopPlay();
  playhead = outToSrc(clamp(srcToOutSeam(t), 0, Math.max(0, outDur - frame())));
  updateReadouts();
  draw();

  const token = ++previewToken;
  try {
    // A segment ends *between* two pictures, and the nearest picture to a
    // time in that gap is the first one the cut took -- so the last moments
    // of a segment would show a frame that no longer exists. Ask again a
    // frame earlier until the picture is one that survived.
    let shot = null;
    for (let ask = playhead, i = 0; i < 3; i++, ask -= frame()) {
      shot = await invoke("preview", { time: ask, width: stageWidth() });
      if (token !== previewToken) return;
      if (srcToOut(shot.time) !== null) break;
    }
    // Snap to the picture that actually came back, so the frame counter and
    // the picture never disagree. They would under 2:3 pulldown, where the
    // pictures do not sit on the 29.97 fps grid the playhead moves along.
    if (srcToOut(shot.time) !== null) playhead = shot.time;
    updateReadouts();
    draw();
    el("preview").src = shot.url;
    el("ovl-kind").textContent = atPoint(shot.time)
      ? `${shot.kind} フレーム — 無劣化点`
      : `${shot.kind} フレーム`;
    el("ovl-kind").className = atPoint(shot.time) ? "key" : "";
  } catch (e) {
    if (token === previewToken) el("status").textContent = `プレビュー失敗: ${e}`;
  }
  scheduleStrip();
}

const seekOut = (o) => showFrame(outToSrc(clamp(o, 0, outDur)));

function updateReadouts() {
  const o = playOut();
  el("ovl-frame").textContent = String(frameNo(o));
  el("ovl-time").textContent = fmt(o);
  el("counter").textContent = `${frameNo(o)} / ${outFrames()}   ${fmt(o)}`;
  // OUT is part of the selection, so its own picture counts towards the length
  const sel = `選択 ${frameNo(selA)} - ${frameNo(selB)} : ${fmt(selEnd() - selA)}`;
  el("selection").textContent = sel;
  el("ovl-sel").textContent = sel;
}

// --- film strip ---------------------------------------------------------
//
// A row of pictures taken at the GOP boundaries and centred on the playhead
// -- which is how the reference tool draws it, and the right unit twice
// over: those boundaries are the only places a cut is free, and the pictures
// at them are exactly the ones already held in memory, so a cell costs
// nothing to fill.
//
// **Every cell is one picture wide.** What the menu picks is therefore how
// much *time* a cell covers, not how wide it is drawn: at three minutes a
// cell swallows a run of GOPs and the boundaries inside it are skipped, at
// three seconds it holds a single one. Widths that followed each GOP's own
// length were tried first and read badly -- a long GOP at a close zoom came
// out as one small picture stranded in a wide black cell, and the same strip
// drew cells of two different sizes for a reason nobody can see. A cell's
// width now says the same thing everywhere, and its caption says when.
//
// The cells hang on a reel drawn wider than the window shows, and following
// the playhead is a transform on that reel rather than a redraw. That is what
// playback needs: pictures arrive fifteen times a second and a redraw costs a
// round trip, so a strip that redrew to follow would step, however often it
// stepped. Sliding it instead is free, and a fresh reel is only built once the
// playhead nears the edge of the drawn one -- around the same pictures, in the
// same places, so the swap does not show.

/// A moment of quiet and the strip redraws, so that a burst of small moves
/// costs one round trip rather than one each -- but with a ceiling on how
/// long that can be put off. A run of moves closer together than the delay
/// -- a held step button, a spun wheel -- kept pushing the redraw back for
/// as long as the run lasted, and the strip sat still until the hand came
/// off. Once a redraw has been deferred this long it is left alone to
/// happen, and the run picks up a fresh delay from there.
const STRIP_WAIT = 140;
const STRIP_FLOOR = 240;

let stripTimer = null;
let stripSince = 0;

function scheduleStrip() {
  const now = Date.now();
  if (!stripTimer) stripSince = now;
  else if (now - stripSince >= STRIP_FLOOR) return;
  clearTimeout(stripTimer);
  stripTimer = setTimeout(() => {
    stripTimer = null;
    askStrip();
  }, STRIP_WAIT);
}

/// One redraw at a time, and the latest place asked for wins.
///
/// A redraw is a round trip, and until the proxy is built it is a decode per
/// cell behind that. Firing one off per pointer notch -- which the scroll
/// search does, fourteen times a second -- queues work far faster than it can
/// finish, and the strip ends up chasing a position the playhead left long
/// ago. This is the treatment the wheel's own decodes already get.
let stripBusy = false;
let stripNext = null;

function askStrip(at) {
  if (stripBusy) {
    stripNext = { at };
    return;
  }
  stripBusy = true;
  runStrip(at);
}

async function runStrip(at) {
  try {
    await refreshStrip(at);
  } catch (e) {
    jlog(`strip: ${e}`);
  }
  if (stripNext) {
    const next = stripNext;
    stripNext = null;
    runStrip(next.at);
  } else {
    stripBusy = false;
  }
}

/// How much of the recording the strip covers, and whether it is divided by
/// GOP or by frame. A null span means frame mode.
function stripView() {
  const v = el("strip-step").value;
  return v === "frame" ? { span: null } : { span: parseFloat(v.slice(4)) };
}

/// The height the pictures are drawn at -- `.strip img` in the stylesheet has
/// the other copy of this number -- and, with the recording's own shape, how
/// wide one cell comes out.
///
/// The shape is read off a picture that is already on screen rather than off
/// the coded size, because the coded size is not it: broadcast material is
/// anamorphic, and the engine has already undone that in everything it hands
/// over. 16:9 until there is a picture to ask.
const CELL_H = 62;

function cellPx() {
  const p = el("preview");
  const r = p.naturalWidth > 0 ? p.naturalWidth / p.naturalHeight : 16 / 9;
  return clamp(Math.round(CELL_H * r), 48, 320);
}

/// Ceiling on how many pictures one reel is worth asking for. Only reached on
/// a very wide window during playback, where the margin doubles the count.
const MAX_CELLS = 40;

const reel = el("reel");

/// What the drawn reel holds: its cells, each with the stretch of output time
/// it stands for and where it sits on the reel in pixels; `px`, how wide the
/// whole reel is; `vis`, how much time the window shows, which is what says
/// when the playhead has wandered far enough to want a fresh one; and `rest`,
/// the place it was drawn for.
///
/// Pixels rather than shares of a span, because the cells are all one width
/// and the time behind them is not.
let reelWin = null;

/// How many windows wide to draw the reel. The margin is what the reel slides
/// across, and every move now slides it -- playback, a held step button, a
/// spun wheel. One window wide there is nothing to slide across: `placeReel`
/// reaches its clamp within a step or two of being drawn, so the strip stands
/// still while the playhead goes on, and then lurches when the next redraw
/// re-centres it.
///
/// Until the pass has pictures in memory each cell of the margin is a decode
/// of its own, which costs more than the sliding is worth -- a strip that dear
/// to draw is better drawn small. Playback keeps its margin either way, having
/// nothing else it can do.
const overscan = () => (playing || held ? 2 : 1);

/// Where output time `o` falls on the reel, in pixels from its left edge.
///
/// A cell stands for the whole stretch of time it covers, so the place inside
/// it is read off proportionally: that is what keeps the playhead creeping
/// across a cell rather than jumping from one to the next.
function reelX(o) {
  const cells = reelWin.cells;
  let i = 0;
  while (i + 1 < cells.length && cells[i + 1].a <= o) i++;
  const c = cells[i];
  const f = c.b > c.a ? (o - c.a) / (c.b - c.a) : 0;
  return c.x + f * c.px;
}

/// Slide the reel so that output time `o` falls under the marker. Clamped to
/// what was drawn: if a redraw is late, the strip holding still for a moment
/// reads far better than a gap opening at its edge.
function placeReel(o) {
  if (!reelWin) return;
  const w = el("strip").clientWidth;
  if (w <= 0) return;
  const x = clamp(w / 2 - reelX(o), Math.min(0, w - reelWin.px), 0);
  reel.style.transform = `translateX(${x.toFixed(2)}px)`;
  markHere(o);
}

/// Where the reel belongs right now: under the playhead, wherever that has
/// got to. Placing a fresh reel at `rest` -- the place it was *drawn* for --
/// was near enough while nothing moved during the round trip, but a held step
/// button moves on while it runs, and the reel arriving a frame behind reads
/// as the strip twitching backwards.
const holdReel = () => reelWin && placeReel(playing ? playPos() : playOut());

/// Outline the cell the playhead stands in. It changes as the reel slides, so
/// it is set here rather than baked in when the cells are built.
///
/// A GOP-divided reel wants the last cell that has begun, while frame cells
/// are centred on their picture and want the nearest.
function markHere(o) {
  const cells = reelWin.cells;
  let i = -1;
  for (let k = 0; k < cells.length; k++) {
    if (reelWin.byNearest) {
      if (i < 0 || Math.abs(cells[k].at - o) < Math.abs(cells[i].at - o)) i = k;
    } else if (o >= cells[k].a - 1e-9) i = k;
  }
  if (i === reelWin.here) return;
  if (cells[reelWin.here]) cells[reelWin.here].fig.classList.remove("here");
  if (cells[i] && cells[i].live) cells[i].fig.classList.add("here");
  reelWin.here = i;
}

/// The cells a GOP-divided reel is made of: `slots` of them, each covering a
/// run of `every` GOPs, centred on the one the playhead stands in. `at` is
/// the picture to show and the time to caption; `a` and `b` are the stretch
/// the cell speaks for, which is what the playhead is placed against.
///
/// `every` is chosen so that the cells the *window* can hold cover about the
/// span the menu asked for. That is what "GOP・3 分" means once the widths are
/// fixed: three minutes across the window, at however many GOPs per cell that
/// comes to. At the short end a cell cannot hold less than one GOP, so the
/// window covers rather more than it says and every boundary is drawn -- the
/// honest answer, and the one that reads.
///
/// Slots that fall outside the recording are kept, as blanks. They are what
/// lets the reel slide far enough to hold the playhead at the middle when it
/// is near either end; without them the reel would run out and the marker
/// would drift off the picture it is meant to be standing on.
function gopCells(o, span, slots, vis) {
  const n = gops.length;
  if (!n) return [{ at: 0, a: 0, b: Math.max(outDur, span), live: true }];
  // the GOP the playhead is standing in
  let i0 = 0;
  while (i0 + 1 < n && gops[i0 + 1] <= o + 1e-9) i0++;
  // How many GOPs one cell has to swallow for `vis` of them to reach across
  // the span the menu asked for. Off the whole recording's average rather
  // than off the boundaries around the playhead, because at either end only
  // half the span is there to count and the answer would come out half what
  // it should be -- which drew a minute and a quarter under "3 分".
  const every = Math.max(1, Math.round((span / vis) * (n / Math.max(outDur, 1e-9))));

  const half = slots >> 1;
  const cells = [];
  for (let k = -half; k < slots - half; k++) {
    const j = i0 + k * every;
    if (j < 0 || j >= n) {
      cells.push({ live: false });
      continue;
    }
    const a = gops[j];
    const b = Math.min(gops[j + every] ?? outDur, outDur);
    cells.push({ at: a, a, b: Math.max(b, a + 1e-3), live: true });
  }
  // A blank has no time of its own, so it takes over where the cell beside it
  // leaves off. The reel stays continuous in time that way, and the playhead
  // can be found on it whichever slot it happens to fall in.
  const d = Math.max(span / vis, 1e-3);
  for (let k = 1; k < cells.length; k++) {
    if (!cells[k].live && cells[k - 1].b !== undefined) {
      cells[k].a = cells[k - 1].b;
      cells[k].b = cells[k].a + d;
    }
  }
  for (let k = cells.length - 2; k >= 0; k--) {
    if (!cells[k].live && cells[k].a === undefined) {
      cells[k].b = cells[k + 1].a;
      cells[k].a = cells[k].b - d;
    }
  }
  return cells;
}

/// Draw a reel centred on `at`, or on the playhead when it is not given.
async function refreshStrip(at) {
  if (!src || outDur <= 0) return;
  const view = stripView();
  const o = at === undefined ? playOut() : clamp(at, 0, outDur);
  if (view.span === null) {
    await refreshFrameStrip(o);
    return;
  }
  stripCache = null;

  const px = cellPx();
  // as many cells as the window holds, and a reel of them wide enough to
  // slide across while playback runs
  const vis = Math.max(1, Math.ceil(el("strip").clientWidth / px));
  const slots = Math.max(vis + 1, Math.min(vis * overscan() + 1, MAX_CELLS));
  const cells = gopCells(o, view.span, slots, vis);
  const live = cells.filter((c) => c.live);
  if (!live.length) return;

  const times = live.map((c) => outToSrc(c.at));
  // Cells that begin on a GOP are already in memory; a cell that begins on a
  // join is not, and asking the held pictures for it would hand back the
  // last picture the cut took. Those few are decoded.
  const wanted = live.map((c) => isJoin(c.at));
  const token = ++stripToken;
  let got;
  try {
    const [kept, decoded] = await Promise.all([
      invoke("thumbs_at", { times: times.filter((_, i) => !wanted[i]), width: 200 }),
      wanted.some(Boolean)
        ? invoke("thumbs_at", { times: times.filter((_, i) => wanted[i]), width: 200, exact: true })
        : Promise.resolve([]),
    ]);
    let h = 0;
    let d = 0;
    got = wanted.map((w) => (w ? decoded[d++] : kept[h++]));
  } catch (e) {
    jlog(`thumbs_at: ${e}`);
    return;
  }
  if (token !== stripToken) return;

  let k = 0;
  const shots = cells.map((c) => {
    if (!c.live) return { url: null, time: null, at: c.a, a: c.a, b: c.b, px };
    const g = got[k++];
    return { url: g ? g.url : null, time: outToSrc(c.at), at: c.at, a: c.a, b: c.b, px };
  });
  // what one cell covers, which is what the marks below are drawn against
  const mid = live[live.length >> 1];
  renderStrip(shots, mid.b - mid.a, {
    vis: vis * (mid.b - mid.a),
    rest: o,
    byNearest: false,
  });
}

/// Frame mode: one cell per picture, with a wide window cached so that
/// stepping does not pay for a seek and a GOP every time.
async function refreshFrameStrip(o) {
  const sp = frame();
  // One picture wide here too, so that changing the menu changes how much of
  // the recording is on screen and nothing else about how it looks.
  const px = cellPx();
  const vis = Math.max(3, Math.ceil(el("strip").clientWidth / px));
  // cells either side of the middle one: the window holds `vis` of them, the
  // reel that many again for the margin
  const half = Math.ceil((vis * overscan()) / 2);
  const put = (shots, i) =>
    renderStrip(
      shots
        .slice(i - half, i + half + 1)
        .map((s) => ({ ...s, a: s.at - sp / 2, b: s.at + sp / 2, px })),
      sp,
      { vis: vis * sp, rest: shots[i].at, byNearest: true }
    );
  if (stripCache) {
    // by nearest picture rather than by index arithmetic: the playhead sits
    // on real picture times, which are not a whole number of frames from
    // wherever the cached window happened to start
    let i = -1;
    for (let j = 0; j < stripCache.shots.length; j++) {
      const t = stripCache.shots[j].time;
      if (t === null) continue;
      if (i < 0 || Math.abs(t - playhead) < Math.abs(stripCache.shots[i].time - playhead)) i = j;
    }
    if (i >= half && i + half < stripCache.shots.length) {
      put(stripCache.shots, i);
      return;
    }
  }
  // wide enough that stepping through it finds a reel's worth of pictures
  // either side of the playhead before it has to be built again
  const n = Math.max(41, 4 * half + 1);
  const first = o - (n >> 1) * sp;
  const times = Array.from({ length: n }, (_, i) => first + i * sp);
  const live = times.map((t) => (t < -1e-9 || t > outDur + 1e-9 ? null : outToSrc(t)));
  const ask = live.filter((t) => t !== null);
  const token = ++stripToken;
  let got;
  try {
    got = await invoke("thumbs_at", { times: ask, width: 200 });
  } catch (e) {
    jlog(`thumbs_at: ${e}`);
    return;
  }
  if (token !== stripToken) return;
  let k = 0;
  const shots = live.map((t, i) => {
    if (t === null) return { url: null, time: null, at: times[i] };
    const g = got[k++];
    return { url: g ? g.url : null, time: t, at: times[i] };
  });
  stripCache = { first, shots };
  put(shots, n >> 1);
}

/// Lay the cells out on the reel. Each is drawn at its own `px` -- one
/// picture wide, the same for every cell on the reel -- and `win` carries
/// what `placeReel` needs to slide the result.
function renderStrip(shots, unit, win) {
  stripShots = shots;
  reel.innerHTML = "";
  const cells = [];
  let x = 0;

  // Every cell shows the picture its GOP begins with, the one the playhead is
  // standing in included. Swapping that one for the picture under the playhead
  // was tried and is worse: crossing a scene change makes a single cell jump
  // to a different shot while its neighbours hold still, which reads as a
  // glitch. The strip is a ruler; the marker says where you are.
  shots.forEach((s, i) => {
    const fig = document.createElement("figure");
    fig.style.width = `${s.px.toFixed(2)}px`;
    cells.push({ fig, at: s.at, a: s.a, b: s.b, x, px: s.px, live: !!s.url });
    x += s.px;
    if (!s.url) {
      fig.className = "blank";
      reel.append(fig);
      return;
    }
    const classes = [];
    if (s.at >= selA && s.at < selB) classes.push("inside");
    // Worth flagging only in frame mode: every cell of a GOP-divided strip
    // is an access point, so marking them all says nothing.
    if (unit < 0.1 && atPoint(s.time)) classes.push("kf");
    if (nearScene(s.time, unit / 2)) classes.push("scene");
    if (keyframes.some((t) => Math.abs(t - s.time) < unit / 2)) classes.push("mark");
    // a join the cuts closed up sits between this cell and the one before it
    const prev = shots[i - 1];
    if (prev && prev.time !== null && s.time - prev.time > unit * 2.5 + 0.5) {
      classes.push("seam");
    }
    fig.className = classes.join(" ");
    const img = document.createElement("img");
    img.src = s.url;
    const cap = document.createElement("figcaption");
    cap.textContent = fmt(s.at);
    fig.append(img, cap);
    fig.addEventListener("click", () => seekOut(s.at));
    fig.addEventListener("auxclick", (ev) => {
      if (ev.button !== 1) return;
      ev.preventDefault();
      toScene(1, s.time - frame());
    });
    reel.append(fig);
  });
  reel.style.width = `${x.toFixed(2)}px`;
  el("playline").hidden = false;
  reelWin = { ...win, cells, px: x, here: null };
  holdReel();
}

// --- scroll search ------------------------------------------------------

let search = null;
let lastFast = -1;
let lastSharp = 0;

async function paintFast(t) {
  if (!held) return;
  if (Math.abs(t - lastFast) < interval / 2) return;
  const token = ++hoverToken;
  const shot = await invoke("hover_thumb", { time: t });
  if (token !== hoverToken || !shot) return;
  lastFast = shot.time;
  el("preview").src = shot.url;
  el("ovl-kind").textContent = "サーチ";
  el("ovl-kind").className = "";
}

/// A properly decoded picture, dropped in behind the held one without moving
/// the playhead. Only worth asking for while the scroll is slow: a decode is
/// a few hundred milliseconds, and at speed the held pictures arrive faster
/// than the eye can use them anyway.
async function paintSharp(t) {
  const token = ++previewToken;
  try {
    const shot = await invoke("preview", { time: t, width: stageWidth() });
    if (token === previewToken) el("preview").src = shot.url;
  } catch {
    /* the next tick will try again */
  }
}

function startSearch(ev) {
  if (!src) return;
  const rect = el("strip").getBoundingClientRect();
  search = { x: ev.clientX, rect };
  el("searching").hidden = false;
  lastFast = -1;
  search.timer = setInterval(() => {
    const half = search.rect.width / 2;
    const dx = clamp((search.x - (search.rect.left + half)) / half, -1, 1);
    // cubed, so the middle of the strip is a fine crawl and the far edges
    // cross a half-hour recording in half a minute
    const rate = Math.sign(dx) * Math.abs(dx) ** 3 * 60;
    if (rate === 0) return;
    playhead = outToSrc(clamp(playOut() + rate * 0.07, 0, Math.max(0, outDur - frame())));
    updateReadouts();
    draw();
    paintFast(playhead);
    askStrip();
    if (Math.abs(rate) < 2.5 && Date.now() - lastSharp > 320) {
      lastSharp = Date.now();
      paintSharp(playhead);
    }
  }, 70);
}

function endSearch() {
  if (!search) return;
  clearInterval(search.timer);
  search = null;
  el("searching").hidden = true;
  showFrame(playhead);
}

el("strip").addEventListener("contextmenu", (ev) => ev.preventDefault());
el("strip").addEventListener("mousedown", (ev) => {
  if (ev.button !== 2) return;
  ev.preventDefault();
  startSearch(ev);
});
window.addEventListener("mousemove", (ev) => {
  if (search) search.x = ev.clientX;
});
window.addEventListener("mouseup", (ev) => {
  if (ev.button === 2) endSearch();
});

// A wheel notch or a held arrow key can arrive faster than a decode:
// `preview` seeks and re-encodes a JPEG, tens of milliseconds at best.
// Firing one off per notch queues up decodes far faster than they can
// finish, and the app visibly falls behind -- exactly what the scrubber's
// drag and the right-click search already avoid, by following the pointer
// with a cheap held picture and only asking for a real decode once. This
// gives the wheel and the arrow keys the same treatment: the position and
// the film strip follow every notch, but the expensive decode is at most
// one in flight, always for the latest place asked for. A single notch
// finds nothing in flight and decodes at once, same as before.
let scrubBusy = false;
let scrubPending = null;
let scrubIdle = [];

/// Resolves once nothing is in flight -- that is, once the picture for the
/// last place asked for is on the stage. What a held step button waits on
/// before asking for the next frame, so that the run goes at the speed the
/// decoder can actually draw at instead of running the counter and the strip
/// away from the picture.
const scrubSettled = () =>
  scrubBusy ? new Promise((r) => scrubIdle.push(r)) : Promise.resolve();

function scrubTo(o) {
  o = clamp(o, 0, outDur);
  playhead = outToSrc(o);
  updateReadouts();
  draw();
  paintFast(playhead);
  // Slide what has already been drawn under the playhead now, rather than
  // waiting on the redraw: it is a transform, and it keeps the marked cell
  // on the cell the playhead is really in.
  placeReel(o);
  scheduleStrip();
  if (scrubBusy) {
    scrubPending = o;
    return;
  }
  scrubBusy = true;
  runScrub(o);
}

async function runScrub(o) {
  await showFrame(outToSrc(o));
  if (scrubPending !== null) {
    const next = scrubPending;
    scrubPending = null;
    runScrub(next);
  } else {
    scrubBusy = false;
    const waiting = scrubIdle;
    scrubIdle = [];
    for (const r of waiting) r();
  }
}

el("strip").addEventListener(
  "wheel",
  (ev) => {
    if (!src) return;
    ev.preventDefault();
    // A notch is a frame, so the GOP boundaries creep across the window
    // rather than jumping; Shift hops whole GOPs for covering ground.
    const dir = Math.sign(ev.deltaY);
    if (ev.shiftKey) scrubTo(srcToOutSeam(nearestPoint(playhead, dir)));
    else scrubTo(playOut() + dir * frame());
  },
  { passive: false }
);

// --- playback -----------------------------------------------------------
//
// Video only, and at whatever resolution the decoder can keep up with. It is
// there to check a cut, not to watch the programme: the useful question is
// "does the join look right", and for that the pictures are enough.

let playing = false;

// Where playback has got to, as the last picture's place and the moment it
// arrived. The engine paces itself against a wall clock at 1x on the edited
// timeline, so the position between two pictures is arithmetic rather than a
// guess -- and it has to be: pictures come fifteen a second, and a strip that
// moved only when one arrived would step in fifteenths however smoothly it
// were drawn.
let playAnchor = null;
let reelRaf = 0;

const playPos = () =>
  playing && playAnchor
    ? clamp(playAnchor.out + (performance.now() - playAnchor.wall) / 1000, 0, outDur)
    : playOut();

/// Take the arriving picture as the truth about where playback is, but ease
/// onto it rather than snap: a decode running a few tens of milliseconds late
/// would otherwise show as the strip twitching backwards. A real break in the
/// clock -- a stall, a seek -- is far bigger than that jitter and is taken whole.
function anchorPlay(o) {
  const wall = performance.now();
  if (!playAnchor) {
    playAnchor = { out: o, wall };
    return;
  }
  const pred = playAnchor.out + (wall - playAnchor.wall) / 1000;
  playAnchor =
    Math.abs(o - pred) > 0.3 ? { out: o, wall } : { out: pred + (o - pred) * 0.15, wall };
}

/// Slide the reel once per repaint, and build a fresh one when the playhead
/// comes within a quarter-window of the edge of what was drawn -- that margin
/// is what the round trip for the new pictures runs inside.
function reelTick() {
  reelRaf = 0;
  if (!playing) return;
  const o = playPos();
  placeReel(o);
  if (!stripBusy && reelWin && Math.abs(o - reelWin.rest) > reelWin.vis / 4) askStrip(o);
  reelRaf = requestAnimationFrame(reelTick);
}

function setPlaying(on) {
  playing = on;
  el("play").textContent = on ? "■ 停止" : "▶ 再生";
  el("play").classList.toggle("on", on);
  if (!on) {
    playAnchor = null;
    if (reelRaf) cancelAnimationFrame(reelRaf);
    reelRaf = 0;
  }
}

function startPlay() {
  if (!src || playing || outDur <= 0) return;
  setPlaying(true);
  // Redraw before the first picture arrives: the reel standing there was
  // drawn without a margin, and there is nowhere for it to slide.
  clearTimeout(stripTimer);
  stripTimer = null;
  askStrip();
  reelRaf = requestAnimationFrame(reelTick);
  // Every picture shown costs a JPEG and a data URL, so how many are worth
  // asking for depends on what is being read: a dozen a second is all a full
  // MPEG-2 decode can keep up with anyway, while a proxy can hand over
  // enough to look like motion.
  const fps = proxied ? 24 : 15;
  // Capped at 1280 whatever the stage asks for. Each picture costs a scale,
  // a JPEG and a data URL, so this is the one place where dropping below the
  // stage's full request buys back frame rate -- and where there is a proxy,
  // 1280 is also its own width, past which the extra pixels are invented.
  const width = Math.min(stageWidth(), 1280);
  // not awaited: it resolves when playback ends, and `play-ended` says so
  invoke("play", { ranges: outputRanges(), from: playhead, width, fps }).catch((e) => {
    el("status").textContent = `再生: ${e}`;
    setPlaying(false);
  });
}

function stopPlay() {
  if (!playing) return;
  invoke("stop_play");
  setPlaying(false);
  showFrame(playhead);
}

el("play").addEventListener("click", () => (playing ? stopPlay() : startPlay()));

// --- scene search -------------------------------------------------------

async function toScene(dir, from = playhead) {
  if (!warmed) return;
  try {
    // A scene inside a cut no longer exists; step past it to the next one.
    for (let i = 0; i < 6; i++) {
      const t = await invoke("scene_search", { from, dir });
      if (t === null || t === undefined) return;
      if (srcToOut(t) !== null) {
        showFrame(t);
        return;
      }
      from = t;
    }
  } catch (e) {
    el("status").textContent = `シーン検索: ${e}`;
  }
}

/// Build the seek index -- and the proxy, where one was asked for -- once, in
/// the background.
///
/// Until this finishes the recording answers for its own pictures with
/// nothing held, which is slow but works, so nothing here blocks editing.
/// When an index from an earlier session is found there is nothing to do at
/// all and this returns at once.
async function prepare() {
  warmed = false;
  held = false;
  // Until this says otherwise the recording is answering for its own
  // pictures -- including when the file just opened is the second one and
  // the first one had a proxy.
  proxied = false;
  scenes = [];
  cardThumbs.clear();
  el("prev-scene").disabled = true;
  el("next-scene").disabled = true;
  el("warm").textContent = "準備中 0%";
  try {
    const r = await invoke("prepare");
    const t = r.track;
    scenes = t.scenes;
    interval = t.interval;
    warmed = true;
    held = true;
    proxied = !!r.proxy;
    el("prev-scene").disabled = false;
    el("next-scene").disabled = false;
    const made = [];
    if (r.proxy) {
      made.push(
        `プロキシ ${r.proxy.width}x${r.proxy.height} ` +
          `${(r.proxy.bytes / 1e6).toFixed(0)}MB（` +
          (r.proxy.cached ? "前回のを再利用 " : "作成 ") +
          `${r.proxy.seconds.toFixed(0)}秒）`,
      );
    }
    // A recording with no proxy is the ordinary case, so it is not worth
    // saying. A proxy that was asked for and failed is.
    if (r.note) made.push(`プロキシなし（${r.note}）`);
    if (r.index) {
      // The seconds are the thumbnail pass's, which is the whole of what the
      // index cost only when there is no proxy -- with one, the same number
      // is already reported above and saying it twice reads as twice the wait.
      const how = r.index.cached
        ? "（前回のを再利用）"
        : r.proxy
          ? ""
          : `（作成 ${t.seconds.toFixed(0)}秒）`;
      made.push(`シーク用インデックス ${(r.index.bytes / 1e6).toFixed(0)}MB${how}`);
    } else {
      made.push("シーク用インデックスは保存できず");
    }
    made.push(`サムネイル ${t.thumbs} 枚 ${t.interval.toFixed(2)}s 間隔`);
    made.push(`シーン ${t.scenes.length} 箇所`);
    el("warm").textContent = made.join(" / ");
    draw();
    stripCache = null;
    askStrip();
    renderKeyframes();
    // The picture on screen came from the recording, decoded before any of
    // this existed. Ask again so that what is shown is what the timeline will
    // keep showing from here on.
    showFrame(playhead);
  } catch (e) {
    // Opening another file supersedes this one; that is not a failure worth
    // showing, because the second file's own pass is already running.
    if (String(e).includes("cancelled")) return;
    el("warm").textContent = `準備: ${e}`;
  }
}

// --- hover preview on the scrubber --------------------------------------

let hoverTimer = null;

function hideHover() {
  clearTimeout(hoverTimer);
  hoverToken++;
  el("hover").hidden = true;
}

track.addEventListener("mousemove", (ev) => {
  if (!src || dragging || outDur <= 0) return;
  const w = track.clientWidth;
  const o = xToTime(ev.offsetX, w);
  const box = el("hover");
  box.hidden = false;
  const bw = box.offsetWidth || 198;
  box.style.left = `${clamp(ev.offsetX + 6 - bw / 2, 0, Math.max(0, w - bw))}px`;
  el("hover-time").textContent = fmt(o);
  el("hover-kind").textContent = held ? "" : "準備中";
  if (!held) return;
  clearTimeout(hoverTimer);
  const token = ++hoverToken;
  hoverTimer = setTimeout(async () => {
    const shot = await invoke("hover_thumb", { time: outToSrc(o) });
    if (token !== hoverToken || !shot) return;
    el("hover-img").src = shot.url;
    el("hover-kind").textContent = nearScene(shot.time, 1.2) ? "シーン" : "";
  }, 20);
});
track.addEventListener("mouseleave", hideHover);

// --- plan ---------------------------------------------------------------

let planTimer = null;
function schedulePlan() {
  clearTimeout(planTimer);
  planTimer = setTimeout(refreshPlan, 120);
}

/// Never round a percentage up to something it has not reached: "100%" when
/// two frames are being re-encoded is the one number a smart renderer must
/// not print. The frame count is what decides, not the arithmetic -- seconds
/// carry float dust, and none re-encoded really is a hundred percent.
function pctText(pct, redone) {
  if (redone === 0 || pct >= 100) return "100%";
  return pct > 99.9 ? "99.9%" : `${pct.toFixed(1)}%`;
}

async function refreshPlan() {
  const ranges = outputRanges();
  if (!src || !ranges.length) {
    el("plan-text").textContent = src ? "すべてカットされています" : "ファイルを開いてください";
    el("segments").innerHTML = "";
    el("copied-bar").style.width = "0%";
    el("smart-badge").textContent = "—";
    return;
  }
  try {
    const p = await invoke("make_plan", { ranges });
    const pct = p.total > 0 ? (100 * p.copied) / p.total : 0;
    el("copied-bar").style.width = `${pct}%`;
    const redone = p.segments
      .filter((g) => g.kind !== "copy")
      .reduce((n, g) => n + g.frames, 0);
    el("plan-text").textContent =
      `出力 ${fmt(p.total)}（${ranges.length} 区間、カット ${cuts.length} 箇所）— ` +
      `無劣化コピー ${p.copied.toFixed(2)}s (${pctText(pct, redone)})` +
      ` / 再エンコード ${p.reencoded.toFixed(2)}s`;
    // "Completely lossless" has to mean not one re-encoded picture, not a
    // percentage that rounds to a hundred: a cut off an access point always
    // re-encodes a frame or two, and 2 frames out of 40000 rounds to 100.0%.
    el("smart-badge").textContent =
      redone === 0 ? "映像 完全無劣化" : `再エンコード ${redone} フレーム`;
    el("segments").innerHTML = p.segments
      .map(
        (s) =>
          `<li class="${s.kind}">${s.kind === "copy" ? "コピー　　" : "再エンコード"} ` +
          `${fmt(s.start)} → ${fmt(s.end)}  (${s.frames} フレーム)</li>`
      )
      .join("");
  } catch (e) {
    el("plan-text").textContent = `計画できません: ${e}`;
  }
}

// --- edit ---------------------------------------------------------------

// IN..OUT is half-open, so the picture at OUT survives. That is right in the
// middle of a recording and wrong at its ends: there is no position past the
// last picture to put OUT at, so "cut to the end" would always leave that one
// picture behind -- a stray frame at the end of the output. The two ends
// therefore snap to the bounds of the timeline.
const atFirstPicture = (o) => outToSrc(o) <= (src.points[0] ?? 0) + frame() / 2;
const atLastPicture = (o) => o >= outDur - frame() * 1.5;

// Marking one end leaves the other where it was: IN..OUT is a range you build
// up by putting down one end and then the other, and moving one of them is no
// reason to lose the other. Only when the two cross does the end just set
// win, and the other runs out to the edge of the timeline -- "from here
// onwards" and "up to here" being the honest reading until it is narrowed.
function setIn(o) {
  selA = atFirstPicture(o) ? 0 : clamp(o, 0, outDur);
  if (selB <= selA) selB = outDur;
  updateReadouts();
  draw();
  scheduleStrip();
}

/// The instant just past the selection: IN..OUT is inclusive of OUT, so
/// removing it means removing everything up to the start of the next picture.
const selEnd = () => (selB >= outDur - 1e-9 ? outDur : Math.min(selB + frame(), outDur));

function setOut(o) {
  selB = atLastPicture(o) ? outDur : clamp(o, 0, outDur);
  if (selB <= selA) selA = 0;
  updateReadouts();
  draw();
  scheduleStrip();
}

/// Pick up the marks saved beside a recording, if any are.
///
/// The export writes `<name>.keyframe` next to the video; opening that video
/// again -- or the cut it produced -- should not start from an empty list
/// when the work is sitting right there. Nothing is selected: this is a batch
/// like CM detection, with no one mark it is about.
///
/// The numbers count from the first picture, the way the export writes them,
/// which is not where the recording's clock starts: broadcast material often
/// opens most of a second in. So the number is an output time and has to be
/// put back through the timeline to become a source time -- nothing is cut
/// yet, so that is the start offset, but going through `outToSrc` keeps it
/// right whatever the timeline turns out to be.
///
/// Marks past the end are dropped -- a list written for a different cut of
/// the same recording is the likely reason, and `outToSrc` would otherwise
/// clamp them all onto the last picture.
async function loadSidecarKeyframes(path) {
  const side = path.replace(/\.[^./\\]*$/, "") + ".keyframe";
  if (side === path) return;
  let frames;
  try {
    frames = await invoke("read_keyframes", { path: side });
  } catch (e) {
    el("status").textContent = `キーフレームを読めません: ${e}`;
    return;
  }
  if (!frames) return;
  const times = frames
    .map((n) => n / src.fps)
    .filter((o) => o <= outDur + 1e-6)
    .map(outToSrc);
  if (!times.length) return;
  addKeyframes(times);
  el("status").textContent =
    `キーフレーム ${liveKeyframes().length} 個を ${side.split(/[/\\]/).pop()} から読み込みました`;
}

async function openPath(picked) {
  jlog(`openPath ${picked}`);
  if (!picked) return;
  el("title").textContent = "解析中…";
  try {
    src = await invoke("open_source", { path: picked });
    const flags = [
      src.interlaced ? "インターレース (TFF)" : "プログレッシブ",
      src.pulldown ? "2:3プルダウン" : null,
    ].filter(Boolean);
    el("title").textContent = picked.split("/").pop();
    el("info").textContent =
      `無劣化点: ${src.points.length}   ${src.width}x${src.height}   ` +
      `${src.fps.toFixed(2)} fps   ${flags.join(" ")}   ` +
      `${src.has_audio ? "音声あり" : "音声なし"}   ${src.codec}` +
      (src.unusable_points ? `   （うち ${src.unusable_points} 個は開始に使えません）` : "");
    cuts = [];
    cutHistory = [];
    keyframes = [];
    activeKey = null;
    cmBlocks = [];
    stripCache = null;
    stripShots = [];
    hideHover();
    rebuildTimeline();
    el("undo-cut").disabled = true;
    el("cm-note").textContent = "";
    renderKeyframes();
    // Opened whole: nothing is cut yet, so the selection is the recording.
    selA = 0;
    selB = outDur;
    el("status").textContent = "";
    await loadSidecarKeyframes(picked);
    await showFrame(0);
    schedulePlan();
    prepare();
  } catch (e) {
    el("title").textContent = "";
    el("status").textContent = `開けません: ${e}`;
  }
}

el("open").addEventListener("click", async () =>
  openPath(
    await dialog.open({
      multiple: false,
      filters: [
        { name: "動画", extensions: ["ts", "m2ts", "mts", "mp4", "mkv", "m2t", "mov", "m4v"] },
      ],
    })
  )
);

// --- scrubber pointer ---------------------------------------------------

track.addEventListener("mousedown", (ev) => {
  if (!src || ev.button !== 0 || outDur <= 0) return;
  if (playing) stopPlay();
  const w = track.clientWidth;
  const x = ev.offsetX;
  const near = (t) => Math.abs(timeToX(t, w) - x) < 8;
  dragging = near(selA) ? "in" : near(selB) ? "out" : "seek";
  hideHover();
  if (dragging === "seek") seekOut(xToTime(x, w));
});
window.addEventListener("mousemove", (ev) => {
  if (!dragging || !src) return;
  const rect = track.getBoundingClientRect();
  const o = xToTime(ev.clientX - rect.left, rect.width);
  if (dragging === "in") setIn(o);
  else if (dragging === "out") setOut(o);
  else {
    playhead = outToSrc(o);
    updateReadouts();
    draw();
    paintFast(playhead);
  }
});
window.addEventListener("mouseup", () => {
  if (dragging === "seek") showFrame(playhead);
  dragging = null;
});

// --- transport ----------------------------------------------------------

// Holding the frame buttons keeps stepping, the way the arrow keys already do
// under the keyboard's own repeat -- a frame at a time is how you find a cut,
// and clicking sixty times to cross two seconds is not an edit, it is typing.
// The first step lands on the press, so a tap is still exactly one frame; the
// run only starts once the button has been held past the point where a tap
// would have ended, and picks up speed after a second, which is about when
// holding stops meaning "one more" and starts meaning "keep going".
//
// The steps go through `scrubTo` rather than `seekOut`: it keeps at most one
// decode in flight and always for the latest place asked for, so a repeat
// faster than the decoder moves the playhead and the film strip at the rate
// asked for instead of queueing decodes it can never catch up on. That is the
// same treatment the wheel and the arrow keys get.
//
// **A step is not asked for until the picture for the last one is up.** A
// fixed repeat was tried first and is wrong at both ends: on a fast file it
// ran the counter ahead of the stage, and on a slow one the run turned into
// the playhead sprinting while the picture and the strip stood still --
// stepping a frame at a time is for *looking* at the frames, so a run that
// outpaces the pictures has nothing left to be for. Waiting on the picture
// makes the speed the machine's answer rather than a guess, and the interval
// below is the floor under it, not the rate.
//
// Capped, all the same: a file the decoder is slow on would otherwise stop
// the run dead, and a hand still on the button is asking to keep going.
const HOLD_DELAY = 400;
const HOLD_SLOW = 130;
const HOLD_FAST = 65;
const HOLD_RAMP = 1500;
const HOLD_WAIT = 400;

const after = (ms) => new Promise((r) => setTimeout(r, ms));

function holdStep(id, dir) {
  const btn = el(id);
  let timer = null;
  let began = 0;
  // Bumped on release, so a run whose picture is still being decoded when the
  // button comes up does not schedule one more step behind it.
  let run = 0;
  const step = () => scrubTo(playOut() + dir * frame());
  const stop = () => {
    run++;
    clearTimeout(timer);
    timer = null;
  };
  const tick = async () => {
    const mine = run;
    const at = Date.now();
    step();
    await Promise.race([scrubSettled(), after(HOLD_WAIT)]);
    if (mine !== run) return;
    const gap = at - began > HOLD_RAMP ? HOLD_FAST : HOLD_SLOW;
    timer = setTimeout(tick, Math.max(0, gap - (Date.now() - at)));
  };
  btn.addEventListener("pointerdown", (ev) => {
    if (!src || ev.button !== 0) return;
    ev.preventDefault();
    if (playing) stopPlay();
    // Captured, so a finger or a pointer that wanders off the button while
    // held goes on stepping and the release is still heard here.
    btn.setPointerCapture(ev.pointerId);
    stop();
    began = Date.now();
    step();
    timer = setTimeout(tick, HOLD_DELAY);
  });
  btn.addEventListener("pointerup", stop);
  btn.addEventListener("pointercancel", stop);
  window.addEventListener("blur", stop);
}

el("go-start").addEventListener("click", () => seekOut(0));
el("go-end").addEventListener("click", () => seekOut(outDur));
holdStep("step-back", -1);
holdStep("step-fwd", 1);
el("prev-kf").addEventListener("click", () => showFrame(nearestPoint(playhead, -1)));
el("next-kf").addEventListener("click", () => showFrame(nearestPoint(playhead, 1)));
el("goto-in").addEventListener("click", () => seekOut(selA));
el("goto-out").addEventListener("click", () => seekOut(selB));
el("set-in").addEventListener("click", () => setIn(playOut()));
el("set-out").addEventListener("click", () => setOut(playOut()));
el("prev-scene").addEventListener("click", () => toScene(-1));
el("next-scene").addEventListener("click", () => toScene(1));
el("add-key").addEventListener("click", () => addKeyframes([playhead], playhead));
el("strip-step").addEventListener("change", (ev) => {
  // Hand the keyboard back, or the arrow keys would go on changing the
  // spacing instead of stepping through frames.
  ev.target.blur();
  stripCache = null;
  askStrip();
});

el("snap").addEventListener("click", () => {
  if (!src || outDur <= 0) return;
  // Access points are places in the recording, so the round trip through
  // source time is the whole job.
  let a = srcToOutSeam(nearestPoint(outToSrc(selA)));
  let b = srcToOutSeam(nearestPoint(outToSrc(selB)));
  if (b <= a) b = srcToOutSeam(nearestPoint(outToSrc(a), 1));
  if (b <= a) a = srcToOutSeam(nearestPoint(outToSrc(b), -1));
  selA = clamp(Math.min(a, b), 0, outDur);
  selB = clamp(Math.max(a, b), 0, outDur);
  updateReadouts();
  draw();
  scheduleStrip();
});

el("cut-range").addEventListener("click", () => {
  if (!src || selB <= selA) return;
  const at = selA;
  const a = atFirstPicture(selA) ? 0 : selA;
  const b = atLastPicture(selB) ? outDur : selEnd();
  applyCuts(cuts.concat(outRangeToSrc(a, b)));
  // The material that was selected is gone and the timeline has closed over
  // it. Collapse the selection onto the join.
  selA = clamp(at, 0, outDur);
  selB = clamp(at + frame(), 0, outDur);
  seekOut(at);
});
el("cut-outside").addEventListener("click", () => {
  if (!src || selB <= selA) return;
  const keep = outRangeToSrc(selA, selB);
  applyCuts(cuts.concat(outRangeToSrc(0, selA)).concat(outRangeToSrc(selEnd(), outDur)));
  selA = 0;
  selB = outDur;
  seekOut(0);
  jlog(`cut outside, kept ${JSON.stringify(keep)}`);
});
el("undo-cut").addEventListener("click", () => {
  if (!cutHistory.length) return;
  cuts = cutHistory.pop();
  afterCutsChanged();
});
el("clear-all").addEventListener("click", () => {
  if (!src) return;
  cuts = [];
  cutHistory = [];
  keyframes = [];
  activeKey = null;
  rebuildTimeline();
  selA = 0;
  selB = outDur;
  el("undo-cut").disabled = true;
  renderKeyframes();
  updateReadouts();
  draw();
  stripCache = null;
  scheduleStrip();
  schedulePlan();
});

/// Whether a held arrow key's repeat is worth acting on.
///
/// The keyboard repeats at whatever rate it is set to, which on most is fast
/// enough to outrun the decoder -- and a frame run whose pictures never catch
/// up is a counter spinning, not a search. Same answer the step buttons get:
/// a repeat waits for the picture for the last one, with the same floor under
/// the gap and the same ceiling on the wait. A deliberate press is never
/// dropped, so a tap is still exactly one frame.
let arrowLast = 0;
let arrowSince = 0;

function arrowDue(ev) {
  const now = Date.now();
  if (!ev.repeat) {
    arrowSince = now;
    arrowLast = now;
    return true;
  }
  const gap = now - arrowSince > HOLD_RAMP ? HOLD_FAST : HOLD_SLOW;
  if (now - arrowLast < gap) return false;
  if (scrubBusy && now - arrowLast < HOLD_WAIT) return false;
  arrowLast = now;
  return true;
}

window.addEventListener("keydown", (ev) => {
  if (!src || ev.target.tagName === "INPUT" || ev.target.tagName === "SELECT") return;
  if (ev.key === " ") {
    ev.preventDefault();
    playing ? stopPlay() : startPlay();
    return;
  }
  if (ev.ctrlKey && (ev.key === "d" || ev.key === "D")) {
    ev.preventDefault();
    el("detect-cm").click();
    return;
  }
  if (ev.ctrlKey || ev.metaKey || ev.altKey) return;
  if (playing && ev.key !== "i" && ev.key !== "o" && ev.key !== "k") stopPlay();
  const step = ev.shiftKey ? 1 : frame();
  if (ev.key === "ArrowRight" || ev.key === "ArrowLeft") {
    ev.preventDefault();
    if (arrowDue(ev)) scrubTo(playOut() + (ev.key === "ArrowRight" ? step : -step));
    return;
  }
  if (ev.key === "i") setIn(playOut());
  if (ev.key === "o") setOut(playOut());
  if (ev.key === "k" || ev.key === "K") addKeyframes([playhead], playhead);
  if (ev.key === "s" || ev.key === "S") toScene(ev.shiftKey ? -1 : 1);
});

// --- commercial breaks --------------------------------------------------

el("detect-cm").addEventListener("click", async () => {
  if (!src) return;
  el("detect-cm").disabled = true;
  const useLogo = el("use-logo").checked;
  el("cm-note").textContent = useLogo ? "検出中…（映像も読みます）" : "検出中…";
  try {
    const res = await invoke("detect_cm", { useLogo });
    cmBlocks = res.blocks;
    const how = res.resets > 0
      ? `字幕リセット ${res.resets} 箇所`
      : !useLogo ? "無音のみ" : res.logo_found ? "ロゴ＋無音" : "無音のみ（ロゴなし）";
    el("cm-note").textContent = cmBlocks.length
      ? `${how}: ${cmBlocks.length} ブロック / 合計 ` +
        fmt(cmBlocks.reduce((n, b) => n + (b.end - b.start), 0))
      : `${how}: CM らしい区間は見つかりませんでした`;
    // Each block's start is where the commercials begin and its end is where
    // the programme comes back, so both are worth a mark -- along with the
    // opening of the recording itself.
    if (cmBlocks.length) {
      // The head of the material, not of the clock: nothing before the first
      // access point can be decoded, so a mark at zero would have nothing
      // under it.
      // Not snapped to an access point: the mark should say where the cut
      // actually is, to the frame. Moving it onto the nearest lossless point
      // is a separate decision, and there is a button for it.
      addKeyframes(
        [src.points[0] ?? 0].concat(cmBlocks.flatMap((b) => [b.start, b.end]))
      );
    }
    draw();
  } catch (e) {
    el("cm-note").textContent = `検出できません: ${e}`;
  } finally {
    el("detect-cm").disabled = false;
    el("detect-cm").textContent = "CM を検出";
  }
});

// --- output -------------------------------------------------------------

el("export").addEventListener("click", async () => {
  const ranges = outputRanges();
  if (!src || !ranges.length) return;
  jlog(`export clicked, ranges=${JSON.stringify(ranges)}`);
  // Named after the recording it came from, beside it, in the same container.
  // A broadcast file's name carries the date, the channel and the episode --
  // everything you would need to find it again -- so throwing it away for
  // "cut.ts" is a loss. The prefix is what says which one is the edit.
  const ext = (src.path.match(/\.([A-Za-z0-9]+)$/)?.[1] || "mp4").toLowerCase();
  const others = ["mp4", "mkv", "ts", "m2ts", "mov"].filter((e) => e !== ext);
  const cut = src.path.lastIndexOf("/") + 1 || src.path.lastIndexOf("\\") + 1;
  const dir = src.path.slice(0, cut);
  const stem = src.path.slice(cut).replace(/\.[^.]*$/, "");
  // One entry per container rather than one lumped "動画": the picker appends
  // the extension of whichever entry is chosen, so separate entries are what
  // make the container an actual choice instead of a filename to remember.
  // The source's own leads, being the default.
  const named = { ts: "MPEG-2 TS", m2ts: "M2TS", mp4: "MP4", mkv: "Matroska", mov: "QuickTime" };
  const out = await dialog.save({
    defaultPath: `${dir}cut_${stem}.${ext}`,
    filters: [ext, ...others].map((e) => ({ name: `${named[e] ?? e} (.${e})`, extensions: [e] })),
  });
  if (!out) return;
  el("export").disabled = true;
  const started = Date.now();
  el("status").textContent = "映像を無劣化出力しています…";
  try {
    // Audio is smart-rendered -- copied, bar the frames a cut lands inside --
    // and written into the same container as the video. The engine will also
    // copy outright or re-encode the track whole (see the CLI's --audio-mode
    // and --audio-es), but neither earned its place on screen: on real cuts,
    // with the boundaries snapped to access points, the default does the
    // right thing without being asked.
    await invoke("export", { ranges, output: out });
    let extra = "";
    if (el("keyframes-out").checked) {
      // Numbered against the file being written, not the recording.
      const frames = liveKeyframes().map((t) => frameNo(srcToOut(t)));
      const side = out.replace(/\.[^./\\]*$/, "") + ".keyframe";
      const n = await invoke("write_keyframes", { path: side, frames, fps: src.fps });
      extra += ` / キーフレーム ${n} 個を ${side.split("/").pop()} へ`;
    }
    el("status").textContent =
      `完了 (${((Date.now() - started) / 1000).toFixed(1)} 秒): ${out}${extra}`;
  } catch (e) {
    el("status").textContent = `失敗: ${e}`;
  } finally {
    el("export").disabled = false;
  }
});

if (listen) {
  listen("play-frame", (ev) => {
    if (!playing) return;
    const [t, url] = ev.payload;
    playhead = t;
    el("preview").src = url;
    updateReadouts();
    draw();
    // The strip is not redrawn here -- it is already sliding, and this is
    // what it slides against.
    anchorPlay(srcToOutSeam(t));
  });
  listen("play-ended", () => {
    if (!playing) return;
    setPlaying(false);
    showFrame(playhead);
  });
  // The video half of playback has no way to notice the audio half failed --
  // they run on separate threads and separate clocks -- so without this the
  // picture just plays silently with nothing on screen to say why.
  listen("audio-error", (ev) => {
    el("status").textContent = `音声再生エラー: ${ev.payload}`;
  });
  listen("cm-progress", (ev) => {
    const [phase, done] = ev.payload;
    el("detect-cm").textContent = `検出中 ${Math.round(done * 100)}%`;
    el("cm-note").textContent = phase;
  });
  listen("prepare-progress", (ev) => {
    const [phase, done] = ev.payload;
    if (!warmed) el("warm").textContent = `${phase}準備中 ${Math.round(done * 100)}%`;
  });
  // Pictures from a pass that is still running. Everything that reads held
  // pictures can use them from here on, for the stretch of the recording the
  // pass has read -- which is what stops the strip decoding the recording
  // itself while the index, or a proxy, is being built.
  listen("prepare-held", (ev) => {
    const [gap] = ev.payload;
    interval = gap;
    // Only the first batch is worth redrawing for. A strip drawn before there
    // were held pictures is not wrong -- the cells it could not fill were
    // decoded -- so the later batches change nothing on screen and arrive
    // twice a second for the length of the build.
    if (held) return;
    held = true;
    scheduleStrip();
    renderKeyframes();
  });
  listen("export-progress", (ev) => {
    const pct = Math.round(ev.payload * 100);
    el("progress-bar").style.width = `${pct}%`;
    if (pct < 100) el("status").textContent = `映像を無劣化出力しています… ${pct}%`;
  });
}

window.addEventListener("resize", () => {
  draw();
  // the reel is placed in pixels, so a narrower window is a wrong offset --
  // and a narrower window holds fewer cells, so it wants a fresh reel too
  holdReel();
  scheduleStrip();
});
renderKeyframes();
draw();
jlog("wiring done");
invoke("initial_path")
  .then((p) => {
    jlog(`initial_path -> ${JSON.stringify(p)}`);
    if (p) return openPath(p);
    el("status").textContent = "ファイルを開いてください";
  })
  .catch((e) => (el("status").textContent = `initial_path: ${e}`));
