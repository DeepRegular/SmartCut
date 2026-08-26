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
const CELLS = 9;

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
let interval = 0.5;
let previewToken = 0;
let stripToken = 0;
let hoverToken = 0;
let stripShots = [];
let stripCache = null;
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

async function cardThumb(t) {
  if (!warmed) return null;
  // A mark sitting on a join needs the picture that is actually there. The
  // held ones are key pictures, and the nearest key picture to a join is
  // usually the last one the cut took.
  //
  // Whether a mark is a join is part of the cache key, not just of the
  // request: cuts come and go, and the same instant wants a different picture
  // on either side of the edit. Cutting a detected break out turns the mark
  // that was already standing at its head into a join, and without this the
  // card kept the key picture it had been drawn with.
  const o = srcToOut(t);
  const join = o !== null && isJoin(o);
  const key = `${t.toFixed(3)}${join ? "J" : ""}`;
  if (cardThumbs.has(key)) return cardThumbs.get(key);
  try {
    const url = join
      ? (await invoke("thumbs_at", { times: [t], width: 200, exact: true }))[0]?.url
      : (await invoke("hover_thumb", { time: t }))?.url;
    if (url) cardThumbs.set(key, url);
    return url ?? null;
  } catch {
    return null;
  }
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
  live.forEach((t, i) => {
    const li = document.createElement("li");
    if (isActive(t)) {
      li.className = "active";
      // The list scrolls, and a mark a cut just made is often below the fold.
      requestAnimationFrame(() => li.scrollIntoView({ block: "nearest" }));
    }
    const img = document.createElement("img");
    img.alt = "";
    cardThumb(t).then((url) => url && (img.src = url));
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
      shot = await invoke("preview", { time: ask, width: 960 });
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
const stepOut = (d) => seekOut(playOut() + d);

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
// A window of fixed duration, centred on the playhead and divided where the
// GOPs divide -- which is how the reference tool draws it, and it is the
// right unit twice over: those boundaries are the only places a cut is free,
// and the pictures at them are exactly the ones already held in memory, so a
// cell costs nothing to fill.
//
// Cell widths are the GOPs' own share of the window, so a partial GOP at the
// end of the recording or at a cut really does come out narrower. The window
// follows the playhead, so the marker stays at the middle and the boundaries
// creep across it a frame at a time.
//
// The cells hang on a reel drawn wider than the window shows, and following
// the playhead is a transform on that reel rather than a redraw. That is what
// playback needs: pictures arrive fifteen times a second and a redraw costs a
// round trip, so a strip that redrew to follow would step, however often it
// stepped. Sliding it instead is free, and a fresh reel is only built once the
// playhead nears the edge of the drawn one -- around the same pictures, in the
// same places, so the swap does not show.

let stripTimer = null;
function scheduleStrip() {
  clearTimeout(stripTimer);
  stripTimer = setTimeout(refreshStrip, 140);
}

/// How much of the recording the strip covers, and whether it is divided by
/// GOP or by frame. A null span means frame mode.
function stripView() {
  const v = el("strip-step").value;
  return v === "frame" ? { span: null } : { span: parseFloat(v.slice(4)) };
}

const MAX_CELLS = 15;

const reel = el("reel");

/// What the drawn reel covers, in output time: `at` is the moment at its left
/// edge, `span` how much it holds, `vis` how much of that the window shows,
/// and `rest` where it sits when nothing is chasing the playhead.
let reelWin = null;

/// How many windows wide to draw the reel. Only playback has anything to
/// slide, and the margin is pictures decoded for nothing everywhere else.
const overscan = () => (playing ? 2 : 1);

/// Slide the reel so that output time `o` falls under the marker. Clamped to
/// what was drawn: if a redraw is late, the strip holding still for a moment
/// reads far better than a gap opening at its edge.
function placeReel(o) {
  if (!reelWin) return;
  const w = el("strip").clientWidth;
  if (w <= 0) return;
  const px = w / reelWin.vis;
  const x = clamp(w / 2 - (o - reelWin.at) * px, Math.min(0, w - reelWin.span * px), 0);
  reel.style.transform = `translateX(${x.toFixed(2)}px)`;
  markHere(o);
}

/// Where the reel belongs right now: under the playhead while it is being
/// played, at the place it was drawn for otherwise.
const holdReel = () => reelWin && placeReel(playing ? playPos() : reelWin.rest);

/// Outline the cell the playhead stands in. It changes as the reel slides, so
/// it is set here rather than baked in when the cells are built.
///
/// A GOP-divided reel wants the last cell that has begun -- the cells are
/// clipped to the window, so their ends cannot be trusted to bracket the
/// playhead at the edges -- while frame cells are centred on their picture
/// and want the nearest.
function markHere(o) {
  const cells = reelWin.cells;
  let i = -1;
  for (let k = 0; k < cells.length; k++) {
    if (reelWin.byNearest) {
      if (i < 0 || Math.abs(cells[k].at - o) < Math.abs(cells[i].at - o)) i = k;
    } else if (o >= cells[k].at - 1e-9) i = k;
  }
  if (i === reelWin.here) return;
  if (cells[reelWin.here]) cells[reelWin.here].fig.classList.remove("here");
  if (cells[i] && cells[i].live) cells[i].fig.classList.add("here");
  reelWin.here = i;
}

/// The cells of a GOP-divided window: `start` is the GOP's own beginning --
/// the picture to show and the time to caption -- and `from`..`to` is the
/// part of it the window can see, which is where the widths come from.
function gopCells(w0, w1, max) {
  let i = 0;
  while (i < gops.length && gops[i] <= w0 + 1e-9) i++;
  const marks = [];
  for (let j = Math.max(0, i - 1); j < gops.length && gops[j] < w1 - 1e-9; j++) {
    marks.push(gops[j]);
  }
  if (!marks.length) marks.push(0);

  // A minute of half-second GOPs is 120 cells; nobody can read a ten-pixel
  // thumbnail, so boundaries get skipped rather than the pictures shrunk.
  const every = Math.ceil(marks.length / max);
  const kept = marks.filter((_, k) => k % every === 0);

  return kept.map((m, k) => {
    const next = kept[k + 1] ?? Math.min(w1, outDur);
    return { start: m, from: Math.max(m, w0), to: Math.min(next, w1) };
  });
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

  // The reel is the window plus its margin; the window is what is seen, and
  // it is the window that decides how wide a second is on screen.
  const span = view.span * overscan();
  const w0 = o - span / 2;
  const w1 = o + span / 2;
  const cells = gopCells(Math.max(0, w0), Math.min(outDur, w1), MAX_CELLS * overscan());
  const times = cells.map((c) => outToSrc(c.start));
  // Cells that begin on a GOP are already in memory; a cell that begins on a
  // join is not, and asking the held pictures for it would hand back the
  // last picture the cut took. Those few are decoded.
  const wanted = cells.map((c) => isJoin(c.start));
  const token = ++stripToken;
  let got;
  try {
    const [held, decoded] = await Promise.all([
      invoke("thumbs_at", { times: times.filter((_, i) => !wanted[i]), width: 200 }),
      wanted.some(Boolean)
        ? invoke("thumbs_at", { times: times.filter((_, i) => wanted[i]), width: 200, exact: true })
        : Promise.resolve([]),
    ]);
    let h = 0;
    let d = 0;
    got = wanted.map((w) => (w ? decoded[d++] : held[h++]));
  } catch (e) {
    jlog(`thumbs_at: ${e}`);
    return;
  }
  if (token !== stripToken) return;

  const shots = cells.map((c, k) => ({
    url: got[k] ? got[k].url : null,
    time: outToSrc(c.start),
    at: c.start,
    width: (c.to - c.from) / span,
  }));
  // nothing exists before the start of the file or past its end
  const lead = (Math.min(w1, Math.max(w0, 0)) - w0) / span;
  const tail = (w1 - Math.max(w0, Math.min(w1, outDur))) / span;
  renderStrip(shots, span / Math.max(1, cells.length), lead, tail, {
    at: w0,
    span,
    vis: view.span,
    rest: o,
    byNearest: false,
  });
}

/// Frame mode: equal cells, one picture each, with a wide window cached so
/// that stepping does not pay for a seek and a GOP every time.
async function refreshFrameStrip(o) {
  const sp = frame();
  // cells either side of the middle one: the window holds CELLS of them, the
  // reel that many again for the margin
  const half = Math.ceil((CELLS * overscan()) / 2);
  const put = (shots, i) =>
    renderStrip(
      shots.slice(i - half, i + half + 1).map((s) => ({ ...s, width: 1 / (2 * half + 1) })),
      sp,
      0,
      0,
      {
        at: shots[i - half].at - sp / 2,
        span: (2 * half + 1) * sp,
        vis: CELLS * sp,
        rest: shots[i].at,
        byNearest: true,
      }
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
  const n = 41;
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

/// Lay the cells out on the reel. `win` says what the reel covers, which is
/// what `placeReel` needs to slide it; widths are the cells' share of the
/// reel, not of the window.
function renderStrip(shots, unit, lead, tail, win) {
  stripShots = shots;
  reel.innerHTML = "";
  reel.style.width = `${(win.span / win.vis) * 100}%`;
  const cells = [];

  const pad = (w) => {
    if (w <= 1e-6) return;
    const fig = document.createElement("figure");
    fig.className = "blank";
    fig.style.width = `${w * 100}%`;
    reel.append(fig);
  };
  pad(lead);

  // Every cell shows the picture its GOP begins with, the one the playhead is
  // standing in included. Swapping that one for the picture under the playhead
  // was tried and is worse: crossing a scene change makes a single cell jump
  // to a different shot while its neighbours hold still, which reads as a
  // glitch. The strip is a ruler; the marker says where you are.
  shots.forEach((s, i) => {
    const fig = document.createElement("figure");
    fig.style.width = `${s.width * 100}%`;
    cells.push({ fig, at: s.at, live: !!s.url });
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
  pad(tail);
  el("playline").hidden = false;
  reelWin = { ...win, cells, here: null };
  holdReel();
}

// --- scroll search ------------------------------------------------------

let search = null;
let lastFast = -1;
let lastSharp = 0;

async function paintFast(t) {
  if (!warmed) return;
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
    const shot = await invoke("preview", { time: t, width: 960 });
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
    refreshStrip();
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

function scrubTo(o) {
  o = clamp(o, 0, outDur);
  playhead = outToSrc(o);
  updateReadouts();
  draw();
  paintFast(playhead);
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
let reelBusy = false;

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
  if (!reelBusy && reelWin && Math.abs(o - (reelWin.at + reelWin.span / 2)) > reelWin.vis / 4) {
    reelBusy = true;
    refreshStrip(o).finally(() => {
      reelBusy = false;
    });
  }
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
  refreshStrip();
  reelRaf = requestAnimationFrame(reelTick);
  // not awaited: it resolves when playback ends, and `play-ended` says so
  invoke("play", { ranges: outputRanges(), from: playhead, width: 640, fps: 15 }).catch((e) => {
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

/// Decode the key pictures once in the background. The hover preview, the
/// scroll search and the scene index all read from what it leaves behind.
async function warmThumbs() {
  warmed = false;
  scenes = [];
  cardThumbs.clear();
  el("prev-scene").disabled = true;
  el("next-scene").disabled = true;
  el("warm").textContent = "サムネイル準備中 0%";
  try {
    const t = await invoke("warm_thumbs");
    scenes = t.scenes;
    interval = t.interval;
    warmed = true;
    el("prev-scene").disabled = false;
    el("next-scene").disabled = false;
    el("warm").textContent =
      `サムネイル ${t.thumbs} 枚 ${t.interval.toFixed(2)}s 間隔 / シーン ${t.scenes.length} 箇所`;
    draw();
    stripCache = null;
    refreshStrip();
    renderKeyframes();
  } catch (e) {
    el("warm").textContent = `サムネイル: ${e}`;
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
  el("hover-kind").textContent = warmed ? "" : "準備中";
  if (!warmed) return;
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
      redone === 0 ? "映像 完全無劣化" : `再エンコード ${redone} コマ`;
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
    await showFrame(0);
    schedulePlan();
    warmThumbs();
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

el("go-start").addEventListener("click", () => seekOut(0));
el("go-end").addEventListener("click", () => seekOut(outDur));
el("step-back").addEventListener("click", () => stepOut(-frame()));
el("step-fwd").addEventListener("click", () => stepOut(frame()));
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
  refreshStrip();
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
  if (ev.key === "ArrowRight") { scrubTo(playOut() + step); ev.preventDefault(); }
  if (ev.key === "ArrowLeft") { scrubTo(playOut() - step); ev.preventDefault(); }
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
    // Audio is copied, and written into the same container as the video.
    // Both were once switchable here; the engine still does either (see the
    // CLI's --audio-mode and --audio-es), but neither earned its place on
    // screen: on real cuts, with the boundaries snapped to access points,
    // copied audio measured no drift at all.
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
  listen("thumbs-progress", (ev) => {
    if (!warmed) el("warm").textContent = `サムネイル準備中 ${Math.round(ev.payload * 100)}%`;
  });
  listen("export-progress", (ev) => {
    const pct = Math.round(ev.payload * 100);
    el("progress-bar").style.width = `${pct}%`;
    if (pct < 100) el("status").textContent = `映像を無劣化出力しています… ${pct}%`;
  });
}

window.addEventListener("resize", () => {
  draw();
  // the reel is placed in pixels, so a narrower window is a wrong offset
  holdReel();
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
